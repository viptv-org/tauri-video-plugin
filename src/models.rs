use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Version of the JavaScript-to-Rust command, payload, response, and error
/// contract. Additive diagnostic or capability fields do not change it.
pub const VIDEO_PLUGIN_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NativePluginDiagnostics {
    pub protocol_version: u32,
    pub crate_name: String,
    pub crate_version: String,
    pub platform: String,
    /// Playback engines compiled into this build, in the order an 'auto'
    /// client should prefer them.
    pub engines: Vec<String>,
    /// Whether HLS sources are served through the plugin's loopback
    /// sanitizing proxy (disguised segments cleaned, headers applied there).
    #[serde(default)]
    pub source_proxy: bool,
}

/// The playback engines compiled into this build, in the order an 'auto'
/// client should prefer them. GStreamer is the primary engine on both
/// desktop platforms and matches the Rust default for an omitted backend;
/// mpv is an optional Linux runtime that must be requested explicitly.
fn compiled_engines() -> Vec<String> {
    let mut engines = Vec::new();
    if cfg!(feature = "gstreamer-runtime") && cfg!(any(target_os = "linux", windows)) {
        engines.push("gstreamer".into());
    }
    if cfg!(all(target_os = "linux", feature = "mpv-runtime")) {
        engines.push("mpv".into());
    }
    engines
}

