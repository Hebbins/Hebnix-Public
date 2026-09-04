// crates/hebnix-app/src/app.rs
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{self, Color32};
use serde_json::Value;

use hebnix_sdk::save_file::WindowMode;
use hebnix_sdk::stats::StatsClient;

use crate::config::Config;
use crate::hotkey::ToggleHotkey;
use crate::messages::AppMsg;
use crate::monitor::{Monitor, MonitorShared};
use crate::plugins::PluginManager;
use crate::spoofer;
use crate::spoofer::SpooferManager;
use crate::statsapi_ini;
use crate::theme;
use crate::tray::Tray;
use crate::ui::console::ConsoleState;
use crate::ui::workshop::{ImageState, WorkshopState};
use crate::winutil;

pub const APP_VERSION: &str = "2.1.6";

pub const DEFAULT_WIDTH: f32 = 1000.0;
pub const DEFAULT_HEIGHT: f32 = 600.0;
pub const MIN_WIDTH: f32 = DEFAULT_WIDTH * 0.7;
pub const MIN_HEIGHT: f32 = DEFAULT_HEIGHT * 0.7;

fn get_retry(url: &str, timeout: Duration) -> Result<ureq::Response, String> {
    let agent = ureq::AgentBuilder::new().try_proxy_from_env(false).build();
    let mut last = String::new();
    for attempt in 0..3 {
        match agent.get(url).timeout(timeout).call() {
            Ok(r) => return Ok(r),
            Err(e @ ureq::Error::Status(..)) => return Err(e.to_string()),
            Err(e) => {
                last = e.to_string();
                if attempt < 2 {
                    std::thread::sleep(Duration::from_millis(600 * (attempt + 1)));
                }
            }
        }
    }
    Err(last)
}

const RANK_SPOOF_PLAYLISTS: &[(i32, &str)] = &[
    (10, "Ranked 1v1"),
    (11, "Ranked 2v2"),
    (13, "Ranked 3v3"),
    (27, "Hoops"),
    (28, "Rumble"),
    (29, "Dropshot"),
    (30, "Snow Day"),
    (63, "Heatseeker"),
];

#[allow(dead_code)]
fn playlist_name(id: i32) -> &'static str {
    match id {
        10 => "Ranked 1v1",
        11 => "Ranked 2v2",
        13 => "Ranked 3v3",
        27 => "Hoops",
        28 => "Rumble",
        29 => "Dropshot",
        30 => "Snow Day",
        63 => "Heatseeker",
        _ => "Unknown Playlist",
    }
}

fn rank_name(id: i32) -> &'static str {
    match id {
        0 => "Unranked",
        1 => "Bronze I",
        2 => "Bronze II",
        3 => "Bronze III",
        4 => "Silver I",
        5 => "Silver II",
        6 => "Silver III",
        7 => "Gold I",
        8 => "Gold II",
        9 => "Gold III",
        10 => "Platinum I",
        11 => "Platinum II",
        12 => "Platinum III",
        13 => "Diamond I",
        14 => "Diamond II",
        15 => "Diamond III",
        16 => "Champion I",
        17 => "Champion II",
        18 => "Champion III",
        19 => "Grand Champion I",
        20 => "Grand Champion II",
        21 => "Grand Champion III",
        22 => "Supersonic Legend",
        _ => "Unknown Rank",
    }
}

fn default_mu_for_tier(tier: i32) -> f64 {
    match tier {
        0 => 0.0,
        1 => 15.0,
        2 => 18.0,
        3 => 21.0,
        4 => 24.0,
        5 => 27.0,
        6 => 30.0,
        7 => 33.0,
        8 => 36.0,
        9 => 39.0,
        10 => 42.0,
        11 => 45.0,
        12 => 48.0,
        13 => 52.0,
        14 => 56.0,
        15 => 60.0,
        16 => 64.0,
        17 => 68.0,
        18 => 72.0,
        19 => 78.0,
        20 => 84.0,
        21 => 90.0,
        22 => 95.0,
        _ => 50.0,
    }
}

#[derive(Clone, Debug)]
struct RankSpoofState {
    enabled: bool,
    rank: i32,
    mmr: String,
}

#[derive(Clone, serde::Deserialize)]
struct TitleCatalogEntry {
    id: String,
    text: String,
    color: Option<String>,
    glow: Option<String>,
}

impl TitleCatalogEntry {
    fn game_color(&self) -> Color32 {
        let [red, green, blue] = self
            .color
            .as_deref()
            .map(parse_hex_color)
            .unwrap_or([0xE8, 0xE8, 0xE8]);
        Color32::from_rgb(red, green, blue)
    }

    fn has_glow(&self) -> bool {
        self.glow.is_some()
    }
}

fn embedded_titles() -> Vec<TitleCatalogEntry> {
    serde_json::from_str::<serde_json::Value>(include_str!("../assets/catalogs/titles.json"))
        .ok()
        .and_then(|root| serde_json::from_value(root.get("titles")?.clone()).ok())
        .unwrap_or_default()
}

const TITLE_RANK_TOKENS: [(&str, &str); 8] = [
    ("{Bronze}", "{Bronze}"),
    ("{Silver}", "{Silver}"),
    ("{Gold}", "{Gold}"),
    ("{Platinum}", "{Platinum}"),
    ("{Diamond}", "{Diamond}"),
    ("{Champion}", "{Champion}"),
    ("{GrandChampion}", "{GrandChampion}"),
    ("{Legend}", "{Legend}"),
];

fn parse_hex_color(value: &str) -> [u8; 3] {
    let value = value.trim().trim_start_matches('#');
    if value.len() == 6 {
        if let (Ok(red), Ok(green), Ok(blue)) = (
            u8::from_str_radix(&value[0..2], 16),
            u8::from_str_radix(&value[2..4], 16),
            u8::from_str_radix(&value[4..6], 16),
        ) {
            return [red, green, blue];
        }
    }
    [0xE8, 0xE8, 0xE8]
}

