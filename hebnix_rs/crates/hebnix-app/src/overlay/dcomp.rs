//! gpu-composited click-through overlay (DirectComposition + d3d11 + direct2d).
//!
//! pipeline: a WS_EX_NOREDIRECTIONBITMAP layered popup (no gdi redirection
//! surface) -> a dxgi composition swapchain drawn with direct2d -> a
//! DirectComposition visual tree binding the swapchain to the window. dwm (and
//! a hw overlay plane on capable gpus) blends the premult-alpha surface over
//! the game. no per-frame cpu blit, true per-pixel alpha, no color key.
//!
//! external window, nothing injected into RL, so it's anti-cheat safe. the
//! only cost is a topmost translucent window makes dwm compose the game
//! instead of flipping it exclusively (small + gpu-side, unlike the gdi path).

use windows::Win32::Foundation::{E_FAIL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED,
    D2D1_FIGURE_END_CLOSED, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_OPTIONS_TARGET,
    D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_ROUNDED_RECT, D2D1CreateFactory, ID2D1Bitmap1, ID2D1DeviceContext, ID2D1Factory1,
    ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWriteCreateFactory,
    IDWriteFactory,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory2,
    IDXGISwapChain1,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_TOPMOST, IsWindowVisible, RegisterClassW,
    SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SetWindowPos, ShowWindow, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{Interface, PCWSTR, Result};
use windows_numerics::Vector2;

use super::Rgba;

const CLASS_NAME: &str = "HebnixDCompOverlayV1";

fn color(c: Rgba) -> D2D1_COLOR_F {
    // Direct2D takes straight (non-premultiplied) 0-1 floats; the target
    // surface is premultiplied, D2D converts on write.
    D2D1_COLOR_F {
        r: c.0 as f32 / 255.0,
        g: c.1 as f32 / 255.0,
        b: c.2 as f32 / 255.0,
        a: c.3 as f32 / 255.0,
    }
}

/// Helper to load a hardware bitmap into Direct2D from standard image files
fn load_d2d_bitmap(ctx: &ID2D1DeviceContext, path: &str) -> Option<ID2D1Bitmap1> {
    let img = image::open(path).ok()?.into_rgba8();
    let (width, height) = img.dimensions();
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_R8G8B8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
        colorContext: std::mem::ManuallyDrop::new(None),
    };

    // Premultiply alpha manually before feeding to Direct2D
    let mut pixels = img.into_raw();
    for chunk in pixels.chunks_exact_mut(4) {
        let a = chunk[3] as f32 / 255.0;
        chunk[0] = (chunk[0] as f32 * a) as u8;
        chunk[1] = (chunk[1] as f32 * a) as u8;
        chunk[2] = (chunk[2] as f32 * a) as u8;
    }

    unsafe {
        ctx.CreateBitmap(
            D2D_SIZE_U { width, height },
            Some(pixels.as_ptr() as *const _),
            width * 4,
            &props,
        )
        .ok()
    }
}

/// a borrowed direct2d surface for one frame, handed to plugins via the overlay
/// dispatcher. every call reuses one solid-color brush.
pub struct D2dCanvas {
    ctx: ID2D1DeviceContext,
    brush: ID2D1SolidColorBrush,
    dwrite: IDWriteFactory,
    d2d_factory: ID2D1Factory1,
    image_cache: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, ID2D1Bitmap1>>>,
}

impl D2dCanvas {
    fn set_color(&self, c: Rgba) {
        unsafe { self.brush.SetColor(&color(c)) };
    }

    pub fn line(&self, x1: f32, y1: f32, x2: f32, y2: f32, c: Rgba, width: f32) {
        self.set_color(c);
        unsafe {
            self.ctx.DrawLine(
                Vector2 { X: x1, Y: y1 },
                Vector2 { X: x2, Y: y2 },
                &self.brush,
                width.max(1.0),
                None,
            );
        }
    }

