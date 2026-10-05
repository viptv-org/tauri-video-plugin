# Concurrent desktop movie playback — 2026-10-04

Twenty distinct real account movie deliveries were admitted through the local
trusted HTTPS backend. Source URLs and credentials remain in private inputs.
The final set includes MKV and MP4, H.264 and HEVC, and resolutions from
720×300 to 3840×2160. Synthetic fixtures were not used for this batch.

The original shared adapter sent a second resume seek immediately after
`native_open`. All twenty libmpv handles reproduced `MPV_ERROR_COMMAND (-12)`
on that command: `loadfile` returns before file loading completes. MPV already
receives the resume target through its native `start` option. The adapter now
leaves startup positioning to MPV. Its regression originally failed with the
reported `prepare-failed` message and diagnostic HTTP 206, then passed.

Final MPV results: **20/20** concurrent production engine opens passed decoding
at 30 seconds, pause, seek to 60 seconds, resume and stop. **20/20** independent
GTK/libmpv OpenGL surfaces passed frame presentation at the requested position,
pause, resume and close on an isolated X11 display. The graphics display used
Xvfb/Mesa; these results do not qualify Windows, HDR tone mapping, audio-device
output or every hardware decoder.

GStreamer engine decoding passed **20/20** for the same deliveries. Native
surface testing also exposed a seek-before-preroll race: `state(3 seconds)`
can return `Ok(Async)` while the pipeline is still READY. The native worker now
waits for PAUSED/PLAYING with a fifteen-second bound and checks replacement
ownership every 100 ms. Before the follow-up graphics fix, a full surface run passed **15/20**; the other
five failed in graphics output, rather than the initial seek. Missing GL
resources were previously mislabeled as missing movies; their error code is
now `VIDEO_OUTPUT_FAILED`, covered by a failing-then-passing bus regression.

## Follow-up: graphics upload failures resolved

The graphics failures also reproduced with one real movie, independently of
concurrency. A fresh baseline passed 16/20 surfaces; the same movie failed
alone with `glupload` reporting RESOURCE/NOT_FOUND. Instrumentation identified
the [upstream upload error](https://github.com/GStreamer/gstreamer/blob/1.28.7/subprojects/gst-plugins-base/ext/gl/gstgluploadelement.c#L317)
as `Failed to upload buffer`, rather than GL
context initialization failure. Keeping the surface visible did not fix it.
Disabling VA decoding made the affected movie pass, establishing the hardware
decoded buffer-to-GL boundary as the failing path.

A SystemMemory capsfilter alone still failed. The production player now uses
`videoconvert ! video/x-raw,format=RGBA` as its playbin video filter, making an
RGBA frame available before the GL upload. This retains native hardware
decoding and the existing GL rendering/subtitle path while removing the
requirement to import decoder buffers directly. It adds CPU conversion and
memory transfer work. Temporary probes observed VA hardware decoding in
**20/20** successful real-movie surface cases and were removed afterward.

The graphics fix passed **20/20 GStreamer native surfaces concurrently** for
the unchanged final movie corpus, including all five previously failing cases.
The missing-source/recovery fixture also passed on both MPV and GStreamer,
covering seek, pause/resume, alternate audio/subtitle selection, rendered
caption pixels, subtitle removal, paused Fit/Fill, and volume. Normal plugin
tests and Clippy passed, as did 37 tests with native engine features disabled.
The final MPV native surface rerun also passed **20/20 concurrently**.
The five previously recorded graphics failures are
closed for these tested paths.

Initial discovery batches also exposed unavailable accounts/sources returning
JSON or HTML with HTTP 200, 503 and 512. Both decoders rejected those bodies.
Those cases were logged separately and replaced for the final playable set.
HTTP 206 on a diagnostic range GET was never a playback refusal.

Retained reproduction seams:

- `concurrent_real_movie_mpv_playback`: twenty full libmpv open sequences.
- `concurrent_real_movie_gstreamer_playback`: twenty real decode pipelines.
- `real_movie_surface_case`: the production native surface and control path.
- `qualification/movie-surfaces.py`: twenty isolated surface test processes,
  launched together with per-movie logs.

Build the test binary with `cargo test --all-features --lib --no-run`.
The engine batch tests are opt-in: set `VIPTV_NATIVE_PROVIDER_CASES` to a private
twenty-entry JSON file, then run the chosen test with `-- --ignored --nocapture`.
For surface testing, set `DISPLAY` and `GDK_BACKEND=x11`, then run
`python3 qualification/movie-surfaces.py mpv TEST_BINARY PRIVATE_JSON OUTPUT_DIR`.
No source URLs, headers, cookies or credentials belong in committed artifacts.
