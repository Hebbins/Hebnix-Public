use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcessId};
use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GWL_EXSTYLE, GetForegroundWindow, GetWindowLongW, GetWindowThreadProcessId,
    HWND_NOTOPMOST, HWND_TOPMOST, IsIconic, LWA_ALPHA, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SetForegroundWindow, SetLayeredWindowAttributes, SetWindowLongW, SetWindowPos, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};
use windows::core::PCWSTR;

static HIDDEN: AtomicBool = AtomicBool::new(false);
static SHOW_REQUESTED: AtomicBool = AtomicBool::new(false);
static CAME_FROM_GAME: AtomicBool = AtomicBool::new(false);

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

/// note which program we came from. ours doesnt count, the monitor calls this
/// on its tick so it tracks whatever you were last actually in.
pub fn note_foreground() {
    if foreground_window_is_ours() {
        return;
    }
    CAME_FROM_GAME.store(
        hebnix_sdk::process::is_rocket_league_focused(),
        Ordering::Relaxed,
    );
}

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

/// hand the foreground back, but only when the game is what we came from.
pub fn focus_rocket_league() {
    // alt-tabbed off and hid it from there, whoever holds focus keeps it
    if !foreground_window_is_ours() {
        return;
    }
    if !CAME_FROM_GAME.swap(false, Ordering::Relaxed) {
        return; // one shot
    }
    if let Some(window) = hebnix_sdk::process::rocket_league_hwnd() {
        unsafe {
            // a minimised game stays minimised, thats the user's call
            if IsIconic(window).as_bool() {
                return;
            }
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
            let style = GetWindowLongW(window, GWL_EXSTYLE);
            SetWindowLongW(window, GWL_EXSTYLE, style & !hidden_bits());
            let _ = SetLayeredWindowAttributes(
                window,
                windows::Win32::Foundation::COLORREF(0),
                255,
                LWA_ALPHA,
            );
            set_taskbar_button(window, true);
        }
    }
}

pub fn take_show_request() -> bool {
    SHOW_REQUESTED.swap(false, Ordering::Relaxed)
}

thread_local! {
    // com object, not Send
    static TASKBAR: std::cell::RefCell<Option<ITaskbarList>> =
        const { std::cell::RefCell::new(None) };
}

fn set_taskbar_button(window: HWND, shown: bool) {
    TASKBAR.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            let list = unsafe {
                CoCreateInstance::<_, ITaskbarList>(&TaskbarList, None, CLSCTX_INPROC_SERVER).ok()
            };
            if let Some(list) = &list {
                unsafe {
                    let _ = list.HrInit();
                }
            }
            *slot = list;
        }
        if let Some(list) = slot.as_ref() {
            unsafe {
                let _ = if shown {
                    list.AddTab(window)
                } else {
                    list.DeleteTab(window)
                };
            }
        }
    });
}

/// an alpha 0 window is still a normal one to the os. NOACTIVATE stops it being
/// handed the foreground, TOOLWINDOW drops it from alt-tab. read live, no frame
/// change, that would resize the client area.
fn hidden_bits() -> i32 {
    (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0) as i32
}

pub fn set_main_window_invisible(invisible: bool) {
    HIDDEN.store(invisible, Ordering::Relaxed);
    if let Some(window) = main_window() {
        unsafe {
            let style = GetWindowLongW(window, GWL_EXSTYLE);
            if invisible {
                SetWindowLongW(
                    window,
                    GWL_EXSTYLE,
                    style | WS_EX_LAYERED.0 as i32 | hidden_bits(),
                );
                let _ = SetLayeredWindowAttributes(
                    window,
                    windows::Win32::Foundation::COLORREF(0),
                    0,
                    LWA_ALPHA,
                );
                set_taskbar_button(window, false);
            } else {
                SetWindowLongW(window, GWL_EXSTYLE, style & !hidden_bits());
                let _ = SetLayeredWindowAttributes(
                    window,
                    windows::Win32::Foundation::COLORREF(0),
                    255,
                    LWA_ALPHA,
                );
                set_taskbar_button(window, true);
            }
        }
    }
}

pub fn start_rocket_league(game_path: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
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

pub fn kill_rocket_league() -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("taskkill")
        .args(["/F", "/IM", "RocketLeague.exe"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|_| ())
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
