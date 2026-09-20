use std::{
    cell::RefCell,
    collections::BTreeSet,
    str::FromStr,
    sync::{Arc, OnceLock},
    time::Instant,
};

use gst::glib::prelude::Cast as GstCast;
use gst::glib::translate::ToGlibPtr as GstToGlibPtr;
use gst::prelude::ObjectExt as GstObjectExt;
use gst::prelude::*;
use gstreamer as gst;
use gtk::prelude::*;
use parking_lot::RwLock;
use tauri::{AppHandle, Runtime};

use crate::{
    models::{NativeOpenRequest, NativePlaybackSnapshot, NativeTrackInfo},
    Error, Result,
};

mod session;

pub use session::{close, control, force_close, layout, stats};
use session::snapshot;

static GST_INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();

thread_local! {
    static PLAYER: RefCell<Option<NativePlayer>> = const { RefCell::new(None) };
}

struct NativePlayer {
    session_key: String,
    pipeline: gst::Element,
    gtk_sink: gst::Element,
    widget: gtk::Widget,
    source: Arc<RwLock<NativeOpenRequest>>,
    buffering_percent: i32,
    buffer_duration_seconds: Option<f64>,
    target_buffer_bytes: Option<u64>,
    desired_playing: bool,
    error: Option<String>,
    last_rendered: u64,
    last_sample_at: Instant,
    measured_fps: f64,
    tracks: Vec<NativeTrackInfo>,
    selected_streams: BTreeSet<String>,
}

pub fn open<R: Runtime>(
    app: &AppHandle<R>,
    payload: NativeOpenRequest,
) -> Result<NativePlaybackSnapshot> {
    initialize_gstreamer()?;
    super::linux_surface::ensure_host(app)?;

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

fn configure_source(element: &gst::Element, source: &NativeOpenRequest) {
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
                Err(error) => tracing::error!(%error, %ca_file, "failed to load TLS CA file"),
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
    let mut pipeline_builder = gst::ElementFactory::make("playbin3")
        .property("uri", &payload.uri)
        .property("video-sink", &gl_sink);
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
    let widget: gtk::Widget = unsafe {
        gtk::glib::translate::from_glib_none(widget_pointer as *mut gtk::ffi::GtkWidget)
    };
    widget.set_hexpand(false);
    widget.set_vexpand(false);
    super::linux_surface::place_widget(
        &widget,
        payload.x,
        payload.y,
        payload.width,
        payload.height,
    )?;

    let mut player = NativePlayer {
        session_key: String::new(),
        pipeline,
        gtk_sink,
        widget,
        source,
        buffering_percent: 0,
        buffer_duration_seconds,
        target_buffer_bytes,
        desired_playing: payload.autoplay,
        error: None,
        last_rendered: 0,
        last_sample_at: Instant::now(),
        measured_fps: 0.0,
        tracks: vec![],
        selected_streams: BTreeSet::new(),
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
    let flattened_caps =
        gst::Caps::from_str("video/x-raw(memory:GLMemory),format=(string)RGBA")
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
    // Keep the GTK GL sink and widget alive across media changes. Destroying
    // and immediately recreating gtkglsink can invalidate GDK's active EGL
    // draw context on Wayland compositors.
    player
        .pipeline
        .set_state(gst::State::Ready)
        .map_err(|error| Error::Pipeline(error.to_string()))?;
    let (transition, current, pending) =
        player.pipeline.state(Some(gst::ClockTime::from_seconds(5)));
    transition.map_err(|error| {
        Error::Pipeline(format!(
            "failed to park native pipeline before source change: {error}"
        ))
    })?;
    if current != gst::State::Ready {
        return Err(Error::Pipeline(format!(
            "native pipeline did not reach READY before source change (current: {current:?}, pending: {pending:?})"
        )));
    }
    if let Some(bus) = player.pipeline.bus() {
        while bus.pop().is_some() {}
    }

    *player.source.write() = payload.clone();
    let buffer_duration_seconds = payload
        .max_buffer_ms
        .map(|value| f64::from(value.clamp(3_000, 120_000)) / 1_000.0);
    let target_buffer_bytes = payload
        .target_buffer_bytes
        .map(|value| value.clamp(4 * 1024 * 1024, i32::MAX as u64));
    player.pipeline.set_property("uri", &payload.uri);
    player.pipeline.set_property(
        "buffer-duration",
        buffer_duration_seconds
            .map(|seconds| (seconds * 1_000_000_000.0) as i64)
            .unwrap_or(-1),
    );
    player.pipeline.set_property(
        "buffer-size",
        target_buffer_bytes.map(|bytes| bytes as i32).unwrap_or(-1),
    );
    player.pipeline.set_property(
        "volume",
        if payload.muted {
            0.0
        } else {
            payload.volume.clamp(0.0, 1.0)
        },
    );
    player.gtk_sink.set_property("force-aspect-ratio", true);
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

    let state = if payload.autoplay {
        gst::State::Playing
    } else {
        gst::State::Paused
    };
    player
        .pipeline
        .set_state(state)
        .map_err(|error| Error::Pipeline(error.to_string()))?;
    player.session_key.clone_from(&payload.session_key);
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
