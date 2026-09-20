use std::{
    ffi::{c_void, CString},
    fmt::Write as _,
    rc::Rc,
    time::Instant,
};

use gtk::prelude::*;
use libmpv2::{events::Event, Mpv};

use crate::{
    models::{
        NativeControlRequest, NativeLayoutRequest, NativePlaybackSnapshot, NativeSessionRequest,
        NativeTrackInfo, TrackKind,
    },
    Error, Result,
};

use super::{glXGetProcAddressARB, MpvPlayer, PLAYER, TrackTarget};

pub fn control(payload: NativeControlRequest) -> Result<NativePlaybackSnapshot> {
    PLAYER.with(|slot| {
        let mut slot = slot.borrow_mut();
        let player = slot
            .as_mut()
            .ok_or_else(|| Error::InvalidRequest("native player is not open".into()))?;
        ensure_session(&player.session_key, &payload.session_key)?;
        match payload.action.as_str() {
            "play" => player.mpv.set_property("pause", false).map_err(mpv_error)?,
            "pause" => player.mpv.set_property("pause", true).map_err(mpv_error)?,
            "seek" => player
                .mpv
                .command(
                    "seek",
                    &[&payload.value.max(0.0).to_string(), "absolute+exact"],
                )
                .map_err(mpv_error)?,
            "volume" => player
                .mpv
                .set_property("volume", payload.value.clamp(0.0, 1.0) * 100.0)
                .map_err(mpv_error)?,
            "fit" => player
                .mpv
                .set_property("panscan", 0.0_f64)
                .map_err(mpv_error)?,
            "crop" => player
                .mpv
                .set_property("panscan", 1.0_f64)
                .map_err(mpv_error)?,
            "stretch" => player
                .mpv
                .set_property("video-unscaled", "downscale-big".to_owned())
                .map_err(mpv_error)?,
            "zoom" => player
                .mpv
                .set_property("video-zoom", (payload.value.max(1.0)).log2())
                .map_err(mpv_error)?,
            "track" => select_track(player, payload.index, true)?,
            "deselectTrack" => select_track(player, payload.index, false)?,
            action => {
                return Err(Error::InvalidRequest(format!(
                    "unsupported native action: {action}"
                )))
            }
        }
        snapshot(player)
    })
}

pub fn layout(payload: NativeLayoutRequest) -> Result<()> {
    PLAYER.with(|slot| {
        let slot = slot.borrow();
        let player = slot
            .as_ref()
            .ok_or_else(|| Error::InvalidRequest("native player is not open".into()))?;
        ensure_session(&player.session_key, &payload.session_key)?;
        crate::desktop::linux_surface::place_widget(
            &player.widget,
            payload.x,
            payload.y,
            payload.width,
            payload.height,
        )?;
        let commits = player.layout_commits.get().saturating_add(1);
        player.layout_commits.set(commits);
        let now = Instant::now();
        let elapsed = now
            .duration_since(player.layout_sample_at.get())
            .as_secs_f64();
        if elapsed >= 0.5 && std::env::var_os("TAURI_VIDEO_TELEMETRY").is_some() {
            eprintln!(
                "mpv layout telemetry: commits={commits} rate={:.1} Hz",
                commits as f64 / elapsed
            );
            player.layout_commits.set(0);
            player.layout_sample_at.set(now);
        }
        schedule_layout_render(player);
        Ok(())
    })
}

pub(super) fn schedule_layout_render(player: &MpvPlayer) {
    if player.layout_redraw_pending.replace(true) {
        return;
    }
    let pending = Rc::clone(&player.layout_redraw_pending);
    player.gl_area.add_tick_callback(move |area, _| {
        pending.set(false);
        if let Some(parent) = area.parent() {
            parent.queue_draw();
        }
        area.queue_render();
        gtk::glib::ControlFlow::Break
    });
}

pub fn stats(payload: NativeSessionRequest) -> Result<NativePlaybackSnapshot> {
    PLAYER.with(|slot| {
        let mut slot = slot.borrow_mut();
        let player = slot
            .as_mut()
            .ok_or_else(|| Error::InvalidRequest("native player is not open".into()))?;
        ensure_session(&player.session_key, &payload.session_key)?;
        snapshot(player)
    })
}

pub fn close(_payload: NativeSessionRequest) -> Result<()> {
    // A close means the caller is finished with the native player. A key
    // mismatch would mean the adapter and the engine desynchronized;
    // leaving the engine running would leak playing audio, so park
    // regardless of the presented key.
    park_player()?;
    Ok(())
}

