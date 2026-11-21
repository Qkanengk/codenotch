//! Jump back to the right terminal: from the session's Claude CLI process PID, walk the parent chain
//! to the hosting terminal window, then SetForegroundWindow + FlashWindowEx. Returns false on failure (the page reports it).

#[cfg(windows)]
pub fn focus_terminal(claude_pid: u32) -> bool {
    use std::collections::HashMap;
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, FlashWindowEx, GetWindowTextLengthW, GetWindowThreadProcessId, IsIconic,
        IsWindowVisible, SetForegroundWindow, ShowWindow, FLASHWINFO, FLASHW_ALL, SW_RESTORE,
    };

    if claude_pid == 0 {
        return false;
    }

    // 1) Full pid -> ppid snapshot
    let mut ppid_map: HashMap<u32, u32> = HashMap::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return false;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                ppid_map.insert(entry.th32ProcessID, entry.th32ParentProcessID);
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = windows::Win32::Foundation::CloseHandle(snap);
    }

    // 2) claude's ancestor chain (itself included), at most 8 levels: node → shell → WindowsTerminal/conhost host…
    let mut chain: Vec<u32> = vec![claude_pid];
    let mut cur = claude_pid;
    for _ in 0..8 {
        match ppid_map.get(&cur) {
            Some(&p) if p != 0 && !chain.contains(&p) => {
                chain.push(p);
                cur = p;
            }
            _ => break,
        }
    }

    // 3) Enumerate visible top-level windows
    struct Cand {
        hwnd: isize,
        pid: u32,
    }
    let mut wins: Vec<Cand> = Vec::new();
    unsafe extern "system" fn cb(hwnd: HWND, l: LPARAM) -> BOOL {
        let v = &mut *(l.0 as *mut Vec<(isize, u32)>);
        if IsWindowVisible(hwnd).as_bool() && GetWindowTextLengthW(hwnd) > 0 {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            v.push((hwnd.0 as isize, pid));
        }
        BOOL(1)
    }
    let mut raw: Vec<(isize, u32)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut raw as *mut _ as isize));
    }
    for (h, p) in raw {
        wins.push(Cand { hwnd: h, pid: p });
    }

    // 4) Score: the window's PID is on the ancestor chain (higher up = the real terminal host = higher
    //    score), or the window PID's parent is on the chain (the classic conhost case).
    let score_of = |pid: u32| -> Option<usize> {
        if let Some(i) = chain.iter().position(|&c| c == pid) {
            return Some(i);
        }
        if let Some(&pp) = ppid_map.get(&pid) {
            if let Some(i) = chain.iter().position(|&c| c == pp) {
                return Some(i);
            }
        }
        None
    };
    let best = wins
        .iter()
        .filter_map(|w| score_of(w.pid).map(|s| (s, w.hwnd)))
        .max_by_key(|(s, _)| *s);

    let Some((_, hwnd_raw)) = best else {
        return false;
    };
    unsafe {
        let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
        let fi = FLASHWINFO {
            cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
            hwnd,
            dwFlags: FLASHW_ALL,
            uCount: 2,
            dwTimeout: 0,
        };
        let _ = FlashWindowEx(&fi);
    }
    true
}

/// On Hyprland: the terminal is the nearest process above Claude that has a window.
#[cfg(target_os = "linux")]
pub fn focus_terminal(claude_pid: u32) -> bool {
    if claude_pid == 0 {
        return false;
    }
    let Some(clients) = crate::hyprland::clients() else { return false };
    let chain = chain_of(claude_pid, &proc_maps().ppid);
    window_of(&chain, &clients).is_some_and(|w| crate::hyprland::focus_window(&w.address))
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn focus_terminal(_claude_pid: u32) -> bool {
    false
}

/// The window a process is running in: that of the first process up its chain that has one, and of
/// those the one focused last, since a terminal can keep every window in one process.
///
/// Not the highest up, as on Windows: there the chain ends at the terminal, here it goes on to
/// whatever started the terminal, and a launcher or a bar with a window of its own is not it.
#[cfg(target_os = "linux")]
pub fn window_of<'a>(chain: &[u32], clients: &'a [crate::hyprland::Client]) -> Option<&'a crate::hyprland::Client> {
    chain.iter().find_map(|pid| clients.iter().filter(|c| c.pid == *pid).min_by_key(|c| c.focus_order))
}

