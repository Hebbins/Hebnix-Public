//! Per-plugin, asynchronous capture of user-selected top-level windows.
//!
//! Lua only sees opaque integer handles. Pixel buffers stay in native memory
//! and are replaced atomically by the worker at the requested (capped) rate.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetWindowDC, HALFTONE, HGDIOBJ, ReleaseDC, SRCCOPY,
    SelectObject, SetStretchBltMode, StretchBlt,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, EnumWindows, GetCursorInfo, GetIconInfo,
    GetSystemMetrics, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, HICON, ICONINFO, IsWindow, IsWindowVisible, SM_CXCURSOR, SM_CYCURSOR,
};
use windows::core::BOOL;

const MAX_CAPTURE_FPS: u32 = 30;
const MAX_FRAME_WIDTH: u32 = 1920;
const MAX_FRAME_HEIGHT: u32 = 1080;
const MAX_PRINT_SOURCE_WIDTH: u32 = 3840;
const MAX_PRINT_SOURCE_HEIGHT: u32 = 2160;
const MAX_CAPTURES_PER_PLUGIN: usize = 4;

static NEXT_FRAME_SERIAL: AtomicU64 = AtomicU64::new(1);
static NEXT_CAPTURE_HANDLE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub id: String,
    pub title: String,
    pub process: String,
}

/// A top-down, tightly packed, opaque BGRA frame.
#[derive(Debug)]
pub struct CapturedFrame {
    pub serial: u64,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

struct WorkerShared {
    stop: AtomicBool,
    latest: Mutex<Option<Arc<CapturedFrame>>>,
}

struct CaptureSession {
    shared: Arc<WorkerShared>,
    worker: Option<JoinHandle<()>>,
}

impl CaptureSession {
    fn stop(mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Owned by one HostCtx. Dropping the plugin drops this registry and joins all
/// capture workers, even if the plugin forgot to stop them in on_unload.
pub struct WindowCaptureRegistry {
    sessions: RefCell<HashMap<u64, CaptureSession>>,
    /// Only HWNDs returned by this plugin's most recent windows() call may be
    /// passed to start(). This rejects guessed/arbitrary native handles.
    enumerated: RefCell<HashMap<isize, WindowInfo>>,
    /// User decisions are remembered for this plugin runtime so a plugin
    /// cannot repeatedly prompt after a denial.
    decisions: RefCell<HashMap<String, bool>>,
}

impl Default for WindowCaptureRegistry {
    fn default() -> Self {
        Self {
            sessions: RefCell::new(HashMap::new()),
            enumerated: RefCell::new(HashMap::new()),
            decisions: RefCell::new(HashMap::new()),
        }
    }
}

impl WindowCaptureRegistry {
    pub fn windows(&self) -> Vec<WindowInfo> {
        let windows = enumerate_windows();
        let allowed = windows
            .iter()
            .filter_map(|window| parse_window_id(&window.id).map(|hwnd| (hwnd, window.clone())))
            .collect();
        *self.enumerated.borrow_mut() = allowed;
        windows
    }

    pub fn start(&self, window_id: &str, fps: u32, cursor: bool, plugin_name: &str) -> Option<u64> {
        let hwnd = parse_window_id(window_id)?;
        let expected_pid = window_process_id(hwnd)?;
        if !self.authorize(hwnd, plugin_name)
            || self.sessions.borrow().len() >= MAX_CAPTURES_PER_PLUGIN
            || !capture_target_is_valid(hwnd, expected_pid)
        {
            return None;
        }

        let handle = NEXT_CAPTURE_HANDLE.fetch_add(1, Ordering::Relaxed);
        let shared = Arc::new(WorkerShared {
            stop: AtomicBool::new(false),
            latest: Mutex::new(None),
        });
        let worker_shared = Arc::clone(&shared);
        let rate = fps.clamp(1, MAX_CAPTURE_FPS);
        let worker = std::thread::Builder::new()
            .name(format!("window-capture-{handle}"))
            .spawn(move || capture_loop(hwnd, expected_pid, rate, cursor, worker_shared))
            .ok()?;
        self.sessions.borrow_mut().insert(
            handle,
            CaptureSession {
                shared,
                worker: Some(worker),
            },
        );
        Some(handle)
    }

    fn authorize(&self, hwnd: isize, plugin_name: &str) -> bool {
        let Some(window) = self.enumerated.borrow().get(&hwnd).cloned() else {
            return false;
        };
        let Some(current) = window_info(HWND(hwnd as *mut _)) else {
            return false;
        };
        if current.title != window.title || current.process != window.process {
            return false;
        }
        let decision_key = format!("{}\0{}\0{}", window.id, window.title, window.process);
        if let Some(decision) = self.decisions.borrow().get(&decision_key) {
            return *decision;
        }
        let process = if window.process.is_empty() {
            "unknown process"
        } else {
            &window.process
        };
        let dialog = rfd::MessageDialog::new()
            .set_title("Allow window capture?")
            .set_description(format!(
                "The plugin “{plugin_name}” wants to capture this window:\n\n{} ({process})\n\nAllow for this plugin session?",
                window.title
            ))
            .set_level(rfd::MessageLevel::Warning)
            .set_buttons(rfd::MessageButtons::YesNo);
        let approved =
            crate::winutil::parent_message_dialog(dialog).show() == rfd::MessageDialogResult::Yes;
        self.decisions.borrow_mut().insert(decision_key, approved);
        approved
    }

    pub fn frame(&self, handle: u64) -> Option<Arc<CapturedFrame>> {
        let shared = Arc::clone(&self.sessions.borrow().get(&handle)?.shared);
        let frame = shared.latest.lock().ok()?.clone();
        frame
    }

    pub fn stop(&self, handle: u64) -> bool {
        let session = self.sessions.borrow_mut().remove(&handle);
        if let Some(session) = session {
            session.stop();
            true
        } else {
            false
        }
    }
}

impl Drop for WindowCaptureRegistry {
    fn drop(&mut self) {
        for (_, session) in self.sessions.get_mut().drain() {
            session.stop();
        }
    }
}

fn parse_window_id(id: &str) -> Option<isize> {
    id.strip_prefix("hwnd:")?
        .parse::<isize>()
        .ok()
        .filter(|v| *v != 0)
}

fn window_process_id(raw: isize) -> Option<u32> {
    let hwnd = HWND(raw as *mut _);
    let mut pid = 0;
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return None;
        }
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    (pid != 0).then_some(pid)
}

fn capture_target_is_valid(raw: isize, expected_pid: u32) -> bool {
    let hwnd = HWND(raw as *mut _);
    unsafe {
        IsWindow(Some(hwnd)).as_bool()
            && IsWindowVisible(hwnd).as_bool()
            && window_process_id(raw) == Some(expected_pid)
    }
}

struct EnumState {
    windows: Vec<WindowInfo>,
}

unsafe extern "system" fn enum_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let Some(window) = window_info(hwnd) else {
            return true.into();
        };
        let state = &mut *(lparam.0 as *mut EnumState);
        state.windows.push(window);
        true.into()
    }
}

