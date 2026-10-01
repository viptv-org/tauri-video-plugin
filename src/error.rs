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
    #[error("The native player could not decode this media delivery.")]
    Pipeline(String),
    #[error("The media source refused playback authorization.")]
    SourceAuthorization,
    #[error("The native player could not connect to the media source.")]
    SourceConnection,
    #[error("The media source is unavailable or its URL has expired.")]
    SourceUnavailable,
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
            Self::SourceAuthorization => "AUTHORIZATION_FAILED",
            Self::SourceConnection => "CONNECTION_FAILED",
            Self::SourceUnavailable => "SOURCE_UNAVAILABLE",
            #[cfg(mobile)]
            Self::PluginInvoke(_) => "MOBILE_PLUGIN_ERROR",
        }
    }

    fn recoverable(&self) -> bool {
        matches!(self, Self::Pipeline(_) | Self::SourceConnection)
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
}

#[cfg(all(feature = "gstreamer-runtime", any(target_os = "linux", windows)))]
impl NativeMediaFailure {
    pub(crate) fn from_gstreamer(error: &gstreamer::glib::Error) -> Self {
        use gstreamer::ResourceError;
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
        } else {
            Self::Decode
        }
    }
    pub(crate) fn into_error(self) -> Error {
        match self {
            Self::Authorization => Error::SourceAuthorization,
            Self::Connection => Error::SourceConnection,
            Self::Unavailable => Error::SourceUnavailable,
            Self::Decode => Error::Pipeline("Native decoder failure".into()),
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
}
