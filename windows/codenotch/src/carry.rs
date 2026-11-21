//! Carrying the notch round the screen's border, as the Mac's ⌥-drag does since 1.19.0
//! (`NotchWindowController.travel`, `BorderTrack` in `CornerPassage.swift`).
//!
//! The work area's border is one line, measured clockwise from its top-left corner, and the notch's
//! place on it is where its middle is. In the hand the notch is not its own window: moving a window
//! every frame is a stutter the screen does not keep time with, so `ui/carry.html` draws it on an
//! overlay over the whole work area, following the pointer's place on a spring, while the notch's
//! page empties itself. Let go, it comes to rest, and the notch window is put down exactly there.

use std::sync::atomic::{AtomicU32, Ordering::SeqCst};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Top,
    Right,
    Bottom,
    Left,
}

impl Edge {
    const ALL: [Edge; 4] = [Edge::Top, Edge::Right, Edge::Bottom, Edge::Left];

    pub fn parse(s: &str) -> Edge {
        match s {
            "top" => Edge::Top,
            "bottom" => Edge::Bottom,
            "left" => Edge::Left,
            _ => Edge::Right,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Edge::Top => "top",
            Edge::Right => "right",
            Edge::Bottom => "bottom",
            Edge::Left => "left",
        }
    }

    pub fn is_vertical(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corner {
    TopRight,
    BottomRight,
    BottomLeft,
    TopLeft,
}

impl Corner {
    const ALL: [Corner; 4] = [Corner::TopRight, Corner::BottomRight, Corner::BottomLeft, Corner::TopLeft];

    /// The edges either side of it, in the order the border runs.
    pub fn edges(self) -> (Edge, Edge) {
        match self {
            Corner::TopRight => (Edge::Top, Edge::Right),
            Corner::BottomRight => (Edge::Right, Edge::Bottom),
            Corner::BottomLeft => (Edge::Bottom, Edge::Left),
            Corner::TopLeft => (Edge::Left, Edge::Top),
        }
    }
}

/// The border as one line, in the work area's own top-left-origin logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderTrack {
    pub width: f64,
    pub height: f64,
}

impl BorderTrack {
    pub fn perimeter(&self) -> f64 {
        2.0 * (self.width + self.height)
    }

    pub fn corner_position(&self, corner: Corner) -> f64 {
        match corner {
            Corner::TopRight => self.width,
            Corner::BottomRight => self.width + self.height,
            Corner::BottomLeft => 2.0 * self.width + self.height,
            Corner::TopLeft => 0.0,
        }
    }

    /// The border position of a point on `edge`.
    pub fn position_on(&self, edge: Edge, x: f64, y: f64) -> f64 {
        let (w, h) = (self.width, self.height);
        match edge {
            Edge::Top => x.clamp(0.0, w),
            Edge::Right => w + y.clamp(0.0, h),
            Edge::Bottom => 2.0 * w + h - x.clamp(0.0, w),
            Edge::Left => self.wrapped(2.0 * w + 2.0 * h - y.clamp(0.0, h)),
        }
    }

    /// The edge a border position is on, and where along it: x on a flat edge, y on an upright one.
    pub fn place_at(&self, position: f64) -> (Edge, f64) {
        let (w, h) = (self.width, self.height);
        let s = self.wrapped(position);
        if s < w {
            (Edge::Top, s)
        } else if s < w + h {
            (Edge::Right, s - w)
        } else if s < 2.0 * w + h {
            (Edge::Bottom, 2.0 * w + h - s)
        } else {
            (Edge::Left, 2.0 * w + 2.0 * h - s)
        }
    }

    pub fn wrapped(&self, position: f64) -> f64 {
        position.rem_euclid(self.perimeter())
    }

