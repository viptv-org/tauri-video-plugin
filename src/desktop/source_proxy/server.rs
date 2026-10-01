//! The loopback HTTP server and the upstream fetch/clean/stream path.
//!
//! Nothing here logs a URL, header, cookie or path: failures are reported as
//! fixed messages with booleans only.

use std::{
    collections::HashMap,
    convert::Infallible,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use hyper::{
    body::{Bytes, Frame, Incoming},
    header::{self, HeaderMap, HeaderValue},
    service::service_fn,
    Method, Request, Response, StatusCode,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use parking_lot::Mutex;
use reqwest::Url;
use tokio::sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore};

use super::{
    playlist::{self, Resource, MAX_PLAYLIST_BYTES},
    sniff::{self, SegmentKind, SNIFF_LIMIT},
};

/// Simultaneous upstream requests across all routes.
const MAX_UPSTREAM: usize = 6;
/// Simultaneous engine connections to the proxy.
const MAX_CONNECTIONS: usize = 32;
const UPSTREAM_QUEUE_TIMEOUT: Duration = Duration::from_secs(20);
pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const READ_TIMEOUT: Duration = Duration::from_secs(30);
const PLAYLIST_TIMEOUT: Duration = Duration::from_secs(30);
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Remembered disguise prefixes per route, so later byte ranges of the same
/// segment are shifted past the stripped bytes.
const MAX_REMEMBERED_SEGMENTS: usize = 1024;

/// One proxied source: its capability token, upstream client and headers.
pub(super) struct Route {
    pub token: String,
    pub session_key: String,
    pub client: reqwest::Client,
    pub headers: HeaderMap,
    closed: watch::Sender<bool>,
    segments: Mutex<HashMap<String, (u64, Option<SegmentKind>)>>,
}

impl Route {
    pub fn new(
        token: String,
        session_key: String,
        client: reqwest::Client,
        headers: HeaderMap,
    ) -> Self {
        Self {
            token,
            session_key,
            client,
            headers,
            closed: watch::Sender::new(false),
            segments: Mutex::new(HashMap::new()),
        }
    }

    /// Ends every in-flight transfer of this route.
    pub fn close(&self) {
        self.closed.send_replace(true);
    }

    fn remember(&self, url: &Url, shift: u64, kind: Option<SegmentKind>) {
        let mut segments = self.segments.lock();
        if segments.len() >= MAX_REMEMBERED_SEGMENTS {
            segments.clear();
        }
        segments.insert(url.as_str().to_owned(), (shift, kind));
    }

    fn remembered(&self, url: &Url) -> Option<(u64, Option<SegmentKind>)> {
        self.segments.lock().get(url.as_str()).copied()
    }
}

pub(super) struct Shared {
    pub port: u16,
    pub routes: Mutex<Vec<Arc<Route>>>,
    upstream: Arc<Semaphore>,
}

impl Shared {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            routes: Mutex::new(Vec::new()),
            upstream: Arc::new(Semaphore::new(MAX_UPSTREAM)),
        }
    }

    fn route(&self, token: &str) -> Option<Arc<Route>> {
        self.routes
            .lock()
            .iter()
            .find(|route| route.token == token)
            .cloned()
    }

    /// The proxy URL an engine uses to fetch `url` through `route`.
    pub fn proxy_url(&self, route: &Route, resource: Resource, url: &Url) -> String {
        let (kind, name) = match resource {
            Resource::Playlist => ("p", "index.m3u8"),
            Resource::Segment { fmp4: false } => ("s", "segment.ts"),
            Resource::Segment { fmp4: true } => ("m", "segment.mp4"),
            Resource::Raw => ("k", "resource.bin"),
        };
        format!(
            "http://127.0.0.1:{}/{}/{kind}/{}/{name}",
            self.port,
            route.token,
            hex_encode(url.as_str().as_bytes())
        )
    }

    fn blocked_url(&self, route: &Route) -> String {
        format!("http://127.0.0.1:{}/{}/blocked", self.port, route.token)
    }
}

