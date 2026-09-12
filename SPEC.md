# Tauri video plugin specification

## Purpose

`viptv-org/tauri-video-plugin` is the desktop-native adapter for the shared VIPTV TypeScript video controller. It maps controller operations to Tauri-native playback without imposing a native UI.

## Interface

The React application attaches a normal video element through the controller, chooses `tauri` explicitly, and supplies its source and policy. The plugin provides playback, volume, tracks, custom headers, live/DVR state, source replacement, and typed unsupported-feature failures.

## Platform behavior

Linux uses GStreamer by default, with MPV an optional runtime. Windows uses its native texture path. Android support remains inherited for consumers that embed the plugin, but the VIPTV desktop app targets desktop Tauri. The plugin does not claim universal codec, HDR, DRM, or UHD support.

## UX boundary

The design repository is authoritative for controls, shortcuts, focus, pause/resume, source picker, up-next, and next-episode behavior. The plugin only exposes playback facts and commands.

## Acceptance

Run TypeScript checks/build and Rust tests. Verify the target native engine on its host for video surface attachment, source replacement, tracks, subtitles, live/DVR seeking, and error mapping.
