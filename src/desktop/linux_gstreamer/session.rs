use std::{sync::Arc, time::Instant};

use gst::prelude::ObjectExt as GstObjectExt;
use gst::prelude::*;
use gstreamer as gst;
use gtk::prelude::*;

use crate::{
    models::{
        NativeControlRequest, NativeLayoutRequest, NativePlaybackSnapshot, NativeSessionRequest,
        NativeTrackInfo, TrackKind,
    },
    Error, Result,
};

use super::{playback_timeline, NativePlayer, Ordering, PLAYER};

/// State changes can rendezvous with the GTK sink; never block GTK waiting
/// for those changes. Coalesce older requests and fence replacement sessions.
pub(super) fn schedule_state(player: &NativePlayer, state: gst::State) {
    if player.requested_state.swap(state as u8, Ordering::SeqCst) == state as u8 {
        return;
    }
    let desired = Arc::clone(&player.requested_state);
    let source = Arc::clone(&player.source);
    let key = player.session_key.clone();
    player.engine.submit(move |pipeline| {
        if desired.load(Ordering::SeqCst) != state as u8 || source.read().session_key != key {
            return;
        }
        if pipeline.set_state(state).is_err() {
            gst::element_error!(
                pipeline,
                gst::CoreError::Failed,
                ("Native playback state change failed")
            );
        }
    });
}

pub(super) fn schedule_aspect(player: &mut NativePlayer, force: bool) {
    if player.force_aspect == force {
        return;
    }
    player.force_aspect = force;
    let sink = player.gtk_sink.clone();
    let source = Arc::clone(&player.source);
    let key = player.session_key.clone();
    player.engine.submit(move |_| {
        if source.read().session_key == key {
            sink.set_property("force-aspect-ratio", force);
        }
    });
}

pub fn control(payload: NativeControlRequest) -> Result<NativePlaybackSnapshot> {
    PLAYER.with(|slot| {
        let mut slot = slot.borrow_mut();
        let player = slot
            .as_mut()
            .ok_or_else(|| Error::InvalidRequest("native player is not open".into()))?;
        ensure_session(&player.session_key, &payload.session_key)?;
        match payload.action.as_str() {
            "play" => {
                player.desired_playing = true;
                schedule_state(player, gst::State::Playing);
            }
            "pause" => {
                player.desired_playing = false;
                schedule_state(player, gst::State::Paused);
            }
            "seek" => {
                // GTK sinks dispatch work back to the UI thread during a
                // flushing seek. Waiting here deadlocks the UI against the sink.
                let position = gst::ClockTime::from_nseconds((payload.value.max(0.0) * 1e9) as u64);
                let source = Arc::clone(&player.source);
                let session_key = payload.session_key.clone();
                player.engine.submit(move |pipeline| {
                    // A network movie can still be preparing after three
                    // seconds. A timed-out state query returns Ok(Async), so
                    // testing only is_err() dispatched the seek in READY and
                    // turned a healthy source into a terminal pipeline error.
                    let deadline = Instant::now() + std::time::Duration::from_secs(15);
                    let ready = loop {
                        if source.read().session_key != session_key {
                            return;
                        }
                        let (transition, current, _) =
                            pipeline.state(Some(gst::ClockTime::from_mseconds(100)));
                        if transition.is_err() {
                            break false;
                        }
                        if matches!(current, gst::State::Paused | gst::State::Playing) {
                            break true;
                        }
                        if Instant::now() >= deadline {
                            break false;
                        }
                    };
                    if source.read().session_key != session_key {
                        return;
                    }
                    if !ready
                        || pipeline
                            .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE, position)
                            .is_err()
                    {
                        gst::element_error!(
                            pipeline,
                            gst::CoreError::Failed,
                            ("Native seek failed")
                        );
                    }
                });
            }
            "volume" => {
                player
                    .volume_filter
                    .set_property("volume", payload.value.clamp(0.0, 1.0));
            }
            "fit" => {
                schedule_aspect(player, true);
                player.picture.fill(false);
            }
            "crop" => {
                schedule_aspect(player, true);
                player.picture.fill(true);
            }
            "stretch" => {
                player.picture.fill(false);
                schedule_aspect(player, false);
            }
            "track" => select_stream(player, payload.index, true)?,
            "deselectTrack" => select_stream(player, payload.index, false)?,
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
        let mut slot = slot.borrow_mut();
        let player = slot
            .as_mut()
            .ok_or_else(|| Error::InvalidRequest("native player is not open".into()))?;
        ensure_session(&player.session_key, &payload.session_key)?;
        player.picture.layout(payload.width, payload.height);
        crate::desktop::linux_surface::place_widget(
            &player.widget,
            payload.x,
            payload.y,
            payload.width,
            payload.height,
        )
    })
}