/// Accepts engine connections until the process exits.
pub(super) async fn serve(listener: tokio::net::TcpListener, shared: Arc<Shared>) {
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            tracing::warn!("source proxy refused a connection: connection limit reached");
            continue;
        };
        let shared = shared.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let service = service_fn(move |request| handle(shared.clone(), request));
            let _ = hyper::server::conn::http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(HEADER_READ_TIMEOUT)
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}

async fn handle(
    shared: Arc<Shared>,
    request: Request<Incoming>,
) -> Result<Response<ProxyBody>, Infallible> {
    Ok(dispatch(&shared, &request).await.unwrap_or_else(empty))
}

async fn dispatch(
    shared: &Shared,
    request: &Request<Incoming>,
) -> Result<Response<ProxyBody>, StatusCode> {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return Err(StatusCode::METHOD_NOT_ALLOWED);
    }
    let mut parts = request.uri().path().trim_start_matches('/').split('/');
    let (Some(token), Some(kind), Some(encoded)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(StatusCode::NOT_FOUND);
    };
    let route = shared.route(token).ok_or(StatusCode::NOT_FOUND)?;
    let url = hex_decode(encoded)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| Url::parse(&text).ok())
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .ok_or(StatusCode::NOT_FOUND)?;
    let range = request
        .headers()
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(EngineRange::parse);
    let fetch = Fetch {
        shared,
        route: &route,
        url,
    };
    match kind {
        "k" => {
            fetch
                .passthrough(range.and_then(EngineRange::into_raw))
                .await
        }
        "p" | "s" | "m" => fetch.media(range).await,
        _ => Err(StatusCode::NOT_FOUND),
    }
}

/// The byte range an engine asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum EngineRange {
    /// `bytes=start-` or `bytes=start-end`.
    From(u64, Option<u64>),
    /// Suffix or multi-range requests, forwarded verbatim.
    Other(String),
}

impl EngineRange {
    pub fn parse(value: &str) -> Self {
        let parsed = value.trim().strip_prefix("bytes=").and_then(|spec| {
            let (start, end) = spec.split_once('-')?;
            let start = start.trim().parse().ok()?;
            let end = match end.trim() {
                "" => None,
                end => Some(end.parse::<u64>().ok().filter(|end| *end >= start)?),
            };
            Some(Self::From(start, end))
        });
        parsed.unwrap_or_else(|| Self::Other(value.to_owned()))
    }

    fn into_raw(self) -> Option<String> {
        Some(match self {
            Self::From(start, Some(end)) => format!("bytes={start}-{end}"),
            Self::From(start, None) => format!("bytes={start}-"),
            Self::Other(raw) => raw,
        })
    }
}

/// Where an upstream body sits within the upstream resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Layout {
    /// Upstream offset of the first body byte.
    pub start: u64,
    /// Body length, when the upstream declared it.
    pub length: Option<u64>,
    /// Whole-resource length, when known.
    pub total: Option<u64>,
}

impl Layout {
    pub fn of(status: StatusCode, headers: &HeaderMap) -> Self {
        let length = headers
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse().ok());
        if status == StatusCode::PARTIAL_CONTENT {
            if let Some((start, end, total)) = headers
                .get(header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_content_range)
            {
                return Self {
                    start,
                    length: Some(end - start + 1),
                    total,
                };
            }
        }
        Self {
            start: 0,
            length,
            total: length,
        }
    }
}

fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let (range, total) = value.trim().strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (start, end) = (start.parse().ok()?, end.parse::<u64>().ok()?);
    (end >= start).then_some((start, end, total.parse().ok()))
}

/// The engine-facing view of a cleaned body: which upstream bytes to drop,
/// how many to emit, and the headers that describe them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Mapped {
    pub skip: u64,
    pub length: Option<u64>,
    pub status: StatusCode,
    pub content_range: Option<String>,
}