/// Whether the window in front is the one a session runs in, which is what seeing it means.
#[cfg(target_os = "linux")]
pub fn in_front(front: &crate::hyprland::Client, chain: &[u32], clients: &[crate::hyprland::Client]) -> bool {
    window_of(chain, clients).is_some_and(|w| w.pid == front.pid)
}

/// The Claude desktop app, by its window class, as there is no telling a process's window from
/// its name here.
#[cfg(target_os = "linux")]
pub fn is_claude_desktop(client: &crate::hyprland::Client) -> bool {
    let class = client.class.to_lowercase();
    class.contains("claude") && !class.contains("codenotch")
}

/// `/proc/<pid>/stat`: the name in brackets, which may itself hold brackets and spaces, then the
/// state and the parent.
#[cfg(target_os = "linux")]
fn parse_stat(stat: &str) -> Option<(String, u32)> {
    let (open, close) = (stat.find('(')?, stat.rfind(')')?);
    let ppid = stat.get(close + 1..)?.split_whitespace().nth(1)?.parse().ok()?;
    Some((stat.get(open + 1..close)?.to_lowercase(), ppid))
}

#[cfg(target_os = "linux")]
pub fn proc_maps() -> ProcMaps {
    let mut m = ProcMaps { ppid: Default::default(), name: Default::default() };
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let Some((name, ppid)) = std::fs::read_to_string(entry.path().join("stat")).ok().and_then(|s| parse_stat(&s)) else {
            continue;
        };
        m.ppid.insert(pid, ppid);
        m.name.insert(pid, name);
    }
    m
}

// ---------------- Process and foreground helpers shared by seen-clears-it and the desktop jump-back ----------------

#[cfg(any(windows, target_os = "linux"))]
pub struct ProcMaps {
    pub ppid: std::collections::HashMap<u32, u32>,
    #[cfg_attr(not(windows), allow(dead_code))]
    pub name: std::collections::HashMap<u32, String>, // lower-case exe name
}

#[cfg(windows)]
pub fn proc_maps() -> ProcMaps {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    let mut m = ProcMaps {
        ppid: Default::default(),
        name: Default::default(),
    };
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return m;
        };
        let mut e = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                m.ppid.insert(e.th32ProcessID, e.th32ParentProcessID);
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(260);
                m.name.insert(
                    e.th32ProcessID,
                    String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase(),
                );
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = windows::Win32::Foundation::CloseHandle(snap);
    }
    m
}

#[cfg(windows)]
pub fn fg_pid() -> u32 {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return 0;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid
    }
}

#[cfg(any(windows, target_os = "linux"))]
pub fn chain_of(pid: u32, ppid: &std::collections::HashMap<u32, u32>) -> Vec<u32> {
    let mut chain = vec![pid];
    let mut cur = pid;
    for _ in 0..8 {
        match ppid.get(&cur) {
            Some(&p) if p != 0 && !chain.contains(&p) => {
                chain.push(p);
                cur = p;
            }
            _ => break,
        }
    }
    chain
}

/// Whether the foreground process belongs to a session's terminal window (itself on the chain, or its parent — the conhost case)
#[cfg(windows)]
pub fn pid_hits_chain(pid: u32, chain: &[u32], maps: &ProcMaps) -> bool {
    chain.contains(&pid)
        || maps
            .ppid
            .get(&pid)
            .map(|p| chain.contains(p))
            .unwrap_or(false)
}

/// Focus the Claude desktop app's main window (the jump-back target for desktop sessions: the largest visible window whose process name contains claude)
#[cfg(windows)]
pub fn focus_claude_desktop() -> bool {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, FlashWindowEx, GetWindowRect, GetWindowTextLengthW, GetWindowThreadProcessId,
        IsIconic, IsWindowVisible, SetForegroundWindow, ShowWindow, FLASHWINFO, FLASHW_ALL,
        SW_RESTORE,
    };
    let maps = proc_maps();
    unsafe extern "system" fn cb(hwnd: HWND, l: LPARAM) -> BOOL {
        let v = &mut *(l.0 as *mut Vec<(isize, u32)>);
        if IsWindowVisible(hwnd).as_bool() && GetWindowTextLengthW(hwnd) > 0 {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            v.push((hwnd.0 as isize, pid));
        }
        BOOL(1)
    }
    let mut wins: Vec<(isize, u32)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut wins as *mut _ as isize));
    }
    let mut best: Option<(isize, i64)> = None;
    for (h, pid) in wins {
        let Some(name) = maps.name.get(&pid) else {
            continue;
        };
        if !name.contains("claude") || name.contains("codenotch") {
            continue;
        }
        let mut r = RECT::default();
        let area = unsafe {
            if GetWindowRect(HWND(h as *mut core::ffi::c_void), &mut r).is_ok() {
                ((r.right - r.left) as i64) * ((r.bottom - r.top) as i64)
            } else {
                0
            }
        };
        if best.map(|(_, a)| area > a).unwrap_or(true) {
            best = Some((h, area));
        }
    }
    let Some((h, _)) = best else {
        return false;
    };
    unsafe {
        let hwnd = HWND(h as *mut core::ffi::c_void);
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
        let fi = FLASHWINFO {
            cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
            hwnd,
            dwFlags: FLASHW_ALL,
            uCount: 2,
            dwTimeout: 0,
        };
        let _ = FlashWindowEx(&fi);
    }
    true
}