    /// The shorter way round from one place to another.
    pub fn signed(&self, d: f64) -> f64 {
        let p = self.perimeter();
        let d = d.rem_euclid(p);
        if d > p / 2.0 {
            d - p
        } else {
            d
        }
    }
}

/// How much nearer another edge the pointer has to be before the notch goes round onto it — enough
/// that a pointer near a corner does not send it back and forth between the two.
const EDGE_SWITCH_MARGIN: f64 = 40.0;
/// Within this of both edges at a corner, the pointer's place is read from both at once.
const CORNER_REACH: f64 = 160.0;

/// How far a point is from `edge`, into the screen.
fn distance_from(edge: Edge, x: f64, y: f64, track: &BorderTrack) -> f64 {
    match edge {
        Edge::Top => y,
        Edge::Bottom => track.height - y,
        Edge::Left => x,
        Edge::Right => track.width - x,
    }
}

/// The edge the notch belongs on for a pointer at (x, y): the one it is on, unless another is nearer
/// by more than `EDGE_SWITCH_MARGIN`.
pub fn sticky_edge(current: Edge, x: f64, y: f64, track: &BorderTrack) -> Edge {
    let nearest = Edge::ALL
        .into_iter()
        .min_by(|a, b| distance_from(*a, x, y, track).total_cmp(&distance_from(*b, x, y, track)))
        .unwrap_or(current);
    if distance_from(nearest, x, y, track) + EDGE_SWITCH_MARGIN < distance_from(current, x, y, track) {
        nearest
    } else {
        current
    }
}

/// How the pointer's place on the border is read: along the edge it is nearest, or near a corner
/// from both edges at once. Read from one edge only, the pointer stopped by the screen's corner moved
/// nothing until it was well down the next edge, and the notch sat pinned in the corner the while.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    Edge(Edge),
    Corner(Corner),
}

pub fn reading(x: f64, y: f64, track: &BorderTrack, nearest: Edge) -> Reading {
    Corner::ALL
        .into_iter()
        .find(|c| {
            let (before, after) = c.edges();
            distance_from(before, x, y, track).max(0.0) < CORNER_REACH
                && distance_from(after, x, y, track).max(0.0) < CORNER_REACH
        })
        .map_or(Reading::Edge(nearest), Reading::Corner)
}

/// The pointer's place on the border as `reading` reads it. At a corner: how far it is from the
/// edge before the corner, less how far from the edge after, so moving along either one moves it round.
pub fn place(x: f64, y: f64, track: &BorderTrack, reading: Reading) -> f64 {
    match reading {
        Reading::Edge(edge) => track.position_on(edge, x, y),
        Reading::Corner(corner) => {
            let (before, after) = corner.edges();
            track.wrapped(
                track.corner_position(corner) + distance_from(before, x, y, track).max(0.0)
                    - distance_from(after, x, y, track).max(0.0),
            )
        }
    }
}

/// The notch in the hand: the edge the pointer was last read from, and how far along the border the
/// notch's middle is from the pointer, as it was taken hold of.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hold {
    pub edge: Edge,
    pub grip: f64,
}

impl Hold {
    /// A notch on `from` with its middle at `middle`, taken hold of at (x, y).
    pub fn take(from: Edge, middle: f64, x: f64, y: f64, track: &BorderTrack) -> Hold {
        Hold { edge: from, grip: track.signed(middle - place(x, y, track, reading(x, y, track, from))) }
    }

    /// Where the notch's middle goes for a pointer at (x, y): the pointer's own place on the border,
    /// held where it was taken hold of. Nothing of the way the pointer came is kept: a pointer taken
    /// straight across the screen changes edge far from any corner, and a notch that kept its place
    /// through that change stayed that far from the pointer and was put down that far from it.
    pub fn target(&mut self, x: f64, y: f64, track: &BorderTrack) -> f64 {
        self.edge = sticky_edge(self.edge, x, y, track);
        track.wrapped(place(x, y, track, reading(x, y, track, self.edge)) + self.grip)
    }
}

// ---------------- the carry ----------------

pub const LABEL: &str = "carry";

/// Everything the overlay needs to draw the notch: its own size, where the notch is, and the
/// notch's shape, which the page measured and Rust passes on untouched.
#[derive(Clone, serde::Serialize)]
pub struct Init {
    seq: u32,
    w: f64,
    h: f64,
    place: f64,
    /// The edge it was picked up from, whose order its rings keep all the way round.
    edge: &'static str,
    shape: serde_json::Value,
    size: f64,
    /// How near an end of an edge the notch's middle can be put down: its window is kept wholly in
    /// the work area, and that window is longer than the notch.
    reach: f64,
}

static CURRENT: Mutex<Option<Init>> = Mutex::new(None);
static SEQ: AtomicU32 = AtomicU32::new(0);
/// The carry whose first frame the overlay has drawn, whose notch page has shown itself again after
/// landing, and where the drawing came to rest.
static SHOWN: AtomicU32 = AtomicU32::new(0);
static REVEALED: AtomicU32 = AtomicU32::new(0);
static LANDED: Mutex<Option<(u32, f64)>> = Mutex::new(None);

fn track_of(s: &crate::Screen) -> BorderTrack {
    let (_, _, aw, ah) = s.area();
    BorderTrack { width: aw as f64 / s.scale, height: ah as f64 / s.scale }
}

fn local(s: &crate::Screen, x: f64, y: f64) -> (f64, f64) {
    let (ax, ay, _, _) = s.area();
    ((x - ax as f64) / s.scale, (y - ay as f64) / s.scale)
}

