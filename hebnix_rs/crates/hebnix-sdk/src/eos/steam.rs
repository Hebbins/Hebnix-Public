//! steam session ticket via steam_api64.dll
//!
// ported from the C++ eos_client flow (python one was flaky): load the dll,
// SteamAPI_Init, grab ISteamUser via the flat C api, then GetAuthSessionTicket
// for RL's appid (252950). hex ticket gets POSTed to epic oauth later.
//
// dll loaded + inited once per process and kept alive (never SteamAPI_Shutdown),
// re-initing churns steam and can trip ticket cooldowns. handles kept as ints so
// the cached state is Send+Sync, fn pointers rebuilt each call.

use std::ffi::c_void;
use std::sync::Mutex;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::core::{PCSTR, PCWSTR};

/// RL's steam appid
pub const RL_STEAM_APPID: u32 = 252950;

// flat steam C api. x64 windows has one calling convention so extern "C" matches
// steam's exports. HSteamUser/HSteamPipe are 32-bit handles.
type FnInit = unsafe extern "C" fn() -> bool;
type FnSteamClient = unsafe extern "C" fn() -> *mut c_void;
type FnGetHSteamUser = unsafe extern "C" fn() -> i32;
type FnGetHSteamPipe = unsafe extern "C" fn() -> i32;
type FnGetISteamUser = unsafe extern "C" fn(*mut c_void, i32, i32, *const u8) -> *mut c_void;
type FnGetAuthSessionTicket =
    unsafe extern "C" fn(*mut c_void, *mut u8, i32, *mut u32, *mut c_void) -> u32;
type FnGetSteamID = unsafe extern "C" fn(*mut c_void) -> u64;

// cached process-wide steam state. pointers held as ints so it's Send/Sync,
// dll + objects live for the whole process.
struct SteamState {
    steam_user: usize,
    p_get_ticket: usize,
    p_get_steam_id: usize,
    steam_id: u64,
}

// SAFETY: the addresses point to process-lifetime steam objects and exported
// fns, and we only rebuild fn pointers from them under the mutex.
unsafe impl Send for SteamState {}

static STATE: Mutex<Option<SteamState>> = Mutex::new(None);