impl RankSpoofState {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.enabled,
            "rank": self.rank,
            "mmr": self.mmr
        })
    }
    fn from_json(v: &serde_json::Value) -> Self {
        Self {
            enabled: v.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false),
            rank: v.get("rank").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            mmr: v
                .get("mmr")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct FriendSpoofState {
    enabled: bool,
    original_name: String,
    spoofed_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Console,
    Workshop,
    Spoofer,
    Patcher,
    Settings,
    Plugins,
    About,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatcherSubTab {
    Ball,
    BoostMeter,
    Decal,
    Swapper(crate::swapper::SwapCategory),
    Active,
    Presets,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsSubTab {
    Hebnix,
    Plugin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HebnixSettingsTab {
    Interface,
    Directories,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpooferSubTab {
    Settings,
    Username,
    TitleRank,
    Friends,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatsApiNotice {
    NotConfigured,
    TooLow(i64),
    BelowTarget(i64),
    AboveTarget(i64),
}

impl StatsApiNotice {
    fn classify(rate: Option<&str>) -> Option<Self> {
        match rate.map(str::trim) {
            None => Some(StatsApiNotice::NotConfigured),
            Some(s) => match s.parse::<i64>() {
                Err(_) => Some(StatsApiNotice::NotConfigured),
                Ok(0) => Some(StatsApiNotice::NotConfigured),
                Ok(n) if n <= 10 => Some(StatsApiNotice::TooLow(n)),
                Ok(n) if n < 20 => Some(StatsApiNotice::BelowTarget(n)),
                Ok(20) => None,
                Ok(n) => Some(StatsApiNotice::AboveTarget(n)),
            },
        }
    }

    fn blocking(self) -> bool {
        matches!(
            self,
            StatsApiNotice::NotConfigured | StatsApiNotice::TooLow(_)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebPortNotice {
    NotConfigured,
    InvalidPort(u16),
}

#[derive(Default)]
struct InstallModal {
    open: bool,
    hebnix_stage: bool,
    fetching: bool,
    downloading_id: Option<String>,
    error: Option<String>,
    catalog: Vec<Value>,
    search_query: String,
    current_page: usize,
    images: HashMap<String, ImageState>,
}

pub struct HebnixApp {
    base_dir: PathBuf,
    themes_dir: PathBuf,
    fonts_dir: PathBuf,
    plugin_dir: PathBuf,
    config: Config,

    tx: Sender<AppMsg>,
    rx: Receiver<AppMsg>,

    stats: Arc<StatsClient>,
    ws_stats: Arc<hebnix_sdk::stats::websocket::WsStatsClient>,
    stats_tx: crossbeam_channel::Sender<hebnix_sdk::stats::StatsEvent>,
    monitor: Monitor,
    plugin_mgr: PluginManager,
    tray: Option<Tray>,
    hotkey: Option<ToggleHotkey>,

    tab: Tab,
    settings_subtab: SettingsSubTab,
    hebnix_settings_tab: HebnixSettingsTab,
    spoofer_subtab: SpooferSubTab,
    patcher_subtab: PatcherSubTab,
    console: ConsoleState,
    workshop: WorkshopState,

    hidden: bool,
    topmost: bool,
    currently_connected: bool,
    last_rl_open: bool,
    last_api_open: bool,
    in_match: bool,
    /// true once MatchEnded fires, until MatchDestroyed/disconnect. while
    /// set, UpdateState (which keeps arriving through the post-game screen)
    /// doesn't re-set in_match, so hebnix.input.send unlocks right at the
    /// final whistle instead of staying locked until the lobby is left.
    match_ended: bool,
    first_status: bool,
    status_text: String,
    status_color: Color32,

    capturing_hotkey: bool,
    theme_options: Vec<String>,
    packet_rate: Option<String>,
    port_value: Option<String>,
    web_port_value: Option<String>,
    packet_rate_edit: String,
    port_edit: String,
    web_port_edit: String,
    current_api_port: u16,

    selected_settings_plugin: Option<String>,
    install_modal: InstallModal,
    restart_notice: bool,
    window_mode: Option<WindowMode>,
    statsapi_notice: Option<StatsApiNotice>,
    statsapi_apply_error: Option<String>,
    statsapi_checked: bool,
    web_port_notice: Option<WebPortNotice>,
    fullscreen_notice: bool,
    fullscreen_notice_dismissed: bool,
    startup_enabled: bool,
    quitting: bool,
    last_size: (u32, u32),
    overlay: crate::overlay::Overlay,
    native_overlay: crate::overlay::native::NativeOverlay,
    webview: Option<crate::webview::host::WebviewHost>,
    overlay_unavailable_said: bool,
    overlay_rect: Option<(i32, i32, i32, i32)>,
    overlay_rect_checked: Option<std::time::Instant>,
    plugin_monitor_size: (f32, f32),
    plugin_monitor_checked: Option<std::time::Instant>,

    update_info: Option<crate::update::UpdateInfo>,
    update_downloading: bool,
    update_error: Option<String>,
    changelog_popup: Option<crate::update::ChangelogEntry>,
    launch_path_notice: bool,

    spoofer_mgr: Arc<SpooferManager>,
    spoofer_master: bool,
    spoofer_http_proxy: bool,
    spoofer_socket_proxy: bool,
    spoofer_username_enabled: bool,
    spoofer_username: String,
    spoofer_username_history: Vec<String>,
    spoofer_title_enabled: bool,
    spoofer_title: String,
    spoofer_title_color: [u8; 3],
    spoofer_title_glow: bool,
    spoofer_title_target: Option<String>,
    spoofer_title_filter: String,
    spoofer_title_copy: Option<String>,
    spoofer_title_copy_filter: String,
    title_catalog: Vec<TitleCatalogEntry>,
    spoofer_rank_enabled: bool,
    spoofer_ranks: HashMap<i32, RankSpoofState>,
    spoofer_cert_installed: bool,
    spoofer_friends_enabled: bool,
    spoofer_friends: HashMap<String, FriendSpoofState>,
    friends_search: String,

    patcher_ball: crate::ball::PatcherState,
    patcher_boost: crate::boost_patcher::BoostPatcherState,
    patcher_decal: crate::decal_patcher::DecalPatcherState,
    swapper: crate::swapper::SwapperState,
    admin_prompt_open: bool,
    owned_admin_requested: bool,
    owned_proxy_prompt_open: bool,
    presets: crate::presets::PresetStore,
}

fn clear_rl_cache(tx: &Sender<AppMsg>) {
    let Ok(user_profile) = std::env::var("USERPROFILE") else {
        return;
    };
    let cache_dir =
        std::path::Path::new(&user_profile).join(r"Documents\My Games\Rocket League\TAGame\Cache");

    if cache_dir.is_dir() {
        if let Err(e) = std::fs::remove_dir_all(&cache_dir) {
            let _ = tx.send(AppMsg::Log(format!("[Spoofer] cant clear cache: {e}")));
            return;
        }
        let _ = std::fs::create_dir_all(&cache_dir);
        let _ = tx.send(AppMsg::Log("[Spoofer] cleared Rocket League cache".into()));
    }
}

impl HebnixApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let base_dir = crate::config::base_dir();
        let themes_dir = base_dir.join("themes");
        let fonts_dir = base_dir.join("fonts");
        let plugin_dir = base_dir.join("plugins");
        let _ = std::fs::create_dir_all(&themes_dir);
        let _ = std::fs::create_dir_all(&fonts_dir);
        let _ = std::fs::create_dir_all(&plugin_dir);

        let mut config = Config::load(&base_dir);

        let error_file = base_dir.join("theme_errors.txt");
        match theme::apply_theme(
            &cc.egui_ctx,
            &themes_dir,
            &fonts_dir,
            &config.settings.theme,
        ) {
            Ok(()) => {
                let _ = std::fs::remove_file(&error_file);
            }
            Err(e) => {
                let _ = std::fs::write(
                    &error_file,
                    format!("Startup Theme Error ({}): {e}\n", config.settings.theme),
                );
                let _ = theme::apply_theme(&cc.egui_ctx, &themes_dir, &fonts_dir, "Dark");
                config.settings.theme = "Dark".to_string();
                let _ = config.save(&base_dir);
            }
        }
        theme::apply_window_opacity(&cc.egui_ctx, config.settings.window_opacity);
        let theme_options = theme::list_themes(&themes_dir);

        let (tx, rx) = crossbeam_channel::unbounded::<AppMsg>();

        let show_changelog = base_dir.join(".first").exists();
        let launch_marker = base_dir.join(".launch");
        if launch_marker.exists() {
            let _ = std::fs::remove_file(&launch_marker);
        }
        let tx_update = tx.clone();
        let ctx_update = cc.egui_ctx.clone();
        std::thread::Builder::new()
            .name("update-checker".into())
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
            std::thread::Builder::new()
                .name("stats-forwarder".into())
                .spawn(move || {
                    while let Ok(event) = stats_rx.recv() {
                        let _ = tx.send(AppMsg::GameEvent(event));
                        ctx.request_repaint();
                    }
                })
                .ok();
        }

        let ws_stats = Arc::new(hebnix_sdk::stats::websocket::WsStatsClient::new(
            "127.0.0.1",
            49124,
        ));

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

        let hidden = config.settings.start_in_tray;
        let tray = Tray::new(&base_dir, "Hebnix", hidden);
        if let Some(tray) = &tray {
            let open_id = tray.open_id.clone();
            let quit_id = tray.quit_id.clone();
            let tx = tx.clone();
            let ctx = cc.egui_ctx.clone();
            std::thread::Builder::new()
                .name("tray-forwarder".into())
                .spawn(move || {
                    let receiver = tray_icon::menu::MenuEvent::receiver();
                    while let Ok(event) = receiver.recv() {
                        if event.id == open_id {
                            let hidden = !winutil::main_window_hidden();
                            if hidden {
                                winutil::set_main_window_invisible(true);
                            } else {
                                winutil::show_main_window_from_tray();
                            }
                            let _ = tx.send(AppMsg::TrayVisibility(hidden));
                        } else if event.id == quit_id {
                            let _ = tx.send(AppMsg::TrayQuit);
                        }
                        ctx.request_repaint();
                    }
                })
                .ok();
        }

        let mut hotkey = ToggleHotkey::new();
        if let Some(hk) = &mut hotkey {
            hk.rebind(&config.settings.hotkey);
        }
        if let Some(hotkey) = &hotkey {
            let tx = tx.clone();
            let ctx = cc.egui_ctx.clone();
            hotkey.listen(move || {
                let _ = tx.send(AppMsg::ToggleVisibility);
                ctx.request_repaint();
            });
        }

        if let Some(hwnd) = winutil::main_window_hwnd() {
            crate::dpi_fix::install(hwnd);
            winutil::install_minimize_hook(hwnd, &cc.egui_ctx);
        } else {
            tracing::warn!("dpi_fix: main window not found at startup");
        }

        let mut workshop = WorkshopState::new(&base_dir);
        workshop.fetch_catalog(tx.clone(), cc.egui_ctx.clone());

        let last_size = (config.window.width, config.window.height);
        let startup_enabled = winutil::is_startup_enabled();

        let spoofer_mgr = Arc::new(SpooferManager::new(base_dir.clone(), tx.clone()));

        let mut spoofer_master = false;
        let mut spoofer_http_proxy = false;
        let mut spoofer_socket_proxy = false;
        let mut spoofer_username_enabled = false;
        let mut spoofer_username = "Hebnix".to_string();
        let mut spoofer_username_history = Vec::new();
        let mut spoofer_title_enabled = false;
        let mut spoofer_title = String::new();
        let mut spoofer_title_color = [0xE8, 0xE8, 0xE8];
        let mut spoofer_title_glow = false;
        let mut spoofer_title_target = None;
        let mut spoofer_title_copy = None;
        let mut spoofer_rank_enabled = false;
        let mut spoofer_ranks = HashMap::new();

        if let Ok(text) = std::fs::read_to_string(base_dir.join("spoofer_settings.json")) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                spoofer_master = json
                    .get("spoofer_master")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                spoofer_http_proxy = json
                    .get("spoofer_http_proxy")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                spoofer_socket_proxy = json
                    .get("spoofer_socket_proxy")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                spoofer_username_enabled = json
                    .get("spoofer_username_enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if let Some(s) = json.get("spoofer_username").and_then(|v| v.as_str()) {
                    spoofer_username = s.to_string();
                }
                if let Some(history) = json
                    .get("spoofer_username_history")
                    .and_then(|v| v.as_array())
                {
                    spoofer_username_history = history
                        .iter()
                        .filter_map(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(|value| value.chars().take(spoofer::MAX_NAME_LENGTH).collect())
                        .take(3)
                        .collect();
                }
                spoofer_title_enabled = json
                    .get("spoofer_title_enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if let Some(s) = json.get("spoofer_title").and_then(|v| v.as_str()) {
                    spoofer_title = s.to_string();
                }
                if let Some(color) = json.get("spoofer_title_color").and_then(|v| v.as_str()) {
                    spoofer_title_color = parse_hex_color(color);
                }
                spoofer_title_glow = json
                    .get("spoofer_title_glow")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                spoofer_title_target = json
                    .get("spoofer_title_target")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                spoofer_title_copy = json
                    .get("spoofer_title_copy")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                spoofer_rank_enabled = json
                    .get("spoofer_rank_enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                if let Some(ranks_obj) = json.get("ranks").and_then(|v| v.as_object()) {
                    for (k, v) in ranks_obj {
                        if let Ok(id) = k.parse::<i32>() {
                            spoofer_ranks.insert(id, RankSpoofState::from_json(v));
                        }
                    }
                }
                // Live match data identifies Heatseeker as playlist 63.
                // Retain an existing spoof when migrating the earlier IDs.
                if !spoofer_ranks.contains_key(&63) {
                    if let Some(state) = spoofer_ranks
                        .remove(&34)
                        .or_else(|| spoofer_ranks.remove(&43))
                        .or_else(|| spoofer_ranks.remove(&38))
                    {
                        spoofer_ranks.insert(63, state);
                    }
                }
                spoofer_ranks.remove(&34);
                spoofer_ranks.remove(&38);
                spoofer_ranks.remove(&43);
            }
        }

        if spoofer_master && !spoofer::is_admin() {
            if spoofer::spawn_elevated_relaunch() {
                std::process::exit(0);
            } else {
                spoofer_master = false;
            }
        }

        let mut spoofer_friends_enabled = false;
        let mut spoofer_friends: HashMap<String, FriendSpoofState> = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(base_dir.join("friends.json")) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                spoofer_friends_enabled = json
                    .get("spoofer_friends_enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if let Some(obj) = json.get("friends").and_then(|v| v.as_object()) {
                    for (k, v) in obj {
                        if let Ok(state) = serde_json::from_value(v.clone()) {
                            spoofer_friends.insert(k.clone(), state);
                        }
                    }
                }
            }
        }

        spoofer_mgr.set_username(&spoofer_username);
        spoofer_mgr.set_title(&spoofer_title);
        spoofer_mgr.set_title_enabled(spoofer_title_enabled);
        spoofer_mgr.set_title_options(
            format!(
                "{:02X}{:02X}{:02X}",
                spoofer_title_color[0], spoofer_title_color[1], spoofer_title_color[2]
            ),
            spoofer_title_glow,
            spoofer_title_target.clone(),
        );

        let patcher_ball = crate::ball::PatcherState::new(&base_dir, &config);
        let patcher_boost = crate::boost_patcher::BoostPatcherState::new(&base_dir, &config);
        let patcher_decal = crate::decal_patcher::DecalPatcherState::new(&base_dir, &config);
        let mut swapper = crate::swapper::SwapperState::new(&base_dir);
        let owned_restart_marker = base_dir.join("enable_owned_replacements.pending");
        if spoofer::is_admin() && owned_restart_marker.is_file() {
            let _ = std::fs::remove_file(&owned_restart_marker);
            spoofer_master = true;
            spoofer_http_proxy = true;
            swapper.set_owned_only(true);
        }
        let cert_installed = spoofer::ca::is_current_installed(&base_dir);

        let mut app = Self {
            base_dir: base_dir.clone(),
            themes_dir,
            fonts_dir,
            plugin_dir,
            config,
            tx,
            rx,
            stats,
            ws_stats,
            stats_tx,
            monitor,
            plugin_mgr,
            tray,
            hotkey,
            tab: Tab::Console,
            settings_subtab: SettingsSubTab::Hebnix,
            hebnix_settings_tab: HebnixSettingsTab::Interface,
            spoofer_subtab: SpooferSubTab::Settings,
            patcher_subtab: PatcherSubTab::Ball,
            console: ConsoleState::default(),
            workshop,
            hidden,
            topmost: false,
            currently_connected: false,
            last_rl_open: false,
            last_api_open: false,
            in_match: false,
            match_ended: false,
            first_status: true,
            status_text: String::new(),
            status_color: Color32::from_rgb(0xDC, 0xE4, 0xEE),
            capturing_hotkey: false,
            theme_options,
            packet_rate: None,
            port_value: None,
            web_port_value: None,
            packet_rate_edit: String::new(),
            port_edit: String::new(),
            web_port_edit: String::new(),
            current_api_port: 49123,
            selected_settings_plugin: None,
            install_modal: InstallModal::default(),
            restart_notice: false,
            window_mode: None,
            statsapi_notice: None,
            statsapi_apply_error: None,
            statsapi_checked: false,
            web_port_notice: None,
            fullscreen_notice: false,
            fullscreen_notice_dismissed: false,
            startup_enabled,
            quitting: false,
            last_size,
            overlay: crate::overlay::Overlay::new(),
            native_overlay: crate::overlay::native::NativeOverlay::new(),
            webview: None,
            overlay_unavailable_said: false,
            overlay_rect: None,
            overlay_rect_checked: None,
            plugin_monitor_size: (1920.0, 1080.0),
            plugin_monitor_checked: None,

            update_info: None,
            update_downloading: false,
            update_error: None,
            changelog_popup: None,
            launch_path_notice: false,
            spoofer_mgr,
            spoofer_master,
            spoofer_http_proxy,
            spoofer_socket_proxy,
            spoofer_username_enabled,
            spoofer_username,
            spoofer_username_history,
            spoofer_title_enabled,
            spoofer_title,
            spoofer_title_color,
            spoofer_title_glow,
            spoofer_title_target,
            spoofer_title_filter: String::new(),
            spoofer_title_copy,
            spoofer_title_copy_filter: String::new(),
            title_catalog: embedded_titles(),
            spoofer_rank_enabled,
            spoofer_ranks,
            spoofer_cert_installed: cert_installed,
            spoofer_friends_enabled,
            spoofer_friends,
            friends_search: String::new(),
            patcher_ball,
            patcher_boost,
            patcher_decal,
            swapper,
            admin_prompt_open: false,
            owned_admin_requested: false,
            owned_proxy_prompt_open: false,
            presets: crate::presets::PresetStore::new(&base_dir.clone()),
        };

        if hidden {
            winutil::set_main_window_invisible(true);
        }
        app.plugin_mgr.shared.borrow_mut().is_gui_open = !hidden;
        app.refresh_stats_api_viewer();
        app.check_web_port();

        app.save_friends_internal();
        app.save_ranks_internal();
        app.evaluate_proxies();

        app
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config.save(&self.base_dir) {
            self.console
                .write(format!("[Console] Failed to save config: {e}"));
        }

        let mut map = serde_json::Map::new();
        for (k, v) in &self.spoofer_ranks {
            map.insert(k.to_string(), v.to_json());
        }
        let json_obj = serde_json::json!({
            "spoofer_master": self.spoofer_master,
            "spoofer_http_proxy": self.spoofer_http_proxy,
            "spoofer_socket_proxy": self.spoofer_socket_proxy,
            "spoofer_username_enabled": self.spoofer_username_enabled,
            "spoofer_username": self.spoofer_username,
            "spoofer_username_history": self.spoofer_username_history,
            "spoofer_title_enabled": self.spoofer_title_enabled,
            "spoofer_title": self.spoofer_title,
            "spoofer_title_color": format!("{:02X}{:02X}{:02X}", self.spoofer_title_color[0], self.spoofer_title_color[1], self.spoofer_title_color[2]),
            "spoofer_title_glow": self.spoofer_title_glow,
            "spoofer_title_target": self.spoofer_title_target,
            "spoofer_title_copy": self.spoofer_title_copy,
            "spoofer_rank_enabled": self.spoofer_rank_enabled,
            "ranks": map,
        });

        let _ = std::fs::write(
            self.base_dir.join("spoofer_settings.json"),
            serde_json::to_string_pretty(&json_obj).unwrap_or_default(),
        );

        // Persist patch status beside the currently selected game too.  This
        // keeps Steam and Epic active-state manifests independent across both
        // app restarts and live store switches.
        let game_root = self.config.settings.rl_path.clone();
        self.remember_patcher_state(&game_root);
    }

    fn remember_username_spoof(&mut self) {
        let name = self.spoofer_username.trim();
        if name.is_empty() {
            return;
        }
        self.spoofer_username_history.retain(|saved| saved != name);
        self.spoofer_username_history.insert(0, name.to_owned());
        self.spoofer_username_history.truncate(3);
    }

    fn render_presets_tab(&mut self, ui: &mut egui::Ui, backups_dir: &std::path::Path) {
        ui.heading("Presets");
        ui.label("Save and restore named collections of your current item changes.");
        ui.horizontal(|ui| {
            ui.label("Name:");
            ui.text_edit_singleline(&mut self.presets.name_edit);
            ui.checkbox(&mut self.presets.include_patches, "Include patches");
            if ui.button("Save Preset").clicked() {
                let swaps = std::fs::read(backups_dir.join("swapper_swaps.json"))
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Vec<serde_json::Value>>(&b).ok())
                    .unwrap_or_default();
                let patches = if self.presets.include_patches {
                    serde_json::json!({
                        "ball": self.patcher_ball.active_ball,
                        "boost": self.patcher_boost.active_boost,
                        "decals": self.patcher_decal.active_decals,
                    })
                } else {
                    serde_json::Value::Null
                };
                let preset = crate::presets::Preset {
                    name: self.presets.name_edit.trim().to_string(),
                    include_patches: self.presets.include_patches,
                    patches,
                    swaps,
                };
                if let Err(error) = self.presets.save(preset) {
                    self.console.write(format!("[Presets] {error}"));
                }
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Refresh").clicked() {
                self.presets.refresh();
            }
            if ui.button("Import JSON").clicked() {
                if let Some(file) = rfd::FileDialog::new()
                    .add_filter("JSON", &["json"])
                    .pick_file()
                {
                    if let Ok(bytes) = std::fs::read(file) {
                        if let Ok(preset) = serde_json::from_slice::<crate::presets::Preset>(&bytes)
                        {
                            let _ = self.presets.save(preset);
                        }
                    }
                }
            }
            if ui
                .add_enabled(
                    !self.presets.presets.is_empty(),
                    egui::Button::new("Delete"),
                )
                .clicked()
            {
                self.presets.delete_selected();
            }
        });
        ui.separator();
        if self.presets.presets.is_empty() {
            ui.weak("No presets saved.");
            return;
        }
        let names: Vec<String> = self
            .presets
            .presets
            .iter()
            .map(|p| p.name.clone())
            .collect();
        egui::ComboBox::from_id_salt("preset_select")
            .selected_text(&names[self.presets.selected])
            .show_ui(ui, |ui| {
                for (index, name) in names.iter().enumerate() {
                    ui.selectable_value(&mut self.presets.selected, index, name);
                }
            });
        if let Some(preset) = self.presets.presets.get(self.presets.selected) {
            ui.add_space(8.0);
            ui.strong(format!(
                "{}{}",
                preset.name,
                if preset.include_patches {
                    " (with patches)"
                } else {
                    " (swaps only)"
                }
            ));
            for swap in &preset.swaps {
                let source = swap
                    .get("source_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let target = swap
                    .get("target_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                ui.label(format!("{source}  →  {target}"));
            }
            if let Some(obj) = preset.patches.as_object() {
                for (kind, value) in obj {
                    if !value.is_null() {
                        ui.label(format!("{kind}: {value}"));
                    }
                }
            }
            ui.weak("Preset contents are shown above; applying a preset will be enabled alongside the patch restore/apply operations.");
        }
    }

    fn save_friends_internal(&self) {
        let mut to_save = HashMap::new();
        for (id, state) in &self.spoofer_friends {
            if !state.spoofed_name.trim().is_empty() {
                to_save.insert(id.clone(), state.clone());
            }
        }
        let obj = serde_json::json!({
            "spoofer_friends_enabled": self.spoofer_friends_enabled,
            "friends": to_save
        });
        let _ = std::fs::write(
            self.base_dir.join("friends.json"),
            serde_json::to_string_pretty(&obj).unwrap_or_default(),
        );

        let mut active = HashMap::new();
        if self.spoofer_master && self.spoofer_http_proxy && self.spoofer_friends_enabled {
            for (id, state) in &self.spoofer_friends {
                if state.enabled && !state.spoofed_name.trim().is_empty() {
                    active.insert(id.clone(), state.spoofed_name.clone());
                }
            }
        }
        self.spoofer_mgr.update_friends(active);
    }

    fn save_friends(&mut self) {
        self.save_friends_internal();
    }

    fn save_ranks_internal(&self) {
        let mut active = HashMap::new();
        if self.spoofer_master && self.spoofer_http_proxy && self.spoofer_rank_enabled {
            for (id, state) in &self.spoofer_ranks {
                if state.enabled
                    && RANK_SPOOF_PLAYLISTS
                        .iter()
                        .any(|(supported, _)| id == supported)
                {
                    let mu = if let Ok(mmr) = state.mmr.parse::<f64>() {
                        f64::max(0.0, (mmr - 100.0) / 20.0)
                    } else {
                        default_mu_for_tier(state.rank)
                    };
                    active.insert(*id, (state.rank, mu));
                }
            }
        }
        self.spoofer_mgr.update_ranks(active);
    }

    fn save_ranks(&mut self) {
        self.save_ranks_internal();
        self.save_config();
    }

    fn evaluate_proxies(&mut self) {
        let cache_cleared = false;

        if !self.spoofer_master {
            if self.spoofer_mgr.socket_running() {
                self.spoofer_mgr.stop_socket();
                clear_rl_cache(&self.tx);
            }
            if self.spoofer_mgr.http_running() {
                self.spoofer_mgr.stop_http();
            }
            return;
        }

        let needs_http = self.spoofer_http_proxy
            && (self.spoofer_username_enabled
                || self.spoofer_friends_enabled
                || self.spoofer_rank_enabled
                || self.swapper.owned_only());
        if needs_http && !self.spoofer_mgr.http_running() {
            if let Err(e) = self.spoofer_mgr.start_http() {
                self.console
                    .write(format!("[Spoofer] Failed to start account proxy: {e}"));
            } else {
                self.console.write("[Spoofer] Account proxy started.");
            }
        } else if !needs_http && self.spoofer_mgr.http_running() {
            self.spoofer_mgr.stop_http();
            self.console.write("[Spoofer] Account proxy stopped.");
        }

        // Rank spoofing uses the same hosts-backed config.psynet.gg reverse
        // proxy as the C# implementation. PsyNet bypasses Windows' HTTP proxy
        // on current clients, so this must not depend on the Title toggle.
        let needs_socket =
            (self.spoofer_socket_proxy && self.spoofer_title_enabled) || self.spoofer_rank_enabled;
        if needs_socket && !self.spoofer_mgr.socket_running() {
            if let Err(e) = self.spoofer_mgr.start_socket() {
                self.console
                    .write(format!("[Spoofer] Failed to start PsyNet proxy: {e}"));
            } else {
                self.console.write("[Spoofer] PsyNet proxy started.");
            }
        } else if !needs_socket && self.spoofer_mgr.socket_running() {
            self.spoofer_mgr.stop_socket();
            self.console.write("[Spoofer] PsyNet proxy stopped.");
            if !cache_cleared {
                clear_rl_cache(&self.tx);
            }
        }

        self.save_friends_internal();
        self.save_ranks_internal();
    }

    fn set_hidden(&mut self, ctx: &egui::Context, hidden: bool) {
        tracing::info!(hidden, "toggling main window visibility");
        let rocket_league_had_focus = !hidden && hebnix_sdk::process::is_rocket_league_focused();
        if !hidden {
            winutil::note_foreground();
        }
        self.hidden = hidden;

        if let Some(tray) = &self.tray {
            tray.set_hidden(hidden);
        }
        winutil::set_main_window_invisible(hidden);
        ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(hidden));

        if hidden {
            self.topmost = false;
            winutil::set_main_window_topmost(false);
            winutil::restore_foreground();
        } else {
            if self.topmost || hebnix_sdk::process::is_rocket_league_focused() {
                self.topmost = true;
                winutil::set_main_window_topmost(true);
            }
            let game_fullscreen =
                self.last_rl_open && self.window_mode == Some(WindowMode::Fullscreen);
            if !game_fullscreen && !rocket_league_had_focus {
                winutil::focus_main_window();
            }
        }

        ctx.request_repaint();
        self.plugin_mgr.dispatch_gui_visibility(!hidden);
    }

    fn handle_toggle_visibility(&mut self, ctx: &egui::Context) {
        let hebnix_focused = winutil::foreground_window_is_ours();
        let rocket_league_focused = hebnix_sdk::process::is_rocket_league_focused();
        if self
            .config
            .settings
            .restrict_hotkey_to_hebnix_or_rocket_league
            && !hebnix_focused
            && !rocket_league_focused
        {
            return;
        }

        self.set_hidden(ctx, !self.hidden);
    }

    fn force_quit(&mut self, _ctx: &egui::Context) {
        self.quitting = true;
        self.config.window.width = self.last_size.0;
        self.config.window.height = self.last_size.1;
        self.save_config();

        self.spoofer_mgr.shutdown();
        self.plugin_mgr.unload_all();
        self.stats.stop();
        self.ws_stats.stop();
        self.monitor.stop();
        self.tray = None;
        std::process::exit(0);
    }

    fn handle_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                AppMsg::Log(s) => self.console.write(s),
                AppMsg::GameEvent(event) => self.handle_game_event(event),
                AppMsg::RlStatus {
                    rl_open,
                    api_open,
                    root_dir,
                } => {
                    let rl_state_changed = rl_open != self.last_rl_open;
                    let mut platform_changed = false;
                    if let Some(root) = root_dir {
                        let previous_root = self.config.settings.rl_path.clone();
                        let previous_platform = hebnix_sdk::process::detect_platform(
                            std::path::Path::new(&previous_root),
                        );
                        let detected_platform =
                            hebnix_sdk::process::detect_platform(std::path::Path::new(&root));
                        platform_changed = !previous_root
                            .trim_end_matches(['\\', '/'])
                            .eq_ignore_ascii_case(root.trim_end_matches(['\\', '/']));
                        let platform_switched = previous_platform != detected_platform
                            && previous_platform != hebnix_sdk::process::RlPlatform::Unknown
                            && detected_platform != hebnix_sdk::process::RlPlatform::Unknown;
                        // Snapshot the currently selected installation before path
                        // auto-detection changes it.  Steam and Epic can each have
                        // independent patched files and backups.
                        if platform_changed {
                            self.remember_patcher_state(&previous_root);
                        }
                        if rl_open && (!self.last_rl_open || platform_changed) {
                            // Load the target installation before auto-resolving
                            // paths, since path resolution saves the global config.
                            // Doing this in the opposite order overwrites Epic's
                            // marker with Steam's active patch state (and vice versa).
                            self.reload_game_patcher_state(&root);
                        }
                        self.auto_resolve_paths(&root);
                        if rl_open && (!self.last_rl_open || platform_changed) {
                            self.workshop.manager.reload_install_state(&root);
                        }
                        self.plugin_mgr.shared.borrow_mut().platform =
                            detected_platform.as_str().to_string();
                        if rl_open && platform_switched {
                            self.plugin_mgr.reload_enabled_silent(&mut self.config);
                        }
                    }
                    let launched = rl_open && !self.last_rl_open;

                    self.handle_rl_status(rl_open, api_open);

                    if launched {
                        self.workshop.rocket_league_reopened();
                        self.check_statsapi_rate();
                        self.check_web_port();
                    }
                    if rl_state_changed || platform_changed {
                        self.workshop.refresh_wizard_status(&self.tx, ctx);
                    }
                }
                AppMsg::AppUpdateFetched { result } => match result {
                    Ok(Some(info)) => {
                        self.console
                            .write(format!("[Core] Update available: v{}", info.version));
                        self.update_info = Some(info);
                    }
                    Ok(None) => {
                        self.console
                            .write("[Core] Hebnix is up to date. Checking for plugin updates...");
                        let payload = self
                            .plugin_mgr
                            .plugins
                            .iter()
                            .filter_map(|plugin| {
                                plugin
                                    .manifest
                                    .plugin_id
                                    .as_deref()
                                    .filter(|id| !id.is_empty())
                                    .map(|id| {
                                        serde_json::json!({
                                            "plugin_id": id,
                                            "version": plugin.manifest.version,
                                        })
                                    })
                            })
                            .collect::<Vec<_>>();
                        if payload.is_empty() {
                            self.console.write("[Core] All plugins are up to date.");
                            continue;
                        }
                        let payload_json = serde_json::Value::Array(payload);
                        let tx = self.tx.clone();
                        let ctx_local = ctx.clone();

                        std::thread::spawn(move || {
                            let result: Result<Vec<serde_json::Value>, String> = (|| {
                                let agent =
                                    ureq::AgentBuilder::new().try_proxy_from_env(false).build();
                                let resp = agent.post("https://api.hebnix.com/check")
                                    .set("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
                                    .send_json(payload_json)
                                    .map_err(|e| format!("Failed to check plugin updates: {e}"))?;
                                let json: serde_json::Value =
                                    resp.into_json().map_err(|e| format!("Invalid JSON: {e}"))?;
                                let arr = json.as_array().cloned().unwrap_or_default();
                                Ok(arr)
                            })(
                            );
                            let _ = tx.send(AppMsg::PluginUpdatesFound { updates: result });
                            ctx_local.request_repaint();
                        });
                    }
                    Err(_) => {}
                },
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
                AppMsg::PluginUpdatesFound { updates } => match updates {
                    Ok(list) => {
                        if list.is_empty() {
                            self.console.write("[Core] All plugins are up to date.");
                        } else {
                            for update in list {
                                let plugin_id = update
                                    .get("plugin_id")
                                    .or_else(|| update.get("id"))
                                    .and_then(|value| value.as_str())
                                    .unwrap_or("");
                                if plugin_id.is_empty() {
                                    continue;
                                }
                                if let Some(local_p) =
                                    self.plugin_mgr.plugins.iter().find(|plugin| {
                                        plugin.manifest.plugin_id.as_deref() == Some(plugin_id)
                                    })
                                {
                                    let slug = local_p.slug.clone();
                                    let name = local_p.display_name().to_string();
                                    let was_enabled = local_p.enabled;

                                    self.console.write(format!(
                                        "[Core] Downloading update for plugin '{name}'..."
                                    ));
                                    self.plugin_mgr.set_enabled(&slug, false, &mut self.config);
                                    self.save_config();

                                    let plugin_dir = self.plugin_dir.clone();
                                    let id_str = plugin_id.to_string();
                                    let name_str = name.to_string();
                                    let tx = self.tx.clone();

                                    std::thread::spawn(move || {
                                        let res: Result<String, String> = (|| {
                                            let url = format!(
                                                "https://api.hebnix.com/download/plugin/{id_str}"
                                            );
                                            let resp = get_retry(&url, Duration::from_secs(20))?;
                                            let mut bytes = Vec::new();
                                            std::io::Read::read_to_end(
                                                &mut resp.into_reader(),
                                                &mut bytes,
                                            )
                                            .map_err(|e| e.to_string())?;
                                            let temp_zip = plugin_dir
                                                .join(format!("temp_plugin_{id_str}.zip"));
                                            std::fs::write(&temp_zip, &bytes)
                                                .map_err(|e| e.to_string())?;
                                            let extract = install_zip(&temp_zip, &plugin_dir);
                                            let _ = std::fs::remove_file(&temp_zip);
                                            extract?;
                                            Ok(format!("Plugin '{name_str}' updated successfully."))
                                        })(
                                        );
                                        let _ = tx.send(AppMsg::PluginAutoUpdateDone {
                                            slug,
                                            was_enabled,
                                            result: res,
                                        });
                                    });
                                }
                            }
                        }
                    }
                    Err(e) => self.console.write(e),
                },
                AppMsg::PluginAutoUpdateDone {
                    slug,
                    was_enabled,
                    result,
                } => match result {
                    Ok(msg) => {
                        self.console.write(format!("[Console] {msg}"));
                        self.plugin_mgr.refresh(&mut self.config, true);
                        if was_enabled {
                            self.plugin_mgr.set_enabled(&slug, true, &mut self.config);
                        }
                        self.save_config();
                    }
                    Err(e) => {
                        self.console
                            .write(format!("[Console] Failed to auto-update plugin: {e}"));
                    }
                },
                AppMsg::SendWsCommand(cmd) => {
                    if let Err(e) = self.ws_stats.send_command(cmd) {
                        self.console
                            .write(format!("[Core] Failed to send WS command: {e}"));
                    }
                }
                AppMsg::WindowMode(mode) => {
                    self.window_mode = Some(mode);
                    if mode == WindowMode::Fullscreen {
                        if !self.config.settings.suppress_fullscreen_warning
                            && !self.fullscreen_notice_dismissed
                        {
                            self.fullscreen_notice = true;
                        }
                    } else {
                        self.fullscreen_notice = false;
                        self.fullscreen_notice_dismissed = false;
                    }
                }
                AppMsg::StatsApiInitialised => {
                    self.restart_notice = true;
                    self.console.write(
                        "[Monitor] StatsAPI initialised (PacketSendRate=20). Restart Rocket League."
                            .to_string(),
                    );
                    self.refresh_stats_api_viewer();
                    self.check_statsapi_rate();
                    self.check_web_port();
                }
                AppMsg::ToggleVisibility => {
                    self.handle_toggle_visibility(ctx);
                }
                AppMsg::TrayVisibility(hidden) => {
                    self.set_hidden(ctx, hidden);
                }
                AppMsg::TrayQuit => {
                    if self.update_info.is_none() {
                        self.force_quit(ctx);
                    }
                }
                AppMsg::HotkeyCaptured(name) => {
                    self.capturing_hotkey = false;
                    if let Some(name) = name {
                        self.update_hotkey(&name);
                    }
                }
                AppMsg::Topmost(topmost) => {
                    if self.topmost != topmost {
                        self.topmost = topmost;
                        winutil::set_main_window_topmost(topmost);
                    }
                }
                AppMsg::WorkshopCatalog(result) => match result {
                    Ok(items) => {
                        self.workshop.catalog = items;
                        self.workshop.execute_search(true);
                        if self.workshop.valid.is_empty() {
                            self.workshop.catalog_status = "No maps found.".to_string();
                        }
                    }
                    Err(e) => {
                        self.workshop.catalog_status = "Failed to load maps.".to_string();
                        self.console
                            .write(format!("[Core] Failed to fetch maps: {e}"));
                    }
                },
                AppMsg::WorkshopImage { key, bytes } => {
                    let state = if bytes.is_empty() {
                        ImageState::Failed
                    } else {
                        ImageState::Ready(Arc::from(bytes))
                    };
                    self.workshop.images.insert(key, state);
                }
                AppMsg::PluginImage { key, bytes } => {
                    let state = if bytes.is_empty() {
                        ImageState::Failed
                    } else {
                        ImageState::Ready(Arc::from(bytes))
                    };
                    self.install_modal.images.insert(key, state);
                }
                AppMsg::WorkshopOpDone { message } => {
                    self.console.write(message);
                    self.workshop.finish_op();
                }
                AppMsg::WorkshopMultiplayerProgress(status) => {
                    self.workshop.set_multiplayer_progress(status);
                }
                AppMsg::WorkshopMultiplayerPrepared { result } => {
                    self.workshop.finish_multiplayer_prepare(result);
                }
                AppMsg::WorkshopHostStarted { result } => {
                    self.workshop.finish_hosting(result);
                }
                AppMsg::WorkshopGuestJoined { result } => {
                    self.workshop.finish_joining(result);
                }
                AppMsg::WorkshopPlayerUpdated { result } => {
                    self.workshop.finish_player_update(result);
                }
                AppMsg::WorkshopHostSessionCheck { result } => {
                    self.workshop.finish_host_session_check(result);
                }
                AppMsg::WorkshopWizardCheck {
                    rl_open,
                    tap_ready,
                    launch_ready,
                    detected_map,
                } => {
                    self.workshop.finish_wizard_check(
                        rl_open,
                        tap_ready,
                        launch_ready,
                        detected_map,
                    );
                    if self.workshop.retry_multihome_check() {
                        self.workshop.refresh_wizard_status(&self.tx, ctx);
                    }
                }
                AppMsg::PluginFetch { result } => {
                    self.install_modal.fetching = false;
                    match result {
                        Ok(data) => {
                            if let Some(arr) = data.as_array() {
                                self.install_modal.catalog = arr.clone();
                                self.install_modal.error = None;
                            } else {
                                self.install_modal.error =
                                    Some("Invalid API response format.".to_string());
                            }
                        }
                        Err(e) => {
                            self.install_modal.error = Some(e);
                        }
                    }
                }
                AppMsg::PluginDownloadDone { result } => {
                    self.install_modal.downloading_id = None;
                    match result {
                        Ok(msg) => {
                            self.console.write(format!("[Console] {msg}"));
                            if !self.install_modal.hebnix_stage {
                                self.install_modal = InstallModal::default();
                            }
                            self.plugin_mgr.refresh(&mut self.config, true);
                            self.save_config();
                        }
                        Err(e) => {
                            self.console.write(format!("[Console] Install failed: {e}"));
                        }
                    }
                }
                AppMsg::OverlayPost { slug, data } => {
                    if let Some(webview) = &self.webview {
                        if let Err(error) = webview.deliver(&slug, data) {
                            tracing::debug!("overlay.send from '{slug}' dropped: {error}");
                        }
                    }
                }
                AppMsg::PluginHttpRes {
                    slug,
                    req_id,
                    status,
                    body,
                } => {
                    self.plugin_mgr
                        .on_http_response(&slug, &req_id, status, &body);
                    ctx.request_repaint();
                }
                AppMsg::PluginHttpDownloadRes {
                    slug,
                    req_id,
                    status,
                    body,
                } => {
                    self.plugin_mgr
                        .on_http_download_response(&slug, &req_id, status, &body);
                    ctx.request_repaint();
                }
                AppMsg::PluginHttpRedirectRes {
                    slug,
                    req_id,
                    status,
                    location,
                } => {
                    self.plugin_mgr
                        .on_http_redirect_response(&slug, &req_id, status, &location);
                    ctx.request_repaint();
                }
                AppMsg::PluginHttpUploadRes {
                    slug,
                    req_id,
                    status,
                    body,
                } => {
                    self.plugin_mgr
                        .on_http_upload_response(&slug, &req_id, status, &body);
                    ctx.request_repaint();
                }
            }
        }
    }

    fn handle_game_event(&mut self, event: hebnix_sdk::stats::StatsEvent) {
        match event.event_type.as_str() {
            "UpdateState" => {
                if !self.match_ended {
                    self.in_match = true;
                    self.plugin_mgr.shared.borrow_mut().in_match = true;
                }
                if let Some(state) = event.update_state() {
                    self.workshop
                        .update_workshop_map_from_stats(&state.game.arena, &self.tx);
                }
                self.plugin_mgr.dispatch_game_event(&event);
            }
            "MatchEnded" => {
                self.in_match = false;
                self.match_ended = true;
                self.plugin_mgr.shared.borrow_mut().in_match = false;
                self.plugin_mgr.dispatch_game_event(&event);
            }
            "MatchDestroyed" => {
                self.in_match = false;
                self.match_ended = false;
                self.plugin_mgr.shared.borrow_mut().in_match = false;
                if !self.config.settings.suppress_left_alerts {
                    self.console
                        .write("[Core] Left match or game closed. Resetting plugin metrics.");
                }
                self.plugin_mgr
                    .dispatch_simple("GameLeft", event.raw_data.clone());
            }
            _ => self.plugin_mgr.dispatch_game_event(&event),
        }
    }

    fn handle_rl_status(&mut self, rl_open: bool, api_open: bool) {
        self.last_rl_open = rl_open;
        self.last_api_open = api_open;
        let is_ready = rl_open && api_open;

        if is_ready && !self.currently_connected {
            self.currently_connected = true;
            self.status_text = "✔ Rocket League Connected".to_string();
            self.status_color = Color32::from_rgb(0x2e, 0xcc, 0x71);
            self.console
                .write("[Monitor] Rocket League & StatsAPI detected. Starting listener...");
            self.stats.set_port(self.current_api_port);
            self.stats.start(self.stats_tx.clone());

            let web_port = self
                .web_port_value
                .as_deref()
                .and_then(|p| p.parse().ok())
                .unwrap_or(49124);
            self.ws_stats.set_port(web_port);

            let (ws_tx, ws_dummy_rx) = crossbeam_channel::unbounded();
            std::thread::Builder::new()
                .name("ws-discarder".into())
                .spawn(move || while ws_dummy_rx.recv().is_ok() {})
                .ok();
            self.ws_stats.start(ws_tx);

            self.console.write(format!(
                "[Core] Connected to Rocket League StatsAPI on 127.0.0.1:{} (TCP) and {} (WS)",
                self.current_api_port, web_port
            ));

            self.plugin_mgr
                .dispatch_simple("GameConnected", serde_json::json!({}));
        } else if !is_ready && (self.currently_connected || self.first_status) {
            let was_connected = self.currently_connected;
            self.currently_connected = false;

            if was_connected {
                self.console
                    .write("[Monitor] Connection lost. Halting listener...");
                self.stats.stop();
                if !rl_open {
                    self.workshop.shutdown_multiplayer();
                }
                self.match_ended = false;
                if self.in_match {
                    self.in_match = false;
                    self.plugin_mgr.shared.borrow_mut().in_match = false;
                    self.console
                        .write("[Core] Stats API connection lost. Resetting plugin metrics.");
                    self.plugin_mgr.dispatch_simple(
                        "GameLeft",
                        serde_json::json!({"reason": "connection_lost"}),
                    );
                }
                self.plugin_mgr.dispatch_simple(
                    "GameDisconnected",
                    serde_json::json!({"reason": "connection_lost"}),
                );
            }
        }

        if !self.currently_connected {
            self.status_text = if rl_open {
                "⌛ Rocket League starting...".to_string()
            } else {
                "⌛ Waiting for Rocket League...".to_string()
            };
            self.status_color = Color32::from_rgb(0xDC, 0xE4, 0xEE);
        }

        self.plugin_mgr.shared.borrow_mut().rl_connected = self.currently_connected;
        self.first_status = false;
    }

    fn update_hotkey(&mut self, key_name: &str) {
        let ok = self
            .hotkey
            .as_mut()
            .map(|hk| hk.rebind(key_name))
            .unwrap_or(false);
        if ok {
            self.config.settings.hotkey = key_name.to_string();
            self.save_config();
            self.console.write(format!(
                "[Console] Menu toggle keybind updated to: {}",
                key_name.to_uppercase()
            ));
        } else {
            self.console.write(format!(
                "[Console] Could not bind '{key_name}' (unmappable or already taken by another application). Keeping '{}'.",
                self.config.settings.hotkey.to_uppercase()
            ));
        }
    }

    fn start_hotkey_capture(&mut self, ctx: &egui::Context) {
        if self.capturing_hotkey {
            return;
        }
        self.capturing_hotkey = true;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let name = hebnix_sdk::input::detect_hotkey(Some(Duration::from_secs(10)));
            let _ = tx.send(AppMsg::HotkeyCaptured(name));
            ctx.request_repaint();
        });
    }

    fn auto_resolve_paths(&mut self, root_dir: &str) {
        let norm = |s: &str| s.trim_end_matches(['\\', '/']).to_lowercase();
        let mut changed = false;

        if !self.config.settings.rl_path_confirmed {
            self.config.settings.rl_path_confirmed = true;
            changed = true;
        }

        if norm(&self.config.settings.rl_path) != norm(root_dir) {
            self.config.settings.rl_path = root_dir.to_string();
            self.remember_patcher_state(root_dir);
            changed = true;
        }

        let detected_ini = std::path::Path::new(root_dir)
            .join("TAGame")
            .join("Config")
            .join("DefaultStatsAPI.ini")
            .to_string_lossy()
            .to_string();
        if norm(&self.config.settings.statsapi_path) != norm(&detected_ini) {
            self.config.settings.statsapi_path = detected_ini;
            changed = true;
        }

        if changed {
            self.save_config();
            self.refresh_stats_api_viewer();
        }
    }

    /// Install state is local to the Steam/Epic game directory.  Reload the
    /// marker and all active manifests at the moment RL transitions to open,
    /// so a store switch cannot leave the previous install selected.
    fn reload_game_patcher_state(&mut self, root_dir: &str) {
        let root = std::path::Path::new(root_dir);
        let marker = root.join("patcher.json");
        let same_install = self
            .config
            .settings
            .rl_path
            .trim_end_matches(['\\', '/'])
            .eq_ignore_ascii_case(root_dir.trim_end_matches(['\\', '/']));
        // A legacy marker has only game_path.  Do not turn a known active state
        // into an empty state simply because that old marker lacks the new field.
        let mut patcher = if same_install {
            self.config.patcher.clone()
        } else {
            crate::config::PatcherCfg::default()
        };
        self.config.settings.rl_path = root.to_string_lossy().to_string();
        if let Ok(bytes) = std::fs::read(&marker) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if let Some(saved) = value.get("patcher") {
                    patcher = serde_json::from_value(saved.clone()).unwrap_or_default();
                }
            }
        }
        self.config.patcher = patcher;
        self.patcher_ball.active_ball = self.config.patcher.active_ball.clone();
        self.patcher_boost.active_boost = self.config.patcher.active_boost.clone();
        self.patcher_decal.active_decals = self.config.patcher.active_decals.clone();
        self.patcher_ball.refresh_balls();
        self.patcher_boost.refresh_boosts();
        self.patcher_decal.refresh_decals();
        self.patcher_decal.load_car_skins();
        self.swapper.refresh_catalogs();
        self.save_config();
    }

    /// Keep install-local patcher state beside the game it belongs to. Steam
    /// and Epic have separate CookedPCConsole/Backups trees; a single global
    /// manifest would otherwise make restore operations target the wrong copy.
    fn remember_patcher_state(&self, root_dir: &str) {
        let root = std::path::Path::new(root_dir);
        if !root.is_dir() {
            return;
        }
        let state = serde_json::json!({
            "game_path": root.to_string_lossy(),
            "patcher": &self.config.patcher,
        });
        let _ = std::fs::write(
            root.join("patcher.json"),
            serde_json::to_vec_pretty(&state).unwrap_or_default(),
        );
    }

    fn refresh_stats_api_viewer(&mut self) {
        let ini_path = statsapi_ini::resolve_ini_path(
            &self.config.settings.statsapi_path,
            &self.config.settings.rl_path,
        );
        let resolved_str = ini_path.to_string_lossy().to_string();
        if ini_path.exists() && resolved_str != self.config.settings.statsapi_path {
            self.config.settings.statsapi_path = resolved_str;
            self.save_config();
        }

        let (rate, port, web_port) = statsapi_ini::read_ini(&ini_path);
        self.packet_rate = rate;
        self.port_value = port.clone();
        self.web_port_value = web_port;
        self.packet_rate_edit = self.packet_rate.clone().unwrap_or_default();
        self.port_edit = self.port_value.clone().unwrap_or_default();
        self.web_port_edit = self.web_port_value.clone().unwrap_or_default();

        self.current_api_port = port
            .as_deref()
            .and_then(|p| p.parse().ok())
            .unwrap_or(49123);
        self.stats.set_port(self.current_api_port);
        self.monitor.update_shared(crate::monitor::MonitorShared {
            api_port: self.current_api_port,
            statsapi_path: self.config.settings.statsapi_path.clone(),
            rl_path: self.config.settings.rl_path.clone(),
        });
    }

    fn check_statsapi_rate(&mut self) {
        let ini_path = statsapi_ini::resolve_ini_path(
            &self.config.settings.statsapi_path,
            &self.config.settings.rl_path,
        );
        if !ini_path.exists() {
            return;
        }
        self.statsapi_checked = true;

        let (rate, _, _) = statsapi_ini::read_ini(&ini_path);
        let notice = StatsApiNotice::classify(rate.as_deref());
        let Some(notice) = notice else {
            self.statsapi_notice = None;
            self.statsapi_apply_error = None;
            return;
        };
        if !notice.blocking() && self.config.settings.suppress_statsapi_rate_warning {
            return;
        }
        self.statsapi_notice = Some(notice);
        self.statsapi_apply_error = None;
    }

    fn check_web_port(&mut self) {
        let ini_path = statsapi_ini::resolve_ini_path(
            &self.config.settings.statsapi_path,
            &self.config.settings.rl_path,
        );
        if !ini_path.exists() {
            return;
        }

        let (_, _, web_port) = statsapi_ini::read_ini(&ini_path);
        match web_port.as_deref().map(str::trim) {
            None => self.web_port_notice = Some(WebPortNotice::NotConfigured),
            Some(s) => match s.parse::<u16>() {
                Ok(49124) => self.web_port_notice = None,
                Ok(p) => self.web_port_notice = Some(WebPortNotice::InvalidPort(p)),
                Err(_) => self.web_port_notice = Some(WebPortNotice::NotConfigured),
            },
        }
    }

    fn apply_statsapi_rate(&mut self) {
        let ini_path = statsapi_ini::resolve_ini_path(
            &self.config.settings.statsapi_path,
            &self.config.settings.rl_path,
        );
        match statsapi_ini::update_ini_setting(&ini_path, "PacketSendRate", "20") {
            Ok(()) => {
                self.console
                    .write("[Console] Successfully set PacketSendRate to 20.");
                self.statsapi_notice = None;
                self.statsapi_apply_error = None;
                self.restart_notice = true;
                self.refresh_stats_api_viewer();
            }
            Err(e) => {
                self.statsapi_apply_error = Some(format!("{e}\n{}", ini_path.display()));
                self.console
                    .write(format!("[Console] Failed to update PacketSendRate: {e}"));
            }
        }
    }

    fn apply_web_port_setting(&mut self) {
        let ini_path = statsapi_ini::resolve_ini_path(
            &self.config.settings.statsapi_path,
            &self.config.settings.rl_path,
        );
        match statsapi_ini::update_ini_setting(&ini_path, "WebPort", "49124") {
            Ok(()) => {
                self.console
                    .write("[Console] Successfully set WebPort to 49124.");
                self.web_port_notice = None;
                self.restart_notice = true;
                self.refresh_stats_api_viewer();
            }
            Err(e) => {
                self.console
                    .write(format!("[Console] Failed to update WebPort: {e}"));
            }
        }
    }

    fn update_ini_setting(&mut self, key: &str, value: &str) {
        let ini_path = statsapi_ini::resolve_ini_path(
            &self.config.settings.statsapi_path,
            &self.config.settings.rl_path,
        );
        match statsapi_ini::update_ini_setting(&ini_path, key, value) {
            Ok(()) => {
                self.console
                    .write(format!("[Console] Successfully set {key} to {value}."));
            }
            Err(e) => {
                self.console
                    .write(format!("[Console] Failed to update {key}: {e}"));
            }
        }
        self.refresh_stats_api_viewer();
    }

    fn execute_command(&mut self, raw: String) {
        let parts: Vec<String> = raw.split_whitespace().map(str::to_string).collect();
        let cmd = parts.first().map(|s| s.to_lowercase()).unwrap_or_default();

        match cmd.as_str() {
            "help" => {
                self.console.write("[Console] Hebnix Commands:");
                self.console
                    .write("  help                 - shows this list of commands");
                self.console
                    .write("  info                 - info about the current Hebnix build & state");
                self.console
                    .write("  server               - shows information about the connected server");
                self.console
                    .write("  webview              - state of the overlay webview");
                self.console
                    .write("  clear                - clears the console output");
                self.console
                    .write("  plugins list         - lists all plugins in the plugins folder");
                self.console
                    .write("  plugin load <name>   - load a disabled plugin");
                self.console
                    .write("  plugin reload <name> - reload an enabled plugin");
                self.console
                    .write("  plugin unload <name> - unloads an enabled plugin");
                self.console
                    .write("  quit                 - force kills the Rocket League process");
                self.console
                    .write("  restart              - restarts Rocket League through Steam or Epic");
            }
            "restart" => {
                let path = self.config.settings.rl_path.clone();
                match crate::winutil::restart_rocket_league(std::path::Path::new(&path)) {
                    Ok(()) => self.console.write("[Console] Rocket League restarted."),
                    Err(error) => self
                        .console
                        .write(format!("[Console] Restart failed: {error}")),
                }
            }
            "server" => {
                let sub_cmd = parts.get(1).map(|s| s.to_lowercase());
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let log_info = hebnix_sdk::log::parse_launch_log(None, true, "INT"); // verify so a menu or a closed game cant serve the last match
                    if let Some(game) = log_info.game {
                        let name = game.server_name.unwrap_or_else(|| "Unknown".to_string());
                        let ip = game.server_ip.unwrap_or_else(|| "Unknown".to_string());
                        let port = game
                            .server_port
                            .map(|p| p.to_string())
                            .unwrap_or_else(|| "Unknown".to_string());
                        let region = game.region.unwrap_or_else(|| "Unknown".to_string());
                        let playlist = game
                            .playlist_id
                            .map(|p| p.to_string())
                            .unwrap_or_else(|| "Unknown".to_string());

                        match sub_cmd.as_deref() {
                            Some("name") => {
                                let _ = tx
                                    .send(AppMsg::Log(format!("[Console] Server Name: {}", name)));
                            }
                            Some("ip") | Some("port") => {
                                let _ = tx.send(AppMsg::Log(format!(
                                    "[Console] Server IP/Port: {}:{}",
                                    ip, port
                                )));
                            }
                            Some("region") => {
                                let _ = tx.send(AppMsg::Log(format!(
                                    "[Console] Server Region: {}",
                                    region
                                )));
                            }
                            Some("playlist") => {
                                let _ = tx.send(AppMsg::Log(format!(
                                    "[Console] Playlist ID: {}",
                                    playlist
                                )));
                            }
                            _ => {
                                let _ = tx
                                    .send(AppMsg::Log(format!("[Console] Server Name: {}", name)));
                                let _ = tx.send(AppMsg::Log(format!(
                                    "[Console] Server IP/Port: {}:{}",
                                    ip, port
                                )));
                                let _ = tx.send(AppMsg::Log(format!(
                                    "[Console] Server Region: {}",
                                    region
                                )));
                                let _ = tx.send(AppMsg::Log(format!(
                                    "[Console] Playlist ID: {}",
                                    playlist
                                )));
                            }
                        }
                    } else {
                        let reason = if !hebnix_sdk::process::is_rocket_league_running() {
                            "Rocket League is not running."
                        } else if !log_info.stats_api_available {
                            "Can't read the match, the stats api is not answering."
                        } else {
                            "Not in a game."
                        };
                        let _ = tx.send(AppMsg::Log(format!("[Console] {reason}")));
                    }
                });
            }
            "webview" => {
                let line = match (
                    crate::webview::runtime::is_available(),
                    self.overlay.webview_target().is_some(),
                    self.webview.as_ref(),
                ) {
                    (false, _, _) => "no WebView2 runtime, overlays are off".to_string(),
                    (true, false, _) => "no overlay window on this machine".to_string(),
                    (true, true, None) => "not built, no plugin uses the overlay".to_string(),
                    (true, true, Some(webview)) => webview.status(),
                };
                self.console
                    .write(format!("[Console] Overlay webview: {line}"));
            }
            "info" => {
                self.console
                    .write(format!("[Console] Hebnix Engine Version: {APP_VERSION}"));
                self.console.write(format!(
                    "[Console] Active Base Directory: {}",
                    self.base_dir.display()
                ));
                self.console.write(format!(
                    "[Console] Registered Plugins In Cache: {}",
                    self.plugin_mgr.plugins.len()
                ));
                self.console.write(format!("[Console] StatsAPI: 127.0.0.1:{} | game running: {} | port open: {} | listener connected: {}", self.current_api_port, self.last_rl_open, self.last_api_open, self.currently_connected));
            }
            "clear" => self.console.clear(),
            "quit" => {
                self.console
                    .write("[Console] Killing RocketLeague process threads and exiting...");
                match winutil::kill_rocket_league() {
                    Ok(()) => self
                        .console
                        .write("[Console] Process rocketleague.exe terminated successfully."),
                    Err(e) => self.console.write(format!(
                        "[Console] Process termination execution fault: {e}"
                    )),
                }
            }
            "plugins" if parts.get(1).map(|s| s.to_lowercase()) == Some("list".into()) => {
                self.console.write("[Console] Installed Plugins List:");
                if self.plugin_mgr.plugins.is_empty() {
                    self.console
                        .write("[Console] No plugins currently loaded in memory.");
                }
                let lines: Vec<String> = self
                    .plugin_mgr
                    .plugins
                    .iter()
                    .map(|p| {
                        let status = if p.enabled { "Enabled" } else { "Disabled" };
                        format!(
                            "[Console] - {} v{} ({status})",
                            p.display_name(),
                            p.manifest.version
                        )
                    })
                    .collect();
                for line in lines {
                    self.console.write(line);
                }
            }
            "plugin" if parts.len() >= 3 => {
                let action = parts[1].to_lowercase();
                let target_name = parts[2..].join(" ");
                let slug = self
                    .plugin_mgr
                    .plugins
                    .iter()
                    .find(|p| {
                        p.display_name().eq_ignore_ascii_case(&target_name)
                            || p.slug.eq_ignore_ascii_case(&target_name)
                    })
                    .map(|p| p.slug.clone());

                match (action.as_str(), slug) {
                    ("load" | "reload", Some(slug)) => {
                        if self.plugin_mgr.set_enabled(&slug, true, &mut self.config) {
                            self.console.write(format!(
                                "[Console] Plugin '{target_name}' loaded successfully."
                            ));
                        } else {
                            self.console.write(format!("[Console] Error: Unable to locate or instantiate plugin '{target_name}'."));
                        }
                        self.save_config();
                    }
                    ("unload", Some(slug)) => {
                        self.plugin_mgr.set_enabled(&slug, false, &mut self.config);
                        self.save_config();
                        self.console.write(format!(
                            "[Console] Plugin '{target_name}' unloaded successfully."
                        ));
                    }
                    (_, None) => {
                        self.console.write(format!(
                            "[Console] Error: plugin '{target_name}' not found."
                        ));
                    }
                    _ => {
                        self.console.write(
                            "[Console] Invalid syntax. Format: plugin [load/reload/unload] <name>",
                        );
                    }
                }
            }
            _ => {
                self.console.write(format!("[Console] Command '{raw}' unrecognized. Options: help, info, plugins list, plugin, clear, quit, restart"));
            }
        }
    }

    fn render_console_tab(&mut self, ui: &mut egui::Ui) {
        let plugin_names: Vec<String> = self
            .plugin_mgr
            .plugins
            .iter()
            .map(|p| p.display_name().to_string())
            .collect();
        let submitted = self.console.render(ui, &plugin_names);
        if let Some(cmd) = submitted {
            self.execute_command(cmd);
        }
    }

    fn render_spoofer_tab(&mut self, ui: &mut egui::Ui) {
        if !self.spoofer_master && (self.spoofer_subtab != SpooferSubTab::Settings) {
            self.spoofer_subtab = SpooferSubTab::Settings;
        }

        egui::Panel::left("spoofer_settings_list")
            .resizable(false)
            .exact_size(150.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("spoofer_settings_names")
                    .show(ui, |ui| {
                        ui.selectable_value(
                            &mut self.spoofer_subtab,
                            SpooferSubTab::Settings,
                            "Spoofer Settings",
                        );
                        ui.add_enabled_ui(self.spoofer_master, |ui| {
                            ui.selectable_value(
                                &mut self.spoofer_subtab,
                                SpooferSubTab::Username,
                                "Name & Title",
                            );
                            ui.selectable_value(
                                &mut self.spoofer_subtab,
                                SpooferSubTab::TitleRank,
                                "Rank",
                            );
                            ui.selectable_value(
                                &mut self.spoofer_subtab,
                                SpooferSubTab::Friends,
                                "Friends",
                            );
                        });
                    });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("spoofer_settings_view")
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.spoofer_subtab {
                        SpooferSubTab::Settings => {
                            ui.heading("Spoofer Settings");
                            ui.add_space(8.0);

                            if ui
                                .checkbox(&mut self.spoofer_master, "Enable Spoofer")
                                .changed()
                            {
                                if self.spoofer_master && !spoofer::is_admin() {
                                    self.admin_prompt_open = true;
                                    self.spoofer_master = false;
                                } else {
                                    self.save_config();
                                    self.evaluate_proxies();
                                }
                            }
                            ui.label(
                                egui::RichText::new("Requires Admin for hosts-file interception")
                                    .color(egui::Color32::GRAY),
                            );

                            ui.add_space(10.0);
                            ui.add_enabled_ui(self.spoofer_master, |ui| {
                                ui.horizontal(|ui| {
                                    if ui
                                        .checkbox(&mut self.spoofer_http_proxy, "Account Proxy")
                                        .changed()
                                    {
                                        self.save_config();
                                        self.evaluate_proxies();
                                    }
                                    if self.spoofer_mgr.http_running() {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(34, 245, 101),
                                            "Running",
                                        );
                                    } else {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(244, 113, 116),
                                            "Stopped",
                                        );
                                    }
                                });
                                ui.label(
                                    egui::RichText::new("For Username, Friends and Rank Spoofing")
                                        .color(egui::Color32::GRAY),
                                );

                                ui.add_space(8.0);

                                ui.horizontal(|ui| {
                                    if ui
                                        .checkbox(&mut self.spoofer_socket_proxy, "PsyNet Proxy")
                                        .changed()
                                    {
                                        self.save_config();
                                        self.evaluate_proxies();
                                    }
                                    if self.spoofer_mgr.socket_running() {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(34, 245, 101),
                                            "Running",
                                        );
                                    } else {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(244, 113, 116),
                                            "Stopped",
                                        );
                                    }
                                });
                                ui.label(
                                    egui::RichText::new("For Title and Rank Spoofing")
                                        .color(egui::Color32::GRAY),
                                );
                            });

                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                ui.label("Certificate Status: ");
                                if self.spoofer_cert_installed {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(34, 245, 101),
                                        "Installed",
                                    );
                                    if ui.button("Remove").clicked() {
                                        match spoofer::ca::uninstall() {
                                            Ok(()) => {
                                                self.console
                                                    .write("[Spoofer] Certificate removed.");
                                                self.spoofer_cert_installed =
                                                    spoofer::ca::is_current_installed(
                                                        &self.base_dir,
                                                    );
                                            }
                                            Err(e) => self.console.write(format!(
                                                "[Spoofer] Certificate removal failed: {e}"
                                            )),
                                        }
                                    }
                                } else {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(244, 113, 116),
                                        "Missing",
                                    );
                                    if ui.button("Install Certificate").clicked() {
                                        match spoofer::ca::install(&self.base_dir) {
                                            Ok(()) => {
                                                self.console
                                                    .write("[Spoofer] Certificate installed.");
                                                self.spoofer_cert_installed =
                                                    spoofer::ca::is_current_installed(
                                                        &self.base_dir,
                                                    );
                                            }
                                            Err(e) => self.console.write(format!(
                                                "[Spoofer] Certificate install failed: {e}"
                                            )),
                                        }
                                    }
                                    if ui.button("↻").on_hover_text("Refresh Status").clicked() {
                                        self.spoofer_cert_installed =
                                            spoofer::ca::is_current_installed(&self.base_dir);
                                    }
                                }
                            });
                        }
                        SpooferSubTab::Username => {
                            ui.heading("Name & Title Spoofing");
                            ui.add_space(8.0);

                            ui.add_enabled_ui(self.spoofer_http_proxy, |ui| {
                                ui.horizontal(|ui| {
                                    if ui
                                        .checkbox(
                                            &mut self.spoofer_username_enabled,
                                            "Enable Username Spoof",
                                        )
                                        .changed()
                                    {
                                        if self.spoofer_username_enabled {
                                            self.remember_username_spoof();
                                        }
                                        self.save_config();
                                        self.evaluate_proxies();
                                    }
                                });
                                ui.add_space(4.0);

                                ui.horizontal(|ui| {
                                    let text_resp = ui.add_enabled(
                                        self.spoofer_username_enabled,
                                        egui::TextEdit::singleline(&mut self.spoofer_username)
                                            .desired_width(200.0),
                                    );

                                    if text_resp.changed() {
                                        if self.spoofer_username.chars().count()
                                            > spoofer::MAX_NAME_LENGTH
                                        {
                                            self.spoofer_username = self
                                                .spoofer_username
                                                .chars()
                                                .take(spoofer::MAX_NAME_LENGTH)
                                                .collect();
                                        }
                                        self.spoofer_mgr.set_username(&self.spoofer_username);
                                    }
                                    if text_resp.lost_focus() {
                                        if self.spoofer_username_enabled {
                                            self.remember_username_spoof();
                                        }
                                        self.save_config();
                                    }

                                    ui.label(format!(
                                        "{} / {}",
                                        self.spoofer_username.chars().count(),
                                        spoofer::MAX_NAME_LENGTH
                                    ));

                                    let recent_names = self.spoofer_username_history.clone();
                                    for name in recent_names {
                                        if ui
                                            .add_enabled(
                                                self.spoofer_username_enabled,
                                                egui::Button::new(&name).small(),
                                            )
                                            .on_hover_text("Use recent spoofed name")
                                            .clicked()
                                        {
                                            self.spoofer_username = name;
                                            self.spoofer_mgr.set_username(&self.spoofer_username);
                                            self.remember_username_spoof();
                                            self.save_config();
                                        }
                                    }
                                });

                                ui.add_space(12.0);

                                let mut title_changed = false;
                                ui.horizontal(|ui| {
                                    if ui
                                        .checkbox(&mut self.spoofer_title_enabled, "Title:    ")
                                        .changed()
                                    {
                                        title_changed = true;
                                        self.evaluate_proxies();
                                    }
                                    let text_resp_2 = ui.add_enabled(
                                        self.spoofer_title_enabled,
                                        egui::TextEdit::singleline(&mut self.spoofer_title)
                                            .desired_width(200.0),
                                    );
                                    if text_resp_2.changed() {
                                        if self.spoofer_title.chars().count() > 64 {
                                            self.spoofer_title =
                                                self.spoofer_title.chars().take(64).collect();
                                        }
                                        title_changed = true;
                                    }
                                    if ui
                                        .add_enabled_ui(self.spoofer_title_enabled, |ui| {
                                            ui.color_edit_button_srgb(&mut self.spoofer_title_color)
                                        })
                                        .inner
                                        .changed()
                                    {
                                        title_changed = true;
                                    }
                                    if ui
                                        .add_enabled(
                                            self.spoofer_title_enabled,
                                            egui::Checkbox::new(
                                                &mut self.spoofer_title_glow,
                                                "Glow",
                                            ),
                                        )
                                        .changed()
                                    {
                                        title_changed = true;
                                    }

                                    let selected_title = self
                                        .spoofer_title_target
                                        .as_ref()
                                        .and_then(|id| {
                                            self.title_catalog.iter().find(|title| &title.id == id)
                                        })
                                        .map(|title| {
                                            egui::RichText::new(&title.text)
                                                .color(title.game_color())
                                        })
                                        .unwrap_or_else(|| egui::RichText::new("All"));
                                    egui::ComboBox::from_id_salt("title_spoof_target")
                                        .width(210.0)
                                        .height(320.0)
                                        .selected_text(selected_title)
                                        .show_ui(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.label("Filter:");
                                                ui.add(
                                                    egui::TextEdit::singleline(
                                                        &mut self.spoofer_title_filter,
                                                    )
                                                    .hint_text("Search titles...")
                                                    .desired_width(145.0),
                                                );
                                                if ui.small_button("Clear").clicked() {
                                                    self.spoofer_title_filter.clear();
                                                }
                                            });
                                            ui.separator();
                                            if ui
                                                .selectable_value(
                                                    &mut self.spoofer_title_target,
                                                    None,
                                                    "All",
                                                )
                                                .changed()
                                            {
                                                title_changed = true;
                                            }
                                            let query = self
                                                .spoofer_title_filter
                                                .trim()
                                                .to_ascii_lowercase();
                                            for title in &self.title_catalog {
                                                if (query.is_empty()
                                                    || title
                                                        .text
                                                        .to_ascii_lowercase()
                                                        .contains(&query))
                                                    && ui
                                                        .selectable_value(
                                                            &mut self.spoofer_title_target,
                                                            Some(title.id.clone()),
                                                            egui::RichText::new(&title.text)
                                                                .color(title.game_color()),
                                                        )
                                                        .changed()
                                                {
                                                    title_changed = true;
                                                }
                                            }
                                        });
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Copy title:");
                                    let copy_label = self
                                        .spoofer_title_copy
                                        .as_ref()
                                        .and_then(|id| {
                                            self.title_catalog.iter().find(|title| &title.id == id)
                                        })
                                        .map(|title| {
                                            egui::RichText::new(&title.text)
                                                .color(title.game_color())
                                        })
                                        .unwrap_or_else(|| {
                                            egui::RichText::new("Choose a title...")
                                        });
                                    egui::ComboBox::from_id_salt("title_spoof_copy")
                                        .width(250.0)
                                        .height(320.0)
                                        .selected_text(copy_label)
                                        .show_ui(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.label("Filter:");
                                                ui.add(
                                                    egui::TextEdit::singleline(
                                                        &mut self.spoofer_title_copy_filter,
                                                    )
                                                    .hint_text("Search titles...")
                                                    .desired_width(145.0),
                                                );
                                                if ui.small_button("Clear").clicked() {
                                                    self.spoofer_title_copy_filter.clear();
                                                }
                                            });
                                            ui.separator();
                                            ui.selectable_value(
                                                &mut self.spoofer_title_copy,
                                                None,
                                                "Choose a title...",
                                            );
                                            let query = self
                                                .spoofer_title_copy_filter
                                                .trim()
                                                .to_ascii_lowercase();
                                            let mut picked = None;
                                            for title in &self.title_catalog {
                                                if query.is_empty()
                                                    || title
                                                        .text
                                                        .to_ascii_lowercase()
                                                        .contains(&query)
                                                {
                                                    if ui
                                                        .selectable_value(
                                                            &mut self.spoofer_title_copy,
                                                            Some(title.id.clone()),
                                                            egui::RichText::new(&title.text)
                                                                .color(title.game_color()),
                                                        )
                                                        .changed()
                                                    {
                                                        picked = Some(title.clone());
                                                    }
                                                }
                                            }
                                            if let Some(title) = picked {
                                                let has_glow = title.has_glow();
                                                self.spoofer_title = title.text;
                                                self.spoofer_title_color = title
                                                    .color
                                                    .as_deref()
                                                    .map(parse_hex_color)
                                                    .unwrap_or([0xE8, 0xE8, 0xE8]);
                                                self.spoofer_title_glow = has_glow;
                                                title_changed = true;
                                            }
                                        });
                                });
                                ui.horizontal(|ui| {
                                    for (label, token) in TITLE_RANK_TOKENS {
                                        if ui
                                            .add_enabled(
                                                self.spoofer_title_enabled,
                                                egui::Button::new(label),
                                            )
                                            .clicked()
                                        {
                                            if !self.spoofer_title.is_empty()
                                                && !self.spoofer_title.ends_with(' ')
                                            {
                                                self.spoofer_title.push(' ');
                                            }
                                            self.spoofer_title.push_str(token);
                                            title_changed = true;
                                        }
                                    }
                                    if ui
                                        .add_enabled(
                                            self.spoofer_title_enabled,
                                            egui::Button::new("Hebnix"),
                                        )
                                        .clicked()
                                    {
                                        self.spoofer_title = "Hebnix".to_string();
                                        title_changed = true;
                                    }
                                });
                                if title_changed {
                                    self.spoofer_mgr.set_title(&self.spoofer_title);
                                    self.spoofer_mgr
                                        .set_title_enabled(self.spoofer_title_enabled);
                                    self.spoofer_mgr.set_title_options(
                                        format!(
                                            "{:02X}{:02X}{:02X}",
                                            self.spoofer_title_color[0],
                                            self.spoofer_title_color[1],
                                            self.spoofer_title_color[2]
                                        ),
                                        self.spoofer_title_glow,
                                        self.spoofer_title_target.clone(),
                                    );
                                    self.save_config();
                                }
                            });
                        }
                        SpooferSubTab::Friends => {
                            ui.heading("Friend Spoofing");
                            ui.add_space(8.0);

                            ui.add_enabled_ui(self.spoofer_http_proxy, |ui| {
                                ui.horizontal(|ui| {
                                    if ui
                                        .checkbox(
                                            &mut self.spoofer_friends_enabled,
                                            "Enable Friend Name Spoof",
                                        )
                                        .changed()
                                    {
                                        self.save_friends();
                                        self.evaluate_proxies();
                                    }
                                });
                                ui.add_space(8.0);

                                ui.horizontal(|ui| {
                                    ui.label("Search:");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.friends_search)
                                            .desired_width(150.0),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.button("Revert All").clicked() {
                                                for state in self.spoofer_friends.values_mut() {
                                                    state.enabled = false;
                                                    state.spoofed_name.clear();
                                                }
                                                self.save_friends();
                                            }
                                        },
                                    );
                                });
                                ui.add_space(8.0);

                                let discovered =
                                    self.spoofer_mgr.discovered_friends.lock().unwrap().clone();

                                let mut changed = false;
                                for (acc_id, orig_name) in &discovered {
                                    let entry = self
                                        .spoofer_friends
                                        .entry(acc_id.clone())
                                        .or_insert_with(|| {
                                            changed = true;
                                            FriendSpoofState {
                                                enabled: false,
                                                original_name: orig_name.clone(),
                                                spoofed_name: String::new(),
                                            }
                                        });
                                    if entry.original_name != *orig_name {
                                        entry.original_name = orig_name.clone();
                                        changed = true;
                                    }
                                }
                                if changed {
                                    self.save_friends();
                                }

                                let mut friends_vec: Vec<_> =
                                    self.spoofer_friends.iter_mut().collect();
                                friends_vec.sort_by(|a, b| {
                                    a.1.original_name
                                        .to_lowercase()
                                        .cmp(&b.1.original_name.to_lowercase())
                                });

                                let query = self.friends_search.to_lowercase();
                                friends_vec.retain(|(_, state)| {
                                    query.is_empty()
                                        || state.original_name.to_lowercase().contains(&query)
                                        || state.spoofed_name.to_lowercase().contains(&query)
                                });

                                if discovered.is_empty() && friends_vec.is_empty() {
                                    ui.label(
                                        egui::RichText::new(
                                            "Start Rocket League to populate friends list",
                                        )
                                        .color(egui::Color32::GRAY),
                                    );
                                } else {
                                    let mut any_interaction = false;

                                    egui::ScrollArea::vertical()
                                        .id_salt("friends_scroll")
                                        .auto_shrink([false, false])
                                        .show(ui, |ui| {
                                            for (_id, state) in friends_vec {
                                                ui.horizontal(|ui| {
                                                    if ui.checkbox(&mut state.enabled, "").changed()
                                                    {
                                                        any_interaction = true;
                                                    }

                                                    let mut orig_name_display =
                                                        state.original_name.clone();
                                                    ui.add_sized(
                                                        [150.0, 20.0],
                                                        egui::TextEdit::singleline(
                                                            &mut orig_name_display,
                                                        )
                                                        .interactive(false),
                                                    );

                                                    let text_resp = ui.add_enabled(
                                                        state.enabled,
                                                        egui::TextEdit::singleline(
                                                            &mut state.spoofed_name,
                                                        )
                                                        .hint_text("Spoofed Name"),
                                                    );
                                                    if text_resp.changed() {
                                                        any_interaction = true;
                                                    }
                                                });
                                            }
                                        });

                                    if any_interaction {
                                        self.save_friends();
                                    }
                                }
                            });
                        }
                        SpooferSubTab::TitleRank => {
                            ui.heading("Rank Spoofing");
                            ui.add_space(8.0);

                            ui.add_enabled_ui(self.spoofer_http_proxy, |ui| {
                                if ui
                                    .checkbox(
                                        &mut self.spoofer_rank_enabled,
                                        "Enable Rank Spoofing",
                                    )
                                    .changed()
                                {
                                    self.save_ranks();
                                    self.evaluate_proxies();
                                }

                                ui.add_space(8.0);

                                ui.add_enabled_ui(self.spoofer_rank_enabled, |ui| {
                                    let mut changed = false;

                                    egui::ScrollArea::vertical().id_salt("ranks_scroll").show(
                                        ui,
                                        |ui| {
                                            for &(id, name) in RANK_SPOOF_PLAYLISTS {
                                                let state = self.spoofer_ranks.entry(id).or_insert(
                                                    RankSpoofState {
                                                        enabled: false,
                                                        rank: 0,
                                                        mmr: String::new(),
                                                    },
                                                );

                                                ui.horizontal(|ui| {
                                                    ui.add_sized(
                                                        [110.0, 20.0],
                                                        |ui: &mut egui::Ui| {
                                                            let r = ui
                                                                .checkbox(&mut state.enabled, name);
                                                            if r.changed() {
                                                                changed = true;
                                                            }
                                                            r
                                                        },
                                                    );

                                                    ui.add_enabled_ui(state.enabled, |ui| {
                                                        let combo = egui::ComboBox::from_id_salt(
                                                            format!("rank_combo_{id}"),
                                                        )
                                                        .selected_text(rank_name(state.rank))
                                                        .width(160.0)
                                                        .show_ui(ui, |ui| {
                                                            let mut local_changed = false;
                                                            for r in 0..=22 {
                                                                if ui
                                                                    .selectable_value(
                                                                        &mut state.rank,
                                                                        r,
                                                                        rank_name(r),
                                                                    )
                                                                    .changed()
                                                                {
                                                                    local_changed = true;
                                                                }
                                                            }
                                                            local_changed
                                                        });

                                                        if combo.inner.unwrap_or(false) {
                                                            changed = true;
                                                        }

                                                        let mmr_resp = ui.add(
                                                            egui::TextEdit::singleline(
                                                                &mut state.mmr,
                                                            )
                                                            .desired_width(60.0)
                                                            .hint_text("MMR"),
                                                        );
                                                        if mmr_resp.changed() {
                                                            changed = true;
                                                        }
                                                    });
                                                });
                                                ui.add_space(4.0);
                                            }
                                        },
                                    );

                                    if changed {
                                        self.save_ranks();
                                        self.evaluate_proxies();
                                    }
                                });
                            });
                        }
                    });
            });
    }

    /// topmost first. hidden with fewer than two layers.
    fn render_overlay_order(&mut self, ui: &mut egui::Ui) {
        let layers = self.plugin_mgr.overlay_layers(&self.config.overlay_order);
        if layers.len() < 2 {
            return;
        }
        let draws = self.plugin_mgr.overlay_plugins();

        // top of screen first, the stack is stored bottom first
        let mut rows: Vec<(String, String, &'static str)> = layers
            .iter()
            .rev()
            .map(|(slug, page, _, _)| {
                let name = self
                    .plugin_mgr
                    .plugins
                    .iter()
                    .find(|p| &p.slug == slug)
                    .map(|p| p.display_name().to_string())
                    .unwrap_or_else(|| slug.clone());
                let kind = match (page.is_some(), draws.contains(slug)) {
                    (true, true) => "page + draw",
                    (true, false) => "page",
                    _ => "draw",
                };
                (slug.clone(), name, kind)
            })
            .collect();

        let mut moved: Option<(usize, usize)> = None;
        egui::CollapsingHeader::new(format!("Overlay order ({} layers)", rows.len()))
            .id_salt("overlay_order")
            .show(ui, |ui| {
                ui.label("Drag to reorder. Renders based on number order.");
                ui.add_space(4.0);
                for (index, (slug, name, kind)) in rows.iter().enumerate() {
                    let row_id = egui::Id::new(("overlay_layer", slug));
                    let (_, dropped) = ui.dnd_drop_zone::<usize, _>(
                        egui::Frame::new().inner_margin(egui::Margin::symmetric(4, 2)),
                        |ui| {
                            ui.dnd_drag_source(row_id, index, |ui| {
                                ui.horizontal(|ui| {
                                    // ascii, a user font can tofu the rest
                                    ui.weak(format!("{}.", index + 1));
                                    ui.label(name);
                                    ui.weak(format!("({kind})"));
                                });
                            });
                        },
                    );
                    if let Some(from) = dropped {
                        moved = Some((*from, index));
                    }
                }
            });

        if let Some((from, to)) = moved
            && from != to
            && from < rows.len()
        {
            let row = rows.remove(from);
            rows.insert(to.min(rows.len()), row);
            // config holds bottom first, the list is top first
            self.config.overlay_order =
                rows.iter().rev().map(|(slug, _, _)| slug.clone()).collect();
            self.save_config();
        }
    }

    fn render_plugins_tab(&mut self, ui: &mut egui::Ui) {
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
                if ui
                    .add(
                        egui::Button::new("Reload").fill(egui::Color32::from_rgb(0xd3, 0x54, 0x00)),
                    )
                    .clicked()
                {
                    self.plugin_mgr.reload_all(&mut self.config);
                    self.save_config();
                }
            });
        });
        ui.add_space(6.0);
        self.render_overlay_order(ui);

        let mut toggles: Vec<(String, bool)> = Vec::new();
        let mut open_settings: Option<String> = None;

        egui::ScrollArea::vertical()
            .id_salt("plugins_list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for plugin in &self.plugin_mgr.plugins {
                    if let Some(err) = &plugin.load_error {
                        ui.horizontal(|ui| {
                            ui.add_enabled(false, egui::Checkbox::new(&mut false, &plugin.slug));
                            ui.colored_label(
                                egui::Color32::from_rgb(0xe7, 0x4c, 0x3c),
                                err.clone(),
                            );
                        });
                        ui.add_space(2.0);
                        continue;
                    }
                    let mut enabled = plugin.enabled;
                    egui::Frame::group(ui.style())
                        .inner_margin(egui::Margin::same(6))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let text = format!(
                                    "{} v{} by {} ({})",
                                    plugin.display_name(),
                                    plugin.manifest.version,
                                    plugin.manifest.author,
                                    plugin.filename
                                );
                                if ui.checkbox(&mut enabled, text).changed() {
                                    toggles.push((plugin.slug.clone(), enabled));
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let has_settings = plugin.has_settings();
                                        if ui
                                            .add_enabled(
                                                has_settings && plugin.enabled,
                                                egui::Button::new("⚙"),
                                            )
                                            .clicked()
                                        {
                                            open_settings = Some(plugin.slug.clone());
                                        }
                                    },
                                );
                            });
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

        for (slug, enabled) in toggles {
            let display = self
                .plugin_mgr
                .plugins
                .iter()
                .find(|p| p.slug == slug)
                .map(|p| p.display_name().to_string())
                .unwrap_or_else(|| slug.clone());
            if enabled {
                if self.plugin_mgr.set_enabled(&slug, true, &mut self.config) {
                    self.console.write(format!(
                        "[Console] {display} has been enabled and reloaded."
                    ));
                } else {
                    self.console.write(format!(
                        "[Console] {display} failed to load (syntax/import error). Disabled."
                    ));
                }
            } else {
                self.plugin_mgr.set_enabled(&slug, false, &mut self.config);
                self.console.write(format!(
                    "[Console] {display} has been disabled and unloaded."
                ));
            }
            self.save_config();
        }

        if let Some(slug) = open_settings {
            self.selected_settings_plugin = Some(slug);
            self.tab = Tab::Settings;
            self.settings_subtab = SettingsSubTab::Plugin;
        }
    }

    fn render_settings_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.settings_subtab,
                SettingsSubTab::Hebnix,
                "Hebnix Settings",
            );
            ui.selectable_value(
                &mut self.settings_subtab,
                SettingsSubTab::Plugin,
                "Plugin Settings",
            );
        });
        ui.separator();

        match self.settings_subtab {
            SettingsSubTab::Hebnix => self.render_hebnix_settings(ui),
            SettingsSubTab::Plugin => self.render_plugin_settings(ui),
        }
    }

    fn render_hebnix_settings(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        egui::Panel::left("hebnix_settings_list")
            .resizable(false)
            .exact_size(200.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("hebnix_settings_names")
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
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("hebnix_settings_view")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                    ui.heading(match self.hebnix_settings_tab {
                        HebnixSettingsTab::Interface => "Interface Configuration",
                        HebnixSettingsTab::Directories => "Directories & Files Configuration",
                        HebnixSettingsTab::System => "System Configuration",
                    });
                    ui.add_space(8.0);

                    match self.hebnix_settings_tab {
                        HebnixSettingsTab::Interface => {
                            ui.horizontal(|ui| {
                                ui.add_sized([130.0, 20.0], egui::Label::new("Open/Close Keybind:"));
                                let display = if self.capturing_hotkey {
                                    "...".to_string()
                                } else {
                                    self.config.settings.hotkey.to_uppercase()
                                };
                                ui.add_enabled(
                                    false,
                                    egui::TextEdit::singleline(&mut display.clone()).desired_width(120.0),
                                );
                                let btn_text = if self.capturing_hotkey {
                                    "Listening..."
                                } else {
                                    "Set Keybind"
                                };
                                if ui
                                    .add_enabled(!self.capturing_hotkey, egui::Button::new(btn_text))
                                    .clicked()
                                {
                                    self.start_hotkey_capture(&ctx);
                                }
                            });

                            ui.horizontal(|ui| {
                                ui.add_sized([130.0, 20.0], egui::Label::new("Theme:"));
                                let mut selected = self.config.settings.theme.clone();
                                let mut changed = false;
                                egui::ComboBox::from_id_salt("theme_select")
                                    .selected_text(selected.clone())
                                    .width(120.0)
                                    .show_ui(ui, |ui| {
                                        for option in self.theme_options.clone() {
                                            if ui
                                                .selectable_value(&mut selected, option.clone(), option)
                                                .changed()
                                            {
                                                changed = true;
                                            }
                                        }
                                    });
                                if changed {
                                    self.change_theme(&ctx, &selected);
                                }
                                if ui.button("Refresh").clicked() {
                                    self.theme_options = theme::list_themes(&self.themes_dir);
                                    self.console
                                        .write("[Console] Themes directory rescanned and updated.");
                                }
                                if ui.button("Open Folder").clicked() {
                                    let _ = open::that(&self.themes_dir);
                                }
                                if ui.button("Open Fonts Folder").clicked() {
                                    let _ = open::that(&self.fonts_dir);
                                }
                            });

                            ui.horizontal(|ui| {
                                ui.add_sized([130.0, 20.0], egui::Label::new("Window Opacity:"));
                                let slider = ui.add(
                                    egui::Slider::new(
                                        &mut self.config.settings.window_opacity,
                                        0.5..=1.0,
                                    )
                                    .fixed_decimals(2),
                                );
                                if slider.changed() {
                                    let theme_name = self.config.settings.theme.clone();
                                    let _ = theme::apply_theme(
                                        &ctx,
                                        &self.themes_dir,
                                        &self.fonts_dir,
                                        &theme_name,
                                    );
                                    theme::apply_window_opacity(
                                        &ctx,
                                        self.config.settings.window_opacity,
                                    );
                                }
                                if slider.drag_stopped() || slider.lost_focus() {
                                    self.save_config();
                                }
                            });
                        }

                        HebnixSettingsTab::Directories => {
                            ui.label(
                                egui::RichText::new("(auto-detected from the running game)")
                                    .size(11.0)
                                    .color(egui::Color32::GRAY),
                            );
                            ui.add_space(8.0);

                            ui.horizontal(|ui| {
                                ui.add_sized([130.0, 20.0], egui::Label::new("Rocket League Folder:"));
                                ui.add_enabled(
                                    false,
                                    egui::TextEdit::singleline(&mut self.config.settings.rl_path)
                                        .desired_width(480.0),
                                );
                                if ui.button("Browse").clicked() {
                                    if let Some(dir) = rfd::FileDialog::new()
                                        .set_directory(&self.config.settings.rl_path)
                                        .pick_folder()
                                    {
                                        self.config.settings.rl_path = dir.to_string_lossy().to_string();
                                        self.remember_patcher_state(&self.config.settings.rl_path);
                                        self.save_config();
                                        self.refresh_stats_api_viewer();
                                    }
                                }
                            });

                            ui.horizontal(|ui| {
                                ui.add_sized([130.0, 20.0], egui::Label::new("DefaultStatsAPI.ini:"));
                                ui.add_enabled(
                                    false,
                                    egui::TextEdit::singleline(&mut self.config.settings.statsapi_path)
                                        .desired_width(480.0),
                                );
                                if ui.button("Browse").clicked() {
                                    let start_dir = std::path::Path::new(&self.config.settings.statsapi_path)
                                        .parent()
                                        .map(|p| p.to_path_buf())
                                        .unwrap_or_default();
                                    if let Some(file) = rfd::FileDialog::new()
                                        .set_directory(start_dir)
                                        .add_filter("INI files", &["ini"])
                                        .pick_file()
                                    {
                                        self.config.settings.statsapi_path =
                                            file.to_string_lossy().to_string();
                                        self.save_config();
                                        self.refresh_stats_api_viewer();
                                    }
                                }
                            });

                            ui.horizontal(|ui| {
                                ui.add_sized([130.0, 20.0], egui::Label::new("PacketSendRate:"));
                                let resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.packet_rate_edit)
                                        .hint_text("Not Found")
                                        .desired_width(100.0),
                                );
                                if resp.changed() {
                                    self.packet_rate_edit.retain(|c| c.is_ascii_digit());
                                }
                                if resp.lost_focus() {
                                    let current = self.packet_rate.clone().unwrap_or_default();
                                    if !self.packet_rate_edit.is_empty() && self.packet_rate_edit != current {
                                        let value = self.packet_rate_edit.clone();
                                        self.update_ini_setting("PacketSendRate", &value);
                                    } else {
                                        self.packet_rate_edit = current;
                                    }
                                }
                                if self.packet_rate.as_deref() != Some("20")
                                    && ui.add(egui::Button::new("Set 20").fill(egui::Color32::from_rgb(0xd3, 0x54, 0x00))).clicked()
                                {
                                    self.update_ini_setting("PacketSendRate", "20");
                                }
                            });

                            ui.horizontal(|ui| {
                                ui.add_sized([130.0, 20.0], egui::Label::new("Port:"));
                                let resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.port_edit)
                                        .hint_text("Not Found")
                                        .desired_width(100.0),
                                );
                                if resp.changed() {
                                    self.port_edit.retain(|c| c.is_ascii_digit());
                                }
                                if resp.lost_focus() {
                                    let current = self.port_value.clone().unwrap_or_default();
                                    let valid = self.port_edit.parse::<u16>().map(|p| p > 0).unwrap_or(false);
                                    if valid && self.port_edit != current {
                                        let value = self.port_edit.clone();
                                        self.update_ini_setting("Port", &value);
                                    } else {
                                        self.port_edit = current;
                                    }
                                }
                                if self.port_value.as_deref() != Some("49123")
                                    && ui.add(egui::Button::new("Reset").fill(egui::Color32::from_rgb(0xc0, 0x39, 0x2b))).clicked()
                                {
                                    self.update_ini_setting("Port", "49123");
                                }
                                if ui.button("Refresh").clicked() {
                                    self.refresh_stats_api_viewer();
                                }
                            });

                            ui.horizontal(|ui| {
                                ui.add_sized([130.0, 20.0], egui::Label::new("WebPort:"));
                                let resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.web_port_edit)
                                        .hint_text("Not Found")
                                        .desired_width(100.0),
                                );
                                if resp.changed() {
                                    self.web_port_edit.retain(|c| c.is_ascii_digit());
                                }
                                if resp.lost_focus() {
                                    let current = self.web_port_value.clone().unwrap_or_default();
                                    let valid = self.web_port_edit.parse::<u16>().map(|p| p > 0).unwrap_or(false);
                                    if valid && self.web_port_edit != current {
                                        let value = self.web_port_edit.clone();
                                        self.update_ini_setting("WebPort", &value);
                                    } else {
                                        self.web_port_edit = current;
                                    }
                                }
                                if self.web_port_value.as_deref() != Some("49124")
                                    && ui.add(egui::Button::new("Set 49124").fill(egui::Color32::from_rgb(0xd3, 0x54, 0x00))).clicked()
                                {
                                    self.update_ini_setting("WebPort", "49124");
                                }
                            });

                            ui.label(
                                egui::RichText::new("Changes to the ini apply after restarting Rocket League.")
                                    .size(11.0)
                                    .color(egui::Color32::GRAY),
                            );
                        }

                        HebnixSettingsTab::System => {
                            ui.horizontal(|ui| {
                                ui.add_sized([130.0, 20.0], egui::Label::new("Start with Windows:"));
                                if ui.checkbox(&mut self.startup_enabled, "").changed() {
                                    if let Err(e) = winutil::set_startup_enabled(self.startup_enabled) {
                                        self.console.write(format!("[Console] Failed to update startup entry: {e}"));
                                        self.startup_enabled = winutil::is_startup_enabled();
                                    }
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.add_sized([130.0, 20.0], egui::Label::new("Start in Tray:"));
                                if ui.checkbox(&mut self.config.settings.start_in_tray, "").changed() {
                                    self.save_config();
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.add_sized([130.0, 20.0], egui::Label::new("Suppress Left Alerts:"));
                                if ui.checkbox(&mut self.config.settings.suppress_left_alerts, "").changed() {
                                    self.save_config();
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.add_sized([130.0, 20.0], egui::Label::new("Fullscreen Warning:"));
                                let mut show = !self.config.settings.suppress_fullscreen_warning;
                                if ui.checkbox(&mut show, "").changed() {
                                    self.config.settings.suppress_fullscreen_warning = !show;
                                    self.fullscreen_notice_dismissed = false;
                                    self.fullscreen_notice =
                                        show && self.window_mode == Some(WindowMode::Fullscreen);
                                    self.save_config();
                                }
                            });
                            ui.label(
                                egui::RichText::new("Warns when the game is fullscreen and the overlay can't draw.")
                                    .size(11.0)
                                    .color(egui::Color32::GRAY),
                            );
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    [130.0, 20.0],
                                    egui::Label::new("Allow Draw on Hebnix Focus:"),
                                );
                                if ui
                                    .checkbox(
                                        &mut self.config.settings.allow_draw_on_hebnix_focus,
                                        "",
                                    )
                                    .changed()
                                {
                                    self.save_config();
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    [130.0, 20.0],
                                    egui::Label::new("Limit Hotkey to Hebnix/Rocket League:"),
                                );
                                if ui
                                    .checkbox(
                                        &mut self.config.settings.restrict_hotkey_to_hebnix_or_rocket_league,
                                        "",
                                    )
                                    .changed()
                                {
                                    self.save_config();
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.add_sized([130.0, 20.0], egui::Label::new("StatsAPI Rate Warning:"));
                                let mut show = !self.config.settings.suppress_statsapi_rate_warning;
                                if ui.checkbox(&mut show, "").changed() {
                                    self.config.settings.suppress_statsapi_rate_warning = !show;
                                    self.save_config();
                                }
                            });
                            ui.label(
                                egui::RichText::new("Warns when PacketSendRate isn't 20. Rates of 10 and under always warn.")
                                    .size(11.0)
                                    .color(egui::Color32::GRAY),
                            );
                        }
                    }
                });
        });
    }

    fn change_theme(&mut self, ctx: &egui::Context, choice: &str) {
        let error_file = self.base_dir.join("theme_errors.txt");
        match theme::apply_theme(ctx, &self.themes_dir, &self.fonts_dir, choice) {
            Ok(()) => {
                let _ = std::fs::remove_file(&error_file);
                self.config.settings.theme = choice.to_string();
                self.save_config();
                self.console
                    .write(format!("[Console] Theme changed to '{choice}'."));
            }
            Err(e) => {
                let _ = std::fs::write(
                    &error_file,
                    format!("Runtime Theme Error ({choice}): {e}\n"),
                );
                let _ = theme::apply_theme(ctx, &self.themes_dir, &self.fonts_dir, "Dark");
                self.config.settings.theme = "Dark".to_string();
                self.save_config();
                self.console.write(format!(
                    "[Console] Theme '{choice}' failed to load. Defaulted to Dark mode. Check theme_errors.txt"
                ));
            }
        }
        theme::apply_window_opacity(ctx, self.config.settings.window_opacity);
    }

    fn render_plugin_settings(&mut self, ui: &mut egui::Ui) {
        let with_settings: Vec<(String, String)> = self
            .plugin_mgr
            .plugins
            .iter()
            .filter(|p| p.enabled && p.has_settings())
            .map(|p| (p.slug.clone(), p.display_name().to_string()))
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
            .map(|s| with_settings.iter().any(|(slug, _)| slug == s))
            .unwrap_or(false);
        if !selected_valid {
            self.selected_settings_plugin = Some(with_settings[0].0.clone());
        }
        let selected = self.selected_settings_plugin.clone().unwrap();

        egui::Panel::left("plugin_settings_list")
            .resizable(false)
            .exact_size(200.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("plugin_settings_names")
                    .show(ui, |ui| {
                        for (slug, name) in &with_settings {
                            if ui.selectable_label(*slug == selected, name).clicked() {
                                self.selected_settings_plugin = Some(slug.clone());
                            }
                        }
                    });
            });

        let display_name = with_settings
            .iter()
            .find(|(slug, _)| *slug == selected)
            .map(|(_, name)| name.clone())
            .unwrap_or_default();

        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("plugin_settings_view")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.heading(format!("{display_name} Configuration"));
                        ui.add_space(8.0);
                        if let Err(e) = self.plugin_mgr.render_settings(&selected, ui) {
                            self.console.write(format!(
                                "[Console] Error rendering settings for {display_name}: {e}"
                            ));
                        }
                    });
            });
    }

    fn render_about_tab(&mut self, ui: &mut egui::Ui) {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.heading("Hebnix");
            ui.add_space(10.0);
            ui.label(format!(
                "Version {APP_VERSION}\n\nA safe, EAC-compliant Mod Loader for Rocket League with integrated Spoofer & Item Changer.\n\nhebnix.com\n\nBuilt by Hebbins & nixvio64.\n\nPress {} to show/hide window.",
                self.config.settings.hotkey.to_uppercase()
            ));
        });
    }

    fn render_statsapi_notice(&mut self, ctx: &egui::Context) {
        let Some(notice) = self.statsapi_notice else {
            return;
        };
        let blocking = notice.blocking();
        let body = match notice {
            StatsApiNotice::NotConfigured => {
                "Rocket League's StatsAPI is not configured, so Hebnix \
                 can't receive any match data.\nPacketSendRate has to be set to 20."
                    .to_string()
            }
            StatsApiNotice::TooLow(n) => format!(
                "PacketSendRate is {n}, too low for Hebnix to work properly.\nIt has to be set to 20."
            ),
            StatsApiNotice::BelowTarget(n) => format!(
                "PacketSendRate is {n}. The recommended value is 20, lower rates make plugin data \
                 update slower than intended."
            ),
            StatsApiNotice::AboveTarget(n) => format!(
                "PacketSendRate is {n}. The recommended value is 20, higher rates send more \
                 packets than Hebnix needs."
            ),
        };

        let mut apply = false;
        let mut dismiss = false;
        egui::Window::new("StatsAPI configuration")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(body);
                if let Some(err) = &self.statsapi_apply_error {
                    ui.add_space(6.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(0xe7, 0x4c, 0x3c),
                        format!("Couldn't write the file:\n{err}"),
                    );
                    ui.label(
                        egui::RichText::new("Edit it by hand, or restart Hebnix as administrator.")
                            .size(11.0)
                            .color(egui::Color32::GRAY),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Set 20").clicked() {
                        apply = true;
                    }
                    let dismiss_label = if blocking { "Quit Hebnix" } else { "Proceed" };
                    if ui.button(dismiss_label).clicked() {
                        dismiss = true;
                    }
                });
                if !blocking {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("Turn this off in Settings > System.")
                            .size(11.0)
                            .color(egui::Color32::GRAY),
                    );
                }
            });

        if apply {
            self.apply_statsapi_rate();
        } else if dismiss {
            if blocking {
                self.force_quit(ctx);
            } else {
                self.statsapi_notice = None;
            }
        }
    }

    fn render_web_port_notice(&mut self, ctx: &egui::Context) {
        let Some(notice) = self.web_port_notice else {
            return;
        };

        let body = match notice {
            WebPortNotice::NotConfigured => {
                "Rocket League's StatsAPI WebPort is not configured in DefaultStatsAPI.ini.\n\
                 It should be set to 49124."
                    .to_string()
            }
            WebPortNotice::InvalidPort(p) => {
                format!("WebPort is set to {p}. The standard port is 49124.")
            }
        };

        let mut apply = false;
        let mut dismiss = false;

        egui::Window::new("WebPort configuration")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(body);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Set 49124").clicked() {
                        apply = true;
                    }
                    if ui.button("Dismiss").clicked() {
                        dismiss = true;
                    }
                });
            });

        if apply {
            self.apply_web_port_setting();
        } else if dismiss {
            self.web_port_notice = None;
        }
    }

    fn render_admin_prompt(&mut self, ctx: &egui::Context) {
        if !self.admin_prompt_open {
            return;
        }
        let mut ok = false;
        let mut cancel = false;

        egui::Window::new("Administrator required")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(if self.owned_admin_requested {
                    "Filtering replacements by ownership needs the Hebnix proxy.\n\
                     Hebnix will restart as Administrator, enable the proxy, and build your owned-item catalog."
                } else {
                    "This action requires Hebnix to be run as Administrator."
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        ok = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if ok {
            self.admin_prompt_open = false;
            self.spoofer_master = true;
            if self.owned_admin_requested {
                let _ = std::fs::write(
                    self.base_dir.join("enable_owned_replacements.pending"),
                    b"1",
                );
            }
            self.save_config();
            if spoofer::spawn_elevated_relaunch() {
                self.spoofer_mgr.shutdown();
                std::process::exit(0);
            }
            self.spoofer_master = false;
            self.save_config();
            self.console.write("[Spoofer] Couldn't relaunch as admin.");
        } else if cancel {
            self.admin_prompt_open = false;
            self.owned_admin_requested = false;
        }
    }

    fn render_owned_proxy_prompt(&mut self, ctx: &egui::Context) {
        if !self.owned_proxy_prompt_open {
            return;
        }
        let mut enable = false;
        let mut cancel = false;
        egui::Window::new("Enable owned-item catalog")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    "Hebnix needs its local proxy to read your Rocket League inventory.\n\
                     The response is observed locally and is not modified or uploaded.",
                );
                if !self.spoofer_cert_installed {
                    ui.add_space(4.0);
                    ui.label("The Hebnix proxy certificate will also be installed.");
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Enable Proxy").clicked() {
                        enable = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if enable {
            self.owned_proxy_prompt_open = false;
            if !self.spoofer_cert_installed {
                match spoofer::ca::install(&self.base_dir) {
                    Ok(()) => {
                        self.spoofer_cert_installed =
                            spoofer::ca::is_current_installed(&self.base_dir);
                    }
                    Err(error) => {
                        self.console.write(format!(
                            "[Swapper] Could not install proxy certificate: {error}"
                        ));
                        return;
                    }
                }
            }
            self.spoofer_master = true;
            self.spoofer_http_proxy = true;
            self.swapper.set_owned_only(true);
            self.save_config();
            self.evaluate_proxies();
            self.console.write(
                "[Swapper] Owned-item capture enabled. Open or restart Rocket League to refresh the catalog.",
            );
        } else if cancel {
            self.owned_proxy_prompt_open = false;
        }
    }

    fn render_fullscreen_notice(&mut self, ctx: &egui::Context) {
        if !self.fullscreen_notice || self.statsapi_notice.is_some() {
            return;
        }
        let mut close = false;
        let mut never = self.config.settings.suppress_fullscreen_warning;

        egui::Window::new("Overlay unavailable")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    "Rocket League is set to Fullscreen, so the overlay won't draw over it.\n\
                     Switch the game's video settings to Borderless or Windowed.",
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut never, "Never show again").changed() {
                        self.config.settings.suppress_fullscreen_warning = never;
                        self.save_config();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    });
                });
            });

        if close {
            self.fullscreen_notice = false;
            self.fullscreen_notice_dismissed = true;
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
                ui.heading(format!("Hebnix v{} is required", info.version));
                ui.label("Hebnix is locked until the required update is installed.");
                ui.add_space(8.0);

                if let Some(err) = &self.update_error {
                    ui.colored_label(egui::Color32::from_rgb(0xe7, 0x4c, 0x3c), err);
                    ui.add_space(8.0);
                }

                if self.update_downloading {
                    ui.add_enabled(false, egui::Button::new("Downloading & Installing..."));
                    ui.spinner();
                } else if ui
                    .add(
                        egui::Button::new("Update Hebnix")
                            .fill(egui::Color32::from_rgb(0x2e, 0xcc, 0x71)),
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
        let mut close_requested = false;

        let window = if self.install_modal.hebnix_stage {
            egui::Window::new("Install Plugin")
                .id(egui::Id::new("install_plugin_catalog"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .fixed_size([900.0, 550.0])
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        } else {
            egui::Window::new("Install Plugin")
                .id(egui::Id::new("install_plugin_prompt"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        };

        window.show(ctx, |ui| {
            if !self.install_modal.hebnix_stage {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_sized(
                            [160.0, 120.0],
                            egui::Button::new("☁\n\nInstall from Hebnix"),
                        )
                        .clicked()
                    {
                        self.install_modal.hebnix_stage = true;
                        self.fetch_hebnix_plugin();
                    }
                    if ui
                        .add_sized([160.0, 120.0], egui::Button::new("📁\n\nInstall from .ZIP"))
                        .clicked()
                    {
                        let dialog = rfd::FileDialog::new().add_filter("Plugin Archives", &["zip"]);
                        if let Some(file) = winutil::parent_file_dialog(dialog).pick_file() {
                            match install_zip(&file, &self.plugin_dir) {
                                Ok(()) => {
                                    self.console.write(format!(
                                        "[Console] Extracted contents of {} into /plugins/",
                                        file.file_name()
                                            .map(|f| f.to_string_lossy().to_string())
                                            .unwrap_or_default()
                                    ));
                                    self.plugin_mgr.refresh(&mut self.config, true);
                                    self.save_config();
                                    close_requested = true;
                                }
                                Err(e) => {
                                    self.console.write(format!(
                                        "[Console] Failed to install plugin from zip: {e}"
                                    ));
                                }
                            }
                        }
                    }
                });
                ui.add_space(10.0);
            } else {
                self.render_hebnix_install(ui, &mut close_requested);
            }
        });

        if !open || close_requested {
            self.install_modal = InstallModal::default();
        }
    }

    fn render_hebnix_install(&mut self, ui: &mut egui::Ui, _close_requested: &mut bool) {
        let ctx = ui.ctx().clone();

        ui.horizontal(|ui| {
            if ui.button("< Back").clicked() {
                self.install_modal.hebnix_stage = false;
                self.install_modal.catalog.clear();
                self.install_modal.error = None;
                self.install_modal.search_query.clear();
                self.install_modal.current_page = 0;
            }
            if ui.button("Refresh").clicked() {
                self.fetch_hebnix_plugin();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let search_resp = ui.add(
                    egui::TextEdit::singleline(&mut self.install_modal.search_query)
                        .hint_text("🔍 Search plugins...")
                        .desired_width(200.0),
                );
                if search_resp.changed() {
                    self.install_modal.current_page = 0;
                }
            });
        });
        ui.add_space(8.0);

        if self.install_modal.fetching {
            ui.vertical_centered(|ui| {
                ui.add_space(20.0);
                ui.spinner();
                ui.add_space(8.0);
                ui.label("Fetching plugins...");
            });
            return;
        }

        if let Some(err) = &self.install_modal.error {
            ui.colored_label(egui::Color32::from_rgb(0xe7, 0x4c, 0x3c), err.clone());
            return;
        }

        let items_per_page = 10;
        let (total_items, total_pages, current_page_items) = {
            let query = self.install_modal.search_query.to_lowercase();
            let filtered: Vec<&Value> = self
                .install_modal
                .catalog
                .iter()
                .filter(|p| {
                    if query.is_empty() {
                        return true;
                    }
                    let name = p
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_lowercase();
                    let author = p
                        .get("author")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_lowercase();
                    let desc = p
                        .get("short_description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_lowercase();
                    name.contains(&query) || author.contains(&query) || desc.contains(&query)
                })
                .collect();

            let total_items = filtered.len();
            let total_pages = (total_items + items_per_page - 1) / items_per_page;

            if self.install_modal.current_page >= total_pages && total_pages > 0 {
                self.install_modal.current_page = total_pages - 1;
            }

            let start_idx = self.install_modal.current_page * items_per_page;
            let page: Vec<Value> = filtered
                .into_iter()
                .skip(start_idx)
                .take(items_per_page)
                .cloned()
                .collect();
            (total_items, total_pages, page)
        };

        for p in &current_page_items {
            let banner = p
                .get("banner_path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            self.ensure_plugin_image(&banner, &ctx);
        }

        let mut action = None;
        enum ModalAction {
            Enable(String),
            Disable(String),
            Download(String),
        }

        egui::ScrollArea::vertical()
            .id_salt("plugin_catalog_scroll")
            .max_height(ui.available_height() - 35.0)
            .show(ui, |ui| {
                if total_items == 0 {
                    ui.add_space(20.0);
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            egui::RichText::new("No plugins found.")
                                .italics()
                                .color(egui::Color32::GRAY),
                        );
                    });
                }

                for plugin_data in current_page_items {
                    let id = plugin_data
                        .get("plugin_id")
                        .or_else(|| plugin_data.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let name = plugin_data
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown");
                    let author = plugin_data
                        .get("author")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown");
                    let desc = plugin_data
                        .get("short_description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let hover = plugin_data
                        .get("long_description")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or(desc);
                    let mut short_desc = desc.trim_start().replace('\n', " ").replace('\r', "");
                    if short_desc.chars().count() > 120 {
                        short_desc = short_desc.chars().take(117).collect();
                        short_desc.push_str("...");
                    }
                    let version = plugin_data
                        .get("version_number")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let banner = plugin_data
                        .get("banner_path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

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
                                let details_width = (ui.available_width() - 88.0).max(150.0);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(details_width, 72.0),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        ui.strong(format!("{name} v{version}"));
                                        ui.weak(format!("by {author}"));
                                        ui.add_sized(
                                            [details_width, 34.0],
                                            egui::Label::new(short_desc)
                                                .wrap()
                                                .halign(egui::Align::Min),
                                        )
                                        .on_hover_text(hover);
                                    },
                                );

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let existing =
                                            self.plugin_mgr.plugins.iter().find(|p| {
                                                p.manifest.plugin_id.as_deref() == Some(id)
                                            });
                                        if let Some(existing) = existing {
                                            let slug = existing.slug.clone();
                                            if existing.enabled {
                                                if ui.button("Disable").clicked() {
                                                    action = Some(ModalAction::Disable(slug));
                                                }
                                            } else if ui
                                                .add(egui::Button::new("Enable").fill(
                                                    egui::Color32::from_rgb(0x2e, 0xcc, 0x71),
                                                ))
                                                .clicked()
                                            {
                                                action = Some(ModalAction::Enable(slug));
                                            }
                                        } else if self.install_modal.downloading_id.as_deref()
                                            == Some(id)
                                        {
                                            ui.add_enabled(
                                                false,
                                                egui::Button::new("Installing..."),
                                            );
                                        } else if ui.button("Install").clicked() {
                                            action = Some(ModalAction::Download(id.to_string()));
                                        }
                                    },
                                );
                            });
                        });
                    ui.add_space(4.0);
                }
            });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.install_modal.current_page + 1 < total_pages,
                        egui::Button::new("Next >"),
                    )
                    .clicked()
                {
                    self.install_modal.current_page += 1;
                }

                let display_page = if total_pages == 0 {
                    0
                } else {
                    self.install_modal.current_page + 1
                };
                ui.label(format!("Page {} of {}", display_page, total_pages));

                if ui
                    .add_enabled(
                        self.install_modal.current_page > 0,
                        egui::Button::new("< Prev"),
                    )
                    .clicked()
                {
                    self.install_modal.current_page -= 1;
                }
            });
        });

        match action {
            Some(ModalAction::Enable(slug)) => {
                if self.plugin_mgr.set_enabled(&slug, true, &mut self.config) {
                    self.console.write(format!("[Console] Enabled {slug}."));
                }
                self.save_config();
            }
            Some(ModalAction::Disable(slug)) => {
                self.plugin_mgr.set_enabled(&slug, false, &mut self.config);
                self.console.write(format!("[Console] Disabled {slug}."));
                self.save_config();
            }
            Some(ModalAction::Download(id)) => {
                self.download_hebnix_plugin(&id);
            }
            None => {}
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
        crate::ui::workshop::spawn_image_fetch(
            banner_path.to_string(),
            cache_dir,
            self.tx.clone(),
            ctx.clone(),
            |key, bytes| AppMsg::PluginImage { key, bytes },
        );
    }

    fn fetch_hebnix_plugin(&mut self) {
        self.install_modal.fetching = true;
        self.install_modal.error = None;
        self.install_modal.catalog.clear();

        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result: Result<Value, String> = (|| {
                let agent = ureq::AgentBuilder::new().try_proxy_from_env(false).build();
                let resp = agent
                    .get("https://api.hebnix.com/plugins")
                    .timeout(Duration::from_secs(10))
                    .call()
                    .map_err(|e| e.to_string())?;
                resp.into_json().map_err(|e| e.to_string())
            })();
            let _ = tx.send(AppMsg::PluginFetch { result });
        });
    }

    fn download_hebnix_plugin(&mut self, plugin_id: &str) {
        self.install_modal.downloading_id = Some(plugin_id.to_string());
        let plugin_id = plugin_id.to_string();
        let plugin_dir = self.plugin_dir.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result: Result<String, String> = (|| {
                let url = format!("https://api.hebnix.com/download/plugin/{plugin_id}");
                let resp = get_retry(&url, Duration::from_secs(20))?;
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut resp.into_reader(), &mut bytes)
                    .map_err(|e| e.to_string())?;
                let temp_zip = plugin_dir.join(format!("temp_plugin_{plugin_id}.zip"));
                std::fs::write(&temp_zip, &bytes).map_err(|e| e.to_string())?;
                let extract = install_zip(&temp_zip, &plugin_dir);
                let _ = std::fs::remove_file(&temp_zip);
                extract?;
                Ok(format!("Plugin ID {plugin_id} installed."))
            })();
            let _ = tx.send(AppMsg::PluginDownloadDone { result });
        });
    }

    /// once, and only when a plugin wants it
    fn ensure_webview(&mut self, wanted: bool) {
        if self.webview.is_some() || !wanted {
            return;
        }
        if !crate::webview::runtime::is_available() {
            if !self.overlay_unavailable_said {
                self.overlay_unavailable_said = true;
                self.console.write(
                    "[Core] Overlays need the WebView2 runtime, which is missing. Restart Hebnix to install it.",
                );
            }
            return;
        }
        let Some((_, visual)) = self.overlay.webview_target() else {
            if !self.overlay_unavailable_said {
                self.overlay_unavailable_said = true;
                self.console
                    .write("[Core] No overlay window on this machine, plugin overlays are off.");
            }
            return;
        };
        self.webview = Some(crate::webview::host::WebviewHost::new(visual));
        self.overlay.commit();
    }

    fn render_game_overlay(&mut self) {
        if !self.last_rl_open {
            crate::overlay::set_webview_clickable(false);
            self.overlay.hide();
            self.native_overlay.hide();
            self.overlay_rect = None;
            if let Some(webview) = &mut self.webview {
                if let Some((hwnd, _)) = self.overlay.webview_target() {
                    webview.tick(hwnd, (1, 1), false);
                }
            }
            return;
        }

        let layers = self.plugin_mgr.overlay_layers(&self.config.overlay_order);
        let html_layers = layers
            .iter()
            .filter(|(_, page, _, _)| page.is_some())
            .cloned()
            .collect::<Vec<_>>();
        self.ensure_webview(!html_layers.is_empty());
        if let Some(webview) = &mut self.webview {
            webview.sync_pages(&html_layers);
        }
        let webview_accepts_input = self
            .webview
            .as_ref()
            .is_some_and(|webview| webview.wants_input());

        let draws = self.plugin_mgr.overlay_plugins();
        let slugs = layers
            .iter()
            .map(|(slug, _, _, _)| slug.clone())
            .filter(|slug| draws.contains(slug))
            .collect::<Vec<_>>();
        let webview_wants = self
            .webview
            .as_ref()
            .is_some_and(|webview| webview.wants_overlay());
        crate::overlay::set_allow_draw_on_hebnix_focus(
            self.config.settings.allow_draw_on_hebnix_focus,
        );
        let focused = crate::overlay::has_render_focus();
        crate::overlay::set_webview_clickable(focused && webview_accepts_input);
        if (slugs.is_empty() && !webview_wants) || !focused {
            self.overlay.hide();
            self.native_overlay.hide();
            self.overlay_rect = None;
            if let Some(webview) = &mut self.webview {
                if let Some((hwnd, _)) = self.overlay.webview_target() {
                    webview.tick(hwnd, (1, 1), false);
                }
            }
            self.report_webview_error();
            return;
        }

        let now = std::time::Instant::now();
        let refresh_due = self
            .overlay_rect_checked
            .map(|checked| now.duration_since(checked).as_millis() > 250)
            .unwrap_or(true);
        if refresh_due {
            self.overlay_rect_checked = Some(now);
            self.overlay_rect = hebnix_sdk::process::get_rocket_league_window_rect();
        }
        let Some(rect) = self.overlay_rect else {
            self.overlay.hide();
            self.native_overlay.hide();
            return;
        };

        let (left, top, right, bottom) = rect;
        let size = ((right - left).max(1) as u32, (bottom - top).max(1) as u32);
        if webview_wants {
            if let Some(webview) = &mut self.webview {
                if let Some((hwnd, _)) = self.overlay.webview_target() {
                    webview.tick(hwnd, size, true);
                }
            }
            self.report_webview_error();
            if self
                .webview
                .as_ref()
                .is_some_and(|webview| webview.is_ready())
            {
                self.overlay.place(rect);
            } else {
                self.overlay.hide();
            }
        } else {
            self.overlay.hide();
        }

        let mut errors = Vec::new();
        if slugs.is_empty() {
            self.native_overlay.hide();
        } else {
            let plugin_manager = &mut self.plugin_mgr;
            self.native_overlay.frame(rect, |width, height| {
                for slug in &slugs {
                    if let Err(error) = plugin_manager.render_overlay_gdi(slug, width, height) {
                        errors.push(format!("[Core] Overlay error in '{slug}': {error}"));
                    }
                }
            });
        }
        for error in errors {
            self.console.write(error);
        }
    }
    fn report_webview_error(&mut self) {
        let Some(message) = self.webview.as_mut().and_then(|w| w.take_error()) else {
            return;
        };
        self.console
            .write(format!("[Core] Overlay webview failed: {message}"));
    }

    fn plugin_monitor_size(&mut self) -> (f32, f32) {
        let now = std::time::Instant::now();
        let due = self
            .plugin_monitor_checked
            .map(|t| now.duration_since(t).as_millis() > 500)
            .unwrap_or(true);
        if due {
            self.plugin_monitor_checked = Some(now);
            let (w, h) = hebnix_sdk::process::rocket_league_monitor_size();
            self.plugin_monitor_size = (w as f32, h as f32);
        }
        self.plugin_monitor_size
    }

    fn render_plugin_windows(&mut self, ctx: &egui::Context) {
        let (mon_w, mon_h) = self.plugin_monitor_size();
        let ppp = ctx.pixels_per_point();

        let mut active_plugins = Vec::new();
        for plugin in &self.plugin_mgr.plugins {
            if !plugin.enabled {
                continue;
            }
            if let Some(rt) = &plugin.runtime {
                let win = rt.host.window.borrow().clone();
                active_plugins.push((plugin.slug.clone(), win));
            }
        }

        for (slug, win) in active_plugins {
            let viewport_id = egui::ViewportId::from_hash_of(("plugin_window", &slug));
            let should_be_visible = win.open;
            let size = [
                win.width.resolve(mon_w, ppp),
                win.height.resolve(mon_h, ppp),
            ];

            let mut builder = egui::ViewportBuilder::default()
                .with_title(win.title.clone())
                .with_inner_size(size)
                .with_decorations(false)
                .with_always_on_top()
                .with_resizable(false)
                .with_transparent(true)
                .with_mouse_passthrough(false)
                .with_visible(should_be_visible)
                .with_taskbar(false);

            if let Some((x, y)) = win.pos {
                builder = builder.with_position([x, y]);
            }

            ctx.show_viewport_immediate(viewport_id, builder, |ui, _class| {
                if !should_be_visible {
                    return;
                }

                let ctx = ui.ctx().clone();
                ctx.request_repaint();
                let fill = ui.visuals().window_fill;
                let fill = egui::Color32::from_rgba_unmultiplied(
                    fill.r(),
                    fill.g(),
                    fill.b(),
                    (win.opacity * 255.0) as u8,
                );

                let frame = egui::Frame::new()
                    .fill(fill)
                    .inner_margin(egui::Margin::same(8));

                egui::CentralPanel::default().frame(frame).show(ui, |ui| {
                    let header_height = 22.0;
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), header_height),
                        egui::Sense::drag(),
                    );
                    if resp.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    let title_rect = if win.close_button {
                        egui::Rect::from_min_max(
                            rect.min,
                            egui::pos2(rect.right() - header_height, rect.bottom()),
                        )
                    } else {
                        rect
                    };
                    ui.painter().text(
                        title_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        &win.title,
                        egui::FontId::proportional(13.0),
                        ui.visuals().strong_text_color(),
                    );
                    if win.close_button {
                        let close_rect = egui::Rect::from_min_max(
                            egui::pos2(rect.right() - header_height, rect.top()),
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

                    if let Err(e) = self.plugin_mgr.render_window(&slug, ui) {
                        ui.colored_label(
                            egui::Color32::from_rgb(0xe7, 0x4c, 0x3c),
                            format!("window error: {e}"),
                        );
                    }
                });

                if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
                    let new_pos = (rect.min.x, rect.min.y);
                    let moved = match win.last_pos {
                        Some((x, y)) => (x - new_pos.0).abs() > 1.0 || (y - new_pos.1).abs() > 1.0,
                        None => true,
                    };

                    if moved {
                        self.plugin_mgr.set_window_pos(&slug, new_pos.0, new_pos.1);
                    }
                }
            });
        }
    }
}

