//! The notch as a layer surface (wlr-layer-shell) on Hyprland, where it is a native Wayland client.
//!
//! A layer surface is anchored to a screen edge by the compositor: it is never tiled, bordered,
//! blurred or tied to a workspace, so it needs no window rule, and it is laid out in the
//! compositor's own logical pixels, so a fractional scale places and sizes it correctly.
//!
//! `libgtk-layer-shell` is opened at run time rather than linked: a desktop without it keeps the
//! X11 path, and the build needs no extra development package.
use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use gtk::glib::translate::ToGlibPtr;
use gtk::prelude::*;
use libloading::Library;

/// Set to keep the notch an X11 window under XWayland, as it is on every other desktop.
pub const FORCE_XWAYLAND: &str = "CODENOTCH_XWAYLAND";
const NAMESPACE: &str = "codenotch";
const CARD_NAMESPACE: &str = "codenotch-card";
const CARRY_NAMESPACE: &str = "codenotch-carry";
const LIBRARY: &str = "libgtk-layer-shell.so.0";

// GtkLayerShellEdge, in the library's order
const LEFT: usize = 0;
const RIGHT: usize = 1;
const TOP: usize = 2;
const BOTTOM: usize = 3;
// GTK_LAYER_SHELL_LAYER_OVERLAY: above full-screen windows too; folding for those is the page's job
const LAYER_OVERLAY: c_int = 3;
// GTK_LAYER_SHELL_KEYBOARD_MODE_NONE: the notch is pointer only and must never take the keyboard
const KEYBOARD_NONE: c_int = 0;

// An exclusive zone of -1: margins are from the output's own edges, not from where its bars end.
// The notch keeps clear of the bars itself (`as_hyprland_has_it`), so both sides agree on the place.
const WHOLE_OUTPUT: c_int = -1;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static DRAGGED: AtomicBool = AtomicBool::new(false);
// What the carry's drag says it holds: nothing any other program asks for
const DRAG_TARGET: &str = "application/x-codenotch-carry";

type Window = *mut gtk::ffi::GtkWindow;

struct Api {
    is_supported: unsafe extern "C" fn() -> c_int,
    init_for_window: unsafe extern "C" fn(Window),
    set_namespace: unsafe extern "C" fn(Window, *const c_char),
    set_layer: unsafe extern "C" fn(Window, c_int),
    set_keyboard_mode: unsafe extern "C" fn(Window, c_int),
    set_exclusive_zone: unsafe extern "C" fn(Window, c_int),
    set_monitor: unsafe extern "C" fn(Window, *mut c_void),
    set_anchor: unsafe extern "C" fn(Window, c_int, c_int),
    set_margin: unsafe extern "C" fn(Window, c_int, c_int),
    _library: Library,
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let library = Library::new(LIBRARY).ok()?;
        macro_rules! symbol {
            ($name:literal) => {
                *library.get(concat!($name, "\0").as_bytes()).ok()?
            };
        }
        Some(Api {
            is_supported: symbol!("gtk_layer_is_supported"),
            init_for_window: symbol!("gtk_layer_init_for_window"),
            set_namespace: symbol!("gtk_layer_set_namespace"),
            set_layer: symbol!("gtk_layer_set_layer"),
            set_keyboard_mode: symbol!("gtk_layer_set_keyboard_mode"),
            set_exclusive_zone: symbol!("gtk_layer_set_exclusive_zone"),
            set_monitor: symbol!("gtk_layer_set_monitor"),
            set_anchor: symbol!("gtk_layer_set_anchor"),
            set_margin: symbol!("gtk_layer_set_margin"),
            _library: library,
        })
    })
    .as_ref()
}

/// A Hyprland session with a Wayland socket, and nobody asked for XWayland.
fn session_wants_it(get: impl Fn(&str) -> Option<String>) -> bool {
    let set = |name: &str| get(name).is_some_and(|v| !v.is_empty());
    set("HYPRLAND_INSTANCE_SIGNATURE") && set("WAYLAND_DISPLAY") && !crate::linux_env::truthy(get(FORCE_XWAYLAND))
}

/// Decided before GTK starts, because it picks GDK's backend: the notch can only be a layer
/// surface as a Wayland client, and only with the library there.
pub fn wanted() -> bool {
    session_wants_it(|name| std::env::var(name).ok()) && api().is_some()
}

/// The notch is a layer surface: it is placed with `place`, not with a window position.
pub fn active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

/// Before the window is first shown; a mapped window cannot change role. Leaves an ordinary
/// window, and says why, when the compositor has no layer shell after all.
pub fn install(window: &gtk::ApplicationWindow) -> Result<(), String> {
    adopt(window, NAMESPACE)?;
    // A drag nobody takes "fails": that is every carry's ending, and GTK's flight of the icon back
    // to where it was picked up would hold the ending back
    window.connect_drag_failed(|_, _, _| {
        DRAGGED.store(false, Ordering::SeqCst);
        gtk::glib::Propagation::Stop
    });
    window.connect_drag_end(|_, _| DRAGGED.store(false, Ordering::SeqCst));
    ACTIVE.store(true, Ordering::SeqCst);
    Ok(())
}