pub fn stats(payload: NativeSessionRequest) -> Result<NativePlaybackSnapshot> {
    PLAYER.with(|slot| {
        let mut slot = slot.borrow_mut();
        let player = slot
            .as_mut()
            .ok_or_else(|| Error::InvalidRequest("native player is not open".into()))?;
        ensure_session(&player.session_key, &payload.session_key)?;
        snapshot(player).map_err(|error| {
            tracing::debug!(%error, "GStreamer stats snapshot failed");
            error
        })
    })
}

/// Parks the engine when the presented key owns it, or when no session owns
/// it. Returns whether the engine was released. Late cleanup from an older
/// controller (a stale key) must not park a newer player's session.
pub fn close(payload: NativeSessionRequest) -> Result<bool> {
    let releases = PLAYER.with(|slot| {
        slot.borrow().as_ref().is_none_or(|player| {
            crate::desktop::linux::close_releases(&player.session_key, &payload.session_key)
        })
    });
    if releases {
        park_player()?;
    }
    Ok(releases)
}

pub fn force_close() -> Result<()> {
    park_player()
}

pub fn shutdown() -> Result<()> {
    PLAYER.with(|slot| {
        let Some(player) = slot.borrow_mut().take() else {
            return Ok(());
        };
        player.volume_filter.set_property("volume", 0.0_f64);
        player.source.write().session_key.clear();
        player
            .requested_state
            .store(gst::State::Null as u8, Ordering::SeqCst);
        player.engine.submit(move |pipeline| {
            let _ = pipeline.set_state(gst::State::Null);
        });
        player.widget.hide();
        Ok(())
    })
}

fn park_player() -> Result<()> {
    PLAYER.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(player) = slot.as_mut() else {
            return Ok(());
        };
        player.volume_filter.set_property("volume", 0.0_f64);
        player.source.write().session_key.clear();
        player
            .requested_state
            .store(gst::State::Null as u8, Ordering::SeqCst);
        player.session_key.clear();
        schedule_state(player, gst::State::Ready);
        player.widget.hide();
        player.session_key.clear();
        player.tracks.clear();
        player.selected_streams.clear();
        player.pending_selection = None;
        player.error = None;
        Ok(())
    })
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

#[derive(Clone, Default)]
pub(super) struct EngineFacts {
    position: f64,
    duration: f64,
    live: bool,
    seekable: bool,
    seekable_start: f64,
    seekable_end: f64,
    rendered: u64,
    dropped: u64,
    width: u32,
    height: u32,
    display_width: f64,
    buffered_end: f64,
}

