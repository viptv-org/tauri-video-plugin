//! Opt-in GTK surface checks against private provider deliveries. Only aliases
//! and numeric playback facts are logged; credentials stay in the input file.
use super::{linux_gstreamer as gst, linux_mpv as mpv, source_proxy};
use crate::models::{
    NativeControlRequest, NativeOpenRequest, NativePlaybackSnapshot, NativeSessionRequest,
    TrackKind,
};
use gtk::gdk::prelude::*;
use gtk::prelude::*;
use std::time::{Duration, Instant};

fn pump() {
    let deadline = Instant::now() + Duration::from_millis(10);
    while gtk::events_pending() && Instant::now() < deadline {
        gtk::main_iteration_do(false);
    }
    std::thread::sleep(Duration::from_millis(20));
}
fn pixels(window: &gtk::Window) -> Vec<u8> {
    let image = window
        .window()
        .unwrap()
        .pixbuf(0, 0, 640, 480)
        .expect("native picture pixels");
    image.read_pixel_bytes().as_ref().to_vec()
}
fn settle_picture() {
    let end = Instant::now() + Duration::from_millis(220);
    while Instant::now() < end {
        pump();
    }
}
fn stats(engine: &str, key: &str) -> crate::Result<NativePlaybackSnapshot> {
    let request = NativeSessionRequest {
        session_key: key.into(),
    };
    if engine == "gstreamer" {
        gst::stats(request)
    } else {
        mpv::stats(request)
    }
}
fn control(
    engine: &str,
    key: &str,
    action: &str,
    value: f64,
    index: i32,
) -> crate::Result<NativePlaybackSnapshot> {
    let request = NativeControlRequest {
        session_key: key.into(),
        action: action.into(),
        value,
        index,
    };
    if engine == "gstreamer" {
        gst::control(request)
    } else {
        mpv::control(request)
    }
}
fn until(
    engine: &str,
    key: &str,
    label: &str,
    timeout: u64,
    done: impl Fn(&NativePlaybackSnapshot) -> bool,
) -> NativePlaybackSnapshot {
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        pump();
        let result = stats(engine, key);
        let s = match result {
            Ok(s) => s,
            Err(error) => panic!("{engine} {label}: {}", error),
        };
        if done(&s) {
            return s;
        }
        if engine == "mpv" && Instant::now() >= deadline {
            println!("MPV surface {:?}", mpv::qualification_surface());
        }
        assert!(
            Instant::now() < deadline,
            "{engine} {label}: timed out; time={} frames={} dimensions={}x{}",
            s.current_time_seconds,
            s.presented_frames,
            s.video_width,
            s.video_height
        );
    }
}
#[test]
#[ignore = "real GTK display and VIPTV_NATIVE_PROVIDER_CASES private JSON required"]
fn real_provider_native_surface_controls() {
    gtk::init().expect("GTK display");
    let input = std::env::var("VIPTV_NATIVE_PROVIDER_CASES").expect("private case file");
    let cases: serde_json::Value = serde_json::from_slice(&std::fs::read(input).unwrap()).unwrap();
    let cases = cases.as_array().unwrap();
    assert!(!cases.is_empty());
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_default_size(640, 480);
    window.set_title("VIPTV native control check");
    let fixed = gtk::Fixed::new();
    window.add(&fixed);
    window.show_all();
    window.present();
    super::linux_surface::install_qualification_host(fixed);
    for engine in ["mpv", "gstreamer"] {
        window.present();
        for (index, case) in cases.iter().enumerate() {
            let alias = case["alias"].as_str().unwrap();
            let mut payload: NativeOpenRequest =
                serde_json::from_value(case["payload"].clone()).unwrap();
            payload.session_key = format!("qualified-{engine}-{index}");
            payload.width = 640.0;
            payload.height = 480.0;
            payload.muted = true;
            payload.volume = 0.0;
            let key = payload.session_key.clone();
            if std::env::var_os("VIPTV_NATIVE_FAILURE_RECOVERY").is_some() && index == 0 {
                let mut missing = payload.clone();
                missing.session_key = format!("failed-{engine}");
                missing.uri = "file:///viptv-qualification-missing-source.mkv".into();
                missing.headers.clear();
                missing.cookies = None;
                missing.user_agent = None;
                let failed_key = missing.session_key.clone();
                let result = if engine == "gstreamer" {
                    gst::open_player(missing)
                } else {
                    mpv::open_player(missing)
                };
                let deadline = Instant::now() + Duration::from_secs(5);
                let error = match result {
                    Err(error) => error,
                    Ok(_) => loop {
                        pump();
                        if let Err(error) = stats(engine, &failed_key) {
                            break error;
                        }
                        assert!(
                            Instant::now() < deadline,
                            "{engine}: missing source never failed"
                        );
                    },
                };
                assert!(
                    !error.to_string().contains("decode"),
                    "{engine}: missing source mislabeled as decoder failure"
                );
                println!("{engine}: missing source reported {}", error.code());
                let request = NativeSessionRequest {
                    session_key: failed_key,
                };
                if engine == "gstreamer" {
                    gst::close(request).unwrap();
                } else {
                    mpv::close(request).unwrap();
                }
            }
            let (payload, pending) = source_proxy::route(payload);
            let opened = if engine == "gstreamer" {
                gst::open_player(payload)
            } else {
                mpv::open_player(payload)
            };
            source_proxy::settle(pending, opened.is_ok());
            assert!(opened.is_ok(), "{engine} {alias}: open failed");
            let s = until(engine, &key, "decode", 20, |s| {
                s.video_width > 0 && s.presented_frames > 2 && s.current_time_seconds > 0.2
            });
            println!("{engine} {alias}: decoded dimensions={}x{} frames={} time={:.2} duration={:.2} tracks={}",s.video_width,s.video_height,s.presented_frames,s.current_time_seconds,s.duration_seconds,s.tracks.len());
            if alias.contains("vod") || alias.contains("fixture") {
                let target = 30.0_f64.min(s.duration_seconds * 0.3).max(2.0);
                let started = Instant::now();
                assert!(
                    control(engine, &key, "seek", target, 0).is_ok(),
                    "seek command"
                );
                assert!(
                    started.elapsed() < Duration::from_millis(250),
                    "{engine}: seek blocked GTK"
                );
                let landed = until(engine, &key, "seek", 12, |s| {
                    (s.current_time_seconds - target).abs() < 1.5
                });
                println!(
                    "{engine} {alias}: seek target={target:.2} landed={:.2}",
                    landed.current_time_seconds
                );
            }
            let started = Instant::now();
            assert!(control(engine, &key, "pause", 0.0, 0).is_ok());
            assert!(
                started.elapsed() < Duration::from_millis(250),
                "pause blocked GTK"
            );
            let paused = until(engine, &key, "pause", 5, |s| !s.playing);
            let deadline = Instant::now() + Duration::from_millis(350);
            while Instant::now() < deadline {
                pump();
            }
            let stable = stats(engine, &key).expect("paused stats");
            assert!(
                (stable.current_time_seconds - paused.current_time_seconds).abs() < 0.8,
                "{engine}: pause clock advanced"
            );
            assert!(control(engine, &key, "play", 0.0, 0).is_ok());
            let resumed = until(engine, &key, "resume", 8, |s| {
                s.playing && s.current_time_seconds > stable.current_time_seconds + 0.2
            });
            println!(
                "{engine} {alias}: pause/resume passed time={:.2}",
                resumed.current_time_seconds
            );
            for (kind, label) in [
                (TrackKind::Audio, "audio"),
                (TrackKind::Subtitle, "subtitle"),
            ] {
                if let Some(track) = resumed
                    .tracks
                    .iter()
                    .find(|t| t.kind == kind && !t.selected)
                {
                    let before = if label == "subtitle" && alias.contains("caption-pixels") {
                        settle_picture();
                        Some(pixels(&window))
                    } else {
                        None
                    };
                    assert!(control(engine, &key, "track", 0.0, track.index).is_ok());
                    until(engine, &key, label, 8, |s| {
                        s.tracks
                            .iter()
                            .any(|t| t.index == track.index && t.selected)
                    });
                    if let Some(before) = before {
                        let deadline = Instant::now() + Duration::from_secs(3);
                        loop {
                            pump();
                            let s = stats(engine, &key).unwrap();
                            let after = pixels(&window);
                            let changed = before
                                .iter()
                                .zip(after.iter())
                                .filter(|(a, b)| a.abs_diff(**b) > 80)
                                .count();
                            let visible_caption = after
                                .chunks_exact(3)
                                .filter(|pixel| pixel[0] > 200 && pixel[1] > 200 && pixel[2] > 200)
                                .count();
                            if changed > 100 && visible_caption > 100 {
                                println!("{engine}: caption pixels changed={changed}");
                                break;
                            }
                            assert!(Instant::now()<deadline,"subtitle selection did not change rendered captions; time={} frames={}",s.current_time_seconds,s.presented_frames);
                        }
                    }
                    println!("{engine} {alias}: {label} selection confirmed");
                }
            }
            if let Some(text) = stats(engine, &key)
                .unwrap()
                .tracks
                .iter()
                .find(|t| t.kind == TrackKind::Subtitle && t.selected)
                .cloned()
            {
                assert!(control(engine, &key, "deselectTrack", 0.0, text.index).is_ok());
                until(engine, &key, "subtitle off", 5, |s| {
                    !s.tracks
                        .iter()
                        .any(|t| t.kind == TrackKind::Subtitle && t.selected)
                });
                if alias.contains("caption-pixels") {
                    let deadline = Instant::now() + Duration::from_secs(3);
                    loop {
                        pump();
                        let after = pixels(&window);
                        let white = after
                            .chunks_exact(3)
                            .filter(|pixel| pixel[0] > 200 && pixel[1] > 200 && pixel[2] > 200)
                            .count();
                        if white == 0 {
                            break;
                        }
                        assert!(
                            Instant::now() < deadline,
                            "{engine}: captions remained visible after disabling subtitles"
                        );
                    }
                }
                println!("{engine} {alias}: subtitles off confirmed");
            }
            assert!(control(engine, &key, "pause", 0.0, 0).is_ok());
            assert!(control(engine, &key, "crop", 0.0, 0).is_ok());
            pump();
            let crop = if engine == "gstreamer" {
                gst::qualification_picture()
            } else {
                (0, 0)
            };
            if engine == "mpv" {
                assert_eq!(mpv::qualification_panscan(), 1.0);
            }
            assert!(control(engine, &key, "fit", 0.0, 0).is_ok());
            pump();
            if engine == "gstreamer"
                && (f64::from(resumed.video_width) / f64::from(resumed.video_height) - 4.0 / 3.0)
                    .abs()
                    > 0.05
            {
                assert_ne!(
                    crop,
                    gst::qualification_picture(),
                    "Fit/Fill allocation did not change"
                );
            }
            if engine == "mpv" {
                assert_eq!(mpv::qualification_panscan(), 0.0);
            }
            // The synthetic audio is silent; real-provider checks keep output muted.
            let volume = if alias.contains("fixture") { 0.37 } else { 0.0 };
            assert!(control(engine, &key, "volume", volume, 0).is_ok());
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                pump();
                let actual = if engine == "gstreamer" {
                    gst::qualification_volume()
                } else {
                    mpv::qualification_volume()
                };
                if (actual - volume).abs() < 0.01 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "native volume differs from control"
                );
            }
            println!("{engine} {alias}: paused Fit/Fill and volume confirmed");
            let before = stats(engine, &key).unwrap().current_time_seconds;
            assert!(control(engine, &key, "play", 0.0, 0).is_ok());
            until(engine, &key, "post-controls progress", 5, |s| {
                s.current_time_seconds > before + 0.2
            });

            let request = NativeSessionRequest {
                session_key: key.clone(),
            };
            let stopped = if engine == "gstreamer" {
                gst::close(request)
            } else {
                mpv::close(request)
            };
            assert!(stopped.is_ok());
            source_proxy::release(&key);
            for _ in 0..5 {
                pump();
            }
        }
        if engine == "gstreamer" {
            gst::shutdown().unwrap();
        } else {
            mpv::force_close().unwrap();
        }
        for _ in 0..5 {
            pump();
        }
    }
    window.close();
}