pub fn force_close() -> Result<()> {
    PLAYER.with(|slot| {
        slot.borrow_mut().take();
    });
    Ok(())
}

fn park_player() -> Result<()> {
    PLAYER.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(player) = slot.as_mut() else {
            return Ok(());
        };
        // Keep GtkGLArea and libmpv's render context alive between source
        // changes. Destroying and immediately recreating a native GL child
        // can invalidate WebKit's transparent composited layer on Wayland,
        // leaving a live DOM whose pixels are no longer presented.
        player.mpv.command("stop", &[]).map_err(mpv_error)?;
        player.widget.hide();
        player.session_key.clear();
        player.tracks.clear();
        player.track_targets.clear();
        player.tracks_dirty = true;
        *player.error.borrow_mut() = None;
        Ok(())
    })
}

pub(super) fn snapshot(player: &mut MpvPlayer) -> Result<NativePlaybackSnapshot> {
    drain_events(player)?;
    if let Some(error) = player.error.borrow().clone() {
        return Err(Error::Pipeline(error));
    }
    if player.tracks_dirty {
        refresh_tracks(player);
        player.tracks_dirty = false;
    }
    let position = property::<f64>(&player.mpv, "time-pos")
        .unwrap_or(0.0)
        .max(0.0);
    let duration = property::<f64>(&player.mpv, "duration").filter(|value| *value > 0.0);
    let live = duration.is_none();
    let duration = duration.unwrap_or(0.0);
    let buffered = property::<f64>(&player.mpv, "demuxer-cache-time")
        .unwrap_or(position)
        .max(position)
        .min(duration.max(position));
    let seekable = property::<bool>(&player.mpv, "seekable").unwrap_or(!live);
    // The engine's own seekable window: for unseekable media mpv reports
    // the demuxer cache range, the only region a seek can serve. The
    // previous full-duration fabrication unlocked seeks the origin could
    // not land, which replayed the beginning instead of failing.
    let seekable_start = property::<f64>(&player.mpv, "seekable-start")
        .map(|value| value.max(0.0))
        .unwrap_or(0.0);
    let seekable_end = property::<f64>(&player.mpv, "seekable-end")
        .filter(|value| *value > seekable_start)
        .unwrap_or(duration.max(buffered));
    let rendered = player.presented_frames.get();
    let dropped = property::<i64>(&player.mpv, "frame-drop-count")
        .unwrap_or(0)
        .max(0) as u64;
    let now = Instant::now();
    let elapsed = now.duration_since(player.last_sample_at).as_secs_f64();
    if elapsed >= 0.5 {
        player.measured_fps =
            rendered.saturating_sub(player.last_presented_frames) as f64 / elapsed;
        player.last_presented_frames = rendered;
        player.last_sample_at = now;
        if std::env::var_os("TAURI_VIDEO_TELEMETRY").is_some() {
            eprintln!(
                "mpv telemetry: presented={rendered} dropped={dropped} fps={:.2} position={position:.2}s",
                player.measured_fps,
            );
        }
    }
    let video_width = property::<i64>(&player.mpv, "video-params/w")
        .unwrap_or(0)
        .max(0) as u32;
    let video_height = property::<i64>(&player.mpv, "video-params/h")
        .unwrap_or(0)
        .max(0) as u32;
    let paused = property::<bool>(&player.mpv, "pause").unwrap_or(true);
    let decoder = property::<String>(&player.mpv, "video-codec").unwrap_or_default();
    let hwdec =
        property::<String>(&player.mpv, "hwdec-current").unwrap_or_else(|| "software".into());
    Ok(NativePlaybackSnapshot {
        duration_seconds: duration,
        current_time_seconds: position,
        buffered_seconds: buffered,
        live,
        seekable,
        seekable_start_seconds: seekable_start,
        seekable_end_seconds: seekable_end,
        playing: !paused,
        video_width,
        video_height,
        tracks: player.tracks.clone(),
        presented_frames: rendered,
        dropped_frames: dropped,
        measured_fps: player.measured_fps,
        hardware_backend: format!("mpv:{hwdec}:{decoder}:gtk-glarea"),
        backend: "mpv".into(),
        encoded_bytes_buffered: property::<i64>(&player.mpv, "cache-used")
            .unwrap_or(0)
            .max(0) as u64
            * 1024,
        average_frame_processing_us: 0.0,
    })
}

