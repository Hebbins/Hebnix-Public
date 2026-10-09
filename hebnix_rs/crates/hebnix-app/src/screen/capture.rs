//! Windows Graphics Capture of the RL window on a worker thread. frames are
//! copied to a staging texture and then into a CPU buffer so lookups from
//! Lua never touch the GPU.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windows::Foundation::TimeSpan;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureAccess,
    GraphicsCaptureAccessKind, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
use windows::core::{Interface, Result};

use super::{Frame, IDLE_STOP, Service};

const PIXEL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
/// how long grab() waits for a frame newer than the last one
const FRESH_WAIT: Duration = Duration::from_millis(60);

struct Capture {
    hwnd: HWND,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    winrt_device: IDirect3DDevice,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    size: SizeInt32,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
}

impl Capture {
    fn start(hwnd: HWND, interval: Duration) -> Result<Self> {
        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        let device: ID3D11Device = device.ok_or_else(windows::core::Error::empty)?;
        let context: ID3D11DeviceContext = context.ok_or_else(windows::core::Error::empty)?;
        let dxgi: IDXGIDevice = device.cast()?;
        let winrt_device: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?;

        let interop =
            windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(hwnd)? };
        let size = item.Size()?;
        let pool =
            Direct3D11CaptureFramePool::CreateFreeThreaded(&winrt_device, PIXEL_FORMAT, 2, size)?;
        let session = pool.CreateCaptureSession(&item)?;
        // all optional, older Windows builds just keep their defaults
        let _ = session.SetIsCursorCaptureEnabled(false);
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetMinUpdateInterval(TimeSpan::from(interval));
        session.StartCapture()?;

        Ok(Self {
            hwnd,
            device,
            context,
            winrt_device,
            pool,
            session,
            size,
            staging: None,
        })
    }

    /// newest frame copied to the CPU, None if RL hasn't drawn one (minimised)
    fn grab(&mut self) -> Result<Option<Frame>> {
        // the pool holds frames from whenever it last had room, drain those so
        // what we read is current
        let mut latest: Option<Direct3D11CaptureFrame> = None;
        while let Ok(frame) = self.pool.TryGetNextFrame() {
            latest = Some(frame);
        }
        let had_stale = latest.is_some();
        drop(latest.take());
        if had_stale {
            let deadline = Instant::now() + FRESH_WAIT;
            while Instant::now() < deadline {
                if let Ok(frame) = self.pool.TryGetNextFrame() {
                    latest = Some(frame);
                    break;
                }
                std::thread::sleep(Duration::from_millis(4));
            }
        }
        let Some(frame) = latest else {
            return Ok(None);
        };

        let content = frame.ContentSize()?;
        if content.Width != self.size.Width || content.Height != self.size.Height {
            // window resized. this frame is still fine to read, later ones come at the new size
            self.size = content;
            self.pool
                .Recreate(&self.winrt_device, PIXEL_FORMAT, 2, content)?;
        }

        let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        let width = (content.Width.max(0) as u32).min(desc.Width);
        let height = (content.Height.max(0) as u32).min(desc.Height);
        if width == 0 || height == 0 {
            return Ok(None);
        }

        let staging = self.staging_for(&desc)?;
        let mut bgra = vec![0u8; width as usize * height as usize * 4];
        unsafe {
            self.context.CopyResource(&staging, &texture);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let src = mapped.pData as *const u8;
            let row_bytes = width as usize * 4;
            for y in 0..height as usize {
                let from = src.add(y * mapped.RowPitch as usize);
                std::ptr::copy_nonoverlapping(from, bgra.as_mut_ptr().add(y * row_bytes), row_bytes);
            }
            self.context.Unmap(&staging, 0);
        }
        drop(frame);

        let (offset_x, offset_y) = capture_offset(self.hwnd);
        Ok(Some(Frame {
            width,
            height,
            bgra,
            offset_x,
            offset_y,
            captured_at: Instant::now(),
        }))
    }

    fn staging_for(&mut self, src: &D3D11_TEXTURE2D_DESC) -> Result<ID3D11Texture2D> {
        if let Some((tex, w, h)) = &self.staging {
            if *w == src.Width && *h == src.Height {
                return Ok(tex.clone());
            }
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: src.Width,
            Height: src.Height,
            MipLevels: 1,
            ArraySize: 1,
            Format: src.Format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut tex = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut tex))? };
        let tex = tex.ok_or_else(windows::core::Error::empty)?;
        self.staging = Some((tex.clone(), src.Width, src.Height));
        Ok(tex)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

