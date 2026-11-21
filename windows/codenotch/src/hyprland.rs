//! Hyprland treats the notch like any other XWayland window: bordered, rounded, blurred behind,
//! focused when it opens and left behind on the workspace it opened on. A window rule turns all of
//! that off, and the notch sets the rule itself over Hyprland's socket so nothing has to be pasted
//! into a config. Hyprland with a Lua config only (0.57 was the one tested); an older one answers
//! "unknown request" and is left as it is.
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The notch's title on Linux: the rule below goes by it, and so does finding the window again.
pub const NOTCH_TITLE: &str = "Codenotch notch";

/// Floating and pinned keep it on the edge on every workspace; the rest is decoration Hyprland adds
/// to a window. The title keeps Settings an ordinary window. No `no_focus`: Hyprland sends such a
/// window no pointer at all, so the notch could be neither hovered nor clicked.
pub const WINDOW_RULE: &str = r#"hl.window_rule({ name = "codenotch", match = { class = "^Codenotch$", title = "^Codenotch( notch| carry)?$", xwayland = true }, float = true, pin = true, border_size = 0, rounding = 0, no_blur = true, no_shadow = true, no_dim = true, no_anim = true, no_initial_focus = true })"#;

/// Ends the line `autostart on` adds to hyprland.lua, and is how that line is found again.
const AUTOSTART_MARK: &str = "-- codenotch autostart";

fn socket(name: &str) -> Option<PathBuf> {
    let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").filter(|s| !s.is_empty())?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|s| !s.is_empty())?;
    Some(PathBuf::from(runtime).join("hypr").join(signature).join(name))
}

fn request(socket: &Path, body: &str) -> std::io::Result<String> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(body.as_bytes())?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    Ok(reply)
}

