#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod dpi_fix;
mod hotkey;
mod lite_app;
#[path = "lite_messages.rs"]
mod messages;
mod monitor;
mod overlay;
mod plugins;
mod statsapi_ini;
mod theme;
mod update;
mod ui {
    pub mod console;
}
#[path = "lite_winutil.rs"]
mod winutil;

use lite_app::{DEFAULT_HEIGHT, DEFAULT_WIDTH, LiteApp, MIN_HEIGHT, MIN_WIDTH};

fn load_window_icon(base_dir: &std::path::Path) -> Option<eframe::egui::IconData> {
    let bytes = std::fs::read(base_dir.join("hebnix.ico"))
        .unwrap_or_else(|_| include_bytes!("../assets/hebnix.ico").to_vec());
    let image = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (width, height) = image.dimensions();
    Some(eframe::egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}

fn setup_logging(base_dir: &std::path::Path) {
    use tracing_subscriber::fmt::writer::MakeWriterExt;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,wgpu_core=warn,wgpu_hal=warn,naga=warn".into());
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(base_dir.join("hebnix-lite.log"))
    {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file).and(std::io::stdout))
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
}

fn dcomp_wgpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::wgpu;
    let mut options = eframe::egui_wgpu::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_setup {
        setup.instance_descriptor.backends = wgpu::Backends::DX12;
        setup
            .instance_descriptor
            .backend_options
            .dx12
            .presentation_system = wgpu::Dx12SwapchainKind::DxgiFromVisual;
    }
    options
}

fn main() -> eframe::Result {
    let base_dir = config::base_dir();
    setup_logging(&base_dir);
    let Some(_mutex) = winutil::acquire_single_instance() else {
        winutil::focus_existing_instance();
        return Ok(());
    };
    let config = config::Config::load(&base_dir);
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("Hebnix Lite")
        .with_inner_size([
            (config.window.width as f32).max(DEFAULT_WIDTH),
            (config.window.height as f32).max(DEFAULT_HEIGHT),
        ])
        .with_min_inner_size([MIN_WIDTH, MIN_HEIGHT])
        .with_transparent(true);
    if let Some(icon) = load_window_icon(&base_dir) {
        viewport = viewport.with_icon(icon);
    }
    eframe::run_native(
        "Hebnix Lite",
        eframe::NativeOptions {
            viewport,
            wgpu_options: dcomp_wgpu_options(),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(LiteApp::new(cc)))),
    )
}
