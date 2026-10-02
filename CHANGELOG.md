# Changelog

Release numbering follows the
[versioning and compatibility policy](VERSIONING.md).

## Unreleased

- Allocate Linux's native-video overlay and its reparented WebView immediately,
  preserving the requested video rectangle and full-window UI allocation.
- Implement GStreamer Crop as aspect-preserving centered presentation inside a
  clipped viewport, including paused frames, rather than stretching the video.


- Serve desktop HLS sources (Linux and Windows, GStreamer and mpv) through a
  plugin-owned loopback sanitizing proxy. It fetches upstream with the
  validated source headers, rewrites playlists back through itself, strips
  image/stylesheet/junk prefixes before the first MPEG-TS packet run, fMP4
  box or ADTS/ID3 audio, corrects the segment `Content-Type`, and maps byte
  ranges past the stripped prefix. Disguised segments (`.png`/`.jpg`/`.gif`/
  `.css`/no extension, lying `Content-Type`, prepended PNG/JPEG/GIF/CSS) now
  play on both engines. Additive: `native_diagnostics.sourceProxy`,
  `NativeOpenRequest.sourceProxy` (force/opt out) and
  `NativePlaybackSnapshot.sourceProxied`; protocol stays 1.
- `native_diagnostics.engines` lists GStreamer first, matching the default
  backend for an omitted request; mpv is an explicit Linux alternative.
- Linux `native_close` ignores late cleanup presented with a stale session
  key instead of parking the newer session (Windows already did).
- Engine diagnostics use `tracing` instead of `TAURI_VIDEO_TELEMETRY`-gated
  `eprintln!`.
- Rewrite `SPEC.md` from the code; retire the obsolete release-consistency
  script and publishing skill; point package metadata at `viptv-org`.

- Return the adapter to a WebView-only contract: remove the Blitz/native-only
  entrypoint, canvas/Blits example, and transparent-canvas bypass. Android
  native playback now requires Tauri to initialize the plugin with a WebView.

- Replace the Windows external video surface with WebView2 TextureStream,
  presenting pooled GStreamer D3D11 textures on the real HTML video element so
  controls, scrolling, live resize, fullscreen, and Snap Layouts are one
  compositor transaction.
- Support Windows fit, crop-to-cover, stretch, and zoom through DOM geometry.
- Report the selected Windows GStreamer decoder and presented-frame count,
  with a GPU-resident NV12 path and software-decoder upload fallback.
- Add Windows pixel checks for playback, seek, pause/resume, zoomed overlays,
  tooltips, resize, fullscreen, and Snap Layouts.
- Rebuild the linked guest adapter before the example starts or bundles, and
  show the underlying native startup cause when attachment fails.

## 0.4.0

- Add the now-retired renderer adapter.
- Open the `@get-air/video` peer compatibility line to `>=0.3.0 <0.4.0` and
  test against exact core `0.3.0`.

## 0.3.0

- Remove the `auto` native-engine option and automatic engine-priority configuration.
- Default omitted engine selection to Media3 on Android and GStreamer on desktop platforms.
- Open the `@get-air/video` peer compatibility line to `>=0.2.0 <0.3.0` and test against exact core `0.2.0`.

## 0.2.0

- Made `@get-air/video` a required peer (`>=0.1.1 <0.2.0`) so applications
  own one shared core instance while the adapter tests an exact published core.
- Added a cached, retryable JS↔Rust protocol handshake and bidirectional
  `native_open` validation before native player allocation.
- Added schema-backed `VideoNativeProtocolMismatchError` diagnostics that retain
  their tag and npm/crate fields through the shared Promise and Effect clients.
- Added generated permissions for read-only native diagnostics and enforced
  npm/Cargo version, peer-range, protocol, changelog, and tag consistency.
- Documented the independent core and lockstep adapter/crate versioning policy,
  and added clone-safe contributor guidance and release skills.

## 0.1.0

- Published the JavaScript adapter as `@get-air/video-tauri`, implementing the
  same client/controller contract as `@get-air/video` without putting Tauri in
  the DOM package.
- Added native-surface playback on Linux and Windows with GStreamer, plus
  optional explicit mpv on Linux.
- Added native-surface playback on Android and Android TV with Media3 and
  optional explicit LibVLC.
- Added CSS aperture synchronization, visibility suspension, track controls,
  and native playback telemetry behind the shared Air controller.
- Kept buffering under the selected player by default, with optional backend-specific tuning.
- Added emulator and Linux qualification tooling.
