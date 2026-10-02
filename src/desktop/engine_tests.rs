use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[cfg(feature = "gstreamer-runtime")]
use gstreamer as gst;

#[cfg(feature = "gstreamer-runtime")]
use gstreamer::prelude::*;

/// A single-file HTTP server on an ephemeral port. libmpv and playbin3
/// open sequential connections and may request ranges; both are honoured
/// so the engines exercise real seeking I/O.
#[cfg(feature = "mpv-runtime")]
fn serve_fixture(file: Vec<u8>) -> std::io::Result<u16> {
    serve_fixture_with_headers(file, &[])
}

fn serve_fixture_with_headers(
    file: Vec<u8>,
    required: &'static [(&'static str, &'static str)],
) -> std::io::Result<u16> {
    serve_fixture_with_header_policy(file, required, &[])
}

fn serve_fixture_with_header_policy(
    file: Vec<u8>,
    required: &'static [(&'static str, &'static str)],
    forbidden: &'static [&'static str],
) -> std::io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let file = file.clone();
            let mut stream = stream;
            let mut request = [0_u8; 4096];
            let Ok(read) = stream.read(&mut request) else {
                continue;
            };
            let request = String::from_utf8_lossy(&request[..read]).to_string();
            if !required.iter().all(|(name, value)| {
                request.lines().any(|line| {
                    line.split_once(':').is_some_and(|(key, actual)| {
                        key.eq_ignore_ascii_case(name) && actual.trim() == *value
                    })
                })
            }) || forbidden.iter().any(|name| {
                request.lines().any(|line| {
                    line.split_once(':')
                        .is_some_and(|(key, _)| key.eq_ignore_ascii_case(name))
                })
            }) {
                let _ = stream.write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                continue;
            }
            let range = request
                .lines()
                .find(|line| line.to_ascii_lowercase().starts_with("range:"))
                .and_then(|line| line.split_once(':'))
                .map(|(_, value)| value.trim().trim_start_matches("bytes=").to_owned());
            let (status, start, end) = match range.as_deref().and_then(|range| {
                let mut parts = range.splitn(2, '-');
                let start: usize = parts.next().unwrap_or("0").parse().ok()?;
                let end: usize = parts
                    .next()
                    .filter(|part| !part.is_empty())
                    .and_then(|part| part.parse().ok())
                    .unwrap_or(file.len().saturating_sub(1));
                Some((start, end.min(file.len().saturating_sub(1))))
            }) {
                Some((start, end)) => ("206 Partial Content", start, end),
                None => ("200 OK", 0, file.len().saturating_sub(1)),
            };
            let length = end - start + 1;
            let content_range = if status.starts_with("206") {
                format!("Content-Range: bytes {start}-{end}/{}\r\n", file.len())
            } else {
                String::new()
            };
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Type: video/mp4\r\nAccept-Ranges: bytes\r\n{content_range}Content-Length: {length}\r\nConnection: close\r\n\r\n"
            );
            if stream.write_all(header.as_bytes()).is_err() {
                continue;
            }
            let _ = stream.write_all(&file[start..end + 1]);
        }
    });
    Ok(port)
}

/// A 6-second 320x240 H.264/AAC mp4, generated once per test binary.
fn mp4_fixture() -> &'static Vec<u8> {
    static FIXTURE: OnceLock<Vec<u8>> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let path = std::env::temp_dir().join(format!(
            "viptv-plugin-engine-test-{}.mp4",
            std::process::id()
        ));
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=6:size=320x240:rate=24",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=6",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&path)
            .status()
            .expect("ffmpeg generates the engine test fixture");
        assert!(status.success(), "ffmpeg fixture generation failed");
        std::fs::read(&path).expect("engine test fixture is readable")
    })
}