/// Maps an upstream body onto engine coordinates, where engine byte 0 is
/// upstream byte `shift` (the stripped disguise prefix).
///
/// `want` is the upstream range wanted (`start`, inclusive `end`) and
/// `engine_start` the first engine byte requested, if the engine sent a Range.
pub(super) fn map_window(
    layout: Layout,
    shift: u64,
    want: (u64, Option<u64>),
    engine_start: Option<u64>,
) -> Option<Mapped> {
    let (want_start, want_end) = want;
    let skip = want_start.checked_sub(layout.start)?;
    let available = layout.length.map(|length| length.saturating_sub(skip));
    let length = match (want_end, available) {
        (Some(end), Some(available)) => Some((end - want_start + 1).min(available)),
        (Some(end), None) => Some(end - want_start + 1),
        (None, available) => available,
    };
    let total = layout.total.map(|total| total.saturating_sub(shift));
    match engine_start {
        Some(start) if start > 0 || length.is_some() => {
            let length = length?;
            let end = (start + length).checked_sub(1)?;
            let total = total.map_or_else(|| "*".to_owned(), |total| total.to_string());
            Some(Mapped {
                skip,
                length: Some(length),
                status: StatusCode::PARTIAL_CONTENT,
                content_range: Some(format!("bytes {start}-{end}/{total}")),
            })
        }
        _ => Some(Mapped {
            skip,
            length,
            status: StatusCode::OK,
            content_range: None,
        }),
    }
}

struct Fetch<'a> {
    shared: &'a Shared,
    route: &'a Arc<Route>,
    url: Url,
}