fn install_zip(zip_path: &std::path::Path, plugin_dir: &std::path::Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    archive.extract(plugin_dir).map_err(|e| e.to_string())?;
    Ok(())
}

impl Drop for HebnixApp {
    fn drop(&mut self) {
        // Covers normal eframe shutdown paths that do not go through the
        // explicit tray/window quit handler.
        self.spoofer_mgr.shutdown();
        self.workshop.suspend_multiplayer();
    }
}

impl eframe::App for HebnixApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        self.handle_messages(ctx);

        if !self.statsapi_checked {
            self.check_statsapi_rate();
            self.check_web_port();
        }

        if winutil::take_minimize_request() && !self.hidden {
            self.set_hidden(ctx, true);
        }

        crate::dpi_fix::install_on_all_windows();

        if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
            let size = rect.size();
            if size.x > 0.0 && size.y > 0.0 {
                self.last_size = (size.x as u32, size.y as u32);
            }
        }

        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            if self.update_info.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            } else {
                self.force_quit(ctx);
            }
        }

        if self.update_info.is_some() {
            if self.hidden {
                self.set_hidden(ctx, false);
            }
            egui::CentralPanel::default().show(ui, |ui| {
                ui.disable();
                ui.centered_and_justified(|ui| {
                    ui.heading("A required Hebnix update is available");
                });
            });
            self.render_update_modal(ctx);
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }

        if !self.hidden {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.tab, Tab::Console, "Console");
                    ui.selectable_value(&mut self.tab, Tab::Workshop, "Workshop Maps");
                    ui.selectable_value(&mut self.tab, Tab::Spoofer, "Spoofer");
                    ui.selectable_value(&mut self.tab, Tab::Patcher, "Items");
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
                                || !std::path::Path::new(&path).is_dir()
                            {
                                self.launch_path_notice = true;
                            } else {
                                let tx = self.tx.clone();
                                std::thread::spawn(move || {
                                    let result = if running {
                                        crate::winutil::restart_rocket_league(
                                            std::path::Path::new(&path),
                                        )
                                    } else {
                                        crate::winutil::start_rocket_league(
                                            std::path::Path::new(&path),
                                        )
                                    };
                                    let action = if running { "restart" } else { "start" };
                                    let message = match result {
                                        Ok(()) => format!(
                                            "[Core] Rocket League {} requested.",
                                            action
                                        ),
                                        Err(error) => format!(
                                            "[Core] Rocket League {} failed: {}",
                                            action, error
                                        ),
                                    };
                                    let _ = tx.send(AppMsg::Log(message));
                                });
                            }
                        }
                    });
                });
                ui.separator();

                match self.tab {
                    Tab::Console => self.render_console_tab(ui),
                    Tab::Workshop => {
                        let rl_path = self.config.settings.rl_path.clone();
                        let tx = self.tx.clone();
                        self.workshop.render(ui, &rl_path, &tx);
                    }
                    Tab::Spoofer => self.render_spoofer_tab(ui),
                    Tab::Patcher => {
                        let rl_path = self.config.settings.rl_path.clone();
                        let cooked_pc = std::path::Path::new(&rl_path)
                            .join("TAGame")
                            .join("CookedPCConsole");
                        let backups_dir = cooked_pc.join("Backups");

                        egui::Panel::left("patcher_settings_list")
                            .resizable(false)
                            .exact_size(150.0)
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt("patcher_subtabs")
                                    .show(ui, |ui| {
                                        ui.selectable_value(
                                            &mut self.patcher_subtab,
                                            PatcherSubTab::Ball,
                                            "Ball Patcher",
                                        );
                                        ui.selectable_value(
                                            &mut self.patcher_subtab,
                                            PatcherSubTab::BoostMeter,
                                            "Boost Patcher",
                                        );
                                        ui.selectable_value(
                                            &mut self.patcher_subtab,
                                            PatcherSubTab::Decal,
                                            "Decal Patcher",
                                        );
                                        ui.separator();
                                        for category in crate::swapper::SwapCategory::ALL {
                                            ui.selectable_value(
                                                &mut self.patcher_subtab,
                                                PatcherSubTab::Swapper(category),
                                                category.label(),
                                            );
                                        }
                                        ui.separator();
                                        ui.selectable_value(
                                            &mut self.patcher_subtab,
                                            PatcherSubTab::Active,
                                            "Active",
                                        );
                                        ui.selectable_value(
                                            &mut self.patcher_subtab,
                                            PatcherSubTab::Presets,
                                            "Presets",
                                        );
                                    });
                            });

                        egui::CentralPanel::default()
                            .frame(egui::Frame::new())
                            .show(ui, |ui| match self.patcher_subtab {
                                PatcherSubTab::Ball => {
                                    self.patcher_ball.render_ball_tab(
                                        ui,
                                        &cooked_pc,
                                        &cooked_pc.join("Mutators_Balls_SF.upk"),
                                        &backups_dir,
                                        &self.tx,
                                        ctx,
                                        &mut self.config,
                                    );
                                }
                                PatcherSubTab::BoostMeter => {
                                    self.patcher_boost.render_tab(
                                        ui,
                                        &cooked_pc,
                                        &backups_dir,
                                        &self.tx,
                                        ctx,
                                        &mut self.config,
                                    );
                                }
                                PatcherSubTab::Decal => {
                                    self.patcher_decal.render_tab(
                                        ui,
                                        &cooked_pc,
                                        &backups_dir,
                                        &self.tx,
                                        ctx,
                                        &mut self.config,
                                    );
                                }
                                PatcherSubTab::Swapper(category) => {
                                    let owned_ids = self.spoofer_mgr.owned_product_ids();
                                    let requested = self.swapper.render_tab(
                                        ui,
                                        category,
                                        &cooked_pc,
                                        &backups_dir,
                                        &self.tx,
                                        &owned_ids,
                                    );
                                    if requested {
                                        self.swapper.set_owned_only(false);
                                        if !spoofer::is_admin() {
                                            self.owned_admin_requested = true;
                                            self.admin_prompt_open = true;
                                        } else {
                                            self.owned_proxy_prompt_open = true;
                                        }
                                    }
                                }
                                PatcherSubTab::Active => {
                                    self.patcher_ball.poll_ops(
                                        &self.tx,
                                        ctx,
                                        &mut self.config,
                                    );
                                    self.patcher_boost.poll_ops(&self.tx, &mut self.config);
                                    self.patcher_decal
                                        .handle_op_result(&self.tx, &mut self.config);
                                    egui::ScrollArea::vertical()
                                        .id_salt("active_items")
                                        .auto_shrink([false, false])
                                        .show(ui, |ui| {
                                    ui.heading("Active Items");
                                    let swap_count = self.swapper.active_count(&backups_dir);
                                    let active_count = swap_count
                                        + usize::from(self.patcher_ball.active_ball.is_some())
                                        + usize::from(self.patcher_boost.active_boost.is_some())
                                        + self.patcher_decal.active_decals.len();
                                    ui.horizontal(|ui| {
                                        ui.label(format!("{active_count} active change(s)"));
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui
                                                    .add_enabled(
                                                        active_count > 0,
                                                        egui::Button::new("Restore All"),
                                                    )
                                                    .clicked()
                                                {
                                                    if self.patcher_ball.active_ball.is_some() {
                                                        self.patcher_ball.begin_restore(
                                                            &cooked_pc,
                                                            &backups_dir,
                                                            &self.tx,
                                                            ctx,
                                                        );
                                                    }
                                                    if self.patcher_boost.active_boost.is_some() {
                                                        self.patcher_boost.begin_restore(
                                                            &cooked_pc,
                                                            &backups_dir,
                                                            &self.tx,
                                                            ctx,
                                                        );
                                                    }
                                                    if !self.patcher_decal.active_decals.is_empty() {
                                                        let _ = self.patcher_decal.restore_all_decals(
                                                            &cooked_pc,
                                                            &backups_dir,
                                                            &self.tx,
                                                            ctx,
                                                        );
                                                    }
                                                    match self.swapper.restore_all_active(
                                                        &cooked_pc,
                                                        &backups_dir,
                                                    ) {
                                                        Ok(count) if count > 0 => {
                                                            let _ = self.tx.send(AppMsg::Log(format!(
                                                                "[Swapper] Restored {count} active swap(s)."
                                                            )));
                                                        }
                                                        Err(error) => {
                                                            let _ = self.tx.send(AppMsg::Log(format!(
                                                                "[Swapper] Error: {error}"
                                                            )));
                                                        }
                                                        _ => {}
                                                    }
                                                }
                                            },
                                        );
                                    });
                                    ui.separator();
                                    if active_count == 0 {
                                        ui.vertical_centered(|ui| ui.weak("No patches or swaps are active."));
                                    }

                                    let active_ball = self.patcher_ball.active_ball.clone();
                                    if let Some(name) = active_ball {
                                        let image = self
                                            .patcher_ball
                                            .balls
                                            .iter()
                                            .find(|item| item.name == name)
                                            .and_then(|item| item.image_bytes.clone());
                                        ui.horizontal(|ui| {
                                            if let Some(image) = image {
                                                ui.add(egui::Image::from_bytes(
                                                    format!("bytes://active/ball/{name}"), image,
                                                ).fit_to_exact_size(egui::vec2(90.0, 58.0)));
                                            }
                                            ui.vertical(|ui| {
                                                ui.strong(&name);
                                                ui.weak("Replaced the default ball");
                                            });
                                            if ui.button("Restore").clicked() {
                                                self.patcher_ball.begin_restore(
                                                    &cooked_pc, &backups_dir, &self.tx, ctx,
                                                );
                                            }
                                        });
                                    }
                                    let active_boost = self.patcher_boost.active_boost.clone();
                                    if let Some(name) = active_boost {
                                        let image = self
                                            .patcher_boost
                                            .boosts
                                            .iter()
                                            .find(|item| item.name == name)
                                            .and_then(|item| item.background_image.clone().or_else(|| item.fill_image.clone()));
                                        ui.horizontal(|ui| {
                                            if let Some(image) = image {
                                                ui.add(egui::Image::from_bytes(
                                                    format!("bytes://active/boost/{name}"), image,
                                                ).fit_to_exact_size(egui::vec2(90.0, 58.0)));
                                            }
                                            ui.vertical(|ui| {
                                                ui.strong(&name);
                                                ui.weak("Replaced the default boost meter");
                                            });
                                            if ui.button("Restore").clicked() {
                                                self.patcher_boost.begin_restore(
                                                    &cooked_pc, &backups_dir, &self.tx, ctx,
                                                );
                                            }
                                        });
                                    }
                                    let active_decals = self
                                        .patcher_decal
                                        .active_decals
                                        .iter()
                                        .map(|(target, name)| (target.clone(), name.clone()))
                                        .collect::<Vec<_>>();
                                    let mut restore_decal = None;
                                    for (target, name) in active_decals {
                                        let target_label = self
                                            .patcher_decal
                                            .target_display_name(&target);
                                        let image = self.patcher_decal.decals.iter()
                                            .find(|item| item.name == name)
                                            .and_then(|item| item.preview_image.clone());
                                        ui.horizontal(|ui| {
                                            if let Some(image) = image {
                                                ui.add(egui::Image::from_bytes(
                                                    format!("bytes://active/decal/{target}"), image,
                                                ).fit_to_exact_size(egui::vec2(90.0, 58.0)));
                                            }
                                            ui.vertical(|ui| {
                                                ui.strong(&name);
                                                ui.weak(format!("Replaced {target_label}"));
                                            });
                                            if ui.button("Restore").clicked() {
                                                restore_decal = target.split_once('|')
                                                    .map(|(car, skin)| (car.to_string(), skin.to_string()));
                                            }
                                        });
                                    }
                                    if let Some((car, skin)) = restore_decal {
                                        let _ = self.patcher_decal.restore_decal_from_skin(
                                            &car, &skin, &cooked_pc, &backups_dir, &self.tx, ctx,
                                        );
                                    }
                                    self.swapper.render_active_swaps(
                                        ui, &cooked_pc, &backups_dir, &self.tx,
                                    );
                                        });
                                }
                                PatcherSubTab::Presets => {
                                    self.render_presets_tab(ui, &backups_dir);
                                }
                            });
                    }
                    Tab::Settings => self.render_settings_tab(ui),
                    Tab::Plugins => self.render_plugins_tab(ui),
                    Tab::About => self.render_about_tab(ui),
                }
            });
        }

        if self.restart_notice && !self.hidden {
            let mut close = false;
            egui::Window::new("Hebnix")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(
                        "Hebnix has initialised StatsAPI. Please restart Rocket League to initialise Hebnix.",
                    );
                    ui.add_space(8.0);
                    ui.vertical_centered(|ui| {
                        if ui.button("Restart Rocket League").clicked() {
                            let path = self.config.settings.rl_path.clone();
                            let _ = crate::winutil::restart_rocket_league(std::path::Path::new(&path));
                            close = true;
                        }
                        if ui.button("OK").clicked() {
                            close = true;
                        }
                    });
                });
            if close {
                self.restart_notice = false;
            }
        }

        if !self.hidden {
            self.render_admin_prompt(ctx);
            self.render_owned_proxy_prompt(ctx);
            self.render_statsapi_notice(ctx);
            self.render_web_port_notice(ctx);
            self.render_fullscreen_notice(ctx);
            self.render_launch_path_notice(ctx);
            self.render_changelog_popup(ctx);
            self.render_update_modal(ctx);
            self.render_install_modal(ctx);
        }
        let plugin_tick_interval = if self.last_rl_open {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(500)
        };
        self.plugin_mgr
            .dispatch_tick_if_due(plugin_tick_interval);
        self.render_plugin_windows(ctx);
        self.plugin_mgr.flush_window_positions();
        self.render_game_overlay();

        let fast = self.last_rl_open
            && (self.plugin_mgr.has_tick_plugins()
                || !self.plugin_mgr.overlay_plugins().is_empty());
        let heartbeat = if fast {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(500)
        };
        ctx.request_repaint_after(heartbeat);
    }
}
