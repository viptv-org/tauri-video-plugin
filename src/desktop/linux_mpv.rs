use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use gtk::prelude::*;
use libmpv2::{
    events::mpv_event_id,
    render::{mpv_render_update, OpenGLInitParams, RenderContext, RenderParam, RenderParamApiType},
    Mpv,
};
use tauri::{AppHandle, Runtime};

use crate::{
    models::{NativeOpenRequest, NativePlaybackSnapshot, NativeTrackInfo, TrackKind},
    Error, Result,
};

mod headers;
mod session;

pub use session::{close, control, force_close, layout, stats};
use session::{mpv_error, open_gl_proc_address, property, schedule_layout_render, snapshot};

#[link(name = "GL")]
unsafe extern "C" {
    fn glXGetProcAddressARB(name: *const u8) -> *mut c_void;
    fn glGetIntegerv(parameter: u32, value: *mut i32);
}

thread_local! {
    static PLAYER: RefCell<Option<MpvPlayer>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct TrackTarget {
    public_index: i32,
    mpv_id: i64,
    kind: TrackKind,
}

#[derive(Clone, Copy)]
pub(super) struct MpvBufferDefaults {
    cache_seconds: f64,
    readahead_seconds: f64,
    forward_bytes: i64,
    backward_bytes: i64,
    donate_buffer: bool,
    hysteresis_seconds: f64,
}

impl MpvBufferDefaults {
    pub(super) fn read(mpv: &Mpv) -> Self {
        Self {
            cache_seconds: property(mpv, "cache-secs").unwrap_or(3_600_000.0),
            readahead_seconds: property(mpv, "demuxer-readahead-secs").unwrap_or(1.0),
            forward_bytes: property(mpv, "demuxer-max-bytes").unwrap_or(150 * 1024 * 1024),
            backward_bytes: property(mpv, "demuxer-max-back-bytes").unwrap_or(50 * 1024 * 1024),
            donate_buffer: property(mpv, "demuxer-donate-buffer").unwrap_or(true),
            hysteresis_seconds: property(mpv, "demuxer-hysteresis-secs").unwrap_or(0.0),
        }
    }
}

static REDRAW_PENDING: AtomicBool = AtomicBool::new(false);

struct MpvPlayer {
    session_key: String,
    mpv: Mpv,
    render_context: Rc<RefCell<Option<RenderContext>>>,
    gl_area: gtk::GLArea,
    widget: gtk::Widget,
    render_signal: Option<gtk::glib::SignalHandlerId>,
    update_source: Option<gtk::glib::SourceId>,
    layout_redraw_pending: Rc<Cell<bool>>,
    presented_frames: Rc<Cell<u64>>,
    last_presented_frames: u64,
    last_sample_at: Instant,
    measured_fps: f64,
    layout_commits: Cell<u64>,
    layout_sample_at: Cell<Instant>,
    tracks: Vec<NativeTrackInfo>,
    track_targets: Vec<TrackTarget>,
    tracks_dirty: bool,
    default_buffer: MpvBufferDefaults,
    error: Rc<RefCell<Option<String>>>,
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        let _ = self.mpv.command("stop", &[]);
        if let Some(source) = self.update_source.take() {
            source.remove();
        }
        if let Some(signal) = self.render_signal.take() {
            self.gl_area.disconnect(signal);
        }
        // libmpv requires its render context to be destroyed before the
        // owning mpv handle, with the same OpenGL context current as at
        // creation. GTK/WebKit may have selected another context since our
        // last render callback.
        self.gl_area.make_current();
        *self.render_context.borrow_mut() = None;
        self.widget.hide();
    }
}

pub fn open<R: Runtime>(
    app: &AppHandle<R>,
    payload: NativeOpenRequest,
) -> Result<NativePlaybackSnapshot> {
    super::linux_surface::ensure_host(app)?;
    open_player(payload)
}

pub(super) fn open_player(payload: NativeOpenRequest) -> Result<NativePlaybackSnapshot> {
    PLAYER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(player) = slot.as_mut() {
            load_source(player, &payload)?;
            return snapshot(player);
        }
        let mut player = create_player(&payload)?;
        tracing::debug!("mpv init: collecting initial snapshot");
        let result = snapshot(&mut player)?;
        tracing::debug!("mpv init: initial snapshot complete");
        *slot = Some(player);
        Ok(result)
    })
}

