//! gdi fallback overlay: the old layered color-key window (python W2SOverlay).
//! only used when d3d11 init fails.
//!
//! cpu-rendered via the layered-window redirection surface, heavier than
//! dcomp. alpha ignored (color key = opaque or transparent), pure black
//! #000000 is the transparent key, so use near-black for dark fills.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, AlphaBlend, BI_RGB, BITMAPINFO,
    BITMAPINFOHEADER, BLACK_BRUSH, BLENDFUNCTION, BitBlt, CLIP_DEFAULT_PRECIS,
    CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreatePen,
    CreateSolidBrush, DEFAULT_CHARSET, DIB_RGB_COLORS, DeleteDC, DeleteObject, Ellipse, FillRect,
    GetDC, GetStockObject, HBITMAP, HBRUSH, HDC, HFONT, HGDIOBJ, HPEN, LineTo, MoveToEx,
    NULL_BRUSH, OUT_DEFAULT_PRECIS, PS_SOLID, Polygon, Rectangle, ReleaseDC, SRCCOPY, SelectObject,
    SetBkMode, SetTextAlign, SetTextColor, TA_CENTER, TA_LEFT, TA_RIGHT, TA_TOP,
    TEXT_ALIGN_OPTIONS, TRANSPARENT, TextOutW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_TOPMOST, IsWindowVisible, LWA_COLORKEY,
    RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SetLayeredWindowAttributes,
    SetWindowPos, ShowWindow, WNDCLASSW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::PCWSTR;

use super::Rgba;

const CLASS_NAME: &str = "HebnixW2SOverlayV1";

fn rgb(c: Rgba) -> COLORREF {
    COLORREF((c.2 as u32) << 16 | (c.1 as u32) << 8 | c.0 as u32)
}

struct GdiImage {
    bmp: HBITMAP,
    w: i32,
    h: i32,
}

thread_local! {
    static PENS: std::cell::RefCell<HashMap<(u32, i32), isize>> =
        std::cell::RefCell::new(HashMap::new());
    static BRUSHES: std::cell::RefCell<HashMap<u32, isize>> =
        std::cell::RefCell::new(HashMap::new());
    static FONTS: std::cell::RefCell<HashMap<(i32, bool), isize>> =
        std::cell::RefCell::new(HashMap::new());
    static IMAGES: std::cell::RefCell<HashMap<String, GdiImage>> =
        std::cell::RefCell::new(HashMap::new());
}

fn cached_pen(color: COLORREF, width: i32) -> HPEN {
    PENS.with(|m| {
        let mut m = m.borrow_mut();
        let key = (color.0, width);
        if let Some(&h) = m.get(&key) {
            return HPEN(h as *mut _);
        }
        let pen = unsafe { CreatePen(PS_SOLID, width, color) };
        m.insert(key, pen.0 as isize);
        pen
    })
}

fn cached_brush(color: COLORREF) -> HBRUSH {
    BRUSHES.with(|m| {
        let mut m = m.borrow_mut();
        if let Some(&h) = m.get(&color.0) {
            return HBRUSH(h as *mut _);
        }
        let brush = unsafe { CreateSolidBrush(color) };
        m.insert(color.0, brush.0 as isize);
        brush
    })
}

fn cached_font(size: i32, bold: bool) -> HFONT {
    FONTS.with(|m| {
        let mut m = m.borrow_mut();
        let key = (size, bold);
        if let Some(&h) = m.get(&key) {
            return HFONT(h as *mut _);
        }
        let face: Vec<u16> = "Segoe UI\0".encode_utf16().collect();
        let weight = if bold { 700 } else { 400 };
        let font = unsafe {
            CreateFontW(
                -size,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                ANTIALIASED_QUALITY,
                0,
                PCWSTR(face.as_ptr()),
            )
        };
        m.insert(key, font.0 as isize);
        font
    })
}

fn load_gdi_bitmap(path: &str) -> Option<GdiImage> {
    let img = image::open(path).ok()?.into_rgba8();
    let (width, height) = img.dimensions();
    let bmi = BITMAPINFO {
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

    let mut ptr = std::ptr::null_mut();
    unsafe {
        let hdc = GetDC(None);
        let hbm_res = CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut ptr, None, 0);
        ReleaseDC(None, hdc);

        let hbm = hbm_res.ok()?; // Handle Result mapping correctly

        if ptr.is_null() {
            return None;
        }

        let slice = std::slice::from_raw_parts_mut(ptr as *mut u8, (width * height * 4) as usize);
        let src = img.as_raw();

        // Convert RGBA memory slice to Premultiplied BGRA for windows GDI AlphaBlend
        for i in 0..(width * height) as usize {
            let r = src[i * 4];
            let g = src[i * 4 + 1];
            let b = src[i * 4 + 2];
            let a = src[i * 4 + 3];
            let af = a as f32 / 255.0;
            slice[i * 4] = (b as f32 * af) as u8;
            slice[i * 4 + 1] = (g as f32 * af) as u8;
            slice[i * 4 + 2] = (r as f32 * af) as u8;
            slice[i * 4 + 3] = a;
        }

        Some(GdiImage {
            bmp: hbm,
            w: width as i32,
            h: height as i32,
        })
    }
}

