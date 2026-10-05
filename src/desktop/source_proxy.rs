//! Loopback HLS sanitizing proxy.
//!
//! IPTV CDNs disguise HLS segments as images, stylesheets or fonts (odd
//! names, lying `Content-Type`, a real image header prepended to the media).
//! GStreamer and mpv typefind those bytes as images and fail, and no engine
//! setting fixes it. For HLS sources the plugin therefore serves the engine
//! from a small HTTP server on `127.0.0.1` that it owns:
//!
//! - it fetches upstream itself with the source's validated headers, cookies,
//!   user agent, referrer and TLS trust, so engines need no header plumbing;
//! - it rewrites playlists so every variant, rendition, segment, init section
//!   and key URI points back through the proxy (relative URIs resolved,
//!   query strings kept, anything that is not http(s) blocked);
//! - it classifies segments by their bytes, strips junk before the first
//!   MPEG-TS packet run, fMP4 box or ADTS/ID3 audio, labels the real media
//!   type and maps byte ranges past the stripped prefix;
//! - keys and opaque data pass through byte-for-byte.
//!
//! The server starts lazily on first use. Each opened source gets a random
//! capability token in the path; the route ends (in-flight transfers abort)
//! when the owning session closes or is replaced. Nothing here logs URLs,
//! headers, cookies or paths.

mod playlist;
mod server;
mod sniff;

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::{Arc, OnceLock},
};

use hyper::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Url;

use crate::models::NativeOpenRequest;
use playlist::Resource;
use server::{Route, Shared};

const DEFAULT_USER_AGENT: &str = "tauri-plugin-video";
const MAX_REDIRECTS: usize = 8;

pub(crate) struct Proxy {
    runtime: tokio::runtime::Runtime,
    shared: Arc<Shared>,
}

static PROXY: OnceLock<Option<Proxy>> = OnceLock::new();

fn proxy() -> Option<&'static Proxy> {
    PROXY
        .get_or_init(|| match Proxy::start() {
            Ok(proxy) => Some(proxy),
            Err(_) => {
                tracing::error!("source proxy could not start; sources open directly");
                None
            }
        })
        .as_ref()
}

/// A source registered with the proxy for one `native_open`. Settle it with
/// [`settle`] once the engine has (or has not) accepted the source.
#[must_use]
pub(crate) struct Pending(Option<String>);

impl Pending {
    pub(crate) fn proxied(&self) -> bool {
        self.0.is_some()
    }
}

/// Whether this source is served through the proxy: HLS over http(s), unless
/// the request opts out (`sourceProxy: false`). `sourceProxy: true` forces it
/// for any http(s) source, e.g. an HLS playlist without an `.m3u8` name.
fn wants_proxy(payload: &NativeOpenRequest) -> bool {
    let Ok(url) = Url::parse(&payload.uri) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    match payload.source_proxy {
        Some(choice) => choice,
        None => {
            let path = url.path().to_ascii_lowercase();
            path.ends_with(".m3u8") || path.ends_with(".m3u")
        }
    }
}

/// Registers `payload` with the process proxy when it should be proxied and
/// returns the payload the engine must open: the proxy URL with no headers,
/// cookies, user agent, referrer or TLS file (the proxy applies them
/// upstream). Any failure leaves the source to open directly, as before.
pub(crate) fn route(payload: NativeOpenRequest) -> (NativeOpenRequest, Pending) {
    if !wants_proxy(&payload) {
        return (payload, Pending(None));
    }
    match proxy() {
        Some(proxy) => proxy.route(payload),
        None => (payload, Pending(None)),
    }
}

/// See [`Proxy::settle`].
pub(crate) fn settle(pending: Pending, opened: bool) {
    if let Some(proxy) = PROXY.get().and_then(Option::as_ref) {
        proxy.settle(pending, opened);
    }
}

/// See [`Proxy::release`].
pub(crate) fn release(session_key: &str) {
    if let Some(proxy) = PROXY.get().and_then(Option::as_ref) {
        proxy.release(session_key);
    }
}

/// Retires every process-owned route when the host exits. Unlike an ordinary
/// session close, host shutdown has authority over all of its capabilities.
pub(crate) fn shutdown() {
    if let Some(proxy) = PROXY.get().and_then(Option::as_ref) {
        proxy.shutdown();
    }
}

/// Whether the session owning the engine is served through the proxy.
pub(crate) fn active(session_key: &str) -> bool {
    PROXY
        .get()
        .and_then(Option::as_ref)
        .is_some_and(|proxy| proxy.active(session_key))
}

