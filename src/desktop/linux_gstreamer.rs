use std::{
    cell::RefCell,
    collections::BTreeSet,
    str::FromStr,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc, Arc, OnceLock,
    },
    time::Instant,
};

use gst::glib::prelude::Cast as GstCast;
use gst::glib::translate::ToGlibPtr as GstToGlibPtr;
use gst::prelude::ObjectExt as GstObjectExt;
use gst::prelude::*;
use gstreamer as gst;
use gtk::prelude::*;
use parking_lot::{Mutex, RwLock};
use tauri::{AppHandle, Runtime};

use crate::{
    models::{NativeOpenRequest, NativePlaybackSnapshot, NativeTrackInfo},
    Error, Result,
};

mod session;

use session::snapshot;
pub use session::{close, control, force_close, layout, shutdown, stats};

static GST_INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();

thread_local! {
    static PLAYER: RefCell<Option<NativePlayer>> = const { RefCell::new(None) };
}

type EngineJob = Box<dyn FnOnce(&gst::Element) + Send + 'static>;
struct EngineQueue {
    sender: mpsc::Sender<EngineJob>,
    alive: Arc<AtomicBool>,
}
struct EngineLifetime(Arc<AtomicBool>);
impl Drop for EngineLifetime {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
impl EngineQueue {
    fn new(pipeline: gst::Element) -> Result<Self> {
        let (sender, receiver) = mpsc::channel::<EngineJob>();
        let alive = Arc::new(AtomicBool::new(true));
        let lifetime = EngineLifetime(Arc::clone(&alive));
        std::thread::Builder::new()
            .name("viptv-gst-engine".into())
            .spawn(move || {
                let _lifetime = lifetime;
                while let Ok(job) = receiver.recv() {
                    job(&pipeline);
                }
            })
            .map_err(|_| Error::RuntimeUnavailable("native engine worker unavailable".into()))?;
        Ok(Self { sender, alive })
    }
    fn submit(&self, job: impl FnOnce(&gst::Element) + Send + 'static) {
        if self.sender.send(Box::new(job)).is_err() {
            self.alive.store(false, Ordering::Release);
        }
    }
}

struct NativePlayer {
    session_key: String,
    pipeline: gst::Element,
    gtk_sink: gst::Element,
    widget: gtk::Widget,
    picture: super::linux_picture::PictureViewport,
    source: Arc<RwLock<NativeOpenRequest>>,
    buffering_percent: i32,
    buffer_duration_seconds: Option<f64>,
    target_buffer_bytes: Option<u64>,
    desired_playing: bool,
    requested_state: Arc<AtomicU8>,
    engine: EngineQueue,
    telemetry: Arc<Mutex<session::EngineFacts>>,
    telemetry_pending: Arc<AtomicBool>,
    volume_filter: gst::Element,
    force_aspect: bool,
    error: Option<crate::error::NativeMediaFailure>,
    last_rendered: u64,
    last_sample_at: Instant,
    measured_fps: f64,
    tracks: Vec<NativeTrackInfo>,
    selected_streams: BTreeSet<String>,
    pending_selection: Option<BTreeSet<String>>,
}

pub fn open<R: Runtime>(
    app: &AppHandle<R>,
    payload: NativeOpenRequest,
) -> Result<NativePlaybackSnapshot> {
    super::linux_surface::ensure_host(app)?;
    open_player(payload)
}

pub(super) fn open_player(payload: NativeOpenRequest) -> Result<NativePlaybackSnapshot> {
    initialize_gstreamer()?;

    PLAYER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(player) = slot.as_mut() {
            load_source(player, &payload)?;
            return snapshot(player);
        }

        let mut player = create_player(&payload)?;
        let result = snapshot(&mut player)?;
        *slot = Some(player);
        Ok(result)
    })
}

fn initialize_gstreamer() -> Result<()> {
    GST_INIT
        .get_or_init(|| gst::init().map_err(|error| error.to_string()))
        .clone()
        .map_err(Error::RuntimeUnavailable)
}

