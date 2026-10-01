//! Real-engine playback of disguised HLS through the sanitizing proxy.
//!
//! Each fixture is a real 6-second H.264/AAC HLS stream whose segments are
//! renamed (`.png`, `.jpg`, `.gif`, `.css`, no extension), served with a
//! lying `Content-Type`, and prefixed with a real image header or stylesheet
//! text. Unproxied, GStreamer's hlsdemux2 fails on image prefixes and mpv on
//! JPEG/GIF prefixes. The fixture origin also requires the source's cookie,
//! referrer and user agent, so a pass proves the proxy applied them upstream.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use super::source_proxy::Proxy;
use crate::models::NativeOpenRequest;

const COOKIE: &str = "session=fixture-only";
const REFERER: &str = "https://fixture.invalid/watch";
const USER_AGENT: &str = "Disguise Fixture";

#[derive(Clone, Copy, Debug)]
struct Disguise {
    name: &'static str,
    extension: &'static str,
    content_type: &'static str,
    prefix: Prefix,
    fmp4: bool,
    ranges: bool,
}

#[derive(Clone, Copy, Debug)]
enum Prefix {
    Png,
    Jpeg,
    Gif,
    Css,
}

const DISGUISES: [Disguise; 7] = [
    Disguise {
        name: "png-prefixed .png",
        extension: ".png",
        content_type: "image/png",
        prefix: Prefix::Png,
        fmp4: false,
        ranges: false,
    },
    Disguise {
        name: "jpeg-prefixed .jpg",
        extension: ".jpg",
        content_type: "image/jpeg",
        prefix: Prefix::Jpeg,
        fmp4: false,
        ranges: false,
    },
    Disguise {
        name: "gif-prefixed .gif",
        extension: ".gif",
        content_type: "image/gif",
        prefix: Prefix::Gif,
        fmp4: false,
        ranges: false,
    },
    Disguise {
        name: "css-prefixed .css",
        extension: ".css",
        content_type: "text/css",
        prefix: Prefix::Css,
        fmp4: false,
        ranges: false,
    },
    Disguise {
        name: "png-prefixed, no extension",
        extension: "",
        content_type: "image/png",
        prefix: Prefix::Png,
        fmp4: false,
        ranges: false,
    },
    Disguise {
        name: "jpeg-prefixed .jpg on a range origin",
        extension: ".jpg",
        content_type: "image/jpeg",
        prefix: Prefix::Jpeg,
        fmp4: false,
        ranges: true,
    },
    Disguise {
        name: "jpeg-prefixed fMP4 .jpg",
        extension: ".jpg",
        content_type: "image/jpeg",
        prefix: Prefix::Jpeg,
        fmp4: true,
        ranges: true,
    },
];

struct Fixtures {
    ts: HashMap<String, Vec<u8>>,
    fmp4: HashMap<String, Vec<u8>>,
    png: Vec<u8>,
    jpeg: Vec<u8>,
}

fn ffmpeg(args: &[&str], dir: &Path) {
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error"])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("ffmpeg generates the disguised HLS fixture");
    assert!(status.success(), "ffmpeg fixture generation failed");
}

fn read_dir(dir: &Path) -> HashMap<String, Vec<u8>> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

/// Generated once per test binary: a TS and an fMP4 HLS rendition of the
/// same 6-second clip, plus a real PNG and JPEG used as disguise headers.
fn fixtures() -> &'static Fixtures {
    static FIXTURES: OnceLock<Fixtures> = OnceLock::new();
    FIXTURES.get_or_init(|| {
        let root =
            std::env::temp_dir().join(format!("viptv-plugin-disguised-{}", std::process::id()));
        let (ts, fmp4) = (root.join("ts"), root.join("fmp4"));
        for dir in [&root, &ts, &fmp4] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let source = [
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
            "-g",
            "24",
            "-c:a",
            "aac",
            "-shortest",
            "-f",
            "hls",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
        ];
        let mut args = source.to_vec();
        args.extend(["-hls_segment_filename", "seg%d.ts", "index.m3u8"]);
        ffmpeg(&args, &ts);
        let mut args = source.to_vec();
        args.extend([
            "-hls_segment_type",
            "fmp4",
            "-hls_fmp4_init_filename",
            "init.mp4",
            "-hls_segment_filename",
            "seg%d.m4s",
            "index.m3u8",
        ]);
        ffmpeg(&args, &fmp4);
        let image = |name: &str| {
            ffmpeg(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc=size=32x32",
                    "-frames:v",
                    "1",
                    name,
                ],
                &root,
            );
            std::fs::read(root.join(name)).unwrap()
        };
        Fixtures {
            ts: read_dir(&ts),
            fmp4: read_dir(&fmp4),
            png: image("prefix.png"),
            jpeg: image("prefix.jpg"),
        }
    })
}

