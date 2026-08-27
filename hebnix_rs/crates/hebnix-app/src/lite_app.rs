use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{self, Color32};
use hebnix_sdk::stats::{StatsClient, StatsEvent, websocket::WsStatsClient};
use serde_json::Value;

use crate::config::Config;
use crate::hotkey::ToggleHotkey;
use crate::messages::AppMsg;
use crate::monitor::{Monitor, MonitorShared};
use crate::overlay::Overlay;
use crate::plugins::PluginManager;
use crate::{dpi_fix, statsapi_ini, theme, winutil};

pub const APP_VERSION: &str = "2.1.2";
pub const DEFAULT_WIDTH: f32 = 760.0;
pub const DEFAULT_HEIGHT: f32 = 520.0;
pub const MIN_WIDTH: f32 = 520.0;
pub const MIN_HEIGHT: f32 = 360.0;
#[derive(Clone)]
enum ImageState {
    Loading,
    Ready(Arc<[u8]>),
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Console,
    Plugins,
    Settings,
    About,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Hebnix,
    Plugin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HebnixSettingsTab {
    Interface,
    Directories,
    System,
}

#[derive(Default)]
struct InstallModal {
    open: bool,
    catalog_open: bool,
    fetching: bool,
    downloading_id: Option<String>,
    error: Option<String>,
    catalog: Vec<Value>,
    search: String,
    page: usize,
    images: HashMap<String, ImageState>,
}

pub struct LiteApp {
    base_dir: std::path::PathBuf,
    themes_dir: std::path::PathBuf,
    plugin_dir: std::path::PathBuf,
    config: Config,
    tx: Sender<AppMsg>,
    rx: Receiver<AppMsg>,
    stats: Arc<StatsClient>,
    ws_stats: Arc<WsStatsClient>,
    stats_tx: Sender<StatsEvent>,
    monitor: Monitor,
    plugin_mgr: PluginManager,
    hotkey: Option<ToggleHotkey>,
    tab: Tab,
    settings_tab: SettingsTab,
    hebnix_settings_tab: HebnixSettingsTab,
    selected_settings_plugin: Option<String>,
    install_modal: InstallModal,
    console: crate::ui::console::ConsoleState,
    theme_options: Vec<String>,
    packet_rate: Option<String>,
    port_value: Option<String>,
    web_port_value: Option<String>,
    packet_rate_edit: String,
    port_edit: String,
    web_port_edit: String,
    current_api_port: u16,
    last_rl_open: bool,
    last_api_open: bool,
    currently_connected: bool,
    first_status: bool,
    status_text: String,
    status_color: Color32,
    topmost: bool,
    hidden: bool,
    capturing_hotkey: bool,
    window_mode: Option<hebnix_sdk::save_file::WindowMode>,
    last_size: (u32, u32),
    overlay: Overlay,
    overlay_rect: Option<(i32, i32, i32, i32)>,
    overlay_rect_checked: Option<std::time::Instant>,
    plugin_monitor_size: (f32, f32),
    plugin_monitor_checked: Option<std::time::Instant>,
    startup_enabled: bool,
    fullscreen_notice: bool,
    fullscreen_notice_dismissed: bool,
    statsapi_notice: Option<String>,
    update_info: Option<crate::update::UpdateInfo>,
    update_downloading: bool,
    update_error: Option<String>,
    changelog_popup: Option<crate::update::ChangelogEntry>,
    launch_path_notice: bool,
}

impl LiteApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let base_dir = crate::config::base_dir();
        let themes_dir = base_dir.join("themes");
        let plugin_dir = base_dir.join("plugins");
        let _ = std::fs::create_dir_all(&themes_dir);
        let _ = std::fs::create_dir_all(&plugin_dir);

        let mut config = Config::load(&base_dir);
        if theme::apply_theme(
            &cc.egui_ctx,
            &themes_dir,
            &themes_dir,
            &config.settings.theme,
        )
        .is_err()
        {
            let _ = theme::apply_theme(&cc.egui_ctx, &themes_dir, &themes_dir, "Dark");
            config.settings.theme = "Dark".to_string();
        }
        theme::apply_window_opacity(&cc.egui_ctx, config.settings.window_opacity);

