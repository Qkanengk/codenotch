//! Whether a full-screen app — a video, a game, a slideshow — is in front on the notch's screen.
//!
//! The Mac folds its notch for one (`FullScreenDetector`), and so does this port, behind the same
//! setting. It also moves the notch, setting or not: the taskbar steps aside for such an app, so a
//! notch still placed above it floats a taskbar's height off the edge, over the picture.
//!
//! Polled rather than hooked: a video going full screen in the browser already in front changes no
//! foreground window, only that window's size.

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

use crate::{AppState, Screen};

/// x, y, width, height in physical pixels
type Rect = (i32, i32, i32, i32);

/// The monitor a full-screen app fills, whichever screen the notch is on
static FILLED: Mutex<Option<Rect>> = Mutex::new(None);
/// That monitor is the notch's
static HERE: AtomicBool = AtomicBool::new(false);
/// What the page was last told
static FOLDING: AtomicBool = AtomicBool::new(false);

const POLL: Duration = Duration::from_millis(400);
/// The Mac's tolerance, for borders and rounding
const SLACK: i32 = 4;

pub fn filled(s: &Screen) -> bool {
    *FILLED.lock().unwrap() == Some((s.x, s.y, s.w, s.h))
}

/// The screen as placement sees it: while a full-screen app fills it the taskbar is out of the way,
/// so the whole monitor is the notch's to sit against.
pub fn apply(mut s: Screen) -> Screen {
    if filled(&s) {
        s.work = (s.x, s.y, s.w, s.h);
    }
    s
}

pub fn folding() -> bool {
    FOLDING.load(SeqCst)
}

/// Tells the page whether to fold, when that has changed: a full-screen app arriving or leaving, or
/// the setting switched.
pub fn tell(app: &AppHandle) {
    let on = app.state::<AppState>().cfg.lock().unwrap().fold_for_full_screen;
    let fold = on && HERE.load(SeqCst);
    if FOLDING.swap(fold, SeqCst) != fold {
        let _ = app.emit_to("notch", "full_screen", fold);
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, PartialEq)]
enum Reading {
    Full(Rect),
    Windowed,
    /// The notch itself (its right-click menu makes it the foreground window), or the task switcher:
    /// nothing to say about what is behind it, so the last answer stands
    Unchanged,
}

#[cfg_attr(not(windows), allow(dead_code))]
fn same_rect(window: Rect, monitor: Rect) -> bool {
    let (a, b) = (window, monitor);
    (a.0 - b.0).abs() <= SLACK && (a.1 - b.1).abs() <= SLACK && (a.2 - b.2).abs() <= SLACK && (a.3 - b.3).abs() <= SLACK
}

/// `maximised`: zoomed and captioned. With the taskbar set to auto-hide, an ordinary maximised
/// window is monitor-sized too, and folding for it is what the Mac's #181 was about.
#[cfg_attr(not(windows), allow(dead_code))]
fn classify(class: &str, notch: bool, window: Rect, monitor: Rect, maximised: bool) -> Reading {
    if notch || matches!(class, "XamlExplorerHostIslandWindow" | "MultitaskingViewFrame" | "TaskSwitcherWnd" | "ForegroundStaging") {
        return Reading::Unchanged;
    }
    // The desktop is monitor-sized, and the taskbar is showing over it
    if matches!(class, "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
        return Reading::Windowed;
    }
    if !maximised && same_rect(window, monitor) {
        Reading::Full(monitor)
    } else {
        Reading::Windowed
    }
}

#[cfg(windows)]
fn read(app: &AppHandle) -> Reading {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONULL};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, IsZoomed, GWL_STYLE, WS_CAPTION,
    };
    let notch = app.get_webview_window("notch").and_then(|w| w.hwnd().ok()).map(|h| h.0 as isize);
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return Reading::Unchanged;
        }
        let mut name = [0u16; 64];
        let len = GetClassNameW(hwnd, &mut name).max(0) as usize;
        let class = String::from_utf16_lossy(&name[..len]);
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return Reading::Unchanged;
        }
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if monitor.is_invalid() || !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return Reading::Unchanged;
        }
        let m = info.rcMonitor;
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let maximised = IsZoomed(hwnd).as_bool() && style & WS_CAPTION.0 == WS_CAPTION.0;
        classify(
            &class,
            notch == Some(hwnd.0 as isize),
            (r.left, r.top, r.right - r.left, r.bottom - r.top),
            (m.left, m.top, m.right - m.left, m.bottom - m.top),
            maximised,
        )
    }
}

/// Hyprland says outright which windows are full screen and which workspace each output shows, so
/// the notch's own output is asked about rather than the window in front: a film on the other
/// screen folds nothing here. Any other desktop is left alone.
#[cfg(target_os = "linux")]
fn read(app: &AppHandle) -> Reading {
    let Some(screen) = crate::attached_screen(app) else { return Reading::Unchanged };
    let Some(covered) = crate::hyprland::covered_outputs() else { return Reading::Unchanged };
    on_hyprland(&screen, &crate::hyprland::outputs(), &covered)
}

/// Hyprland's name for the output a screen is. The notch as a layer surface already goes by it;
/// through XWayland the screen has X11's name for it, so it is found by where it is.
#[cfg(target_os = "linux")]
fn output_of<'a>(screen: &Screen, outputs: &'a [crate::hyprland::Output]) -> Option<&'a str> {
    let scale = screen.scale.max(0.01);
    let at = ((screen.x as f64 / scale).round() as i32, (screen.y as f64 / scale).round() as i32);
    outputs
        .iter()
        .find(|o| Some(o.name.as_str()) == screen.name.as_deref())
        .or_else(|| outputs.iter().find(|o| o.at == at))
        .or(outputs.first().filter(|_| outputs.len() == 1))
        .map(|o| o.name.as_str())
}

