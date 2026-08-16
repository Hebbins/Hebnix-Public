//! windows bits: single-instance mutex, run-at-startup registry, focus handoff,
//! minimize hook, killing RL.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcessId};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetForegroundWindow, GetWindowThreadProcessId, HWND_NOTOPMOST, HWND_TOPMOST,
    IsIconic, IsWindow, IsWindowVisible, SW_RESTORE, SW_SHOW, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SetForegroundWindow, SetWindowPos, ShowWindow, SwitchToThisWindow,
};
use windows::core::PCWSTR;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// grab the single-instance mutex. None if another instance already holds it.
/// handle is leaked on purpose for the process lifetime.
pub fn acquire_single_instance() -> Option<HANDLE> {
    let name = wide("Global\\hebnix_SingleInstanceMutex_v1");
    unsafe {
        let handle = CreateMutexW(None, false, PCWSTR(name.as_ptr())).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return None;
        }
        Some(handle)
    }
}

fn find_hebnix_window(own_process_only: bool) -> Option<HWND> {
    unsafe {
        let name = wide("Hebnix");
        let hwnd = FindWindowW(None, PCWSTR(name.as_ptr())).ok()?;
        if hwnd.is_invalid() {
            return None;
        }
        if own_process_only {
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid != GetCurrentProcessId() {
                return None;
            }
        }
        Some(hwnd)
    }
}

/// our main window's HWND (this process only), if it exists yet
pub fn main_window_hwnd() -> Option<HWND> {
    find_hebnix_window(true)
}

/// pin/unpin the main window over everything (incl the game) without
/// activating it. same os mechanism the plugin windows use, works no matter
/// who has focus.
pub fn set_main_window_topmost(topmost: bool) {
    if let Some(hwnd) = find_hebnix_window(true) {
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(if topmost {
                    HWND_TOPMOST
                } else {
                    HWND_NOTOPMOST
                }),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
}

/// a second instance calls this before exiting to surface the already-running
/// window, so launching twice isn't a silent no-op.
pub fn focus_existing_instance() {
    unsafe {
        let name = wide("Hebnix");
        if let Ok(hwnd) = FindWindowW(None, PCWSTR(name.as_ptr())) {
            if !hwnd.is_invalid() {
                if !IsWindowVisible(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_SHOW);
                }
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }
}

// focus handoff

static MAIN_HIDDEN: AtomicBool = AtomicBool::new(false);
static CAME_FROM_GAME: AtomicBool = AtomicBool::new(false);

fn window_is_ours(hwnd: HWND) -> bool {
    unsafe {
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid == GetCurrentProcessId()
    }
}

pub fn foreground_window_is_ours() -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();
        !hwnd.is_invalid() && window_is_ours(hwnd)
    }
}

pub fn main_window_hidden() -> bool {
    MAIN_HIDDEN.load(Ordering::Relaxed)
}

fn focus_window(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return false;
        }
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if SetForegroundWindow(hwnd).as_bool() {
            return true;
        }
        SwitchToThisWindow(hwnd, true); // alt-tab's path, no AttachThreadInput needed
        GetForegroundWindow() == hwnd
    }
}

/// note which program we came from
pub fn note_foreground() {
    if foreground_window_is_ours() {
        return;
    }
    CAME_FROM_GAME.store(
        hebnix_sdk::process::is_rocket_league_focused(),
        Ordering::Relaxed,
    );
}

/// hand the foreground back, but only when the game is what we came from.
pub fn restore_foreground() -> bool {
    if !CAME_FROM_GAME.load(Ordering::Relaxed) {
        return false;
    }
    let Some(hwnd) = hebnix_sdk::process::rocket_league_hwnd() else {
        return false;
    };
    // a minimised game stays minimised, thats the user's call
    if unsafe { IsIconic(hwnd).as_bool() } {
        return false;
    }
    focus_window(hwnd)
}

pub fn focus_main_window() -> bool {
    match find_hebnix_window(true) {
        Some(hwnd) => focus_window(hwnd),
        None => false,
    }
}

// minimize hook so that instead of minimizing it triggers the
// hide function since minimization cause bugs

const WM_SYSCOMMAND: u32 = 0x0112;
const WM_ACTIVATE: u32 = 0x0006;
const SC_MINIMIZE: usize = 0xF020;

static MINIMIZE_REQUESTED: AtomicBool = AtomicBool::new(false);
static SHOW_REQUESTED: AtomicBool = AtomicBool::new(false);
static REPAINT_CTX: std::sync::OnceLock<eframe::egui::Context> = std::sync::OnceLock::new();

