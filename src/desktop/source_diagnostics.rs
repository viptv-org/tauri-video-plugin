//! Failure-only source diagnostics. No request runs on GTK or during healthy playback.
//! Keep authorization private; only bounded, redacted response text reaches the UI.
use super::source_proxy;
use crate::{models::NativeOpenRequest, Error};
use parking_lot::Mutex;
use reqwest::Url;
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
    time::Duration,
};

const LIMIT: usize = 2048;
const DEADLINE: Duration = Duration::from_secs(2);
#[derive(Clone)]
pub(super) struct SourceReport {
    status: Option<u16>,
    detail: String,
    observed: bool,
}
struct Source {
    request: Arc<NativeOpenRequest>,
    report: Option<SourceReport>,
    checked: bool,
}
static SOURCES: OnceLock<Mutex<HashMap<String, Source>>> = OnceLock::new();
fn sources() -> &'static Mutex<HashMap<String, Source>> {
    SOURCES.get_or_init(Mutex::default)
}
pub(super) fn remember(request: &NativeOpenRequest) {
    let mut sources = sources().lock();
    // The engine owns one current source. Bound abandoned opening attempts too.
    if sources.len() >= 8 {
        sources.clear();
    }
    sources.insert(
        request.session_key.clone(),
        Source {
            request: Arc::new(request.clone()),
            report: None,
            checked: false,
        },
    );
}
pub(super) fn release(key: &str) {
    sources().lock().remove(key);
}
pub(super) fn shutdown() {
    sources().lock().clear();
}
fn record(request: &Arc<NativeOpenRequest>, report: SourceReport) {
    if let Some(source) = sources().lock().get_mut(&request.session_key) {
        if Arc::ptr_eq(&source.request, request) {
            // A diagnostic request must never replace an observed playback response.
            if source
                .report
                .as_ref()
                .is_none_or(|current| !current.observed)
                || report.observed
            {
                source.report = Some(report);
            }
            source.checked = true;
        }
    }
}

/// Enrich an engine rejection on the async command thread, using an observed
/// upstream refusal first. A direct source gets one bounded range GET only on
/// failure. Its result is explicitly described as a diagnostic GET, not a
/// claim about the engine's exact network exchange.
pub(crate) async fn enrich(key: &str, error: Error) -> Error {
    if !matches!(
        error.code(),
        "PIPELINE_FAILED"
            | "SOURCE_OPEN_FAILED"
            | "DECODE_FAILED"
            | "MEDIA_FORMAT_FAILED"
            | "AUTHORIZATION_FAILED"
            | "CONNECTION_FAILED"
            | "SOURCE_UNAVAILABLE"
    ) {
        return error;
    }
    let (request, observed, check) = {
        let mut sources = sources().lock();
        let Some(source) = sources.get_mut(key) else {
            return error;
        };
        let check = !source.checked;
        source.checked = true;
        (source.request.clone(), source.report.clone(), check)
    };
    let report = if let Some(report) = observed {
        Some(report)
    } else if check {
        let report = probe(&request).await;
        if let Some(report) = report.clone() {
            record(&request, report);
        }
        report
    } else {
        None
    };
    // Ignore a late probe from a closed/replaced attempt, even if its key was reused.
    let report = {
        let sources = sources().lock();
        let Some(source) = sources
            .get(key)
            .filter(|source| Arc::ptr_eq(&source.request, &request))
        else {
            return error;
        };
        source.report.clone().or(report)
    };
    let Some(report) = report else {
        return error;
    };
    let code = match report.status.filter(|_| report.observed) {
        Some(401 | 403 | 407) => "AUTHORIZATION_FAILED",
        Some(404 | 410) => "SOURCE_UNAVAILABLE",
        Some(400..=599) => "CONNECTION_FAILED",
        _ => error.code(),
    };
    let engine = if request.backend.as_deref() == Some("mpv") {
        "MPV"
    } else {
        "GStreamer"
    };
    Error::SourceDiagnostic {
        code,
        message: format!("{engine}: {error}\n{}", report.detail),
        original: Box::new(error),
    }
}

async fn probe(request: &NativeOpenRequest) -> Option<SourceReport> {
    let url = Url::parse(&request.uri).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let client = source_proxy::client(request, &url)?;
    let headers = source_proxy::upstream_headers(request)?;
    let result = tokio::time::timeout(DEADLINE, async {
        let response = client
            .get(url)
            .headers(headers)
            .header(reqwest::header::RANGE, "bytes=0-2047")
            .timeout(DEADLINE)
            .send()
            .await;
        match response {
            Ok(response) => response_report(request, response, "Diagnostic GET").await,
            Err(error) => SourceReport {
                status: None,
                detail: transport_detail(&error).into(),
                observed: false,
            },
        }
    })
    .await;
    Some(result.unwrap_or(SourceReport {
        status: None,
        detail: "Diagnostic GET timed out after 2 seconds.".into(),
        observed: false,
    }))
}

