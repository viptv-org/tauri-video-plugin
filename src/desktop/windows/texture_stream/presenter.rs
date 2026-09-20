use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use gstreamer as gst;

use parking_lot::{Mutex, RwLock};

use windows::{
    core::{IUnknown, Interface, PCWSTR},
    Win32::Graphics::{
        Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11VideoDevice},
        Dxgi::IDXGIKeyedMutex,
    },
};

use crate::{Error, Result};

use super::UiDispatch;
use super::d3d11::{
    D3d11MemoryMap, SendGstDevice, gst_d3d11_device_lock, gst_d3d11_device_unlock,
    gst_d3d11_memory_get_resource_handle, gst_d3d11_memory_get_subresource_index,
    gst_device_for_adapter, gst_is_d3d11_memory,
};
use super::targets::{DrawTarget, draw_target, reclaim_available};
use super::texture_error;
use super::webview2::{ICoreWebView2ExperimentalEnvironment12, SendStream};

const MAX_TEXTURES: u32 = 4;

pub struct TextureStreamPresenter {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    gst_device: SendGstDevice,
    gst_context: gst::Context,
    stream: SendStream,
    dispatch: UiDispatch,
    free: Arc<Mutex<Vec<DrawTarget>>>,
    texture_size: Arc<RwLock<Option<(u32, u32)>>>,
    pool_pending: Arc<AtomicBool>,
    reclaim_pending: Arc<AtomicBool>,
}

// NV12 resources are copied by GStreamer's serialized sample callback. All
// WebView2 COM method calls are dispatched back to its UI thread.
unsafe impl Send for TextureStreamPresenter {}
unsafe impl Sync for TextureStreamPresenter {}