    pub fn rect(
        &self,
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
        let r = D2D_RECT_F {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        };
        let radius = radius.max(0.0).min(w.abs() * 0.5).min(h.abs() * 0.5);
        unsafe {
            if radius > 0.0 {
                let rounded = D2D1_ROUNDED_RECT {
                    rect: r,
                    radiusX: radius,
                    radiusY: radius,
                };
                if filled {
                    self.set_color(fill);
                    self.ctx.FillRoundedRectangle(&rounded, &self.brush);
                }
                if !filled || width > 0.0 {
                    self.set_color(border);
                    self.ctx
                        .DrawRoundedRectangle(&rounded, &self.brush, width.max(1.0), None);
                }
            } else {
                if filled {
                    self.set_color(fill);
                    self.ctx.FillRectangle(&r, &self.brush);
                }
                if !filled || width > 0.0 {
                    self.set_color(border);
                    self.ctx
                        .DrawRectangle(&r, &self.brush, width.max(1.0), None);
                }
            }
        }
    }

    pub fn circle(&self, x: f32, y: f32, radius: f32, c: Rgba, width: f32, filled: bool) {
        self.set_color(c);
        let e = D2D1_ELLIPSE {
            point: Vector2 { X: x, Y: y },
            radiusX: radius,
            radiusY: radius,
        };
        unsafe {
            if filled {
                self.ctx.FillEllipse(&e, &self.brush);
            } else {
                self.ctx.DrawEllipse(&e, &self.brush, width.max(1.0), None);
            }
        }
    }

    pub fn polygon(&self, points: &[(f32, f32)], c: Rgba) {
        if points.len() < 3 {
            return;
        }
        self.set_color(c);
        unsafe {
            // Build a filled path geometry from the points.
            let Ok(geometry) = self.d2d_factory.CreatePathGeometry() else {
                return;
            };
            let Ok(sink) = geometry.Open() else { return };
            sink.BeginFigure(
                Vector2 {
                    X: points[0].0,
                    Y: points[0].1,
                },
                D2D1_FIGURE_BEGIN_FILLED,
            );
            let rest: Vec<Vector2> = points[1..]
                .iter()
                .map(|&(x, y)| Vector2 { X: x, Y: y })
                .collect();
            sink.AddLines(&rest);
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            let _ = sink.Close();
            self.ctx.FillGeometry(&geometry, &self.brush, None);
        }
    }