/// A card of the notch's, which has to be a layer surface as well: Wayland lets no ordinary window
/// choose where it goes, so Hyprland would tile it. Only while the notch is one.
pub fn install_card(window: &gtk::ApplicationWindow) -> bool {
    active() && adopt(window, CARD_NAMESPACE).is_ok()
}

/// The drawing of the notch in the hand, over its screen's work area.
pub fn install_overlay(window: &gtk::ApplicationWindow) -> bool {
    if !active() || adopt(window, CARRY_NAMESPACE).is_err() {
        return false;
    }
    // No part of it takes the pointer, said before it is first shown and kept by GTK through
    // every resize
    window.input_shape_combine_region(Some(&gtk::cairo::Region::create()));
    true
}

fn adopt(window: &gtk::ApplicationWindow, namespace: &str) -> Result<(), String> {
    let api = api().ok_or_else(|| format!("{LIBRARY} not found"))?;
    if unsafe { (api.is_supported)() } == 0 {
        return Err("not a Wayland session with wlr-layer-shell".into());
    }
    let namespace = CString::new(namespace).unwrap();
    let raw: Window = window.upcast_ref::<gtk::Window>().to_glib_none().0;
    unsafe {
        (api.init_for_window)(raw);
        (api.set_namespace)(raw, namespace.as_ptr());
        (api.set_layer)(raw, LAYER_OVERLAY);
        (api.set_keyboard_mode)(raw, KEYBOARD_NONE);
        (api.set_exclusive_zone)(raw, WHOLE_OUTPUT);
    }
    Ok(())
}

/// Which edges the surface is anchored to and how far in from them, in logical pixels.
#[derive(Debug, PartialEq)]
pub struct Placement {
    pub anchors: [bool; 4],
    pub margins: [i32; 4],
}

/// The notch sits on `edge` of a screen `screen` wide and high, its top-left corner at `offset`
/// inside it. It is anchored to that edge, so a surface a pixel off the size asked for is still
/// flush with it, and kept `offset` away from the other edges by margins: a bar on the same edge
/// is room the notch leaves alone.
pub fn placement(edge: &str, offset: (i32, i32), size: (i32, i32), screen: (i32, i32)) -> Placement {
    let mut anchors = [false; 4];
    let mut margins = [0; 4];
    let (x, y) = (offset.0.max(0), offset.1.max(0));
    match edge {
        "left" | "top" | "bottom" => (anchors[LEFT], margins[LEFT]) = (true, x),
        _ => (anchors[RIGHT], margins[RIGHT]) = (true, (screen.0 - x - size.0).max(0)),
    }
    match edge {
        "bottom" => (anchors[BOTTOM], margins[BOTTOM]) = (true, (screen.1 - y - size.1).max(0)),
        _ => (anchors[TOP], margins[TOP]) = (true, y),
    }
    Placement { anchors, margins }
}

/// A surface whose top-left corner is `offset` inside the screen, wherever the notch is.
pub fn corner(offset: (i32, i32)) -> Placement {
    let mut anchors = [false; 4];
    let mut margins = [0; 4];
    (anchors[LEFT], anchors[TOP]) = (true, true);
    (margins[LEFT], margins[TOP]) = (offset.0.max(0), offset.1.max(0));
    Placement { anchors, margins }
}

/// The GDK monitor at this logical position, for an output other than the one the compositor
/// would pick itself.
fn monitor_at(origin: (i32, i32)) -> Option<gtk::gdk::Monitor> {
    let display = gtk::gdk::Display::default()?;
    (0..display.n_monitors()).filter_map(|i| display.monitor(i)).find(|m| {
        let g = m.geometry();
        (g.x(), g.y()) == origin
    })
}

/// Size and place the surface. Everything is in logical pixels; `screen` is the output's origin.
pub fn place(window: &gtk::ApplicationWindow, screen: (i32, i32), size: (i32, i32), to: &Placement) {
    let Some(api) = api().filter(|_| active()) else { return };
    // A layer surface takes its size from the widget's request; resizing the window does nothing
    window.set_size_request(size.0, size.1);
    let raw: Window = window.upcast_ref::<gtk::Window>().to_glib_none().0;
    unsafe {
        if let Some(monitor) = monitor_at(screen) {
            let monitor: *mut gtk::gdk::ffi::GdkMonitor = monitor.to_glib_none().0;
            (api.set_monitor)(raw, monitor.cast());
        }
        for edge in [LEFT, RIGHT, TOP, BOTTOM] {
            (api.set_anchor)(raw, edge as c_int, to.anchors[edge] as c_int);
            (api.set_margin)(raw, edge as c_int, to.margins[edge]);
        }
    }
}

