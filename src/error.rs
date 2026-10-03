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
            Self::Decode(_) => "DECODE_FAILED",
            Self::MediaFormat => "MEDIA_FORMAT_FAILED",
            Self::VideoOutput(_) => "VIDEO_OUTPUT_FAILED",
            Self::AudioOutput => "AUDIO_OUTPUT_FAILED",
            Self::ProtectedMedia => "PROTECTED_MEDIA",
            Self::SourceAuthorization => "AUTHORIZATION_FAILED",
            Self::SourceConnection => "CONNECTION_FAILED",
            Self::SourceUnavailable => "SOURCE_UNAVAILABLE",
            Self::SourceOpenFailed => "SOURCE_OPEN_FAILED",
            #[cfg(mobile)]
            Self::PluginInvoke(_) => "MOBILE_PLUGIN_ERROR",
        }
    }

    fn recoverable(&self) -> bool {
        matches!(
            self,
            Self::Pipeline(_) | Self::Decode(_) | Self::SourceConnection
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
}

#[cfg(all(feature = "gstreamer-runtime", any(target_os = "linux", windows)))]
impl NativeMediaFailure {
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
            stage: match self {
                Self::ProtocolMismatch { .. } => Some("protocol"),
                Self::Pipeline(_) => Some("pipeline"),
                Self::Decode(_) | Self::MediaFormat => Some("decode"),
                Self::VideoOutput(_) => Some("video-output"),
                Self::AudioOutput => Some("audio-output"),
                _ => None,
            },
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