impl Fetch<'_> {
    async fn send(
        &self,
        range: Option<String>,
    ) -> Result<(reqwest::Response, OwnedSemaphorePermit), StatusCode> {
        let permit = tokio::time::timeout(
            UPSTREAM_QUEUE_TIMEOUT,
            self.shared.upstream.clone().acquire_owned(),
        )
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let mut request = self
            .route
            .client
            .get(self.url.clone())
            .headers(self.route.headers.clone());
        if let Some(range) = range {
            request = request.header(header::RANGE, range);
        }
        let mut closed = self.route.closed.subscribe();
        let response = tokio::select! {
            _ = closed.wait_for(|closed| *closed) => return Err(StatusCode::GONE),
            response = request.send() => response,
        };
        let response = response.map_err(|error| {
            tracing::debug!(
                timeout = error.is_timeout(),
                connect = error.is_connect(),
                redirect = error.is_redirect(),
                "source proxy upstream request failed"
            );
            StatusCode::BAD_GATEWAY
        })?;
        if !response.status().is_success() {
            tracing::debug!(
                status = response.status().as_u16(),
                "source proxy upstream refused a request"
            );
            return Err(response.status());
        }
        Ok((response, permit))
    }

    /// Keys and opaque resources: the engine's range and the upstream bytes
    /// are forwarded unchanged.
    async fn passthrough(&self, range: Option<String>) -> Result<Response<ProxyBody>, StatusCode> {
        let (response, permit) = self.send(range).await?;
        let mut builder = Response::builder().status(response.status());
        for name in [
            header::CONTENT_TYPE,
            header::CONTENT_LENGTH,
            header::CONTENT_RANGE,
            header::ACCEPT_RANGES,
        ] {
            if let Some(value) = response.headers().get(&name) {
                builder = builder.header(name, value.clone());
            }
        }
        let body = self.stream(Bytes::new(), response, Window::all(), permit);
        builder
            .body(body)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    }

    /// Playlists and segments: playlists are rewritten, segments are cleaned
    /// of any disguise prefix and labelled with their real media type.
    async fn media(&self, range: Option<EngineRange>) -> Result<Response<ProxyBody>, StatusCode> {
        let remembered = self.route.remembered(&self.url);
        match (range, remembered) {
            (Some(EngineRange::Other(raw)), _) => self.passthrough(Some(raw)).await,
            (range, Some((shift, kind))) => {
                let (start, end) = match range {
                    Some(EngineRange::From(start, end)) => (start, end),
                    _ => (0, None),
                };
                let engine_start = range_start(&range);
                self.window(
                    shift,
                    kind,
                    (start + shift, end.map(|end| end + shift)),
                    engine_start,
                )
                .await
            }
            // An unseen segment entered mid-way: its prefix is unknown.
            (Some(EngineRange::From(start, end)), None) if start > 0 => {
                self.passthrough(EngineRange::From(start, end).into_raw())
                    .await
            }
            (range, None) => {
                let end = match range {
                    Some(EngineRange::From(_, end)) => end,
                    _ => None,
                };
                self.sniffed(end, range.is_some()).await
            }
        }
    }

    /// A segment whose prefix is already known.
    async fn window(
        &self,
        shift: u64,
        kind: Option<SegmentKind>,
        want: (u64, Option<u64>),
        engine_start: Option<u64>,
    ) -> Result<Response<ProxyBody>, StatusCode> {
        let upstream_range = (want.0 > 0 || want.1.is_some()).then(|| {
            format!(
                "bytes={}-{}",
                want.0,
                want.1.map_or(String::new(), |end| end.to_string())
            )
        });
        let (response, permit) = self.send(upstream_range).await?;
        let layout = Layout::of(response.status(), response.headers());
        let mapped =
            map_window(layout, shift, want, engine_start).ok_or(StatusCode::BAD_GATEWAY)?;
        let content_type = segment_content_type(kind, response.headers());
        let accept_ranges = response.headers().get(header::ACCEPT_RANGES).cloned();
        let body = self.stream(
            Bytes::new(),
            response,
            Window::new(mapped.skip, mapped.length),
            permit,
        );
        respond(mapped, content_type, accept_ranges, body)
    }

    /// A segment fetched from its first byte: sniff it, strip the prefix.
    async fn sniffed(
        &self,
        engine_end: Option<u64>,
        engine_ranged: bool,
    ) -> Result<Response<ProxyBody>, StatusCode> {
        // A bounded engine range is widened by the sniff window so the
        // cleaned body can still fill it.
        let upstream_end = engine_end.map(|end| end + SNIFF_LIMIT as u64);
        let (mut response, permit) = self
            .send(upstream_end.map(|end| format!("bytes=0-{end}")))
            .await?;
        let layout = Layout::of(response.status(), response.headers());
        if layout.start != 0 {
            return Err(StatusCode::BAD_GATEWAY);
        }
        let mut closed = self.route.closed.subscribe();
        let (head, ended) = read_head(&mut response, SNIFF_LIMIT, &mut closed).await?;
        if playlist::is_playlist(&head) {
            return self.playlist(head, ended, response, &mut closed).await;
        }
        let whole = layout.total.is_none() || layout.length == layout.total;
        let found = sniff::sniff(&head, ended && whole);
        if found.offset > 0 {
            tracing::debug!(
                stripped = found.offset,
                "source proxy removed a disguise prefix from a segment"
            );
        }
        let shift = found.offset as u64;
        self.route.remember(&self.url, shift, found.kind);
        let want_end = engine_end.map(|end| end + shift);
        let mapped = map_window(layout, shift, (shift, want_end), engine_ranged.then_some(0))
            .ok_or(StatusCode::BAD_GATEWAY)?;
        let content_type = segment_content_type(found.kind, response.headers());
        let accept_ranges = response.headers().get(header::ACCEPT_RANGES).cloned();
        let body = self.stream(
            Bytes::from(head),
            response,
            Window::new(mapped.skip, mapped.length),
            permit,
        );
        respond(mapped, content_type, accept_ranges, body)
    }

    async fn playlist(
        &self,
        mut text: Vec<u8>,
        mut ended: bool,
        mut response: reqwest::Response,
        closed: &mut watch::Receiver<bool>,
    ) -> Result<Response<ProxyBody>, StatusCode> {
        let base = response.url().clone();
        let deadline = tokio::time::Instant::now() + PLAYLIST_TIMEOUT;
        while !ended {
            if text.len() > MAX_PLAYLIST_BYTES {
                tracing::warn!("source proxy refused an oversized playlist");
                return Err(StatusCode::BAD_GATEWAY);
            }
            let chunk = tokio::select! {
                _ = closed.wait_for(|closed| *closed) => return Err(StatusCode::GONE),
                chunk = tokio::time::timeout_at(deadline, response.chunk()) => chunk,
            };
            match chunk {
                Ok(Ok(Some(chunk))) => text.extend_from_slice(&chunk),
                Ok(Ok(None)) => ended = true,
                _ => return Err(StatusCode::BAD_GATEWAY),
            }
        }
        if text.len() > MAX_PLAYLIST_BYTES {
            return Err(StatusCode::BAD_GATEWAY);
        }
        let text = String::from_utf8_lossy(&text);
        let blocked = self.shared.blocked_url(self.route);
        let route = |resource, url: &Url| self.shared.proxy_url(self.route, resource, url);
        let rewritten = playlist::rewrite(&text, &base, &route, &blocked);
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::CONTENT_LENGTH, rewritten.len())
            .body(ProxyBody::full(Bytes::from(rewritten)))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    }

    /// Streams `first` and then the rest of `response` through `window`.
    /// The upstream permit is held until the transfer ends; closing the route
    /// or dropping the engine connection ends it immediately.
    fn stream(
        &self,
        first: Bytes,
        mut response: reqwest::Response,
        mut window: Window,
        permit: OwnedSemaphorePermit,
    ) -> ProxyBody {
        let (sender, receiver) = mpsc::channel::<io::Result<Bytes>>(4);
        let mut closed = self.route.closed.subscribe();
        tokio::spawn(async move {
            let _permit = permit;
            let mut next = Some(first);
            loop {
                if let Some(chunk) = next.take().and_then(|chunk| window.apply(chunk)) {
                    let sent = tokio::select! {
                        _ = closed.wait_for(|closed| *closed) => false,
                        sent = sender.send(Ok(chunk)) => sent.is_ok(),
                    };
                    if !sent {
                        return;
                    }
                }
                if window.done() {
                    return;
                }
                let chunk = tokio::select! {
                    _ = closed.wait_for(|closed| *closed) => return,
                    chunk = tokio::time::timeout(READ_TIMEOUT, response.chunk()) => chunk,
                };
                match chunk {
                    Ok(Ok(Some(chunk))) => next = Some(chunk),
                    Ok(Ok(None)) if window.satisfied() => return,
                    _ => {
                        tracing::debug!("source proxy upstream body ended early");
                        let _ = sender
                            .send(Err(io::Error::other("upstream body ended early")))
                            .await;
                        return;
                    }
                }
            }
        });
        ProxyBody::stream(receiver)
    }
}