fn window_info(hwnd: HWND) -> Option<WindowInfo> {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return None;
        }
        let mut title = vec![0u16; len as usize + 1];
        let copied = GetWindowTextW(hwnd, &mut title);
        if copied <= 0 {
            return None;
        }
        title.truncate(copied as usize);
        let title = String::from_utf16_lossy(&title);
        if title.trim().is_empty() {
            return None;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        Some(WindowInfo {
            id: format!("hwnd:{}", hwnd.0 as isize),
            title,
            process: process_name(pid).unwrap_or_default(),
        })
    }
}

fn enumerate_windows() -> Vec<WindowInfo> {
    let mut state = EnumState {
        windows: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(
            Some(enum_window),
            LPARAM(&mut state as *mut EnumState as isize),
        );
    }
    state.windows.sort_by(|a, b| {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    state.windows
}

fn process_name(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = vec![0u16; 32768];
        let mut len = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut len,
        );
        let _ = windows::Win32::Foundation::CloseHandle(process);
        result.ok()?;
        let path = String::from_utf16_lossy(&buffer[..len as usize]);
        Some(
            std::path::Path::new(&path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&path)
                .to_string(),
        )
    }
}

fn capture_loop(raw: isize, expected_pid: u32, fps: u32, cursor: bool, shared: Arc<WorkerShared>) {
    let interval = Duration::from_secs_f64(1.0 / fps as f64);
    while !shared.stop.load(Ordering::Acquire) {
        let started = Instant::now();
        if !capture_target_is_valid(raw, expected_pid) {
            if let Ok(mut latest) = shared.latest.lock() {
                *latest = None;
            }
            break;
        }
        let frame = capture_frame(raw, cursor);
        if let Ok(mut latest) = shared.latest.lock() {
            *latest = frame.map(Arc::new);
        }
        let remaining = interval.saturating_sub(started.elapsed());
        if !remaining.is_zero() {
            std::thread::sleep(remaining);
        }
    }
}