impl Proxy {
    /// Starts a proxy on an ephemeral loopback port with its own small runtime.
    pub(crate) fn start() -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("video-source-proxy")
            .enable_all()
            .build()?;
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let shared = Arc::new(Shared::new(port));
        let listener = {
            let _context = runtime.enter();
            tokio::net::TcpListener::from_std(listener)?
        };
        runtime.spawn(server::serve(listener, shared.clone()));
        tracing::debug!("source proxy started on loopback");
        Ok(Self { runtime, shared })
    }

    /// Registers an http(s) source; see [`route`].
    pub(crate) fn route(&self, payload: NativeOpenRequest) -> (NativeOpenRequest, Pending) {
        let registered = (|| {
            let upstream = Url::parse(&payload.uri).ok()?;
            let token = random_token()?;
            let headers = upstream_headers(&payload)?;
            let client = {
                let _context = self.runtime.enter();
                client(&payload, &upstream)?
            };
            let route = Arc::new(Route::new(
                token.clone(),
                payload.session_key.clone(),
                client,
                headers,
            ));
            let uri = self.shared.proxy_url(&route, Resource::Playlist, &upstream);
            self.shared.routes.lock().push(route);
            Some((uri, token))
        })();
        let Some((uri, token)) = registered else {
            tracing::warn!("source could not be registered with the proxy; opening directly");
            return (payload, Pending(None));
        };
        let mut proxied = payload;
        proxied.uri = uri;
        proxied.headers.clear();
        proxied.cookies = None;
        proxied.user_agent = None;
        proxied.referrer = None;
        proxied.tls_ca_file = None;
        (proxied, Pending(Some(token)))
    }

    /// Settles a registration. When the engine accepted the source, every
    /// older route is retired (the singleton engine no longer plays them);
    /// when it did not, only this registration is discarded and the previous
    /// source keeps being served.
    pub(crate) fn settle(&self, pending: Pending, opened: bool) {
        self.shared.routes.lock().retain(|route| {
            let current = pending.0.as_deref() == Some(route.token.as_str());
            let keep = if opened { current } else { !current };
            if !keep {
                route.close();
            }
            keep
        });
    }

    /// Ends the routes released by a `native_close` presenting `session_key`,
    /// with the engine's late-cleanup rule: a route owned by another (newer)
    /// session survives; an unowned route is always released.
    pub(crate) fn release(&self, session_key: &str) {
        self.shared.routes.lock().retain(|route| {
            let released = route.session_key.is_empty() || route.session_key == session_key;
            if released {
                route.close();
            }
            !released
        });
    }

    pub(crate) fn shutdown(&self) {
        for route in self.shared.routes.lock().drain(..) {
            route.close();
        }
    }

    pub(crate) fn active(&self, session_key: &str) -> bool {
        self.shared
            .routes
            .lock()
            .iter()
            .any(|route| route.session_key == session_key)
    }
}

fn random_token() -> Option<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    Some(server::hex_encode(&bytes))
}

/// The validated source authorization as upstream request headers.
pub(super) fn upstream_headers(payload: &NativeOpenRequest) -> Option<HeaderMap> {
    let mut headers = HeaderMap::new();
    let mut insert = |name: &str, value: &str| -> Option<()> {
        let name = HeaderName::from_bytes(name.as_bytes()).ok()?;
        if !headers.contains_key(&name) {
            headers.insert(name, HeaderValue::from_bytes(value.as_bytes()).ok()?);
        }
        Some(())
    };
    for (name, value) in &payload.headers {
        insert(name, value)?;
    }
    if let Some(cookies) = &payload.cookies {
        insert("cookie", cookies)?;
    }
    if let Some(referrer) = &payload.referrer {
        insert("referer", referrer)?;
    }
    insert(
        "user-agent",
        payload.user_agent.as_deref().unwrap_or(DEFAULT_USER_AGENT),
    )?;
    Some(headers)
}

pub(super) fn client(payload: &NativeOpenRequest, upstream: &Url) -> Option<reqwest::Client> {
    // A public source may not redirect the proxy into the local network or
    // to the proxy itself; a source that is already local may.
    let allow_private = is_private_host(upstream);
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else if !matches!(attempt.url().scheme(), "http" | "https") {
            attempt.error("redirect to a non-http scheme")
        } else if !allow_private && is_private_host(attempt.url()) {
            attempt.error("redirect into the local network")
        } else {
            attempt.follow()
        }
    });
    let mut builder = reqwest::Client::builder()
        .redirect(policy)
        .connect_timeout(server::CONNECT_TIMEOUT)
        .read_timeout(server::READ_TIMEOUT)
        .pool_max_idle_per_host(4);
    if let Some(ca_file) = payload.tls_ca_file.as_deref() {
        match std::fs::read(ca_file)
            .ok()
            .and_then(|pem| reqwest::Certificate::from_pem_bundle(&pem).ok())
        {
            Some(certificates) => {
                for certificate in certificates {
                    builder = builder.add_root_certificate(certificate);
                }
            }
            None => tracing::error!("failed to load configured TLS trust database"),
        }
    }
    builder.build().ok()
}