fn range_start(range: &Option<EngineRange>) -> Option<u64> {
    match range {
        Some(EngineRange::From(start, _)) => Some(*start),
        _ => None,
    }
}

async fn read_head(
    response: &mut reqwest::Response,
    limit: usize,
    closed: &mut watch::Receiver<bool>,
) -> Result<(Vec<u8>, bool), StatusCode> {
    let mut head = Vec::with_capacity(limit);
    while head.len() < limit {
        let chunk = tokio::select! {
            _ = closed.wait_for(|closed| *closed) => return Err(StatusCode::GONE),
            chunk = tokio::time::timeout(READ_TIMEOUT, response.chunk()) => chunk,
        };
        match chunk {
            Ok(Ok(Some(chunk))) => head.extend_from_slice(&chunk),
            Ok(Ok(None)) => return Ok((head, true)),
            _ => return Err(StatusCode::BAD_GATEWAY),
        }
    }
    Ok((head, false))
}

/// The real media type of a segment. When the bytes were not recognised, a
/// disguise type (image, font, stylesheet, page) is never repeated to the
/// engine; any other upstream type (e.g. WebVTT) is kept.
fn segment_content_type(kind: Option<SegmentKind>, headers: &HeaderMap) -> HeaderValue {
    if let Some(kind) = kind {
        return HeaderValue::from_static(kind.content_type());
    }
    headers
        .get(header::CONTENT_TYPE)
        .filter(|value| {
            let value = value.to_str().unwrap_or("").to_ascii_lowercase();
            !(value.starts_with("image/")
                || value.starts_with("font/")
                || value.starts_with("text/css")
                || value.starts_with("text/html")
                || value.starts_with("application/font"))
        })
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("application/octet-stream"))
}