// Primitives (called via the overlay dispatcher with the frame's HDC)

pub fn line(hdc: HDC, x1: i32, y1: i32, x2: i32, y2: i32, color: Rgba, width: i32) {
    unsafe {
        let pen = cached_pen(rgb(color), width.max(1));
        let old = SelectObject(hdc, HGDIOBJ(pen.0));
        let _ = MoveToEx(hdc, x1, y1, None);
        let _ = LineTo(hdc, x2, y2);
        SelectObject(hdc, old);
    }
}

pub fn rect(hdc: HDC, x: i32, y: i32, w: i32, h: i32, color: Rgba, width: i32, filled: bool) {
    unsafe {
        let c = rgb(color);
        let pen = cached_pen(c, width.max(1));
        let old_pen = SelectObject(hdc, HGDIOBJ(pen.0));
        let brush = if filled {
            cached_brush(c)
        } else {
            HBRUSH(GetStockObject(NULL_BRUSH).0)
        };
        let old_brush = SelectObject(hdc, HGDIOBJ(brush.0));
        let _ = Rectangle(hdc, x, y, x + w, y + h);
        SelectObject(hdc, old_brush);
        SelectObject(hdc, old_pen);
    }
}

pub fn circle(hdc: HDC, x: i32, y: i32, radius: i32, color: Rgba, width: i32, filled: bool) {
    unsafe {
        let c = rgb(color);
        let pen = cached_pen(c, width.max(1));
        let old_pen = SelectObject(hdc, HGDIOBJ(pen.0));
        let brush = if filled {
            cached_brush(c)
        } else {
            HBRUSH(GetStockObject(NULL_BRUSH).0)
        };
        let old_brush = SelectObject(hdc, HGDIOBJ(brush.0));
        let _ = Ellipse(hdc, x - radius, y - radius, x + radius, y + radius);
        SelectObject(hdc, old_brush);
        SelectObject(hdc, old_pen);
    }
}

pub fn polygon(hdc: HDC, points: &[(i32, i32)], color: Rgba) {
    if points.len() < 3 {
        return;
    }
    unsafe {
        let brush = cached_brush(rgb(color));
        let old_brush = SelectObject(hdc, HGDIOBJ(brush.0));
        let old_pen = SelectObject(hdc, GetStockObject(NULL_BRUSH));
        let pts: Vec<POINT> = points.iter().map(|&(x, y)| POINT { x, y }).collect();
        let _ = Polygon(hdc, &pts);
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
    }
}

pub fn text(hdc: HDC, x: i32, y: i32, s: &str, color: Rgba, size: i32, halign: &str) {
    unsafe {
        let font = cached_font(size.max(6), false);
        let old = SelectObject(hdc, HGDIOBJ(font.0));
        let align = match halign {
            "center" => TA_CENTER,
            "right" => TA_RIGHT,
            _ => TA_LEFT,
        };
        SetTextAlign(hdc, TEXT_ALIGN_OPTIONS(align.0 | TA_TOP.0));
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, rgb(color));
        let wide: Vec<u16> = s.encode_utf16().collect();
        let _ = TextOutW(hdc, x, y, &wide);
        SelectObject(hdc, old);
    }
}

pub fn image(hdc: HDC, path: &str, x: i32, y: i32, w: i32, h: i32, opacity: f32) {
    IMAGES.with(|m| {
        let mut m = m.borrow_mut();
        if !m.contains_key(path) {
            if let Some(img) = load_gdi_bitmap(path) {
                m.insert(path.to_string(), img);
            }
        }
        if let Some(img) = m.get(path) {
            unsafe {
                let dc = CreateCompatibleDC(Some(hdc));
                let old = SelectObject(dc, HGDIOBJ(img.bmp.0));
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: (opacity.clamp(0.0, 1.0) * 255.0) as u8,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let _ = AlphaBlend(hdc, x, y, w, h, dc, 0, 0, img.w, img.h, blend);
                SelectObject(dc, old);
                let _ = DeleteDC(dc);
            }
        }
    });
}

static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// the raw win32 + gdi game overlay window
pub struct GdiOverlay {
    hwnd: Option<HWND>,
    last_rect: Option<(i32, i32, i32, i32)>,
    back_dc: Option<HDC>,
    back_bmp: Option<HBITMAP>,
    back_old: HGDIOBJ,
    back_w: i32,
    back_h: i32,
}