fn capture_frame(raw: isize, include_cursor: bool) -> Option<CapturedFrame> {
    let hwnd = HWND(raw as *mut _);
    unsafe {
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        let source_width = (rect.right - rect.left).max(0) as u32;
        let source_height = (rect.bottom - rect.top).max(0) as u32;
        if source_width == 0 || source_height == 0 {
            return None;
        }
        let scale = (MAX_FRAME_WIDTH as f64 / source_width as f64)
            .min(MAX_FRAME_HEIGHT as f64 / source_height as f64)
            .min(1.0);
        let width = (source_width as f64 * scale).round().max(1.0) as u32;
        let height = (source_height as f64 * scale).round().max(1.0) as u32;

        let source_dc = GetWindowDC(Some(hwnd));
        if source_dc.is_invalid() {
            return None;
        }
        let memory_dc = CreateCompatibleDC(Some(source_dc));
        if memory_dc.is_invalid() {
            ReleaseDC(Some(hwnd), source_dc);
            return None;
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap =
            CreateDIBSection(Some(source_dc), &info, DIB_RGB_COLORS, &mut bits, None, 0).ok();
        let Some(bitmap) = bitmap else {
            let _ = DeleteDC(memory_dc);
            ReleaseDC(Some(hwnd), source_dc);
            return None;
        };
        let old = SelectObject(memory_dc, HGDIOBJ(bitmap.0));
        let _ = SetStretchBltMode(memory_dc, HALFTONE);
        // PrintWindow asks DWM/the target to render its composed contents and
        // works for many hardware-accelerated windows whose window DC is just
        // black. Keep the window-DC copy as a fallback for apps that reject it.
        let copied = print_window_scaled(
            hwnd,
            source_dc,
            memory_dc,
            source_width,
            source_height,
            width,
            height,
        ) || StretchBlt(
            memory_dc,
            0,
            0,
            width as i32,
            height as i32,
            Some(source_dc),
            0,
            0,
            source_width as i32,
            source_height as i32,
            SRCCOPY | CAPTUREBLT,
        )
        .as_bool();
        if copied && include_cursor {
            draw_cursor(memory_dc, rect, scale);
        }
        let mut pixels = Vec::new();
        if copied && !bits.is_null() {
            pixels.extend_from_slice(std::slice::from_raw_parts(
                bits as *const u8,
                (width * height * 4) as usize,
            ));
            for pixel in pixels.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
        }
        SelectObject(memory_dc, old);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(memory_dc);
        ReleaseDC(Some(hwnd), source_dc);
        copied.then(|| CapturedFrame {
            serial: NEXT_FRAME_SERIAL.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            pixels,
        })
    }
}

unsafe fn print_window_scaled(
    hwnd: HWND,
    reference_dc: windows::Win32::Graphics::Gdi::HDC,
    target_dc: windows::Win32::Graphics::Gdi::HDC,
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> bool {
    unsafe {
        let flags = PRINT_WINDOW_FLAGS(2); // PW_RENDERFULLCONTENT
        if source_width == target_width && source_height == target_height {
            return PrintWindow(hwnd, target_dc, flags).as_bool();
        }
        if source_width > MAX_PRINT_SOURCE_WIDTH || source_height > MAX_PRINT_SOURCE_HEIGHT {
            return false;
        }

        let source_memory_dc = CreateCompatibleDC(Some(reference_dc));
        if source_memory_dc.is_invalid() {
            return false;
        }
        let source_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: source_width as i32,
                biHeight: -(source_height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut source_bits = std::ptr::null_mut();
        let source_bitmap = CreateDIBSection(
            Some(reference_dc),
            &source_info,
            DIB_RGB_COLORS,
            &mut source_bits,
            None,
            0,
        )
        .ok();
        let Some(source_bitmap) = source_bitmap else {
            let _ = DeleteDC(source_memory_dc);
            return false;
        };
        let old = SelectObject(source_memory_dc, HGDIOBJ(source_bitmap.0));
        let printed = PrintWindow(hwnd, source_memory_dc, flags).as_bool();
        let copied = printed
            && StretchBlt(
                target_dc,
                0,
                0,
                target_width as i32,
                target_height as i32,
                Some(source_memory_dc),
                0,
                0,
                source_width as i32,
                source_height as i32,
                SRCCOPY,
            )
            .as_bool();
        SelectObject(source_memory_dc, old);
        let _ = DeleteObject(HGDIOBJ(source_bitmap.0));
        let _ = DeleteDC(source_memory_dc);
        copied
    }
}

unsafe fn draw_cursor(target: windows::Win32::Graphics::Gdi::HDC, window_rect: RECT, scale: f64) {
    unsafe {
        let mut cursor = CURSORINFO {
            cbSize: std::mem::size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        if GetCursorInfo(&mut cursor).is_err() || cursor.flags != CURSOR_SHOWING {
            return;
        }
        let icon = HICON(cursor.hCursor.0);
        let mut info = ICONINFO::default();
        if GetIconInfo(icon, &mut info).is_err() {
            return;
        }
        let x = ((cursor.ptScreenPos.x - window_rect.left - info.xHotspot as i32) as f64 * scale)
            .round() as i32;
        let y = ((cursor.ptScreenPos.y - window_rect.top - info.yHotspot as i32) as f64 * scale)
            .round() as i32;
        let width = (GetSystemMetrics(SM_CXCURSOR) as f64 * scale)
            .round()
            .max(1.0) as i32;
        let height = (GetSystemMetrics(SM_CYCURSOR) as f64 * scale)
            .round()
            .max(1.0) as i32;
        let _ = DrawIconEx(target, x, y, icon, width, height, 0, None, DI_NORMAL);
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        }
        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_ids_are_strict() {
        assert_eq!(parse_window_id("hwnd:123"), Some(123));
        assert_eq!(parse_window_id("123"), None);
        assert_eq!(parse_window_id("hwnd:0"), None);
        assert_eq!(parse_window_id("hwnd:not-a-number"), None);
    }
}