pub(super) fn configure_source(element: &gst::Element, source: &NativeOpenRequest) {
    if let Some(user_agent) = source.user_agent.as_deref() {
        if element.find_property("user-agent").is_some() {
            element.set_property("user-agent", user_agent);
        }
    }
    if element.find_property("timeout").is_some() {
        element.set_property("timeout", 60u32);
    }
    if let Some(cookies) = &source.cookies {
        if element.find_property("cookies").is_some() {
            let cookies = gst::glib::StrV::from([cookies.as_str()]);
            element.set_property("cookies", cookies);
        }
    }
    if let Some(ca_file) = source.tls_ca_file.as_deref() {
        if element.find_property("tls-database").is_some() {
            match gio::TlsFileDatabase::new(ca_file) {
                Ok(database) => element.set_property("tls-database", database),
                Err(_) => tracing::error!("failed to load configured TLS trust database"),
            }
        } else if element.find_property("ssl-ca-file").is_some() {
            element.set_property("ssl-ca-file", ca_file);
        }
    }
    if element.find_property("extra-headers").is_some()
        && (!source.headers.is_empty() || source.referrer.is_some())
    {
        let mut headers = gst::Structure::new_empty("request-headers");
        for (name, value) in &source.headers {
            headers.set(name, value);
        }
        if let Some(referrer) = &source.referrer {
            if !source
                .headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("referer"))
            {
                headers.set("Referer", referrer);
            }
        }
        element.set_property("extra-headers", headers);
    }
}

fn create_player(payload: &NativeOpenRequest) -> Result<NativePlayer> {
    let source = Arc::new(RwLock::new(payload.clone()));

    let gtk_sink = gst::ElementFactory::make("gtkglsink")
        .property("force-aspect-ratio", true)
        .property("sync", true)
        .property("enable-last-sample", false)
        .build()
        .map_err(|error| Error::Pipeline(format!("gtkglsink is unavailable: {error}")))?;
    let terminal_sink = subtitle_safe_gtk_sink(&gtk_sink)?;
    let gl_sink = gst::ElementFactory::make("glsinkbin")
        .property("sink", &terminal_sink)
        .build()
        .map_err(|error| Error::Pipeline(format!("glsinkbin is unavailable: {error}")))?;
    let buffer_duration_seconds = payload
        .max_buffer_ms
        .map(|value| f64::from(value.clamp(3_000, 120_000)) / 1_000.0);
    let target_buffer_bytes = payload
        .target_buffer_bytes
        .map(|value| value.clamp(4 * 1024 * 1024, i32::MAX as u64));
    // Own volume outside playbin's topology lock: its volume setter can block
    // while audio/subtitle branches are being replaced.
    let volume_filter = gst::ElementFactory::make("volume")
        .property(
            "volume",
            if payload.muted {
                0.0
            } else {
                payload.volume.clamp(0.0, 1.0)
            },
        )
        .build()
        .map_err(|_| Error::RuntimeUnavailable("native audio volume filter unavailable".into()))?;
    let mut pipeline_builder = gst::ElementFactory::make("playbin3")
        .property("uri", &payload.uri)
        .property("video-sink", &gl_sink)
        .property("audio-filter", &volume_filter);
    if let Some(seconds) = buffer_duration_seconds {
        pipeline_builder =
            pipeline_builder.property("buffer-duration", (seconds * 1_000_000_000.0) as i64);
    }
    if let Some(bytes) = target_buffer_bytes {
        pipeline_builder = pipeline_builder.property("buffer-size", bytes as i32);
    }
    let pipeline = pipeline_builder
        .build()
        .map_err(|error| Error::Pipeline(format!("playbin3 is unavailable: {error}")))?;

    let source_for_setup = Arc::clone(&source);
    pipeline.connect("source-setup", false, move |values| {
        if let Ok(element) = values[1].get::<gst::Element>() {
            configure_source(&element, &source_for_setup.read());
        }
        None
    });

    let widget_object = gtk_sink.property::<gst::glib::Object>("widget");
    let widget_pointer: *mut gst::glib::gobject_ffi::GObject = widget_object.to_glib_none().0;
    let widget: gtk::Widget =
        unsafe { gtk::glib::translate::from_glib_none(widget_pointer as *mut gtk::ffi::GtkWidget) };
    widget.set_hexpand(false);
    widget.set_vexpand(false);
    let mut picture = super::linux_picture::PictureViewport::new(widget);
    let widget = picture.widget.clone();
    picture.layout(payload.width, payload.height);
    super::linux_surface::place_widget(
        &widget,
        payload.x,
        payload.y,
        payload.width,
        payload.height,
    )?;

    let engine = EngineQueue::new(pipeline.clone())?;
    let mut player = NativePlayer {
        session_key: String::new(),
        pipeline,
        gtk_sink,
        widget,
        picture,
        source,
        buffering_percent: 0,
        buffer_duration_seconds,
        target_buffer_bytes,
        desired_playing: payload.autoplay,
        requested_state: Arc::new(AtomicU8::new(gst::State::Null as u8)),
        engine,
        telemetry: Arc::new(Mutex::new(session::EngineFacts::default())),
        telemetry_pending: Arc::new(AtomicBool::new(false)),
        volume_filter,
        force_aspect: true,
        error: None,
        last_rendered: 0,
        last_sample_at: Instant::now(),
        measured_fps: 0.0,
        tracks: vec![],
        selected_streams: BTreeSet::new(),
        pending_selection: None,
    };
    load_source(&mut player, payload)?;
    Ok(player)
}