impl GdiOverlay {
    pub fn new() -> Self {
        Self {
            hwnd: None,
            last_rect: None,
            back_dc: None,
            back_bmp: None,
            back_old: HGDIOBJ(std::ptr::null_mut()),
            back_w: 0,
            back_h: 0,
        }
    }

    fn ensure_window(&mut self) {
        if self.hwnd.is_some() {
            return;
        }
        unsafe {
            let class_wide: Vec<u16> = format!("{CLASS_NAME}\0").encode_utf16().collect();
            let hinstance = GetModuleHandleW(None).ok();
            let hinst = hinstance.map(|h| windows::Win32::Foundation::HINSTANCE(h.0));

            if !CLASS_REGISTERED.swap(true, Ordering::SeqCst) {
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(wnd_proc),
                    hInstance: hinst.unwrap_or_default(),
                    lpszClassName: PCWSTR(class_wide.as_ptr()),
                    ..Default::default()
                };
                RegisterClassW(&wc);
            }

            let ex_style = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW;
            let empty: Vec<u16> = "\0".encode_utf16().collect();
            let hwnd = CreateWindowExW(
                ex_style,
                PCWSTR(class_wide.as_ptr()),
                PCWSTR(empty.as_ptr()),
                WS_POPUP,
                0,
                0,
                100,
                100,
                None,
                None,
                hinst,
                None,
            );
            if let Ok(hwnd) = hwnd {
                // Pure black becomes transparent (color-key), like Python.
                let _ = SetLayeredWindowAttributes(hwnd, rgb(Rgba(0, 0, 0, 255)), 0, LWA_COLORKEY);
                super::register_hwnd(hwnd);
                self.hwnd = Some(hwnd);
            }
        }
    }

    fn ensure_backbuffer(&mut self, ref_dc: HDC, w: i32, h: i32) {
        if self.back_dc.is_some() && self.back_w == w && self.back_h == h {
            return;
        }
        unsafe {
            self.free_backbuffer();
            let dc = CreateCompatibleDC(Some(ref_dc));
            let bmp = CreateCompatibleBitmap(ref_dc, w, h);
            let old = SelectObject(dc, HGDIOBJ(bmp.0));
            self.back_dc = Some(dc);
            self.back_bmp = Some(bmp);
            self.back_old = old;
            self.back_w = w;
            self.back_h = h;
        }
    }

    fn free_backbuffer(&mut self) {
        unsafe {
            if let Some(dc) = self.back_dc.take() {
                SelectObject(dc, self.back_old);
                if let Some(bmp) = self.back_bmp.take() {
                    let _ = DeleteObject(HGDIOBJ(bmp.0));
                }
                let _ = DeleteDC(dc);
            }
        }
        self.back_w = 0;
        self.back_h = 0;
    }

    /// position over rect, run draw_fn on the back-buffer HDC, blit to screen
    pub fn frame(&mut self, rect: (i32, i32, i32, i32), draw_fn: impl FnOnce(HDC, f32, f32)) {
        self.ensure_window();
        let Some(hwnd) = self.hwnd else {
            return;
        };
        let (left, top, right, bottom) = rect;
        let (w, h) = (right - left, bottom - top);
        if w <= 0 || h <= 0 {
            return;
        }

        unsafe {
            if self.last_rect != Some(rect) {
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), left, top, w, h, SWP_NOACTIVATE);
                self.last_rect = Some(rect);
            }

            let win_dc = GetDC(Some(hwnd));
            self.ensure_backbuffer(win_dc, w, h);
            let Some(back_dc) = self.back_dc else {
                ReleaseDC(Some(hwnd), win_dc);
                return;
            };

            // Clear to the color-key (black = transparent).
            let full = RECT {
                left: 0,
                top: 0,
                right: w,
                bottom: h,
            };
            FillRect(back_dc, &full, HBRUSH(GetStockObject(BLACK_BRUSH).0));

            draw_fn(back_dc, w as f32, h as f32);

            // Composite onto the layered window.
            let _ = BitBlt(win_dc, 0, 0, w, h, Some(back_dc), 0, 0, SRCCOPY);
            ReleaseDC(Some(hwnd), win_dc);

            // Real state, not a cached bool, the monitor thread may have
            // hidden the window when the game lost focus.
            if !IsWindowVisible(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }

    /// hide the overlay (game not focused / no overlay plugins)
    pub fn hide(&mut self) {
        if let Some(hwnd) = self.hwnd {
            unsafe {
                if IsWindowVisible(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
        }
    }
}

impl Drop for GdiOverlay {
    fn drop(&mut self) {
        self.free_backbuffer();
        if let Some(hwnd) = self.hwnd.take() {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
    }
}