fn respond(
    mapped: Mapped,
    content_type: HeaderValue,
    accept_ranges: Option<HeaderValue>,
    body: ProxyBody,
) -> Result<Response<ProxyBody>, StatusCode> {
    let mut builder = Response::builder()
        .status(mapped.status)
        .header(header::CONTENT_TYPE, content_type);
    if let Some(length) = mapped.length {
        builder = builder.header(header::CONTENT_LENGTH, length);
    }
    if let Some(range) = mapped.content_range {
        builder = builder.header(header::CONTENT_RANGE, range);
    }
    if let Some(accept_ranges) = accept_ranges {
        builder = builder.header(header::ACCEPT_RANGES, accept_ranges);
    }
    builder
        .body(body)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn empty(status: StatusCode) -> Response<ProxyBody> {
    let mut response = Response::new(ProxyBody::full(Bytes::new()));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
    response
}

/// Drops `skip` leading bytes and stops after `remaining` bytes.
#[derive(Clone, Copy, Debug)]
pub(super) struct Window {
    skip: u64,
    remaining: Option<u64>,
}

impl Window {
    pub fn new(skip: u64, remaining: Option<u64>) -> Self {
        Self { skip, remaining }
    }

    fn all() -> Self {
        Self::new(0, None)
    }

    pub fn apply(&mut self, mut chunk: Bytes) -> Option<Bytes> {
        if self.skip > 0 {
            let dropped = self.skip.min(chunk.len() as u64);
            chunk = chunk.slice(dropped as usize..);
            self.skip -= dropped;
        }
        if let Some(remaining) = self.remaining.as_mut() {
            chunk.truncate((*remaining).min(chunk.len() as u64) as usize);
            *remaining -= chunk.len() as u64;
        }
        (!chunk.is_empty()).then_some(chunk)
    }

    fn done(&self) -> bool {
        self.remaining == Some(0)
    }

    /// Whether ending the upstream body now is a complete transfer.
    fn satisfied(&self) -> bool {
        self.remaining.is_none_or(|remaining| remaining == 0)
    }
}

/// A response body that is either a buffered playlist or a streamed segment.
pub(super) struct ProxyBody {
    full: Option<Bytes>,
    stream: Option<mpsc::Receiver<io::Result<Bytes>>>,
}

impl ProxyBody {
    fn full(bytes: Bytes) -> Self {
        Self {
            full: Some(bytes),
            stream: None,
        }
    }

    fn stream(receiver: mpsc::Receiver<io::Result<Bytes>>) -> Self {
        Self {
            full: None,
            stream: Some(receiver),
        }
    }
}

impl hyper::body::Body for ProxyBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        if let Some(bytes) = self.full.take() {
            return Poll::Ready((!bytes.is_empty()).then(|| Ok(Frame::data(bytes))));
        }
        match self.stream.as_mut() {
            Some(receiver) => receiver
                .poll_recv(context)
                .map(|chunk| chunk.map(|chunk| chunk.map(Frame::data))),
            None => Poll::Ready(None),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.stream.is_none() && self.full.as_ref().is_none_or(Bytes::is_empty)
    }
}

