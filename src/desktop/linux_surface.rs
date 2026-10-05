use std::cell::RefCell;

use gtk::prelude::*;
use tauri::{AppHandle, Manager, Runtime};
use webkit2gtk::WebViewExt;

use crate::{Error, Result};

thread_local! {
    static HOST: RefCell<Option<SurfaceHost>> = const { RefCell::new(None) };
}

struct SurfaceHost {
    fixed: gtk::Fixed,
    backing: Option<(webkit2gtk::WebView, gtk::gdk::RGBA)>,
}

pub fn ensure_host<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    HOST.with(|slot| {
        if let Some(host) = slot.borrow().as_ref() {
            if let Some((webview, _)) = &host.backing {
                webview.set_background_color(&gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));
            }
            return Ok(());
        }
        let window = app
            .webview_windows()
            .into_values()
            .next()
            .ok_or_else(|| Error::Pipeline("no Tauri webview window is available".into()))?;
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
        let webview = find_webview(&child)
            .ok_or_else(|| Error::Pipeline("Tauri GTK child has no WebKit view".into()))?;
        let original_background = webview.background_color();
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
        // Tauri keeps a realized WebView. Reparenting it after realization
        // can leave the new GTK hierarchy at 1x1 until an external resize.
        // Allocate the new content immediately and follow future allocations.
        let content = overlay.clone();
        gtk_window.connect_size_allocate(move |_, allocation| {
            content.size_allocate(&gtk::Allocation::new(
                0,
                0,
                allocation.width(),
                allocation.height(),
            ));
        });
        gtk_window.show_all();
        overlay.size_allocate(&gtk::Allocation::new(
            0,
            0,
            gtk_window.allocated_width(),
            gtk_window.allocated_height(),
        ));
        // Only native playback needs transparent WebKit backing. Preserve
        // the consumer's actual color for ordinary screens after close.
        webview.set_background_color(&gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));
        *slot.borrow_mut() = Some(SurfaceHost {
            fixed,
            backing: Some((webview, original_background)),
        });
        Ok(())
    })
}

fn find_webview(widget: &gtk::Widget) -> Option<webkit2gtk::WebView> {
    if let Ok(webview) = widget.clone().downcast::<webkit2gtk::WebView>() {
        return Some(webview);
    }
    widget
        .downcast_ref::<gtk::Container>()?
        .children()
        .iter()
        .find_map(find_webview)
}

pub fn restore_backing() {
    HOST.with(|slot| {
        if let Some(host) = slot.borrow().as_ref() {
            if let Some((webview, background)) = &host.backing {
                webview.set_background_color(background);
            }
        }
    });
}

pub fn place_widget(widget: &gtk::Widget, x: f64, y: f64, width: f64, height: f64) -> Result<()> {
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
        // Native surfaces need the same allocation even while their parent
        // awaits GTK's next layout pass (including a paused GL frame).
        widget.size_allocate(&gtk::Allocation::new(x, y, width, height));
        Ok(())
    })
}

#[cfg(test)]
pub(super) fn install_qualification_host(fixed: gtk::Fixed) {
    HOST.with(|slot| {
        *slot.borrow_mut() = Some(SurfaceHost {
            fixed,
            backing: None,
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display; run with xvfb-run or a real desktop"]
    fn layout_preserves_the_engine_owned_visibility_gate() {
        gtk::init().unwrap();
        let fixed = gtk::Fixed::new();
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.add(&fixed);
        window.show_all();
        install_qualification_host(fixed);
        let widget: gtk::Widget = gtk::DrawingArea::new().upcast();
        assert!(!widget.is_visible());
        place_widget(&widget, 0.0, 0.0, 320.0, 180.0).unwrap();
        assert!(!widget.is_visible(), "layout revealed an undecoded source");
        widget.show();
        place_widget(&widget, 0.0, 0.0, 640.0, 360.0).unwrap();
        assert!(widget.is_visible());
        widget.hide();
        place_widget(&widget, 0.0, 0.0, 1920.0, 1080.0).unwrap();
        assert!(!widget.is_visible(), "fullscreen revealed a retired frame");
    }
}
