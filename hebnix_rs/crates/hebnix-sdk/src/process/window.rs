//! RL window focus + geometry (windows only).
//!
//! id by process id, not window title. GetWindowText can hang forever on
//! same-process windows owned by non-pumping threads (medal/discord overlay
//! hooks inject those everywhere), and OpenProcess on the game is unreliable
//! once eac locks it. GetWindowThreadProcessId dodges both.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetCursorPos, GetForegroundWindow, GetWindowRect, GetWindowTextLengthW,
    GetWindowThreadProcessId, IsWindowVisible,
};

/// cached RL pid, the process scan costs a few ms and the focus helpers run
/// twice a second.
fn cached_rl_pid() -> Option<u32> {
    static CACHE: Mutex<Option<(Instant, Option<u32>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap();
    if let Some((ts, pid)) = *cache {
        if ts.elapsed() < Duration::from_secs(3) {
            return pid;
        }
    }
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let pid = sys
        .processes()
        .iter()
        .find(|(_, p)| {
            p.name()
                .to_string_lossy()
                .to_lowercase()
                .contains("rocketleague")
        })
        .map(|(pid, _)| pid.as_u32());
    *cache = Some((Instant::now(), pid));
    pid
}

fn window_pid(hwnd: HWND) -> u32 {
    let mut pid: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid
}

struct EnumState {
    pid: u32,
    result: Option<HWND>,
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    unsafe {
        let state = &mut *(lparam.0 as *mut EnumState);
        if window_pid(hwnd) == state.pid
            && IsWindowVisible(hwnd).as_bool()
            // main window = the visible one with a caption. GetWindowTextLengthW
            // reads a cached length, never sends messages, so it can't hang.
            && GetWindowTextLengthW(hwnd) > 0
        {
            state.result = Some(hwnd);
            return false.into(); // stop enumeration
        }
        true.into()
    }
}

/// RL's main window handle by pid
pub fn rocket_league_hwnd() -> Option<HWND> {
    let pid = cached_rl_pid()?;
    let mut state = EnumState { pid, result: None };
    unsafe {
        // EnumWindows errors when the callback stops early, not a failure here
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut state as *mut EnumState as isize));
    }
    state.result
}

/// does the RL window have focus. compares the foreground window's pid to
/// RocketLeague.exe, no title reads, no handle opens.
pub fn is_rocket_league_focused() -> bool {
    let Some(pid) = cached_rl_pid() else {
        return false;
    };
    unsafe {
        let fg = GetForegroundWindow();
        !fg.is_invalid() && window_pid(fg) == pid
    }
}

/// (left, top, right, bottom) pixel rect of the RL window
pub fn get_rocket_league_window_rect() -> Option<(i32, i32, i32, i32)> {
    let hwnd = rocket_league_hwnd()?;
    unsafe {
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        if rect.right > rect.left && rect.bottom > rect.top {
            Some((rect.left, rect.top, rect.right, rect.bottom))
        } else {
            None
        }
    }
}

/// pixel size of the monitor RL is on, falls back to the primary. what plugin
/// windows size themselves against when they ask in percent.
pub fn rocket_league_monitor_size() -> (i32, i32) {
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    unsafe {
        if let Some(hwnd) = rocket_league_hwnd() {
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(monitor, &mut info).as_bool() {
                let r = info.rcMonitor;
                if r.right > r.left && r.bottom > r.top {
                    return (r.right - r.left, r.bottom - r.top);
                }
            }
        }
        (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN))
    }
}

/// is the cursor inside the RL window
pub fn is_cursor_inside_rl_window() -> bool {
    let Some((left, top, right, bottom)) = get_rocket_league_window_rect() else {
        return false;
    };
    unsafe {
        let mut pt = windows::Win32::Foundation::POINT::default();
        if GetCursorPos(&mut pt).is_err() {
            return false;
        }
        left <= pt.x && pt.x <= right && top <= pt.y && pt.y <= bottom
    }
}