impl NativePluginDiagnostics {
    pub fn current() -> Self {
        Self {
            protocol_version: VIDEO_PLUGIN_PROTOCOL_VERSION,
            crate_name: env!("CARGO_PKG_NAME").to_owned(),
            crate_version: env!("CARGO_PKG_VERSION").to_owned(),
            platform: std::env::consts::OS.to_owned(),
            engines: compiled_engines(),
            source_proxy: cfg!(any(target_os = "linux", windows)),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Video,
    Audio,
    Subtitle,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeOpenRequest {
    /// Protocol and adapter version are required by protocol 1. They remain
    /// optional while deserializing so legacy JavaScript receives a deliberate
    /// protocol error instead of a generic invalid-payload rejection.
    #[serde(default)]
    pub protocol_version: Option<u32>,
    #[serde(default)]
    pub package_version: Option<String>,
    /// Identifies the JavaScript controller that owns the singleton native surface.
    /// Late cleanup from an older React render must not close a newer player.
    #[serde(default)]
    pub session_key: String,
    pub uri: String,
    /// Playback engine requested by the JavaScript controller. Omission selects
    /// the platform's primary native backend; alternatives must be explicit.
    #[serde(default)]
    pub backend: Option<String>,
    /// Serve this source through the loopback sanitizing proxy. Omitted:
    /// http(s) HLS (`.m3u8`/`.m3u`) sources are proxied, others open
    /// directly. `true` forces it for any http(s) source; `false` opts out.
    #[serde(default)]
    pub source_proxy: Option<bool>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub cookies: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub referrer: Option<String>,
    #[serde(default)]
    pub tls_ca_file: Option<String>,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub scroll_x: f64,
    #[serde(default)]
    pub scroll_y: f64,
    pub width: f64,
    pub height: f64,
    #[serde(default = "default_true")]
    pub autoplay: bool,
    #[serde(default = "default_volume")]
    pub volume: f64,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub start_at_seconds: f64,
    #[serde(default)]
    pub min_buffer_ms: Option<u32>,
    #[serde(default)]
    pub max_buffer_ms: Option<u32>,
    #[serde(default)]
    pub playback_buffer_ms: Option<u32>,
    #[serde(default)]
    pub rebuffer_ms: Option<u32>,
    #[serde(default)]
    pub target_buffer_bytes: Option<u64>,
    #[serde(default)]
    pub decoder_fallback: Option<bool>,
    #[serde(default)]
    pub dolby_vision_mode: Option<String>,
    #[serde(default)]
    pub tunneling: Option<bool>,
}

const fn default_true() -> bool {
    true
}

impl std::fmt::Debug for NativeOpenRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeOpenRequest { source_and_credentials: <redacted> }")
    }
}

impl NativeOpenRequest {
    pub(crate) fn validate_authorization(&self) -> crate::Result<()> {
        let valid = |value: &str| value.len() <= 8192 && !value.chars().any(char::is_control);
        let mut names = std::collections::BTreeSet::new();
        let bad = self.headers.len() > 32
            || self.headers.iter().any(|(name, value)| {
                let lower = name.to_ascii_lowercase();
                name.is_empty()
                    || name.len() > 128
                    || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    || !valid(value)
                    || !names.insert(lower.clone())
                    || matches!(
                        lower.as_str(),
                        "host"
                            | "connection"
                            | "content-length"
                            | "transfer-encoding"
                            | "proxy-authorization"
                            | "upgrade"
                            | "keep-alive"
                            | "te"
                            | "trailer"
                    )
            })
            || [&self.cookies, &self.user_agent, &self.referrer]
                .into_iter()
                .flatten()
                .any(|value| !valid(value));
        let conflicting = [
            ("cookie", self.cookies.as_ref()),
            ("user-agent", self.user_agent.as_ref()),
            ("referer", self.referrer.as_ref()),
        ]
        .into_iter()
        .any(|(name, property)| {
            property.is_some_and(|property| {
                self.headers
                    .iter()
                    .any(|(key, value)| key.eq_ignore_ascii_case(name) && value != property)
            })
        });
        if bad || conflicting {
            Err(crate::Error::InvalidRequest(
                "Invalid source authorization".into(),
            ))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod authorization_tests {
    use super::*;
    #[test]
    fn native_headers_are_bounded_and_debug_redacts_all_source_fields() {
        let mut value = serde_json::json!({"uri":"https://private.invalid/token","x":0,"y":0,"width":100,"height":100,"headers":{"Authorization":"Bearer secret"},"tlsCaFile":"/home/private/ca.pem"});
        let payload: NativeOpenRequest = serde_json::from_value(value.clone()).unwrap();
        payload.validate_authorization().unwrap();
        let debug = format!("{payload:?}");
        for secret in ["private.invalid", "secret", "/home/private"] {
            assert!(!debug.contains(secret));
        }
        for headers in [
            serde_json::json!({"Host":"private"}),
            serde_json::json!({"Cookie":"a\r\nX-Injected: secret"}),
            serde_json::json!({"Referer":"a","referer":"b"}),
        ] {
            value["headers"] = headers;
            assert!(serde_json::from_value::<NativeOpenRequest>(value.clone())
                .unwrap()
                .validate_authorization()
                .is_err());
        }
        value["headers"] = serde_json::json!({"Cookie":"one"});
        value["cookies"] = serde_json::json!("two");
        assert!(serde_json::from_value::<NativeOpenRequest>(value)
            .unwrap()
            .validate_authorization()
            .is_err());
    }
}

const fn default_volume() -> f64 {
    1.0
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeLayoutRequest {
    #[serde(default)]
    pub session_key: String,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub scroll_x: f64,
    #[serde(default)]
    pub scroll_y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeControlRequest {
    #[serde(default)]
    pub session_key: String,
    pub action: String,
    #[serde(default)]
    pub value: f64,
    #[serde(default = "default_native_track_index")]
    pub index: i32,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSessionRequest {
    #[serde(default)]
    pub session_key: String,
}

const fn default_native_track_index() -> i32 {
    -1
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeTrackInfo {
    pub id: String,
    pub index: i32,
    pub kind: TrackKind,
    pub language: String,
    pub label: String,
    pub codec: String,
    #[serde(default)]
    pub selected: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativePlaybackSnapshot {
    pub duration_seconds: f64,
    pub current_time_seconds: f64,
    pub buffered_seconds: f64,
    #[serde(default)]
    pub live: bool,
    #[serde(default = "default_true")]
    pub seekable: bool,
    #[serde(default)]
    pub seekable_start_seconds: f64,
    #[serde(default)]
    pub seekable_end_seconds: f64,
    pub playing: bool,
    pub video_width: u32,
    pub video_height: u32,
    pub tracks: Vec<NativeTrackInfo>,
    #[serde(default)]
    pub presented_frames: u64,
    #[serde(default)]
    pub dropped_frames: u64,
    #[serde(default)]
    pub measured_fps: f64,
    #[serde(default)]
    pub hardware_backend: String,
    /// The engine serving this snapshot, e.g. "gstreamer" or "mpv".
    #[serde(default)]
    pub backend: String,
    #[serde(default)]
    pub encoded_bytes_buffered: u64,
    /// Whether this session is served through the loopback sanitizing proxy.
    #[serde(default)]
    pub source_proxied: bool,
    #[serde(default)]
    pub average_frame_processing_us: f64,
}
