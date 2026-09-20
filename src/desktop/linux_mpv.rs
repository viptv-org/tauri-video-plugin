use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    rc::Rc,
    time::Instant,
};

use gtk::prelude::*;
use libmpv2::{
    events::mpv_event_id,
    render::{
        mpv_render_update, OpenGLInitParams, RenderContext, RenderParam, RenderParamApiType,
    },
    Mpv,
};
use tauri::{AppHandle, Runtime};

use crate::{
    models::{NativeOpenRequest, NativePlaybackSnapshot, NativeTrackInfo, TrackKind},
    Error, Result,
};

mod session;

pub use session::{close, control, force_close, layout, stats};
use session::{encode_mpv_list, mpv_error, open_gl_proc_address, property, schedule_layout_render, snapshot};

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
struct MpvBufferDefaults {
    cache_seconds: f64,
    readahead_seconds: f64,
    forward_bytes: i64,
    backward_bytes: i64,
    donate_buffer: bool,
    hysteresis_seconds: f64,
}

impl MpvBufferDefaults {
    fn read(mpv: &Mpv) -> Self {
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
        // owning mpv handle.
        *self.render_context.borrow_mut() = None;
        self.widget.hide();
    }
}

pub fn open<R: Runtime>(
    app: &AppHandle<R>,
    payload: NativeOpenRequest,
) -> Result<NativePlaybackSnapshot> {
    super::linux_surface::ensure_host(app)?;
    PLAYER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(player) = slot.as_mut() {
            load_source(player, &payload)?;
            return snapshot(player);
        }
        let mut player = create_player(&payload)?;
        if std::env::var_os("TAURI_VIDEO_TELEMETRY").is_some() {
            eprintln!("mpv init: collecting initial snapshot");
        }
        let result = snapshot(&mut player)?;
        if std::env::var_os("TAURI_VIDEO_TELEMETRY").is_some() {
            eprintln!("mpv init: initial snapshot complete");
        }
        *slot = Some(player);
        Ok(result)
    })
}

/// The libmpv engine handle with the embedded player's production
/// options. Separated from the surface binding so engine-level tests can
/// drive the same handle configuration without a GL area.
pub(super) fn create_engine(trace: bool) -> Result<Mpv> {
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
    if trace {
        eprintln!("mpv init: handle ready");
    }
    mpv.disable_deprecated_events().map_err(mpv_error)?;
    mpv.disable_event(mpv_event_id::Tick).map_err(mpv_error)?;
    Ok(mpv)
}

fn create_player(payload: &NativeOpenRequest) -> Result<MpvPlayer> {
    let trace = std::env::var_os("TAURI_VIDEO_TELEMETRY").is_some();
    if trace {
        eprintln!("mpv init: begin");
    }
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
        return Err(Error::Pipeline(format!(
            "could not create the mpv OpenGL surface: {error}"
        )));
    }
    if trace {
        eprintln!("mpv init: GL area ready");
    }

    let mut mpv = create_engine(trace)?;
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
    if trace {
        eprintln!("mpv init: render context ready");
    }

    #[allow(deprecated)]
    let (redraw_sender, redraw_receiver) =
        gtk::glib::MainContext::sync_channel::<()>(gtk::glib::Priority::HIGH_IDLE, 1);
    context.set_update_callback(move || {
        // A one-item channel coalesces bursts without blocking libmpv or
        // building a main-loop backlog. HIGH_IDLE runs after normal event
        // and Tauri command dispatch but just before GTK's redraw phase.
        let _ = redraw_sender.try_send(());
    });
    let render_context = Rc::new(RefCell::new(Some(context)));
    let update_area = gl_area.clone();
    let context_for_update = Rc::clone(&render_context);
    let update_source = redraw_receiver.attach(None, move |_| {
        let update = context_for_update
            .borrow()
            .as_ref()
            .ok_or_else(|| "mpv render context is closed".to_owned())
            .and_then(|context| context.update().map_err(|error| error.to_string()));
        match update {
            Ok(flags) if flags & mpv_render_update::Frame != 0 => {
                update_area.queue_render();
            }
            Ok(_) => {}
            Err(error) => eprintln!("mpv render update error: {error}"),
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
                eprintln!("mpv render error: {error}");
                *error_for_render.borrow_mut() = Some(error);
            }
        }
        gtk::glib::Propagation::Stop
    });
    if trace {
        eprintln!("mpv init: GTK render callbacks ready");
    }
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
    if trace {
        eprintln!("mpv init: loading source");
    }
    load_source(&mut player, payload)?;
    if trace {
        eprintln!("mpv init: source command complete");
    }
    Ok(player)
}

