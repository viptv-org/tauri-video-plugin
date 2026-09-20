use std::sync::Arc;

use crate::{Error, Result};

mod d3d11;
mod presenter;
mod targets;
mod webview2;

pub use presenter::TextureStreamPresenter;
pub use targets::DrawTarget;
#[allow(unused_imports)]
pub use webview2::{
    ICoreWebView2ExperimentalEnvironment12, ICoreWebView2ExperimentalEnvironment12_Vtbl,
    ICoreWebView2ExperimentalTexture, ICoreWebView2ExperimentalTexture_Vtbl,
    ICoreWebView2ExperimentalTextureStream, ICoreWebView2ExperimentalTextureStream_Vtbl,
};

use d3d11::SendGstDevice;
use webview2::SendStream;

type UiJob = Box<dyn FnOnce() + Send + 'static>;
pub type UiDispatch = Arc<dyn Fn(UiJob) -> Result<()> + Send + Sync + 'static>;

// These COM references are transported back to the WebView UI thread before
// any methods are invoked. Shared-handle metadata is immutable between calls.
unsafe impl Send for SendStream {}
unsafe impl Sync for SendStream {}
unsafe impl Send for SendGstDevice {}
unsafe impl Sync for SendGstDevice {}
unsafe impl Send for DrawTarget {}
unsafe impl Sync for DrawTarget {}

fn texture_error(context: &'static str) -> impl FnOnce(windows::core::Error) -> Error {
    move |error| Error::Pipeline(format!("failed to {context}: {error}"))
}