        let (tx, rx) = crossbeam_channel::unbounded();
        let show_changelog = base_dir.join(".first").exists();
        let launch_marker = base_dir.join(".launch");
        if launch_marker.exists() {
            let _ = std::fs::remove_file(&launch_marker);
        }
        let tx_update = tx.clone();
        let ctx_update = cc.egui_ctx.clone();
        std::thread::Builder::new()
            .name("lite-update-checker".into())
            .spawn(move || {
                if let Ok(info) = crate::update::fetch_info(APP_VERSION) {
                    let _ = tx_update.send(AppMsg::AppUpdateFetched {
                        result: Ok(info.update),
                    });
                    if show_changelog {
                        let _ = tx_update.send(AppMsg::ChangelogFetched {
                            result: Ok(info.newest_changelog),
                        });
                    }
                    ctx_update.request_repaint();
                }
            })
            .ok();
        let stats = Arc::new(StatsClient::new("127.0.0.1", 49123));
        let (stats_tx, stats_rx) = crossbeam_channel::unbounded();
        {
            let tx = tx.clone();
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                while let Ok(event) = stats_rx.recv() {
                    let _ = tx.send(AppMsg::GameEvent(event));
                    ctx.request_repaint();
                }
            });
        }
        let ws_stats = Arc::new(WsStatsClient::new("127.0.0.1", 49124));
        let monitor = Monitor::start(
            MonitorShared {
                api_port: 49123,
                statsapi_path: config.settings.statsapi_path.clone(),
                rl_path: config.settings.rl_path.clone(),
            },
            tx.clone(),
            cc.egui_ctx.clone(),
        );
        let mut plugin_mgr = PluginManager::new(plugin_dir.clone(), tx.clone(), APP_VERSION);
        plugin_mgr.refresh(&mut config, true);
        let _ = config.save(&base_dir);

        let mut hotkey = ToggleHotkey::new();
        if let Some(hotkey) = &mut hotkey {
            hotkey.rebind(&config.settings.hotkey);
        }
        {
            let tx = tx.clone();
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                let receiver = global_hotkey::GlobalHotKeyEvent::receiver();
                while let Ok(event) = receiver.recv() {
                    if event.state() == global_hotkey::HotKeyState::Pressed {
                        if winutil::main_window_hidden() {
                            winutil::request_show();
                        }
                        let _ = tx.send(AppMsg::ToggleVisibility);
                        ctx.request_repaint();
                    }
                }
            });
        }

        if let Some(hwnd) = winutil::main_window_hwnd() {
            dpi_fix::install(hwnd);
            winutil::install_minimize_hook(hwnd, &cc.egui_ctx);
        }

        let mut app = Self {
            base_dir,
            themes_dir,
            plugin_dir,
            config,
            tx,
            rx,
            stats,
            ws_stats,
            stats_tx,
            monitor,
            plugin_mgr,
            hotkey,
            tab: Tab::Console,
            settings_tab: SettingsTab::Hebnix,
            hebnix_settings_tab: HebnixSettingsTab::Interface,
            selected_settings_plugin: None,
            install_modal: InstallModal::default(),
            console: crate::ui::console::ConsoleState::default(),
            theme_options: Vec::new(),
            packet_rate: None,
            port_value: None,
            web_port_value: None,
            packet_rate_edit: String::new(),
            port_edit: String::new(),
            web_port_edit: String::new(),
            current_api_port: 49123,
            last_rl_open: false,
            last_api_open: false,
            currently_connected: false,
            first_status: true,
            status_text: "⌛ Waiting for Rocket League...".to_string(),
            status_color: Color32::from_rgb(0xdc, 0xe4, 0xee),
            topmost: false,
            hidden: false,
            capturing_hotkey: false,
            window_mode: None,
            last_size: (0, 0),
            overlay: Overlay::new(),
            overlay_rect: None,
            overlay_rect_checked: None,
            plugin_monitor_size: (1920.0, 1080.0),
            plugin_monitor_checked: None,
            startup_enabled: winutil::is_startup_enabled(),
            fullscreen_notice: false,
            fullscreen_notice_dismissed: false,
            statsapi_notice: None,
            update_info: None,
            update_downloading: false,
            update_error: None,
            changelog_popup: None,
            launch_path_notice: false,
        };
        app.theme_options = theme::list_themes(&app.themes_dir);
        app.refresh_statsapi();
        app.check_plugin_updates();
        app
    }

    fn save_config(&mut self) {
        if let Err(error) = self.config.save(&self.base_dir) {
            self.console
                .write(format!("[Console] Could not save config: {error}"));
        }
    }

    fn refresh_statsapi(&mut self) {
        let path = statsapi_ini::resolve_ini_path(
            &self.config.settings.statsapi_path,
            &self.config.settings.rl_path,
        );
        let (rate, port, web_port) = statsapi_ini::read_ini(&path);
        self.packet_rate_edit = rate.clone().unwrap_or_default();
        self.port_edit = port.clone().unwrap_or_else(|| "49123".to_string());
        self.web_port_edit = web_port.clone().unwrap_or_else(|| "49124".to_string());
        self.current_api_port = port
            .as_deref()
            .and_then(|value| value.parse().ok())
            .unwrap_or(49123);
        self.packet_rate = rate;
        self.port_value = port;
        self.web_port_value = web_port;
        self.statsapi_notice = match self
            .packet_rate
            .as_deref()
            .and_then(|rate| rate.parse::<i64>().ok())
        {
            None | Some(0) => {
                Some("StatsAPI is not configured. PacketSendRate must be set to 20.".to_string())
            }
            Some(rate) if rate <= 10 => Some(format!(
                "PacketSendRate is {rate}, which is too low. Set it to 20."
            )),
            Some(rate) if rate < 20 => Some(format!(
                "PacketSendRate is {rate}. The recommended value is 20."
            )),
            Some(rate) if rate > 20 => Some(format!(
                "PacketSendRate is {rate}. The recommended value is 20."
            )),
            _ => None,
        };
    }

    fn update_ini_setting(&mut self, key: &str, value: &str) {
        let path = statsapi_ini::resolve_ini_path(
            &self.config.settings.statsapi_path,
            &self.config.settings.rl_path,
        );
        match statsapi_ini::update_ini_setting(&path, key, value) {
            Ok(()) => self
                .console
                .write(format!("[Console] Set {key} to {value}.")),
            Err(error) => self
                .console
                .write(format!("[Console] Failed to set {key}: {error}")),
        }
        self.refresh_statsapi();
    }

    fn handle_status(&mut self, rl_open: bool, api_open: bool) {
        self.last_rl_open = rl_open;
        self.last_api_open = api_open;
        let ready = rl_open && api_open;
        if ready && !self.currently_connected {
            self.currently_connected = true;
            self.status_text = "✔ Rocket League Connected".to_string();
            self.status_color = Color32::from_rgb(0x2e, 0xcc, 0x71);
            self.stats.set_port(self.current_api_port);
            self.stats.start(self.stats_tx.clone());
            let web_port = self
                .web_port_value
                .as_deref()
                .and_then(|v| v.parse().ok())
                .unwrap_or(49124);
            self.ws_stats.set_port(web_port);
            let (tx, rx) = crossbeam_channel::unbounded();
            std::thread::spawn(move || while rx.recv().is_ok() {});
            self.ws_stats.start(tx);
            self.plugin_mgr
                .dispatch_simple("GameConnected", serde_json::json!({}));
            self.console
                .write("[Monitor] Rocket League & StatsAPI detected. Starting listener.");
        } else if !ready && (self.currently_connected || self.first_status) {
            let was_connected = self.currently_connected;
            self.currently_connected = false;
            if was_connected {
                self.stats.stop();
                self.ws_stats.stop();
                self.plugin_mgr.dispatch_simple(
                    "GameDisconnected",
                    serde_json::json!({"reason": "connection_lost"}),
                );
                self.console
                    .write("[Monitor] Connection lost. Halting listener...");
            }
        }
        if !self.currently_connected {
            self.status_text = if rl_open {
                "⌛ Rocket League starting..."
            } else {
                "⌛ Waiting for Rocket League..."
            }
            .to_string();
            self.status_color = Color32::from_rgb(0xdc, 0xe4, 0xee);
        }
        self.plugin_mgr.shared.borrow_mut().rl_connected = self.currently_connected;
        self.first_status = false;
    }

    fn handle_messages(&mut self, ctx: &egui::Context) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                AppMsg::Log(line) => self.console.write(line),
                AppMsg::GameEvent(event) => self.handle_game_event(event),
                AppMsg::RlStatus {
                    rl_open,
                    api_open,
                    root_dir,
                } => {
                    if let Some(root) = root_dir {
                        let platform = hebnix_sdk::process::detect_platform(Path::new(&root));
                        let changed = !self
                            .config
                            .settings
                            .rl_path
                            .trim_end_matches(['\\', '/'])
                            .eq_ignore_ascii_case(root.trim_end_matches(['\\', '/']));
                        let newly_confirmed = !self.config.settings.rl_path_confirmed;
                        if changed {
                            self.config.settings.rl_path = root.clone();
                            self.config.settings.statsapi_path = Path::new(&root)
                                .join("TAGame")
                                .join("Config")
                                .join("DefaultStatsAPI.ini")
                                .to_string_lossy()
                                .to_string();
                            self.refresh_statsapi();
                        }
                        if changed || newly_confirmed {
                            self.config.settings.rl_path_confirmed = true;
                            self.save_config();
                        }
                        self.plugin_mgr.shared.borrow_mut().platform =
                            platform.as_str().to_string();
                    }
                    self.handle_status(rl_open, api_open);
                }
                AppMsg::StatsApiInitialised => {
                    self.refresh_statsapi();
                    self.console.write("[Monitor] StatsAPI initialised. Restart Rocket League to use the changed setting.");
                }
                AppMsg::WindowMode(mode) => {
                    self.window_mode = Some(mode);
                    self.fullscreen_notice = mode == hebnix_sdk::save_file::WindowMode::Fullscreen
                        && !self.config.settings.suppress_fullscreen_warning
                        && !self.fullscreen_notice_dismissed;
                }
                AppMsg::ToggleVisibility => {
                    self.set_hidden(ctx, !self.hidden);
                }
                AppMsg::HotkeyCaptured(value) => {
                    self.capturing_hotkey = false;
                    if let Some(key) = value {
                        self.update_hotkey(&key);
                    }
                }
                AppMsg::Topmost(topmost) => {
                    if self.topmost != topmost {
                        self.topmost = topmost;
                        winutil::set_main_window_topmost(topmost);
                    }
                }
                AppMsg::PluginHttpRes {
                    slug,
                    req_id,
                    status,
                    body,
                } => self
                    .plugin_mgr
                    .on_http_response(&slug, &req_id, status, &body),
                AppMsg::PluginHttpDownloadRes {
                    slug,
                    req_id,
                    status,
                    body,
                } => self
                    .plugin_mgr
                    .on_http_download_response(&slug, &req_id, status, &body),
                AppMsg::PluginHttpRedirectRes {
                    slug,
                    req_id,
                    status,
                    location,
                } => self
                    .plugin_mgr
                    .on_http_redirect_response(&slug, &req_id, status, &location),
                AppMsg::PluginFetch { result } => {
                    self.install_modal.fetching = false;
                    match result {
                        Ok(catalog) => {
                            self.install_modal.catalog =
                                catalog.as_array().cloned().unwrap_or_default();
                            self.install_modal.error = (!catalog.is_array()).then(|| {
                                "Plugin catalog returned an invalid response.".to_string()
                            });
                        }
                        Err(error) => self.install_modal.error = Some(error),
                    }
                }
                AppMsg::PluginImage { key, bytes } => {
                    let state = if bytes.is_empty() {
                        ImageState::Failed
                    } else {
                        ImageState::Ready(Arc::from(bytes))
                    };
                    self.install_modal.images.insert(key, state);
                }
                AppMsg::PluginDownloadDone { result } => {
                    self.install_modal.downloading_id = None;
                    match result {
                        Ok(message) => {
                            self.console.write(format!("[Console] {message}"));
                            self.plugin_mgr.refresh(&mut self.config, true);
                            self.save_config();
                        }
                        Err(error) => self
                            .console
                            .write(format!("[Console] Plugin installation failed: {error}")),
                    }
                }
                AppMsg::AppUpdateFetched { result } => {
                    if let Ok(Some(info)) = result {
                        self.update_info = Some(info);
                    }
                }
                AppMsg::ChangelogFetched { result } => {
                    if let Ok(Some(entry)) = result {
                        self.changelog_popup = Some(entry);
                        let _ = std::fs::remove_file(self.base_dir.join(".first"));
                    }
                }
                AppMsg::AppUpdateFailed { error } => {
                    self.update_downloading = false;
                    self.update_error = Some(error);
                }
                AppMsg::PluginUpdatesFound { result } => match result {
                    Ok(updates) => self.start_plugin_updates(updates),
                    Err(error) => self
                        .console
                        .write(format!("[Core] Plugin update check failed: {error}")),
                },
                AppMsg::PluginAutoUpdateDone {
                    slug,
                    was_enabled,
                    result,
                } => match result {
                    Ok(message) => {
                        self.console.write(format!("[Console] {message}"));
                        self.plugin_mgr.refresh(&mut self.config, true);
                        if was_enabled {
                            self.plugin_mgr.set_enabled(&slug, true, &mut self.config);
                        }
                        self.save_config();
                    }
                    Err(error) => self
                        .console
                        .write(format!("[Console] Plugin update failed: {error}")),
                },
                AppMsg::SendWsCommand(command) => {
                    if self.ws_stats.send_command(command).is_err() {
                        self.console
                            .write("[Core] Could not send StatsAPI websocket command.");
                    }
                }
            }
        }
        ctx.request_repaint();
    }

    fn handle_game_event(&mut self, event: StatsEvent) {
        if event.event_type == "MatchDestroyed" {
            if !self.config.settings.suppress_left_alerts {
                self.console
                    .write("[Core] Left match or game closed. Resetting plugin metrics.");
            }
            self.plugin_mgr
                .dispatch_simple("GameLeft", event.raw_data.clone());
        } else {
            self.plugin_mgr.dispatch_game_event(&event);
        }
    }

    fn update_hotkey(&mut self, key: &str) {
        if self
            .hotkey
            .as_mut()
            .map(|hotkey| hotkey.rebind(key))
            .unwrap_or(false)
        {
            self.config.settings.hotkey = key.to_string();
            self.save_config();
            self.console.write(format!(
                "[Console] Menu toggle keybind updated to: {}",
                key.to_uppercase()
            ));
        } else {
            self.console.write("[Console] Could not bind that key.");
        }
    }

    fn set_hidden(&mut self, ctx: &egui::Context, hidden: bool) {
        if !hidden {
            winutil::note_foreground();
        }
        self.hidden = hidden;
        winutil::set_main_window_invisible(hidden);
        ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(hidden));

        if hidden {
            self.topmost = false;
            winutil::set_main_window_topmost(false);
            winutil::focus_rocket_league();
        } else {
            if self.topmost || hebnix_sdk::process::is_rocket_league_focused() {
                self.topmost = true;
                winutil::set_main_window_topmost(true);
            }
            let game_fullscreen = self.last_rl_open
                && self.window_mode == Some(hebnix_sdk::save_file::WindowMode::Fullscreen);
            if !game_fullscreen {
                winutil::focus_main_window();
            }
        }

        self.plugin_mgr.dispatch_gui_visibility(!hidden);
        ctx.request_repaint();
    }
    fn start_hotkey_capture(&mut self, ctx: &egui::Context) {
        if self.capturing_hotkey {
            return;
        }
        self.capturing_hotkey = true;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let key = hebnix_sdk::input::detect_hotkey(Some(Duration::from_secs(10)));
            let _ = tx.send(AppMsg::HotkeyCaptured(key));
            ctx.request_repaint();
        });
    }

    fn execute_command(&mut self, raw: String) {
        let words: Vec<_> = raw.split_whitespace().collect();
        match words.first().map(|word| word.to_ascii_lowercase()).as_deref() {
            Some("help") => self.console.write("[Console] Commands: help, info, server, clear, restart, plugins list, plugin load|reload|unload <name>"),
            Some("info") => self.console.write(format!("[Console] Hebnix Lite {APP_VERSION} | plugins: {} | StatsAPI: {}", self.plugin_mgr.plugins.len(), self.currently_connected)),
            Some("clear") => self.console.clear(),
            Some("restart") => match winutil::restart_rocket_league(Path::new(&self.config.settings.rl_path)) {
                Ok(()) => self.console.write("[Console] Rocket League restarted."),
                Err(error) => self.console.write(format!("[Console] Restart failed: {error}")),
            },
            Some("server") => {
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let info = hebnix_sdk::log::parse_launch_log(None, true, "INT"); // verify so a menu or a closed game cant serve the last match
                    let fallback = if !hebnix_sdk::process::is_rocket_league_running() {
                        "[Console] Rocket League is not running."
                    } else if !info.stats_api_available {
                        "[Console] Can't read the match, the stats api is not answering."
                    } else {
                        "[Console] Not in a game."
                    };
                    let message = info.game.map(|game| format!("[Console] Server: {}:{}", game.server_ip.unwrap_or_else(|| "Unknown".to_string()), game.server_port.map(|port| port.to_string()).unwrap_or_else(|| "Unknown".to_string()))).unwrap_or_else(|| fallback.to_string());
                    let _ = tx.send(AppMsg::Log(message));
                });
            }
            Some("plugins") if words.get(1).is_some_and(|word| word.eq_ignore_ascii_case("list")) => {
                for plugin in &self.plugin_mgr.plugins {
                    self.console.write(format!("[Console] {} v{} [{}]", plugin.display_name(), plugin.manifest.version, if plugin.enabled { "enabled" } else { "disabled" }));
                }
            }
            Some("plugin") if words.len() >= 3 => {
                let action = words[1].to_ascii_lowercase();
                let target = words[2..].join(" ");
                let slug = self.plugin_mgr.plugins.iter().find(|plugin| plugin.slug.eq_ignore_ascii_case(&target) || plugin.display_name().eq_ignore_ascii_case(&target)).map(|plugin| plugin.slug.clone());
                match (action.as_str(), slug) {
                    ("load" | "reload", Some(slug)) => { self.plugin_mgr.set_enabled(&slug, true, &mut self.config); self.save_config(); }
                    ("unload", Some(slug)) => { self.plugin_mgr.set_enabled(&slug, false, &mut self.config); self.save_config(); }
                    _ => self.console.write("[Console] Plugin command failed. Use plugin load|reload|unload <name>."),
                }
            }
            _ => self.console.write("[Console] Unknown command. Type help."),
        }
    }

    fn render_console(&mut self, ui: &mut egui::Ui) {
        let names = self
            .plugin_mgr
            .plugins
            .iter()
            .map(|plugin| plugin.display_name().to_string())
            .collect::<Vec<_>>();
        if let Some(command) = self.console.render(ui, &names) {
            self.execute_command(command);
        }
    }

    fn render_plugins(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Open Plugins Folder").clicked() {
                let _ = open::that(&self.plugin_dir);
            }
            if ui.button("Install Plugin").clicked() {
                self.install_modal = InstallModal {
                    open: true,
                    ..Default::default()
                };
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Reload").clicked() {
                    self.plugin_mgr.refresh(&mut self.config, true);
                    self.save_config();
                }
            });
        });
        ui.separator();

        let mut updates = Vec::new();
        let mut settings = None;
        egui::ScrollArea::vertical()
            .id_salt("lite_plugins_list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for plugin in &self.plugin_mgr.plugins {
                    let row_width = ui.available_width();
                    let mut enabled = plugin.enabled;
                    egui::Frame::group(ui.style())
                        .inner_margin(egui::Margin::same(6))
                        .show(ui, |ui| {
                            ui.set_width((row_width - 12.0).max(0.0));
                            ui.horizontal(|ui| {
                                ui.set_min_height(24.0);
                                let text = format!(
                                    "{} v{} by {} ({})",
                                    plugin.display_name(),
                                    plugin.manifest.version,
                                    plugin.manifest.author,
                                    plugin.filename
                                );
                                if plugin.load_error.is_some() {
                                    ui.add_enabled(false, egui::Checkbox::new(&mut enabled, text));
                                } else if ui.checkbox(&mut enabled, text).changed() {
                                    updates.push((plugin.slug.clone(), enabled));
                                }

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if plugin.load_error.is_none()
                                            && ui
                                                .add_enabled(
                                                    plugin.enabled && plugin.has_settings(),
                                                    egui::Button::new("⚙"),
                                                )
                                                .clicked()
                                        {
                                            settings = Some(plugin.slug.clone());
                                        }
                                    },
                                );
                            });
                            if let Some(error) = &plugin.load_error {
                                ui.colored_label(Color32::LIGHT_RED, error);
                            }
                        });
                    ui.add_space(2.0);
                }

                if self.plugin_mgr.plugins.is_empty() {
                    ui.add_space(30.0);
                    ui.vertical_centered(|ui| {
                        ui.label("No plugins installed. Drop a plugin folder into plugins/.");
                    });
                }
            });

        for (slug, enabled) in updates {
            self.plugin_mgr
                .set_enabled(&slug, enabled, &mut self.config);
            self.save_config();
        }
        if let Some(slug) = settings {
            self.selected_settings_plugin = Some(slug);
            self.tab = Tab::Settings;
            self.settings_tab = SettingsTab::Plugin;
        }
    }

    fn render_update_modal(&mut self, ctx: &egui::Context) {
        let Some(info) = self.update_info.clone() else {
            return;
        };
        egui::Window::new("Update Required")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.heading(format!("Hebnix Lite v{} is required", info.version));
                ui.label("Hebnix Lite is locked until the required update is installed.");
                ui.add_space(8.0);
                if let Some(error) = &self.update_error {
                    ui.colored_label(Color32::from_rgb(0xe7, 0x4c, 0x3c), error);
                    ui.add_space(8.0);
                }
                if self.update_downloading {
                    ui.add_enabled(false, egui::Button::new("Downloading & Installing..."));
                    ui.spinner();
                } else if ui
                    .add(
                        egui::Button::new("Update Hebnix")
                            .fill(Color32::from_rgb(0x2e, 0xcc, 0x71)),
                    )
                    .clicked()
                {
                    self.update_downloading = true;
                    self.update_error = None;
                    let setup_url = info.setup_url;
                    let base_dir = self.base_dir.clone();
                    let tx = self.tx.clone();
                    let ctx = ctx.clone();
                    std::thread::spawn(move || {
                        if let Err(error) =
                            crate::update::download_and_install_update(&setup_url, &base_dir)
                        {
                            let _ = tx.send(AppMsg::AppUpdateFailed { error });
                            ctx.request_repaint();
                        }
                    });
                }
            });
    }

    fn render_changelog_popup(&mut self, ctx: &egui::Context) {
        let Some(entry) = self.changelog_popup.clone() else {
            return;
        };
        let mut open = true;
        egui::Window::new("Change Log")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| crate::update::render_changelog(ui, &entry));
        if !open {
            self.changelog_popup = None;
        }
    }

    fn render_launch_path_notice(&mut self, ctx: &egui::Context) {
        if !self.launch_path_notice {
            return;
        }
        let mut close = false;
        egui::Window::new("Rocket League")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("You must start Rocket League at least once with Hebnix running to use this function.");
                if ui.button("OK").clicked() {
                    close = true;
                }
            });
        if close {
            self.launch_path_notice = false;
        }
    }
    fn render_install_modal(&mut self, ctx: &egui::Context) {
        if !self.install_modal.open {
            return;
        }
        let mut open = true;
        let window = if self.install_modal.catalog_open {
            egui::Window::new("Install Plugin")
                .resizable(false)
                .fixed_size([900.0, 550.0])
        } else {
            egui::Window::new("Install Plugin")
                .resizable(false)
                .fixed_size([350.0, 160.0])
        };
        window
            .open(&mut open)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                if !self.install_modal.catalog_open {
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add_sized(
                                [160.0, 120.0],
                                egui::Button::new("☁\n\nInstall from Hebnix"),
                            )
                            .clicked()
                        {
                            self.install_modal.catalog_open = true;
                            self.fetch_plugin_catalog();
                        }
                        if ui
                            .add_sized([160.0, 120.0], egui::Button::new("📁\n\nInstall from .ZIP"))
                            .clicked()
                        {
                            if let Some(file) = rfd::FileDialog::new()
                                .add_filter("Plugin archive", &["zip"])
                                .pick_file()
                            {
                                match install_zip(&file, &self.plugin_dir) {
                                    Ok(()) => {
                                        self.console.write(format!(
                                            "[Console] Installed {}.",
                                            file.file_name()
                                                .and_then(|name| name.to_str())
                                                .unwrap_or("plugin archive")
                                        ));
                                        self.plugin_mgr.refresh(&mut self.config, true);
                                        self.save_config();
                                        self.install_modal.open = false;
                                    }
                                    Err(error) => self.console.write(format!(
                                        "[Console] Plugin installation failed: {error}"
                                    )),
                                }
                            }
                        }
                    });
                    return;
                }

                ui.horizontal(|ui| {
                    if ui.button("< Back").clicked() {
                        self.install_modal.catalog_open = false;
                    }
                    if ui.button("Refresh").clicked() {
                        self.fetch_plugin_catalog();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut self.install_modal.search)
                                    .hint_text("Search plugins")
                                    .desired_width(220.0),
                            )
                            .changed()
                        {
                            self.install_modal.page = 0;
                        }
                    });
                });
                ui.separator();
                if self.install_modal.fetching {
                    ui.spinner();
                    ui.label("Fetching plugins...");
                    return;
                }
                if let Some(error) = &self.install_modal.error {
                    ui.colored_label(Color32::LIGHT_RED, error);
                    return;
                }

                let query = self.install_modal.search.to_lowercase();
                let mut entries = self
                    .install_modal
                    .catalog
                    .iter()
                    .filter(|entry| {
                        query.is_empty()
                            || ["name", "author", "short_description"].iter().any(|key| {
                                entry
                                    .get(*key)
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_lowercase()
                                    .contains(&query)
                            })
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let per_page = 10;
                let total_pages = entries.len().div_ceil(per_page).max(1);
                self.install_modal.page = self.install_modal.page.min(total_pages - 1);
                let start = self.install_modal.page * per_page;
                entries = entries.into_iter().skip(start).take(per_page).collect();

                let mut install = None;
                let mut enable = None;
                let mut disable = None;
                for entry in &entries {
                    let banner = entry
                        .get("banner_path")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    self.ensure_plugin_image(&banner, ctx);
                }

                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for entry in entries {
                            let id = entry
                                .get("plugin_id")
                                .or_else(|| entry.get("id"))
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            let name = entry
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("Unknown");
                            let author = entry
                                .get("author")
                                .and_then(Value::as_str)
                                .unwrap_or("Unknown");
                            let description = entry
                                .get("short_description")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            let version = entry
                                .get("version_number")
                                .and_then(Value::as_str)
                                .unwrap_or("?");
                            let banner = entry
                                .get("banner_path")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            let mut short_description = description
                                .trim_start()
                                .replace('\n', " ")
                                .replace('\r', "");
                            if short_description.chars().count() > 120 {
                                short_description = short_description.chars().take(117).collect();
                                short_description.push_str("...");
                            }

                            egui::Frame::group(ui.style())
                                .inner_margin(egui::Margin::same(8))
                                .show(ui, |ui| {
                                    ui.set_height(76.0);
                                    ui.horizontal(|ui| {
                                        ui.set_height(76.0);
                                        match self.install_modal.images.get(banner) {
                                            Some(ImageState::Ready(bytes)) => {
                                                ui.add(
                                                    egui::Image::from_bytes(
                                                        format!("bytes://plugin/{banner}"),
                                                        bytes.clone(),
                                                    )
                                                    .fit_to_exact_size(egui::vec2(160.0, 72.0)),
                                                );
                                            }
                                            Some(ImageState::Loading) => {
                                                ui.allocate_ui(egui::vec2(160.0, 72.0), |ui| {
                                                    ui.centered_and_justified(|ui| ui.spinner());
                                                });
                                            }
                                            Some(ImageState::Failed) | None => {
                                                ui.allocate_space(egui::vec2(160.0, 72.0));
                                            }
                                        }
                                        ui.add_space(4.0);
                                        let details_width =
                                            (ui.available_width() - 88.0).max(150.0);
                                        ui.allocate_ui_with_layout(
                                            egui::vec2(details_width, 72.0),
                                            egui::Layout::top_down(egui::Align::Min),
                                            |ui| {
                                                ui.strong(format!("{name} v{version}"));
                                                ui.weak(format!("by {author}"));
                                                ui.add_sized(
                                                    [details_width, 34.0],
                                                    egui::Label::new(short_description)
                                                        .wrap()
                                                        .halign(egui::Align::Min),
                                                )
                                                .on_hover_text(description);
                                            },
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                let existing =
                                                    self.plugin_mgr.plugins.iter().find(|p| {
                                                        p.manifest.plugin_id.as_deref() == Some(id)
                                                    });
                                                if let Some(plugin) = existing {
                                                    if plugin.enabled {
                                                        if ui.button("Disable").clicked() {
                                                            disable = Some(plugin.slug.clone());
                                                        }
                                                    } else if ui.button("Enable").clicked() {
                                                        enable = Some(plugin.slug.clone());
                                                    }
                                                } else if self
                                                    .install_modal
                                                    .downloading_id
                                                    .as_deref()
                                                    == Some(id)
                                                {
                                                    ui.add_enabled(
                                                        false,
                                                        egui::Button::new("Installing..."),
                                                    );
                                                } else if ui
                                                    .add_enabled(
                                                        !id.is_empty(),
                                                        egui::Button::new("Install"),
                                                    )
                                                    .clicked()
                                                {
                                                    install = Some(id.to_string());
                                                }
                                            },
                                        );
                                    });
                                });
                            ui.add_space(4.0);
                        }
                    });
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(self.install_modal.page > 0, egui::Button::new("< Prev"))
                        .clicked()
                    {
                        self.install_modal.page -= 1;
                    }
                    ui.label(format!(
                        "Page {} of {}",
                        self.install_modal.page + 1,
                        total_pages
                    ));
                    if ui
                        .add_enabled(
                            self.install_modal.page + 1 < total_pages,
                            egui::Button::new("Next >"),
                        )
                        .clicked()
                    {
                        self.install_modal.page += 1;
                    }
                });
                if let Some(slug) = enable {
                    self.plugin_mgr.set_enabled(&slug, true, &mut self.config);
                    self.save_config();
                }
                if let Some(slug) = disable {
                    self.plugin_mgr.set_enabled(&slug, false, &mut self.config);
                    self.save_config();
                }
                if let Some(id) = install {
                    self.download_plugin(&id);
                }
            });
        if !open || !self.install_modal.open {
            self.install_modal = InstallModal::default();
        }
    }

    fn ensure_plugin_image(&mut self, banner_path: &str, ctx: &egui::Context) {
        if banner_path.is_empty() || self.install_modal.images.contains_key(banner_path) {
            return;
        }
        self.install_modal
            .images
            .insert(banner_path.to_string(), ImageState::Loading);

        let cache_dir = self
            .base_dir
            .join("plugins")
            .join("cache")
            .join("plugin_store");
        let key = banner_path.to_string();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let normalized = key.replace('\\', "/");
            let url = format!("https://hebnix.com{normalized}");
            let local_path = cache_dir.join(normalized.trim_start_matches('/'));
            let bytes = if local_path.exists() {
                std::fs::read(&local_path).ok()
            } else {
                ureq::AgentBuilder::new()
                    .try_proxy_from_env(false)
                    .build()
                    .get(&url)
                    .timeout(Duration::from_secs(10))
                    .call()
                    .ok()
                    .and_then(|response| {
                        let mut bytes = Vec::new();
                        response.into_reader().read_to_end(&mut bytes).ok()?;
                        if let Some(parent) = local_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = std::fs::write(&local_path, &bytes);
                        Some(bytes)
                    })
            };
            let _ = tx.send(AppMsg::PluginImage {
                key,
                bytes: bytes.unwrap_or_default(),
            });
            ctx.request_repaint();
        });
    }
    fn fetch_plugin_catalog(&mut self) {
        self.install_modal.fetching = true;
        self.install_modal.error = None;
        self.install_modal.catalog.clear();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = ureq::AgentBuilder::new()
                .try_proxy_from_env(false)
                .build()
                .get("https://api.hebnix.com/plugins")
                .timeout(Duration::from_secs(10))
                .call()
                .map_err(|error| error.to_string())
                .and_then(|response| response.into_json().map_err(|error| error.to_string()));
            let _ = tx.send(AppMsg::PluginFetch { result });
        });
    }

    fn download_plugin(&mut self, id: &str) {
        self.install_modal.downloading_id = Some(id.to_string());
        let id = id.to_string();
        let plugin_dir = self.plugin_dir.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = download_and_extract_plugin(&id, &plugin_dir)
                .map(|_| format!("Plugin {id} installed."));
            let _ = tx.send(AppMsg::PluginDownloadDone { result });
        });
    }

    fn check_plugin_updates(&self) {
        let payload = self
            .plugin_mgr
            .plugins
            .iter()
            .filter_map(|plugin| {
                plugin.manifest.plugin_id.as_deref().filter(|id| !id.is_empty()).map(|id| {
                serde_json::json!({ "plugin_id": id, "version": plugin.manifest.version })
            })
            })
            .collect::<Vec<_>>();
        if payload.is_empty() {
            return;
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = ureq::AgentBuilder::new()
                .try_proxy_from_env(false)
                .build()
                .post("https://api.hebnix.com/check")
                .timeout(Duration::from_secs(15))
                .send_json(Value::Array(payload))
                .map_err(|error| error.to_string())
                .and_then(|response| {
                    response
                        .into_json::<Value>()
                        .map_err(|error| error.to_string())
                })
                .map(|value| value.as_array().cloned().unwrap_or_default());
            let _ = tx.send(AppMsg::PluginUpdatesFound { result });
        });
    }

    fn start_plugin_updates(&mut self, updates: Vec<Value>) {
        for update in updates {
            let plugin_id = update
                .get("plugin_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if plugin_id.is_empty() {
                continue;
            }
            let Some(plugin) = self
                .plugin_mgr
                .plugins
                .iter()
                .find(|plugin| plugin.manifest.plugin_id.as_deref() == Some(plugin_id))
            else {
                continue;
            };
            let slug = plugin.slug.clone();
            let name = plugin.display_name().to_string();
            let was_enabled = plugin.enabled;
            let plugin_dir = self.plugin_dir.clone();
            let tx = self.tx.clone();
            let id = plugin_id.to_string();
            self.console
                .write(format!("[Core] Updating plugin '{name}'..."));
            std::thread::spawn(move || {
                let result = download_and_extract_plugin(&id, &plugin_dir)
                    .map(|_| format!("Plugin '{slug}' updated."));
                let _ = tx.send(AppMsg::PluginAutoUpdateDone {
                    slug,
                    was_enabled,
                    result,
                });
            });
        }
    }

    fn render_settings(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.settings_tab,
                SettingsTab::Hebnix,
                "Hebnix Settings",
            );
            ui.selectable_value(
                &mut self.settings_tab,
                SettingsTab::Plugin,
                "Plugin Settings",
            );
        });
        ui.separator();
        match self.settings_tab {
            SettingsTab::Hebnix => self.render_hebnix_settings(ui),
            SettingsTab::Plugin => self.render_plugin_settings(ui),
        }
    }

    fn render_hebnix_settings(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("lite_hebnix_settings_list")
            .resizable(false)
            .exact_size(150.0)
            .show(ui, |ui| {
                ui.selectable_value(
                    &mut self.hebnix_settings_tab,
                    HebnixSettingsTab::Interface,
                    "Interface",
                );
                ui.selectable_value(
                    &mut self.hebnix_settings_tab,
                    HebnixSettingsTab::Directories,
                    "Directories & Files",
                );
                ui.selectable_value(
                    &mut self.hebnix_settings_tab,
                    HebnixSettingsTab::System,
                    "System",
                );
            });
        ui.vertical(|ui| {
            ui.heading(match self.hebnix_settings_tab {
                HebnixSettingsTab::Interface => "Interface Configuration",
                HebnixSettingsTab::Directories => "Directories & Files Configuration",
                HebnixSettingsTab::System => "System Configuration",
            });
            ui.add_space(8.0);
            match self.hebnix_settings_tab {
                HebnixSettingsTab::Interface => self.render_interface_settings(ui),
                HebnixSettingsTab::Directories => self.render_stats_settings(ui),
                HebnixSettingsTab::System => self.render_system_settings(ui),
            }
        });
    }

    fn render_interface_settings(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.label("Open/Close Keybind:");
            ui.label(self.config.settings.hotkey.to_uppercase());
            if ui
                .add_enabled(
                    !self.capturing_hotkey,
                    egui::Button::new(if self.capturing_hotkey {
                        "Listening..."
                    } else {
                        "Set Keybind"
                    }),
                )
                .clicked()
            {
                self.start_hotkey_capture(&ctx);
            }
        });
        ui.horizontal(|ui| {
            ui.label("Theme:");
            let mut choice = self.config.settings.theme.clone();
            egui::ComboBox::from_id_salt("lite_theme")
                .selected_text(&choice)
                .show_ui(ui, |ui| {
                    for item in &self.theme_options {
                        ui.selectable_value(&mut choice, item.clone(), item);
                    }
                });
            if choice != self.config.settings.theme {
                if theme::apply_theme(&ctx, &self.themes_dir, &self.themes_dir, &choice).is_ok() {
                    self.config.settings.theme = choice;
                    self.save_config();
                }
                theme::apply_window_opacity(&ctx, self.config.settings.window_opacity);
            }
            if ui.button("Refresh").clicked() {
                self.theme_options = theme::list_themes(&self.themes_dir);
            }
            if ui.button("Open Folder").clicked() {
                let _ = open::that(&self.themes_dir);
            }
        });
        ui.horizontal(|ui| {
            ui.label("Window Opacity:");
            if ui
                .add(egui::Slider::new(
                    &mut self.config.settings.window_opacity,
                    0.5..=1.0,
                ))
                .changed()
            {
                let _ = theme::apply_theme(
                    &ctx,
                    &self.themes_dir,
                    &self.themes_dir,
                    &self.config.settings.theme,
                );
                theme::apply_window_opacity(&ctx, self.config.settings.window_opacity);
                self.save_config();
            }
        });
    }

    fn render_stats_settings(&mut self, ui: &mut egui::Ui) {
        ui.weak("(auto-detected from the running game)");
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Rocket League Folder:");
            ui.add_enabled(
                false,
                egui::TextEdit::singleline(&mut self.config.settings.rl_path).desired_width(420.0),
            );
            if ui.button("Browse").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.config.settings.rl_path = path.to_string_lossy().to_string();
                    self.refresh_statsapi();
                    self.save_config();
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("DefaultStatsAPI.ini:");
            ui.add_enabled(
                false,
                egui::TextEdit::singleline(&mut self.config.settings.statsapi_path)
                    .desired_width(420.0),
            );
            if ui.button("Browse").clicked() {
                let start_dir = Path::new(&self.config.settings.statsapi_path)
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                if let Some(path) = rfd::FileDialog::new()
                    .set_directory(start_dir)
                    .add_filter("INI files", &["ini"])
                    .pick_file()
                {
                    self.config.settings.statsapi_path = path.to_string_lossy().to_string();
                    self.refresh_statsapi();
                    self.save_config();
                }
            }
        });
        let packet_current = self.packet_rate.clone().unwrap_or_default();
        let mut packet_edit = std::mem::take(&mut self.packet_rate_edit);
        if let Some(value) = ini_row(
            ui,
            "PacketSendRate",
            &mut packet_edit,
            &packet_current,
            "20",
        ) {
            self.update_ini_setting("PacketSendRate", &value);
        }
        self.packet_rate_edit = packet_edit;

        let port_current = self
            .port_value
            .clone()
            .unwrap_or_else(|| "49123".to_string());
        let mut port_edit = std::mem::take(&mut self.port_edit);
        if let Some(value) = ini_row(ui, "Port", &mut port_edit, &port_current, "49123") {
            self.update_ini_setting("Port", &value);
        }
        self.port_edit = port_edit;

        let web_port_current = self
            .web_port_value
            .clone()
            .unwrap_or_else(|| "49124".to_string());
        let mut web_port_edit = std::mem::take(&mut self.web_port_edit);
        if let Some(value) = ini_row(
            ui,
            "WebPort",
            &mut web_port_edit,
            &web_port_current,
            "49124",
        ) {
            self.update_ini_setting("WebPort", &value);
        }
        self.web_port_edit = web_port_edit;
        if ui.button("Refresh StatsAPI values").clicked() {
            self.refresh_statsapi();
        }
        ui.weak("Changes to the ini apply after restarting Rocket League.");
    }

    fn render_system_settings(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_sized([180.0, 20.0], egui::Label::new("Start with Windows:"));
            if ui.checkbox(&mut self.startup_enabled, "").changed() {
                if let Err(error) = winutil::set_startup_enabled(self.startup_enabled) {
                    self.console
                        .write(format!("[Console] Failed to update startup entry: {error}"));
                    self.startup_enabled = winutil::is_startup_enabled();
                }
            }
        });
        ui.horizontal(|ui| {
            ui.add_sized([180.0, 20.0], egui::Label::new("Start in Tray:"));
            if ui
                .checkbox(&mut self.config.settings.start_in_tray, "")
                .changed()
            {
                self.save_config();
            }
        });
        ui.horizontal(|ui| {
            ui.add_sized([180.0, 20.0], egui::Label::new("Suppress Left Alerts:"));
            if ui
                .checkbox(&mut self.config.settings.suppress_left_alerts, "")
                .changed()
            {
                self.save_config();
            }
        });
        ui.horizontal(|ui| {
            ui.add_sized([180.0, 20.0], egui::Label::new("Fullscreen Warning:"));
            let mut show = !self.config.settings.suppress_fullscreen_warning;
            if ui.checkbox(&mut show, "").changed() {
                self.config.settings.suppress_fullscreen_warning = !show;
                self.fullscreen_notice_dismissed = false;
                self.fullscreen_notice =
                    show && self.window_mode == Some(hebnix_sdk::save_file::WindowMode::Fullscreen);
                self.save_config();
            }
        });
        ui.weak("Warns when the game is fullscreen and overlays cannot draw.");
        ui.horizontal(|ui| {
            ui.add_sized([180.0, 20.0], egui::Label::new("StatsAPI Rate Warning:"));
            let mut show = !self.config.settings.suppress_statsapi_rate_warning;
            if ui.checkbox(&mut show, "").changed() {
                self.config.settings.suppress_statsapi_rate_warning = !show;
                self.save_config();
            }
        });
        ui.weak("Warns when PacketSendRate is not 20.");
    }

    fn render_plugin_settings(&mut self, ui: &mut egui::Ui) {
        let with_settings: Vec<(String, String)> = self
            .plugin_mgr
            .plugins
            .iter()
            .filter(|plugin| plugin.enabled && plugin.has_settings())
            .map(|plugin| (plugin.slug.clone(), plugin.display_name().to_string()))
            .collect();

        if with_settings.is_empty() {
            ui.add_space(50.0);
            ui.vertical_centered(|ui| {
                ui.label("No Plugins with Settings Enabled");
                ui.add_space(8.0);
                if ui.button("Go to Plugins").clicked() {
                    self.tab = Tab::Plugins;
                }
            });
            return;
        }

        let selected_valid = self
            .selected_settings_plugin
            .as_ref()
            .map(|slug| with_settings.iter().any(|(entry, _)| entry == slug))
            .unwrap_or(false);
        if !selected_valid {
            self.selected_settings_plugin = Some(with_settings[0].0.clone());
        }
        let selected = self.selected_settings_plugin.clone().unwrap_or_default();
        let display_name = with_settings
            .iter()
            .find(|(slug, _)| *slug == selected)
            .map(|(_, name)| name.clone())
            .unwrap_or_default();

        egui::Panel::left("lite_plugin_settings_list")
            .resizable(false)
            .exact_size(150.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("lite_plugin_settings_names")
                    .show(ui, |ui| {
                        for (slug, name) in &with_settings {
                            if ui.selectable_label(*slug == selected, name).clicked() {
                                self.selected_settings_plugin = Some(slug.clone());
                            }
                        }
                    });
            });

        egui::ScrollArea::vertical()
            .id_salt("lite_plugin_settings_view")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.heading(format!("{display_name} Configuration"));
                ui.add_space(8.0);
                if let Err(error) = self.plugin_mgr.render_settings(&selected, ui) {
                    self.console
                        .write(format!("[Console] Plugin settings error: {error}"));
                }
            });
    }

    fn render_about(&self, ui: &mut egui::Ui) {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.heading("Hebnix Lite");
            ui.add_space(10.0);
            ui.label(format!(
                "Version {APP_VERSION}\n\nA safe, EAC-compliant Mod Loader for Rocket League.\n\nhebnix.com\n\nBuilt by Hebbins & nixvio64.\n\nPress {} to show/hide window.",
                self.config.settings.hotkey.to_uppercase()
            ));
        });
    }

    fn render_notices(&mut self, ctx: &egui::Context) {
        if self.fullscreen_notice {
            let mut dismiss = false;
            let mut suppress = false;
            egui::Window::new("Fullscreen warning")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("Rocket League is fullscreen. Plugin overlays cannot draw in fullscreen mode.");
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() { dismiss = true; }
                        if ui.button("Don't show again").clicked() { suppress = true; }
                    });
                });
            if dismiss {
                self.fullscreen_notice = false;
                self.fullscreen_notice_dismissed = true;
            }
            if suppress {
                self.config.settings.suppress_fullscreen_warning = true;
                self.fullscreen_notice = false;
                self.save_config();
            }
        }
        if !self.config.settings.suppress_statsapi_rate_warning {
            if let Some(message) = self.statsapi_notice.clone() {
                let mut dismiss = false;
                let mut suppress = false;
                egui::Window::new("StatsAPI configuration")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(message);
                        ui.horizontal(|ui| {
                            if ui.button("Set PacketSendRate to 20").clicked() {
                                self.update_ini_setting("PacketSendRate", "20");
                                dismiss = true;
                            }
                            if ui.button("Later").clicked() {
                                dismiss = true;
                            }
                            if ui.button("Don't show again").clicked() {
                                suppress = true;
                            }
                        });
                    });
                if suppress {
                    self.config.settings.suppress_statsapi_rate_warning = true;
                    self.save_config();
                }
                if dismiss {
                    self.statsapi_notice = None;
                }
            }
        }
    }

    fn render_game_overlay(&mut self) {
        let slugs = self.plugin_mgr.overlay_plugins();
        if slugs.is_empty() || !hebnix_sdk::process::is_rocket_league_focused() {
            self.overlay.hide();
            self.overlay_rect = None;
            return;
        }
        let due = self
            .overlay_rect_checked
            .map(|time| time.elapsed() > Duration::from_millis(250))
            .unwrap_or(true);
        if due {
            self.overlay_rect_checked = Some(std::time::Instant::now());
            self.overlay_rect = hebnix_sdk::process::get_rocket_league_window_rect();
        }
        let Some(rect) = self.overlay_rect else {
            self.overlay.hide();
            return;
        };
        let mut errors = Vec::new();
        self.overlay.frame(rect, |width, height| {
            for slug in &slugs {
                if let Err(error) = self.plugin_mgr.render_overlay_gdi(slug, width, height) {
                    errors.push(format!("[Core] Overlay error in '{slug}': {error}"));
                }
            }
        });
        for error in errors {
            self.console.write(error);
        }
    }

    fn render_plugin_windows(&mut self, ctx: &egui::Context) {
        let due = self
            .plugin_monitor_checked
            .map(|time| time.elapsed() > Duration::from_millis(500))
            .unwrap_or(true);
        if due {
            self.plugin_monitor_checked = Some(std::time::Instant::now());
            let (width, height) = hebnix_sdk::process::rocket_league_monitor_size();
            self.plugin_monitor_size = (width as f32, height as f32);
        }
        let ppp = ctx.pixels_per_point();
        let windows = self
            .plugin_mgr
            .plugins
            .iter()
            .filter(|plugin| plugin.enabled)
            .filter_map(|plugin| {
                plugin
                    .runtime
                    .as_ref()
                    .map(|runtime| (plugin.slug.clone(), runtime.host.window.borrow().clone()))
            })
            .collect::<Vec<_>>();
        for (slug, state) in windows {
            let viewport_id = egui::ViewportId::from_hash_of(("lite_plugin_window", &slug));
            let mut builder = egui::ViewportBuilder::default()
                .with_title(state.title.clone())
                .with_inner_size([
                    state.width.resolve(self.plugin_monitor_size.0, ppp),
                    state.height.resolve(self.plugin_monitor_size.1, ppp),
                ])
                .with_decorations(false)
                .with_always_on_top()
                .with_resizable(false)
                .with_transparent(true)
                .with_visible(state.open)
                .with_taskbar(false);
            if let Some((x, y)) = state.pos {
                builder = builder.with_position([x, y]);
            }
            ctx.show_viewport_immediate(viewport_id, builder, |ui, _| {
                if !state.open {
                    return;
                }
                let ctx = ui.ctx().clone();
                egui::CentralPanel::default().show(ui, |ui| {
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 22.0),
                        egui::Sense::drag(),
                    );
                    if response.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    let title_rect = if state.close_button {
                        egui::Rect::from_min_max(
                            rect.min,
                            egui::pos2(rect.right() - 22.0, rect.bottom()),
                        )
                    } else {
                        rect
                    };
                    ui.painter().text(
                        title_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        &state.title,
                        egui::FontId::proportional(13.0),
                        ui.visuals().strong_text_color(),
                    );
                    if state.close_button {
                        let close_rect = egui::Rect::from_min_max(
                            egui::pos2(rect.right() - 22.0, rect.top()),
                            rect.max,
                        );
                        if ui
                            .put(close_rect, egui::Button::new("×").frame(false))
                            .on_hover_text("Close")
                            .clicked()
                        {
                            self.plugin_mgr.close_window(&slug);
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                    ui.separator();
                    if let Err(error) = self.plugin_mgr.render_window(&slug, ui) {
                        ui.colored_label(Color32::LIGHT_RED, error);
                    }
                });
                if let Some(rect) = ctx.input(|input| input.viewport().outer_rect) {
                    self.plugin_mgr
                        .set_window_pos(&slug, rect.min.x, rect.min.y);
                }
            });
        }
    }
}

