use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcessId};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetForegroundWindow, GetWindowLongW, GetWindowThreadProcessId,
    SetForegroundWindow, SetLayeredWindowAttributes, SetWindowLongW, SetWindowPos, GWL_EXSTYLE,
    HWND_NOTOPMOST, HWND_TOPMOST, LWA_ALPHA, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WS_EX_LAYERED,
};

static HIDDEN: AtomicBool = AtomicBool::new(false);
static SHOW_REQUESTED: AtomicBool = AtomicBool::new(false);

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn main_window() -> Option<HWND> {
    let title = wide("Hebnix Lite");
    unsafe {
        FindWindowW(None, PCWSTR(title.as_ptr()))
            .ok()
            .filter(|window| !window.is_invalid())
    }
}

pub fn acquire_single_instance() -> Option<HANDLE> {
    let name = wide("Global\\hebnix_LiteSingleInstanceMutex_v1");
    unsafe {
        let handle = CreateMutexW(None, false, PCWSTR(name.as_ptr())).ok()?;
        (GetLastError() != ERROR_ALREADY_EXISTS).then_some(handle)
    }
}

pub fn focus_existing_instance() {
    if let Some(window) = main_window() {
        unsafe {
            let _ = SetForegroundWindow(window);
        }
    }
}

pub fn main_window_hwnd() -> Option<HWND> {
    main_window()
}

pub fn foreground_window_is_ours() -> bool {
    unsafe {
        let window = GetForegroundWindow();
        if window.is_invalid() {
            return false;
        }
        let mut process_id = 0;
        GetWindowThreadProcessId(window, Some(&mut process_id));
        process_id == GetCurrentProcessId()
    }
}

pub fn note_foreground() {}

pub fn set_main_window_topmost(topmost: bool) {
    if let Some(window) = main_window() {
        unsafe {
            let _ = SetWindowPos(
                window,
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

pub fn focus_main_window() {
    if let Some(window) = main_window() {
        unsafe {
            let _ = SetForegroundWindow(window);
        }
    }
}

pub fn focus_rocket_league() {
    if let Some(window) = hebnix_sdk::process::rocket_league_hwnd() {
        unsafe {
            let _ = SetForegroundWindow(window);
        }
    }
}

pub fn install_minimize_hook(_: HWND, _: &eframe::egui::Context) {}

pub fn main_window_hidden() -> bool {
    HIDDEN.load(Ordering::Relaxed)
}

pub fn request_show() {
    SHOW_REQUESTED.store(true, Ordering::Relaxed);
    if let Some(window) = main_window() {
        unsafe {
            let _ = SetLayeredWindowAttributes(
                window,
                windows::Win32::Foundation::COLORREF(0),
                255,
                LWA_ALPHA,
            );
        }
    }
}

pub fn take_show_request() -> bool {
    SHOW_REQUESTED.swap(false, Ordering::Relaxed)
}

pub fn set_main_window_invisible(invisible: bool) {
    HIDDEN.store(invisible, Ordering::Relaxed);
    if let Some(window) = main_window() {
        unsafe {
            let style = GetWindowLongW(window, GWL_EXSTYLE);
            if invisible {
                SetWindowLongW(window, GWL_EXSTYLE, style | WS_EX_LAYERED.0 as i32);
                let _ = SetLayeredWindowAttributes(
                    window,
                    windows::Win32::Foundation::COLORREF(0),
                    0,
                    LWA_ALPHA,
                );
            } else {
                let _ = SetLayeredWindowAttributes(
                    window,
                    windows::Win32::Foundation::COLORREF(0),
                    255,
                    LWA_ALPHA,
                );
            }
        }
    }
}

pub fn restart_rocket_league(game_path: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/IM", "RocketLeague.exe"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    for _ in 0..60 {
        let running = hebnix_sdk::process::is_rocket_league_running();
        if !running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let path = game_path.to_string_lossy().to_ascii_lowercase();
    let launch = if path.contains("steamapps")
        || path.contains("steam\\common")
        || path.contains("steam/library")
    {
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

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Hebnix Lite";

pub fn is_startup_enabled() -> bool {
    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(RUN_KEY)
        .and_then(|key| key.get_value::<String, _>(RUN_VALUE))
        .is_ok()
}

pub fn set_startup_enabled(enabled: bool) -> std::io::Result<()> {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey(RUN_KEY)?;
    if enabled {
        key.set_value(
            RUN_VALUE,
            &format!("\"{}\"", std::env::current_exe()?.display()),
        )?;
    } else {
        let _ = key.delete_value(RUN_VALUE);
    }
    Ok(())
}
