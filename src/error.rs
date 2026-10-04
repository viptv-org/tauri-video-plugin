use serde::{ser::Serializer, Serialize};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(thiserror::Error)]
pub enum Error {
    #[error("Native video protocol mismatch. Update the application.")]
    ProtocolMismatch {
        expected: u32,
        actual: Option<u32>,
        package_version: Option<String>,
    },
    #[error("The native playback request is invalid.")]
    InvalidRequest(String),
    #[error("The selected native playback engine is unavailable.")]
    RuntimeUnavailable(String),
    #[error("The native playback engine failed. Reopen this source to retry.")]
    Pipeline(String),
    #[error("{0}")]
    PipelineFault(PipelineFault),
    #[error("The media source returned HTTP {0}.")]
    SourceHttpStatus(u16),
    #[error("The native player cannot decode this media format or codec.")]
    Decode(String),
    #[error("The source did not provide a recognizable media stream.")]
    MediaFormat,
    #[error("The native video surface could not initialize or render playback.")]
    VideoOutput(String),
    #[error("The native audio output could not start playback.")]
    AudioOutput,
    #[error("The native player could not unlock this protected media.")]
    ProtectedMedia,
    #[error("The media source refused playback authorization.")]
    SourceAuthorization,
    #[error("The native player could not connect to the media source.")]
    SourceConnection,
    #[error("The media source is unavailable or its URL has expired.")]
    SourceUnavailable,
    #[error("The native player could not open this media source.")]
    SourceOpenFailed,
    #[error("{message}")]
    SourceDiagnostic {
        code: &'static str,
        message: String,
        original: Box<Error>,
    },
    #[cfg(mobile)]
    #[error("Native mobile playback failed.")]
    PluginInvoke(#[from] tauri::plugin::mobile::PluginInvokeError),
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ProtocolMismatch { .. } => "PROTOCOL_MISMATCH",
            Self::InvalidRequest(_) => "INVALID_REQUEST",
            Self::RuntimeUnavailable(_) => "RUNTIME_UNAVAILABLE",
            Self::Pipeline(_) => "PIPELINE_FAILED",
            Self::PipelineFault(_) => "PIPELINE_FAILED",
            Self::SourceHttpStatus(401 | 403 | 407) => "AUTHORIZATION_FAILED",
            Self::SourceHttpStatus(404 | 410) => "SOURCE_UNAVAILABLE",
            Self::SourceHttpStatus(_) => "CONNECTION_FAILED",
            Self::Decode(_) => "DECODE_FAILED",
            Self::MediaFormat => "MEDIA_FORMAT_FAILED",
            Self::VideoOutput(_) => "VIDEO_OUTPUT_FAILED",
            Self::AudioOutput => "AUDIO_OUTPUT_FAILED",
            Self::ProtectedMedia => "PROTECTED_MEDIA",
            Self::SourceAuthorization => "AUTHORIZATION_FAILED",
            Self::SourceConnection => "CONNECTION_FAILED",
            Self::SourceUnavailable => "SOURCE_UNAVAILABLE",
            Self::SourceOpenFailed => "SOURCE_OPEN_FAILED",
            Self::SourceDiagnostic { code, .. } => code,
            #[cfg(mobile)]
            Self::PluginInvoke(_) => "MOBILE_PLUGIN_ERROR",
        }
    }

    fn recoverable(&self) -> bool {
        if let Self::SourceDiagnostic { original, .. } = self {
            return original.recoverable();
        }
        matches!(
            self,
            Self::Pipeline(_) | Self::PipelineFault(_) | Self::Decode(_) | Self::SourceConnection
        )
    }

    fn stage(&self) -> Option<&'static str> {
        match self {
            Self::ProtocolMismatch { .. } => Some("protocol"),
            Self::Pipeline(_) | Self::PipelineFault(_) => Some("pipeline"),
            Self::Decode(_) | Self::MediaFormat => Some("decode"),
            Self::VideoOutput(_) => Some("video-output"),
            Self::AudioOutput => Some("audio-output"),
            Self::SourceDiagnostic { original, .. } => original.stage(),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PipelineFault {
    pub component: &'static str,
    pub reason: &'static str,
    pub domain: &'static str,
    pub native_code: i32,
}
impl std::fmt::Display for PipelineFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "GStreamer {}: {} ({}/{})",
            self.component, self.reason, self.domain, self.native_code
        )
    }
}

impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeVideoError")
            .field("code", &self.code())
            .field("message", &self.to_string())
            .finish()
    }
}

