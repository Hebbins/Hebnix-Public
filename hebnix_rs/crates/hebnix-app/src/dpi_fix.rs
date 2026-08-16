//! WM_DPICHANGED: take the rect windows puts in lparam.
//!
//! winit recomputes its own instead, and mid drag that can land back over the
//! old monitor and fire the next one (winit #4041). so we run ahead of winit,
//! let it have the message for the scale factor, then put the geometry back.
//! viewports open whenever a plugin asks, hence the sweep.

use std::cell::Cell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Shell::{DefSubclassProc, GetWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumThreadWindows, GetClassNameW, HWND_TOP, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos,
};
use windows::core::BOOL;

const WM_DPICHANGED: u32 = 0x02E0;

const SUBCLASS_ID: usize = 0x4842_5844; // "HBXD"

const WINIT_CLASS: &str = "Window Class"; // winit's default, main + viewports

// nothing gets dragged in the first frames of existing
const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

thread_local! {
    /// nested ones come from winit moving us across the boundary
    static IN_DPI_CHANGE: Cell<bool> = const { Cell::new(false) };
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    unsafe {
        if msg != WM_DPICHANGED {
            return DefSubclassProc(hwnd, msg, wparam, lparam);
        }
        if IN_DPI_CHANGE.with(Cell::get) {
            return LRESULT(0);
        }

        let suggested = *(lparam.0 as *const RECT); // winit moves us before we use it

        IN_DPI_CHANGE.with(|f| f.set(true));
        let res = DefSubclassProc(hwnd, msg, wparam, lparam);
        let placed = SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            suggested.left,
            suggested.top,
            suggested.right - suggested.left,
            suggested.bottom - suggested.top,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        IN_DPI_CHANGE.with(|f| f.set(false));

        if let Err(e) = placed {
            tracing::debug!("dpi_fix: couldnt apply the suggested rect: {e}");
        }
        res
    }
}

/// no-op if it already carries ours
pub fn install(hwnd: HWND) {
    unsafe {
        let mut existing = 0usize; // pdwRefData isnt optional, comctl32 writes to it
        if GetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, Some(&mut existing)).as_bool()
        {
            return;
        }
        if SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, 0).as_bool() {
            tracing::debug!(hwnd = ?hwnd.0, "dpi_fix: guard installed");
        } else {
            tracing::warn!("dpi_fix: SetWindowSubclass failed, mixed dpi drags will jump");
        }
    }
}

/// subclass any window on this thread we havent yet
pub fn install_on_all_windows() {
    thread_local! {
        static LAST_SWEEP: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
    }
    let due = LAST_SWEEP.with(|t| match t.get() {
        Some(at) if at.elapsed() < SWEEP_INTERVAL => false,
        _ => {
            t.set(Some(std::time::Instant::now()));
            true
        }
    });
    if !due {
        return;
    }
    unsafe {
        let _ = EnumThreadWindows(GetCurrentThreadId(), Some(enum_proc), LPARAM(0));
    }
}

unsafe extern "system" fn enum_proc(hwnd: HWND, _lparam: LPARAM) -> BOOL {
    unsafe {
        if is_winit_window(hwnd) {
            install(hwnd);
        }
    }
    BOOL(1)
}

/// tray-icon and global-hotkey park message-only windows on this thread too
unsafe fn is_winit_window(hwnd: HWND) -> bool {
    unsafe {
        let mut buf = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut buf);
        if n <= 0 {
            return false;
        }
        String::from_utf16_lossy(&buf[..n as usize]) == WINIT_CLASS
    }
}