/// The libmpv engine handle with the embedded player's production
/// options. Separated from the surface binding so engine-level tests can
/// drive the same handle configuration without a GL area.
pub(super) fn create_engine() -> Result<Mpv> {
    let mpv = Mpv::with_initializer(|init| {
        init.set_option("vo", "libmpv")?;
        init.set_option("hwdec", "auto-safe")?;
        init.set_option("gpu-api", "opengl")?;
        // GtkGLArea renders on GTK's main thread. libmpv otherwise waits
        // here for each frame's target time (50 ms by default), delaying
        // widget allocation and scroll-driven surface moves.
        init.set_option("video-timing-offset", 0.0_f64)?;
        init.set_option("keep-open", "yes")?;
        init.set_option("osc", "no")?;
        init.set_option("osd-level", "0")?;
        init.set_option("input-default-bindings", "no")?;
        init.set_option("cache", "yes")?;
        Ok(())
    })
    .map_err(mpv_error)?;
    tracing::debug!("mpv init: handle ready");
    mpv.disable_deprecated_events().map_err(mpv_error)?;
    mpv.disable_event(mpv_event_id::Tick).map_err(mpv_error)?;
    Ok(mpv)
}

fn create_player(payload: &NativeOpenRequest) -> Result<MpvPlayer> {
    tracing::debug!("mpv init: begin");
    // libmpv parses floating-point options through the C locale and rejects
    // process locales whose decimal separator is not `.`. This is its
    // documented embedding precondition and is set before the handle exists.
    let locale = unsafe { libc::setlocale(libc::LC_NUMERIC, c"C".as_ptr()) };
    if locale.is_null() {
        return Err(Error::Pipeline(
            "mpv backend could not select the required C numeric locale".into(),
        ));
    }
    let gl_area = gtk::GLArea::new();
    gl_area.set_auto_render(false);
    gl_area.set_has_alpha(false);
    gl_area.set_has_depth_buffer(false);
    gl_area.set_has_stencil_buffer(false);
    gl_area.set_use_es(false);
    gl_area.set_required_version(3, 2);
    gl_area.connect_resize(|area, _, _| {
        // GtkGLArea::resize runs with the new framebuffer and a current GL
        // context. Invalidate libmpv's previous-sized frame immediately;
        // otherwise a paused or ended video stays at the old dimensions
        // until playback happens to produce another render notification.
        area.queue_render();
    });
    gl_area.set_hexpand(false);
    gl_area.set_vexpand(false);
    let widget = gl_area.clone().upcast::<gtk::Widget>();
    super::linux_surface::place_widget(
        &widget,
        payload.x,
        payload.y,
        payload.width,
        payload.height,
    )?;
    gl_area.realize();
    gl_area.make_current();
    if let Some(error) = gl_area.error() {
        return Err(Error::VideoOutput(format!(
            "could not create the mpv OpenGL surface: {error}"
        )));
    }
    tracing::debug!("mpv init: GL area ready");

    let mut mpv = create_engine()?;
    let default_buffer = MpvBufferDefaults::read(&mpv);

    let mut context = RenderContext::new(
        unsafe { mpv.ctx.as_mut() },
        [
            RenderParam::ApiType(RenderParamApiType::OpenGl),
            RenderParam::InitParams(OpenGLInitParams {
                get_proc_address: open_gl_proc_address,
                ctx: (),
            }),
        ],
    )
    .map_err(mpv_error)?;
    tracing::debug!("mpv init: render context ready");

    // This callback runs on libmpv's render thread. A channel lock here can
    // deadlock render-context destruction on GTK. The callback must stay
    // allocation-free and lock-free, including while its owner is closing.
    REDRAW_PENDING.store(false, Ordering::Release);
    context.set_update_callback(|| REDRAW_PENDING.store(true, Ordering::Release));
    let render_context = Rc::new(RefCell::new(Some(context)));
    let update_area = gl_area.clone();
    let context_for_update = Rc::clone(&render_context);
    let update_source = gtk::glib::timeout_add_local(Duration::from_millis(16), move || {
        if !REDRAW_PENDING.swap(false, Ordering::AcqRel) {
            return gtk::glib::ControlFlow::Continue;
        }
        update_area.make_current();
        if update_area.error().is_some() {
            return gtk::glib::ControlFlow::Continue;
        }
        let update = context_for_update
            .borrow()
            .as_ref()
            .ok_or_else(|| "mpv render context is closed".to_owned())
            .and_then(|context| context.update().map_err(|error| error.to_string()));
        match update {
            Ok(flags) if flags & mpv_render_update::Frame != 0 => {
                if let Some(parent) = update_area.parent() {
                    parent.queue_draw();
                }
                update_area.queue_render();
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "mpv render update error"),
        }
        gtk::glib::ControlFlow::Continue
    });

    let presented_frames = Rc::new(Cell::new(0_u64));
    let render_error = Rc::new(RefCell::new(None));
    let context_for_render = Rc::clone(&render_context);
    let frames_for_render = Rc::clone(&presented_frames);
    let error_for_render = Rc::clone(&render_error);
    let render_signal = gl_area.connect_render(move |area, _| {
        const GL_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
        let scale = area.scale_factor().max(1);
        let width = area.allocated_width().max(1).saturating_mul(scale);
        let height = area.allocated_height().max(1).saturating_mul(scale);
        let mut framebuffer = 0_i32;
        // GtkGLArea renders into its own framebuffer rather than necessarily
        // binding OpenGL's default framebuffer 0.
        unsafe { glGetIntegerv(GL_FRAMEBUFFER_BINDING, &mut framebuffer) };
        let rendered = context_for_render
            .borrow()
            .as_ref()
            .ok_or_else(|| "mpv render context is closed".to_owned())
            .and_then(|context| {
                context
                    .render::<()>(framebuffer, width, height, true)
                    .map_err(|error| error.to_string())?;
                context.report_swap();
                Ok(())
            });
        match rendered {
            Ok(()) => frames_for_render.set(frames_for_render.get().saturating_add(1)),
            Err(error) => {
                tracing::warn!(%error, "mpv render error");
                *error_for_render.borrow_mut() = Some(error);
            }
        }
        gtk::glib::Propagation::Stop
    });
    tracing::debug!("mpv init: GTK render callbacks ready");
    // The render update callback is installed before GTK's render signal;
    // request the bootstrap frame explicitly so mpv cannot wait forever
    // for a consumer after its immediate callback races this connection.
    gl_area.queue_render();

    let mut player = MpvPlayer {
        session_key: String::new(),
        mpv,
        render_context,
        gl_area,
        widget,
        render_signal: Some(render_signal),
        update_source: Some(update_source),
        layout_redraw_pending: Rc::new(Cell::new(false)),
        presented_frames,
        last_presented_frames: 0,
        last_sample_at: Instant::now(),
        measured_fps: 0.0,
        layout_commits: Cell::new(0),
        layout_sample_at: Cell::new(Instant::now()),
        tracks: Vec::new(),
        track_targets: Vec::new(),
        tracks_dirty: true,
        default_buffer,
        error: render_error,
    };
    tracing::debug!("mpv init: loading source");
    load_source(&mut player, payload)?;
    tracing::debug!("mpv init: source command complete");
    Ok(player)
}