#[cfg(feature = "mpv-runtime")]
#[test]
fn mpv_engine_opens_stats_and_seeks_an_http_mp4() {
    let port = serve_fixture(mp4_fixture().clone()).expect("local HTTP fixture starts");
    let uri = format!("http://127.0.0.1:{port}/fixture.mp4");
    let mpv = super::linux_mpv::create_engine().expect("mpv engine handle");
    // No GL surface exists in cargo tests; the null output still runs the
    // demuxer and decoders, which is the failure domain under test. Audio is
    // likewise nulled: headless CI probes PipeWire/ALSA with no daemon and
    // that path is not what this test exercises.
    mpv.set_property("vo", "null".to_owned())
        .expect("null video output");
    mpv.set_property("ao", "null".to_owned())
        .expect("null audio output");
    mpv.set_property("pause", false).expect("autoplay");
    mpv.command("loadfile", &[&uri, "replace"])
        .expect("loadfile");
    let position = || mpv.get_property::<f64>("time-pos").unwrap_or(0.0);
    let duration = || mpv.get_property::<f64>("duration").unwrap_or(0.0);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if position() > 0.4 && duration() > 5.0 {
            break;
        }
        assert!(Instant::now() < deadline, "mpv engine never progressed");
        std::thread::sleep(Duration::from_millis(100));
    }
    mpv.command("seek", &["3.0", "absolute+exact"])
        .expect("engine seek");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if (position() - 3.0).abs() < 0.75 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "mpv engine never landed the seek"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    mpv.command("stop", &[]).expect("engine stop");
}

