//! screen colour sampling of the Rocket League window, for plugins.
//!
//! uses Windows Graphics Capture on RL's window, the same thing OBS's window
//! capture does. nothing is injected into the game and no memory is read, so
//! it stays on the same side of EAC as the rest of the overlay. window capture
//! only sees RL's own pixels, never the overlay drawn on top of it.
//!
//! the capture is lazy: it starts the first time something asks for a pixel,
//! and stops after IDLE_STOP with no requests.

mod capture;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;

use crate::messages::AppMsg;

/// stop capturing after this long with nobody asking
const IDLE_STOP: Duration = Duration::from_secs(10);
/// frames older than this count as unavailable (RL minimised, capture stalled)
const STALE_AFTER: Duration = Duration::from_secs(2);
/// frames copied to the CPU per second while capturing
const CAPTURE_FPS: u64 = 30;

/// a CPU copy of one captured frame. coordinates passed in are window pixels
/// from the top-left of GetWindowRect, the same space the overlay draws in;
/// offset_x/y map them onto the captured image.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// BGRA8, tightly packed (stride = width * 4)
    pub bgra: Vec<u8>,
    pub offset_x: i32,
    pub offset_y: i32,
    pub captured_at: Instant,
}

impl Frame {
    /// window pixel -> (r, g, b)
    pub fn pixel(&self, x: i32, y: i32) -> Option<(u8, u8, u8)> {
        let fx = x - self.offset_x;
        let fy = y - self.offset_y;
        if fx < 0 || fy < 0 || fx >= self.width as i32 || fy >= self.height as i32 {
            return None;
        }
        let i = (fy as usize * self.width as usize + fx as usize) * 4;
        Some((self.bgra[i + 2], self.bgra[i + 1], self.bgra[i]))
    }

    /// window rect clipped to the frame, in frame coordinates
    fn clip(&self, x: i32, y: i32, w: i32, h: i32) -> Option<(u32, u32, u32, u32)> {
        let x0 = (x - self.offset_x).max(0);
        let y0 = (y - self.offset_y).max(0);
        let x1 = (x - self.offset_x + w).min(self.width as i32);
        let y1 = (y - self.offset_y + h).min(self.height as i32);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some((x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32))
    }

    fn for_each_pixel(&self, rect: (u32, u32, u32, u32), mut f: impl FnMut(u8, u8, u8)) {
        let (x0, y0, w, h) = rect;
        // big regions are sampled on a grid, colour checks don't need every pixel
        let step = (((w as u64 * h as u64) / 40_000) as f64).sqrt().ceil().max(1.0) as u32;
        let mut y = y0;
        while y < y0 + h {
            let row = y as usize * self.width as usize;
            let mut x = x0;
            while x < x0 + w {
                let i = (row + x as usize) * 4;
                f(self.bgra[i + 2], self.bgra[i + 1], self.bgra[i]);
                x += step;
            }
            y += step;
        }
    }

    /// mean colour of a window rect
    pub fn average(&self, x: i32, y: i32, w: i32, h: i32) -> Option<(u8, u8, u8)> {
        let rect = self.clip(x, y, w, h)?;
        let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
        self.for_each_pixel(rect, |pr, pg, pb| {
            r += pr as u64;
            g += pg as u64;
            b += pb as u64;
            n += 1;
        });
        let n = n.max(1);
        Some(((r / n) as u8, (g / n) as u8, (b / n) as u8))
    }

    /// share (0..1) of pixels in the rect within `tol` of the colour on every channel
    pub fn match_color(&self, x: i32, y: i32, w: i32, h: i32, rgb: (u8, u8, u8), tol: u8) -> Option<f64> {
        let rect = self.clip(x, y, w, h)?;
        let (mut hits, mut n) = (0u64, 0u64);
        self.for_each_pixel(rect, |r, g, b| {
            n += 1;
            if r.abs_diff(rgb.0) <= tol && g.abs_diff(rgb.1) <= tol && b.abs_diff(rgb.2) <= tol {
                hits += 1;
            }
        });
        Some(hits as f64 / n.max(1) as f64)
    }

    /// true when the frame is (nearly) all black, which is what exclusive
    /// fullscreen gives window capture
    fn is_black(&self) -> bool {
        let n = (self.width as usize * self.height as usize).max(1);
        let step = (n / 2000).max(1);
        let mut i = 0;
        while i < n {
            let p = i * 4;
            if self.bgra[p] > 8 || self.bgra[p + 1] > 8 || self.bgra[p + 2] > 8 {
                return false;
            }
            i += step;
        }
        true
    }
}

pub struct Service {
    tx: Sender<AppMsg>,
    frame: Mutex<Option<Arc<Frame>>>,
    /// ms since `epoch` of the last request, drives the idle stop
    last_demand_ms: AtomicU64,
    epoch: Instant,
    worker_running: AtomicBool,
    /// exclusive fullscreen etc, capture runs but only gets black frames
    black: AtomicBool,
}

static SERVICE: OnceLock<Service> = OnceLock::new();

/// true while a plugin is actively reading pixels, so the app can tick and
/// repaint faster to keep the overlay in step with what's on screen
pub fn is_active() -> bool {
    SERVICE.get().is_some_and(|s| {
        s.worker_running.load(Ordering::Relaxed) && s.idle_for() < Duration::from_secs(1)
    })
}

/// call once at startup with the app's message sender
pub fn init(tx: Sender<AppMsg>) {
    let _ = SERVICE.set(Service {
        tx,
        frame: Mutex::new(None),
        last_demand_ms: AtomicU64::new(0),
        epoch: Instant::now(),
        worker_running: AtomicBool::new(false),
        black: AtomicBool::new(false),
    });
}

pub fn service() -> Option<&'static Service> {
    SERVICE.get()
}

impl Service {
    /// mark that someone wants frames, starting the worker if it's stopped
    pub fn touch(&'static self) {
        self.last_demand_ms
            .store(self.epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
        if !self.worker_running.swap(true, Ordering::AcqRel) {
            let spawned = std::thread::Builder::new()
                .name("screen-capture".into())
                .spawn(move || capture::run_worker(self));
            if spawned.is_err() {
                self.worker_running.store(false, Ordering::Release);
            }
        }
    }

    fn idle_for(&self) -> Duration {
        let now = self.epoch.elapsed().as_millis() as u64;
        Duration::from_millis(now.saturating_sub(self.last_demand_ms.load(Ordering::Relaxed)))
    }

    fn frame_interval(&self) -> Duration {
        Duration::from_millis(1000 / CAPTURE_FPS)
    }

    /// latest usable frame, touching the capture so it keeps running
    pub fn frame(&'static self) -> Option<Arc<Frame>> {
        self.touch();
        let frame = self.frame.lock().ok()?.clone()?;
        if frame.captured_at.elapsed() > STALE_AFTER || self.black.load(Ordering::Relaxed) {
            return None;
        }
        Some(frame)
    }

    fn publish(&self, frame: Frame) {
        let black = frame.is_black();
        if black && !self.black.swap(true, Ordering::Relaxed) {
            self.log("[Screen] Rocket League frames are black. Screen reading needs Borderless or Windowed mode, not Fullscreen.");
        } else if !black {
            self.black.store(false, Ordering::Relaxed);
        }
        if let Ok(mut slot) = self.frame.lock() {
            *slot = Some(Arc::new(frame));
        }
    }

    fn clear_frame(&self) {
        if let Ok(mut slot) = self.frame.lock() {
            *slot = None;
        }
    }

    fn log(&self, msg: &str) {
        let _ = self.tx.send(AppMsg::Log(msg.to_string()));
    }
}