fn subtitle_safe_gtk_sink(gtk_sink: &gst::Element) -> Result<gst::Element> {
    let (major, minor, _, _) = gst::version();
    if (major, minor) < (1, 26) {
        return Ok(gtk_sink.clone());
    }
    let compositor = match gst::ElementFactory::make("gloverlaycompositor").build() {
        Ok(compositor) => compositor,
        Err(error) => {
            tracing::warn!(
                %error,
                "gloverlaycompositor is unavailable; GStreamer subtitles may flicker"
            );
            return Ok(gtk_sink.clone());
        }
    };
    let flattened_caps = gst::Caps::from_str("video/x-raw(memory:GLMemory),format=(string)RGBA")
        .map_err(|error| Error::Pipeline(error.to_string()))?;
    let caps_filter = gst::ElementFactory::make("capsfilter")
        // Excluding GstVideoOverlayCompositionMeta here prevents
        // gloverlaycompositor passthrough. Captions are flattened into the
        // GL texture before gtkglsink's redraw path can lose the metadata.
        .property("caps", flattened_caps)
        .build()
        .map_err(|error| Error::Pipeline(error.to_string()))?;
    let sink_bin = gst::Bin::new();
    sink_bin
        .add_many([&compositor, &caps_filter, gtk_sink])
        .map_err(|error| Error::Pipeline(error.to_string()))?;
    gst::Element::link_many([&compositor, &caps_filter, gtk_sink])
        .map_err(|error| Error::Pipeline(error.to_string()))?;
    let compositor_sink = compositor
        .static_pad("sink")
        .ok_or_else(|| Error::Pipeline("gloverlaycompositor has no sink pad".into()))?;
    let ghost = gst::GhostPad::builder_with_target(&compositor_sink)
        .map_err(|error| Error::Pipeline(error.to_string()))?
        .name("sink")
        .build();
    ghost
        .set_active(true)
        .map_err(|error| Error::Pipeline(error.to_string()))?;
    sink_bin
        .add_pad(&ghost)
        .map_err(|error| Error::Pipeline(error.to_string()))?;
    Ok(GstCast::upcast(sink_bin))
}