/// window capture starts at the visible frame (DWM extended bounds), the
/// overlay draws from GetWindowRect which includes the invisible resize
/// border in windowed mode. returns where the capture's (0,0) sits in overlay
/// coordinates. both are 0 in borderless.
fn capture_offset(hwnd: HWND) -> (i32, i32) {
    unsafe {
        let mut window = RECT::default();
        let mut visible = RECT::default();
        if GetWindowRect(hwnd, &mut window).is_err() {
            return (0, 0);
        }
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut visible as *mut RECT as *mut core::ffi::c_void,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_err()
        {
            return (0, 0);
        }
        (visible.left - window.left, visible.top - window.top)
    }
}

/// ask once for border-free capture. unpackaged apps get it without a prompt
/// on Windows 11, elsewhere this fails and the default applies.
fn request_borderless() {
    if let Ok(op) = GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless) {
        let _ = op.join();
    }
}

pub(super) fn run_worker(svc: &'static Service) {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
        svc.log("[Screen] Screen capture is not supported on this version of Windows.");
    }
    request_borderless();

    let mut capture: Option<Capture> = None;
    let mut last_grab = Instant::now() - Duration::from_secs(60);
    let mut hwnd_checked = Instant::now() - Duration::from_secs(60);
    let mut retry_at = Instant::now();
    let mut warned_start = false;

    loop {
        if svc.idle_for() > IDLE_STOP {
            drop(capture.take());
            svc.clear_frame();
            svc.worker_running.store(false, Ordering::Release);
            // a request between the idle check and the store above saw the
            // worker as running and didn't spawn one, so pick it up here
            if svc.idle_for() <= IDLE_STOP && !svc.worker_running.swap(true, Ordering::AcqRel) {
                continue;
            }
            return;
        }

        if hwnd_checked.elapsed() >= Duration::from_secs(1) {
            hwnd_checked = Instant::now();
            match hebnix_sdk::process::rocket_league_hwnd() {
                None => {
                    if capture.take().is_some() {
                        svc.clear_frame();
                    }
                }
                Some(hwnd) => {
                    let same = capture.as_ref().is_some_and(|c| c.hwnd == hwnd);
                    if !same && Instant::now() >= retry_at {
                        drop(capture.take());
                        match Capture::start(hwnd, svc.frame_interval()) {
                            Ok(c) => {
                                capture = Some(c);
                                warned_start = false;
                            }
                            Err(e) => {
                                if !warned_start {
                                    svc.log(&format!(
                                        "[Screen] Could not capture the Rocket League window: {e}"
                                    ));
                                    warned_start = true;
                                }
                                retry_at = Instant::now() + Duration::from_secs(5);
                            }
                        }
                    }
                }
            }
        }

        let mut grab_now = |capture: &mut Option<Capture>| {
            if let Some(c) = capture.as_mut() {
                match c.grab() {
                    Ok(Some(frame)) => svc.publish(frame),
                    Ok(None) => {}
                    Err(_) => {
                        // device lost / window gone, rebuild on the next hwnd check
                        *capture = None;
                        hwnd_checked = Instant::now() - Duration::from_secs(60);
                    }
                }
            }
        };

        if last_grab.elapsed() >= svc.frame_interval() {
            last_grab = Instant::now();
            grab_now(&mut capture);
        }

        std::thread::sleep(Duration::from_millis(15));
    }
}
