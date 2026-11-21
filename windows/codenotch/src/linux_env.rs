//! What has to be true of the process before GTK starts on Linux. The launcher script sets some of
//! it, but a desktop entry or a compositor's autostart runs the bare binary, so it is done here.
use std::path::Path;

const DISABLE_DMABUF: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
const FORCE_SHM: &str = "WEBKIT_DMABUF_RENDERER_FORCE_SHM";
/// Set to keep the WebKit variables exactly as the environment has them.
const KEEP_WEBKIT_ENV: &str = "CODENOTCH_KEEP_WEBKIT_ENV";

pub(crate) fn truthy(value: Option<String>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// The variables to set (`Some`) or remove (`None`) before GTK starts.
///
/// * A Wayland client cannot place its own window, and the notch sits on a screen edge, so GDK is
///   pinned to X11 (XWayland). Hyprland's own docs suggest `GDK_BACKEND=wayland,x11,*` session-wide.
///   The exception is the notch as a layer surface (`layer_shell`), which the compositor anchors
///   to the edge itself and which only a Wayland client can have.
/// * WebKitGTK 2.54 without its DMA-BUF renderer keeps only what was repainted in the last frame of
///   a transparent window: the ring spins on an otherwise empty notch (#489). That path is taken
///   when the renderer is switched off (a common session-wide NVIDIA workaround) and when the
///   driver cannot allocate GBM buffers. Shared-memory buffers keep the renderer and need no GBM.
fn env_plan(
    get: impl Fn(&str) -> Option<String>,
    nvidia: bool,
    layer_surface: bool,
) -> Vec<(&'static str, Option<&'static str>)> {
    let mut plan = vec![("GDK_BACKEND", Some(if layer_surface { "wayland" } else { "x11" }))];
    if truthy(get(KEEP_WEBKIT_ENV)) {
        return plan;
    }
    let renderer_off = truthy(get(DISABLE_DMABUF));
    if renderer_off {
        plan.push((DISABLE_DMABUF, None));
    }
    if (renderer_off || nvidia) && get(FORCE_SHM).is_none() {
        plan.push((FORCE_SHM, Some("1")));
    }
    plan
}

/// The proprietary driver's kernel module is loaded. With an integrated GPU beside it the buffers
/// may well come from Mesa, where shared memory costs a copy but draws the same.
fn nvidia_driver_loaded() -> bool {
    Path::new("/sys/module/nvidia").exists()
}

/// tao reads X events on one thread while worker threads ask GDK for the monitors; Xlib aborts on
/// that ("Unknown request in queue while dequeuing") unless it was told about threads first.
fn init_xlib_threads() -> Result<(), String> {
    let xlib = x11_dl::xlib::Xlib::open().map_err(|e| e.to_string())?;
    let status = unsafe { (xlib.XInitThreads)() };
    // GDK loads the same library; it must stay mapped for the life of the process.
    std::mem::forget(xlib);
    if status == 0 {
        return Err("XInitThreads failed".into());
    }
    Ok(())
}

/// Call first thing in `main`, before any thread exists and before GTK is touched.
pub fn prepare() {
    let layer_surface = crate::layer_shell::wanted();
    for (key, value) in env_plan(|k| std::env::var(k).ok(), nvidia_driver_loaded(), layer_surface) {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
    if let Err(e) = init_xlib_threads() {
        eprintln!("codenotch: Xlib threads not initialised ({e})");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(env: &[(&str, &str)], nvidia: bool) -> Vec<(&'static str, Option<&'static str>)> {
        let env: Vec<(String, String)> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        env_plan(|k| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone()), nvidia, false)
    }

    #[test]
    fn a_layer_surface_needs_the_wayland_backend() {
        assert_eq!(env_plan(|_| None, false, true), vec![("GDK_BACKEND", Some("wayland"))]);
        assert_eq!(
            env_plan(|_| None, true, true),
            vec![("GDK_BACKEND", Some("wayland")), (FORCE_SHM, Some("1"))],
            "the WebKit workaround is the same on either backend"
        );
    }

    #[test]
    fn gdk_is_pinned_to_x11_whatever_the_session_asks_for() {
        assert_eq!(plan(&[], false), vec![("GDK_BACKEND", Some("x11"))]);
        assert_eq!(plan(&[("GDK_BACKEND", "wayland,x11,*")], false), vec![("GDK_BACKEND", Some("x11"))]);
    }

    #[test]
    fn nvidia_gets_shared_memory_buffers() {
        assert_eq!(plan(&[], true), vec![("GDK_BACKEND", Some("x11")), (FORCE_SHM, Some("1"))]);
    }

    #[test]
    fn a_disabled_dmabuf_renderer_is_switched_back_on_with_shared_memory() {
        let want = vec![("GDK_BACKEND", Some("x11")), (DISABLE_DMABUF, None), (FORCE_SHM, Some("1"))];
        assert_eq!(plan(&[(DISABLE_DMABUF, "1")], false), want);
        assert_eq!(plan(&[(DISABLE_DMABUF, "1")], true), want);
        // "0" is how the renderer is asked for explicitly: nothing to undo.
        assert_eq!(plan(&[(DISABLE_DMABUF, "0")], false), vec![("GDK_BACKEND", Some("x11"))]);
    }

    #[test]
    fn an_explicit_buffer_choice_is_left_alone() {
        assert_eq!(plan(&[(FORCE_SHM, "0")], true), vec![("GDK_BACKEND", Some("x11"))]);
        assert_eq!(
            plan(&[(DISABLE_DMABUF, "1"), (KEEP_WEBKIT_ENV, "1")], true),
            vec![("GDK_BACKEND", Some("x11"))]
        );
    }

    #[test]
    fn xlib_accepts_threads_where_libx11_is_installed() {
        if x11_dl::xlib::Xlib::open().is_ok() {
            assert_eq!(init_xlib_threads(), Ok(()));
        }
    }
}
