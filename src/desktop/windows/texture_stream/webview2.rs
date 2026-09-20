use std::ffi::c_void;

use windows::{
    core::{IUnknown, IUnknown_Vtbl, Interface, HRESULT, PCWSTR},
    Win32::{
        Foundation::HANDLE,
        Graphics::Direct3D11::ID3D11Resource,
    },
};

#[derive(Clone)]
pub(super) struct SendStream(pub(super) ICoreWebView2ExperimentalTextureStream);

impl SendStream {
    pub(super) unsafe fn create_texture(
        &self,
        width: u32,
        height: u32,
    ) -> windows::core::Result<ICoreWebView2ExperimentalTexture> {
        unsafe { self.0.create_texture(width, height) }
    }

    pub(super) unsafe fn present_texture(
        &self,
        texture: &ICoreWebView2ExperimentalTexture,
    ) -> windows::core::Result<()> {
        unsafe { self.0.present_texture(texture) }
    }
}

windows::core::imp::define_interface!(
    ICoreWebView2ExperimentalEnvironment12,
    ICoreWebView2ExperimentalEnvironment12_Vtbl,
    0x96c27a45_f142_4873_80ad_9d0cd899b2b9
);
windows::core::imp::interface_hierarchy!(ICoreWebView2ExperimentalEnvironment12, IUnknown);

impl ICoreWebView2ExperimentalEnvironment12 {
    pub(super) unsafe fn create_texture_stream(
        &self,
        stream_id: PCWSTR,
        device: &IUnknown,
    ) -> windows::core::Result<ICoreWebView2ExperimentalTextureStream> {
        unsafe {
            let mut result = std::ptr::null_mut();
            (Interface::vtable(self).CreateTextureStream)(
                Interface::as_raw(self),
                stream_id,
                Interface::as_raw(device),
                &mut result,
            )
            .and_then(|| windows::core::Type::from_abi(result))
        }
    }

    pub(super) unsafe fn render_adapter_luid(&self) -> windows::core::Result<u64> {
        unsafe {
            let mut result = 0;
            (Interface::vtable(self).get_RenderAdapterLUID)(Interface::as_raw(self), &mut result)
                .map(|| result)
        }
    }
}

#[repr(C)]
#[allow(non_snake_case)]
pub struct ICoreWebView2ExperimentalEnvironment12_Vtbl {
    base__: IUnknown_Vtbl,
    CreateTextureStream:
        unsafe extern "system" fn(*mut c_void, PCWSTR, *mut c_void, *mut *mut c_void) -> HRESULT,
    get_RenderAdapterLUID: unsafe extern "system" fn(*mut c_void, *mut u64) -> HRESULT,
    add_RenderAdapterLUIDChanged:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut i64) -> HRESULT,
    remove_RenderAdapterLUIDChanged: unsafe extern "system" fn(*mut c_void, i64) -> HRESULT,
}

windows::core::imp::define_interface!(
    ICoreWebView2ExperimentalTexture,
    ICoreWebView2ExperimentalTexture_Vtbl,
    0x0836f09c_34bd_47bf_914a_99fb56ae2d07
);
windows::core::imp::interface_hierarchy!(ICoreWebView2ExperimentalTexture, IUnknown);

impl ICoreWebView2ExperimentalTexture {
    pub(super) unsafe fn resource(&self) -> windows::core::Result<ID3D11Resource> {
        unsafe {
            let mut resource = std::ptr::null_mut();
            (Interface::vtable(self).get_Resource)(Interface::as_raw(self), &mut resource).ok()?;
            let resource: IUnknown = windows::core::Type::from_abi(resource)?;
            resource.cast()
        }
    }

    pub(super) unsafe fn set_timestamp(&self, value: u64) -> windows::core::Result<()> {
        unsafe { (Interface::vtable(self).put_Timestamp)(Interface::as_raw(self), value).ok() }
    }
}

#[repr(C)]
#[allow(non_snake_case)]
pub struct ICoreWebView2ExperimentalTexture_Vtbl {
    base__: IUnknown_Vtbl,
    get_Handle: unsafe extern "system" fn(*mut c_void, *mut HANDLE) -> HRESULT,
    get_Resource: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    get_Timestamp: unsafe extern "system" fn(*mut c_void, *mut u64) -> HRESULT,
    put_Timestamp: unsafe extern "system" fn(*mut c_void, u64) -> HRESULT,
}