#[cfg(target_os = "linux")]
pub fn focus_claude_desktop() -> bool {
    let Some(clients) = crate::hyprland::clients() else { return false };
    let window = clients.iter().filter(|c| is_claude_desktop(c)).min_by_key(|c| c.focus_order);
    window.is_some_and(|w| crate::hyprland::focus_window(&w.address))
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn focus_claude_desktop() -> bool {
    false
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::hyprland::Client;
    use std::collections::HashMap;

    fn window(address: &str, pid: u32, class: &str, focus_order: i64) -> Client {
        Client { address: address.into(), pid, class: class.into(), workspace: "1".into(), full_screen: false, focus_order }
    }

    #[test]
    fn a_process_is_read_out_of_proc_whatever_its_name_holds() {
        assert_eq!(parse_stat("4242 (claude) S 4100 4242 4100 34817 4242 4194304 1"), Some(("claude".into(), 4100)));
        assert_eq!(parse_stat("77 (Web Content (x) 1) R 60 77 77 0 -1"), Some(("web content (x) 1".into(), 60)));
        assert_eq!(parse_stat("no such process"), None);
        let me = std::process::id();
        let maps = proc_maps();
        assert_eq!(maps.ppid.get(&me), Some(&std::os::unix::process::parent_id()), "this very process, as /proc has it");
        assert_eq!(chain_of(me, &maps.ppid)[..2], [me, std::os::unix::process::parent_id()]);
    }

    #[test]
    fn the_terminal_is_the_nearest_window_above_claude() {
        // Hyprland started a launcher, which started a terminal, in which a shell runs Claude
        let ppid = HashMap::from([(500, 400), (400, 300), (300, 200), (200, 100), (100, 1), (310, 100)]);
        let chain = chain_of(500, &ppid);
        assert_eq!(chain, [500, 400, 300, 200, 100, 1]);
        let clients = [window("0xb", 310, "firefox", 0), window("0xl", 200, "launcher", 1), window("0xt", 300, "foot", 2)];
        assert_eq!(window_of(&chain, &clients).map(|w| w.address.as_str()), Some("0xt"), "not the launcher further up");
        assert_eq!(window_of(&chain_of(310, &ppid), &clients).map(|w| w.address.as_str()), Some("0xb"));
        assert_eq!(window_of(&chain, &clients[..1]), None, "a browser beside it is not where Claude runs");
    }

    #[test]
    fn of_one_terminal_process_with_several_windows_it_is_the_one_used_last() {
        let clients = [window("0x1", 300, "kitty", 4), window("0x2", 300, "kitty", 1), window("0x3", 300, "kitty", i64::MAX)];
        assert_eq!(window_of(&[500, 400, 300], &clients).map(|w| w.address.as_str()), Some("0x2"));
    }

    #[test]
    fn a_session_is_seen_when_its_terminal_is_in_front() {
        let clients = [window("0xb", 310, "firefox", 1), window("0xt", 300, "foot", 0)];
        assert!(in_front(&clients[1], &[500, 400, 300, 100], &clients));
        assert!(!in_front(&clients[0], &[500, 400, 300, 100], &clients), "the browser is in front, not the terminal");
        assert!(!in_front(&clients[1], &[900, 800], &clients), "another session, whose terminal has gone");
    }

    #[test]
    fn the_desktop_app_is_known_by_its_window_class() {
        assert!(is_claude_desktop(&window("0x1", 1, "Claude", 0)));
        assert!(is_claude_desktop(&window("0x1", 1, "claude-desktop", 0)));
        assert!(!is_claude_desktop(&window("0x1", 1, "Codenotch", 0)));
        assert!(!is_claude_desktop(&window("0x1", 1, "foot", 0)));
    }
}