/// Queries may wait for streaming locks. Sample off GTK and copy the finished
/// facts atomically; never hold this small cache lock during engine work.
fn sample_engine(player: &NativePlayer) {
    if player.telemetry_pending.swap(true, Ordering::SeqCst) {
        return;
    }
    let cache = Arc::clone(&player.telemetry);
    let pending = Arc::clone(&player.telemetry_pending);
    let source = Arc::clone(&player.source);
    let key = player.session_key.clone();
    let sink = player.gtk_sink.clone();
    player.engine.submit(move |pipeline| {
        if source.read().session_key != key {
            pending.store(false, Ordering::SeqCst);
            return;
        }
        let previous = cache.lock().clone();
        let position = pipeline
            .query_position::<gst::ClockTime>()
            .map(|time| time.seconds_f64())
            .unwrap_or(previous.position);
        let duration = pipeline
            .query_duration::<gst::ClockTime>()
            .map(|time| time.seconds_f64())
            .filter(|value| *value > 0.0)
            .unwrap_or(previous.duration);
        let (live, seekable, seekable_start, seekable_end) = playback_timeline(pipeline, duration);
        let mut buffering = gst::query::Buffering::new(gst::Format::Time);
        let buffered_end = if pipeline.query(&mut buffering) {
            buffering
                .ranges()
                .map(|(_, end)| end.value())
                .chain(std::iter::once(buffering.range().1.value()))
                .filter(|value| *value >= 0)
                .max()
                .map(|value| value as f64 / 1e9)
                .unwrap_or(position)
        } else {
            position
        };
        // Decoded/encoded AV queues expose media-time lead even when the
        // progressive-download query cannot describe this source's ranges.
        let mut queued = 0u64;
        if let Ok(bin) = gst::glib::prelude::Cast::dynamic_cast::<gst::Bin>(pipeline.clone()) {
            for element in bin.iterate_recurse().into_iter().flatten() {
                if element.find_property("current-level-time").is_some() {
                    let av = element
                        .static_pad("src")
                        .and_then(|pad| pad.current_caps())
                        .and_then(|caps| {
                            caps.structure(0).map(|s| {
                                s.name().starts_with("audio/") || s.name().starts_with("video/")
                            })
                        })
                        .unwrap_or(false);
                    if av {
                        queued = queued.max(element.property::<u64>("current-level-time"));
                    }
                }
                if element.factory().is_some_and(|factory| {
                    gst::prelude::GstObjectExt::name(&factory) == "multiqueue"
                }) {
                    let levels = element.property::<gst::Structure>("stats");
                    if let Ok(queues) = levels.get::<gst::Array>("queues") {
                        for value in queues.as_slice() {
                            if let Ok(queue) = value.get::<gst::Structure>() {
                                if let Ok(id) = queue.get::<u32>("id") {
                                    let av = element
                                        .static_pad(&format!("src_{id}"))
                                        .and_then(|pad| pad.current_caps())
                                        .and_then(|caps| {
                                            caps.structure(0).map(|s| {
                                                s.name().starts_with("audio/")
                                                    || s.name().starts_with("video/")
                                            })
                                        })
                                        .unwrap_or(false);
                                    if av {
                                        let caps = element
                                            .static_pad(&format!("src_{id}"))
                                            .and_then(|pad| pad.current_caps());
                                        let encoded = caps
                                            .as_ref()
                                            .and_then(|caps| caps.structure(0))
                                            .is_some_and(|s| {
                                                s.name() != "video/x-raw"
                                                    && s.name() != "audio/x-raw"
                                            });
                                        // Grow only compressed AV queues, never decoded frames.
                                        // This gives the direct engine a useful, bounded cache.
                                        if encoded
                                            && element.property::<u64>("max-size-time")
                                                < 30_000_000_000
                                        {
                                            element
                                                .set_property("max-size-time", 30_000_000_000u64);
                                            element.set_property(
                                                "max-size-bytes",
                                                32 * 1024 * 1024u32,
                                            );
                                            element.set_property("max-size-buffers", 0u32);
                                        }
                                        queued = queued.max(queue.get::<u64>("time").unwrap_or(0));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        let buffered_end = buffered_end.max(position + queued as f64 / 1e9);
        let stats = sink.property::<gst::Structure>("stats");
        let rendered = stats.get::<u64>("rendered").unwrap_or(0);
        let dropped = stats.get::<u64>("dropped").unwrap_or(0);
        let (width, height, display_width) = sink
            .static_pad("sink")
            .and_then(|pad| pad.current_caps())
            .and_then(|caps| {
                caps.structure(0).map(|structure| {
                    let width = structure.get::<i32>("width").unwrap_or(0).max(0) as u32;
                    let height = structure.get::<i32>("height").unwrap_or(0).max(0) as u32;
                    let aspect = structure
                        .get::<gst::Fraction>("pixel-aspect-ratio")
                        .map(|ratio| f64::from(ratio.numer()) / f64::from(ratio.denom()))
                        .unwrap_or(1.0);
                    (width, height, f64::from(width) * aspect)
                })
            })
            .unwrap_or((0, 0, 0.0));
        if source.read().session_key == key {
            *cache.lock() = EngineFacts {
                position,
                duration,
                live,
                seekable,
                seekable_start,
                seekable_end,
                rendered,
                dropped,
                width,
                height,
                display_width,
                buffered_end,
            };
        }
        pending.store(false, Ordering::SeqCst);
    });
}

pub(super) fn snapshot(player: &mut NativePlayer) -> Result<NativePlaybackSnapshot> {
    if !player.engine.alive.load(Ordering::Acquire) {
        return Err(Error::RuntimeUnavailable(
            "native engine worker stopped".into(),
        ));
    }
    drain_bus(player)?;
    sample_engine(player);
    let facts = player.telemetry.lock().clone();
    let position = facts.position;
    let duration = facts.duration;
    let (live, seekable, seekable_start, seekable_end) = (
        facts.live,
        facts.seekable,
        facts.seekable_start,
        facts.seekable_end,
    );
    let buffered = facts.buffered_end.max(position);
    let buffered = if duration > 0.0 {
        buffered.min(duration)
    } else {
        buffered
    };
    let seekable_end = if seekable_end > seekable_start {
        seekable_end
    } else {
        duration.max(buffered)
    };
    let rendered = facts.rendered;
    let dropped = facts.dropped;
    let now = Instant::now();
    let elapsed = now.duration_since(player.last_sample_at).as_secs_f64();
    if elapsed >= 0.5 {
        player.measured_fps = rendered.saturating_sub(player.last_rendered) as f64 / elapsed;
        player.last_rendered = rendered;
        player.last_sample_at = now;
        tracing::trace!(
            presented = rendered,
            dropped,
            fps = player.measured_fps,
            position_seconds = position,
            "GStreamer playback telemetry"
        );
    }
    let (video_width, video_height, display_width) =
        (facts.width, facts.height, facts.display_width);
    player
        .picture
        .source_size(display_width, f64::from(video_height));
    if video_width > 0 && rendered > 0 {
        player.widget.show_all();
    }
    let playing = player.desired_playing;
    Ok(NativePlaybackSnapshot {
        duration_seconds: duration,
        current_time_seconds: position,
        buffered_seconds: buffered,
        live,
        seekable,
        seekable_start_seconds: seekable_start,
        seekable_end_seconds: seekable_end,
        playing,
        video_width,
        video_height,
        tracks: player.tracks.clone(),
        presented_frames: rendered,
        dropped_frames: dropped,
        measured_fps: player.measured_fps,
        hardware_backend: "gstreamer-va-gl-gtk".into(),
        backend: "gstreamer".into(),
        source_proxied: false,
        encoded_bytes_buffered: player.target_buffer_bytes.map_or(0, |target| {
            target.saturating_mul(player.buffering_percent.max(0) as u64) / 100
        }),
        average_frame_processing_us: 0.0,
    })
}

fn drain_bus(player: &mut NativePlayer) -> Result<()> {
    let Some(bus) = player.pipeline.bus() else {
        return Ok(());
    };
    while let Some(message) = bus.pop() {
        match message.view() {
            gst::MessageView::Buffering(buffering) => {
                player.buffering_percent = buffering.percent();
                let target = if player.desired_playing && player.buffering_percent >= 100 {
                    gst::State::Playing
                } else {
                    gst::State::Paused
                };
                schedule_state(player, target);
            }
            gst::MessageView::StreamCollection(message) => {
                let collection = message.stream_collection();
                player.tracks.clear();
                for index in 0..collection.size() {
                    let Some(stream) = collection.stream(index) else {
                        continue;
                    };
                    let stream_type = stream.stream_type();
                    let kind = if stream_type.contains(gst::StreamType::VIDEO) {
                        TrackKind::Video
                    } else if stream_type.contains(gst::StreamType::AUDIO) {
                        TrackKind::Audio
                    } else if stream_type.contains(gst::StreamType::TEXT) {
                        TrackKind::Subtitle
                    } else {
                        continue;
                    };
                    let id = stream
                        .stream_id()
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| format!("native-{index}"));
                    let tags = stream.tags();
                    let language = tags
                        .as_ref()
                        .and_then(|tags| tags.get::<gst::tags::LanguageCode>())
                        .map(|tag| tag.get().to_string())
                        .unwrap_or_default();
                    let language = if language.eq_ignore_ascii_case("und") {
                        String::new()
                    } else {
                        language
                    };
                    let label = tags
                        .as_ref()
                        .and_then(|tags| tags.get::<gst::tags::Title>())
                        .map(|tag| tag.get().to_string())
                        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("und"))
                        .unwrap_or_else(|| {
                            if language.is_empty() {
                                format!(
                                    "{} {}",
                                    match kind {
                                        TrackKind::Audio => "Audio",
                                        TrackKind::Video => "Video",
                                        TrackKind::Subtitle => "Subtitle",
                                    },
                                    player
                                        .tracks
                                        .iter()
                                        .filter(|track| track.kind == kind)
                                        .count()
                                        + 1
                                )
                            } else {
                                language.to_uppercase()
                            }
                        });
                    let codec = stream
                        .caps()
                        .and_then(|caps| caps.structure(0).map(|value| value.name().to_string()))
                        .unwrap_or_default();
                    player.tracks.push(NativeTrackInfo {
                        id: id.clone(),
                        index: index as i32,
                        kind,
                        language,
                        label,
                        codec,
                        selected: player.selected_streams.contains(&id),
                    });
                }
                tracing::debug!(count = player.tracks.len(), "GStreamer streams discovered");
            }
            gst::MessageView::StreamsSelected(message) => {
                player.selected_streams = message
                    .streams()
                    .filter_map(|stream| stream.stream_id().map(|id| id.to_string()))
                    .collect();
                for track in &mut player.tracks {
                    track.selected = player.selected_streams.contains(&track.id);
                }
                if player.pending_selection.as_ref() == Some(&player.selected_streams) {
                    player.pending_selection = None;
                    // A new subtitle branch can retain an unseeked segment while
                    // video uses the previous seek's running-time offset. Reset
                    // the complete seekable timeline only after selection is
                    // confirmed, so the newly activated branch receives it too.
                    let source = Arc::clone(&player.source);
                    let key = player.session_key.clone();
                    player.engine.submit(move |pipeline| {
                        if source.read().session_key != key {
                            return;
                        }
                        let mut seeking = gst::query::Seeking::new(gst::Format::Time);
                        if pipeline.query(&mut seeking) && seeking.result().0 {
                            if let Some(position) = pipeline.query_position::<gst::ClockTime>() {
                                let _ = pipeline.seek_simple(
                                    gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                                    position,
                                );
                            }
                        }
                    });
                }
                tracing::debug!(
                    count = player.selected_streams.len(),
                    "GStreamer streams selected"
                );
            }
            gst::MessageView::Error(error) => {
                let failure = crate::error::NativeMediaFailure::from_bus(error);
                player.error = Some(failure);
                return Err(failure.into_error());
            }
            _ => {}
        }
    }
    if let Some(error) = player.error {
        return Err(error.into_error());
    }
    Ok(())
}

fn select_stream(player: &mut NativePlayer, index: i32, enabled: bool) -> Result<()> {
    let (kind, id) = player
        .tracks
        .iter()
        .find(|track| track.index == index)
        .map(|track| (track.kind, track.id.clone()))
        .ok_or_else(|| Error::InvalidRequest(format!("unknown native track index {index}")))?;
    let mut selected = player
        .pending_selection
        .clone()
        .unwrap_or_else(|| player.selected_streams.clone());
    selected.retain(|id| {
        player
            .tracks
            .iter()
            .find(|item| &item.id == id)
            .is_some_and(|item| item.kind != kind)
    });
    if enabled {
        selected.insert(id);
    }
    let ids: Vec<String> = selected.iter().cloned().collect();
    player.pending_selection = Some(selected);
    let source = Arc::clone(&player.source);
    let key = player.session_key.clone();
    player.engine.submit(move |pipeline| {
        if source.read().session_key != key {
            return;
        }
        if !pipeline.send_event(gst::event::SelectStreams::new(
            ids.iter().map(String::as_str),
        )) {
            gst::element_error!(
                pipeline,
                gst::CoreError::Failed,
                ("Native track selection failed")
            );
        }
    });
    Ok(())
}