windows::core::imp::define_interface!(
    ICoreWebView2ExperimentalTextureStream,
    ICoreWebView2ExperimentalTextureStream_Vtbl,
    0xafca8431_633f_4528_abfe_7fc3bedd8962
);
windows::core::imp::interface_hierarchy!(ICoreWebView2ExperimentalTextureStream, IUnknown);

impl ICoreWebView2ExperimentalTextureStream {
    pub(super) unsafe fn add_allowed_origin(
        &self,
        origin: PCWSTR,
        allow_web_texture: bool,
    ) -> windows::core::Result<()> {
        unsafe {
            (Interface::vtable(self).AddAllowedOrigin)(
                Interface::as_raw(self),
                origin,
                allow_web_texture.into(),
            )
            .ok()
        }
    }

    unsafe fn create_texture(
        &self,
        width: u32,
        height: u32,
    ) -> windows::core::Result<ICoreWebView2ExperimentalTexture> {
        unsafe {
            let mut result = std::ptr::null_mut();
            (Interface::vtable(self).CreateTexture)(
                Interface::as_raw(self),
                width,
                height,
                &mut result,
            )
            .and_then(|| windows::core::Type::from_abi(result))
        }
    }

    pub(super) unsafe fn get_available_texture(
        &self,
    ) -> windows::core::Result<ICoreWebView2ExperimentalTexture> {
        unsafe {
            let mut result = std::ptr::null_mut();
            (Interface::vtable(self).GetAvailableTexture)(Interface::as_raw(self), &mut result)
                .and_then(|| windows::core::Type::from_abi(result))
        }
    }

    pub(super) unsafe fn close_texture(
        &self,
        texture: &ICoreWebView2ExperimentalTexture,
    ) -> windows::core::Result<()> {
        unsafe {
            (Interface::vtable(self).CloseTexture)(
                Interface::as_raw(self),
                Interface::as_raw(texture),
            )
            .ok()
        }
    }

    unsafe fn present_texture(
        &self,
        texture: &ICoreWebView2ExperimentalTexture,
    ) -> windows::core::Result<()> {
        unsafe {
            (Interface::vtable(self).PresentTexture)(
                Interface::as_raw(self),
                Interface::as_raw(texture),
            )
            .ok()
        }
    }
}

#[repr(C)]
#[allow(non_snake_case)]
pub struct ICoreWebView2ExperimentalTextureStream_Vtbl {
    base__: IUnknown_Vtbl,
    get_Id: unsafe extern "system" fn(*mut c_void, *mut *mut u16) -> HRESULT,
    AddAllowedOrigin: unsafe extern "system" fn(*mut c_void, PCWSTR, i32) -> HRESULT,
    RemoveAllowedOrigin: unsafe extern "system" fn(*mut c_void, PCWSTR) -> HRESULT,
    add_StartRequested: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut i64) -> HRESULT,
    remove_StartRequested: unsafe extern "system" fn(*mut c_void, i64) -> HRESULT,
    add_Stopped: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut i64) -> HRESULT,
    remove_Stopped: unsafe extern "system" fn(*mut c_void, i64) -> HRESULT,
    CreateTexture: unsafe extern "system" fn(*mut c_void, u32, u32, *mut *mut c_void) -> HRESULT,
    GetAvailableTexture: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    CloseTexture: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
    PresentTexture: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
    Stop: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    add_ErrorReceived: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut i64) -> HRESULT,
    remove_ErrorReceived: unsafe extern "system" fn(*mut c_void, i64) -> HRESULT,
    SetD3DDevice: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
    add_WebTextureReceived:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut i64) -> HRESULT,
    remove_WebTextureReceived: unsafe extern "system" fn(*mut c_void, i64) -> HRESULT,
    add_WebTextureStreamStopped:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut i64) -> HRESULT,
    remove_WebTextureStreamStopped: unsafe extern "system" fn(*mut c_void, i64) -> HRESULT,
}
