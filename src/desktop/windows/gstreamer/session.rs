use std::{
    sync::atomic::Ordering,
    time::Instant,
};

use gst::prelude::*;
use gstreamer as gst;

use crate::{
    models::{
        NativeControlRequest, NativeLayoutRequest, NativePlaybackSnapshot, NativeSessionRequest,
        NativeTrackInfo, TrackKind,
    },
    Error, Result,
};

use super::{active_video_decoder, playback_timeline, NativePlayer, PLAYER};

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
                player
                    .pipeline
                    .set_state(gst::State::Playing)
                    .map_err(|error| Error::Pipeline(error.to_string()))?;
            }
            "pause" => {
                player.desired_playing = false;
                player
                    .pipeline
                    .set_state(gst::State::Paused)
                    .map_err(|error| Error::Pipeline(error.to_string()))?;
            }
            "seek" => {
                // A resume seek can arrive immediately after open, while
                // playbin3 is still mid-async-transition (not prerolled);
                // seeking a transitioning pipeline stalls it. Wait bounded
                // for the pending state change to settle before flushing.
                let (transition, _current, _pending) =
                    player.pipeline.state(Some(gst::ClockTime::from_seconds(3)));
                transition.map_err(|error| {
                    Error::Pipeline(format!(
                        "native pipeline state did not settle before seek: {error}"
                    ))
                })?;
                player
                    .pipeline
                    .seek_simple(
                        gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                        gst::ClockTime::from_nseconds((payload.value.max(0.0) * 1e9) as u64),
                    )
                    .map_err(|error| Error::Pipeline(error.to_string()))?;
            }
            "volume" => player
                .pipeline
                .set_property("volume", payload.value.clamp(0.0, 1.0)),
            "fit" | "crop" | "stretch" | "zoom" => {}
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
        let mut source = player.source.write();
        source.x = payload.x;
        source.y = payload.y;
        source.width = payload.width;
        source.height = payload.height;
        Ok(())
    })
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

pub fn close(payload: NativeSessionRequest) -> Result<()> {
    let owns_player = PLAYER.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|player| player.session_key == payload.session_key)
    });
    if owns_player {
        park_player()?;
    }
    Ok(())
}

fn park_player() -> Result<()> {
    PLAYER.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(player) = slot.as_mut() else {
            return Ok(());
        };
        player
            .pipeline
            .set_state(gst::State::Ready)
            .map_err(|error| Error::Pipeline(error.to_string()))?;
        player.session_key.clear();
        player.tracks.clear();
        player.selected_streams.clear();
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

pub(super) fn snapshot(player: &mut NativePlayer) -> Result<NativePlaybackSnapshot> {
    drain_bus(player)?;
    let position = player
        .pipeline
        .query_position::<gst::ClockTime>()
        .map(|time| time.seconds_f64())
        .unwrap_or(0.0);
    let duration = player
        .pipeline
        .query_duration::<gst::ClockTime>()
        .map(|time| time.seconds_f64())
        .unwrap_or(0.0);
    let (live, seekable, seekable_start, seekable_end) =
        playback_timeline(&player.pipeline, duration);
    let buffered = position
        + player.buffer_duration_seconds.unwrap_or(0.0) * player.buffering_percent as f64
            / 100.0;
    let buffered = if live {
        buffered.max(position)
    } else {
        buffered.min(duration.max(position))
    };
    let seekable_end = if seekable_end > seekable_start {
        seekable_end
    } else {
        duration.max(buffered)
    };
    let structure = player.video_sink.property::<gst::Structure>("stats");
    let rendered = player.presented_frames.load(Ordering::Relaxed);
    let dropped = structure.get::<u64>("dropped").unwrap_or(0);
    let now = Instant::now();
    let elapsed = now.duration_since(player.last_sample_at).as_secs_f64();
    if elapsed >= 0.5 {
        player.measured_fps = rendered.saturating_sub(player.last_rendered) as f64 / elapsed;
        player.last_rendered = rendered;
        player.last_sample_at = now;
    }
    let (video_width, video_height) = player
        .video_sink
        .static_pad("sink")
        .and_then(|pad| pad.current_caps())
        .and_then(|caps| {
            caps.structure(0).map(|structure| {
                (
                    structure.get::<i32>("width").unwrap_or(0).max(0) as u32,
                    structure.get::<i32>("height").unwrap_or(0).max(0) as u32,
                )
            })
        })
        .unwrap_or((0, 0));
    Ok(NativePlaybackSnapshot {
        duration_seconds: duration,
        current_time_seconds: position,
        buffered_seconds: buffered,
        live,
        seekable,
        seekable_start_seconds: seekable_start,
        seekable_end_seconds: seekable_end,
        playing: player.desired_playing,
        video_width,
        video_height,
        tracks: player.tracks.clone(),
        presented_frames: rendered,
        dropped_frames: dropped,
        measured_fps: player.measured_fps,
        hardware_backend: format!(
            "gstreamer:{}:d3d11-webview2-texture-stream",
            active_video_decoder(&player.pipeline)
        ),
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
                player
                    .pipeline
                    .set_state(target)
                    .map_err(|error| Error::Pipeline(error.to_string()))?;
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
                    let label = tags
                        .as_ref()
                        .and_then(|tags| tags.get::<gst::tags::Title>())
                        .map(|tag| tag.get().to_string())
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| {
                            if language.is_empty() {
                                format!("Track {}", index + 1)
                            } else {
                                language.to_uppercase()
                            }
                        });
                    let codec = stream
                        .caps()
                        .and_then(|caps| {
                            caps.structure(0).map(|value| value.name().to_string())
                        })
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
            }
            gst::MessageView::StreamsSelected(message) => {
                player.selected_streams = message
                    .streams()
                    .filter_map(|stream| stream.stream_id().map(|id| id.to_string()))
                    .collect();
                for track in &mut player.tracks {
                    track.selected = player.selected_streams.contains(&track.id);
                }
            }
            gst::MessageView::Error(error) => {
                let message =
                    format!("{}: {}", error.error(), error.debug().unwrap_or_default());
                player.error = Some(message.clone());
                return Err(Error::Pipeline(message));
            }
            _ => {}
        }
    }
    if let Some(error) = player.error.clone() {
        return Err(Error::Pipeline(error));
    }
    Ok(())
}

fn select_stream(player: &mut NativePlayer, index: i32, enabled: bool) -> Result<()> {
    let track = player
        .tracks
        .iter()
        .find(|track| track.index == index)
        .cloned()
        .ok_or_else(|| Error::InvalidRequest(format!("unknown native track index {index}")))?;
    player.selected_streams.retain(|id| {
        player
            .tracks
            .iter()
            .find(|item| &item.id == id)
            .is_some_and(|item| item.kind != track.kind)
    });
    if enabled {
        player.selected_streams.insert(track.id);
    }
    let ids: Vec<&str> = player.selected_streams.iter().map(String::as_str).collect();
    if !player
        .pipeline
        .send_event(gst::event::SelectStreams::new(ids))
    {
        return Err(Error::Pipeline("decoder rejected track selection".into()));
    }
    for item in &mut player.tracks {
        item.selected = player.selected_streams.contains(&item.id);
    }
    Ok(())
}
