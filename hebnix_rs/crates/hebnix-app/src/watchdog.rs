use std::sync::{Arc, OnceLock};
use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};

use crate::spoofer::SpooferManager;

// Keep the live listeners in this process after the UI closes. Recreating
// them in the cleanup child would drop Rocket League's active WebSocket.
static LIVE_SPOOFER: OnceLock<Arc<SpooferManager>> = OnceLock::new();

pub fn handoff_live_spoofer(spoofer: Arc<SpooferManager>) -> bool {
    if !hebnix_sdk::process::is_rocket_league_running()
        || !(spoofer.http_running() || spoofer.socket_running() || spoofer.rlapi_running())
    {
        return false;
    }
    LIVE_SPOOFER.set(spoofer).is_ok() || LIVE_SPOOFER.get().is_some()
}

pub fn has_live_handoff() -> bool {
    LIVE_SPOOFER.get().is_some()
}

pub fn finish_live_handoff() {
    let Some(spoofer) = LIVE_SPOOFER.get() else {
        return;
    };
    tracing::info!("Hebnix UI closed; proxy watchdog remains active until Rocket League exits");
    while hebnix_sdk::process::is_rocket_league_running() {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    spoofer.shutdown();
    for _ in 0..5 {
        if crate::winutil::clear_rocket_league_web_cache().is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let _ = crate::winutil::clear_rocket_league_multihome();
    tracing::info!("Rocket League closed; proxy watchdog cleaned up");
}

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
    // Escape a launcher job when Windows allows it; otherwise use a detached
    // child so cleanup can outlive the Hebnix window.
    let start = |flags| {
        std::process::Command::new(&exe)
            .args([CLEANUP_WATCHDOG_ARG, &owner.to_string()])
            .creation_flags(flags)
            .spawn()
    };
    let spawned = start(0x09000208).or_else(|_| start(0x08000208)).is_ok();
    if !spawned {
        let _ = std::fs::remove_file(watchdog_owner_path());
    }
    spawned
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
    if replacement_is_running(parent_pid) {
        return;
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
    // Game shutdown can briefly keep WebCache files locked. Retry cleanup
    // after the process has gone, including any redirect left by a failed exit.
    for _ in 0..5 {
        let hosts_clean = crate::spoofer::hosts::clear().is_ok();
        let cache_clean = crate::winutil::clear_rocket_league_web_cache().is_ok();
        if hosts_clean && cache_clean {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
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
    crate::config::base_dir()
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
