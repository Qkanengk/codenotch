//! Native input regions avoid polling a stale global pointer under XWayland.
use std::sync::atomic::{AtomicBool, Ordering};
use std::{cell::RefCell, rc::Rc, time::Duration};

use gtk::{glib, prelude::*};
use tauri::{AppHandle, Manager, WebviewWindow};

const LEAVE_GRACE: Duration = Duration::from_millis(250);
type PendingLeave = Rc<RefCell<Option<glib::SourceId>>>;

const HYPRLAND_POLL: Duration = Duration::from_millis(100);
static WATCHING: AtomicBool = AtomicBool::new(false);

/// Polls of the cursor being elsewhere that add up to the grace a real leave gets.
fn misses_for_leave() -> u32 {
    (LEAVE_GRACE.as_millis() / HYPRLAND_POLL.as_millis()).max(1) as u32 + 1
}

/// Hyprland keeps Xwayland's pointer where it last was once the cursor moves on to a Wayland
/// window, so no leave event comes and the card would stay open. From an enter until the cursor is
/// off the notch, Hyprland is asked where it is instead. Does nothing on any other desktop, nor
/// for the notch as a layer surface, which is told when the pointer leaves like any Wayland client.
fn watch_for_leave_on_hyprland(window: &WebviewWindow) {
    if crate::layer_shell::active()
        || crate::hyprland::cursor_in_notch((1.0, 1.0)).is_none()
        || WATCHING.swap(true, Ordering::SeqCst)
    {
        return;
    }
    let window = window.clone();
    std::thread::spawn(move || {
        let mut misses = 0;
        while misses < misses_for_leave() {
            std::thread::sleep(HYPRLAND_POLL);
            let Ok(size) = window.outer_size() else { break };
            let size = (size.width as f64, size.height as f64);
            let Some((x, y)) = crate::hyprland::cursor_in_notch(size) else { break };
            let rects = crate::HOT.lock().unwrap().clone();
            misses = if crate::cursor_in_hot(&rects, x, y, Some(size)) { 0 } else { misses + 1 };
        }
        WATCHING.store(false, Ordering::SeqCst);
        if misses >= misses_for_leave() {
            let _ = window.eval("window.codenotchPointer(false)");
        }
    });
}

fn cancel_leave(pending: &PendingLeave) {
    if let Some(source) = pending.borrow_mut().take() {
        source.remove();
    }
}

/// Install once, with the GTK window's lifecycle, on the main thread.
pub fn install(window: &WebviewWindow) -> tauri::Result<()> {
    let gtk_window = window.gtk_window()?;
    let pending: PendingLeave = Rc::new(RefCell::new(None));
    gtk_window.add_events(gtk::gdk::EventMask::ENTER_NOTIFY_MASK | gtk::gdk::EventMask::LEAVE_NOTIFY_MASK);

    let leave_window = window.clone();
    let leave_pending = pending.clone();
    gtk_window.connect_leave_notify_event(move |_, event| {
        if event.detail() != gtk::gdk::NotifyType::Inferior {
            cancel_leave(&leave_pending);
            let window = leave_window.clone();
            let pending = leave_pending.clone();
            *leave_pending.borrow_mut() = Some(glib::timeout_add_local_once(LEAVE_GRACE, move || {
                pending.borrow_mut().take();
                let _ = window.eval("window.codenotchPointer(false)");
            }));
        }
        glib::Propagation::Proceed
    });

    let enter_window = window.clone();
    let enter_pending = pending.clone();
    gtk_window.connect_enter_notify_event(move |_, event| {
        if event.detail() != gtk::gdk::NotifyType::Inferior {
            cancel_leave(&enter_pending);
            let _ = enter_window.eval("window.codenotchPointer(true)");
            watch_for_leave_on_hyprland(&enter_window);
        }
        glib::Propagation::Proceed
    });
    gtk_window.connect_destroy(move |_| cancel_leave(&pending));
    Ok(())
}

/// `rects` are physical pixels; GDK input regions use logical coordinates.
pub fn set_hot(app: &AppHandle, rects: Vec<[f64; 4]>) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(window) = handle.get_webview_window("notch") else { return };
        let Ok(gtk_window) = window.gtk_window() else { return };
        let Some(gdk_window) = gtk_window.window() else { return };
        let scale = gtk_window.scale_factor().max(1) as f64;
        let region = gtk::cairo::Region::create();
        for [x, y, width, height] in rects {
            if ![x, y, width, height].iter().all(|n| n.is_finite()) || width <= 0.0 || height <= 0.0 {
                continue;
            }
            let left = (x / scale).floor() as i32;
            let top = (y / scale).floor() as i32;
            let right = ((x + width) / scale).ceil() as i32;
            let bottom = ((y + height) / scale).ceil() as i32;
            let _ = region.union_rectangle(&gtk::cairo::RectangleInt::new(left, top, right - left, bottom - top));
        }
        gdk_window.input_shape_combine_region(&region, 0, 0);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leave_seen_through_hyprland_waits_at_least_the_grace() {
        assert!(HYPRLAND_POLL * misses_for_leave() >= LEAVE_GRACE);
        assert!(HYPRLAND_POLL * misses_for_leave() <= LEAVE_GRACE * 2);
    }
}
