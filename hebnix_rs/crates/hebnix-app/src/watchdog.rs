use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{
    INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

pub const CLEANUP_WATCHDOG_ARG: &str = "--cleanup-watchdog";

pub fn spawn() -> bool {
    use std::os::windows::process::CommandExt;

    if !crate::spoofer::is_admin() {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let owner = std::process::id();
    let _ = std::fs::create_dir_all(watchdog_owner_path().parent().unwrap());
    let _ = std::fs::write(watchdog_owner_path(), owner.to_string());
    std::process::Command::new(exe)
        .args([CLEANUP_WATCHDOG_ARG, &owner.to_string()])
        .creation_flags(0x08000208)
        .spawn()
        .is_ok()
}

pub fn parent_pid() -> Option<u32> {
    let mut args = std::env::args();
    while let Some(argument) = args.next() {
        if argument == CLEANUP_WATCHDOG_ARG {
            return args.next()?.parse().ok();
        }
    }
    None
}

pub fn run(parent_pid: u32) {
    if let Ok(parent) = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, parent_pid) } {
        unsafe {
            WaitForSingleObject(parent, INFINITE);
            let _ = CloseHandle(parent);
        }
    }
    let _ = crate::spoofer::hosts::clear();
    while hebnix_sdk::process::is_rocket_league_running() {
        if replacement_is_running(parent_pid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    if replacement_is_running(parent_pid) {
        return;
    }
    for _ in 0..3 {
        let _ = crate::winutil::clear_rocket_league_multihome();
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let _ = crate::multiplayer_lan::cleanup_system_state();
    crate::spoofer::hosts::flush_dns();
    if watchdog_owner().is_some_and(|owner| owner == parent_pid) {
        let _ = std::fs::remove_file(watchdog_owner_path());
    }
}

fn watchdog_owner_path() -> std::path::PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("Hebnix")
        .join("state")
        .join("watchdog_owner.pid")
}

fn watchdog_owner() -> Option<u32> {
    std::fs::read_to_string(watchdog_owner_path())
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn replacement_is_running(parent_pid: u32) -> bool {
    let Some(owner) = watchdog_owner().filter(|owner| *owner != parent_pid) else {
        return false;
    };
    let Ok(process) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, owner) }) else {
        return false;
    };
    let running = unsafe { WaitForSingleObject(process, 0) } == WAIT_TIMEOUT;
    unsafe {
        let _ = CloseHandle(process);
    }
    running
}