// find steam_api64.dll. order: HEBNIX_STEAM_API_DLL env (full path), then next
// to the exe, then _dlls/ next to the exe.
fn locate_dll() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("HEBNIX_STEAM_API_DLL") {
        let path = std::path::PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let candidates = [
        exe_dir.join("steam_api64.dll"),
        exe_dir.join("_dlls").join("steam_api64.dll"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn proc_addr(module: HMODULE, name: &[u8]) -> Option<usize> {
    debug_assert_eq!(name.last(), Some(&0), "proc name must be NUL-terminated");
    unsafe { GetProcAddress(module, PCSTR(name.as_ptr())).map(|f| f as usize) }
}

// init steam once, cache the ISteamUser handle + accessors
fn ensure_init(state: &mut Option<SteamState>) -> Result<(), String> {
    if state.is_some() {
        return Ok(());
    }

    let dll = locate_dll().ok_or_else(|| {
        "steam_api64.dll not found (set HEBNIX_STEAM_API_DLL or place it next to the exe)"
            .to_string()
    })?;

    // SteamAPI_Init reads the appid from the SteamAppId env var (checked before
    // steam_appid.txt) so we set it here instead of writing a steam_appid.txt.
    // no stray file left around, shipping just the dll is enough. also steam
    // only looks for steam_appid.txt in the cwd, never next to the dll, so a
    // bundled file there would do nothing anyway.
    unsafe {
        std::env::set_var("SteamAppId", RL_STEAM_APPID.to_string());
        std::env::set_var("SteamGameId", RL_STEAM_APPID.to_string());
    }

    unsafe {
        let module = LoadLibraryW(PCWSTR(wide(&dll.to_string_lossy()).as_ptr()))
            .map_err(|e| format!("LoadLibraryW(steam_api64.dll) failed: {e}"))?;

        let p_init = proc_addr(module, b"SteamAPI_Init\0").ok_or("missing export SteamAPI_Init")?;
        let p_client = proc_addr(module, b"SteamClient\0").ok_or("missing export SteamClient")?;
        let p_get_user = proc_addr(module, b"SteamAPI_ISteamClient_GetISteamUser\0")
            .ok_or("missing export SteamAPI_ISteamClient_GetISteamUser")?;
        let p_get_ticket = proc_addr(module, b"SteamAPI_ISteamUser_GetAuthSessionTicket\0")
            .ok_or("missing export SteamAPI_ISteamUser_GetAuthSessionTicket")?;
        let p_get_steam_id = proc_addr(module, b"SteamAPI_ISteamUser_GetSteamID\0").unwrap_or(0);

        let init: FnInit = std::mem::transmute(p_init);
        if !init() {
            return Err(
                "SteamAPI_Init failed, is Steam running with an RL-owning account?".to_string(),
            );
        }

        let steam_client: FnSteamClient = std::mem::transmute(p_client);
        let p_steam_client = steam_client();
        if p_steam_client.is_null() {
            return Err("SteamClient() returned null".to_string());
        }

        // real user/pipe handles if we can get them, else 1 (like the C++ path)
        let h_user = proc_addr(module, b"SteamAPI_GetHSteamUser\0")
            .map(|p| (std::mem::transmute::<usize, FnGetHSteamUser>(p))())
            .unwrap_or(1);
        let h_pipe = proc_addr(module, b"SteamAPI_GetHSteamPipe\0")
            .map(|p| (std::mem::transmute::<usize, FnGetHSteamPipe>(p))())
            .unwrap_or(1);

        let get_user: FnGetISteamUser = std::mem::transmute(p_get_user);
        let mut steam_user = std::ptr::null_mut();
        for ver in [
            b"SteamUser021\0".as_slice(),
            b"SteamUser020\0",
            b"SteamUser019\0",
            b"SteamUser018\0",
            b"SteamUser017\0",
            b"SteamUser016\0",
        ] {
            steam_user = get_user(p_steam_client, h_user, h_pipe, ver.as_ptr());
            if !steam_user.is_null() {
                break;
            }
        }
        if steam_user.is_null() {
            return Err("could not obtain ISteamUser (no matching interface version)".to_string());
        }

        let steam_id = if p_get_steam_id != 0 {
            let get_id: FnGetSteamID = std::mem::transmute(p_get_steam_id);
            get_id(steam_user)
        } else {
            0
        };

        tracing::info!(steam_id, "eos/steam: Steam API initialised");
        *state = Some(SteamState {
            steam_user: steam_user as usize,
            p_get_ticket,
            p_get_steam_id,
            steam_id,
        });
    }
    Ok(())
}

/// fresh steam auth session ticket (uppercase hex) + the steamid
// GetAuthSessionTicket has a short cooldown so we retry with 1s/2s backoff like
// the python impl did.
pub fn get_ticket() -> Result<(String, String), String> {
    let mut guard = STATE.lock().unwrap();
    ensure_init(&mut guard)?;
    let st = guard.as_mut().expect("state initialised above");

    // grab the steamid now if we missed it at init
    if st.steam_id == 0 && st.p_get_steam_id != 0 {
        unsafe {
            let get_id: FnGetSteamID = std::mem::transmute(st.p_get_steam_id);
            st.steam_id = get_id(st.steam_user as *mut c_void);
        }
    }
    let steam_id = st.steam_id.to_string();

    let mut buf = [0u8; 4096];
    unsafe {
        let get_ticket: FnGetAuthSessionTicket = std::mem::transmute(st.p_get_ticket);
        for attempt in 0..3 {
            let mut ticket_size: u32 = 0;
            let handle = get_ticket(
                st.steam_user as *mut c_void,
                buf.as_mut_ptr(),
                buf.len() as i32,
                &mut ticket_size,
                std::ptr::null_mut(),
            );
            if handle != 0 && ticket_size > 0 {
                let hex = hex_upper(&buf[..ticket_size as usize]);
                return Ok((hex, steam_id));
            }
            if attempt < 2 {
                // backoff between retries: 1s then 2s
                std::thread::sleep(std::time::Duration::from_secs(attempt + 1));
            }
        }
    }
    Err("GetAuthSessionTicket returned no ticket (cooldown or not logged in)".to_string())
}

fn hex_upper(data: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(data.len() * 2);
    for &b in data {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xF) as usize] as char);
    }
    out
}