fn load_source(player: &mut MpvPlayer, payload: &NativeOpenRequest) -> Result<()> {
    open_engine_source(&player.mpv, payload, player.default_buffer)?;
    super::linux_surface::place_widget(
        &player.widget,
        payload.x,
        payload.y,
        payload.width,
        payload.height,
    )?;
    schedule_layout_render(player);
    player.session_key.clone_from(&payload.session_key);
    player.tracks.clear();
    player.track_targets.clear();
    player.tracks_dirty = true;
    *player.error.borrow_mut() = None;
    player.last_presented_frames = player.presented_frames.get();
    player.last_sample_at = Instant::now();
    player.measured_fps = 0.0;
    player.layout_commits.set(0);
    player.layout_sample_at.set(Instant::now());
    Ok(())
}

/// The complete engine open/replace sequence, shared with real HTTP tests.
pub(super) fn open_engine_source(
    mpv: &Mpv,
    payload: &NativeOpenRequest,
    defaults: MpvBufferDefaults,
) -> Result<()> {
    mpv.command("stop", &[]).map_err(mpv_error)?;
    configure_network(mpv, payload)?;
    configure_buffer(mpv, defaults, payload)?;
    mpv.set_property(
        "volume",
        if payload.muted {
            0.0
        } else {
            payload.volume.clamp(0.0, 1.0) * 100.0
        },
    )
    .map_err(mpv_error)?;
    // Open at the requested title position instead of starting at zero
    // and seeking after opening: the opening seconds of the wrong
    // position were visible and audible before the post-open seek
    // landed, which read as "the first seek restarts at the beginning".
    mpv.set_property(
        "start",
        format!("+{:.3}", payload.start_at_seconds.max(0.0)),
    )
    .map_err(mpv_error)?;
    mpv.set_property("pause", !payload.autoplay)
        .map_err(mpv_error)?;
    mpv.command("loadfile", &[&payload.uri, "replace"])
        .map_err(mpv_error)?;
    Ok(())
}

