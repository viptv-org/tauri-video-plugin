# Validation

## Linux native allocation and picture viewport — 2026-10-02

A fresh generated, header-protected H.264/AAC source was admitted through a real
account/profile/source-discovery/direct-playback lease against a test-configured
backend. Its existing test-only loopback source allowance was enabled; production
transport policy was unchanged. GStreamer decoded and advanced playback. Private
diagnostics in a muted debug desktop found a 1280x720 GTK window with every newly
reparented overlay/video/WebView child still allocated at 1x1. After immediate
allocation and window-allocation propagation, the overlay/WebView were 1280x720
and the native video widget matched its 1278x677 DOM viewport. Captures and raw
fixture credentials remained outside Git.

`cargo test --locked --lib --features mpv-runtime` passes 42 tests, including real
GStreamer/MPV decode/stat/seek and source-header checks; the GTK display test is
ignored in that ordinary run. Strict library Clippy and formatting pass. The
explicit GTK display test under Xvfb passes: a 400x300 viewport clips a 534x300
wide-source surface at a centered horizontal adjustment of 67, clips a tall
400x600 surface at vertical adjustment 150, then restores Fit without growing the
viewport. These are actual GTK allocations of an existing static widget, not a
pixel assertion or physical decoder test. Crop is presentation only, without a
pipeline format change, decoder seek or new backend lease.

The actual desktop UI must adopt the video picture-mode command and this plugin
revision together before its toggle is qualified. Final real-display pixel/input
checks and Windows/macOS/clean-machine qualification remain separate. No production
data, source, deployment or installer publication was used.

## Native source headers and safe failures — 2026-09-29

`cargo test --locked --lib --features mpv-runtime` passed 13 tests on Linux,
including real GStreamer HTTP MP4 decode/stat/seek against a fixture requiring
Authorization, Referer, Cookie and User-Agent. The test calls the production
source-setup function. A real HTTP 401 retains AUTHORIZATION_FAILED without
exposing the source query token. MPV decode/stat/seek/start-position fixtures
also passed. Strict library Clippy passed; TypeScript/Effect checks and 31 JS
tests passed, and the JS build passed (existing circular-dependency warning).

Native input headers are bounded and validated before opening. Error Display,
Debug and serialized payloads use safe messages; typed GStreamer source errors
survive subsequent polls. Windows uses the same classification but was not built
or run on Windows. These tests do not qualify an installed desktop surface,
hardware codecs, 4K/HDR/DRM, or Windows TextureStream playback.

Historical validation follows.

Validated from commit `3147bd3d0798a9c4107608fd4a9d190c120ff611` on 2026-09-12.

| Command | Result |
| --- | --- |
| `npm ci --ignore-scripts --no-audit --no-fund` | passed; 87 packages installed for local validation. npm warned that the locked Git dependency `get-air/video` skipped an integrity check. |
| `npm run check` | passed: TypeScript, Effect diagnostics, and 32 Vitest tests |
| `npm run build` | passed |
| `cargo test` | not run: this validation shell's `PATH` omitted the installed toolchain at `/home/node/.cargo/bin` |
| native GStreamer/MPV validation | not run: `gstreamer-1.0` and `libmpv` are absent from `pkg-config` |

Rust/Cargo are installed at `/home/node/.cargo/bin`; only this shell path prevented the Rust command from being discovered. The passing suite verifies the TypeScript adapter/controller protocol. It does not establish a native Tauri surface, GStreamer, MPV, Windows texture, Android, codec, DRM, HDR, or physical-device playback claim.