/// The pointer in a screen's work area, in logical pixels from its top-left corner.
fn at(s: &crate::Screen, pointer: Pointer) -> (f64, f64) {
    let (x, y) = pointer.on(s);
    local(s, x, y)
}

fn reach_on(s: &crate::Screen, size: f64) -> f64 {
    let (_, _, aw, ah) = s.area();
    let half = crate::NOTCH_LONG * size / 2.0;
    half.min(aw.min(ah) as f64 / s.scale / 2.0)
}

/// How far into another screen the pointer has to go, in that screen's logical pixels, before the
/// notch goes with it. Nothing stops the pointer where two screens meet, as the screen's own edge
/// does, so without this the notch left for the neighbour every time it was carried along the shared
/// edge. The Mac never changes screen mid-carry at all; this keeps Windows' carry across screens
/// for a pointer that is plainly on the other one.
const CROSS: f64 = 150.0;

/// How far a point is outside the screen, in physical pixels: 0 on it.
fn beyond(s: &crate::Screen, x: f64, y: f64) -> f64 {
    let (left, top, right, bottom) = (s.x as f64, s.y as f64, (s.x + s.w) as f64, (s.y + s.h) as f64);
    let dx = (left - x).max(x - right).max(0.0);
    let dy = (top - y).max(y - bottom).max(0.0);
    dx.max(dy)
}

fn wait_for(flag: &AtomicU32, seq: u32, ms: u64) -> bool {
    for _ in 0..ms / 5 {
        if flag.load(SeqCst) == seq {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}

/// Where the pointer is, as the platform reports it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pointer {
    x: f64,
    y: f64,
    /// Hyprland's layout pixels, which GDK scales by each screen's own factor, rather than one
    /// physical grid for every screen as on Windows.
    logical: bool,
}

impl Pointer {
    /// In the pixels `screen` is measured in.
    fn on(&self, screen: &crate::Screen) -> (f64, f64) {
        if self.logical {
            (self.x * screen.scale, self.y * screen.scale)
        } else {
            (self.x, self.y)
        }
    }

    /// The screen it is over. Each is tried in its own pixels: two screens at different scales
    /// overlap in GDK's, where a point alone does not say which one it is on.
    fn over<'a>(&self, all: &'a [crate::Screen]) -> Option<&'a crate::Screen> {
        all.iter().find(|s| {
            let (x, y) = self.on(s);
            crate::screen_at(std::slice::from_ref(*s), x, y).is_some()
        })
    }

    /// How far off `from` it is, in logical pixels: those of `onto`, the screen it is over, where
    /// every screen shares one grid, and `from`'s own where each has its own.
    fn depth(&self, from: &crate::Screen, onto: &crate::Screen) -> f64 {
        let (x, y) = self.on(from);
        beyond(from, x, y) / if self.logical { from.scale } else { onto.scale }
    }
}

fn pointer(app: &AppHandle) -> Option<Pointer> {
    #[cfg(target_os = "linux")]
    if crate::layer_shell::active() {
        return crate::hyprland::cursor().map(|(x, y)| Pointer { x, y, logical: true });
    }
    app.cursor_position().ok().map(|at| Pointer { x: at.x, y: at.y, logical: false })
}

/// Whether the button the notch was taken with is still down.
fn held(app: &AppHandle) -> bool {
    #[cfg(target_os = "linux")]
    if crate::layer_shell::active() {
        return crate::layer_shell::dragged();
    }
    let _ = app;
    crate::left_button_down()
}

/// GTK is only for the main thread, and the carry runs on one of its own.
#[cfg(target_os = "linux")]
fn on_main<T: Send + 'static>(app: &AppHandle, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (done, result) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let _ = done.send(f());
    })
    .ok()?;
    result.recv_timeout(std::time::Duration::from_millis(500)).ok()
}

/// Alt+drag on the pill, or its six dots taken hold of. The page empties once the overlay has the
/// notch drawn; from then on the pointer is followed here and its place on the border sent to the
/// overlay, until the button comes up.
///
/// As an X11 window off Windows nothing tells when the button is let go, so the notch is put
/// straight back down. As a layer surface on Hyprland it is carried: the press becomes a drag
/// (`layer_shell::begin_drag`), which ends when the button comes up, wherever the pointer has gone.
#[tauri::command]
pub fn begin_carry(app: AppHandle, shape: serde_json::Value) {
    if crate::DRAGGING.swap(true, SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        run(&app, shape);
        crate::DRAGGING.store(false, SeqCst);
        let _ = app.emit_to("notch", "drag_end", true);
    });
}

