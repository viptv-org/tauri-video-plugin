# Validation

Validated from commit `3147bd3d0798a9c4107608fd4a9d190c120ff611` on 2026-09-12.

| Command | Result |
| --- | --- |
| `npm ci --ignore-scripts --no-audit --no-fund` | passed; 87 packages installed for local validation. npm warned that the locked Git dependency `get-air/video` skipped an integrity check. |
| `npm run check` | passed: TypeScript, Effect diagnostics, and 32 Vitest tests |
| `npm run build` | passed |
| `cargo test` | not run: this validation shell's `PATH` omitted the installed toolchain at `/home/node/.cargo/bin` |
| native GStreamer/MPV validation | not run: `gstreamer-1.0` and `libmpv` are absent from `pkg-config` |

Rust/Cargo are installed at `/home/node/.cargo/bin`; only this shell path prevented the Rust command from being discovered. The passing suite verifies the TypeScript adapter/controller protocol. It does not establish a native Tauri surface, GStreamer, MPV, Windows texture, Android, codec, DRM, HDR, or physical-device playback claim.
