#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod patcher;
mod ball {
    pub use crate::patcher::ball::*;
}
mod boost_patcher {
    pub use crate::patcher::boost_patcher::*;
}
mod config;
mod cosmetic_thumbnail {
    pub use crate::patcher::cosmetic_thumbnail::*;
}
mod cosmetic_upk {
    pub use crate::patcher::cosmetic_upk::*;
}
mod decal_patcher {
    pub use crate::patcher::decal_patcher::*;
}
mod dpi_fix;
mod hotkey;
mod messages;
mod monitor;
mod overlay;
mod patch_core {
    pub use crate::patcher::patch_core::*;
}
mod plugins;
mod presets;
mod spoofer;
mod statsapi_ini;
mod swapper {
    pub use crate::patcher::swapper::*;
}
mod theme;
mod tray;
mod ui;
mod update;
mod upk_keys {
    pub use crate::patcher::upk_keys::*;
}
mod winutil;

use app::HebnixApp;

fn load_window_icon(base_dir: &std::path::Path) -> Option<eframe::egui::IconData> {
    let bytes =
        std::fs::read(base_dir.join("hebnix.ico")).unwrap_or_else(|_| tray::EMBEDDED_ICON.to_vec());
    let img = image::load_from_memory(&bytes).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(eframe::egui::IconData {
        rgba: rgba.into_raw(),
        width,
        height,
    })
}

fn setup_logging(base_dir: &std::path::Path) {
    use tracing_subscriber::fmt::writer::MakeWriterExt;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,wgpu_core=warn,wgpu_hal=warn,naga=warn".into());

    // sync writer, nothing buffered so the log survives a hard crash.
    // volume's low enough that sync writes don't matter.
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(base_dir.join("hebnix.log"))
        .ok();

    match log_file {
        Some(file) => {
            let file = std::sync::Mutex::new(file);
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(file.and(std::io::stdout))
                .init();
        }
        None => {
            tracing_subscriber::fmt().with_env_filter(filter).init();
        }
    }
}

/// on panic dump msg + backtrace to crash.txt next to the exe, also log it,
/// then fall through to the default hook.
fn setup_panic_hook(base_dir: &std::path::Path) {
    let crash_path = base_dir.join("crash.txt");
    let restore_dir = base_dir.to_path_buf();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        spoofer::disable_proxy(&restore_dir);
        let _ = spoofer::hosts::clear();
        let backtrace = std::backtrace::Backtrace::force_capture();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let thread = std::thread::current();
        let text = format!(
            "=== PANIC (unix time {ts}, thread {:?}) ===\n{info}\n\nbacktrace:\n{backtrace}\n\n",
            thread.name().unwrap_or("<unnamed>")
        );
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&crash_path)
        {
            use std::io::Write;
            let _ = f.write_all(text.as_bytes());
        }
        tracing::error!("PANIC: {info}");
        default_hook(info);
    }));
}

/// dwm always composites a dcomp visual, an hwnd swapchain only sometimes.
/// pairs with WS_EX_NOREDIRECTIONBITMAP in the patched egui-winit.
fn dcomp_wgpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::wgpu;

    let mut options = eframe::egui_wgpu::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_setup {
        let desc = &mut setup.instance_descriptor;
        desc.backends = wgpu::Backends::DX12;
        desc.backend_options.dx12.presentation_system = wgpu::Dx12SwapchainKind::DxgiFromVisual;
    }
    options
}

fn main() -> eframe::Result {
    let base_dir = config::base_dir();
    setup_logging(&base_dir);
    setup_panic_hook(&base_dir);
    tracing::info!("Hebnix {} starting", app::APP_VERSION);

    let cfg = config::Config::load(&base_dir);

    // relaunch elevated if the user asked for it. --no-elevate comes back when
    // uac was declined
    let skip_elevate = std::env::args().any(|a| a == spoofer::SKIP_ELEVATE_ARG);
    if cfg.settings.run_as_admin && !skip_elevate && !spoofer::is_admin() {
        if spoofer::spawn_elevated_relaunch() {
            return Ok(());
        }
        tracing::warn!("couldnt spawn the elevated relaunch helper");
    }

    // single instance guard
    let Some(_mutex) = winutil::acquire_single_instance() else {
        winutil::focus_existing_instance();
        return Ok(());
    };

    spoofer::restore_if_crashed(&base_dir);

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("Hebnix")
        .with_inner_size([
            (cfg.window.width as f32).max(app::MIN_WIDTH),
            (cfg.window.height as f32).max(app::MIN_HEIGHT),
        ])
        .with_min_inner_size([app::MIN_WIDTH, app::MIN_HEIGHT])
        .with_transparent(true)
        .with_visible(!cfg.settings.start_in_tray);
    if let Some(icon) = load_window_icon(&base_dir) {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        wgpu_options: dcomp_wgpu_options(),
        ..Default::default()
    };

    eframe::run_native(
        "Hebnix",
        options,
        Box::new(|cc| Ok(Box::new(HebnixApp::new(cc)))),
    )
}