fn load_source(player: &mut NativePlayer, payload: &NativeOpenRequest) -> Result<()> {
    // GTK owns the surface; a serialized engine worker owns blocking
    // transitions, seeks and queries. Never wait on a sink from GTK.
    let buffer_duration_seconds = payload
        .max_buffer_ms
        .map(|value| f64::from(value.clamp(3_000, 120_000)) / 1_000.0);
    let target_buffer_bytes = payload
        .target_buffer_bytes
        .map(|value| value.clamp(4 * 1024 * 1024, i32::MAX as u64));
    player.requested_state.store(
        if payload.autoplay {
            gst::State::Playing
        } else {
            gst::State::Paused
        } as u8,
        Ordering::SeqCst,
    );
    player.pending_selection = None;
    *player.source.write() = payload.clone();
    if let Some(bus) = player.pipeline.bus() {
        bus.set_flushing(true);
    }
    *player.telemetry.lock() = session::EngineFacts::default();
    let source = Arc::clone(&player.source);
    let desired = Arc::clone(&player.requested_state);
    let payload = payload.clone();
    let loaded = payload.clone();
    player.volume_filter.set_property(
        "volume",
        if payload.muted {
            0.0
        } else {
            payload.volume.clamp(0.0, 1.0)
        },
    );
    player.engine.submit(move |pipeline| {
        if source.read().session_key != loaded.session_key {
            return;
        }
        let (transition, current, _) = match pipeline.set_state(gst::State::Ready) {
            Ok(_) => pipeline.state(Some(gst::ClockTime::from_seconds(5))),
            Err(_) => {
                if let Some(bus) = pipeline.bus() {
                    bus.set_flushing(false);
                }
                gst::element_error!(
                    pipeline,
                    gst::CoreError::Failed,
                    ("Native source transition failed")
                );
                return;
            }
        };
        if transition.is_err() || current != gst::State::Ready {
            if let Some(bus) = pipeline.bus() {
                bus.set_flushing(false);
            }
            gst::element_error!(
                pipeline,
                gst::CoreError::Failed,
                ("Native source transition failed")
            );
            return;
        }
        if source.read().session_key != loaded.session_key {
            return;
        }
        if let Some(bus) = pipeline.bus() {
            while bus.pop().is_some() {}
        }
        pipeline.set_property("uri", &loaded.uri);
        pipeline.set_property(
            "buffer-duration",
            buffer_duration_seconds
                .map(|seconds| (seconds * 1_000_000_000.0) as i64)
                .unwrap_or(-1),
        );
        pipeline.set_property(
            "buffer-size",
            target_buffer_bytes.map(|bytes| bytes as i32).unwrap_or(-1),
        );
        if let Some(bus) = pipeline.bus() {
            bus.set_flushing(false);
        }
        let state = if desired.load(Ordering::SeqCst) == gst::State::Playing as u8 {
            gst::State::Playing
        } else {
            gst::State::Paused
        };
        if pipeline.set_state(state).is_err() {
            gst::element_error!(
                pipeline,
                gst::CoreError::Failed,
                ("Native source start failed")
            );
        }
    });

    player.picture.source_size(0.0, 0.0);
    player.picture.layout(payload.width, payload.height);
    super::linux_surface::place_widget(
        &player.widget,
        payload.x,
        payload.y,
        payload.width,
        payload.height,
    )?;

    player.buffering_percent = 0;
    player.buffer_duration_seconds = buffer_duration_seconds;
    player.target_buffer_bytes = target_buffer_bytes;
    player.desired_playing = payload.autoplay;
    player.error = None;
    player.last_rendered = rendered_frames(&player.gtk_sink);
    player.last_sample_at = Instant::now();
    player.measured_fps = 0.0;
    player.tracks.clear();
    player.selected_streams.clear();

    player.session_key.clone_from(&payload.session_key);
    session::schedule_aspect(player, true);
    Ok(())
}

fn rendered_frames(gtk_sink: &gst::Element) -> u64 {
    gtk_sink
        .property::<gst::Structure>("stats")
        .get::<u64>("rendered")
        .unwrap_or(0)
}

fn playback_timeline(pipeline: &gst::Element, duration: f64) -> (bool, bool, f64, f64) {
    let mut latency = gst::query::Latency::new();
    let live = duration <= 0.0 || (pipeline.query(latency.query_mut()) && latency.result().0);
    let mut seeking = gst::query::Seeking::new(gst::Format::Time);
    if !pipeline.query(seeking.query_mut()) {
        return (live, !live, 0.0, duration);
    }
    let (seekable, start, end) = seeking.result();
    let seconds = |value: gst::GenericFormattedValue| match value {
        gst::GenericFormattedValue::Time(Some(time)) => time.seconds_f64(),
        _ => 0.0,
    };
    (live, seekable, seconds(start), seconds(end))
}

#[cfg(test)]
pub(super) fn qualification_volume() -> f64 {
    PLAYER.with(|slot| {
        slot.borrow()
            .as_ref()
            .unwrap()
            .volume_filter
            .property::<f64>("volume")
    })
}

#[cfg(test)]
pub(super) fn qualification_picture() -> (i32, i32) {
    PLAYER.with(|slot| {
        slot.borrow()
            .as_ref()
            .unwrap()
            .picture
            .qualification_dimensions()
    })
}