/// The disguised origin: path → (lying content type, bytes).
fn disguised_origin(disguise: Disguise) -> HashMap<String, (String, Vec<u8>)> {
    let fixtures = fixtures();
    let prefix: Vec<u8> = match disguise.prefix {
        Prefix::Png => fixtures.png.clone(),
        Prefix::Jpeg => fixtures.jpeg.clone(),
        Prefix::Gif => b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xff\xff\xff!\xf9\x04\x01\0\0\0\0,\0\0\0\0\x01\0\x01\0\0\x02\x02D\x01\0;".to_vec(),
        Prefix::Css => b"@font-face{font-family:G;src:url(g.woff)}\nbody{background:#000}\n".to_vec(),
    };
    let files = if disguise.fmp4 {
        &fixtures.fmp4
    } else {
        &fixtures.ts
    };
    let mut origin = HashMap::new();
    let mut playlist = String::from_utf8(files["index.m3u8"].clone()).unwrap();
    for (name, bytes) in files {
        let Some(stem) = name
            .strip_suffix(".ts")
            .or_else(|| name.strip_suffix(".m4s"))
            .or_else(|| name.strip_suffix(".mp4"))
        else {
            continue;
        };
        let disguised = format!("{stem}-x{}", disguise.extension);
        // Query strings must survive the proxy's rewrite.
        playlist = playlist.replace(name.as_str(), &format!("{disguised}?sig={stem}"));
        let mut body = prefix.clone();
        body.extend(bytes);
        origin.insert(
            format!("/media/{disguised}"),
            (disguise.content_type.to_owned(), body),
        );
    }
    origin.insert(
        "/media/index.m3u8".into(),
        (
            "application/vnd.apple.mpegurl".into(),
            playlist.into_bytes(),
        ),
    );
    origin
}

/// Serves `origin`, refusing requests that lack the source authorization.
fn serve(origin: HashMap<String, (String, Vec<u8>)>, ranges: bool) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let origin = Arc::new(origin);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let origin = origin.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut stream = stream;
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() {
                    return;
                }
                let mut headers = HashMap::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
                    }
                }
                let path = request_line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .split('?')
                    .next()
                    .unwrap_or("/");
                let authorized = headers.get("cookie").map(String::as_str) == Some(COOKIE)
                    && headers.get("referer").map(String::as_str) == Some(REFERER)
                    && headers.get("user-agent").map(String::as_str) == Some(USER_AGENT);
                let Some((content_type, body)) = origin.get(path).filter(|_| authorized) else {
                    let status = if authorized {
                        "404 Not Found"
                    } else {
                        "401 Unauthorized"
                    };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    return;
                };
                let range = headers
                    .get("range")
                    .filter(|_| ranges)
                    .and_then(|range| range.strip_prefix("bytes="))
                    .and_then(|range| range.split_once('-'))
                    .and_then(|(start, end)| {
                        let start: usize = start.parse().ok()?;
                        let end = end
                            .parse::<usize>()
                            .unwrap_or(body.len() - 1)
                            .min(body.len() - 1);
                        Some((start, end))
                    });
                let (status, start, end) = match range {
                    Some((start, end)) => ("206 Partial Content", start, end),
                    None => ("200 OK", 0, body.len() - 1),
                };
                let content_range = if range.is_some() {
                    format!("Content-Range: bytes {start}-{end}/{}\r\n", body.len())
                } else {
                    String::new()
                };
                let accept = if ranges {
                    "Accept-Ranges: bytes\r\n"
                } else {
                    ""
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n{accept}{content_range}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    end - start + 1
                );
                let _ = stream.write_all(&body[start..=end]);
            });
        }
    });
    port
}

