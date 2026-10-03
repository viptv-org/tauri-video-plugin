# VIPTV Tauri video plugin

Actions delivery: main pushes and manual builds produce sideloading artifacts
(Android universal APK; desktop Windows/Linux installers; Roku ZIP; TV WGT/IPK).
Other repositories have no Actions workflows. Local checks remain; previous
CI/release-publication descriptions below are historical. No automatic deploys.

Native playback adapter for the VIPTV Tauri desktop application. It was independently imported from `get-air/tauri-video-plugin` at `6baf19ff74bfe5278838f05d9eaa4af55ecc4d05`; its Apache-2.0 and MIT license texts and source history are retained.

The desktop product uses Tauri + React. The DOM owns VIPTV layout, controls, focus, and overlays. This Rust plugin owns native engine adaptation and attaches that capability to the shared controller in `viptv-org/video`.

Inherited GitHub workflows and release commands were removed. Run `npm run check`, `npm run build`, and `cargo test` locally when the required host media runtime is installed.

## Identity

This repository is VIPTV's native Tauri video engine. The Rust crate
(`tauri-plugin-video`) is consumed by the VIPTV desktop app by path. The npm
package surface (`@viptv/video-tauri`) is the JavaScript protocol façade; it
wraps the upstream get-air player library (`@get-air/video`) as a peer
dependency, which is why that dependency remains.

## Linux playback control checks

GTK owns widget allocation and presentation. GStreamer transitions, seeks,
stream selection, and timing queries run in a FIFO worker; snapshots read only
completed telemetry. A dedicated audio filter keeps volume changes outside
playbin's topology lock. MPV redraw callbacks publish an atomic flag, with the
owning GL context made current before render updates and destruction.

Run the ordinary native checks with both engines enabled:

```sh
cargo test --features 'gstreamer-runtime mpv-runtime' --lib -- --test-threads=1
```

For native GTK controls and rendered caption checks, generate silent media and
use an isolated display so occlusion cannot suppress frame presentation:

```sh
python3 qualification/generate-controls-fixture.py /tmp/viptv-native-controls
GDK_BACKEND=x11 VIPTV_NATIVE_PROVIDER_CASES=/tmp/viptv-native-controls/cases.json \
  xvfb-run -a cargo test --features 'gstreamer-runtime mpv-runtime' \
  real_provider_native_surface_controls -- --ignored --nocapture --test-threads=1
```

The same opt-in test accepts private, authorized real-provider cases. Each entry
has an anonymous `alias` and a `NativeOpenRequest` in `payload`; a `vod` alias
also checks a seek to 30 seconds. Keep input files and credentials outside Git.
The check verifies decoded frames, timing, pause/resume, available alternate
tracks, subtitles off, paused picture modes, volume, and continued progress.
Caption pixel comparisons require the generated solid-color fixture and its
`caption-pixels` alias. After track-selection confirmation, seekable GStreamer deliveries reset their
timeline at the current position so new subtitle branches share the video
clock. GStreamer may still lose an already-active sparse subtitle cue when
seeking into it, until the next cue arrives. This check does not establish universal codec,
HDR, DRM, or device support.

Threading references: [GStreamer element operations](https://gstreamer.freedesktop.org/documentation/gstreamer/gstelement.html),
[libmpv render API](https://github.com/mpv-player/mpv/blob/master/include/mpv/render.h).

## Failure classification

Generic pipeline failures do not establish a decoder problem. The unchanged
wire shape carries distinct codes for explicit decoder/format failures, video
and audio output, protected media, source loading, runtime setup, authorization
and network failures. GStreamer classification uses its typed error domains;
MPV classification uses numeric API errors. Raw runtime messages remain private.

Set `VIPTV_NATIVE_FAILURE_RECOVERY=1` for the opt-in GTK control check to first
open a missing source, verify a source failure rather than a decoder diagnosis,
and then open valid media in the same engine. Both engines pass this failure
and replacement sequence on the local desktop display.