fn is_private_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => is_private_ip(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => is_private_ip(IpAddr::V6(ip)),
        Some(url::Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain == "localhost" || domain.ends_with(".localhost")
        }
        None => true,
    }
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                // Carrier-grade NAT, 100.64.0.0/10.
                || (ip.octets()[0] == 100 && ip.octets()[1] & 0xc0 == 64)
        }
        IpAddr::V6(ip) => {
            let segment = ip.segments()[0];
            ip.is_loopback()
                || ip.is_unspecified()
                || segment & 0xfe00 == 0xfc00
                || segment & 0xffc0 == 0xfe80
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|ip| is_private_ip(IpAddr::V4(ip)))
                || ip == Ipv6Addr::LOCALHOST
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(uri: &str, proxy: Option<bool>) -> NativeOpenRequest {
        let mut payload: NativeOpenRequest = serde_json::from_value(serde_json::json!({
            "uri": uri, "x": 0, "y": 0, "width": 1, "height": 1,
            "headers": {"Authorization": "Bearer fixture"},
            "cookies": "a=b", "referrer": "https://ref.example/", "userAgent": "Fixture UA",
            "sessionKey": "session-a"
        }))
        .unwrap();
        payload.source_proxy = proxy;
        payload
    }

    #[test]
    fn only_http_hls_is_proxied_unless_requested() {
        assert!(wants_proxy(&request(
            "https://cdn.example/live/1.m3u8?t=1",
            None
        )));
        assert!(wants_proxy(&request("http://cdn.example/LIST.M3U", None)));
        assert!(!wants_proxy(&request(
            "https://cdn.example/movie.mkv",
            None
        )));
        assert!(!wants_proxy(&request(
            "https://cdn.example/live/1.m3u8",
            Some(false)
        )));
        assert!(wants_proxy(&request(
            "https://cdn.example/play?id=1",
            Some(true)
        )));
        assert!(!wants_proxy(&request("file:///tmp/a.m3u8", Some(true))));
    }

    #[test]
    fn upstream_headers_fold_in_the_source_authorization() {
        let headers = upstream_headers(&request("https://cdn.example/a.m3u8", None)).unwrap();
        assert_eq!(headers["authorization"], "Bearer fixture");
        assert_eq!(headers["cookie"], "a=b");
        assert_eq!(headers["referer"], "https://ref.example/");
        assert_eq!(headers["user-agent"], "Fixture UA");
        let mut bare = request("https://cdn.example/a.m3u8", None);
        bare.user_agent = None;
        assert_eq!(
            upstream_headers(&bare).unwrap()["user-agent"],
            DEFAULT_USER_AGENT
        );
    }

    #[test]
    fn private_hosts_are_recognised() {
        for url in [
            "http://127.0.0.1/",
            "http://10.1.2.3/",
            "http://192.168.88.1/",
            "http://169.254.1.1/",
            "http://100.64.0.1/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:10.0.0.1]/",
            "http://localhost:8080/",
        ] {
            assert!(is_private_host(&Url::parse(url).unwrap()), "{url}");
        }
        for url in [
            "https://8.8.8.8/",
            "https://cdn.example/",
            "http://[2001:db8::1]/",
        ] {
            assert!(!is_private_host(&Url::parse(url).unwrap()), "{url}");
        }
    }

    #[test]
    fn host_shutdown_retires_every_current_and_pending_capability() {
        let proxy = Proxy::start().unwrap();
        let (first, _first_pending) = proxy.route(request("http://127.0.0.1:9/a.m3u8", None));
        let mut next = request("http://127.0.0.1:9/b.m3u8", None);
        next.session_key = "session-b".into();
        let (second, _second_pending) = proxy.route(next);
        assert!(proxy.active("session-a") && proxy.active("session-b"));
        proxy.shutdown();
        proxy.shutdown();
        assert!(!proxy.active("session-a") && !proxy.active("session-b"));
        proxy.runtime.block_on(async {
            for uri in [first.uri, second.uri] {
                let response = reqwest::Client::new().get(uri).send().await.unwrap();
                assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
            }
        });
    }

    #[test]
    fn routed_sources_lose_their_credentials_and_settle_by_ownership() {
        let proxy = Proxy::start().unwrap();
        let (first, first_pending) = proxy.route(request("http://127.0.0.1:9/a.m3u8", None));
        assert!(first_pending.proxied());
        assert!(first.uri.starts_with("http://127.0.0.1:"));
        assert!(!first.uri.contains("a.m3u8"));
        assert!(first.headers.is_empty());
        assert!(first.cookies.is_none() && first.user_agent.is_none() && first.referrer.is_none());
        proxy.settle(first_pending, true);
        assert!(proxy.active("session-a"));

        // A failed replacement keeps the playing route.
        let mut next = request("http://127.0.0.1:9/b.m3u8", None);
        next.session_key = "session-b".into();
        let (_, pending) = proxy.route(next.clone());
        proxy.settle(pending, false);
        assert!(proxy.active("session-a") && !proxy.active("session-b"));

        // A successful replacement retires it; late cleanup from the older
        // session leaves the newer route alone.
        let (_, pending) = proxy.route(next);
        proxy.settle(pending, true);
        assert!(!proxy.active("session-a") && proxy.active("session-b"));
        proxy.release("session-a");
        assert!(proxy.active("session-b"));
        proxy.release("session-b");
        assert!(!proxy.active("session-b"));

        // Direct files never reach the proxy.
        let (direct, pending) = route(request("https://cdn.example/movie.mp4", None));
        assert!(!pending.proxied());
        assert_eq!(direct.uri, "https://cdn.example/movie.mp4");
        assert!(!direct.headers.is_empty());
    }
}
