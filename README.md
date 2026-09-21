# VIPTV Tauri video plugin

Native playback adapter for the VIPTV Tauri desktop application. It was independently imported from `get-air/tauri-video-plugin` at `6baf19ff74bfe5278838f05d9eaa4af55ecc4d05`; its Apache-2.0 and MIT license texts and source history are retained.

The desktop product uses Tauri + React. The DOM owns VIPTV layout, controls, focus, and overlays. This Rust plugin owns native engine adaptation and attaches that capability to the shared controller in `viptv-org/video`.

Inherited GitHub workflows and release commands were removed. Run `npm run check`, `npm run build`, and `cargo test` locally when the required host media runtime is installed.

## Identity

This repository is VIPTV's native Tauri video engine. The Rust crate
(`tauri-plugin-video`) is consumed by the VIPTV desktop app by path. The npm
package surface (`@viptv/video-tauri`) is the JavaScript protocol façade; it
wraps the upstream get-air player library (`@get-air/video`) as a peer
dependency, which is why that dependency remains.