#[cfg(all(feature = "gstreamer-runtime", any(target_os = "linux", windows)))]
#[derive(Clone, Copy, Debug)]
pub(crate) enum NativeMediaFailure {
    Authorization,
    Connection,
    Unavailable,
    Decode,
    MediaFormat,
    Runtime,
    Protected,
    Pipeline,
    Bus(PipelineFault, &'static str, Option<u16>),
}

#[cfg(all(feature = "gstreamer-runtime", any(target_os = "linux", windows)))]
impl NativeMediaFailure {
    pub(crate) fn from_bus(message: &gstreamer::message::Error) -> Self {
        use gstreamer::glib::translate::IntoGlib;
        use gstreamer::prelude::ElementExt;
        use gstreamer::{CoreError, LibraryError, ResourceError, StreamError};
        let mut http_status = None;
        if let Some(details) = message.details() {
            for key in ["http-status-code", "http-status"] {
                let status = details.get::<u32>(key).ok().or_else(|| {
                    details
                        .get::<i32>(key)
                        .ok()
                        .and_then(|value| u32::try_from(value).ok())
                });
                if let Some(status) = status.filter(|value| (400..=599).contains(value)) {
                    http_status = Some(status as u16);
                    break;
                }
            }
        }
        let error = message.error();
        let classified = Self::from_gstreamer(&error);
        let component = message
            .src()
            .and_then(|object| {
                gstreamer::glib::prelude::Cast::downcast_ref::<gstreamer::Element>(object)
            })
            .and_then(|element| element.factory())
            .map(|factory| gstreamer::prelude::GstObjectExt::name(&factory));
        let component = match component.as_deref() {
            Some("souphttpsrc" | "curlhttpsrc") => "HTTP source",
            Some("hlsdemux" | "hlsdemux2") => "HLS reader",
            Some("qtdemux" | "matroskademux" | "tsdemux") => "container reader",
            Some("gtkglsink" | "glsinkbin" | "glupload" | "glcolorconvert") => "video output",
            _ => "pipeline",
        };
        let debug = message.debug().unwrap_or_default();
        let reason =
            if debug.contains("reason not-negotiated") || error.matches(CoreError::Negotiation) {
                "media format negotiation failed"
            } else if debug.contains("reason not-linked") || error.matches(CoreError::Pad) {
                "a media processing component is not connected"
            } else if error.matches(CoreError::StateChange)
                || error
                    .message()
                    .starts_with("Native playback state change failed")
            {
                "the requested playback state could not be entered"
            } else if error
                .message()
                .starts_with("Native source transition failed")
            {
                "the previous media source could not be reset"
            } else if error.message().starts_with("Native source start failed") {
                "this media source could not start"
            } else if error.message().starts_with("Native seek failed") {
                "the engine rejected the requested seek"
            } else if error.matches(StreamError::Demux) {
                "the media container could not be read"
            } else if error.matches(CoreError::Clock) {
                "the playback clock could not start"
            } else {
                "the media stream stopped with a native processing error"
            };
        let (domain, native_code) = if let Some(code) = error.kind::<CoreError>() {
            ("core", code.into_glib())
        } else if let Some(code) = error.kind::<StreamError>() {
            ("stream", code.into_glib())
        } else if let Some(code) = error.kind::<ResourceError>() {
            ("resource", code.into_glib())
        } else if let Some(code) = error.kind::<LibraryError>() {
            ("library", code.into_glib())
        } else {
            ("native", 0)
        };
        Self::Bus(
            PipelineFault {
                component,
                reason,
                domain,
                native_code,
            },
            classified.into_error().code(),
            http_status,
        )
    }
    pub(crate) fn from_gstreamer(error: &gstreamer::glib::Error) -> Self {
        use gstreamer::{CoreError, LibraryError, ResourceError, StreamError};
        if error.matches(ResourceError::NotAuthorized) {
            Self::Authorization
        } else if error.matches(ResourceError::NotFound) {
            Self::Unavailable
        } else if [
            ResourceError::OpenRead,
            ResourceError::Read,
            ResourceError::OpenReadWrite,
            ResourceError::Close,
            ResourceError::Busy,
        ]
        .into_iter()
        .any(|kind| error.matches(kind))
        {
            Self::Connection
        } else if error.matches(StreamError::Decode) || error.matches(StreamError::CodecNotFound) {
            Self::Decode
        } else if [
            StreamError::TypeNotFound,
            StreamError::WrongType,
            StreamError::Format,
        ]
        .into_iter()
        .any(|kind| error.matches(kind))
        {
            Self::MediaFormat
        } else if error.matches(CoreError::MissingPlugin) || error.matches(LibraryError::Init) {
            Self::Runtime
        } else if error.matches(StreamError::Decrypt) || error.matches(StreamError::DecryptNokey) {
            Self::Protected
        } else {
            Self::Pipeline
        }
    }
    pub(crate) fn into_error(self) -> Error {
        match self {
            Self::Authorization => Error::SourceAuthorization,
            Self::Connection => Error::SourceConnection,
            Self::Unavailable => Error::SourceUnavailable,
            Self::Decode => Error::Decode("Native decoder failure".into()),
            Self::MediaFormat => Error::MediaFormat,
            Self::Runtime => {
                Error::RuntimeUnavailable("Required native component unavailable".into())
            }
            Self::Protected => Error::ProtectedMedia,
            Self::Pipeline => Error::Pipeline("Native pipeline failure".into()),
            Self::Bus(fault, code, http_status) => {
                let original = if let Some(status) = http_status {
                    Error::SourceHttpStatus(status)
                } else {
                    match code {
                        "AUTHORIZATION_FAILED" => Self::Authorization.into_error(),
                        "CONNECTION_FAILED" => Self::Connection.into_error(),
                        "SOURCE_UNAVAILABLE" => Self::Unavailable.into_error(),
                        "DECODE_FAILED" => Self::Decode.into_error(),
                        "MEDIA_FORMAT_FAILED" => Self::MediaFormat.into_error(),
                        "RUNTIME_UNAVAILABLE" => Self::Runtime.into_error(),
                        "PROTECTED_MEDIA" => Self::Protected.into_error(),
                        _ => Error::PipelineFault(fault),
                    }
                };
                let message = if matches!(original, Error::PipelineFault(_)) {
                    original.to_string()
                } else {
                    format!(
                        "GStreamer {}: {original} ({}/{})",
                        fault.component, fault.domain, fault.native_code
                    )
                };
                Error::SourceDiagnostic {
                    code: original.code(),
                    message,
                    original: Box::new(original),
                }
            }
        }
    }
}

impl Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct WireError<'a> {
            code: &'static str,
            message: String,
            recoverable: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            stage: Option<&'a str>,
        }

        WireError {
            code: self.code(),
            message: self.to_string(),
            recoverable: self.recoverable(),
            stage: self.stage(),
        }
        .serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn error_displays_and_debug_never_echo_runtime_details() {
        for error in [
            Error::Pipeline("https://private.invalid/token".into()),
            Error::InvalidRequest("Cookie: secret".into()),
            Error::RuntimeUnavailable("/home/private/media".into()),
            Error::Decode("https://private.invalid/token".into()),
            Error::VideoOutput("Cookie: secret".into()),
        ] {
            let text = format!(
                "{error} {error:?} {}",
                serde_json::to_string(&error).unwrap()
            );
            for secret in ["private.invalid", "secret", "/home/private"] {
                assert!(!text.contains(secret));
            }
        }
    }
    #[test]
    #[cfg(all(feature = "gstreamer-runtime", any(target_os = "linux", windows)))]
    fn typed_gstreamer_causes_survive_without_debug_payloads() {
        for (kind, code) in [
            (
                gstreamer::ResourceError::NotAuthorized,
                "AUTHORIZATION_FAILED",
            ),
            (gstreamer::ResourceError::NotFound, "SOURCE_UNAVAILABLE"),
            (gstreamer::ResourceError::OpenRead, "CONNECTION_FAILED"),
        ] {
            let raw =
                gstreamer::glib::Error::new(kind, "https://private.invalid/token Cookie=secret");
            let failure = NativeMediaFailure::from_gstreamer(&raw).into_error();
            assert_eq!(failure.code(), code);
            assert!(!format!("{failure:?}").contains("private.invalid"));
        }
    }

    #[test]
    #[cfg(all(feature = "gstreamer-runtime", any(target_os = "linux", windows)))]
    fn bus_failures_preserve_http_status_and_negotiation_without_private_details() {
        let message =
            gstreamer::message::Error::builder(gstreamer::ResourceError::Read, "private source")
                .details(
                    gstreamer::Structure::builder("details")
                        .field("http-status-code", 407u32)
                        .build(),
                )
                .build();
        let gstreamer::MessageView::Error(error) = message.view() else {
            panic!("expected error message")
        };
        let failure = NativeMediaFailure::from_bus(error).into_error();
        assert_eq!(failure.code(), "AUTHORIZATION_FAILED");
        assert!(failure.to_string().contains("407"));
        let message =
            gstreamer::message::Error::builder(gstreamer::StreamError::Failed, "private URL")
                .debug(
                    "streaming stopped, reason not-negotiated (-4); https://private.invalid/token",
                )
                .build();
        let gstreamer::MessageView::Error(error) = message.view() else {
            panic!("expected error message")
        };
        let failure = NativeMediaFailure::from_bus(error).into_error();
        assert!(failure.to_string().contains("negotiation"));
        assert!(!format!("{failure:?}").contains("private.invalid"));
    }

    #[test]
    #[cfg(all(feature = "gstreamer-runtime", any(target_os = "linux", windows)))]
    fn only_explicit_decoder_errors_are_reported_as_decode_failures() {
        use gstreamer::{CoreError, StreamError};
        for (raw, code) in [
            (
                gstreamer::glib::Error::new(CoreError::Failed, "private"),
                "PIPELINE_FAILED",
            ),
            (
                gstreamer::glib::Error::new(CoreError::MissingPlugin, "private"),
                "RUNTIME_UNAVAILABLE",
            ),
            (
                gstreamer::glib::Error::new(StreamError::Failed, "private"),
                "PIPELINE_FAILED",
            ),
            (
                gstreamer::glib::Error::new(StreamError::Decode, "private"),
                "DECODE_FAILED",
            ),
            (
                gstreamer::glib::Error::new(StreamError::CodecNotFound, "private"),
                "DECODE_FAILED",
            ),
        ] {
            let failure = NativeMediaFailure::from_gstreamer(&raw).into_error();
            assert_eq!(failure.code(), code);
            if code != "DECODE_FAILED" {
                assert!(!failure.to_string().contains("decode"));
            }
        }
    }
}