impl TextureStreamPresenter {
    pub fn new<E: Interface>(
        environment: &E,
        stream_id: &str,
        dispatch: UiDispatch,
    ) -> Result<Self> {
        let environment: ICoreWebView2ExperimentalEnvironment12 = environment
            .cast()
            .map_err(texture_error("query WebView2 TextureStream support"))?;
        let adapter_luid = unsafe { environment.render_adapter_luid() }
            .map_err(texture_error("query the WebView2 render adapter"))?;
        let (device, context, gst_context, gst_device) =
            unsafe { gst_device_for_adapter(adapter_luid) }?;
        let device_unknown: IUnknown = device
            .cast()
            .map_err(texture_error("query the D3D11 device identity"))?;
        let stream_id = wide(stream_id);
        let stream = unsafe {
            environment.create_texture_stream(PCWSTR(stream_id.as_ptr()), &device_unknown)
        }
        .map_err(texture_error("create the WebView2 texture stream"))?;
        for origin in [
            "http://localhost:1420",
            "http://tauri.localhost",
            "https://tauri.localhost",
            "tauri://localhost",
        ] {
            let origin = wide(origin);
            let _ = unsafe { stream.add_allowed_origin(PCWSTR(origin.as_ptr()), false) };
        }
        Ok(Self {
            device,
            context,
            gst_device,
            gst_context,
            stream: SendStream(stream),
            dispatch,
            free: Arc::new(Mutex::new(Vec::with_capacity(MAX_TEXTURES as usize))),
            texture_size: Arc::new(RwLock::new(None)),
            pool_pending: Arc::new(AtomicBool::new(false)),
            reclaim_pending: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn gst_context(&self) -> gst::Context {
        self.gst_context.clone()
    }

    pub fn supports_gpu_color_conversion(&self) -> bool {
        self.device.cast::<ID3D11VideoDevice>().is_ok()
    }

    pub fn copy_sample(&self, sample: &gst::Sample, target: &DrawTarget) -> Result<()> {
        let buffer = sample
            .buffer()
            .ok_or_else(|| Error::Pipeline("Windows video sample has no buffer".into()))?;
        if buffer.n_memory() == 0 {
            return Err(Error::Pipeline(
                "Windows video sample has no D3D11 memory".into(),
            ));
        }
        let memory = buffer.peek_memory(0);
        let memory_ptr = memory.as_ptr() as *mut gst::ffi::GstMemory;
        if unsafe { gst_is_d3d11_memory(memory_ptr) } == 0 {
            return Err(Error::Pipeline(
                "Windows video sample did not negotiate D3D11Memory".into(),
            ));
        }
        let _memory_map = unsafe { D3d11MemoryMap::new(memory_ptr) }?;
        let source_raw = unsafe { gst_d3d11_memory_get_resource_handle(memory_ptr.cast()) };
        let source = unsafe { ID3D11Resource::from_raw_borrowed(&source_raw) }
            .ok_or_else(|| Error::Pipeline("GStreamer returned no D3D11 texture".into()))?;
        let source_subresource =
            unsafe { gst_d3d11_memory_get_subresource_index(memory_ptr.cast()) };
        let keyed_mutex: IDXGIKeyedMutex = target
            .resource
            .cast()
            .map_err(texture_error("query the WebView2 texture mutex"))?;
        unsafe {
            keyed_mutex
                .AcquireSync(0, 1_000)
                .map_err(texture_error("acquire the WebView2 texture"))?;
            gst_d3d11_device_lock(self.gst_device.0);
        }
        unsafe {
            if source_subresource == 0 {
                self.context.CopyResource(&target.resource, source);
            } else {
                self.context.CopySubresourceRegion(
                    &target.resource,
                    0,
                    0,
                    0,
                    0,
                    source,
                    source_subresource,
                    None,
                );
            }
            self.context.Flush();
        }
        unsafe {
            gst_d3d11_device_unlock(self.gst_device.0);
            keyed_mutex
                .ReleaseSync(0)
                .map_err(texture_error("release the WebView2 texture"))?;
        }
        Ok(())
    }

    pub fn acquire(&mut self, width: u32, height: u32) -> Result<Option<DrawTarget>> {
        let width = width.max(1);
        let height = height.max(1);
        if *self.texture_size.read() != Some((width, height)) {
            self.create_pool(width, height)?;
            return Ok(None);
        }
        if let Some(target) = self.free.lock().pop() {
            return Ok(Some(target));
        }
        self.reclaim()?;
        Ok(None)
    }

    pub fn present(&self, target: DrawTarget, timestamp_ns: u64) -> Result<()> {
        let stream = self.stream.clone();
        let free = Arc::clone(&self.free);
        let texture_size = Arc::clone(&self.texture_size);
        (self.dispatch)(Box::new(move || {
            let result = unsafe {
                target
                    .texture
                    .set_timestamp(timestamp_ns)
                    .and_then(|_| stream.present_texture(&target.texture))
            };
            if let Err(error) = result {
                tracing::warn!(%error, "failed to present a WebView2 video texture");
                free.lock().push(target);
                return;
            }
            reclaim_available(&stream, &free, &texture_size);
        }))
    }

    fn create_pool(&mut self, width: u32, height: u32) -> Result<()> {
        if self.pool_pending.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let stream = self.stream.clone();
        let free = Arc::clone(&self.free);
        let texture_size = Arc::clone(&self.texture_size);
        let pending = Arc::clone(&self.pool_pending);
        let dispatch_result = (self.dispatch)(Box::new(move || {
            let result: Result<()> = (|| {
                for old in free.lock().drain(..) {
                    unsafe { stream.0.close_texture(&old.texture) }
                        .map_err(texture_error("retire an old WebView2 video texture"))?;
                }
                let mut targets = Vec::with_capacity(MAX_TEXTURES as usize);
                for _ in 0..MAX_TEXTURES {
                    let texture = unsafe { stream.create_texture(width, height) }
                        .map_err(texture_error("allocate a WebView2 video texture"))?;
                    targets.push(unsafe { draw_target(texture) }?);
                }
                *free.lock() = targets;
                *texture_size.write() = Some((width, height));
                Ok(())
            })();
            if let Err(error) = result {
                tracing::warn!(%error, "failed to allocate the WebView2 video texture pool");
            }
            pending.store(false, Ordering::Release);
        }));
        if dispatch_result.is_err() {
            self.pool_pending.store(false, Ordering::Release);
        }
        dispatch_result
    }

    fn reclaim(&self) -> Result<()> {
        if self.reclaim_pending.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let stream = self.stream.clone();
        let free = Arc::clone(&self.free);
        let texture_size = Arc::clone(&self.texture_size);
        let pending = Arc::clone(&self.reclaim_pending);
        let dispatch_result = (self.dispatch)(Box::new(move || {
            reclaim_available(&stream, &free, &texture_size);
            pending.store(false, Ordering::Release);
        }));
        if dispatch_result.is_err() {
            self.reclaim_pending.store(false, Ordering::Release);
        }
        dispatch_result
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}