fn load_source(player: &mut MpvPlayer, payload: &NativeOpenRequest) -> Result<()> {
    player.mpv.command("stop", &[]).map_err(mpv_error)?;
    configure_network(player, payload)?;
    configure_buffer(player, payload)?;
    player
        .mpv
        .set_property(
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
    player
        .mpv
        .set_property(
            "start",
            format!("+{:.3}", payload.start_at_seconds.max(0.0)),
        )
        .map_err(mpv_error)?;
    player
        .mpv
        .set_property("pause", !payload.autoplay)
        .map_err(mpv_error)?;
    player
        .mpv
        .command("loadfile", &[&payload.uri, "replace"])
        .map_err(mpv_error)?;
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

fn configure_network(player: &MpvPlayer, payload: &NativeOpenRequest) -> Result<()> {
    let mut headers = payload.headers.clone();
    if let Some(value) = payload.cookies.as_ref().filter(|value| !value.is_empty()) {
        headers.insert("Cookie".into(), value.clone());
    }
    if let Some(value) = payload.referrer.as_ref().filter(|value| !value.is_empty()) {
        headers.insert("Referer".into(), value.clone());
    }
    if !headers.is_empty() {
        let values = headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}"))
            .collect::<Vec<_>>();
        player
            .mpv
            .set_property("http-header-fields", encode_mpv_list(&values))
            .map_err(mpv_error)?;
    } else {
        player
            .mpv
            .set_property("http-header-fields", String::new())
            .map_err(mpv_error)?;
    }
    player
        .mpv
        .set_property(
            "user-agent",
            payload
                .user_agent
                .clone()
                .unwrap_or_else(|| "tauri-plugin-video".into()),
        )
        .map_err(mpv_error)?;
    player
        .mpv
        .set_property(
            "tls-ca-file",
            payload.tls_ca_file.clone().unwrap_or_default(),
        )
        .map_err(mpv_error)?;
    Ok(())
}

fn configure_buffer(player: &MpvPlayer, payload: &NativeOpenRequest) -> Result<()> {
    let (cache_seconds, readahead_seconds) = payload.max_buffer_ms.map_or(
        (
            player.default_buffer.cache_seconds,
            player.default_buffer.readahead_seconds,
        ),
        |milliseconds| {
            let seconds = (f64::from(milliseconds) / 1_000.0).clamp(3.0, 120.0);
            (seconds, seconds)
        },
    );
    let (forward_bytes, backward_bytes, donate_buffer) = payload.target_buffer_bytes.map_or(
        (
            player.default_buffer.forward_bytes,
            player.default_buffer.backward_bytes,
            player.default_buffer.donate_buffer,
        ),
        |requested| {
            let total = requested.clamp(8 * 1024 * 1024, i64::MAX as u64);
            let backward = (total / 4)
                .clamp(4 * 1024 * 1024, 16 * 1024 * 1024)
                .min(total / 2);
            ((total - backward) as i64, backward as i64, false)
        },
    );
    player
        .mpv
        .set_property("cache-secs", cache_seconds)
        .map_err(mpv_error)?;
    player
        .mpv
        .set_property("demuxer-readahead-secs", readahead_seconds)
        .map_err(mpv_error)?;
    player
        .mpv
        .set_property("demuxer-max-bytes", forward_bytes)
        .map_err(mpv_error)?;
    player
        .mpv
        .set_property("demuxer-max-back-bytes", backward_bytes)
        .map_err(mpv_error)?;
    player
        .mpv
        .set_property("demuxer-donate-buffer", donate_buffer)
        .map_err(mpv_error)?;
    player
        .mpv
        .set_property(
            "demuxer-hysteresis-secs",
            player.default_buffer.hysteresis_seconds,
        )
        .map_err(mpv_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpv_list_encoding_preserves_commas_in_headers() {
        assert_eq!(
            encode_mpv_list(&["Cookie: a=1,b=2".into()]),
            "%15%Cookie: a=1,b=2"
        );
    }
}
