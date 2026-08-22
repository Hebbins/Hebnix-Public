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

pub fn restart_rocket_league_multihome(
    game_path: &std::path::Path,
    address: &str,
) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    kill_rocket_league().map_err(|error| error.to_string())?;
    for _ in 0..60 {
        if !hebnix_sdk::process::is_rocket_league_running() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let path = game_path.to_string_lossy().to_ascii_lowercase();
    let is_steam = path.contains("steamapps")
        || path.contains("steam\\common")
        || path.contains("steam/library");
    if is_steam {
        let launch = format!("steam://run/252950//-multihome%3D{address}/");
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &launch])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|error| error.to_string())?;
        return Ok(());
    }
    apply_epic_multihome(address)?;
    restart_epic_launcher_for_multihome()?;
    std::process::Command::new("cmd")
        .args([
            "/C",
            "start",
            "",
            "com.epicgames.launcher://apps/9773aa1aa54f4f7b80e44bef04986cea%3A530145df28a24424923f5828cc9031a1%3ASugar?action=launch&silent=true",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn restart_epic_launcher_for_multihome() -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/IM", "EpicGamesLauncher.exe"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    for _ in 0..40 {
        let running = std::process::Command::new("tasklist")
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .to_ascii_lowercase()
                    .contains("epicgameslauncher.exe")
            })
            .unwrap_or(false);
        if !running {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    Err(
        "Epic Games Launcher did not close so its launch options could not be refreshed"
            .to_string(),
    )
}

pub fn clear_rocket_league_multihome() -> Result<(), String> {
    clear_epic_multihome()
}

fn apply_epic_multihome(address: &str) -> Result<(), String> {
    let config_root = dirs::data_local_dir()
        .ok_or_else(|| "could not find LocalAppData".to_string())?
        .join("EpicGamesLauncher")
        .join("Saved")
        .join("Config");
    let paths = [
        config_root
            .join("WindowsEditor")
            .join("GameUserSettings.ini"),
        config_root.join("Windows").join("GameUserSettings.ini"),
    ];
    let command_key = "9773aa1aa54f4f7b80e44bef04986cea:530145df28a24424923f5828cc9031a1:Sugar_AdditionalCommands";
    let enabled_key = "9773aa1aa54f4f7b80e44bef04986cea:530145df28a24424923f5828cc9031a1:Sugar_AdditionalCommandsEnabled";
    let mut changed = false;
    for path in paths.into_iter().filter(|path| path.is_file()) {
        let original = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
        let mut lines: Vec<String> = original.lines().map(str::to_owned).collect();
        let prefixes = lines
            .iter()
            .filter_map(|line| {
                line.strip_prefix('[')
                    .and_then(|line| line.strip_suffix("_Launcher]"))
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        for prefix in prefixes {
            let settings_section = format!("[{prefix}_Settings]");
            let settings = match lines.iter().position(|line| line == &settings_section) {
                Some(index) => index,
                None => {
                    lines.push(settings_section);
                    lines.len() - 1
                }
            };
            let end = lines
                .iter()
                .skip(settings + 1)
                .position(|line| line.starts_with('['))
                .map(|offset| settings + 1 + offset)
                .unwrap_or(lines.len());
            let mut command_found = false;
            let mut enabled_found = false;
            for line in &mut lines[settings + 1..end] {
                if let Some((key, value)) = line.split_once('=') {
                    if key == command_key {
                        let mut arguments = value
                            .split_whitespace()
                            .filter(|argument| !argument.starts_with("-multihome="))
                            .map(str::to_owned)
                            .collect::<Vec<_>>();
                        arguments.push(format!("-multihome={address}"));
                        *line = format!("{command_key}={}", arguments.join(" "));
                        command_found = true;
                    } else if key == enabled_key {
                        *line = format!("{enabled_key}=True");
                        enabled_found = true;
                    }
                }
            }
            if !command_found {
                lines.insert(end, format!("{command_key}=-multihome={address}"));
            }
            if !enabled_found {
                let enabled_at = if command_found { end } else { end + 1 };
                lines.insert(enabled_at, format!("{enabled_key}=True"));
            }
            changed = true;
        }
        if lines.join("\n") != original.replace("\r\n", "\n") {
            std::fs::write(path, lines.join("\r\n")).map_err(|error| error.to_string())?;
        }
    }
    if changed {
        Ok(())
    } else {
        Err("Epic Games Launcher settings were not found".to_string())
    }
}

fn clear_epic_multihome() -> Result<(), String> {
    let backup = dirs::data_dir()
        .ok_or_else(|| "could not find AppData".to_string())?
        .join("Hebnix")
        .join("state")
        .join("epic_multihome_backup.txt");
    if let Ok(contents) = std::fs::read_to_string(&backup) {
        if let Some((path, original)) = contents.split_once('\n') {
            std::fs::write(path, original).map_err(|error| error.to_string())?;
            let _ = std::fs::remove_file(&backup);
            return Ok(());
        }
    }
    let config_root = dirs::data_local_dir()
        .ok_or_else(|| "could not find LocalAppData".to_string())?
        .join("EpicGamesLauncher")
        .join("Saved")
        .join("Config");
    let paths = [
        config_root
            .join("WindowsEditor")
            .join("GameUserSettings.ini"),
        config_root.join("Windows").join("GameUserSettings.ini"),
    ];
    for path in paths.into_iter().filter(|path| path.is_file()) {
        let original = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
        let mut lines: Vec<String> = original.lines().map(str::to_owned).collect();
        let mut changed = false;
        for line in &mut lines {
            if let Some((key, value)) = line.split_once('=') {
                if key.ends_with(":Sugar_AdditionalCommands") {
                    let remaining = value
                        .split_whitespace()
                        .filter(|argument| {
                            !argument.starts_with("-multihome=10.242.77.")
                                && !argument.starts_with("-multihome=192.10.192.")
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    if remaining != value {
                        *line = format!("{key}={remaining}");
                        changed = true;
                    }
                }
            }
        }
        let commands_empty = lines.iter().all(|line| {
            line.split_once('=')
                .filter(|(key, _)| key.ends_with(":Sugar_AdditionalCommands"))
                .is_none_or(|(_, value)| value.trim().is_empty())
        });
        if commands_empty {
            for line in &mut lines {
                if let Some((key, value)) = line.split_once('=')
                    && key.ends_with(":Sugar_AdditionalCommandsEnabled")
                    && !value.eq_ignore_ascii_case("false")
                {
                    *line = format!("{key}=False");
                    changed = true;
                }
            }
        }
        if changed {
            std::fs::write(&path, lines.join("\r\n")).map_err(|error| error.to_string())?;
        }
    }
    let _ = std::fs::remove_file(backup);
    Ok(())
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
