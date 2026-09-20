use std::ffi::c_void;

use gst::glib::translate::from_glib_full;
use gstreamer as gst;

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext};

use crate::{Error, Result};

const GST_MAP_D3D11: gst::ffi::GstMapFlags = gst::ffi::GST_MAP_FLAG_LAST << 1;

pub(super) struct D3d11MemoryMap {
    memory: *mut gst::ffi::GstMemory,
    info: gst::ffi::GstMapInfo,
}

impl D3d11MemoryMap {
    pub(super) unsafe fn new(memory: *mut gst::ffi::GstMemory) -> Result<Self> {
        let mut info = unsafe { std::mem::zeroed() };
        // This map synchronizes a deferred upload and exposes the D3D resource;
        // it does not map decoded pixels into CPU-addressable memory.
        let flags = gst::ffi::GST_MAP_READ | GST_MAP_D3D11;
        if unsafe { gst::ffi::gst_memory_map(memory, &mut info, flags) } == gst::glib::ffi::GFALSE {
            return Err(Error::Pipeline(
                "could not map GStreamer D3D11 memory".into(),
            ));
        }
        Ok(Self { memory, info })
    }
}

impl Drop for D3d11MemoryMap {
    fn drop(&mut self) {
        unsafe { gst::ffi::gst_memory_unmap(self.memory, &mut self.info) };
    }
}

pub(super) struct SendGstDevice(pub(super) *mut c_void);

impl Drop for SendGstDevice {
    fn drop(&mut self) {
        unsafe { gst::glib::gobject_ffi::g_object_unref(self.0.cast()) };
    }
}

pub(super) unsafe fn gst_device_for_adapter(
    adapter_luid: u64,
) -> Result<(
    ID3D11Device,
    ID3D11DeviceContext,
    gst::Context,
    SendGstDevice,
)> {
    unsafe { gst_d3d11_memory_init_once() };
    let gst_device = unsafe { gst_d3d11_device_new_for_adapter_luid(adapter_luid as i64, 0) };
    if gst_device.is_null() {
        return Err(Error::Pipeline(
            "GStreamer could not create a device on WebView2's adapter".into(),
        ));
    }
    let device_raw = unsafe { gst_d3d11_device_get_device_handle(gst_device) };
    let context_raw = unsafe { gst_d3d11_device_get_device_context_handle(gst_device) };
    let device = unsafe { ID3D11Device::from_raw_borrowed(&device_raw) }
        .cloned()
        .ok_or_else(|| Error::Pipeline("GStreamer returned no D3D11 device".into()))?;
    let device_context = unsafe { ID3D11DeviceContext::from_raw_borrowed(&context_raw) }
        .cloned()
        .ok_or_else(|| Error::Pipeline("GStreamer returned no D3D11 device context".into()))?;
    let context = unsafe { gst_d3d11_context_new(gst_device) };
    if context.is_null() {
        return Err(Error::Pipeline(
            "GStreamer could not create a D3D11 device context".into(),
        ));
    }
    Ok((
        device,
        device_context,
        unsafe { from_glib_full(context) },
        SendGstDevice(gst_device),
    ))
}

#[link(name = "gstd3d11-1.0")]
unsafe extern "C" {
    fn gst_d3d11_memory_init_once();
    fn gst_d3d11_device_new_for_adapter_luid(adapter_luid: i64, flags: u32) -> *mut c_void;
    fn gst_d3d11_device_get_device_handle(device: *mut c_void) -> *mut c_void;
    fn gst_d3d11_device_get_device_context_handle(device: *mut c_void) -> *mut c_void;
    pub(super) fn gst_d3d11_device_lock(device: *mut c_void);
    pub(super) fn gst_d3d11_device_unlock(device: *mut c_void);
    fn gst_d3d11_context_new(device: *mut c_void) -> *mut gst::ffi::GstContext;
    pub(super) fn gst_is_d3d11_memory(memory: *mut gst::ffi::GstMemory) -> i32;
    pub(super) fn gst_d3d11_memory_get_resource_handle(memory: *mut c_void) -> *mut c_void;
    pub(super) fn gst_d3d11_memory_get_subresource_index(memory: *mut c_void) -> u32;
}