/// Registers the disguised origin with `proxy` exactly as `native_open` does
/// and returns the URI the engine opens.
fn proxied_uri(proxy: &Proxy, disguise: Disguise) -> String {
    let port = serve(disguised_origin(disguise), disguise.ranges);
    let payload: NativeOpenRequest = serde_json::from_value(serde_json::json!({
        "uri": format!("http://127.0.0.1:{port}/media/index.m3u8"),
        "x": 0, "y": 0, "width": 100, "height": 100,
        "sessionKey": disguise.name,
        "cookies": COOKIE, "referrer": REFERER, "userAgent": USER_AGENT,
    }))
    .unwrap();
    payload.validate_authorization().unwrap();
    let (proxied, pending) = proxy.route(payload);
    assert!(
        pending.proxied(),
        "{}: HLS source was not proxied",
        disguise.name
    );
    assert!(proxied.cookies.is_none() && proxied.referrer.is_none());
    proxied.uri
}

#[cfg(feature = "gstreamer-runtime")]
#[test]
fn gstreamer_plays_disguised_hls_through_the_proxy() {
    use gstreamer as gst;
    use gstreamer::prelude::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    gst::init().unwrap();
    let proxy = Proxy::start().expect("source proxy starts");
    let mut failures = Vec::new();
    for disguise in DISGUISES {
        let uri = proxied_uri(&proxy, disguise);
        let pipeline = gst::ElementFactory::make("playbin3").build().unwrap();
        pipeline.set_property("uri", uri);
        let frames = Arc::new(AtomicU64::new(0));
        let video = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .property("signal-handoffs", true)
            .build()
            .unwrap();
        let counted = frames.clone();
        video.connect("handoff", false, move |_| {
            counted.fetch_add(1, Ordering::Relaxed);
            None
        });
        let audio = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .build()
            .unwrap();
        pipeline.set_property("video-sink", &video);
        pipeline.set_property("audio-sink", &audio);
        pipeline.set_state(gst::State::Playing).unwrap();
        let message = pipeline.bus().unwrap().timed_pop_filtered(
            gst::ClockTime::from_seconds(60),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        let ended = matches!(
            message.as_ref().map(|m| m.view()),
            Some(gst::MessageView::Eos(_))
        );
        pipeline.set_state(gst::State::Null).unwrap();
        let frames = frames.load(Ordering::Relaxed);
        // 6 s at 24 fps is 144 frames; every segment must have decoded.
        if !ended || frames < 140 {
            failures.push(format!("{}: eos={ended} frames={frames}", disguise.name));
        }
    }
    assert!(failures.is_empty(), "GStreamer failures: {failures:#?}");
}

#[cfg(feature = "mpv-runtime")]
#[test]
fn mpv_plays_disguised_hls_through_the_proxy() {
    use std::time::{Duration, Instant};

    let proxy = Proxy::start().expect("source proxy starts");
    let mut failures = Vec::new();
    for disguise in DISGUISES {
        let uri = proxied_uri(&proxy, disguise);
        let mpv = super::linux_mpv::create_engine().expect("mpv engine handle");
        mpv.set_property("vo", "null".to_owned()).unwrap();
        mpv.set_property("ao", "null".to_owned()).unwrap();
        mpv.set_property("speed", 4.0_f64).unwrap();
        mpv.set_property("pause", false).unwrap();
        mpv.command("loadfile", &[&uri, "replace"]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let position = || mpv.get_property::<f64>("time-pos").unwrap_or(0.0);
        let ended = loop {
            // keep-open holds the last frame at the end instead of unloading.
            if mpv.get_property::<bool>("eof-reached").unwrap_or(false) {
                break true;
            }
            if Instant::now() > deadline
                || mpv.get_property::<bool>("idle-active").unwrap_or(false)
                    && Instant::now() > deadline - Duration::from_secs(50)
            {
                break false;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let (position, frames) = (
            position(),
            mpv.get_property::<i64>("estimated-frame-number")
                .unwrap_or(0),
        );
        let _ = mpv.command("stop", &[]);
        if !ended || position < 5.5 || frames < 130 {
            failures.push(format!(
                "{}: eof={ended} position={position:.2} frames={frames}",
                disguise.name
            ));
        }
    }
    assert!(failures.is_empty(), "mpv failures: {failures:#?}");
}
