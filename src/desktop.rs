use serde::de::DeserializeOwned;
use std::{sync::mpsc, time::Duration};
use tauri::{plugin::PluginApi, AppHandle, Runtime};

use crate::models::{
    NativeControlRequest, NativeLayoutRequest, NativeOpenRequest, NativePlaybackSnapshot,
    NativeSessionRequest,
};

pub fn init<R: Runtime, C: DeserializeOwned>(
    app: &AppHandle<R>,
    _api: PluginApi<R, C>,
) -> crate::Result<DesktopVideo<R>> {
    #[cfg(target_os = "linux")]
    {
        #[allow(deprecated)]
        let (main_sender, main_receiver) =
            gtk::glib::MainContext::sync_channel::<MainJob<R>>(gtk::glib::Priority::DEFAULT, 32);
        let context = app.clone();
        main_receiver.attach(None, move |job| {
            job(&context);
            gtk::glib::ControlFlow::Continue
        });
        Ok(DesktopVideo {
            _app: app.clone(),
            main_sender,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(DesktopVideo { _app: app.clone() })
    }
}

#[cfg(target_os = "linux")]
type MainJob<R> = Box<dyn FnOnce(&AppHandle<R>) + Send + 'static>;

pub struct DesktopVideo<R: Runtime> {
    _app: AppHandle<R>,
    #[cfg(target_os = "linux")]
    main_sender: gtk::glib::SyncSender<MainJob<R>>,
}

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(windows)]
use windows as platform;

impl<R: Runtime> DesktopVideo<R> {
    #[cfg(any(target_os = "linux", windows))]
    pub fn open_native(&self, payload: NativeOpenRequest) -> crate::Result<NativePlaybackSnapshot> {
        // HLS sources are served through the loopback sanitizing proxy; the
        // registration only becomes current once the engine accepts it.
        let (payload, pending) = source_proxy::route(payload);
        let proxied = pending.proxied();
        let result = self.run_on_main(move |app| platform::open(app, payload));
        source_proxy::settle(pending, result.is_ok());
        result.map(|mut snapshot| {
            snapshot.source_proxied = proxied;
            snapshot
        })
    }

    #[cfg(any(target_os = "linux", windows))]
    pub fn control_native(
        &self,
        payload: NativeControlRequest,
    ) -> crate::Result<NativePlaybackSnapshot> {
        let proxied = source_proxy::active(&payload.session_key);
        self.run_on_main(move |_| platform::control(payload))
            .map(|snapshot| with_proxy_flag(snapshot, proxied))
    }

    #[cfg(any(target_os = "linux", windows))]
    pub fn layout_native(&self, payload: NativeLayoutRequest) -> crate::Result<()> {
        // The WebView publishes its matching transparent aperture after this
        // command resolves, so completion must mean the native host received the move.
        // The guest serializes layout calls, keeping this dispatcher bounded.
        self.run_on_main(move |_| platform::layout(payload))
    }

    #[cfg(any(target_os = "linux", windows))]
    pub fn stats_native(
        &self,
        payload: NativeSessionRequest,
    ) -> crate::Result<NativePlaybackSnapshot> {
        let proxied = source_proxy::active(&payload.session_key);
        self.run_on_main(move |_| platform::stats(payload))
            .map(|snapshot| with_proxy_flag(snapshot, proxied))
    }

    #[cfg(any(target_os = "linux", windows))]
    pub fn close_native(&self, payload: NativeSessionRequest) -> crate::Result<()> {
        let session_key = payload.session_key.clone();
        self.run_on_main(move |_| {
            // Explicit debug-only fault injection for the host's watchdog test.
            #[cfg(debug_assertions)]
            if std::env::var("VIPTV_TEST_BLOCK_NATIVE_CLOSE")
                .ok()
                .as_deref()
                == Some("1")
            {
                std::thread::sleep(Duration::from_secs(60));
            }
            platform::close(payload)
        })?;
        // Same ownership rule as the engine: a late close from an older
        // controller leaves the newer session's route alone.
        source_proxy::release(&session_key);
        Ok(())
    }

    #[cfg(any(target_os = "linux", windows))]
    pub fn shutdown_native(&self) -> crate::Result<()> {
        let result = self.run_on_main(move |_| platform::shutdown());
        // Route cancellation is thread-safe and must still run when the
        // native UI dispatcher times out or never executes the operation.
        source_proxy::shutdown();
        result
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    pub fn shutdown_native(&self) -> crate::Result<()> {
        Ok(())
    }

    #[cfg(windows)]
    pub fn prepare_texture_stream(&self, stream_id: String) -> crate::Result<()> {
        windows::prepare_texture_stream(&self._app, stream_id)
    }

    #[cfg(windows)]
    fn run_on_main<T, F>(&self, operation: F) -> crate::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&AppHandle<R>) -> crate::Result<T> + Send + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel(1);
        let context = self._app.clone();
        self._app
            .run_on_main_thread(move || {
                let _ = sender.send(operation(&context));
            })
            .map_err(|error| {
                crate::Error::Pipeline(format!("native UI dispatcher is unavailable: {error}"))
            })?;
        receiver
            .recv_timeout(Duration::from_secs(15))
            .map_err(|error| {
                crate::Error::Pipeline(format!("native UI thread timed out: {error}"))
            })?
    }

    #[cfg(target_os = "linux")]
    fn run_on_main<T, F>(&self, operation: F) -> crate::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&AppHandle<R>) -> crate::Result<T> + Send + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.main_sender
            .send(Box::new(move |context| {
                let _ = sender.send(operation(context));
            }))
            .map_err(|error| {
                crate::Error::Pipeline(format!("native UI dispatcher is unavailable: {error}"))
            })?;
        receiver
            .recv_timeout(Duration::from_secs(15))
            .map_err(|error| {
                crate::Error::Pipeline(format!("native UI thread timed out: {error}"))
            })?
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    fn unsupported<T>(&self) -> crate::Result<T> {
        Err(crate::Error::InvalidRequest(
            "native desktop surfaces are currently implemented on Linux and Windows".into(),
        ))
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    pub fn open_native(&self, _: NativeOpenRequest) -> crate::Result<NativePlaybackSnapshot> {
        self.unsupported()
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    pub fn control_native(&self, _: NativeControlRequest) -> crate::Result<NativePlaybackSnapshot> {
        self.unsupported()
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    pub fn layout_native(&self, _: NativeLayoutRequest) -> crate::Result<()> {
        self.unsupported()
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    pub fn stats_native(&self, _: NativeSessionRequest) -> crate::Result<NativePlaybackSnapshot> {
        self.unsupported()
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    pub fn close_native(&self, _: NativeSessionRequest) -> crate::Result<()> {
        self.unsupported()
    }
}

#[cfg(any(target_os = "linux", windows))]
fn with_proxy_flag(mut snapshot: NativePlaybackSnapshot, proxied: bool) -> NativePlaybackSnapshot {
    snapshot.source_proxied = proxied;
    snapshot
}

#[cfg(any(target_os = "linux", windows))]
pub(crate) mod source_proxy;

#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
#[cfg(any(feature = "gstreamer-runtime", feature = "mpv-runtime"))]
mod linux_surface;

#[cfg(all(target_os = "linux", feature = "gstreamer-runtime"))]
mod linux_gstreamer;

#[cfg(all(target_os = "linux", feature = "gstreamer-runtime"))]
mod linux_picture;

#[cfg(all(target_os = "linux", feature = "mpv-runtime"))]
mod linux_mpv;

/// Engine-level tests: each real engine decodes a real mp4 served over a
/// local HTTP server — the same failure domain as a flaky provider — through
/// open → stats/first progress → seek → close. Cargo tests have no app
/// surface, so the mpv engine drives the production handle configuration
/// with the null video output and GStreamer drives playbin3 with unembedded
/// sinks; the demuxers and decoders run for real.
#[cfg(all(
    test,
    target_os = "linux",
    any(feature = "mpv-runtime", feature = "gstreamer-runtime")
))]
#[path = "desktop/engine_tests.rs"]
mod engine_http_tests;

/// Real-engine playback of disguised HLS (renamed segments, lying
/// Content-Type, image/stylesheet prefixes) through the sanitizing proxy.
#[cfg(all(
    test,
    target_os = "linux",
    any(feature = "mpv-runtime", feature = "gstreamer-runtime")
))]
#[path = "desktop/source_proxy_engine_tests.rs"]
mod source_proxy_engine_tests;

#[cfg(all(target_os = "linux", not(feature = "mpv-runtime")))]
use unavailable_linux_backend as linux_mpv;
#[cfg(all(target_os = "linux", not(feature = "gstreamer-runtime")))]
use unavailable_linux_backend as linux_gstreamer;

#[cfg(all(
    target_os = "linux",
    any(not(feature = "mpv-runtime"), not(feature = "gstreamer-runtime"))
))]
mod unavailable_linux_backend;

#[cfg(all(
    test,
    target_os = "linux",
    feature = "gstreamer-runtime",
    feature = "mpv-runtime"
))]
mod native_surface_tests;