pub fn take_minimize_request() -> bool {
    MINIMIZE_REQUESTED.swap(false, Ordering::Relaxed)
}

pub fn take_show_request() -> bool {
    SHOW_REQUESTED.swap(false, Ordering::Relaxed)
}

fn wake_ui() {
    if let Some(ctx) = REPAINT_CTX.get() {
        ctx.request_repaint();
    }
}

unsafe extern "system" fn minimize_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    unsafe {
        match msg {
            // low 4 bits are reserved
            WM_SYSCOMMAND if (wparam.0 & 0xFFF0) == SC_MINIMIZE => {
                // taskbar click on an already focused window minimizes it, so
                // while hidden that same click is a request to come back
                if MAIN_HIDDEN.load(Ordering::Relaxed) {
                    SHOW_REQUESTED.store(true, Ordering::Relaxed);
                } else {
                    MINIMIZE_REQUESTED.store(true, Ordering::Relaxed);
                }
                wake_ui();
                return LRESULT(0);
            }
            // taskbar or alt-tab onto a window thats invisible at alpha 0
            WM_ACTIVATE if (wparam.0 & 0xFFFF) != 0 && MAIN_HIDDEN.load(Ordering::Relaxed) => {
                SHOW_REQUESTED.store(true, Ordering::Relaxed);
                wake_ui();
            }
            _ => {}
        }
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }
}

pub fn install_minimize_hook(hwnd: HWND, ctx: &eframe::egui::Context) {
    let _ = REPAINT_CTX.set(ctx.clone());
    unsafe {
        if !SetWindowSubclass(hwnd, Some(minimize_proc), 0x4842_584D /* "HBXM" */, 0).as_bool() {
            tracing::warn!("minimize hook not installed, the button will still minimize");
        }
    }
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Hebnix";

/// is hebnix set to start with windows
pub fn is_startup_enabled() -> bool {
    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(RUN_KEY)
        .and_then(|key| key.get_value::<String, _>(RUN_VALUE))
        .is_ok()
}

/// toggle start-with-windows via the HKCU Run key
pub fn set_startup_enabled(enabled: bool) -> std::io::Result<()> {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey(RUN_KEY)?;
    if enabled {
        let exe = std::env::current_exe()?;
        key.set_value(RUN_VALUE, &format!("\"{}\"", exe.display()))?;
    } else {
        let _ = key.delete_value(RUN_VALUE);
    }
    Ok(())
}

/// force-kill RocketLeague.exe (console quit command)
pub fn kill_rocket_league() -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("taskkill")
        .args(["/F", "/IM", "RocketLeague.exe"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|_| ())
}

pub fn restart_rocket_league(game_path: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = kill_rocket_league();
    for _ in 0..60 {
        let running = std::process::Command::new("tasklist")
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .to_ascii_lowercase()
                    .contains("rocketleague.exe")
            })
            .unwrap_or(false);
        if !running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let path = game_path.to_string_lossy().to_ascii_lowercase();
    // The monitor updates SettingsCfg.rl_path from the running process before
    // this command is handled. Steam installs can live in any library, so use
    // the Steam-owned directory markers rather than a fixed drive/path.
    let is_steam = path.contains("steamapps")
        || path.contains("steam\\common")
        || path.contains("steam/library");
    let launch = if is_steam {
        "steam://rungameid/252950"
    } else {
        "com.epicgames.launcher://apps/9773aa1aa54f4f7b80e44bef04986cea%3A530145df28a24424923f5828cc9031a1%3ASugar?action=launch&silent=true"
    };
    std::process::Command::new("cmd")
        .args(["/C", "start", "", launch])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}

/// hide by dropping the os alpha to 0. SW_HIDE crashes wgpu and takes the
/// plugin child windows with it.
pub fn set_main_window_invisible(invisible: bool) {
    MAIN_HIDDEN.store(invisible, Ordering::Relaxed);
    if let Some(hwnd) = find_hebnix_window(true) {
        unsafe {
            use windows::Win32::Foundation::COLORREF;
            use windows::Win32::UI::WindowsAndMessaging::{
                GWL_EXSTYLE, GetWindowLongW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongW,
                WS_EX_LAYERED,
            };
            let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE);
            if invisible {
                SetWindowLongW(hwnd, GWL_EXSTYLE, ex_style | WS_EX_LAYERED.0 as i32);
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
            } else {
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
            }
        }
    }
}