fn transport_detail(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "Diagnostic GET: network timeout."
    } else if error.is_redirect() {
        "Diagnostic GET: redirect failed or was rejected by the source redirect policy."
    } else if error.is_connect() {
        "Diagnostic GET: connection failed (DNS, TLS or connection establishment)."
    } else {
        "Diagnostic GET: HTTP transport failed before a response was received."
    }
}

pub(super) async fn observe_response(key: &str, response: reqwest::Response) {
    let request = sources()
        .lock()
        .get(key)
        .map(|source| source.request.clone());
    if let Some(request) = request {
        // Refusals must still reach the engine promptly if their body stalls.
        let status = response.status().as_u16();
        let report = tokio::time::timeout(
            Duration::from_millis(500),
            response_report(&request, response, "Source response"),
        )
        .await
        .unwrap_or(SourceReport {
            status: Some(status),
            detail: format!("Source response: HTTP {status}; response body timed out."),
            observed: true,
        });
        record(&request, report);
    }
}

async fn response_report(
    request: &NativeOpenRequest,
    mut response: reqwest::Response,
    label: &str,
) -> SourceReport {
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "/.+-".contains(*c))
        .take(64)
        .collect::<String>();
    let text = content_type.starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("xml");
    let mut body = Vec::new();
    if text {
        while body.len() < LIMIT {
            match response.chunk().await {
                Ok(Some(bytes)) => {
                    body.extend_from_slice(&bytes[..bytes.len().min(LIMIT - body.len())])
                }
                _ => break,
            }
        }
    }
    let excerpt = safe_excerpt(&String::from_utf8_lossy(&body), request);
    SourceReport {
        status: Some(status.as_u16()),
        observed: label == "Source response",
        detail: format!(
            "{label}: HTTP {} {}{}{}",
            status.as_u16(),
            status.canonical_reason().unwrap_or(""),
            if content_type.is_empty() {
                String::new()
            } else {
                format!(" · {content_type}")
            },
            if excerpt.is_empty() {
                String::new()
            } else {
                format!("\nResponse: {excerpt}")
            }
        ),
    }
}