#[cfg(target_os = "linux")]
fn on_hyprland(screen: &Screen, outputs: &[crate::hyprland::Output], covered: &[String]) -> Reading {
    match output_of(screen, outputs) {
        Some(name) if covered.iter().any(|c| c == name) => Reading::Full((screen.x, screen.y, screen.w, screen.h)),
        Some(_) => Reading::Windowed,
        None => Reading::Unchanged,
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn read(_app: &AppHandle) -> Reading {
    Reading::Unchanged
}

pub fn start(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(POLL);
        match read(&app) {
            Reading::Full(m) => *FILLED.lock().unwrap() = Some(m),
            Reading::Windowed => *FILLED.lock().unwrap() = None,
            Reading::Unchanged => {}
        }
        let here = FILLED.lock().unwrap().is_some() && crate::target_screen(&app).is_some_and(|s| filled(&s));
        if HERE.swap(here, SeqCst) != here {
            crate::applog(&format!("full-screen app {}", if here { "in front" } else { "gone" }));
            let app = app.clone();
            std::thread::spawn(move || crate::glide_notch(&app));
        }
        tell(&app);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = (0, 0, 2560, 1600);

    #[test]
    fn a_borderless_window_the_size_of_the_screen_is_full_screen() {
        assert_eq!(classify("Chrome_WidgetWin_1", false, SCREEN, SCREEN, false), Reading::Full(SCREEN));
        let second = (2560, -200, 1920, 1080);
        assert_eq!(classify("UnityWndClass", false, second, second, false), Reading::Full(second));
    }

    #[test]
    fn a_maximised_window_is_not_even_where_the_taskbar_hides() {
        assert_eq!(classify("Chrome_WidgetWin_1", false, SCREEN, SCREEN, true), Reading::Windowed);
        let above_the_taskbar = (0, 0, 2560, 1528);
        assert_eq!(classify("Notepad", false, above_the_taskbar, SCREEN, false), Reading::Windowed);
        // Resize borders reach past the screen on every side
        assert_eq!(classify("Notepad", false, (-8, -8, 2576, 1616), SCREEN, false), Reading::Windowed);
    }

    #[test]
    fn the_desktop_is_screen_sized_and_is_not_full_screen() {
        assert_eq!(classify("Progman", false, SCREEN, SCREEN, false), Reading::Windowed);
        assert_eq!(classify("WorkerW", false, SCREEN, SCREEN, false), Reading::Windowed);
    }

    #[test]
    fn a_filled_screen_gives_the_notch_the_taskbars_room() {
        let s = Screen { name: None, x: 0, y: 0, w: 2560, h: 1600, scale: 1.5, work: (0, 0, 2560, 1528) };
        *FILLED.lock().unwrap() = Some(SCREEN);
        assert_eq!(apply(s.clone()).work, SCREEN);
        *FILLED.lock().unwrap() = Some((2560, 0, 1920, 1080));
        assert_eq!(apply(s).work, (0, 0, 2560, 1528), "full screen on the other monitor");
        *FILLED.lock().unwrap() = None;
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn on_hyprland_the_notch_folds_for_a_full_screen_window_on_its_own_output_only() {
        use crate::hyprland::Output;
        let outputs = [
            Output { name: "DP-1".into(), at: (0, 0), reserved: [0, 40, 0, 0] },
            Output { name: "HDMI-A-1".into(), at: (1707, 0), reserved: [0; 4] },
        ];
        let notch = Screen { name: Some("DP-1".into()), x: 0, y: 0, w: 2560, h: 1600, scale: 1.5, work: (0, 60, 2560, 1540) };
        let covered = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(on_hyprland(&notch, &outputs, &covered(&["DP-1"])), Reading::Full(SCREEN));
        assert_eq!(on_hyprland(&notch, &outputs, &covered(&["HDMI-A-1"])), Reading::Windowed, "a film on the other screen");
        assert_eq!(on_hyprland(&notch, &outputs, &[]), Reading::Windowed);

        // Through XWayland the screen has X11's name, or none: it is the output at the same place
        let by_place = Screen { name: Some("XWAYLAND1".into()), x: 1707, y: 0, w: 1920, h: 1080, scale: 1.0, work: (1707, 0, 1920, 1080) };
        assert_eq!(output_of(&by_place, &outputs), Some("HDMI-A-1"));
        assert_eq!(on_hyprland(&by_place, &outputs, &covered(&["HDMI-A-1"])), Reading::Full((1707, 0, 1920, 1080)));

        let nowhere = Screen { name: None, x: 5000, y: 0, w: 1920, h: 1080, scale: 1.0, work: (5000, 0, 1920, 1080) };
        assert_eq!(on_hyprland(&nowhere, &outputs, &covered(&["DP-1"])), Reading::Unchanged, "no telling which output it is");
        assert_eq!(output_of(&nowhere, &outputs[..1]), Some("DP-1"), "with one output there is nothing to tell apart");
    }

    #[test]
    fn the_notch_itself_and_the_task_switcher_leave_the_answer_as_it_was() {
        assert_eq!(classify("Tauri Window", true, SCREEN, SCREEN, false), Reading::Unchanged);
        assert_eq!(classify("XamlExplorerHostIslandWindow", false, SCREEN, SCREEN, false), Reading::Unchanged);
    }
}