pub(super) fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|byte| {
            [
                DIGITS[usize::from(byte >> 4)],
                DIGITS[usize::from(byte & 0x0f)],
            ]
        })
        .map(char::from)
        .collect()
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    let digit = |byte: u8| char::from(byte).to_digit(16).map(|digit| digit as u8);
    text.as_bytes()
        .chunks(2)
        .map(|pair| Some(digit(pair[0])? << 4 | digit(pair[1])?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_ranges_parse_or_pass_through() {
        assert_eq!(EngineRange::parse("bytes=0-"), EngineRange::From(0, None));
        assert_eq!(
            EngineRange::parse("bytes=10-99"),
            EngineRange::From(10, Some(99))
        );
        assert_eq!(
            EngineRange::parse("bytes=-500"),
            EngineRange::Other("bytes=-500".into())
        );
        assert_eq!(
            EngineRange::parse("bytes=9-3"),
            EngineRange::Other("bytes=9-3".into())
        );
    }

    #[test]
    fn layouts_follow_status_and_content_range() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("1000"));
        assert_eq!(
            Layout::of(StatusCode::OK, &headers),
            Layout {
                start: 0,
                length: Some(1000),
                total: Some(1000)
            }
        );
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("100"));
        headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_static("bytes 200-299/1000"),
        );
        assert_eq!(
            Layout::of(StatusCode::PARTIAL_CONTENT, &headers),
            Layout {
                start: 200,
                length: Some(100),
                total: Some(1000)
            }
        );
    }

    #[test]
    fn a_whole_disguised_segment_maps_to_a_shorter_plain_body() {
        let layout = Layout {
            start: 0,
            length: Some(1000),
            total: Some(1000),
        };
        // No engine range: 200 with the prefix-less length.
        assert_eq!(
            map_window(layout, 70, (70, None), None).unwrap(),
            Mapped {
                skip: 70,
                length: Some(930),
                status: StatusCode::OK,
                content_range: None
            }
        );
        // Engine asked from 0: 206 over the cleaned resource.
        assert_eq!(
            map_window(layout, 70, (70, None), Some(0)).unwrap(),
            Mapped {
                skip: 70,
                length: Some(930),
                status: StatusCode::PARTIAL_CONTENT,
                content_range: Some("bytes 0-929/930".into())
            }
        );
    }

    #[test]
    fn later_ranges_are_shifted_past_the_known_prefix() {
        // Engine wants 100-199; upstream served 170-269 of 1000.
        let layout = Layout {
            start: 170,
            length: Some(100),
            total: Some(1000),
        };
        assert_eq!(
            map_window(layout, 70, (170, Some(269)), Some(100)).unwrap(),
            Mapped {
                skip: 0,
                length: Some(100),
                status: StatusCode::PARTIAL_CONTENT,
                content_range: Some("bytes 100-199/930".into())
            }
        );
        // An upstream that ignores Range sends everything: skip to the window.
        let whole = Layout {
            start: 0,
            length: Some(1000),
            total: Some(1000),
        };
        assert_eq!(
            map_window(whole, 70, (170, Some(269)), Some(100)).unwrap(),
            Mapped {
                skip: 170,
                length: Some(100),
                status: StatusCode::PARTIAL_CONTENT,
                content_range: Some("bytes 100-199/930".into())
            }
        );
        // An upstream that starts after the wanted byte cannot serve it.
        assert!(map_window(layout, 70, (100, None), Some(30)).is_none());
    }

    #[test]
    fn bounded_first_ranges_are_trimmed_after_the_prefix() {
        // Engine asked 0-99; upstream (asked 0-99+window) sent 0-999 of 5000.
        let layout = Layout {
            start: 0,
            length: Some(1000),
            total: Some(5000),
        };
        assert_eq!(
            map_window(layout, 70, (70, Some(169)), Some(0)).unwrap(),
            Mapped {
                skip: 70,
                length: Some(100),
                status: StatusCode::PARTIAL_CONTENT,
                content_range: Some("bytes 0-99/4930".into())
            }
        );
    }

    #[test]
    fn windows_skip_and_truncate_across_chunks() {
        let mut window = Window::new(5, Some(6));
        assert_eq!(window.apply(Bytes::from_static(b"abc")), None);
        assert_eq!(
            window.apply(Bytes::from_static(b"defgh")).as_deref(),
            Some(&b"fgh"[..])
        );
        assert_eq!(
            window.apply(Bytes::from_static(b"ijklm")).as_deref(),
            Some(&b"ijk"[..])
        );
        assert!(window.done());
    }

    #[test]
    fn hex_round_trips_urls() {
        let url = "https://cdn.example/a b/seg.png?x=1&y=%20";
        assert_eq!(
            hex_decode(&hex_encode(url.as_bytes())).unwrap(),
            url.as_bytes()
        );
        assert!(hex_decode("abc").is_none());
        assert!(hex_decode("zz").is_none());
    }

    #[test]
    fn disguise_content_types_are_never_repeated() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
        assert_eq!(
            segment_content_type(None, &headers),
            "application/octet-stream"
        );
        assert_eq!(
            segment_content_type(Some(SegmentKind::MpegTs), &headers),
            "video/mp2t"
        );
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/vtt"));
        assert_eq!(segment_content_type(None, &headers), "text/vtt");
    }
}
