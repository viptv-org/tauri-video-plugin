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
        "the requested Linux playback backend was not compiled".into(),
    ))
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
pub fn force_close() -> Result<()> {
    Ok(())
}