fn ini_row(
    ui: &mut egui::Ui,
    key: &str,
    value: &mut String,
    current: &str,
    default: &str,
) -> Option<String> {
    let mut apply = None;
    ui.horizontal(|ui| {
        ui.label(format!("{key}:"));
        let response = ui.add(egui::TextEdit::singleline(value).desired_width(100.0));
        if response.changed() {
            value.retain(|character| character.is_ascii_digit());
        }
        if response.lost_focus() && !value.is_empty() && value != current {
            apply = Some(value.clone());
        }
        if ui.button(format!("Set {default}")).clicked() {
            apply = Some(default.to_string());
        }
    });
    apply
}

impl Drop for LiteApp {
    fn drop(&mut self) {
        self.plugin_mgr.unload_all();
        self.stats.stop();
        self.ws_stats.stop();
        self.monitor.stop();
    }
}

impl eframe::App for LiteApp {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_messages(&ctx);
        if winutil::take_show_request() && self.hidden {
            self.set_hidden(&ctx, false);
        }
        dpi_fix::install_on_all_windows();
        if let Some(rect) = ctx.input(|input| input.viewport().inner_rect) {
            self.last_size = (rect.width().max(0.0) as u32, rect.height().max(0.0) as u32);
        }
        if ctx.input(|input| input.viewport().close_requested()) {
            if self.update_info.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        if self.update_info.is_some() {
            if self.hidden {
                self.set_hidden(&ctx, false);
            }
            egui::CentralPanel::default().show(ui, |ui| {
                ui.disable();
                ui.centered_and_justified(|ui| {
                    ui.heading("A required Hebnix Lite update is available");
                });
            });
            self.render_update_modal(&ctx);
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Console, "Console");
                ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
                ui.selectable_value(&mut self.tab, Tab::Plugins, "Plugins");
                ui.selectable_value(&mut self.tab, Tab::About, "About");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(&self.status_text)
                            .strong()
                            .size(12.0)
                            .color(self.status_color),
                    );
                    let running = self.last_rl_open;
                    let label = if running {
                        "Restart Rocket League"
                    } else {
                        "Start Rocket League"
                    };
                    if ui.button(label).clicked() {
                        let path = self.config.settings.rl_path.clone();
                        if !self.config.settings.rl_path_confirmed
                            || path.trim().is_empty()
                            || !Path::new(&path).is_dir()
                        {
                            self.launch_path_notice = true;
                        } else {
                            let tx = self.tx.clone();
                            std::thread::spawn(move || {
                                let result = if running {
                                    winutil::restart_rocket_league(Path::new(&path))
                                } else {
                                    winutil::start_rocket_league(Path::new(&path))
                                };
                                let action = if running { "restart" } else { "start" };
                                let _ = tx.send(AppMsg::Log(match result {
                                    Ok(()) => {
                                        format!("[Console] Rocket League {} requested.", action)
                                    }
                                    Err(error) => format!(
                                        "[Console] Rocket League {} failed: {}",
                                        action, error
                                    ),
                                }));
                            });
                        }
                    }
                });
            });
            ui.separator();
            match self.tab {
                Tab::Console => self.render_console(ui),
                Tab::Plugins => self.render_plugins(ui),
                Tab::Settings => self.render_settings(ui),
                Tab::About => self.render_about(ui),
            }
        });
        self.plugin_mgr.dispatch_tick();
        self.plugin_mgr.flush_window_positions();
        self.render_plugin_windows(&ctx);
        self.render_game_overlay();
        self.render_install_modal(&ctx);
        self.render_notices(&ctx);
        self.render_launch_path_notice(&ctx);
        self.render_changelog_popup(&ctx);
        ctx.request_repaint_after(
            if self.plugin_mgr.has_tick_plugins() || !self.plugin_mgr.overlay_plugins().is_empty() {
                Duration::from_millis(50)
            } else {
                Duration::from_millis(500)
            },
        );
    }
}

fn install_zip(zip_path: &std::path::Path, plugin_dir: &std::path::Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|error| error.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    archive
        .extract(plugin_dir)
        .map_err(|error| error.to_string())
}

fn download_and_extract_plugin(id: &str, plugin_dir: &std::path::Path) -> Result<(), String> {
    let response = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .build()
        .get(&format!("https://api.hebnix.com/download/plugin/{id}"))
        .timeout(Duration::from_secs(30))
        .call()
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut response.into_reader(), &mut bytes)
        .map_err(|error| error.to_string())?;
    let archive = plugin_dir.join(format!("hebnix-plugin-{id}.zip"));
    std::fs::write(&archive, bytes).map_err(|error| error.to_string())?;
    let result = install_zip(&archive, plugin_dir);
    let _ = std::fs::remove_file(&archive);
    result
}