fn run(app: &AppHandle, shape: serde_json::Value) {
    let (Some(start), Some(notch)) = (crate::target_screen(app), app.get_webview_window("notch")) else {
        return;
    };
    let (Some((nx, ny, nw, nh)), Some(cur)) = (crate::notch_rect(&notch), pointer(app)) else {
        return;
    };
    #[cfg(target_os = "linux")]
    if as_layer() {
        let window = notch.clone();
        let begun = move || window.gtk_window().is_ok_and(|w| crate::layer_shell::begin_drag(&w));
        if on_main(app, begun) != Some(true) {
            crate::applog("carry: no drag could be begun from the notch; it stays where it is");
            return;
        }
    }
    let from = Edge::parse(&{
        let st = app.state::<crate::AppState>();
        let c = st.cfg.lock().unwrap();
        crate::config::edge_or_right(&c.notch_edge)
    });
    let size = crate::ui_scale(app);
    let mut mon = start.clone();
    let mut track = track_of(&mon);
    // The notch is centred in its window along the edge, so the window's middle is the notch's
    let (mx, my) = local(&mon, nx as f64 + nw as f64 / 2.0, ny as f64 + nh as f64 / 2.0);
    let middle = track.position_on(from, mx, my);
    let (px, py) = at(&mon, cur);
    let mut hold = Hold::take(from, middle, px, py, &track);

    let seq = SEQ.fetch_add(1, SeqCst) + 1;
    *LANDED.lock().unwrap() = None;
    *CURRENT.lock().unwrap() = Some(Init {
        seq,
        w: track.width,
        h: track.height,
        place: middle,
        edge: from.as_str(),
        shape,
        size,
        reach: reach_on(&mon, size),
    });
    show(app, &mon);
    // Emptied only once the drawing is up under it, so there is no frame with neither
    if !wait_for(&SHOWN, seq, 1500) {
        crate::applog("carry: the overlay never drew; carried without it");
    }
    let _ = app.emit_to("notch", "carry_hold", ());

    let all = crate::screens(app);
    let mut last = middle;
    // Once more after it is let go: the pointer may have moved since it was last read, and the notch
    // is put down where the pointer is then
    let mut going = true;
    while going {
        going = held(app);
        if let Some(cur) = pointer(app) {
            if let Some(s) = cur.over(&all).filter(|s| !crate::same_screen(s, &mon) && cur.depth(&mon, s) >= CROSS) {
                // Onto another screen: there is no border between the two to travel, so it arrives
                // under the pointer on the edge nearest it
                mon = s.clone();
                track = track_of(&mon);
                let (px, py) = at(&mon, cur);
                hold.grip = 0.0;
                last = hold.target(px, py, &track);
                if let Some(init) = CURRENT.lock().unwrap().as_mut() {
                    (init.w, init.h, init.place, init.reach) = (track.width, track.height, last, reach_on(&mon, size));
                }
                relocate(app, &mon);
                continue;
            }
            let (px, py) = at(&mon, cur);
            let target = hold.target(px, py, &track);
            if (target - last).abs() > 0.01 {
                last = target;
                let _ = app.emit_to(LABEL, "carry_target", target);
            }
        }
        if going {
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
    }

    // Let go: the drawing comes to rest on its own and says where
    let _ = app.emit_to(LABEL, "carry_release", ());
    let mut rest = last;
    for _ in 0..400 {
        if let Some((s, at)) = *LANDED.lock().unwrap() {
            if s == seq {
                rest = at;
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    land(app, &start, &mon, &track, from, rest, seq);
}

/// The notch window put down where the drawing came to rest, shown again, and only then the drawing
/// taken away.
fn land(app: &AppHandle, start: &crate::Screen, mon: &crate::Screen, track: &BorderTrack, from: Edge, rest: f64, seq: u32) {
    let (edge, along) = track.place_at(rest);
    // Through `along_at`, which `edge_origin` is tested to read back exactly
    let (ax, ay, aw, ah) = mon.area();
    let from_start = (along * mon.scale).round() as i32;
    let ratio = if edge.is_vertical() {
        crate::along_at(ay + from_start, 0, ay, ah)
    } else {
        crate::along_at(ax + from_start, 0, ax, aw)
    };
    let mut crossed = !crate::same_screen(mon, start);
    // As the move handle's carry does: a screen Windows will not name cannot be remembered, and saving
    // `None` would mean the primary, so the notch keeps to the screen it came from
    let unnameable = crossed && mon.name.is_none();
    if unnameable {
        crossed = false;
    }
    {
        let st = app.state::<crate::AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.notch_edge = edge.as_str().into();
        c.set_along(edge.as_str(), ratio);
        if crossed {
            c.notch_monitor = mon.name.clone();
        }
        crate::config::save(&c);
    }
    crate::applog(&format!("notch carried: {} -> {} at {ratio:.3} on {:?}", from.as_str(), edge.as_str(), mon.name));
    let resized = edge.is_vertical() != from.is_vertical() || (crossed && (mon.scale - start.scale).abs() >= 0.01);
    if resized {
        // The window changes shape or scale, and the page re-zooms for it: shown once that settles
        crate::land_quietly(app);
    } else {
        crate::place_notch(app);
        let _ = app.emit_to("notch", "notch_reveal", ());
    }
    if !wait_for(&REVEALED, seq, 1500) {
        crate::applog("carry: the notch never reported itself shown; the drawing taken away anyway");
    }
    hide(app);
}

fn show(app: &AppHandle, screen: &crate::Screen) {
    if let Some(w) = app.get_webview_window(LABEL) {
        pin(&w, screen);
        let _ = w.emit_to(LABEL, "carry_init", CURRENT.lock().unwrap().clone());
        let _ = w.show();
        ignore_pointer(&w);
        return;
    }
    let (ax, ay, aw, ah) = screen.area();
    let builder = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("carry.html".into()))
        .title("Codenotch carry")
        .position(ax as f64 / screen.scale, ay as f64 / screen.scale)
        .inner_size(aw as f64 / screen.scale, ah as f64 / screen.scale)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .focusable(false)
        .resizable(false)
        // A window becomes a layer surface before it is first shown, or not at all
        .visible(!as_layer())
        .theme(crate::theme_choice(app))
        .initialization_script(crate::theme_script(crate::resolved_theme(app)));
    match builder.build() {
        Ok(w) => {
            #[cfg(target_os = "linux")]
            if as_layer() {
                let window = w.clone();
                let adopted = move || window.gtk_window().is_ok_and(|w| crate::layer_shell::install_overlay(&w));
                if on_main(app, adopted) != Some(true) {
                    crate::applog("carry overlay: not a layer surface; carried without it");
                    let _ = w.destroy();
                    return;
                }
            }
            pin(&w, screen);
            let _ = w.show();
            ignore_pointer(&w);
        }
        Err(e) => crate::applog(&format!("carry overlay: {e}")),
    }
}

/// A layer surface takes no pointer from the moment it is adopted (`install_overlay`), and
/// asking it here as well would leave it one pixel that does.
fn ignore_pointer(w: &tauri::WebviewWindow) {
    if !as_layer() {
        let _ = w.set_ignore_cursor_events(true);
    }
}

/// The drawing is a layer surface wherever the notch is one: Wayland lets no ordinary window say
/// where it goes, so Hyprland would tile it.
fn as_layer() -> bool {
    #[cfg(target_os = "linux")]
    return crate::layer_shell::active();
    #[cfg(not(target_os = "linux"))]
    false
}

/// The output's origin, the work area's size, and the work area's corner inside the output, all in
/// logical pixels: what a layer surface is laid out in.
#[cfg(target_os = "linux")]
fn layer_place(screen: &crate::Screen) -> ((i32, i32), (i32, i32), crate::layer_shell::Placement) {
    let logical = |v: i32| (v as f64 / screen.scale.max(0.01)).round() as i32;
    let (ax, ay, aw, ah) = screen.area();
    let corner = crate::layer_shell::corner((logical(ax - screen.x), logical(ay - screen.y)));
    ((logical(screen.x), logical(screen.y)), (logical(aw.max(1)), logical(ah.max(1))), corner)
}

/// Pinned in physical pixels, as the drop zones are: the builder's logical figures are converted
/// with whichever monitor Windows decides the window belongs to.
fn pin(w: &tauri::WebviewWindow, screen: &crate::Screen) {
    #[cfg(target_os = "linux")]
    if as_layer() {
        let (window, (origin, size, to)) = (w.clone(), layer_place(screen));
        let _ = w.run_on_main_thread(move || {
            if let Ok(window) = window.gtk_window() {
                crate::layer_shell::place(&window, origin, size, &to);
            }
        });
        return;
    }
    let (ax, ay, aw, ah) = screen.area();
    let size = tauri::PhysicalSize::new(aw.max(1) as u32, ah.max(1) as u32);
    for _ in 0..2 {
        let _ = w.set_position(tauri::PhysicalPosition::new(ax, ay));
        let _ = w.set_size(size);
        if w.outer_size().map(|s| s == size).unwrap_or(true) {
            break;
        }
    }
}

/// Onto another screen: hidden while it goes, so the resize Windows makes for a new scale is not seen.
fn relocate(app: &AppHandle, screen: &crate::Screen) {
    let Some(w) = app.get_webview_window(LABEL) else { return show(app, screen) };
    let _ = w.hide();
    pin(&w, screen);
    let _ = w.emit_to(LABEL, "carry_init", CURRENT.lock().unwrap().clone());
    std::thread::sleep(std::time::Duration::from_millis(40));
    let _ = w.show();
    ignore_pointer(&w);
}

fn hide(app: &AppHandle) {
    *CURRENT.lock().unwrap() = None;
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.destroy();
    }
}

/// What the overlay asks for when it loads, in case it started listening after the first push.
#[tauri::command]
pub fn get_carry() -> Option<Init> {
    CURRENT.lock().unwrap().clone()
}

#[tauri::command]
pub fn carry_shown(seq: u32) {
    SHOWN.store(seq, SeqCst);
}

#[tauri::command]
pub fn carry_landed(seq: u32, place: f64) {
    *LANDED.lock().unwrap() = Some((seq, place));
}

/// The notch page has shown itself again after a carry: the drawing can go.
#[tauri::command]
pub fn carry_revealed() {
    REVEALED.store(SEQ.load(SeqCst), SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACK: BorderTrack = BorderTrack { width: 1800.0, height: 1169.0 };

    fn screen(name: &str, x: i32, w: i32, h: i32, scale: f64, top: i32) -> crate::Screen {
        crate::Screen { name: Some(name.into()), x, y: 0, w, h, scale, work: (x, top, w, h - top) }
    }

    #[test]
    fn on_windows_the_pointer_is_in_the_same_pixels_as_every_screen() {
        let all = [screen("1", 0, 2560, 1440, 1.5, 0), screen("2", 2560, 1920, 1080, 1.0, 0)];
        let p = Pointer { x: 2700.0, y: 300.0, logical: false };
        assert_eq!(p.on(&all[0]), (2700.0, 300.0));
        assert_eq!(p.over(&all).and_then(|s| s.name.as_deref()), Some("2"));
        assert_eq!(p.depth(&all[0], &all[1]), 140.0, "in the pixels of the screen it has gone onto");
        assert!(Pointer { x: 2700.0, y: 1200.0, logical: false }.over(&all).is_none(), "below the smaller screen");
    }

    #[test]
    fn on_hyprland_the_pointer_is_scaled_into_each_screens_own_pixels() {
        // A 1.5 output GDK scales by 2, 1280 logical pixels wide, and a 1920 one at scale 1 beside
        // it: in GDK's pixels the first reaches 2560 and the second starts at 1280
        let all = [screen("DP-1", 0, 2560, 1440, 2.0, 80), screen("HDMI-A-1", 1280, 1920, 1080, 1.0, 0)];
        let on_first = Pointer { x: 1000.0, y: 300.0, logical: true };
        assert_eq!(on_first.on(&all[0]), (2000.0, 600.0));
        assert_eq!(on_first.over(&all).and_then(|s| s.name.as_deref()), Some("DP-1"));
        assert_eq!(at(&all[0], on_first), (1000.0, 260.0), "from the work area's corner, under a 40 pixel bar");
        assert_eq!(on_first.depth(&all[0], &all[1]), 0.0);

        let on_second = Pointer { x: 1400.0, y: 300.0, logical: true };
        assert_eq!(on_second.over(&all).and_then(|s| s.name.as_deref()), Some("HDMI-A-1"), "not the first, whose pixels reach 2560");
        assert_eq!(on_second.depth(&all[0], &all[1]), 120.0, "not yet far enough onto it to cross");
        assert_eq!(at(&all[1], on_second), (120.0, 300.0));
        assert!(Pointer { x: 1431.0, y: 300.0, logical: true }.depth(&all[0], &all[1]) >= CROSS);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_drawing_is_laid_over_the_work_area_in_logical_pixels() {
        let (origin, size, to) = layer_place(&screen("DP-1", 0, 2560, 1440, 2.0, 80));
        assert_eq!((origin, size), ((0, 0), (1280, 680)));
        assert_eq!(to, crate::layer_shell::corner((0, 40)), "below the bar");
        let (origin, size, to) = layer_place(&screen("HDMI-A-1", 1280, 1920, 1080, 1.0, 0));
        assert_eq!((origin, size, to), ((1280, 0), (1920, 1080), crate::layer_shell::corner((0, 0))));
    }

    // The Mac's `OptionCarryTests` are in AppKit's bottom-left-origin points; here the same points
    // are in top-left-origin ones, y turned over.
    fn tl(x: f64, y_from_bottom: f64) -> (f64, f64) {
        (x, TRACK.height - y_from_bottom)
    }

    #[test]
    fn how_far_the_pointer_is_from_each_edge() {
        let (x, y) = tl(300.0, 900.0);
        assert_eq!(distance_from(Edge::Top, x, y, &TRACK), 269.0);
        assert_eq!(distance_from(Edge::Bottom, x, y, &TRACK), 900.0);
        assert_eq!(distance_from(Edge::Left, x, y, &TRACK), 300.0);
        assert_eq!(distance_from(Edge::Right, x, y, &TRACK), 1500.0);
    }

    #[test]
    fn it_goes_round_onto_the_nearest_edge() {
        let edge = |current, (x, y): (f64, f64)| sticky_edge(current, x, y, &TRACK);
        assert_eq!(edge(Edge::Top, tl(1200.0, 1150.0)), Edge::Top, "dragged along the top, it stays on the top");
        assert_eq!(edge(Edge::Top, tl(1790.0, 700.0)), Edge::Right, "down the right-hand side, onto the right");
        assert_eq!(edge(Edge::Right, tl(1500.0, 15.0)), Edge::Bottom);
        assert_eq!(edge(Edge::Bottom, tl(10.0, 600.0)), Edge::Left);
        assert_eq!(edge(Edge::Left, tl(900.0, 1160.0)), Edge::Top);
    }

    #[test]
    fn it_does_not_flicker_at_a_corner() {
        let (x, y) = tl(1780.0, 1150.0); // 20 from the right, 19 from the top
        assert_eq!(sticky_edge(Edge::Top, x, y, &TRACK), Edge::Top);
        assert_eq!(sticky_edge(Edge::Right, x, y, &TRACK), Edge::Right);
    }

    #[test]
    fn every_place_is_on_one_edge_and_back() {
        for (edge, x, y) in [(Edge::Top, 400.0, 0.0), (Edge::Right, 1800.0, 300.0), (Edge::Bottom, 700.0, 1169.0), (Edge::Left, 0.0, 900.0)] {
            let (back, along) = TRACK.place_at(TRACK.position_on(edge, x, y));
            assert_eq!(back, edge);
            assert!((along - if edge.is_vertical() { y } else { x }).abs() < 0.001);
        }
        let p = TRACK.perimeter();
        assert!((TRACK.wrapped(-10.0) - (p - 10.0)).abs() < 0.001);
        assert!((TRACK.wrapped(p + 10.0) - 10.0).abs() < 0.001);
    }

    #[test]
    fn near_a_corner_it_is_read_from_both_edges() {
        assert_eq!(reading(1790.0, 20.0, &TRACK, Edge::Top), Reading::Corner(Corner::TopRight));
        assert_eq!(reading(900.0, 10.0, &TRACK, Edge::Top), Reading::Edge(Edge::Top));
    }

    #[test]
    fn it_never_stops_dead_in_the_corner() {
        let read = Reading::Corner(Corner::TopRight);
        let path = (0..=15).map(|i| (1650.0 + 10.0 * i as f64, 5.0)).chain((0..=13).map(|i| (1800.0, 15.0 + 10.0 * i as f64)));
        let mut last = f64::NEG_INFINITY;
        for (x, y) in path {
            let at = place(x, y, &TRACK, read);
            assert!(at > last, "stopped at ({x}, {y})");
            last = at;
        }
    }

    #[test]
    fn the_corner_reading_agrees_with_each_edge() {
        let read = Reading::Corner(Corner::TopRight);
        assert!((place(1700.0, 0.0, &TRACK, read) - TRACK.position_on(Edge::Top, 1700.0, 0.0)).abs() < 0.001);
        assert!((place(1800.0, 100.0, &TRACK, read) - TRACK.position_on(Edge::Right, 1800.0, 100.0)).abs() < 0.001);
    }

    #[test]
    fn it_goes_to_another_screen_only_well_onto_it() {
        let s = crate::Screen { name: None, x: 0, y: 0, w: 2560, h: 1600, scale: 1.5, work: (0, 0, 2560, 1528) };
        assert_eq!(beyond(&s, 1200.0, 800.0), 0.0, "on it");
        assert_eq!(beyond(&s, 2560.0 + 90.0, 800.0), 90.0, "just past the shared edge");
        assert_eq!(beyond(&s, -40.0, 1700.0), 100.0, "past a corner, the further of the two");
        assert!(beyond(&s, 2560.0 + 150.0 * 1.5 - 1.0, 800.0) / 1.5 < CROSS, "hugging the shared edge it stays");
        assert!(beyond(&s, 2560.0 + 150.0 * 1.5, 800.0) / 1.5 >= CROSS);
    }

    #[test]
    fn the_shorter_way_round() {
        let p = TRACK.perimeter();
        assert!((TRACK.signed(p - 10.0) + 10.0).abs() < 0.001, "just behind the start is a step back");
        assert!((TRACK.signed(10.0) - 10.0).abs() < 0.001);
    }

    /// The pointer walked from one point to the next in `steps`, as the carry reads it; where the
    /// notch's middle is at the end.
    fn walked(hold: &mut Hold, from: (f64, f64), to: (f64, f64), steps: u32) -> f64 {
        let mut target = 0.0;
        for i in 0..=steps {
            let t = f64::from(i) / f64::from(steps);
            target = hold.target(from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t, &TRACK);
        }
        target
    }

    #[test]
    fn taken_hold_of_it_stays_where_it_is() {
        let middle = TRACK.position_on(Edge::Right, 1800.0, 500.0);
        let mut hold = Hold::take(Edge::Right, middle, 1770.0, 630.0, &TRACK);
        assert_eq!(hold.grip, -130.0, "held 130 further round than its middle");
        assert!((hold.target(1770.0, 630.0, &TRACK) - middle).abs() < 0.001);
    }

    #[test]
    fn taken_straight_across_the_screen_it_is_put_down_under_the_pointer() {
        // From the right edge to the top, nowhere near the corner between them. It used to keep
        // its place through the change of edge, and landed at the far end of the top.
        let middle = TRACK.position_on(Edge::Right, 1800.0, 500.0);
        for steps in [1, 12, 400] {
            let mut hold = Hold::take(Edge::Right, middle, 1770.0, 630.0, &TRACK);
            let rest = walked(&mut hold, (1770.0, 630.0), (900.0, 30.0), steps);
            assert_eq!(TRACK.place_at(rest), (Edge::Top, 900.0 - 130.0), "in {steps} steps");
        }
    }

    #[test]
    fn it_is_put_down_under_the_pointer_whichever_way_it_came() {
        let middle = TRACK.position_on(Edge::Right, 1800.0, 500.0);
        let drop = (900.0, 30.0);
        let ways: [&[(f64, f64)]; 4] = [
            &[],
            // Up the right edge and round by the corner
            &[(1790.0, 20.0), (1700.0, 10.0)],
            // Out into the middle of the screen and back
            &[(900.0, 600.0)],
            // The long way: down, along the bottom, up the left
            &[(1780.0, 1150.0), (20.0, 1150.0), (20.0, 20.0)],
        ];
        for way in ways {
            let mut hold = Hold::take(Edge::Right, middle, 1770.0, 630.0, &TRACK);
            let mut at = (1770.0, 630.0);
            for next in way.iter().copied().chain([drop]) {
                walked(&mut hold, at, next, 60);
                at = next;
            }
            assert_eq!(TRACK.place_at(hold.target(drop.0, drop.1, &TRACK)), (Edge::Top, 770.0), "by way of {way:?}");
        }
    }

    #[test]
    fn carried_again_and_again_it_is_always_put_down_by_the_pointer() {
        // Every drop on a grid over the screen, each carry begun where the last one ended
        let mut middle = TRACK.position_on(Edge::Right, 1800.0, 500.0);
        let mut at = (1770.0, 500.0);
        let mut edge = Edge::Right;
        for i in 0..200u32 {
            let drop = (f64::from((i * 617) % 1800), f64::from((i * 389) % 1169));
            let mut hold = Hold::take(edge, middle, at.0, at.1, &TRACK);
            let grip = hold.grip.abs();
            let rest = walked(&mut hold, at, drop, 25);
            let nearest = Edge::ALL
                .into_iter()
                .map(|e| distance_from(e, drop.0, drop.1, &TRACK))
                .fold(f64::INFINITY, f64::min);
            let (on, along) = TRACK.place_at(rest);
            let (x, y) = match on {
                Edge::Top => (along, 0.0),
                Edge::Bottom => (along, TRACK.height),
                Edge::Left => (0.0, along),
                Edge::Right => (TRACK.width, along),
            };
            let away = (x - drop.0).abs().max((y - drop.1).abs());
            // On the border as near the pointer as the border gets, give or take where it was held,
            // the margin an edge is kept by and the corner's own reading
            assert!(away <= nearest + grip + EDGE_SWITCH_MARGIN + CORNER_REACH, "drop {i} at {drop:?}: put down at {on:?} {along}, {away} away");
            (middle, edge) = (rest, on);
            at = (x.clamp(1.0, TRACK.width - 1.0), y.clamp(1.0, TRACK.height - 1.0));
        }
    }
}
