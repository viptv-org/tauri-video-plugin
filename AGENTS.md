# VIPTV Tauri video plugin agent guide

Delivery policy (owner approved 2026-09-27): only Android, desktop, Roku and TV-web
build workflows remain, triggered by main pushes and manual dispatch. No PR
gates, automatic releases, image publishing or deployment. Retain local checks.
This supersedes older automation/release-gate instructions below.

Read `DESIGN_REF` and `SPEC.md` before changing behavior. Keep the plugin's seam narrow: it adapts native playback engines to the shared `viptv-org/video` controller; React owns every visible VIPTV control and overlay.

Preserve explicit backend selection, typed failures, source replacement safety, header support, independent tracks, and live/DVR facts. Never log source URLs, headers, cookies, licenses, or local paths. Treat platform codec/HDR/DRM/UHD support as runtime facts.

Validate Rust and TypeScript changes locally. Linux needs GStreamer or the selected native engine; Windows and Android checks need their respective hosts. Do not add publication or CI/CD automation until the organization delivery policy is approved.
