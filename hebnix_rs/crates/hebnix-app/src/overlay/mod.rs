//! click-through game overlay, two backends behind one interface.
//!
//! dcomp (preferred): gpu-composited transparent window via DirectComposition
//! (d3d11 + direct2d + premult alpha), same trick discord/game bar use, near
//! zero cost, full #RRGGBBAA alpha. gdi (fallback when d3d11 init fails): the
//! old layered color-key window (python W2SOverlay), opaque only, pure black =
//! transparent.
//!
//! plugins draw via the free fns here (line, rect, ..) which dispatch to
//! whichever backend's canvas is live this frame. all on the main thread.

pub mod dcomp;
pub mod gdi;

use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};

/// active overlay window, readable from any thread. the monitor thread uses it
/// to force-hide the overlay the instant the game loses focus, independent of
/// egui's loop (which can stall while the main window's hidden, leaving a
/// stale overlay up).
static OVERLAY_HWND: AtomicIsize = AtomicIsize::new(0);

pub(crate) fn register_hwnd(hwnd: windows::Win32::Foundation::HWND) {
    OVERLAY_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
}

/// hide the overlay now if it's visible. safe from any thread.
pub fn enforce_hidden() {
    let raw = OVERLAY_HWND.load(Ordering::Relaxed);
    if raw == 0 {
        return;
    }
    let hwnd = windows::Win32::Foundation::HWND(raw as *mut _);
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{IsWindowVisible, SW_HIDE, ShowWindow};
        if IsWindowVisible(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// rgba color, straight (non-premultiplied) alpha 0-255
#[derive(Clone, Copy)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

enum Canvas {
    Gdi(windows::Win32::Graphics::Gdi::HDC),
    D2d(dcomp::D2dCanvas),
}

thread_local! {
    static CANVAS: RefCell<Option<Canvas>> = const { RefCell::new(None) };
}

fn with_canvas(f: impl FnOnce(&Canvas)) {
    CANVAS.with(|c| {
        if let Some(canvas) = c.borrow().as_ref() {
            f(canvas);
        }
    });
}

// Drawing primitives called from the Lua `draw` table. No-ops outside a
// frame (canvas unset).

pub fn line(x1: f32, y1: f32, x2: f32, y2: f32, color: Rgba, width: f32) {
    with_canvas(|canvas| match canvas {
        Canvas::Gdi(hdc) => gdi::line(
            *hdc,
            x1 as i32,
            y1 as i32,
            x2 as i32,
            y2 as i32,
            color,
            width as i32,
        ),
        Canvas::D2d(c) => c.line(x1, y1, x2, y2, color, width),
    });
}

pub fn rect(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    fill: Rgba,
    border: Rgba,
    width: f32,
    filled: bool,
    radius: f32,
) {
    with_canvas(|canvas| match canvas {
        Canvas::Gdi(hdc) => gdi::rect(
            *hdc,
            x as i32,
            y as i32,
            w as i32,
            h as i32,
            fill,
            border,
            width as i32,
            filled,
            radius as i32,
        ),
        Canvas::D2d(c) => c.rect(x, y, w, h, fill, border, width, filled, radius),
    });
}

pub fn circle(x: f32, y: f32, radius: f32, color: Rgba, width: f32, filled: bool) {
    with_canvas(|canvas| match canvas {
        Canvas::Gdi(hdc) => gdi::circle(
            *hdc,
            x as i32,
            y as i32,
            radius as i32,
            color,
            width as i32,
            filled,
        ),
        Canvas::D2d(c) => c.circle(x, y, radius, color, width, filled),
    });
}

pub fn text(x: f32, y: f32, s: &str, color: Rgba, size: f32, halign: &str) {
    with_canvas(|canvas| match canvas {
        Canvas::Gdi(hdc) => gdi::text(*hdc, x as i32, y as i32, s, color, size as i32, halign),
        Canvas::D2d(c) => c.text(x, y, s, color, size, halign),
    });
}

pub fn polygon(points: &[(f32, f32)], color: Rgba) {
    if points.len() < 3 {
        return;
    }
    with_canvas(|canvas| match canvas {
        Canvas::Gdi(hdc) => {
            let pts: Vec<(i32, i32)> = points.iter().map(|&(x, y)| (x as i32, y as i32)).collect();
            gdi::polygon(*hdc, &pts, color);
        }
        Canvas::D2d(c) => c.polygon(points, color),
    });
}

pub fn image(path: &str, x: f32, y: f32, w: f32, h: f32, opacity: f32) {
    with_canvas(|canvas| match canvas {
        Canvas::Gdi(hdc) => gdi::image(*hdc, path, x as i32, y as i32, w as i32, h as i32, opacity),
        Canvas::D2d(c) => c.image(path, x, y, w, h, opacity),
    });
}

/// the overlay window, whichever backend the machine supports
pub enum Overlay {
    Dcomp(dcomp::DcompOverlay),
    Gdi(gdi::GdiOverlay),
}

impl Overlay {
    pub fn new() -> Self {
        match dcomp::DcompOverlay::new() {
            Ok(o) => {
                tracing::info!("game overlay: DirectComposition backend");
                Overlay::Dcomp(o)
            }
            Err(e) => {
                tracing::warn!("DirectComposition overlay unavailable ({e}); using GDI fallback");
                Overlay::Gdi(gdi::GdiOverlay::new())
            }
        }
    }

    /// position over rect, let draw_fn paint, present. draw_fn gets the overlay
    /// size in pixels.
    pub fn frame(&mut self, rect: (i32, i32, i32, i32), draw_fn: impl FnOnce(f32, f32)) {
        match self {
            Overlay::Dcomp(o) => {
                o.frame(rect, |canvas, w, h| {
                    CANVAS.with(|c| *c.borrow_mut() = Some(Canvas::D2d(canvas)));
                    draw_fn(w, h);
                    CANVAS.with(|c| *c.borrow_mut() = None);
                });
            }
            Overlay::Gdi(o) => {
                o.frame(rect, |hdc, w, h| {
                    CANVAS.with(|c| *c.borrow_mut() = Some(Canvas::Gdi(hdc)));
                    draw_fn(w, h);
                    CANVAS.with(|c| *c.borrow_mut() = None);
                });
            }
        }
    }

    pub fn hide(&mut self) {
        match self {
            Overlay::Dcomp(o) => o.hide(),
            Overlay::Gdi(o) => o.hide(),
        }
    }
}
