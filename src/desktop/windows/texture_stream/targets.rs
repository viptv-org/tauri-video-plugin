use parking_lot::{Mutex, RwLock};

use windows::{core::Interface, Win32::Graphics::Direct3D11::ID3D11Resource};

use crate::{Error, Result};

use super::texture_error;
use super::webview2::{ICoreWebView2ExperimentalTexture, SendStream};

pub struct DrawTarget {
    pub(super) texture: ICoreWebView2ExperimentalTexture,
    pub(super) resource: ID3D11Resource,
}

pub(super) fn reclaim_available(
    stream: &SendStream,
    free: &Mutex<Vec<DrawTarget>>,
    texture_size: &RwLock<Option<(u32, u32)>>,
) {
    while let Ok(texture) = unsafe { stream.0.get_available_texture() } {
        match unsafe { draw_target(texture) } {
            Ok(target) => {
                let expected = *texture_size.read();
                let actual = unsafe { resource_desc(&target.resource) }
                    .ok()
                    .map(|desc| (desc.Width, desc.Height));
                if actual == expected {
                    free.lock().push(target);
                } else {
                    let _ = unsafe { stream.0.close_texture(&target.texture) };
                }
            }
            Err(error) => tracing::warn!(%error, "failed to reclaim a WebView2 video texture"),
        }
    }
}

pub(super) unsafe fn draw_target(texture: ICoreWebView2ExperimentalTexture) -> Result<DrawTarget> {
    let resource = unsafe { texture.resource() }
        .map_err(texture_error("get a WebView2 video texture resource"))?;
    let desc = unsafe { resource_desc(&resource) }
        .map_err(texture_error("inspect a WebView2 video texture"))?;
    if desc.Format != windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12 {
        return Err(Error::Pipeline(format!(
            "WebView2 created unsupported video texture format {:?}",
            desc.Format
        )));
    }
    Ok(DrawTarget { texture, resource })
}

unsafe fn resource_desc(
    resource: &ID3D11Resource,
) -> windows::core::Result<windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC> {
    let texture: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D = resource.cast()?;
    let mut desc = unsafe { std::mem::zeroed() };
    unsafe { texture.GetDesc(&mut desc) };
    Ok(desc)
}
