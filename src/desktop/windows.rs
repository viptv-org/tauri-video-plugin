use tauri::{AppHandle, Runtime};

use crate::{
    models::{
        NativeControlRequest, NativeLayoutRequest, NativeOpenRequest, NativePlaybackSnapshot,
        NativeSessionRequest,
    },
    Error, Result,
};

#[cfg(feature = "gstreamer-runtime")]
mod texture_stream;

pub fn prepare_texture_stream<R: Runtime>(app: &AppHandle<R>, stream_id: String) -> Result<()> {
    gstreamer::prepare_texture_stream(app, stream_id)
}

pub fn open<R: Runtime>(
    app: &AppHandle<R>,
    payload: NativeOpenRequest,
) -> Result<NativePlaybackSnapshot> {
    match payload.backend.as_deref() {
        None | Some("gstreamer") => gstreamer::open(app, payload),
        Some("mpv") => Err(Error::RuntimeUnavailable(
            "the mpv backend is not implemented on Windows; use gstreamer".into(),
        )),
        Some(backend) => Err(Error::InvalidRequest(format!(
            "backend '{backend}' is not available on Windows"
        ))),
    }
}

pub fn control(payload: NativeControlRequest) -> Result<NativePlaybackSnapshot> {
    gstreamer::control(payload)
}

pub fn layout(payload: NativeLayoutRequest) -> Result<()> {
    gstreamer::layout(payload)
}

pub fn stats(payload: NativeSessionRequest) -> Result<NativePlaybackSnapshot> {
    gstreamer::stats(payload)
}

pub fn close(payload: NativeSessionRequest) -> Result<()> {
    gstreamer::close(payload)
}

#[cfg(feature = "gstreamer-runtime")]
mod gstreamer;

#[cfg(not(feature = "gstreamer-runtime"))]
mod gstreamer {
    use tauri::{AppHandle, Runtime};

    use crate::{
        models::{
            NativeControlRequest, NativeLayoutRequest, NativeOpenRequest, NativePlaybackSnapshot,
            NativeSessionRequest,
        },
        Error, Result,
    };

    fn unavailable<T>() -> Result<T> {
        Err(Error::RuntimeUnavailable(
            "Windows playback requires the gstreamer-runtime feature".into(),
        ))
    }

    pub fn prepare_texture_stream<R: Runtime>(_: &AppHandle<R>, _: String) -> Result<()> {
        unavailable()
    }

    pub fn open<R: Runtime>(
        _: &AppHandle<R>,
        _: NativeOpenRequest,
    ) -> Result<NativePlaybackSnapshot> {
        unavailable()
    }

    pub fn control(_: NativeControlRequest) -> Result<NativePlaybackSnapshot> {
        unavailable()
    }

    pub fn layout(_: NativeLayoutRequest) -> Result<()> {
        unavailable()
    }

    pub fn stats(_: NativeSessionRequest) -> Result<NativePlaybackSnapshot> {
        unavailable()
    }

    pub fn close(_: NativeSessionRequest) -> Result<()> {
        Ok(())
    }
}
