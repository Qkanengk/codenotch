//! codenotch-hook: the minimal client Claude Code's hooks call.
//! Duty: report the event plus stdin JSON to the main app. It never starts the app: as on the Mac,
//! Codenotch runs when it is opened or at sign-in, and a hook that finds it closed gives up.
//! Iron rule: never block Claude Code — ~2 s total budget, and every failure exits 0 silently.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

const DEFAULT_PORT: u16 = 48666;
const MAX_STDIN: u64 = 256 * 1024;

fn main() {
    let event = std::env::args().nth(1).unwrap_or_else(|| "ping".into());

    // The hook's stdin is the JSON Claude Code provides (session_id / cwd / prompt / message…)
    let mut body = String::new();
    let _ = std::io::stdin().take(MAX_STDIN).read_to_string(&mut body);

    let port = read_port();
    let token = read_token();
    let ppid = parent_pid();

    // Not running: give up quietly — never affect Claude Code
    let _ = send(port, &token, &event, ppid, &body);
}

/// Where the app keeps its files: the directory `dirs::config_dir()` gives it, worked out by hand
/// to keep this binary free of dependencies. %APPDATA% on Windows, XDG elsewhere.
fn config_dir(var: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let set = |name: &str| var(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        set("APPDATA")?
    } else if cfg!(target_os = "macos") {
        set("HOME")?.join("Library").join("Application Support")
    } else {
        match set("XDG_CONFIG_HOME").filter(|p| p.is_absolute()) {
            Some(dir) => dir,
            None => set("HOME")?.join(".config"),
        }
    };
    Some(base.join("codenotch"))
}

fn config_file(name: &str) -> Option<String> {
    let path = config_dir(|v| std::env::var(v).ok())?.join(name);
    std::fs::read_to_string(path).ok()
}

/// The token the app wrote to `auth_token`; without it the app refuses the event.
fn read_token() -> String {
    config_file("auth_token").map(|s| s.trim().to_string()).unwrap_or_default()
}

/// Pulls "port" out of config.json (hand-rolled scan, no dependency)
fn read_port() -> u16 {
    config_file("config.json").and_then(|txt| parse_port(&txt)).unwrap_or(DEFAULT_PORT)
}

fn parse_port(txt: &str) -> Option<u16> {
    let i = txt.find("\"port\"")?;
    let digits: String = txt[i + 6..]
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<u16>().ok().filter(|p| *p > 0)
}

fn build_request(event: &str, ppid: u32, token: &str, body: &str) -> String {
    let auth_header = if !token.is_empty() {
        format!("X-Codenotch-Token: {}\r\n", token)
    } else {
        String::new()
    };
    format!(
        "POST /event?e={}&ppid={} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n{}",
        event,
        ppid,
        body.len(),
        auth_header,
        body
    )
}

fn send(port: u16, token: &str, event: &str, ppid: u32, body: &str) -> std::io::Result<()> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(300))?;
    s.set_write_timeout(Some(Duration::from_millis(700)))?;
    s.set_read_timeout(Some(Duration::from_millis(700)))?;
    let req = build_request(event, ppid, token, body);
    s.write_all(req.as_bytes())?;
    let mut buf = [0u8; 64];
    let _ = s.read(&mut buf); // wait for a response fragment to confirm delivery; failure does not matter
    Ok(())
}

/// Parent process PID (≈ the Claude Code CLI process) via NtQueryInformationProcess, no dependency
#[cfg(windows)]
fn parent_pid() -> u32 {
    #[repr(C)]
    struct Pbi {
        exit_status: isize,
        peb: usize,
        affinity_mask: usize,
        base_priority: isize,
        unique_process_id: usize,
        inherited_from_unique_process_id: usize,
    }
    extern "system" {
        fn NtQueryInformationProcess(
            handle: isize,
            class: u32,
            info: *mut Pbi,
            len: u32,
            ret_len: *mut u32,
        ) -> i32;
    }
    unsafe {
        let mut pbi = std::mem::zeroed::<Pbi>();
        let mut ret = 0u32;
        // -1 = GetCurrentProcess()
        if NtQueryInformationProcess(
            -1,
            0,
            &mut pbi,
            std::mem::size_of::<Pbi>() as u32,
            &mut ret,
        ) == 0
        {
            return pbi.inherited_from_unique_process_id as u32;
        }
    }
    0
}

#[cfg(not(windows))]
fn parent_pid() -> u32 {
    std::os::unix::process::parent_id()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_request_with_token() {
        let req = build_request("session_start", 1234, "secret123", "{}");
        assert!(req.contains("POST /event?e=session_start&ppid=1234 HTTP/1.1\r\n"));
        assert!(req.contains("X-Codenotch-Token: secret123\r\n"));
        assert!(req.contains("Content-Length: 2\r\n"));
    }

    #[test]
    fn test_build_request_without_token() {
        let req = build_request("running", 5678, "", "{}");
        assert!(req.contains("POST /event?e=running&ppid=5678 HTTP/1.1\r\n"));
        assert!(!req.contains("X-Codenotch-Token"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn the_token_is_read_from_where_the_app_writes_it_on_linux() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
        };
        assert_eq!(config_dir(env(&[("HOME", "/home/k")])), Some(PathBuf::from("/home/k/.config/codenotch")));
        assert_eq!(
            config_dir(env(&[("HOME", "/home/k"), ("XDG_CONFIG_HOME", "/data/cfg")])),
            Some(PathBuf::from("/data/cfg/codenotch"))
        );
        // As `dirs` does: a relative or empty XDG_CONFIG_HOME is not one.
        assert_eq!(
            config_dir(env(&[("HOME", "/home/k"), ("XDG_CONFIG_HOME", "cfg")])),
            Some(PathBuf::from("/home/k/.config/codenotch"))
        );
        assert_eq!(config_dir(env(&[("APPDATA", "C:\\Users\\k\\AppData\\Roaming")])), None);
    }

    #[test]
    fn the_port_is_read_out_of_the_config() {
        assert_eq!(parse_port("{\n  \"port\": 50123,\n  \"lang\": \"auto\"\n}"), Some(50123));
        assert_eq!(parse_port("{\"lang\":\"auto\"}"), None);
        assert_eq!(parse_port("{\"port\": 0}"), None);
    }
}