/// Where the pointer is inside the notch, in logical pixels. A menu on Wayland has to be a popup
/// of the surface under the pointer; asked to open "at the pointer" it becomes a stray window.
pub fn pointer_in(window: &gtk::ApplicationWindow) -> Option<(i32, i32)> {
    if !active() || !gtk::is_initialized_main_thread() {
        return None;
    }
    let surface = window.window()?;
    let pointer = surface.display().default_seat()?.pointer()?;
    let (_, x, y, _) = surface.device_position(&pointer);
    Some((x, y))
}

/// The press on the notch handed to Hyprland as a drag, which is how a Wayland client goes on
/// hearing of a held button once the pointer is off its own surface. A plain press is not enough:
/// Hyprland keeps it with the notch only while some window has the keyboard, so on an empty
/// workspace it ended the moment the pointer left the notch. Nothing takes the drop; the drag is
/// there to be told when the button comes up, on whichever screen.
pub fn begin_drag(window: &gtk::ApplicationWindow) -> bool {
    if !active() || !gtk::is_initialized_main_thread() {
        return false;
    }
    let targets = gtk::TargetList::new(&[gtk::TargetEntry::new(DRAG_TARGET, gtk::TargetFlags::SAME_APP, 0)]);
    let Some(drag) = window.drag_begin_with_coordinates(&targets, gtk::gdk::DragAction::MOVE, 1, None, -1, -1) else {
        return false;
    };
    // The overlay draws the notch in the hand; GTK's own icon beside the pointer would be a second one
    if let Ok(blank) = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, 1, 1) {
        drag.drag_set_icon_surface(&blank);
    }
    DRAGGED.store(true, Ordering::SeqCst);
    true
}

/// Whether the drag `begin_drag` began is still on: the button is still down.
pub fn dragged() -> bool {
    DRAGGED.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
    }

    #[test]
    fn only_a_hyprland_wayland_session_gets_a_layer_surface() {
        let hyprland = [("HYPRLAND_INSTANCE_SIGNATURE", "abc"), ("WAYLAND_DISPLAY", "wayland-1")];
        assert!(session_wants_it(env(&hyprland)));
        assert!(!session_wants_it(env(&[("WAYLAND_DISPLAY", "wayland-0")])), "GNOME and KDE stay on X11");
        assert!(!session_wants_it(env(&[("HYPRLAND_INSTANCE_SIGNATURE", "abc")])));
        assert!(!session_wants_it(env(&[("HYPRLAND_INSTANCE_SIGNATURE", ""), ("WAYLAND_DISPLAY", "wayland-1")])));
    }

    #[test]
    fn xwayland_can_be_asked_for() {
        let mut vars = vec![("HYPRLAND_INSTANCE_SIGNATURE", "abc"), ("WAYLAND_DISPLAY", "wayland-1")];
        vars.push((FORCE_XWAYLAND, "1"));
        assert!(!session_wants_it(env(&vars)));
        vars.pop();
        vars.push((FORCE_XWAYLAND, "0"));
        assert!(session_wants_it(env(&vars)));
    }

    #[test]
    fn each_edge_anchors_the_notch_and_keeps_its_place_with_margins() {
        let (size, screen) = ((360, 650), (1920, 1080));
        let at = |edge, offset| placement(edge, offset, size, screen);
        assert_eq!(at("right", (1560, 215)), Placement { anchors: [false, true, true, false], margins: [0, 0, 215, 0] });
        assert_eq!(at("left", (0, 215)), Placement { anchors: [true, false, true, false], margins: [0, 0, 215, 0] });
        assert_eq!(at("top", (780, 0)), Placement { anchors: [true, false, true, false], margins: [780, 0, 0, 0] });
        assert_eq!(at("bottom", (780, 430)), Placement { anchors: [true, false, false, true], margins: [780, 0, 0, 0] });
        assert_eq!(at("nonsense", (1560, 215)), at("right", (1560, 215)), "an unknown edge is the right one");
    }

    #[test]
    fn a_bar_on_the_notchs_edge_is_left_alone() {
        let (size, screen) = ((360, 650), (1920, 1080));
        let at = |edge, offset| placement(edge, offset, size, screen).margins;
        assert_eq!(at("right", (1520, 215)), [0, 40, 215, 0]);
        assert_eq!(at("left", (40, 215)), [40, 0, 215, 0]);
        assert_eq!(at("top", (780, 30)), [780, 0, 30, 0]);
        assert_eq!(at("bottom", (780, 400)), [780, 0, 0, 30]);
    }

    #[test]
    fn a_place_off_the_screen_is_no_margin() {
        assert_eq!(placement("right", (1600, -9), (360, 650), (1920, 1080)).margins, [0; 4]);
        assert_eq!(placement("bottom", (-4, 500), (360, 650), (1920, 1080)).margins, [0; 4]);
    }

    #[test]
    fn a_card_is_placed_by_its_corner() {
        assert_eq!(corner((1290, 465)), Placement { anchors: [true, false, true, false], margins: [1290, 0, 465, 0] });
        assert_eq!(corner((-3, -3)).margins, [0; 4]);
    }

    #[test]
    fn nothing_is_a_layer_surface_until_one_is_installed() {
        assert!(!active());
    }
}