fn sensitive_key(key: &str) -> bool {
    [
        "token",
        "password",
        "passwd",
        "secret",
        "cookie",
        "authorization",
        "username",
        "credential",
        "signature",
        "url",
        "uri",
        "path",
        "session",
        "api_key",
        "apikey",
        "license",
    ]
    .iter()
    .any(|name| key.to_ascii_lowercase().contains(name))
}
fn redact_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if sensitive_key(key) {
                    *value = serde_json::Value::String("[redacted]".into());
                } else {
                    redact_json(value);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_json(value);
            }
        }
        serde_json::Value::String(text) => *text = redact_inline(text, true),
        _ => {}
    }
}
fn safe_excerpt(raw: &str, request: &NativeOpenRequest) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(raw);
    let structured = parsed.is_ok();
    let mut text = match parsed {
        Ok(mut value) => {
            redact_json(&mut value);
            value.to_string()
        }
        Err(_) if raw.trim_start().starts_with(['{', '[']) => {
            "Response was truncated or was not valid JSON.".into()
        }
        Err(_) => raw.to_owned(),
    };
    let mut secrets = vec![request.uri.clone()];
    if let Ok(url) = Url::parse(&request.uri) {
        secrets.extend(
            url.path_segments()
                .into_iter()
                .flatten()
                .filter(|part| !part.is_empty())
                .map(str::to_owned),
        );
        secrets.extend(url.query_pairs().map(|(_, value)| value.into_owned()));
        secrets.extend([
            url.username().to_owned(),
            url.password().unwrap_or("").to_owned(),
            url.host_str().unwrap_or("").to_owned(),
        ]);
    }
    for value in request
        .headers
        .values()
        .chain(request.cookies.iter())
        .chain(request.referrer.iter())
        .chain(request.user_agent.iter())
    {
        secrets.push(value.clone());
        secrets.extend(
            value
                .split(|c: char| c.is_whitespace() || c == ';' || c == '=')
                .map(str::to_owned),
        );
    }
    // URLs may percent-encode credentials which the upstream echoes decoded.
    let decoded = secrets
        .iter()
        .flat_map(|value| {
            url::form_urlencoded::parse(format!("value={}", value.replace('+', "%2B")).as_bytes())
                .map(|(_, value)| value.into_owned())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    secrets.extend(decoded);
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    for secret in secrets.into_iter().filter(|secret| !secret.is_empty()) {
        text = text.replace(&secret, "[redacted]");
    }
    // Strip HTML tags. Keep text only; the UI renders it as text, never HTML.
    let mut tag = false;
    text = text
        .chars()
        .filter(|c| {
            if *c == '<' {
                tag = true;
                return false;
            }
            if *c == '>' {
                tag = false;
                return false;
            }
            !tag && (!c.is_control() || c.is_whitespace())
        })
        .collect();
    redact_inline(&text, !structured)
        .chars()
        .take(512)
        .collect()
}

fn redact_inline(text: &str, detect_keys: bool) -> String {
    let mut hide_next = false;
    text.split_whitespace()
        .map(|word| {
            let hide = hide_next;
            let key = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
            hide_next = (hide && key.is_empty())
                || detect_keys
                    && (sensitive_key(key)
                        || key.eq_ignore_ascii_case("bearer")
                        || key.eq_ignore_ascii_case("basic"));
            let keyed_secret = detect_keys
                && word
                    .split_once(':')
                    .is_some_and(|(key, _)| sensitive_key(key));
            let opaque = word.len() >= 24
                && word.chars().any(|c| c.is_ascii_digit())
                && word.chars().any(|c| c.is_ascii_alphabetic());
            if hide
                || keyed_secret
                || opaque
                || word.contains("://")
                || word.contains('/')
                || word.contains('@')
                || word.contains('=')
            {
                "[redacted]"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> NativeOpenRequest {
        serde_json::from_value(serde_json::json!({"uri":"https://private.example/live/userabc/passxyz/123.ts?token=tokensecret", "headers":{"Authorization":"Bearer authsecret"}, "cookies":"session=cookiesecret", "sessionKey":"diagnostic-test", "x":0,"y":0,"width":1,"height":1})).unwrap()
    }
    #[test]
    fn response_excerpt_keeps_real_reasons_and_removes_credentials() {
        let text = safe_excerpt(
            r#"{"error":"Account expired","password":"unrelated-secret","url":"https://other.example/a","message":"userabc passxyz tokensecret authsecret cookiesecret"}"#,
            &request(),
        );
        assert!(text.contains("Account expired"));
        for secret in [
            "userabc",
            "passxyz",
            "tokensecret",
            "authsecret",
            "cookiesecret",
            "other.example",
            "unrelated-secret",
        ] {
            assert!(!text.contains(secret), "{text}");
        }
        assert!(safe_excerpt(&"x".repeat(5000), &request()).len() <= 512);
        assert_eq!(
            safe_excerpt("<h1>Account expired</h1>", &request()),
            "Account expired"
        );
    }
    #[test]
    fn json_message_values_redact_unknown_inline_credentials() {
        let spaced = safe_excerpt(
            r#"{"message":"Account expired. password : newpass Authorization : Basic newbasic"}"#,
            &request(),
        );
        assert!(spaced.contains("Account expired"));
        assert!(!spaced.contains("newpass"));
        assert!(!spaced.contains("newbasic"));
        let text = safe_excerpt(
            r#"{"message":"Account expired. Authorization: Bearer freshsecret password: othersecret","details":["Cookie: newcookie", "license: newlicense"]}"#,
            &request(),
        );
        assert!(text.contains("Account expired"));
        for secret in ["freshsecret", "othersecret", "newcookie", "newlicense"] {
            assert!(!text.contains(secret), "inline credential was exposed");
        }
    }
    #[tokio::test]
    async fn observed_http_refusal_survives_generic_engine_failure_without_refetch() {
        let request = request();
        remember(&request);
        let remembered = sources()
            .lock()
            .get(&request.session_key)
            .unwrap()
            .request
            .clone();
        record(&remembered, SourceReport { status: Some(407), detail: "Source response: HTTP 407 Proxy Authentication Required\nResponse: Account expired".into(), observed: true });
        let error = enrich(&request.session_key, Error::SourceOpenFailed).await;
        assert_eq!(error.code(), "AUTHORIZATION_FAILED");
        assert!(error.to_string().contains("HTTP 407"));
        assert!(error.to_string().contains("Account expired"));
        release(&request.session_key);
        assert!(sources().lock().get(&request.session_key).is_none());
    }
    fn serve(
        status: u16,
        body: &'static str,
        stalled: bool,
    ) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let uri = format!("http://{}/source", listener.local_addr().unwrap());
        let task = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut bytes = [0; 4096];
            let count = stream.read(&mut bytes).unwrap();
            assert!(String::from_utf8_lossy(&bytes[..count]).contains("GET /source"));
            write!(stream, "HTTP/1.1 {status} Failure\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", if stalled { 4096 } else { body.len() }).unwrap();
            if stalled {
                std::thread::sleep(Duration::from_millis(750));
            } else {
                stream.write_all(body.as_bytes()).unwrap();
            }
        });
        (uri, task)
    }
    #[tokio::test]
    async fn real_diagnostic_range_refusal_preserves_original_code_and_metadata_and_does_not_refetch(
    ) {
        let (uri, server) = serve(
            416,
            r#"{"error":"Range refused","token":"private-body-secret"}"#,
            false,
        );
        let mut request = request();
        request.uri = uri;
        request.session_key = "diagnostic-range".into();
        remember(&request);
        for _ in 0..2 {
            let error = enrich(
                &request.session_key,
                Error::Decode("private-engine-details".into()),
            )
            .await;
            assert_eq!(error.code(), "DECODE_FAILED");
            let wire = serde_json::to_value(error).unwrap();
            assert_eq!(wire["stage"], "decode");
            assert_eq!(wire["recoverable"], true);
            assert!(wire["message"]
                .as_str()
                .unwrap()
                .contains("Diagnostic GET: HTTP 416"));
            assert!(!wire.to_string().contains("private-body-secret"));
        }
        release(&request.session_key);
        server.join().unwrap();
    }
    #[tokio::test]
    async fn real_observed_refusal_keeps_status_and_redacted_body() {
        let (uri, server) = serve(
            407,
            r#"{"error":"Account expired","password":"body-secret"}"#,
            false,
        );
        let mut request = request();
        request.uri = uri;
        request.session_key = "observed-refusal".into();
        remember(&request);
        let response = reqwest::Client::new()
            .get(&request.uri)
            .send()
            .await
            .unwrap();
        observe_response(&request.session_key, response).await;
        let error = enrich(&request.session_key, Error::SourceOpenFailed).await;
        assert_eq!(error.code(), "AUTHORIZATION_FAILED");
        assert!(error.to_string().contains("Source response: HTTP 407"));
        assert!(error.to_string().contains("Account expired"));
        assert!(!error.to_string().contains("body-secret"));
        release(&request.session_key);
        server.join().unwrap();
    }
    #[tokio::test]
    async fn observed_stalled_body_is_bounded_and_stale_reports_cannot_attach_to_reused_keys() {
        let (uri, server) = serve(407, "", true);
        let mut request = request();
        request.uri = uri;
        request.session_key = "stalled-refusal".into();
        remember(&request);
        let old = sources()
            .lock()
            .get(&request.session_key)
            .unwrap()
            .request
            .clone();
        let response = reqwest::Client::new()
            .get(&request.uri)
            .send()
            .await
            .unwrap();
        let start = std::time::Instant::now();
        observe_response(&request.session_key, response).await;
        assert!(start.elapsed() < Duration::from_millis(700));
        assert!(enrich(&request.session_key, Error::SourceOpenFailed)
            .await
            .to_string()
            .contains("body timed out"));
        release(&request.session_key);
        remember(&request);
        record(
            &old,
            SourceReport {
                status: Some(407),
                detail: "stale".into(),
                observed: true,
            },
        );
        assert!(sources()
            .lock()
            .get(&request.session_key)
            .unwrap()
            .report
            .is_none());
        release(&request.session_key);
        record(
            &old,
            SourceReport {
                status: Some(407),
                detail: "stale".into(),
                observed: true,
            },
        );
        assert!(!sources().lock().contains_key(&request.session_key));
        server.join().unwrap();
    }
    #[test]
    fn body_redaction_handles_nested_truncated_and_unicode_content() {
        let request = request();
        assert!(!safe_excerpt(
            r#"{"error":"refused","nested":{"session_id":"private"}}"#,
            &request
        )
        .contains("private"));
        assert_eq!(
            safe_excerpt(r#"{"password":"truncated"#, &request),
            "Response was truncated or was not valid JSON."
        );
        assert!(safe_excerpt(&"界".repeat(3000), &request).chars().count() <= 512);
        let text = safe_excerpt(
            "Account expired; password: unrelated-private-value token:unrelated-other-value",
            &request,
        );
        assert!(text.contains("Account expired"));
        assert!(!text.contains("unrelated-private-value"));
        assert!(!text.contains("unrelated-other-value"));
    }
}
