# Contributing to the VIPTV Tauri video plugin

This repository contains VIPTV's native Tauri playback plugin: the
`tauri-plugin-video` Rust crate and its JavaScript protocol façade
(`@viptv/video-tauri`). The shared playback controller lives in
[`viptv-org/video`](https://github.com/viptv-org/video); product behavior and
UX are owned by [`viptv-org/design`](https://github.com/viptv-org/design).
Read [`SPEC.md`](SPEC.md) and [`AGENTS.md`](AGENTS.md) before changing behavior.

## Set up

Use Node.js 24, the stable Rust toolchain supported by `Cargo.toml`, and frozen
lockfiles:

```sh
npm ci
rustup toolchain install stable
```

Install platform media development packages before native builds; see
[`docs/linux.md`](docs/linux.md), [`docs/windows.md`](docs/windows.md), and
[`docs/android.md`](docs/android.md). Never use a parent workspace or
cross-repository path dependency in the manifests.

## Design expectations

- Keep VIPTV controls, overlays, focus and layout in React (design repo).
- Keep native playback behind the `tauri` backend and its IPC boundary.
- Preserve explicit engine selection, native-surface geometry, source
  replacement safety, cleanup, typed errors, and truthful engine capabilities.
- Keep Android free of desktop GIO/GStreamer/GTK/mpv dependencies and keep
  desktop dependencies target-scoped.
- Never log source URLs, headers, cookies, licenses or local paths. Use the
  `tracing` macros for diagnostics; the host application decides where they go.
- Treat incompatible command, payload, response, or cross-boundary error
  changes as protocol changes (see [`VERSIONING.md`](VERSIONING.md)). Additive
  diagnostics use capabilities instead.

## Validate a change

Run the applicable focused test first, then the local gates:

```sh
npm run check
npm run build
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo test --locked --all-targets --no-default-features
```

Windows and Android changes need their respective hosts; record what could not
be exercised locally in the pull request.

## Delivery policy

Per the organization delivery policy (2026-09-27) this
repository has no GitHub Actions workflows, PR gates, automatic releases, or
registry publication. Consumers (`viptv-org/desktop`, `viptv-org/tv-web`) pin
the crate and package by Git commit. Merge reviewed changes to `main`, then
update the consumer pins deliberately.

## Repository skills

- [Effect best practices](.agents/skills/effect-best-practices/SKILL.md) for
  the JavaScript façade.
