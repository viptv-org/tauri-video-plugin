use std::cell::RefCell;

use tauri::{AppHandle, Runtime};

use crate::{
    models::{
        NativeControlRequest, NativeLayoutRequest, NativeOpenRequest, NativePlaybackSnapshot,
        NativeSessionRequest,
    },
    Error, Result,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Gstreamer,
    Mpv,
}

thread_local! {
    static ACTIVE_BACKEND: RefCell<Option<Backend>> = const { RefCell::new(None) };
}

pub fn open<R: Runtime>(
    app: &AppHandle<R>,
    payload: NativeOpenRequest,
) -> Result<NativePlaybackSnapshot> {
    let requested = select_backend(payload.backend.as_deref())?;
    let previous = ACTIVE_BACKEND.with(|active| *active.borrow());
    if previous != Some(requested) {
        match previous {
            Some(Backend::Gstreamer) => super::linux_gstreamer::force_close()?,
            Some(Backend::Mpv) => super::linux_mpv::force_close()?,
            None => {}
        }
        // The old player is gone. If the replacement fails to open,
        // controls must not continue routing to a stale backend.
        ACTIVE_BACKEND.with(|active| *active.borrow_mut() = None);
    }
    let result = match requested {
        Backend::Gstreamer => super::linux_gstreamer::open(app, payload),
        Backend::Mpv => super::linux_mpv::open(app, payload),
    };
    if result.is_ok() {
        ACTIVE_BACKEND.with(|active| *active.borrow_mut() = Some(requested));
    }
    result
}

pub fn control(payload: NativeControlRequest) -> Result<NativePlaybackSnapshot> {
    match active_backend()? {
        Backend::Gstreamer => super::linux_gstreamer::control(payload),
        Backend::Mpv => super::linux_mpv::control(payload),
    }
}

pub fn layout(payload: NativeLayoutRequest) -> Result<()> {
    match active_backend()? {
        Backend::Gstreamer => super::linux_gstreamer::layout(payload),
        Backend::Mpv => super::linux_mpv::layout(payload),
    }
}

pub fn stats(payload: NativeSessionRequest) -> Result<NativePlaybackSnapshot> {
    match active_backend()? {
        Backend::Gstreamer => super::linux_gstreamer::stats(payload),
        Backend::Mpv => super::linux_mpv::stats(payload),
    }
}

pub fn close(payload: NativeSessionRequest) -> Result<()> {
    let backend = active_backend()?;
    // A close means the caller is finished with the native player. A key
    // mismatch would mean the adapter and the engine desynchronized;
    // leaving the engine running would leak playing audio, so the engine
    // parks regardless of the presented key.
    let result = match backend {
        Backend::Gstreamer => super::linux_gstreamer::close(payload),
        Backend::Mpv => super::linux_mpv::close(payload),
    };
    if result.is_ok() {
        ACTIVE_BACKEND.with(|active| *active.borrow_mut() = None);
    }
    result
}

fn active_backend() -> Result<Backend> {
    ACTIVE_BACKEND
        .with(|active| *active.borrow())
        .ok_or_else(|| Error::InvalidRequest("native player is not open".into()))
}

fn select_backend(requested: Option<&str>) -> Result<Backend> {
    match requested {
        None if cfg!(feature = "gstreamer-runtime") => Ok(Backend::Gstreamer),
        Some("gstreamer") if cfg!(feature = "gstreamer-runtime") => Ok(Backend::Gstreamer),
        Some("mpv") if cfg!(feature = "mpv-runtime") => Ok(Backend::Mpv),
        Some("gstreamer") => Err(Error::RuntimeUnavailable(
            "the gstreamer backend was not compiled; enable gstreamer-runtime".into(),
        )),
        Some("mpv") => Err(Error::RuntimeUnavailable(
            "the mpv backend was not compiled; enable mpv-runtime".into(),
        )),
        None => Err(Error::RuntimeUnavailable(
            "the default gstreamer backend was not compiled; request 'mpv' explicitly or enable gstreamer-runtime".into(),
        )),
        Some(backend) => Err(Error::InvalidRequest(format!(
            "backend '{backend}' is not available on Linux"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_mobile_backend_is_rejected_on_linux() {
        assert!(select_backend(Some("libvlc")).is_err());
    }

    #[cfg(feature = "gstreamer-runtime")]
    #[test]
    fn omitted_backend_selects_gstreamer() {
        assert_eq!(select_backend(None).unwrap(), Backend::Gstreamer);
    }

    #[cfg(feature = "mpv-runtime")]
    #[test]
    fn mpv_requires_an_explicit_request() {
        assert_eq!(select_backend(Some("mpv")).unwrap(), Backend::Mpv);
        #[cfg(not(feature = "gstreamer-runtime"))]
        assert!(select_backend(None).is_err());
    }
}