fn refresh_tracks(player: &mut MpvPlayer) {
    let count = property::<i64>(&player.mpv, "track-list/count")
        .unwrap_or(0)
        .max(0);
    let mut tracks = Vec::with_capacity(count as usize);
    let mut targets = Vec::with_capacity(count as usize);
    for source_index in 0..count {
        let prefix = format!("track-list/{source_index}");
        let track_type =
            property::<String>(&player.mpv, &format!("{prefix}/type")).unwrap_or_default();
        let kind = match track_type.as_str() {
            "video" => TrackKind::Video,
            "audio" => TrackKind::Audio,
            "sub" => TrackKind::Subtitle,
            _ => continue,
        };
        let mpv_id =
            property::<i64>(&player.mpv, &format!("{prefix}/id")).unwrap_or(source_index);
        let public_index = tracks.len() as i32;
        let language = property::<String>(&player.mpv, &format!("{prefix}/lang"))
            .unwrap_or_else(|| "und".into());
        let title =
            property::<String>(&player.mpv, &format!("{prefix}/title")).unwrap_or_default();
        let codec =
            property::<String>(&player.mpv, &format!("{prefix}/codec")).unwrap_or_default();
        let selected =
            property::<bool>(&player.mpv, &format!("{prefix}/selected")).unwrap_or(false);
        tracks.push(NativeTrackInfo {
            id: format!("mpv-{track_type}-{mpv_id}"),
            index: public_index,
            kind,
            language: language.clone(),
            label: if title.is_empty() {
                if language == "und" {
                    track_type.clone()
                } else {
                    language.to_uppercase()
                }
            } else {
                title
            },
            codec,
            selected,
        });
        targets.push(TrackTarget {
            public_index,
            mpv_id,
            kind,
        });
    }
    player.tracks = tracks;
    player.track_targets = targets;
}

fn select_track(player: &mut MpvPlayer, index: i32, enabled: bool) -> Result<()> {
    let target = player
        .track_targets
        .iter()
        .find(|target| target.public_index == index)
        .cloned()
        .ok_or_else(|| Error::InvalidRequest(format!("unknown native track index {index}")))?;
    let property = match target.kind {
        TrackKind::Video => "vid",
        TrackKind::Audio => "aid",
        TrackKind::Subtitle => "sid",
    };
    let result = if enabled {
        player
            .mpv
            .set_property(property, target.mpv_id)
            .map_err(mpv_error)
    } else {
        player
            .mpv
            .set_property(property, "no".to_owned())
            .map_err(mpv_error)
    };
    if result.is_ok() {
        player.tracks_dirty = true;
    }
    result
}

fn drain_events(player: &mut MpvPlayer) -> Result<()> {
    // Event production is independent of the UI poll rate. Never let a hot
    // event queue monopolize GTK's main thread and starve frame presentation.
    for _ in 0..64 {
        let Some(event) = player.mpv.wait_event(0.0) else {
            break;
        };
        match event.map_err(|error| {
            let error = mpv_error(error);
            eprintln!("mpv event error: {error}");
            error
        })? {
            Event::Shutdown => return Err(Error::Pipeline("mpv shut down".into())),
            Event::StartFile
            | Event::FileLoaded
            | Event::VideoReconfig
            | Event::AudioReconfig => player.tracks_dirty = true,
            Event::EndFile(_) => {}
            _ => {}
        }
    }
    Ok(())
}

fn ensure_session(active: &str, requested: &str) -> Result<()> {
    if active == requested {
        Ok(())
    } else {
        Err(Error::InvalidRequest(
            "native player session is stale".into(),
        ))
    }
}

pub(super) fn property<T: libmpv2::GetData>(mpv: &Mpv, name: &str) -> Option<T> {
    mpv.get_property(name).ok()
}

pub(super) fn encode_mpv_list(values: &[String]) -> String {
    values.iter().fold(String::new(), |mut encoded, value| {
        let separator = if encoded.is_empty() { "" } else { "," };
        let _ = write!(encoded, "{separator}%{}%{value}", value.len());
        encoded
    })
}

pub(super) fn mpv_error(error: libmpv2::Error) -> Error {
    Error::Pipeline(format!("mpv backend: {error}"))
}

pub(super) fn open_gl_proc_address(_: &(), name: &str) -> *mut c_void {
    let Ok(name) = CString::new(name) else {
        return std::ptr::null_mut();
    };
    let pointer = unsafe { glXGetProcAddressARB(name.as_ptr().cast()) };
    if pointer.is_null() {
        unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) }
    } else {
        pointer
    }
}