fn configure_network(mpv: &Mpv, payload: &NativeOpenRequest) -> Result<()> {
    let mut headers = payload.headers.clone();
    if let Some(value) = payload.cookies.as_ref().filter(|value| !value.is_empty()) {
        headers.insert("Cookie".into(), value.clone());
    }
    if let Some(value) = payload.referrer.as_ref().filter(|value| !value.is_empty()) {
        headers.insert("Referer".into(), value.clone());
    }
    let fields = headers
        .into_iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect();
    mpv.set_property("http-header-fields", headers::HeaderList::new(fields)?)
        .map_err(mpv_error)?;
    mpv.set_property(
        "user-agent",
        payload
            .user_agent
            .clone()
            .unwrap_or_else(|| "tauri-plugin-video".into()),
    )
    .map_err(mpv_error)?;
    mpv.set_property(
        "tls-ca-file",
        payload.tls_ca_file.clone().unwrap_or_default(),
    )
    .map_err(mpv_error)?;
    Ok(())
}

fn configure_buffer(
    mpv: &Mpv,
    defaults: MpvBufferDefaults,
    payload: &NativeOpenRequest,
) -> Result<()> {
    let (cache_seconds, readahead_seconds) = payload.max_buffer_ms.map_or(
        (defaults.cache_seconds, defaults.readahead_seconds),
        |milliseconds| {
            let seconds = (f64::from(milliseconds) / 1_000.0).clamp(3.0, 120.0);
            (seconds, seconds)
        },
    );
    let (forward_bytes, backward_bytes, donate_buffer) = payload.target_buffer_bytes.map_or(
        (
            defaults.forward_bytes,
            defaults.backward_bytes,
            defaults.donate_buffer,
        ),
        |requested| {
            let total = requested.clamp(8 * 1024 * 1024, i64::MAX as u64);
            let backward = (total / 4)
                .clamp(4 * 1024 * 1024, 16 * 1024 * 1024)
                .min(total / 2);
            ((total - backward) as i64, backward as i64, false)
        },
    );
    mpv.set_property("cache-secs", cache_seconds)
        .map_err(mpv_error)?;
    mpv.set_property("demuxer-readahead-secs", readahead_seconds)
        .map_err(mpv_error)?;
    mpv.set_property("demuxer-max-bytes", forward_bytes)
        .map_err(mpv_error)?;
    mpv.set_property("demuxer-max-back-bytes", backward_bytes)
        .map_err(mpv_error)?;
    mpv.set_property("demuxer-donate-buffer", donate_buffer)
        .map_err(mpv_error)?;
    mpv.set_property("demuxer-hysteresis-secs", defaults.hysteresis_seconds)
        .map_err(mpv_error)?;
    Ok(())
}

#[cfg(test)]
pub(super) fn qualification_volume() -> f64 {
    PLAYER.with(|slot| {
        property::<f64>(&slot.borrow().as_ref().unwrap().mpv, "volume").unwrap() / 100.0
    })
}
#[cfg(test)]
pub(super) fn qualification_panscan() -> f64 {
    PLAYER.with(|slot| property::<f64>(&slot.borrow().as_ref().unwrap().mpv, "panscan").unwrap())
}

#[cfg(test)]
pub(super) fn qualification_surface() -> (bool, bool, bool, i32, i32, String) {
    PLAYER.with(|slot| {
        let p = slot.borrow();
        let p = p.as_ref().unwrap();
        (
            p.gl_area.is_visible(),
            p.gl_area.is_mapped(),
            p.gl_area.is_realized(),
            p.gl_area.allocated_width(),
            p.gl_area.allocated_height(),
            p.gl_area.error().map(|e| e.to_string()).unwrap_or_default(),
        )
    })
}