fn apply_rule(socket: &Path) -> Result<(), String> {
    match request(socket, &format!("eval {WINDOW_RULE}")) {
        Ok(reply) if reply.trim() == "ok" => Ok(()),
        Ok(reply) => Err(reply.trim().to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// A reload drops every rule that is not in the config file, this one included.
fn reapply_on_reload(events: impl Read, command: &Path) {
    for line in BufReader::new(events).lines() {
        let Ok(line) = line else { return };
        if line.starts_with("configreloaded>>") {
            match apply_rule(command) {
                Ok(()) => crate::applog("hyprland: window rule restored after a config reload"),
                Err(e) => crate::applog(&format!("hyprland: window rule not restored after a reload ({e})")),
            }
        }
    }
}

/// Before the window is created: Hyprland reads `float` and `pin` once, when a window is mapped.
pub fn start() {
    let (Some(command), Some(events)) = (socket(".socket.sock"), socket(".socket2.sock")) else {
        return;
    };
    match apply_rule(&command) {
        Ok(()) => crate::applog("hyprland: window rule applied"),
        Err(e) => {
            crate::applog(&format!("hyprland: window rule not applied ({e}); see the Hyprland section of the README"));
            return;
        }
    }
    std::thread::spawn(move || match UnixStream::connect(&events) {
        Ok(stream) => reapply_on_reload(stream, &command),
        Err(e) => crate::applog(&format!("hyprland: no event socket, a config reload will drop the window rule ({e})")),
    });
}

/// "900, 300", Hyprland's answer to `cursorpos`, in its layout coordinates.
fn parse_cursor(reply: &str) -> Option<(f64, f64)> {
    let (x, y) = reply.trim().split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// The notch's `[x, y, width, height]` out of `j/clients`, in the same coordinates as the cursor.
fn notch_box(clients: &str, pid: u32) -> Option<[f64; 4]> {
    let clients: serde_json::Value = serde_json::from_str(clients).ok()?;
    let notch = clients.as_array()?.iter().find(|c| c["pid"].as_u64() == Some(pid as u64) && c["title"] == NOTCH_TITLE)?;
    let n = |v: &serde_json::Value| v.as_f64();
    Some([n(&notch["at"][0])?, n(&notch["at"][1])?, n(&notch["size"][0])?, n(&notch["size"][1])?])
}

/// Hyprland scales an XWayland window unless told not to, so its box and the X window's pixels can differ.
fn relative_to(cursor: (f64, f64), on_screen: [f64; 4], x_size: (f64, f64)) -> Option<(f64, f64)> {
    let [x, y, w, h] = on_screen;
    (w > 0.0 && h > 0.0).then(|| ((cursor.0 - x) * x_size.0 / w, (cursor.1 - y) * x_size.1 / h))
}

fn cursor_in_notch_at(socket: &Path, pid: u32, x_size: (f64, f64)) -> Option<(f64, f64)> {
    let cursor = parse_cursor(&request(socket, "cursorpos").ok()?)?;
    relative_to(cursor, notch_box(&request(socket, "j/clients").ok()?, pid)?, x_size)
}

/// Where the cursor is over the notch, in the X window's own pixels (`x_size` is its size in them);
/// None when Hyprland is not there to ask. Xwayland's own pointer stops at the edge of an X window.
pub fn cursor_in_notch(x_size: (f64, f64)) -> Option<(f64, f64)> {
    cursor_in_notch_at(&socket(".socket.sock")?, std::process::id(), x_size)
}

fn lua_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn shell_word(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn autostart_line(exe: &Path) -> String {
    let command = format!("{} --silent", shell_word(&exe.to_string_lossy()));
    format!("hl.on(\"hyprland.start\", function() hl.exec_cmd({}) end) {AUTOSTART_MARK}", lua_string(&command))
}

/// What `codenotch hyprland-config` prints, for a config that should carry the rule itself.
pub fn config_snippet(exe: &Path) -> String {
    format!("-- Codenotch on Hyprland (XWayland)\n{WINDOW_RULE}\n{}\n", autostart_line(exe))
}

fn without_autostart(config: &str) -> String {
    config.lines().filter(|line| !line.trim_end().ends_with(AUTOSTART_MARK)).map(|line| format!("{line}\n")).collect()
}

fn with_autostart(config: &str, exe: &Path) -> String {
    let mut out = without_autostart(config);
    out.push_str(&autostart_line(exe));
    out.push('\n');
    out
}

fn config_file() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("hypr").join("hyprland.lua"))
}

fn rewrite(path: &Path, edit: impl Fn(&str) -> String) -> Result<bool, String> {
    let before = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let after = edit(&before);
    if after != before {
        std::fs::write(path, after).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(true)
}

/// Hyprland does not read ~/.config/autostart on its own, so a Lua config gets one marked line.
/// `Ok(true)` when there is such a config.
pub fn enable_autostart(exe: &Path) -> Result<bool, String> {
    let Some(path) = config_file() else { return Ok(false) };
    rewrite(&path, |config| with_autostart(config, exe))
}

pub fn disable_autostart() -> Result<bool, String> {
    let Some(path) = config_file() else { return Ok(false) };
    rewrite(&path, without_autostart)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// A stand-in for Hyprland's command socket: answers each request with `reply` and reports it.
    fn fake_hyprland(reply: &'static str) -> (PathBuf, std::sync::mpsc::Receiver<String>) {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("codenotch-hypr-{}-{n}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let (seen, requests) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let _ = stream.write_all(reply.as_bytes());
                if seen.send(String::from_utf8_lossy(&buf[..n]).into_owned()).is_err() {
                    return;
                }
            }
        });
        (path, requests)
    }

    #[test]
    fn the_rule_is_sent_as_lua_and_ok_is_success() {
        let (socket, requests) = fake_hyprland("ok");
        assert_eq!(apply_rule(&socket), Ok(()));
        assert_eq!(requests.recv().unwrap(), format!("eval {WINDOW_RULE}"));
        let _ = std::fs::remove_file(socket);
    }

    #[test]
    fn a_hyprland_without_lua_is_reported_not_assumed() {
        let (socket, _requests) = fake_hyprland("unknown request");
        assert_eq!(apply_rule(&socket), Err("unknown request".into()));
        let _ = std::fs::remove_file(&socket);
        assert!(apply_rule(&socket).is_err(), "no socket at all is an error too");
    }

    #[test]
    fn the_rule_comes_back_after_each_config_reload() {
        let (socket, requests) = fake_hyprland("ok");
        let events = "openwindow>>1,1,Codenotch,Codenotch notch\nconfigreloaded>>\nworkspace>>2\nconfigreloaded>>\n";
        reapply_on_reload(events.as_bytes(), &socket);
        assert_eq!(requests.try_iter().count(), 2);
        let _ = std::fs::remove_file(socket);
    }

    #[test]
    fn the_rule_covers_what_hyprland_adds_to_a_window() {
        for field in ["float = true", "pin = true", "border_size = 0", "rounding = 0", "no_blur = true", "no_shadow = true", "no_anim = true", "no_initial_focus = true"] {
            assert!(WINDOW_RULE.contains(field), "{field}");
        }
        assert!(!WINDOW_RULE.contains("no_focus"), "Hyprland sends a no_focus window no pointer");
        assert!(WINDOW_RULE.contains(r#"class = "^Codenotch$""#));
        assert!(WINDOW_RULE.contains(&format!("title = \"^Codenotch( {}| carry)?$\"", NOTCH_TITLE.trim_start_matches("Codenotch "))));
        assert!(!WINDOW_RULE.contains('\n'), "the socket takes one line");
    }

    const CLIENTS: &str = r#"[
        {"address": "0x1", "at": [22, 22], "size": [931, 1036], "class": "foot", "title": "foot", "pid": 7, "xwayland": false},
        {"address": "0x2", "at": [300, 200], "size": [900, 700], "class": "Codenotch", "title": "Codenotch Settings", "pid": 42, "xwayland": true},
        {"address": "0x3", "at": [1560, 215], "size": [360, 650], "class": "Codenotch", "title": "Codenotch notch", "pid": 42, "xwayland": true}
    ]"#;

    #[test]
    fn the_cursor_is_read_from_hyprland() {
        assert_eq!(parse_cursor("900, 300\n"), Some((900.0, 300.0)));
        assert_eq!(parse_cursor("-12, 0"), Some((-12.0, 0.0)));
        assert_eq!(parse_cursor("unknown request"), None);
    }

    #[test]
    fn the_notch_is_picked_out_of_this_process_s_windows() {
        assert_eq!(notch_box(CLIENTS, 42), Some([1560.0, 215.0, 360.0, 650.0]));
        assert_eq!(notch_box(CLIENTS, 7), None, "another process");
        assert_eq!(notch_box("[]", 42), None);
        assert_eq!(notch_box("unknown request", 42), None);
    }

    #[test]
    fn the_cursor_is_mapped_into_the_x_window_s_pixels() {
        let on_screen = [1560.0, 215.0, 360.0, 650.0];
        assert_eq!(relative_to((1915.0, 540.0), on_screen, (360.0, 650.0)), Some((355.0, 325.0)));
        assert_eq!(relative_to((900.0, 300.0), on_screen, (360.0, 650.0)), Some((-660.0, 85.0)));
        // A 1.25x monitor without force_zero_scaling: the box is in layout units, the X window is not
        assert_eq!(relative_to((1740.0, 540.0), on_screen, (450.0, 812.5)), Some((225.0, 406.25)));
        assert_eq!(relative_to((0.0, 0.0), [0.0, 0.0, 0.0, 0.0], (360.0, 650.0)), None);
    }

    #[test]
    fn the_cursor_over_the_notch_takes_two_questions() {
        let (socket, requests) = fake_hyprland("900, 300");
        // The fake gives one answer to everything, and a cursor position is not a client list
        assert_eq!(cursor_in_notch_at(&socket, 42, (360.0, 650.0)), None);
        assert_eq!(requests.try_iter().collect::<Vec<_>>(), ["cursorpos", "j/clients"]);
        let _ = std::fs::remove_file(&socket);
        assert_eq!(cursor_in_notch_at(&socket, 42, (360.0, 650.0)), None, "no Hyprland");
    }

    #[test]
    fn autostart_is_one_marked_line_that_survives_odd_paths() {
        let line = autostart_line(Path::new("/opt/My Apps/it's \"here\"/codenotch"));
        assert_eq!(
            line,
            r#"hl.on("hyprland.start", function() hl.exec_cmd("'/opt/My Apps/it'\\''s \"here\"/codenotch' --silent") end) -- codenotch autostart"#
        );
        assert!(!line.contains('\n'));
    }

    #[test]
    fn autostart_is_added_once_and_removed_without_touching_the_rest() {
        let config = "local terminal = \"foot\"\nhl.bind(\"SUPER + Q\", hl.dsp.exec_cmd(terminal))";
        let exe = Path::new("/usr/bin/codenotch");
        let on = with_autostart(config, exe);
        assert_eq!(on, format!("{config}\n{}\n", autostart_line(exe)));
        assert_eq!(with_autostart(&on, exe), on, "enabling twice adds nothing");
        let moved = with_autostart(&on, Path::new("/home/k/codenotch"));
        assert_eq!(moved.matches(AUTOSTART_MARK).count(), 1, "a moved binary replaces the line");
        assert!(moved.contains("/home/k/codenotch"));
        assert_eq!(without_autostart(&on), format!("{config}\n"));
    }

    #[test]
    fn only_an_existing_config_is_edited() {
        let dir = std::env::temp_dir().join(format!("codenotch-hyprcfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hyprland.lua");
        let exe = Path::new("/usr/bin/codenotch");
        assert_eq!(rewrite(&path, |c| with_autostart(c, exe)), Ok(false));
        assert!(!path.exists());
        std::fs::write(&path, "-- mine\n").unwrap();
        assert_eq!(rewrite(&path, |c| with_autostart(c, exe)), Ok(true));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("-- mine\n{}\n", autostart_line(exe)));
        assert_eq!(rewrite(&path, without_autostart), Ok(true));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "-- mine\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_printed_config_holds_the_rule_and_the_autostart() {
        let snippet = config_snippet(Path::new("/usr/bin/codenotch"));
        assert!(snippet.contains(WINDOW_RULE));
        assert!(snippet.contains(&autostart_line(Path::new("/usr/bin/codenotch"))));
    }
}