    pub fn text(&self, x: f32, y: f32, s: &str, c: Rgba, size: f32, halign: &str) {
        self.set_color(c);
        let family: Vec<u16> = "Segoe UI\0".encode_utf16().collect();
        let locale: Vec<u16> = "en-us\0".encode_utf16().collect();
        unsafe {
            let Ok(format) = self.dwrite.CreateTextFormat(
                PCWSTR(family.as_ptr()),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size.max(6.0),
                PCWSTR(locale.as_ptr()),
            ) else {
                return;
            };
            let alignment = match halign {
                "center" => DWRITE_TEXT_ALIGNMENT_CENTER,
                "right" => DWRITE_TEXT_ALIGNMENT_TRAILING,
                _ => DWRITE_TEXT_ALIGNMENT_LEADING,
            };
            let _ = format.SetTextAlignment(alignment);

            let wide: Vec<u16> = s.encode_utf16().collect();
            // Give the layout rect generous width so alignment has room; the
            // draw position is the rect's left/top (or center for center).
            let layout = D2D_RECT_F {
                left: x - 4000.0 * (alignment == DWRITE_TEXT_ALIGNMENT_CENTER) as u32 as f32,
                top: y,
                right: x + 4000.0,
                bottom: y + size.max(6.0) * 1.6,
            };
            let layout = if alignment == DWRITE_TEXT_ALIGNMENT_TRAILING {
                D2D_RECT_F {
                    left: x - 4000.0,
                    right: x,
                    ..layout
                }
            } else {
                layout
            };
            self.ctx.DrawText(
                &wide,
                &format,
                &layout,
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    pub fn image(&self, path: &str, x: f32, y: f32, w: f32, h: f32, opacity: f32) {
        let mut cache = self.image_cache.borrow_mut();
        let bitmap = if let Some(bmp) = cache.get(path) {
            Some(bmp.clone())
        } else {
            let bmp = load_d2d_bitmap(&self.ctx, path);
            if let Some(ref b) = bmp {
                cache.insert(path.to_string(), b.clone());
            }
            bmp
        };

        if let Some(bmp) = bitmap {
            let dest = D2D_RECT_F {
                left: x,
                top: y,
                right: x + w,
                bottom: y + h,
            };
            unsafe {
                self.ctx.DrawBitmap(
                    &bmp,
                    Some(&dest as *const _),
                    opacity.clamp(0.0, 1.0),
                    D2D1_INTERPOLATION_MODE_LINEAR,
                    None,
                    None,
                );
            }
        }
    }
}

/// gpu-composited overlay window + its device stack
pub struct DcompOverlay {
    hwnd: HWND,
    last_rect: Option<(i32, i32, i32, i32)>,
    size: (u32, u32),
    #[allow(dead_code)]
    d3d: ID3D11Device,
    dxgi_factory: IDXGIFactory2,
    d2d_factory: ID2D1Factory1,
    d2d_ctx: ID2D1DeviceContext,
    dwrite: IDWriteFactory,
    dcomp_device: IDCompositionDevice,
    #[allow(dead_code)]
    dcomp_target: IDCompositionTarget,
    dcomp_visual: IDCompositionVisual,
    swapchain: Option<IDXGISwapChain1>,
    image_cache: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, ID2D1Bitmap1>>>,
}

impl DcompOverlay {
    pub fn new() -> Result<Self> {
        unsafe {
            let hwnd = create_window()?;
            super::register_hwnd(hwnd);

            // D3D11 device (hardware, WARP fallback), BGRA for D2D interop.
            let mut device: Option<ID3D11Device> = None;
            let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
            let mut hr = D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                Default::default(),
                flags,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            );
            if hr.is_err() {
                hr = D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_WARP,
                    Default::default(),
                    flags,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    None,
                );
            }
            hr?;
            let d3d = device.ok_or_else(|| windows::core::Error::from_hresult(E_FAIL))?;
            let dxgi_device: IDXGIDevice = d3d.cast()?;

            // D2D device + single-threaded factory.
            let d2d_factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2d_device = d2d_factory.CreateDevice(&dxgi_device)?;
            let d2d_ctx = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;

            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;

            let dxgi_factory: IDXGIFactory2 = CreateDXGIFactory2(Default::default())?;

            // DirectComposition device + target bound to the window.
            let dcomp_device: IDCompositionDevice = DCompositionCreateDevice(&dxgi_device)?;
            let dcomp_target = dcomp_device.CreateTargetForHwnd(hwnd, true)?;
            let dcomp_visual = dcomp_device.CreateVisual()?;
            dcomp_target.SetRoot(&dcomp_visual)?;

            Ok(Self {
                hwnd,
                last_rect: None,
                size: (0, 0),
                d3d,
                dxgi_factory,
                d2d_factory,
                d2d_ctx,
                dwrite,
                dcomp_device,
                dcomp_target,
                dcomp_visual,
                swapchain: None,
                image_cache: std::rc::Rc::new(std::cell::RefCell::new(
                    std::collections::HashMap::new(),
                )),
            })
        }
    }

    /// (Re)create the composition swap chain + bind it to the visual.
    fn ensure_swapchain(&mut self, w: u32, h: u32) -> Result<()> {
        if self.swapchain.is_some() && self.size == (w, h) {
            return Ok(());
        }
        unsafe {
            // Drop the old target bitmap before resizing.
            self.d2d_ctx.SetTarget(None);
            self.swapchain = None;

            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: w,
                Height: h,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: false.into(),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                Flags: 0,
            };
            let swapchain = self
                .dxgi_factory
                .CreateSwapChainForComposition(&self.d3d, &desc, None)?;

            self.dcomp_visual.SetContent(&swapchain)?;
            self.dcomp_device.Commit()?;
            self.swapchain = Some(swapchain);
            self.size = (w, h);
        }
        Ok(())
    }

