use std::cell::RefCell;

use gtk::prelude::*;
use tauri::{AppHandle, Manager, Runtime};

use crate::{Error, Result};

thread_local! {
    static HOST: RefCell<Option<SurfaceHost>> = const { RefCell::new(None) };
}

struct SurfaceHost {
    fixed: gtk::Fixed,
}

pub fn ensure_host<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    HOST.with(|slot| {
        if slot.borrow().is_some() {
            return Ok(());
        }
        let window =
            app.webview_windows().into_values().next().ok_or_else(|| {
                Error::Pipeline("no Tauri webview window is available".into())
            })?;
        // Native video is a sibling below WebKit, so only the WebView's
        // backing layer must be transparent. Do this at runtime instead of
        // requiring every consuming app to opt its whole OS window into
        // transparency in tauri.conf.json.
        window
            .as_ref()
            .set_background_color(Some(tauri::webview::Color(0, 0, 0, 0)))
            .map_err(|error| Error::Pipeline(error.to_string()))?;
        let gtk_window = window
            .gtk_window()
            .map_err(|error| Error::Pipeline(error.to_string()))?;
        let background_style = gtk::CssProvider::new();
        background_style
            .load_from_data(b".tauri-video-window { background-color: #000; }")
            .map_err(|error| Error::Pipeline(error.to_string()))?;
        let context = gtk_window.style_context();
        context.add_class("tauri-video-window");
        context.add_provider(&background_style, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let child = gtk_window
            .child()
            .ok_or_else(|| Error::Pipeline("Tauri GTK window has no webview child".into()))?;
        gtk_window.remove(&child);

        let overlay = gtk::Overlay::new();
        overlay.set_hexpand(true);
        overlay.set_vexpand(true);
        let opaque_base = gtk::DrawingArea::new();
        opaque_base.set_hexpand(true);
        opaque_base.set_vexpand(true);
        opaque_base.connect_draw(|_, context| {
            context.set_source_rgb(0.0, 0.0, 0.0);
            let _ = context.paint();
            // This widget is the compositor-safe opaque floor beneath the
            // native video and transparent WebView. Letting GTK continue
            // into its default DrawingArea handler can clear our paint back
            // to transparent on Wayland compositors such as COSMIC.
            gtk::glib::Propagation::Stop
        });
        let fixed = gtk::Fixed::new();
        fixed.set_hexpand(true);
        fixed.set_vexpand(true);
        child.set_hexpand(true);
        child.set_vexpand(true);
        overlay.add(&opaque_base);
        overlay.add_overlay(&fixed);
        overlay.add_overlay(&child);
        gtk_window.add(&overlay);
        gtk_window.show_all();
        *slot.borrow_mut() = Some(SurfaceHost { fixed });
        Ok(())
    })
}

pub fn place_widget(
    widget: &gtk::Widget,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<()> {
    // Preserve negative positions. GtkFixed clips children against the
    // window, which is exactly what we need when an HTML anchor scrolls
    // partially above or left of the viewport. Clamping here pins the
    // native video to the edge while the DOM continues moving.
    let x = x.round() as i32;
    let y = y.round() as i32;
    let width = width.max(1.0).round() as i32;
    let height = height.max(1.0).round() as i32;
    HOST.with(|host| {
        let host = host.borrow();
        let host = host
            .as_ref()
            .ok_or_else(|| Error::Pipeline("native surface host is unavailable".into()))?;
        if widget.parent().is_none() {
            host.fixed.put(widget, x, y);
        } else {
            host.fixed.move_(widget, x, y);
        }
        widget.set_size_request(width, height);
        widget.queue_resize();
        host.fixed.queue_resize();
        host.fixed.queue_draw();
        if !widget.is_visible() {
            widget.show();
        }
        Ok(())
    })
}