#[cfg(feature = "mpv-runtime")]
#[test]
fn mpv_engine_preserves_required_authorization_and_escaped_header_values() {
    let port = serve_fixture_with_headers(
        mp4_fixture().clone(),
        &[
            ("Authorization", "Bearer fixture-only\\"),
            ("Referer", "https://fixture.invalid/watch"),
            ("User-Agent", "Native Fixture"),
            ("Cookie", "session=fixture-only,second=two"),
            ("X-Fixture", "one\\two,three"),
        ],
    )
    .unwrap();
    let mpv = super::linux_mpv::create_engine().unwrap();
    mpv.set_property("vo", "null").unwrap();
    mpv.set_property("ao", "null").unwrap();
    let mut payload: crate::models::NativeOpenRequest = serde_json::from_value(serde_json::json!({
        "uri":format!("http://127.0.0.1:{port}/fixture.mp4"), "x":0,"y":0,"width":100,"height":100,
        "headers":{"Authorization":"Bearer fixture-only\\","Referer":"https://fixture.invalid/watch","X-Fixture":"one\\two,three"},
        "cookies":"session=fixture-only,second=two", "userAgent":"Native Fixture"
    })).unwrap();
    let defaults = super::linux_mpv::MpvBufferDefaults::read(&mpv);
    super::linux_mpv::open_engine_source(&mpv, &payload, defaults).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while mpv.get_property::<f64>("time-pos").unwrap_or(0.0) < 0.25 {
        assert!(
            Instant::now() < deadline,
            "authorized MPV media never decoded"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let replacement_port = serve_fixture_with_header_policy(
        mp4_fixture().clone(),
        &[
            ("Cookie", "replacement-only"),
            ("User-Agent", "Native Fixture"),
        ],
        &["Authorization", "Referer", "X-Fixture"],
    )
    .unwrap();
    payload.uri = format!("http://127.0.0.1:{replacement_port}/replacement.mp4");
    payload.headers.clear();
    payload.cookies = Some("replacement-only".into());
    payload.start_at_seconds = 3.0;
    super::linux_mpv::open_engine_source(&mpv, &payload, defaults).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while mpv.get_property::<f64>("time-pos").unwrap_or(0.0) < 3.1 {
        assert!(
            Instant::now() < deadline,
            "replacement MPV media never decoded"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    mpv.command("stop", &[]).unwrap();
}

#[cfg(feature = "mpv-runtime")]
#[test]
fn mpv_engine_opens_at_the_requested_start_position() {
    let port = serve_fixture(mp4_fixture().clone()).expect("local HTTP fixture starts");
    let uri = format!("http://127.0.0.1:{port}/fixture.mp4");
    let mpv = super::linux_mpv::create_engine().expect("mpv engine handle");
    mpv.set_property("vo", "null".to_owned())
        .expect("null video output");
    mpv.set_property("pause", false).expect("autoplay");
    // The open payload's start position must take effect as the file
    // loads, not through a post-open seek: that seek is visible as the
    // opening of the wrong position before the jump lands.
    mpv.set_property("start", "+3.0".to_owned())
        .expect("start position");
    mpv.command("loadfile", &[&uri, "replace"])
        .expect("loadfile");
    let position = || mpv.get_property::<f64>("time-pos").unwrap_or(0.0);
    let duration = || mpv.get_property::<f64>("duration").unwrap_or(0.0);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if duration() > 5.0 && position() > 0.0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "mpv engine never progressed from the requested start"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    // The first progress report is already past the requested start;
    // playback never showed the beginning of the file.
    assert!(
        position() >= 2.0,
        "engine opened at the beginning instead of the requested start: {}",
        position()
    );
    mpv.command("stop", &[]).expect("engine stop");
}

#[cfg(feature = "gstreamer-runtime")]
#[test]
fn gstreamer_http_authorization_failure_has_a_safe_typed_cause() {
    let port = serve_fixture_with_headers(
        mp4_fixture().clone(),
        &[("Authorization", "Bearer required-fixture")],
    )
    .unwrap();
    gst::init().unwrap();
    let pipeline = gst::ElementFactory::make("playbin3").build().unwrap();
    pipeline.set_property(
        "uri",
        format!("http://127.0.0.1:{port}/fixture.mp4?private-token"),
    );
    for property in ["video-sink", "audio-sink"] {
        pipeline.set_property(
            property,
            gst::ElementFactory::make("fakesink").build().unwrap(),
        );
    }
    let _ = pipeline.set_state(gst::State::Playing);
    let message = pipeline
        .bus()
        .unwrap()
        .timed_pop_filtered(gst::ClockTime::from_seconds(5), &[gst::MessageType::Error])
        .expect("HTTP authorization failure reaches bus");
    let gst::MessageView::Error(error) = message.view() else {
        panic!("Expected media error")
    };
    let failure = crate::error::NativeMediaFailure::from_gstreamer(&error.error()).into_error();
    pipeline.set_state(gst::State::Null).unwrap();
    assert_eq!(failure.code(), "AUTHORIZATION_FAILED");
    assert!(!serde_json::to_string(&failure)
        .unwrap()
        .contains("private-token"));
}

#[cfg(feature = "gstreamer-runtime")]
#[test]
fn gstreamer_engine_opens_stats_and_seeks_an_http_mp4() {
    let port = serve_fixture_with_headers(
        mp4_fixture().clone(),
        &[
            ("Authorization", "Bearer fixture-only"),
            ("Referer", "https://fixture.invalid/watch"),
            ("User-Agent", "Native Fixture"),
            ("Cookie", "session=fixture-only"),
        ],
    )
    .expect("local HTTP fixture starts");
    let uri = format!("http://127.0.0.1:{port}/fixture.mp4");
    gst::init().expect("GStreamer initializes");
    let pipeline = gst::ElementFactory::make("playbin3")
        .build()
        .expect("playbin3 pipeline");
    pipeline.set_property("uri", uri);
    let authorization: crate::models::NativeOpenRequest = serde_json::from_value(serde_json::json!({"uri":"http://fixture.invalid/movie","x":0,"y":0,"width":100,"height":100,"headers":{"Authorization":"Bearer fixture-only","Referer":"https://fixture.invalid/watch"},"cookies":"session=fixture-only","userAgent":"Native Fixture"})).unwrap();
    authorization.validate_authorization().unwrap();
    pipeline.connect("source-setup", false, move |values| {
        if let Ok(element) = values[1].get::<gst::Element>() {
            super::linux_gstreamer::configure_source(&element, &authorization);
        }
        None
    });
    // No app surface exists in cargo tests; unembedded sinks keep the
    // decode chain running exactly as the embedded pipeline does.
    for (property, name) in [("video-sink", "fakesink"), ("audio-sink", "fakesink")] {
        let sink = gst::ElementFactory::make(name).build().expect("test sink");
        // Production sinks render on the clock. An unsynced fakesink
        // drains the 6-second fixture faster than the position poll can
        // observe the seek, so the sinks sync exactly like the embedded
        // pipeline does.
        sink.set_property("sync", true);
        pipeline.set_property(property, &sink);
    }
    pipeline
        .set_state(gst::State::Playing)
        .expect("pipeline starts");
    let position = || {
        pipeline
            .query_position::<gst::ClockTime>()
            .map(|value| value.seconds() as f64)
            .unwrap_or(0.0)
    };
    let duration = || {
        pipeline
            .query_duration::<gst::ClockTime>()
            .map(|value| value.seconds() as f64)
            .unwrap_or(0.0)
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if position() > 0.4 && duration() > 5.0 {
            break;
        }
        assert!(Instant::now() < deadline, "playbin3 never progressed");
        std::thread::sleep(Duration::from_millis(100));
    }
    pipeline
        .seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::ClockTime::from_seconds(3),
        )
        .expect("engine seek");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if (position() - 3.0).abs() < 0.75 {
            break;
        }
        assert!(Instant::now() < deadline, "playbin3 never landed the seek");
        std::thread::sleep(Duration::from_millis(100));
    }
    pipeline
        .set_state(gst::State::Null)
        .expect("pipeline stops");
}