    /// bind the swapchain back buffer as the d2d target
    fn bind_target(&self) -> Result<()> {
        unsafe {
            let Some(sc) = &self.swapchain else {
                return Err(windows::core::Error::from_hresult(E_FAIL));
            };
            let surface: windows::Win32::Graphics::Dxgi::IDXGISurface = sc.GetBuffer(0)?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let bitmap = self
                .d2d_ctx
                .CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
            self.d2d_ctx.SetTarget(&bitmap);
        }
        Ok(())
    }

    /// position over rect, render draw_fn via direct2d, present
    pub fn frame(&mut self, rect: (i32, i32, i32, i32), draw_fn: impl FnOnce(D2dCanvas, f32, f32)) {
        let (left, top, right, bottom) = rect;
        let (w, h) = ((right - left).max(1) as u32, (bottom - top).max(1) as u32);

        unsafe {
            if self.last_rect != Some(rect) {
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    left,
                    top,
                    w as i32,
                    h as i32,
                    SWP_NOACTIVATE,
                );
                self.last_rect = Some(rect);
            }
            if self.ensure_swapchain(w, h).is_err() || self.bind_target().is_err() {
                return;
            }

            self.d2d_ctx.BeginDraw();
            self.d2d_ctx.Clear(Some(&D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0, // fully transparent, true per-pixel alpha
            }));

            if let Ok(brush) = self.d2d_ctx.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
                None,
            ) {
                let canvas = D2dCanvas {
                    ctx: self.d2d_ctx.clone(),
                    brush,
                    dwrite: self.dwrite.clone(),
                    d2d_factory: self.d2d_factory.clone(),
                    image_cache: self.image_cache.clone(),
                };
                draw_fn(canvas, w as f32, h as f32);
            }

            let _ = self.d2d_ctx.EndDraw(None, None);
            self.d2d_ctx.SetTarget(None);
            if let Some(sc) = &self.swapchain {
                let _ = sc.Present(1, Default::default());
            }
            let _ = self.dcomp_device.Commit();

            // Query the real state: the monitor thread may have hidden the
            // window behind our back (game lost focus), so a cached bool
            // would desync.
            if !IsWindowVisible(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }

    pub fn hide(&mut self) {
        unsafe {
            if IsWindowVisible(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }
}

impl Drop for DcompOverlay {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

use std::sync::atomic::{AtomicBool, Ordering};
static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // Never own a hit-test: every click/hover falls through to whatever is
    // underneath (the game), independent of window styles.
    const WM_NCHITTEST: u32 = 0x0084;
    const HTTRANSPARENT: isize = -1;
    if msg == WM_NCHITTEST {
        return LRESULT(HTTRANSPARENT);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn create_window() -> Result<HWND> {
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

        // NOREDIRECTIONBITMAP: no GDI surface; content comes only from the
        // composition swap chain. LAYERED+TRANSPARENT: click-through, the
        // TRANSPARENT bit only passes input through when LAYERED is also set
        // (we never call SetLayeredWindowAttributes; with no redirection
        // bitmap there is nothing for it to affect).
        let ex_style = WS_EX_NOREDIRECTIONBITMAP
            | WS_EX_LAYERED
            | WS_EX_TRANSPARENT
            | WS_EX_TOPMOST
            | WS_EX_TOOLWINDOW;
        let empty: Vec<u16> = "\0".encode_utf16().collect();
        CreateWindowExW(
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
        )
    }
}
