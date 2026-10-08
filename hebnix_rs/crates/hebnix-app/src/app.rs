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

use crate::config::{ActionButtonAction, ActionButtonEntry, Config};
use crate::hotkey::ToggleHotkey;
use crate::i18n::{t, t_args, t_n};
use crate::messages::AppMsg;
use crate::monitor::{Monitor, MonitorShared};
use crate::plugins::PluginManager;
use crate::spoofer;
use crate::spoofer::SpooferManager;
use crate::spoofer::rules::TitleSpoofSettings;
use crate::statsapi_ini;
use crate::theme;
use crate::tray::Tray;
use crate::ui::console::ConsoleState;
use crate::ui::workshop::{ImageState, WorkshopState};
use crate::winutil;

pub const APP_VERSION: &str = "2.2.2";

pub const DEFAULT_WIDTH: f32 = 1250.0;
pub const DEFAULT_HEIGHT: f32 = 700.0;
pub const MIN_WIDTH: f32 = DEFAULT_WIDTH;
pub const MIN_HEIGHT: f32 = DEFAULT_HEIGHT;

/// width for a settings page's label column: the widest label on that page,
/// never narrower than the old fixed 130px, so longer translations aren't
/// clipped and the inputs next to them still line up.
fn label_column_width(ui: &egui::Ui, labels: &[String]) -> f32 {
    crate::i18n::layout::label_column_width(ui, labels, 130.0)
}

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

const CATALOG_NAMES: [&str; 14] = [
    "antennas", "anthems", "banners", "bodies", "boosts", "borders", "engines", "finishes",
    "goals", "skins", "titles", "toppers", "trails", "wheels",
];

fn titles_from_catalog(root: &Value) -> Result<Vec<TitleCatalogEntry>, String> {
    let titles = root
        .get("titles")
        .cloned()
        .ok_or_else(|| "The titles catalog has no 'titles' array".to_string())?;
    let titles: Vec<TitleCatalogEntry> = serde_json::from_value(titles)
        .map_err(|error| format!("Failed to parse the titles catalog: {error}"))?;
    if titles.is_empty() {
        Err("The titles catalog is empty".to_string())
    } else {
        Ok(titles)
    }
}

fn fetch_catalogs(tx: Sender<AppMsg>, ctx: egui::Context) {
    std::thread::Builder::new()
        .name("catalog-fetcher".into())
        .spawn(move || {
            let result = (|| {
                let mut catalogs = HashMap::new();
                for name in CATALOG_NAMES {
                    let url = format!("https://api.hebnix.com/catalogs/{name}.json");
                    let response = get_retry(&url, Duration::from_secs(20))
                        .map_err(|error| format!("Failed to fetch {name}.json: {error}"))?;
                    let root: Value = response
                        .into_json()
                        .map_err(|error| format!("Failed to parse {name}.json: {error}"))?;
                    let valid = if name == "skins" {
                        root.get("cars").is_some_and(Value::is_object)
                    } else {
                        root.get(name).is_some_and(Value::is_array)
                    };
                    if !valid {
                        return Err(format!("{name}.json has an invalid catalog structure"));
                    }
                    catalogs.insert(name.to_string(), root);
                }
                Ok(catalogs)
            })();
            let _ = tx.send(AppMsg::CatalogsFetched { result });
            ctx.request_repaint();
        })
        .ok();
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
    Colours,
    Settings,
    Plugins,
    Notifications,
    RlApi,
    About,
}

impl Tab {
    const ALL: [Self; 10] = [
        Self::Console,
        Self::Workshop,
        Self::Spoofer,
        Self::Colours,
        Self::Patcher,
        Self::Settings,
        Self::Plugins,
        Self::Notifications,
        Self::RlApi,
        Self::About,
    ];

    /// translated tab name for the UI
    fn label(self) -> String {
        match self {
            Self::Console => t("tab-console"),
            Self::Workshop => t("tab-workshop"),
            Self::Spoofer => t("tab-spoofer"),
            Self::Patcher => t("tab-patcher"),
            Self::Colours => t("tab-colours"),
            Self::Settings => t("tab-settings"),
            Self::Plugins => t("tab-plugins"),
            Self::Notifications => t("tab-notifications"),
            Self::RlApi => t("tab-rlapi"),
            Self::About => t("tab-about"),
        }
    }

    /// value stored in config.toml (`default_tab`). these are the original
    /// english names so existing configs keep working, never translate them.
    fn config_key(self) -> &'static str {
        match self {
            Self::Console => "Console",
            Self::Workshop => "Maps",
            Self::Spoofer => "Spoofer",
            Self::Patcher => "Items",
            Self::Colours => "Colours",
            Self::Settings => "Settings",
            Self::Plugins => "Plugins",
            Self::Notifications => "Notifications",
            Self::RlApi => "Experimental",
            Self::About => "About",
        }
    }

    fn from_config_key(key: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|tab| tab.config_key() == key)
            .unwrap_or(Self::Console)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemsMode {
    Swapper,
    Spawner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpawnerSubTab {
    Tutorial,
    Category(crate::swapper::SwapCategory),
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
enum ExperimentalSubTab {
    RlApi,
    AutoUploadReplays,
    BallAppearance,
    CarPatcher,
    WheelAlignment,
    SpeedPatch,
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
    Discord,
    ActionButton,
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

struct ReplayUploadStartPrompt {
    issues: Vec<String>,
    restart_required: bool,
    needs_admin: bool,
    needs_certificate: bool,
    missing_api_key: bool,
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
    discord_presence: crate::discord_presence::DiscordPresence,
    discord_link: crate::discord_link::DiscordLinkState,
    plugin_mgr: PluginManager,
    tray: Option<Tray>,
    hotkey: Option<ToggleHotkey>,
    _secretsequence: crate::veryimportantfile::SecretSequenceListener,

    tab: Tab,
    settings_subtab: SettingsSubTab,
    hebnix_settings_tab: HebnixSettingsTab,
    spoofer_subtab: SpooferSubTab,
    patcher_subtab: PatcherSubTab,
    experimental_subtab: ExperimentalSubTab,
    speed_patch: crate::speed_patch::State,
    console: ConsoleState,
    workshop: WorkshopState,
    rlapi_panel: crate::ui::rlapi::RlApiPanel,
    auto_upload_replays: crate::auto_upload_replays::AutoUploadReplays,
    replay_upload_start_prompt: Option<ReplayUploadStartPrompt>,
    replay_upload_restart_after_capture: bool,

    hidden: bool,
    topmost: bool,
    currently_connected: bool,
    last_rl_open: bool,
    last_api_open: bool,
    in_match: bool,
    discord_match: Option<crate::discord_presence::MatchInfo>,
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
    toasts: crate::toast::ToastCenter,
    toast_view: crate::ui::toasts::ToastView,
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
    epic_repair: crate::epic_connection::RepairState,
    launch_path_notice: bool,
    rl_launch_setup_open: bool,
    rl_launch_draft: crate::config::RlLaunchCfg,
    rl_launch_shortcut_candidates: Vec<crate::rl_launch::ShortcutCandidate>,

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
    spoofer_titles: Vec<TitleSpoofSettings>,
    spoofer_title_filter: String,
    spoofer_title_copy: Option<String>,
    spoofer_title_copy_filter: String,
    title_catalog: Vec<TitleCatalogEntry>,
    catalogs_loading: bool,
    catalogs_loaded: bool,
    catalogs_error: Option<String>,
    spoofer_rank_enabled: bool,
    spoofer_ranks: HashMap<i32, RankSpoofState>,
    spoofer_cert_installed: bool,
    spoofer_certificate_prompt_open: bool,
    spoofer_friends_enabled: bool,
    spoofer_friends: HashMap<String, FriendSpoofState>,
    friends_search: String,
    item_spawner_enabled: bool,
    items_mode: ItemsMode,
    spawn_restart_pending: bool,
    spawner_subtab: SpawnerSubTab,
    spawner_enable_prompt_open: bool,
    spawner_admin_requested: bool,

    patcher_ball: crate::ball::PatcherState,
    patcher_boost: crate::boost_patcher::BoostPatcherState,
    patcher_decal: crate::decal_patcher::DecalPatcherState,
    patcher_car: crate::car_patcher::CarPatcherState,
    wheel_alignment: crate::patcher::wheel_alignment::WheelAlignmentState,
    swapper: crate::swapper::SwapperState,
    colours: crate::patcher::colours::ColoursState,
    colour_admin_prompt_open: bool,
    admin_prompt_open: bool,
    item_action_prompt_open: bool,
    plugin_delete_prompt: Option<String>,
    owned_admin_requested: bool,
    owned_proxy_prompt_open: bool,
    presets: crate::presets::PresetStore,
}

fn clear_rl_cache(tx: &Sender<AppMsg>) {
    match winutil::clear_rocket_league_web_cache() {
        Ok(()) => {
            let _ = tx.send(AppMsg::Log(
                "[Spoofer] cleared Rocket League WebCache".into(),
            ));
        }
        Err(error) => {
            let _ = tx.send(AppMsg::Log(format!(
                "[Spoofer] could not clear Rocket League WebCache: {error}"
            )));
        }
    }
}

impl HebnixApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        hebnix_sdk::input::init_controllers();
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

        // one-shot check for a stale Tailscale DNS policy rule left behind by
        // a crashed/force-killed multiplayer session (see dns_cleanup.rs) --
        // registry + a short socket probe, so it goes on its own thread
        // rather than blocking startup.
        {
            let tx = tx.clone();
            let ctx = cc.egui_ctx.clone();
            std::thread::Builder::new()
                .name("dns-policy-check".into())
                .spawn(move || {
                    crate::multiplayer_lan::clean_stale_nrpt_rule(|msg| {
                        let _ = tx.send(AppMsg::Log(msg.to_string()));
                        ctx.request_repaint();
                    });
                })
                .ok();
        }

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

        crate::screen::init(tx.clone());
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

        let secretsequence = crate::veryimportantfile::SecretSequenceListener::new();
        secretsequence.start();

        if let Some(hwnd) = winutil::main_window_hwnd() {
            crate::dpi_fix::install(hwnd);
            winutil::install_minimize_hook(hwnd, &cc.egui_ctx);
        } else {
            tracing::warn!("dpi_fix: main window not found at startup");
        }

        let mut workshop = WorkshopState::new(&base_dir);
        workshop.set_share_files(config.settings.p2p_file_sharing);
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
        let mut spoofer_titles = Vec::new();
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
                if let Some(titles) = json.get("spoofer_titles") {
                    spoofer_titles = serde_json::from_value(titles.clone()).unwrap_or_default();
                }
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

        // Migrate the former single-title settings into the stack once.
        if spoofer_titles.is_empty() && spoofer_title_enabled && !spoofer_title.trim().is_empty() {
            spoofer_titles.push(TitleSpoofSettings {
                text: spoofer_title.trim().chars().take(64).collect(),
                color: format!(
                    "{:02X}{:02X}{:02X}",
                    spoofer_title_color[0], spoofer_title_color[1], spoofer_title_color[2]
                ),
                glow: spoofer_title_glow,
                target_id: spoofer_title_target.clone(),
            });
            spoofer_title.clear();
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
        spoofer_mgr.set_titles(spoofer_titles.clone());
        spoofer_mgr.set_title_enabled(spoofer_title_enabled);

        let patcher_ball = crate::ball::PatcherState::new(&base_dir, &config);
        let patcher_boost = crate::boost_patcher::BoostPatcherState::new(&base_dir, &config);
        let patcher_decal = crate::decal_patcher::DecalPatcherState::new(&base_dir, &config);
        let patcher_car = crate::car_patcher::CarPatcherState::new(&base_dir, &config);
        let mut swapper = crate::swapper::SwapperState::new(&base_dir);
        let owned_restart_marker = base_dir.join("enable_owned_replacements.pending");
        if spoofer::is_admin() && owned_restart_marker.is_file() {
            let _ = std::fs::remove_file(&owned_restart_marker);
            spoofer_master = true;
            spoofer_http_proxy = true;
            swapper.set_owned_only(true);
        }
        let spawner_restart_marker = base_dir.join("enable_item_spawner.pending");
        let enable_spawner_after_admin = spoofer::is_admin() && spawner_restart_marker.is_file();
        if enable_spawner_after_admin {
            let _ = std::fs::remove_file(&spawner_restart_marker);
            spoofer_master = true;
        }
        let cert_installed = spoofer::ca::is_current_installed(&base_dir);
        let rl_launch_draft = config.rl_launch.clone();
        let default_tab = Tab::from_config_key(&config.settings.default_tab);

        let discord_presence =
            crate::discord_presence::DiscordPresence::start(config.settings.discord_rich_presence);
        discord_presence.set_idle(
            &config.settings,
            hebnix_sdk::process::is_rocket_league_running(),
        );

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
            discord_presence,
            discord_link: crate::discord_link::DiscordLinkState::new(&base_dir),
            plugin_mgr,
            tray,
            hotkey,
            _secretsequence: secretsequence,
            tab: default_tab,
            settings_subtab: SettingsSubTab::Hebnix,
            hebnix_settings_tab: HebnixSettingsTab::Interface,
            spoofer_subtab: SpooferSubTab::Settings,
            patcher_subtab: PatcherSubTab::Ball,
            experimental_subtab: ExperimentalSubTab::BallAppearance,
            speed_patch: crate::speed_patch::State::default(),
            console: ConsoleState::default(),
            workshop,
            rlapi_panel: crate::ui::rlapi::RlApiPanel::default(),
            auto_upload_replays: crate::auto_upload_replays::AutoUploadReplays::new(&base_dir),
            replay_upload_start_prompt: None,
            replay_upload_restart_after_capture: false,
            hidden,
            topmost: false,
            currently_connected: false,
            last_rl_open: false,
            last_api_open: false,
            in_match: false,
            discord_match: None,
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
            toasts: Default::default(),
            toast_view: Default::default(),
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
            epic_repair: Default::default(),
            launch_path_notice: false,
            rl_launch_setup_open: false,
            rl_launch_draft,
            rl_launch_shortcut_candidates: Vec::new(),
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
            spoofer_titles,
            spoofer_title_filter: String::new(),
            spoofer_title_copy,
            spoofer_title_copy_filter: String::new(),
            title_catalog: Vec::new(),
            catalogs_loading: true,
            catalogs_loaded: false,
            catalogs_error: None,
            spoofer_rank_enabled,
            spoofer_ranks,
            spoofer_cert_installed: cert_installed,
            spoofer_certificate_prompt_open: spoofer_master
                && (spoofer_http_proxy || spoofer_socket_proxy)
                && !cert_installed,
            spoofer_friends_enabled,
            spoofer_friends,
            friends_search: String::new(),
            item_spawner_enabled: false,
            items_mode: ItemsMode::Swapper,
            spawn_restart_pending: false,
            spawner_subtab: SpawnerSubTab::Tutorial,
            spawner_enable_prompt_open: false,
            spawner_admin_requested: false,
            patcher_ball,
            patcher_boost,
            patcher_decal,
            patcher_car,
            wheel_alignment: Default::default(),
            swapper,
            colours: crate::patcher::colours::ColoursState::new(),
            colour_admin_prompt_open: false,
            admin_prompt_open: false,
            item_action_prompt_open: false,
            plugin_delete_prompt: None,
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
        if enable_spawner_after_admin {
            app.enable_item_spawner();
        }
        fetch_catalogs(app.tx.clone(), cc.egui_ctx.clone());

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
            "spoofer_titles": self.spoofer_titles,
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

    fn apply_downloaded_catalogs(
        &mut self,
        catalogs: HashMap<String, Value>,
    ) -> Result<(), String> {
        let titles = titles_from_catalog(
            catalogs
                .get("titles")
                .ok_or_else(|| "The titles catalog was not downloaded".to_string())?,
        )?;
        let skins = catalogs
            .get("skins")
            .cloned()
            .ok_or_else(|| "The skins catalog was not downloaded".to_string())?;
        let bodies = catalogs
            .get("bodies")
            .cloned()
            .ok_or_else(|| "The bodies catalog was not downloaded".to_string())?;
        self.swapper.set_catalogs(&catalogs)?;
        self.wheel_alignment.set_catalog(&bodies);
        self.patcher_decal.set_catalogs(skins, bodies)?;
        self.patcher_car.set_catalog(
            catalogs
                .get("bodies")
                .cloned()
                .ok_or_else(|| "The bodies catalog was not downloaded".to_string())?,
        )?;
        self.title_catalog = titles;
        Ok(())
    }

    fn reload_catalogs(&mut self, ctx: &egui::Context) {
        if self.catalogs_loading {
            return;
        }
        self.catalogs_loading = true;
        self.catalogs_error = None;
        fetch_catalogs(self.tx.clone(), ctx.clone());
    }

    fn enable_rlapi(&mut self) {
        if self.rlapi_panel.starting {
            return;
        }
        if !spoofer::is_admin() {
            self.rlapi_panel.complete(Err(t(
                "enable-rlapi-run-hebnix-as-administrator-then-enable",
            )
            .into()));
            return;
        }
        self.rlapi_panel.starting = true;
        let manager = Arc::clone(&self.spoofer_mgr);
        let base_dir = self.base_dir.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                // Reuse the user's existing setup. Never install a CA or
                // request elevated privileges implicitly from this tab.
                if !spoofer::ca::is_current_installed(&base_dir) {
                    return Err("Install the existing Hebnix certificate in Spoofer settings, then enable RLAPI.".into());
                }
                if !hebnix_sdk::process::is_rocket_league_running() {
                    crate::winutil::clear_rocket_league_web_cache().map_err(|e| {
                        format!("Could not refresh Rocket League's cached configuration: {e}")
                    })?;
                }
                manager.enable_rlapi()
            })();
            let _ = tx.send(AppMsg::RlApiCaptureReady(result));
        });
    }

    fn render_rlapi_tab(&mut self, ui: &mut egui::Ui) {
        use crate::ui::rlapi::Action;
        let session = hebnix_sdk::rlapi::session::shared_game_session();
        let status = if self.rlapi_panel.starting {
            t("rlapi-enabling-capture").into()
        } else {
            session.status()
        };
        match self
            .rlapi_panel
            .show(ui, session.enabled(), session.connected(), &status)
        {
            Some(Action::Enable) => self.enable_rlapi(),
            Some(Action::Disable) => self.spoofer_mgr.disable_rlapi(),
            Some(Action::Send { service, body }) => {
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let result = session.request(&service, body);
                    let _ = tx.send(AppMsg::RlApiResponse(result));
                });
            }
            None => {}
        }
    }

    fn replay_upload_start_requirements(&mut self) -> ReplayUploadStartPrompt {
        let needs_admin = !spoofer::is_admin();
        let needs_certificate = !spoofer::ca::is_current_installed(&self.base_dir);
        self.spoofer_cert_installed = !needs_certificate;
        let missing_api_key = self.config.replay_upload.api_key.trim().is_empty();
        let session = hebnix_sdk::rlapi::session::shared_game_session();
        let restart_required =
            hebnix_sdk::process::is_rocket_league_running() && !session.has_connection();
        let mut issues = Vec::new();
        if missing_api_key {
            issues.push("Enter a ballchasing.com API key.".to_string());
        }
        if needs_admin {
            issues.push("Hebnix must be running as administrator.".to_string());
        }
        if needs_certificate {
            issues.push("Install the Hebnix proxy certificate in Spoofer > Settings.".to_string());
        }
        if restart_required {
            issues.push(
                "Rocket League is open, but its PsyNet session was not captured. Enable capture and restart Rocket League."
                    .to_string(),
            );
        }
        ReplayUploadStartPrompt {
            issues,
            restart_required,
            needs_admin,
            needs_certificate,
            missing_api_key,
        }
    }

    fn request_replay_auto_upload_start(&mut self) {
        let prompt = self.replay_upload_start_requirements();
        if prompt.issues.is_empty() {
            self.enable_replay_auto_upload();
        } else {
            self.replay_upload_start_prompt = Some(prompt);
        }
    }

    fn enable_replay_auto_upload(&mut self) {
        if self.auto_upload_replays.begin_start().is_err() {
            return;
        }
        if self.config.replay_upload.api_key.trim().is_empty() {
            self.auto_upload_replays.finish_start(&Err(
                "Enter a ballchasing.com API key before starting.".to_string(),
            ));
            return;
        }
        if !spoofer::is_admin() {
            self.auto_upload_replays.finish_start(&Err(
                "Run Hebnix as administrator, then start auto upload.".to_string(),
            ));
            return;
        }
        let manager = Arc::clone(&self.spoofer_mgr);
        let base_dir = self.base_dir.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                if !spoofer::ca::is_current_installed(&base_dir) {
                    return Err("Install the existing Hebnix certificate in Spoofer settings, then start auto upload.".into());
                }
                if !hebnix_sdk::process::is_rocket_league_running() {
                    crate::winutil::clear_rocket_league_web_cache().map_err(|error| {
                        format!("Could not refresh Rocket League's cached configuration: {error}")
                    })?;
                }
                manager.enable_rlapi()
            })();
            let _ = tx.send(AppMsg::ReplayUploadCaptureReady(result));
        });
    }

    fn render_replay_auto_upload_tab(&mut self, ui: &mut egui::Ui) {
        let session = hebnix_sdk::rlapi::session::shared_game_session();
        ui.horizontal(|ui| {
            let running = self.auto_upload_replays.running();
            if ui
                .add_enabled(
                    !self.auto_upload_replays.starting(),
                    egui::Button::new(if running { "Stop" } else { "Start" }),
                )
                .clicked()
            {
                if running {
                    self.auto_upload_replays.stop();
                    self.spoofer_mgr.disable_rlapi();
                } else {
                    self.request_replay_auto_upload_start();
                }
            }
            if self.auto_upload_replays.starting() {
                ui.spinner();
            }
        });
        ui.add_space(8.0);

        let mut config_changed = false;
        ui.horizontal(|ui| {
            ui.label("Provider:");
            egui::ComboBox::from_id_salt("replay_upload_provider")
                .selected_text("ballchasing.com")
                .show_ui(ui, |ui| {
                    config_changed |= ui
                        .selectable_value(
                            &mut self.config.replay_upload.provider,
                            "ballchasing.com".to_string(),
                            "ballchasing.com",
                        )
                        .changed();
                });
        });
        ui.horizontal(|ui| {
            ui.label("API key:");
            config_changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut self.config.replay_upload.api_key)
                        .password(true)
                        .desired_width(360.0),
                )
                .changed();
        });
        ui.horizontal(|ui| {
            ui.label("Replay name:");
            config_changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut self.config.replay_upload.naming_template)
                        .desired_width(520.0),
                )
                .changed();
        });
        ui.label(
            egui::RichText::new(
                "Placeholders: {date}  {time}  {time24}  {winning_team}  {gamemode}  {map}  {match_guid}",
            )
            .small()
            .weak(),
        );
        ui.horizontal(|ui| {
            ui.label("Visibility:");
            egui::ComboBox::from_id_salt("replay_upload_visibility")
                .selected_text(&self.config.replay_upload.visibility)
                .show_ui(ui, |ui| {
                    for visibility in ["private", "unlisted", "public"] {
                        config_changed |= ui
                            .selectable_value(
                                &mut self.config.replay_upload.visibility,
                                visibility.to_string(),
                                visibility,
                            )
                            .changed();
                    }
                });
        });
        ui.horizontal(|ui| {
            ui.label("Group ID (optional):");
            config_changed |= ui
                .text_edit_singleline(&mut self.config.replay_upload.group_id)
                .changed();
        });
        config_changed |= ui
            .checkbox(
                &mut self.config.replay_upload.debug_logging,
                "Debug logging",
            )
            .changed();
        if config_changed {
            self.save_config();
        }
        ui.separator();
        self.auto_upload_replays.show(ui, &session.status());
    }
    fn process_preset_operations(
        &mut self,
        cooked_pc: &std::path::Path,
        backups_dir: &std::path::Path,
        ctx: &egui::Context,
    ) {
        self.patcher_ball.poll_ops(&self.tx, ctx, &mut self.config);
        self.patcher_boost.poll_ops(&self.tx, &mut self.config);
        self.patcher_decal.handle_op_result(&self.tx, &mut self.config);
        if self.patcher_ball.processing_target.is_none()
            && self.patcher_ball.active_ball.is_none()
            && let Some(name) = self.presets.pending_ball.take()
            && let Err(error) = self.patcher_ball.begin_apply_named(
                &name, cooked_pc, backups_dir, &self.tx, ctx,
            )
        {
            self.presets.status.push_str(&format!("\nSkipped ball '{name}': {error}"));
            self.presets.missing.push(crate::presets::MissingItem::Patch {
                kind: "ball".into(), key: None,
            });
        }
        if self.patcher_boost.processing_target.is_none()
            && self.patcher_boost.active_boost.is_none()
            && let Some(name) = self.presets.pending_boost.take()
            && let Err(error) = self.patcher_boost.begin_apply_named(
                &name, cooked_pc, backups_dir, &self.tx, ctx,
            )
        {
            self.presets.status.push_str(&format!("\nSkipped boost '{name}': {error}"));
            self.presets.missing.push(crate::presets::MissingItem::Patch {
                kind: "boost".into(), key: None,
            });
        }
        if self.patcher_decal.processing_target.is_none() && !self.presets.pending_decals.is_empty() {
            let (key, name) = self.presets.pending_decals.remove(0);
            let (car, skin) = key
                .split_once('|')
                .map(|(car, skin)| (car.to_string(), skin.to_string()))
                .unwrap_or_default();
            if let Err(error) = self.patcher_decal.apply_decal_to_skin(
                &name, &car, &skin, 1.0, cooked_pc, backups_dir, &self.tx, ctx,
            ) {
                self.presets.status.push_str(&format!("\nSkipped decal '{name}': {error}"));
                self.presets.missing.push(crate::presets::MissingItem::Patch {
                    kind: "decals".into(), key: Some(key),
                });
            }
        }
    }

    fn apply_selected_preset(
        &mut self,
        cooked_pc: &std::path::Path,
        backups_dir: &std::path::Path,
        ctx: &egui::Context,
    ) {
        if crate::messages::block_item_action_if_game_running(&self.tx) {
            return;
        }
        let Some(preset) = self.presets.presets.get(self.presets.selected).cloned() else { return };
        self.presets.missing.clear();
        self.presets.pending_ball = None;
        self.presets.pending_boost = None;
        self.presets.pending_decals.clear();
        let swap_report = self.swapper.apply_preset_swaps(&preset.swaps, cooked_pc, backups_dir);
        self.presets.missing.extend(
            swap_report.skipped_indexes.iter().copied().map(crate::presets::MissingItem::Swap),
        );
        let mut messages = swap_report.skipped;
        let mut queued = 0usize;
        if preset.include_patches {
            let patches = preset.patches.as_object();
            if let Some(name) = patches.and_then(|p| p.get("ball")).and_then(|v| v.as_str()) {
                if !self.patcher_ball.balls.iter().any(|item| item.name == name) {
                    messages.push(format!("Ball '{name}' is not installed locally"));
                    self.presets.missing.push(crate::presets::MissingItem::Patch { kind: "ball".into(), key: None });
                } else if self.patcher_ball.active_ball.is_some() {
                    self.presets.pending_ball = Some(name.to_string());
                    self.patcher_ball.begin_restore(cooked_pc, backups_dir, &self.tx, ctx);
                    queued += 1;
                } else if let Err(error) = self.patcher_ball.begin_apply_named(name, cooked_pc, backups_dir, &self.tx, ctx) {
                    messages.push(error);
                } else {
                    queued += 1;
                }
            }
            if let Some(name) = patches.and_then(|p| p.get("boost")).and_then(|v| v.as_str()) {
                if !self.patcher_boost.boosts.iter().any(|item| item.name == name) {
                    messages.push(format!("Boost '{name}' is not installed locally"));
                    self.presets.missing.push(crate::presets::MissingItem::Patch { kind: "boost".into(), key: None });
                } else if self.patcher_boost.active_boost.is_some() {
                    self.presets.pending_boost = Some(name.to_string());
                    self.patcher_boost.begin_restore(cooked_pc, backups_dir, &self.tx, ctx);
                    queued += 1;
                } else if let Err(error) = self.patcher_boost.begin_apply_named(name, cooked_pc, backups_dir, &self.tx, ctx) {
                    messages.push(error);
                } else {
                    queued += 1;
                }
            }
            if let Some(decals) = patches.and_then(|p| p.get("decals")).and_then(|v| v.as_object()) {
                for (key, value) in decals {
                    let Some(name) = value.as_str() else { continue };
                    let Some((car, skin)) = key.split_once('|') else {
                        messages.push(format!("Decal '{name}' has an invalid target"));
                        self.presets.missing.push(crate::presets::MissingItem::Patch { kind: "decals".into(), key: Some(key.clone()) });
                        continue;
                    };
                    if self.patcher_decal.preset_item_available(name, car, skin) {
                        self.presets.pending_decals.push((key.clone(), name.into()));
                        queued += 1;
                    } else {
                        messages.push(format!("Decal '{name}' or its target '{key}' is not installed locally"));
                        self.presets.missing.push(crate::presets::MissingItem::Patch { kind: "decals".into(), key: Some(key.clone()) });
                    }
                }
            }
            if let Some(cars) = patches.and_then(|p| p.get("cars")).and_then(|v| v.as_object()) {
                for (target, value) in cars {
                    let Some(name) = value.as_str() else { continue };
                    if let Err(error) = self.patcher_car.apply_preset(
                        name, target, cooked_pc, backups_dir, &self.tx, &mut self.config,
                    ) {
                        messages.push(format!("Car '{name}': {error}"));
                        self.presets.missing.push(crate::presets::MissingItem::Patch { kind: "cars".into(), key: Some(target.clone()) });
                    } else {
                        queued += 1;
                    }
                }
            }
        }
        self.presets.status = format!(
            "Applied {} swap(s); queued/applied {queued} patch item(s).", swap_report.applied
        );
        if !messages.is_empty() {
            self.presets.status.push_str("\nSkipped items:\n- ");
            self.presets.status.push_str(&messages.join("\n- "));
        }
        self.process_preset_operations(cooked_pc, backups_dir, ctx);
    }

    fn render_presets_tab(
        &mut self,
        ui: &mut egui::Ui,
        cooked_pc: &std::path::Path,
        backups_dir: &std::path::Path,
        ctx: &egui::Context,
    ) {
        self.process_preset_operations(cooked_pc, backups_dir, ctx);
        ui.heading(t("presets-presets"));
        ui.label(t("presets-save-and-restore-named-collections-of"));
        ui.horizontal(|ui| {
            ui.label(t("presets-name"));
            ui.text_edit_singleline(&mut self.presets.name_edit);
            ui.checkbox(
                &mut self.presets.include_patches,
                t("presets-include-patches"),
            );
            if ui.button(t("presets-save-preset")).clicked() {
                let swaps = std::fs::read(backups_dir.join("swapper_swaps.json"))
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Vec<serde_json::Value>>(&b).ok())
                    .unwrap_or_default();
                let patches = if self.presets.include_patches {
                    serde_json::json!({
                        "ball": self.patcher_ball.active_ball,
                        "boost": self.patcher_boost.active_boost,
                        "decals": self.patcher_decal.active_decals,
                        "cars": self.patcher_car.active_cars,
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
            if ui.button(t("btn-refresh")).clicked() {
                self.presets.refresh();
            }
            if ui.button(t("presets-import-json")).clicked() {
                if let Some(file) = rfd::FileDialog::new()
                    .add_filter(t("presets-json"), &["json"])
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
        });
        ui.separator();
        if self.presets.presets.is_empty() {
            ui.weak(t("presets-no-presets-saved"));
        } else {
            let names: Vec<String> = self.presets.presets.iter().map(|p| p.name.clone()).collect();
            egui::ComboBox::from_id_salt("preset_select")
                .selected_text(&names[self.presets.selected])
                .show_ui(ui, |ui| {
                    for (index, name) in names.iter().enumerate() {
                        ui.selectable_value(&mut self.presets.selected, index, name);
                    }
                });
            ui.horizontal(|ui| {
                if ui.button("Apply Preset").clicked() {
                    self.apply_selected_preset(cooked_pc, backups_dir, ctx);
                }
                if ui.button("Edit Preset").clicked() {
                    self.presets.begin_edit_selected();
                }
                if ui.button(t("presets-delete")).clicked() {
                    self.presets.delete_selected();
                }
            });
        }
        let editing = self.presets.editing.is_some();
        let mut save_edit = false;
        let mut cancel_edit = false;
        if let Some(draft) = self.presets.editing.as_mut() {
            ui.group(|ui| {
                ui.strong("Edit preset");
                ui.horizontal(|ui| {
                    ui.label(t("presets-name"));
                    ui.text_edit_singleline(&mut draft.name);
                    ui.checkbox(&mut draft.include_patches, t("presets-include-patches"));
                });
                let mut remove_swap = None;
                for (index, swap) in draft.swaps.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let source = swap.get("source_name").and_then(|v| v.as_str()).unwrap_or("?");
                        let target = swap.get("target_name").and_then(|v| v.as_str()).unwrap_or("?");
                        ui.label(source);
                        let arrow =
                            ui.allocate_response(egui::vec2(18.0, 18.0), egui::Sense::hover());
                        let visuals = ui.style().interact(&arrow);
                        let center = arrow.rect.center();
                        ui.painter().line_segment(
                            [
                                center + egui::vec2(-3.5, -3.5),
                                center + egui::vec2(0.0, 0.0),
                            ],
                            visuals.fg_stroke,
                        );
                        ui.painter().line_segment(
                            [
                                center + egui::vec2(0.0, 0.0),
                                center + egui::vec2(-3.5, 3.5),
                            ],
                            visuals.fg_stroke,
                        );
                        ui.label(target);
                        if ui.small_button("Remove").clicked() { remove_swap = Some(index); }
                    });
                }
                if let Some(index) = remove_swap { draft.swaps.remove(index); }
                let mut remove_patch = None;
                if let Some(patches) = draft.patches.as_object() {
                    for (kind, value) in patches {
                        if let Some(entries) = value.as_object() {
                            for (key, item) in entries {
                                ui.horizontal(|ui| {
                                    ui.label(format!("{kind} / {key}: {item}"));
                                    if ui.small_button("Remove").clicked() { remove_patch = Some((kind.clone(), Some(key.clone()))); }
                                });
                            }
                        } else if !value.is_null() {
                            ui.horizontal(|ui| {
                                ui.label(format!("{kind}: {value}"));
                                if ui.small_button("Remove").clicked() { remove_patch = Some((kind.clone(), None)); }
                            });
                        }
                    }
                }
                if let Some((kind, key)) = remove_patch {
                    if let Some(patches) = draft.patches.as_object_mut() {
                        if let Some(key) = key {
                            if let Some(entries) = patches.get_mut(&kind).and_then(|v| v.as_object_mut()) { entries.remove(&key); }
                        } else { patches.remove(&kind); }
                    }
                }
                ui.horizontal(|ui| {
                    if ui.button("Save Changes").clicked() {
                        save_edit = true;
                    }
                    if ui.button("Cancel").clicked() { cancel_edit = true; }
                });
            });
        }
        if save_edit && let Err(error) = self.presets.save_edit() {
            self.presets.status = error;
        }
        if cancel_edit {
            self.presets.cancel_edit();
        }
        if !editing && let Some(preset) = self.presets.presets.get(self.presets.selected) {
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
                ui.horizontal(|ui| {
                    ui.label(source);
                    let arrow = ui.allocate_response(egui::vec2(18.0, 18.0), egui::Sense::hover());
                    let visuals = ui.style().interact(&arrow);
                    let center = arrow.rect.center();
                    ui.painter().line_segment(
                        [
                            center + egui::vec2(-3.5, -3.5),
                            center + egui::vec2(0.0, 0.0),
                        ],
                        visuals.fg_stroke,
                    );
                    ui.painter().line_segment(
                        [
                            center + egui::vec2(0.0, 0.0),
                            center + egui::vec2(-3.5, 3.5),
                        ],
                        visuals.fg_stroke,
                    );
                    ui.label(target);
                });
            }
            if let Some(obj) = preset.patches.as_object() {
                for (kind, value) in obj {
                    if !value.is_null() {
                        ui.label(format!("{kind}: {value}"));
                    }
                }
            }
        }
        if !self.presets.status.is_empty() {
            ui.separator();
            ui.label(&self.presets.status);
        }
        if !self.presets.missing.is_empty() {
            ui.horizontal(|ui| {
                if ui.button("Remove Missing Items from Preset").clicked()
                    && let Err(error) = self.presets.remove_missing_selected()
                { self.presets.status = error; }
                if ui.button("Ignore Missing Items").clicked() {
                    self.presets.missing.clear();
                    self.presets.status.clear();
                }
            });
        }
        ui.separator();
        ui.strong("Preset Key");
        ui.label("Share or paste a base64 preset key. Imported items remain listed even when their local files are missing.");
        ui.add(egui::TextEdit::multiline(&mut self.presets.preset_key).desired_rows(3).desired_width(f32::INFINITY));
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.presets.presets.is_empty(), egui::Button::new("Show Preset Key")).clicked() {
                match self.presets.selected_key() {
                    Ok(key) => self.presets.preset_key = key,
                    Err(error) => self.presets.status = error,
                }
            }
            if ui.button("Import Preset").clicked() {
                let key = self.presets.preset_key.clone();
                match self.presets.import_key(&key) {
                    Ok(()) => self.presets.status = "Preset imported. Missing local items will be skipped when applied.".into(),
                    Err(error) => self.presets.status = error,
                }
            }
        });
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
            if self.item_spawner_enabled {
                self.item_spawner_enabled = false;
                self.spawn_restart_pending = false;
                self.spawner_subtab = SpawnerSubTab::Tutorial;
                let _ = self.spoofer_mgr.set_item_spawner_enabled(false);
                clear_rl_cache(&self.tx);
            }
            if self.spoofer_mgr.socket_running() {
                self.spoofer_mgr.stop_socket();
                clear_rl_cache(&self.tx);
            }
            if self.spoofer_mgr.http_running() {
                self.spoofer_mgr.stop_http();
            }
            if let Err(error) = self.spoofer_mgr.reconcile_hosts() {
                self.console
                    .write(format!("[Spoofer] Hosts cleanup failed: {error}"));
            }
            return;
        }

        let needs_http = self.spoofer_http_proxy
            && (self.spoofer_username_enabled
                || self.spoofer_friends_enabled
                || self.spoofer_rank_enabled
                || self.item_spawner_enabled
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
        let needs_socket = (self.spoofer_socket_proxy && self.spoofer_title_enabled)
            || self.spoofer_rank_enabled
            || self.item_spawner_enabled;
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

        if let Err(error) = self.spoofer_mgr.reconcile_hosts() {
            self.console
                .write(format!("[Spoofer] Hosts reconciliation failed: {error}"));
        }
        self.save_friends_internal();
        self.save_ranks_internal();
    }

    fn prompt_for_missing_spoofer_certificate(&mut self) {
        self.spoofer_cert_installed = spoofer::ca::is_current_installed(&self.base_dir);
        if self.spoofer_cert_installed {
            self.spoofer_certificate_prompt_open = false;
        } else if self.spoofer_master && (self.spoofer_http_proxy || self.spoofer_socket_proxy) {
            self.spoofer_certificate_prompt_open = true;
        }
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
            // Show/Hide is an explicit request to surface Hebnix.  In
            // particular, don't leave it behind Rocket League just because
            // the game was focused when the hotkey was pressed (including in
            // exclusive fullscreen).
            if rocket_league_had_focus || !winutil::foreground_window_is_ours() {
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

    fn force_quit(&mut self, ctx: &egui::Context) {
        self.quitting = true;
        self.config.window.width = self.last_size.0;
        self.config.window.height = self.last_size.1;
        self.save_config();

        if crate::watchdog::handoff_live_spoofer(Arc::clone(&self.spoofer_mgr)) {
            self.plugin_mgr.unload_all();
            self.stats.stop();
            self.ws_stats.stop();
            self.monitor.stop();
            self.discord_presence.stop();
            self.tray = None;
            // Keep the proxy alive without leaving a transparent viewport on screen.
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        if self.item_spawner_enabled {
            self.disable_item_spawner();
        }
        self.spoofer_mgr.shutdown();
        self.plugin_mgr.unload_all();
        self.stats.stop();
        self.ws_stats.stop();
        self.monitor.stop();
        self.discord_presence.stop();
        self.tray = None;
        // std::process::exit() terminates immediately without running Rust
        // destructors, so `impl Drop for HebnixApp` (which would otherwise
        // clean up the WinDivert capture handle and bring the tailnet down)
        // never runs on this path. Windows does force-close the process's
        // handles on exit regardless, but that's exactly what left the
        // WinDivert driver in a corrupted, file-locked state before - it
        // needs the cooperative shutdown (WinDivertShutdown then close) to
        // unload cleanly, not just have its handle yanked. Do that
        // explicitly here rather than relying on process teardown to do it.
        self.workshop.suspend_multiplayer();
        std::process::exit(0);
    }

    fn handle_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                AppMsg::Log(s) => self.console.write(s),
                AppMsg::RlApiCaptureReady(result) => {
                    self.rlapi_panel.starting = false;
                    if let Err(error) = result {
                        self.rlapi_panel.complete(Err(error));
                    }
                }
                AppMsg::RlApiResponse(result) => self.rlapi_panel.complete(result),
                AppMsg::ReplayUploadCaptureReady(result) => {
                    let capture_ready = result.is_ok();
                    self.auto_upload_replays.finish_start(&result);
                    if self.replay_upload_restart_after_capture {
                        self.replay_upload_restart_after_capture = false;
                        if capture_ready {
                            let path = self.config.settings.rl_path.clone();
                            if let Err(error) = crate::winutil::restart_rocket_league(
                                &self.config.rl_launch,
                                std::path::Path::new(&path),
                            ) {
                                self.auto_upload_replays.stop();
                                self.spoofer_mgr.disable_rlapi();
                                self.replay_upload_start_prompt = Some(ReplayUploadStartPrompt {
                                    issues: vec![format!(
                                        "RLAPI capture was enabled, but Rocket League could not be restarted: {error}"
                                    )],
                                    restart_required: false,
                                    needs_admin: false,
                                    needs_certificate: false,
                                    missing_api_key: false,
                                });
                            }
                        }
                    }
                }
                AppMsg::ReplayUploadFinished(result) => {
                    let debug = self.config.replay_upload.debug_logging;
                    let message = if debug {
                        Some(match &result.result {
                            Ok(entry) if entry.duplicate => format!(
                                "[Replay Upload] match {} already exists at {}",
                                entry.match_guid, entry.location
                            ),
                            Ok(entry) => format!(
                                "[Replay Upload] uploaded match {} to {}",
                                entry.match_guid, entry.location
                            ),
                            Err(error) => format!(
                                "[Replay Upload] match {} failed: {error}",
                                result.match_guid
                            ),
                        })
                    } else {
                        None
                    };
                    self.auto_upload_replays.finish_upload(result);
                    if let Some(message) = message {
                        self.console.write(message);
                    }
                }
                AppMsg::GameEvent(event) => self.handle_game_event(event, ctx),
                AppMsg::RlStatus {
                    rl_open,
                    api_open,
                    root_dir,
                } => {
                    self.spoofer_mgr.cleanup_idle_rlapi();
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
                        self.spawn_restart_pending = false;
                        self.workshop.rocket_league_reopened();
                        self.check_statsapi_rate();
                        self.check_web_port();
                    }
                    if rl_state_changed || platform_changed {
                        self.workshop.refresh_launch_status(&self.tx, ctx);
                    }
                    self.workshop.tick_shutdown_grace();
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
                                let update_version = update
                                    .get("version")
                                    .and_then(|value| value.as_str())
                                    .unwrap_or("unknown");
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
                                    self.plugin_mgr.set_enabled(&slug, false, &mut self.config);
                                    self.save_config();

                                    let plugin_dir = self.plugin_dir.clone();
                                    let id_str = plugin_id.to_string();
                                    let name_str = name.to_string();
                                    let version_str = update_version.to_string();
                                    let slug_str = slug.clone();
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
                                            let extract = install_plugin_update(
                                                &temp_zip,
                                                &plugin_dir,
                                                &slug_str,
                                                &id_str,
                                            );
                                            let _ = std::fs::remove_file(&temp_zip);
                                            extract?;
                                            Ok(format!(
                                                "Successfully updated {name_str} to {version_str}."
                                            ))
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
                        self.plugin_mgr
                            .reload_updated_plugin(&slug, was_enabled, &mut self.config);
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
                AppMsg::ItemActionBlocked => {
                    self.item_action_prompt_open = true;
                }
                AppMsg::ReloadCatalogs => {
                    self.reload_catalogs(ctx);
                }
                AppMsg::CatalogsFetched { result } => {
                    self.catalogs_loading = false;
                    let result =
                        result.and_then(|catalogs| self.apply_downloaded_catalogs(catalogs));
                    match result {
                        Ok(()) => {
                            self.catalogs_loaded = true;
                            self.catalogs_error = None;
                        }
                        Err(error) => {
                            self.catalogs_error = Some(error.clone());
                            self.console.write(format!("[Catalogs] {error}"));
                        }
                    }
                }
                AppMsg::WorkshopCatalog(result) => match result {
                    Ok(items) => {
                        self.workshop.set_catalog(items);
                        if self.workshop.valid.is_empty() {
                            self.workshop.catalog_status =
                                t("handle-messages-no-maps-found").to_string();
                        }
                    }
                    Err(e) => {
                        self.workshop.merge_local_maps();
                        self.workshop.catalog_status =
                            t("handle-messages-failed-to-load-maps").to_string();
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
                AppMsg::BackgroundChangerDone(result) => {
                    let message = self.workshop.finish_background_changer(result);
                    self.console.write(message);
                }
                AppMsg::BackgroundChangerProgress(message) => {
                    let message = self.workshop.update_background_changer_progress(message);
                    self.console.write(message);
                }
                AppMsg::WorkshopMultiplayerProgress(status) => {
                    self.workshop.set_multiplayer_progress(status);
                }
                AppMsg::WorkshopTailnetStarted { result } => {
                    self.workshop.finish_tailnet_started(result);
                }
                AppMsg::WorkshopMultiplayerLaunched { result } => {
                    self.workshop.finish_multiplayer_launch(result);
                }
                AppMsg::WorkshopRelayStarted { result } => {
                    self.workshop.finish_relay_started(result);
                }
                AppMsg::WorkshopPlayerUpdated { result } => {
                    self.workshop.finish_player_update(result);
                }
                AppMsg::WorkshopHostSessionCheck { result } => {
                    self.workshop.finish_host_session_check(result);
                }
                AppMsg::WorkshopLaunchCheck {
                    rl_open,
                    launch_ready,
                } => {
                    self.workshop.finish_launch_check(rl_open, launch_ready);
                    if self.workshop.retry_multihome_check() {
                        self.workshop.refresh_launch_status(&self.tx, ctx);
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
                                self.install_modal.error = Some(
                                    t("handle-messages-invalid-api-response-format").to_string(),
                                );
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
                        Ok((plugin_id, message)) => {
                            match self
                                .plugin_mgr
                                .enable_installed_plugin(&plugin_id, &mut self.config)
                            {
                                Ok(()) => self.console.write(format!("[Console] {message}")),
                                Err(error) => self.console.write(format!(
                                    "[Console] Plugin was installed but could not be enabled: {error}"
                                )),
                            }
                            if !self.install_modal.hebnix_stage {
                                self.install_modal = InstallModal::default();
                            }
                            self.save_config();
                        }
                        Err(e) => {
                            self.console.write(format!("[Console] Install failed: {e}"));
                        }
                    }
                }
                AppMsg::ThemeInstallDone { result } => match result {
                    Ok((name, author)) => {
                        self.theme_options = theme::list_themes(&self.themes_dir);
                        self.console
                            .write(format!("[Console] Installed Theme {name} by {author}"));
                    }
                    Err(error) => self
                        .console
                        .write(format!("[Console] Theme installation failed: {error}")),
                },
                AppMsg::OverlayPost { slug, data } => {
                    if let Some(webview) = &self.webview {
                        if let Err(error) = webview.deliver(&slug, data) {
                            tracing::debug!("overlay.send from '{slug}' dropped: {error}");
                        }
                    }
                }
                AppMsg::Toast {
                    slug,
                    name,
                    text,
                    style,
                } => self.toasts.push(&slug, &name, &text, style),
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
                AppMsg::PluginHttpResult {
                    slug,
                    req_id,
                    status,
                    body,
                    headers,
                } => {
                    self.plugin_mgr
                        .on_http_result(&slug, &req_id, status, &body, &headers);
                    ctx.request_repaint();
                }
                AppMsg::PluginWsOpen { slug, id } => {
                    self.plugin_mgr.on_ws_open(&slug, &id);
                    ctx.request_repaint();
                }
                AppMsg::PluginWsMessage { slug, id, data } => {
                    self.plugin_mgr.on_ws_message(&slug, &id, &data);
                    ctx.request_repaint();
                }
                AppMsg::PluginWsClose { slug, id, reason } => {
                    self.plugin_mgr.on_ws_close(&slug, &id, &reason);
                    ctx.request_repaint();
                }
                AppMsg::TsnetUpResult { result } => {
                    if self.workshop.multiplayer_connection_requested() {
                        match result {
                            Ok(ip) => {
                                self.console
                                    .write("[Multiplayer] Connected to the Hebnix Network");
                                self.workshop.set_tailnet_ip(ip);
                                self.workshop.refresh_launch_status(&self.tx, ctx);
                            }
                            Err(error) => {
                                self.console.write(format!(
                                    "[Multiplayer] Could not connect: {}",
                                    crate::multiplayer_lan::redact(&error)
                                ));
                                self.workshop.tailnet_failed(error);
                            }
                        }
                    }
                }
                // not otherwise consumed yet -- peer-level status/events only
                // drive the console for now. See the tsnet-multiplayer rework
                // plan's beacon-relay/firewall-scoping notes for what would
                // consume a live peer list if this UI panel grows one later.
                AppMsg::TsnetStatus {
                    state,
                    tailnet_ip: _,
                    peers,
                } => {
                    self.console.write(format!(
                        "[Multiplayer] Network status: {state:?}; peers={}",
                        peers.len()
                    ));
                }
                AppMsg::TsnetPeerEvent {
                    online,
                    tailnet_ip: _,
                } => {
                    let word = if online { "online" } else { "offline" };
                    self.console.write(format!("[Multiplayer] Peer {word}"));
                }
                AppMsg::TsnetDownResult { ok } => {
                    if ok {
                        self.console
                            .write("[Multiplayer] Disconnected from the Hebnix Network");
                    } else {
                        self.console.write("[Multiplayer] Disconnect failed");
                    }
                }
                AppMsg::TsnetSidecarDisconnected => {
                    self.console
                        .write("[tsnet] lost connection to the multiplayer helper".to_string());
                }
            }
        }
    }

    fn handle_game_event(&mut self, event: hebnix_sdk::stats::StatsEvent, ctx: &egui::Context) {
        match event.event_type.as_str() {
            "UpdateState" => {
                let entered_match = !self.in_match && !self.match_ended;
                if !self.match_ended {
                    self.in_match = true;
                    self.plugin_mgr.shared.borrow_mut().in_match = true;
                }
                if let Some(state) = event.update_state() {
                    self.workshop
                        .update_workshop_map_from_stats(&state.game.arena, &self.tx);
                    if entered_match {
                        let log = hebnix_sdk::log::parse_launch_log(None, true, "INT");
                        if let Some(match_guid) = event.match_guid.as_deref() {
                            self.auto_upload_replays
                                .track_match(match_guid, log.session.primary_id.clone());
                        }
                        self.discord_match = Some(crate::discord_presence::MatchInfo::from_state(
                            state,
                            log.game.as_ref(),
                        ));
                    } else if let Some(info) = self.discord_match.as_mut() {
                        info.update_state(state);
                    }
                    if let Some(info) = self.discord_match.as_ref() {
                        self.discord_presence.set_match(&self.config.settings, info);
                    }
                }
                self.plugin_mgr.dispatch_game_event(&event);
            }
            "MatchEnded" => {
                if let Some(message) = self.auto_upload_replays.submit_finished(
                    event.match_guid.as_deref(),
                    &self.config.replay_upload,
                    self.tx.clone(),
                    ctx.clone(),
                ) {
                    self.console.write(message);
                }
                self.in_match = false;
                self.match_ended = true;
                self.plugin_mgr.shared.borrow_mut().in_match = false;
                self.refresh_discord_presence();
                self.plugin_mgr.dispatch_game_event(&event);
            }
            "MatchDestroyed" => {
                if let Some(message) = self.auto_upload_replays.submit_finished(
                    event.match_guid.as_deref(),
                    &self.config.replay_upload,
                    self.tx.clone(),
                    ctx.clone(),
                ) {
                    self.console.write(message);
                }
                self.in_match = false;
                self.match_ended = false;
                self.plugin_mgr.shared.borrow_mut().in_match = false;
                self.discord_match = None;
                self.refresh_discord_presence();
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
        if !self.in_match && !self.match_ended {
            self.discord_presence
                .set_idle(&self.config.settings, rl_open);
        }
        self.first_status = false;
    }

    fn refresh_discord_presence(&self) {
        if (self.in_match || self.match_ended)
            && let Some(info) = self.discord_match.as_ref()
        {
            self.discord_presence.set_match(&self.config.settings, info);
        } else {
            self.discord_presence
                .set_idle(&self.config.settings, self.last_rl_open);
        }
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
        self.patcher_car.active_cars = self.config.patcher.active_cars.clone();
        self.patcher_ball.source = self.config.patcher.ball_source;
        self.patcher_boost.source = self.config.patcher.boost_source;
        self.patcher_decal.source = self.config.patcher.decal_source;
        self.patcher_car.source = self.config.patcher.car_source;
        self.patcher_ball.refresh_balls();
        self.patcher_boost.refresh_boosts();
        self.patcher_decal.refresh_decals();
        self.patcher_car.refresh_cars();
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
                    .write("  cvar <name>          - gets a registered plugin cvar");
                self.console
                    .write("  cvar <name> <value>  - sets a cvar (quote string values)");
                self.console
                    .write("  quit                 - force kills the Rocket League process");
                self.console
                    .write("  restart              - restarts Rocket League through Steam or Epic");
            }
            "restart" => {
                let path = self.config.settings.rl_path.clone();
                match crate::winutil::restart_rocket_league(
                    &self.config.rl_launch,
                    std::path::Path::new(&path),
                ) {
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
                            t("execute-command-rocket-league-is-not-running")
                        } else if !log_info.stats_api_available {
                            t("execute-command-can-t-read-the-match-the")
                        } else {
                            t("execute-command-not-in-a-game")
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
            "cvar" => self
                .console
                .write(self.plugin_mgr.execute_cvar_command(&raw)),
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
                        let status = if p.enabled {
                            t("execute-command-enabled")
                        } else {
                            t("execute-command-disabled")
                        };
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
                self.console.write(format!("[Console] Command '{raw}' unrecognized. Options: help, info, plugins list, plugin, cvar, clear, quit, restart"));
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
            .default_size(200.0)
            .size_range(200.0..=320.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("spoofer_settings_names")
                    .show(ui, |ui| {
                        ui.selectable_value(
                            &mut self.spoofer_subtab,
                            SpooferSubTab::Settings,
                            t("spoofer-spoofer-settings"),
                        );
                        ui.add_enabled_ui(self.spoofer_master, |ui| {
                            ui.selectable_value(
                                &mut self.spoofer_subtab,
                                SpooferSubTab::Username,
                                t("spoofer-name-title"),
                            );
                            ui.selectable_value(
                                &mut self.spoofer_subtab,
                                SpooferSubTab::TitleRank,
                                t("spoofer-rank"),
                            );
                            ui.selectable_value(
                                &mut self.spoofer_subtab,
                                SpooferSubTab::Friends,
                                t("spoofer-friends"),
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
                            ui.heading(t("spoofer-spoofer-settings"));
                            ui.add_space(8.0);

                            if ui
                                .checkbox(&mut self.spoofer_master, t("spoofer-enable-spoofer"))
                                .changed()
                            {
                                if self.spoofer_master && !spoofer::is_admin() {
                                    self.admin_prompt_open = true;
                                    self.spoofer_master = false;
                                } else {
                                    self.prompt_for_missing_spoofer_certificate();
                                    self.save_config();
                                    self.evaluate_proxies();
                                }
                            }
                            ui.label(
                                egui::RichText::new(t("spoofer-requires-admin-for-hosts-file-intercepti"))
                                    .color(egui::Color32::GRAY),
                            );

                            ui.add_space(10.0);
                            ui.add_enabled_ui(self.spoofer_master, |ui| {
                                ui.horizontal(|ui| {
                                    if ui
                                        .checkbox(&mut self.spoofer_http_proxy, t("spoofer-account-proxy"))
                                        .changed()
                                    {
                                        self.prompt_for_missing_spoofer_certificate();
                                        self.save_config();
                                        self.evaluate_proxies();
                                    }
                                    if self.spoofer_mgr.http_running() {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(34, 245, 101),
                                            t("spoofer-running"),
                                        );
                                    } else {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(244, 113, 116),
                                            t("spoofer-stopped"),
                                        );
                                    }
                                });
                                ui.label(
                                    egui::RichText::new(t("spoofer-for-username-friends-and-rank-spoofing"))
                                        .color(egui::Color32::GRAY),
                                );

                                ui.add_space(8.0);

                                ui.horizontal(|ui| {
                                    if ui
                                        .checkbox(&mut self.spoofer_socket_proxy, t("spoofer-psynet-proxy"))
                                        .changed()
                                    {
                                        self.prompt_for_missing_spoofer_certificate();
                                        self.save_config();
                                        self.evaluate_proxies();
                                    }
                                    if self.spoofer_mgr.socket_running() {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(34, 245, 101),
                                            t("spoofer-running"),
                                        );
                                    } else {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(244, 113, 116),
                                            t("spoofer-stopped"),
                                        );
                                    }
                                });
                                ui.label(
                                    egui::RichText::new(t("spoofer-for-title-and-rank-spoofing"))
                                        .color(egui::Color32::GRAY),
                                );
                            });

                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                ui.label(format!("{} ", t("spoofer-certificate-status")));
                                if self.spoofer_cert_installed {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(34, 245, 101),
                                        t("spoofer-installed"),
                                    );
                                    if ui.button(t("spoofer-remove")).clicked() {
                                        match spoofer::ca::uninstall() {
                                            Ok(()) => {
                                                self.console
                                                    .write("[Spoofer] Certificate removed.");
                                                self.spoofer_cert_installed =
                                                    spoofer::ca::is_current_installed(
                                                        &self.base_dir,
                                                    );
                                                self.prompt_for_missing_spoofer_certificate();
                                            }
                                            Err(e) => self.console.write(format!(
                                                "[Spoofer] Certificate removal failed: {e}"
                                            )),
                                        }
                                    }
                                } else {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(244, 113, 116),
                                        t("spoofer-missing"),
                                    );
                                    if ui.button(t("spoofer-install-certificate")).clicked() {
                                        match spoofer::ca::install(&self.base_dir) {
                                            Ok(()) => {
                                                self.console
                                                    .write("[Spoofer] Certificate installed.");
                                                self.spoofer_cert_installed =
                                                    spoofer::ca::is_current_installed(
                                                        &self.base_dir,
                                                    );
                                                self.prompt_for_missing_spoofer_certificate();
                                            }
                                            Err(e) => self.console.write(format!(
                                                "[Spoofer] Certificate install failed: {e}"
                                            )),
                                        }
                                    }
                                    if ui.button("↻").on_hover_text(t("spoofer-refresh-status")).clicked() {
                                        self.spoofer_cert_installed =
                                            spoofer::ca::is_current_installed(&self.base_dir);
                                        self.prompt_for_missing_spoofer_certificate();
                                    }
                                }
                            });
                        }
                        SpooferSubTab::Username => {
                            ui.heading(t("spoofer-name-title-spoofing"));
                            ui.add_space(8.0);

                            ui.add_enabled_ui(self.spoofer_http_proxy, |ui| {
                                ui.horizontal(|ui| {
                                    if ui
                                        .checkbox(
                                            &mut self.spoofer_username_enabled,
                                            t("spoofer-enable-username-spoof"),
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
                                            .on_hover_text(t("spoofer-use-recent-spoofed-name"))
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
                                        .checkbox(&mut self.spoofer_title_enabled, t("spoofer-titles"))
                                        .changed()
                                    {
                                        if !self.spoofer_title_enabled {
                                            match winutil::clear_rocket_league_web_cache() {
                                                Ok(()) => self.console.write(
                                                    "[Spoofer] Cleared Rocket League WebCache.",
                                                ),
                                                Err(error) => self.console.write(format!(
                                                    "[Spoofer] Could not clear Rocket League WebCache: {error}"
                                                )),
                                            }
                                        }
                                        title_changed = true;
                                        self.evaluate_proxies();
                                    }
                                });
                                ui.add_enabled_ui(self.spoofer_title_enabled, |ui| {
                                    ui.horizontal(|ui| {
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
                                                t("spoofer-glow"),
                                            ),
                                        )
                                        .changed()
                                    {
                                        title_changed = true;
                                    }

                                    });
                                    ui.horizontal(|ui| {
                                    ui.label(t("spoofer-title-to-replace"));
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
                                        .unwrap_or_else(|| egui::RichText::new(t("spoofer-all")));
                                    egui::ComboBox::from_id_salt("title_spoof_target")
                                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                                        .width(210.0)
                                        .height(320.0)
                                        .selected_text(selected_title)
                                        .show_ui(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.label(t("spoofer-filter"));
                                                ui.add(
                                                    egui::TextEdit::singleline(
                                                        &mut self.spoofer_title_filter,
                                                    )
                                                    .hint_text(t("spoofer-search-titles"))
                                                    .desired_width(145.0),
                                                );
                                                if ui.small_button(t("spoofer-clear")).clicked() {
                                                    self.spoofer_title_filter.clear();
                                                }
                                            });
                                            ui.separator();
                                            if ui
                                                .selectable_value(
                                                    &mut self.spoofer_title_target,
                                                    None,
                                                    t("spoofer-all"),
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
                                    ui.label(t("spoofer-copy-title"));
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
                                            egui::RichText::new(t("spoofer-choose-a-title"))
                                        });
                                    egui::ComboBox::from_id_salt("title_spoof_copy")
                                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                                        .width(250.0)
                                        .height(320.0)
                                        .selected_text(copy_label)
                                        .show_ui(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.label(t("spoofer-filter"));
                                                ui.add(
                                                    egui::TextEdit::singleline(
                                                        &mut self.spoofer_title_copy_filter,
                                                    )
                                                    .hint_text(t("spoofer-search-titles"))
                                                    .desired_width(145.0),
                                                );
                                                if ui.small_button(t("spoofer-clear")).clicked() {
                                                    self.spoofer_title_copy_filter.clear();
                                                }
                                            });
                                            ui.separator();
                                            ui.selectable_value(
                                                &mut self.spoofer_title_copy,
                                                None,
                                                t("spoofer-choose-a-title"),
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
                                ui.horizontal_wrapped(|ui| {
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
                                            egui::Button::new(t("about-hebnix")),
                                        )
                                        .clicked()
                                    {
                                        self.spoofer_title = "Hebnix".to_string();
                                        title_changed = true;
                                    }
                                });

                                ui.horizontal(|ui| {
                                    let target_already_used = self
                                        .spoofer_titles
                                        .iter()
                                        .any(|title| title.target_id == self.spoofer_title_target);
                                    let target_is_available = !target_already_used;
                                    let can_add = self.spoofer_title_enabled
                                        && !self.spoofer_title.trim().is_empty()
                                        && target_is_available;
                                    let add_response = ui.add_enabled(
                                        can_add,
                                        egui::Button::new(t("spoofer-add-title-to-list")),
                                    );
                                    if add_response.clicked() {
                                        self.spoofer_titles.push(TitleSpoofSettings {
                                            text: self
                                                .spoofer_title
                                                .trim()
                                                .chars()
                                                .take(64)
                                                .collect(),
                                            color: format!(
                                                "{:02X}{:02X}{:02X}",
                                                self.spoofer_title_color[0],
                                                self.spoofer_title_color[1],
                                                self.spoofer_title_color[2]
                                            ),
                                            glow: self.spoofer_title_glow,
                                            target_id: self.spoofer_title_target.clone(),
                                        });
                                        self.spoofer_title.clear();
                                        self.spoofer_title_copy = None;
                                        title_changed = true;
                                    } else if self.spoofer_title_enabled
                                        && !self.spoofer_title.trim().is_empty()
                                        && !target_is_available
                                    {
                                        add_response.on_disabled_hover_text(
                                            t("spoofer-this-title-is-already-being-replaced"),
                                        );
                                    }
                                });
                                ui.add_space(6.0);
                                ui.label(egui::RichText::new(t("spoofer-enabled-titles")).strong());
                                if self.spoofer_titles.is_empty() {
                                    ui.weak(t("spoofer-no-titles-added"));
                                } else {
                                    let mut remove_title = None;
                                    for (index, title) in self.spoofer_titles.iter().enumerate() {
                                        ui.horizontal(|ui| {
                                            let color = parse_hex_color(&title.color);
                                            ui.label(
                                                egui::RichText::new(&title.text)
                                                    .color(Color32::from_rgb(
                                                        color[0], color[1], color[2],
                                                    )),
                                            );
                                            if title.glow {
                                                ui.weak(t("spoofer-glow"));
                                            }
                                            ui.weak(t("spoofer-replaces"));
                                            let target = title
                                                .target_id
                                                .as_ref()
                                                .and_then(|id| {
                                                    self.title_catalog
                                                        .iter()
                                                        .find(|catalog_title| &catalog_title.id == id)
                                                })
                                                .map(|catalog_title| catalog_title.text.as_str())
                                                .unwrap_or("All");
                                            ui.label(target);
                                            if ui
                                                .small_button("x")
                                                .on_hover_text(t("spoofer-remove-this-title"))
                                                .clicked()
                                            {
                                                remove_title = Some(index);
                                            }
                                        });
                                    }
                                    if let Some(index) = remove_title {
                                        self.spoofer_titles.remove(index);
                                        title_changed = true;
                                    }
                                }
                                });
                                if title_changed {
                                    self.spoofer_mgr.set_titles(self.spoofer_titles.clone());
                                    self.spoofer_mgr
                                        .set_title_enabled(self.spoofer_title_enabled);
                                    self.save_config();
                                }
                            });
                        }
                        SpooferSubTab::Friends => {
                            ui.heading(t("spoofer-friend-spoofing"));
                            ui.add_space(8.0);

                            ui.add_enabled_ui(self.spoofer_http_proxy, |ui| {
                                ui.horizontal(|ui| {
                                    if ui
                                        .checkbox(
                                            &mut self.spoofer_friends_enabled,
                                            t("spoofer-enable-friend-name-spoof"),
                                        )
                                        .changed()
                                    {
                                        self.save_friends();
                                        self.evaluate_proxies();
                                    }
                                });
                                ui.add_space(8.0);

                                ui.horizontal(|ui| {
                                    ui.label(t("spoofer-search"));
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.friends_search)
                                            .desired_width(150.0),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.button(t("spoofer-revert-all")).clicked() {
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
                                            t("spoofer-start-rocket-league-to-populate-friends"),
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
                                                        .hint_text(t("spoofer-spoofed-name")),
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
                            ui.heading(t("spoofer-rank-spoofing"));
                            ui.add_space(8.0);

                            ui.add_enabled_ui(self.spoofer_http_proxy, |ui| {
                                if ui
                                    .checkbox(
                                        &mut self.spoofer_rank_enabled,
                                        t("spoofer-enable-rank-spoofing"),
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
                                                            .hint_text(t("spoofer-mmr")),
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
        egui::CollapsingHeader::new(t_args(
            "overlay-order-overlay-order-rows-layers",
            &[("rows", (rows.len()).to_string().into())],
        ))
        .id_salt("overlay_order")
        .show(ui, |ui| {
            ui.label(t("overlay-order-drag-to-reorder-renders-based-on"));
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
            if ui.button(t("action-open-plugins-folder")).clicked() {
                let _ = open::that(&self.plugin_dir);
            }
            if ui.button(t("plugins-install-plugin")).clicked() {
                self.install_modal = InstallModal {
                    open: true,
                    ..Default::default()
                };
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(t("plugins-reload"))
                            .fill(egui::Color32::from_rgb(0xd3, 0x54, 0x00)),
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
                                        if ui
                                            .add(egui::Button::new(
                                                egui::RichText::new("🗑").color(
                                                    egui::Color32::from_rgb(0xe7, 0x4c, 0x3c),
                                                ),
                                            ))
                                            .on_hover_text(t("plugins-delete-plugin"))
                                            .clicked()
                                        {
                                            self.plugin_delete_prompt = Some(plugin.slug.clone());
                                        }
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
                        ui.label(t("plugins-no-plugins-installed-drop-a-plugin"));
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

    fn execute_action_button_action(&mut self, action: ActionButtonAction, ctx: &egui::Context) {
        match action {
            ActionButtonAction::StartRocketLeague | ActionButtonAction::RestartRocketLeague => {
                let path = self.config.settings.rl_path.clone();
                if !self.config.settings.rl_path_confirmed
                    || path.trim().is_empty()
                    || !std::path::Path::new(&path).is_dir()
                {
                    self.launch_path_notice = true;
                    return;
                }

                let tx = self.tx.clone();
                let rl_launch = self.config.rl_launch.clone();
                std::thread::spawn(move || {
                    let (verb, result) = match action {
                        ActionButtonAction::StartRocketLeague => (
                            "start",
                            crate::winutil::start_rocket_league(
                                &rl_launch,
                                std::path::Path::new(&path),
                            ),
                        ),
                        ActionButtonAction::RestartRocketLeague => (
                            "restart",
                            crate::winutil::restart_rocket_league(
                                &rl_launch,
                                std::path::Path::new(&path),
                            ),
                        ),
                        _ => unreachable!(),
                    };
                    let message = match result {
                        Ok(()) => format!("[Core] Rocket League {verb} requested."),
                        Err(error) => format!("[Core] Rocket League {verb} failed: {error}"),
                    };
                    let _ = tx.send(AppMsg::Log(message));
                });
            }
            ActionButtonAction::CloseRocketLeague => {
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let message = match crate::winutil::kill_rocket_league() {
                        Ok(()) => "[Core] Rocket League close requested.".to_string(),
                        Err(error) => format!("[Core] Rocket League close failed: {error}"),
                    };
                    let _ = tx.send(AppMsg::Log(message));
                });
            }
            ActionButtonAction::OpenHebnixFolder => {
                if let Err(error) = open::that(&self.base_dir) {
                    self.console
                        .write(format!("[Core] Could not open Hebnix folder: {error}"));
                }
            }
            ActionButtonAction::OpenPluginsFolder => {
                if let Err(error) = open::that(&self.plugin_dir) {
                    self.console
                        .write(format!("[Core] Could not open Plugins folder: {error}"));
                }
            }
            ActionButtonAction::ReloadPlugins => {
                self.plugin_mgr.reload_all(&mut self.config);
                self.save_config();
            }
            ActionButtonAction::FixEpicConnection => {
                self.epic_repair.begin(ctx);
                if self.epic_repair.running {
                    self.spoofer_mgr.shutdown();
                }
            }
        }
    }

    fn render_action_button(&mut self, ui: &mut egui::Ui) {
        let entries = if self.last_rl_open {
            &self.config.action_button.rocket_league_open
        } else {
            &self.config.action_button.rocket_league_closed
        };
        let actions: Vec<ActionButtonAction> = entries
            .iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.action)
            .collect();
        let Some((&primary, secondary)) = actions.split_first() else {
            return;
        };

        if secondary.is_empty() {
            if ui.button(primary.label()).clicked() {
                self.execute_action_button_action(primary, ui.ctx());
            }
            return;
        }

        let mut run_primary = false;
        let mut selected = None;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.horizontal(|ui| {
                let arrow = ui.add(
                    egui::Button::new("")
                        .min_size(egui::vec2(24.0, 0.0))
                        .corner_radius(egui::CornerRadius {
                            nw: 0,
                            ne: 3,
                            sw: 0,
                            se: 3,
                        }),
                );
                let main = ui.add(egui::Button::new(primary.label()).corner_radius(
                    egui::CornerRadius {
                        nw: 3,
                        ne: 0,
                        sw: 3,
                        se: 0,
                    },
                ));
                run_primary = main.clicked();
                let arrow_visuals = ui.style().interact(&arrow);
                let center = arrow.rect.center();
                ui.painter().line_segment(
                    [
                        center + egui::vec2(-3.5, -1.5),
                        center + egui::vec2(0.0, 2.0),
                    ],
                    arrow_visuals.fg_stroke,
                );
                ui.painter().line_segment(
                    [
                        center + egui::vec2(0.0, 2.0),
                        center + egui::vec2(3.5, -1.5),
                    ],
                    arrow_visuals.fg_stroke,
                );

                let combined_rect = main.rect.union(arrow.rect);
                let popup_anchor = ui.interact(
                    combined_rect,
                    ui.make_persistent_id("action_button_menu_anchor"),
                    egui::Sense::hover(),
                );
                egui::Popup::menu(&popup_anchor)
                    .open_memory(arrow.clicked().then_some(egui::SetOpenCommand::Toggle))
                    .width(combined_rect.width())
                    .show(|ui| {
                        for &action in secondary {
                            if ui.button(action.label()).clicked() {
                                selected = Some(action);
                                ui.close();
                            }
                        }
                    });
            });
        });
        if run_primary {
            self.execute_action_button_action(primary, ui.ctx());
        } else if let Some(action) = selected {
            self.execute_action_button_action(action, ui.ctx());
        }
    }

    fn render_action_button_state_editor(
        ui: &mut egui::Ui,
        id: &'static str,
        entries: &mut Vec<ActionButtonEntry>,
    ) -> bool {
        let enabled_count = entries.iter().filter(|entry| entry.enabled).count();
        let mut moved = None;
        let mut changed = false;

        for (index, entry) in entries.iter_mut().enumerate() {
            let row_id = egui::Id::new((id, entry.action));
            let (_, dropped) = ui.dnd_drop_zone::<usize, _>(
                egui::Frame::new().inner_margin(egui::Margin::symmetric(4, 2)),
                |ui| {
                    ui.horizontal(|ui| {
                        ui.weak(format!("{}.", index + 1));
                        ui.dnd_drag_source(row_id, index, |ui| {
                            ui.weak("::");
                        });
                        let can_toggle = !entry.enabled || enabled_count > 1;
                        if ui
                            .add_enabled(
                                can_toggle,
                                egui::Checkbox::new(&mut entry.enabled, entry.action.label()),
                            )
                            .on_disabled_hover_text(t("action-keep-one"))
                            .changed()
                        {
                            changed = true;
                        }
                    });
                },
            );
            if let Some(from) = dropped {
                moved = Some((*from, index));
            }
        }

        if let Some((from, to)) = moved
            && from != to
            && from < entries.len()
        {
            let entry = entries.remove(from);
            entries.insert(to.min(entries.len()), entry);
            changed = true;
        }
        changed
    }

    fn render_action_button_settings(&mut self, ui: &mut egui::Ui) {
        ui.label(t("action-settings-intro"));
        ui.weak(t("action-settings-hint"));
        ui.add_space(12.0);

        ui.strong(t("action-state-closed"));
        let closed_changed = Self::render_action_button_state_editor(
            ui,
            "action_button_closed",
            &mut self.config.action_button.rocket_league_closed,
        );
        ui.add_space(12.0);
        ui.strong(t("action-state-open"));
        let open_changed = Self::render_action_button_state_editor(
            ui,
            "action_button_open",
            &mut self.config.action_button.rocket_league_open,
        );
        if closed_changed || open_changed {
            self.save_config();
        }
    }

    fn render_settings_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.settings_subtab,
                SettingsSubTab::Hebnix,
                t("settings-subtab-hebnix"),
            );
            ui.selectable_value(
                &mut self.settings_subtab,
                SettingsSubTab::Plugin,
                t("settings-subtab-plugin"),
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
            // min_size not exact_size so longer translated names can widen it
            .default_size(200.0)
            .size_range(200.0..=320.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("hebnix_settings_names")
                    .show(ui, |ui| {
                        ui.selectable_value(
                            &mut self.hebnix_settings_tab,
                            HebnixSettingsTab::Interface,
                            t("settings-nav-interface"),
                        );
                        ui.selectable_value(
                            &mut self.hebnix_settings_tab,
                            HebnixSettingsTab::Directories,
                            t("settings-nav-directories"),
                        );
                        ui.selectable_value(
                            &mut self.hebnix_settings_tab,
                            HebnixSettingsTab::System,
                            t("settings-nav-system"),
                        );
                        ui.selectable_value(
                            &mut self.hebnix_settings_tab,
                            HebnixSettingsTab::Discord,
                            t("settings-nav-discord"),
                        );
                        ui.selectable_value(
                            &mut self.hebnix_settings_tab,
                            HebnixSettingsTab::ActionButton,
                            t("settings-nav-action-button"),
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
                            HebnixSettingsTab::Interface => t("settings-heading-interface"),
                            HebnixSettingsTab::Directories => t("settings-heading-directories"),
                            HebnixSettingsTab::Discord => t("settings-heading-discord"),
                            HebnixSettingsTab::ActionButton => t("settings-heading-action-button"),
                            HebnixSettingsTab::System => t("settings-heading-system"),
                        });
                        ui.add_space(8.0);

                        match self.hebnix_settings_tab {
                            HebnixSettingsTab::Interface => {
                                let label_w = label_column_width(
                                    ui,
                                    &[
                                        t("settings-keybind-label"),
                                        t("settings-language-label"),
                                        t("settings-theme-label"),
                                        t("settings-opacity-label"),
                                        t("settings-toast-position-label"),
                                    ],
                                );
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("settings-keybind-label")),
                                    );
                                    let display = if self.capturing_hotkey {
                                        "...".to_string()
                                    } else {
                                        self.config.settings.hotkey.to_uppercase()
                                    };
                                    ui.add_enabled(
                                        false,
                                        egui::TextEdit::singleline(&mut display.clone())
                                            .desired_width(120.0),
                                    );
                                    let btn_text = if self.capturing_hotkey {
                                        t("settings-keybind-listening")
                                    } else {
                                        t("settings-keybind-set")
                                    };
                                    if ui
                                        .add_enabled(
                                            !self.capturing_hotkey,
                                            egui::Button::new(btn_text),
                                        )
                                        .clicked()
                                    {
                                        self.start_hotkey_capture(&ctx);
                                    }
                                });

                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("settings-language-label")),
                                    );
                                    let mut chosen = self.config.settings.language.clone();
                                    let mut language_changed = false;
                                    let known = crate::i18n::available();
                                    let shown = if chosen.eq_ignore_ascii_case(crate::i18n::AUTO) {
                                        t("settings-language-auto")
                                    } else {
                                        known
                                            .iter()
                                            .find(|l| l.code.eq_ignore_ascii_case(&chosen))
                                            .map(crate::i18n::display_name)
                                            .unwrap_or_else(|| chosen.clone())
                                    };
                                    egui::ComboBox::from_id_salt("language_select")
                                        .selected_text(shown)
                                        .show_ui(ui, |ui| {
                                            language_changed |= ui
                                                .selectable_value(
                                                    &mut chosen,
                                                    crate::i18n::AUTO.to_string(),
                                                    t("settings-language-auto"),
                                                )
                                                .changed();
                                            for locale in &known {
                                                language_changed |= ui
                                                    .selectable_value(
                                                        &mut chosen,
                                                        locale.code.clone(),
                                                        crate::i18n::display_name(locale),
                                                    )
                                                    .changed();
                                            }
                                        });
                                    if language_changed {
                                        self.change_language(&ctx, &chosen);
                                    }
                                });

                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("settings-theme-label")),
                                    );
                                    let mut selected = self.config.settings.theme.clone();
                                    let mut changed = false;
                                    egui::ComboBox::from_id_salt("theme_select")
                                        .selected_text(selected.clone())
                                        .width(120.0)
                                        .show_ui(ui, |ui| {
                                            for option in self.theme_options.clone() {
                                                if ui
                                                    .selectable_value(
                                                        &mut selected,
                                                        option.clone(),
                                                        option,
                                                    )
                                                    .changed()
                                                {
                                                    changed = true;
                                                }
                                            }
                                        });
                                    if changed {
                                        self.change_theme(&ctx, &selected);
                                    }
                                    if ui.button(t("btn-refresh")).clicked() {
                                        self.theme_options = theme::list_themes(&self.themes_dir);
                                        self.console.write(format!(
                                            "[Console] {}",
                                            t("console-themes-rescanned")
                                        ));
                                    }
                                    if ui.button(t("btn-open-folder")).clicked() {
                                        let _ = open::that(&self.themes_dir);
                                    }
                                    if ui.button(t("settings-open-fonts-folder")).clicked() {
                                        let _ = open::that(&self.fonts_dir);
                                    }
                                });

                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("settings-opacity-label")),
                                    );
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

                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("settings-toast-position-label")),
                                    );
                                    let mut changed = false;
                                    egui::ComboBox::from_id_salt("toast_position_select")
                                        .selected_text(self.config.settings.toast_position.label())
                                        .show_ui(ui, |ui| {
                                            for pos in crate::toast::ToastPos::ALL {
                                                changed |= ui
                                                    .selectable_value(
                                                        &mut self.config.settings.toast_position,
                                                        pos,
                                                        pos.label(),
                                                    )
                                                    .changed();
                                            }
                                        });
                                    if changed {
                                        self.save_config();
                                    }
                                });
                            }

                            HebnixSettingsTab::Directories => {
                                ui.label(
                                    egui::RichText::new(t("settings-dirs-autodetected"))
                                        .size(11.0)
                                        .color(egui::Color32::GRAY),
                                );
                                ui.add_space(8.0);

                                // ini key names are technical, they stay untranslated
                                let label_w = label_column_width(
                                    ui,
                                    &[
                                        t("settings-rl-folder-label"),
                                        t("settings-statsapi-ini-label"),
                                        "PacketSendRate:".to_string(),
                                        "Port:".to_string(),
                                        "WebPort:".to_string(),
                                    ],
                                );
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("settings-rl-folder-label")),
                                    );
                                    ui.add_enabled(
                                        false,
                                        egui::TextEdit::singleline(
                                            &mut self.config.settings.rl_path,
                                        )
                                        .desired_width(480.0),
                                    );
                                    if ui.button(t("btn-browse")).clicked() {
                                        if let Some(dir) = rfd::FileDialog::new()
                                            .set_directory(&self.config.settings.rl_path)
                                            .pick_folder()
                                        {
                                            self.config.settings.rl_path =
                                                dir.to_string_lossy().to_string();
                                            self.remember_patcher_state(
                                                &self.config.settings.rl_path,
                                            );
                                            self.save_config();
                                            self.refresh_stats_api_viewer();
                                        }
                                    }
                                });

                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("settings-statsapi-ini-label")),
                                    );
                                    ui.add_enabled(
                                        false,
                                        egui::TextEdit::singleline(
                                            &mut self.config.settings.statsapi_path,
                                        )
                                        .desired_width(480.0),
                                    );
                                    if ui.button(t("btn-browse")).clicked() {
                                        let start_dir = std::path::Path::new(
                                            &self.config.settings.statsapi_path,
                                        )
                                        .parent()
                                        .map(|p| p.to_path_buf())
                                        .unwrap_or_default();
                                        if let Some(file) = rfd::FileDialog::new()
                                            .set_directory(start_dir)
                                            .add_filter(t("filter-ini-files"), &["ini"])
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
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("hebnix-settings-packetsendrate")),
                                    );
                                    let resp = ui.add(
                                        egui::TextEdit::singleline(&mut self.packet_rate_edit)
                                            .hint_text(t("settings-not-found"))
                                            .desired_width(100.0),
                                    );
                                    if resp.changed() {
                                        self.packet_rate_edit.retain(|c| c.is_ascii_digit());
                                    }
                                    if resp.lost_focus() {
                                        let current = self.packet_rate.clone().unwrap_or_default();
                                        if !self.packet_rate_edit.is_empty()
                                            && self.packet_rate_edit != current
                                        {
                                            let value = self.packet_rate_edit.clone();
                                            self.update_ini_setting("PacketSendRate", &value);
                                        } else {
                                            self.packet_rate_edit = current;
                                        }
                                    }
                                    if self.packet_rate.as_deref() != Some("20")
                                        && ui
                                            .add(
                                                egui::Button::new(t_args(
                                                    "settings-set-value",
                                                    &[("value", "20".into())],
                                                ))
                                                .fill(egui::Color32::from_rgb(0xd3, 0x54, 0x00)),
                                            )
                                            .clicked()
                                    {
                                        self.update_ini_setting("PacketSendRate", "20");
                                    }
                                });

                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("hebnix-settings-port")),
                                    );
                                    let resp = ui.add(
                                        egui::TextEdit::singleline(&mut self.port_edit)
                                            .hint_text(t("settings-not-found"))
                                            .desired_width(100.0),
                                    );
                                    if resp.changed() {
                                        self.port_edit.retain(|c| c.is_ascii_digit());
                                    }
                                    if resp.lost_focus() {
                                        let current = self.port_value.clone().unwrap_or_default();
                                        let valid = self
                                            .port_edit
                                            .parse::<u16>()
                                            .map(|p| p > 0)
                                            .unwrap_or(false);
                                        if valid && self.port_edit != current {
                                            let value = self.port_edit.clone();
                                            self.update_ini_setting("Port", &value);
                                        } else {
                                            self.port_edit = current;
                                        }
                                    }
                                    if self.port_value.as_deref() != Some("49123")
                                        && ui
                                            .add(
                                                egui::Button::new(t("btn-reset")).fill(
                                                    egui::Color32::from_rgb(0xc0, 0x39, 0x2b),
                                                ),
                                            )
                                            .clicked()
                                    {
                                        self.update_ini_setting("Port", "49123");
                                    }
                                    if ui.button(t("btn-refresh")).clicked() {
                                        self.refresh_stats_api_viewer();
                                    }
                                });

                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("hebnix-settings-webport")),
                                    );
                                    let resp = ui.add(
                                        egui::TextEdit::singleline(&mut self.web_port_edit)
                                            .hint_text(t("settings-not-found"))
                                            .desired_width(100.0),
                                    );
                                    if resp.changed() {
                                        self.web_port_edit.retain(|c| c.is_ascii_digit());
                                    }
                                    if resp.lost_focus() {
                                        let current =
                                            self.web_port_value.clone().unwrap_or_default();
                                        let valid = self
                                            .web_port_edit
                                            .parse::<u16>()
                                            .map(|p| p > 0)
                                            .unwrap_or(false);
                                        if valid && self.web_port_edit != current {
                                            let value = self.web_port_edit.clone();
                                            self.update_ini_setting("WebPort", &value);
                                        } else {
                                            self.web_port_edit = current;
                                        }
                                    }
                                    if self.web_port_value.as_deref() != Some("49124")
                                        && ui
                                            .add(
                                                egui::Button::new(t_args(
                                                    "settings-set-value",
                                                    &[("value", "49124".into())],
                                                ))
                                                .fill(egui::Color32::from_rgb(0xd3, 0x54, 0x00)),
                                            )
                                            .clicked()
                                    {
                                        self.update_ini_setting("WebPort", "49124");
                                    }
                                });

                                ui.label(
                                    egui::RichText::new(t("settings-ini-restart-note"))
                                        .size(11.0)
                                        .color(egui::Color32::GRAY),
                                );

                                ui.add_space(12.0);
                                ui.separator();
                                ui.add_space(4.0);
                                ui.strong(t("settings-rl-launch-heading"));
                                ui.label(
                                    egui::RichText::new(t("settings-rl-launch-desc"))
                                        .size(11.0)
                                        .color(egui::Color32::GRAY),
                                );
                                let mode_label = match self.config.rl_launch.mode {
                                    crate::config::RlLaunchMode::Unconfigured => {
                                        t("settings-rl-mode-unconfigured")
                                    }
                                    crate::config::RlLaunchMode::SteamNative => {
                                        t("settings-rl-mode-steam-native")
                                    }
                                    crate::config::RlLaunchMode::EpicDirect => {
                                        t("settings-rl-mode-epic-direct")
                                    }
                                    crate::config::RlLaunchMode::SteamShortcutToHeroic => {
                                        t("settings-rl-mode-steam-shortcut")
                                    }
                                    crate::config::RlLaunchMode::HeroicDirect => {
                                        t("settings-rl-mode-heroic-direct")
                                    }
                                };
                                ui.label(t_args(
                                    "settings-rl-current-setup",
                                    &[("mode", mode_label.as_str().into())],
                                ));
                                if ui.button(t("settings-rl-launch-setup-button")).clicked() {
                                    self.rl_launch_draft = self.config.rl_launch.clone();
                                    self.rl_launch_shortcut_candidates.clear();
                                    self.rl_launch_setup_open = true;
                                }
                            }

                            HebnixSettingsTab::Discord => {
                                let mut changed = false;
                                if ui
                                    .checkbox(
                                        &mut self.config.settings.discord_rich_presence,
                                        t("discord-enable"),
                                    )
                                    .changed()
                                {
                                    self.discord_presence
                                        .configure(self.config.settings.discord_rich_presence);
                                    changed = true;
                                }
                                if ui
                                    .checkbox(
                                        &mut self.config.settings.discord_rocket_league_only,
                                        t("discord-rl-only"),
                                    )
                                    .changed()
                                {
                                    changed = true;
                                }
                                ui.add_space(8.0);
                                ui.label(t("discord-message-label"));
                                let mut game_state = self.config.settings.discord_game_state;
                                if ui
                                    .checkbox(&mut game_state, t("discord-game-state"))
                                    .changed()
                                {
                                    self.config.settings.discord_game_state = game_state;
                                    changed = true;
                                }

                                ui.indent("discord_game_state_fields", |ui| {
                                    ui.add_enabled_ui(
                                        self.config.settings.discord_game_state,
                                        |ui| {
                                            let selected = self.config.settings.discord_show_score
                                                as u8
                                                + self.config.settings.discord_show_map as u8
                                                + self.config.settings.discord_show_gamemode as u8;
                                            if ui
                                                .add_enabled(
                                                    !self.config.settings.discord_show_score
                                                        || selected > 1,
                                                    egui::Checkbox::new(
                                                        &mut self
                                                            .config
                                                            .settings
                                                            .discord_show_score,
                                                        t("discord-show-score"),
                                                    ),
                                                )
                                                .changed()
                                            {
                                                changed = true;
                                            }
                                            let selected = self.config.settings.discord_show_score
                                                as u8
                                                + self.config.settings.discord_show_map as u8
                                                + self.config.settings.discord_show_gamemode as u8;
                                            if ui
                                                .add_enabled(
                                                    !self.config.settings.discord_show_map
                                                        || selected > 1,
                                                    egui::Checkbox::new(
                                                        &mut self.config.settings.discord_show_map,
                                                        t("discord-show-map"),
                                                    ),
                                                )
                                                .changed()
                                            {
                                                changed = true;
                                            }
                                            let selected = self.config.settings.discord_show_score
                                                as u8
                                                + self.config.settings.discord_show_map as u8
                                                + self.config.settings.discord_show_gamemode as u8;
                                            if ui
                                                .add_enabled(
                                                    !self.config.settings.discord_show_gamemode
                                                        || selected > 1,
                                                    egui::Checkbox::new(
                                                        &mut self
                                                            .config
                                                            .settings
                                                            .discord_show_gamemode,
                                                        t("discord-show-gamemode"),
                                                    ),
                                                )
                                                .changed()
                                            {
                                                changed = true;
                                            }
                                        },
                                    );
                                });

                                ui.horizontal(|ui| {
                                    let mut custom = !self.config.settings.discord_game_state;
                                    if ui.checkbox(&mut custom, t("discord-custom")).changed() {
                                        self.config.settings.discord_game_state = !custom;
                                        changed = true;
                                    }
                                    if ui
                                        .add_enabled(
                                            custom,
                                            egui::TextEdit::singleline(
                                                &mut self.config.settings.discord_custom_message,
                                            )
                                            .hint_text(t("discord-custom-hint"))
                                            .desired_width(280.0),
                                        )
                                        .changed()
                                    {
                                        changed = true;
                                    }
                                });
                                if self.config.settings.discord_game_state {
                                    ui.label(
                                        egui::RichText::new(t("discord-custom-disabled-note"))
                                            .size(11.0)
                                            .color(egui::Color32::GRAY),
                                    );
                                }
                                if changed {
                                    self.save_config();
                                    self.refresh_discord_presence();
                                }
                                let ctx = ui.ctx().clone();
                                self.discord_link.show(ui, &ctx);
                            }

                            HebnixSettingsTab::ActionButton => {
                                self.render_action_button_settings(ui);
                            }

                            HebnixSettingsTab::System => {
                                let label_w = label_column_width(
                                    ui,
                                    &[
                                        t("system-start-with-windows"),
                                        t("system-start-in-tray"),
                                        t("system-close-to-tray"),
                                        t("system-p2p"),
                                        t("system-suppress-left"),
                                        t("system-fullscreen-warning"),
                                        t("system-allow-draw-focus"),
                                        t("system-limit-hotkey"),
                                        t("system-statsapi-rate"),
                                        t("system-default-tab"),
                                    ],
                                );
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("system-start-with-windows")),
                                    );
                                    if ui.checkbox(&mut self.startup_enabled, "").changed() {
                                        if let Err(e) =
                                            winutil::set_startup_enabled(self.startup_enabled)
                                        {
                                            self.console.write(format!(
                                                "[Console] {}",
                                                t_args(
                                                    "console-startup-failed",
                                                    &[("error", e.to_string().into())]
                                                )
                                            ));
                                            self.startup_enabled = winutil::is_startup_enabled();
                                        }
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("system-start-in-tray")),
                                    );
                                    if ui
                                        .checkbox(&mut self.config.settings.start_in_tray, "")
                                        .changed()
                                    {
                                        self.save_config();
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("system-close-to-tray")),
                                    );
                                    if ui
                                        .checkbox(&mut self.config.settings.close_to_tray, "")
                                        .changed()
                                    {
                                        self.save_config();
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("system-p2p")),
                                    );
                                    if ui
                                        .checkbox(&mut self.config.settings.p2p_file_sharing, "")
                                        .changed()
                                    {
                                        self.workshop
                                            .set_share_files(self.config.settings.p2p_file_sharing);
                                        self.save_config();
                                    }
                                });
                                ui.label(
                                    egui::RichText::new(t("system-p2p-note"))
                                        .size(11.0)
                                        .color(egui::Color32::GRAY),
                                );
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("system-suppress-left")),
                                    );
                                    if ui
                                        .checkbox(
                                            &mut self.config.settings.suppress_left_alerts,
                                            "",
                                        )
                                        .changed()
                                    {
                                        self.save_config();
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("system-fullscreen-warning")),
                                    );
                                    let mut show =
                                        !self.config.settings.suppress_fullscreen_warning;
                                    if ui.checkbox(&mut show, "").changed() {
                                        self.config.settings.suppress_fullscreen_warning = !show;
                                        self.fullscreen_notice_dismissed = false;
                                        self.fullscreen_notice = show
                                            && self.window_mode == Some(WindowMode::Fullscreen);
                                        self.save_config();
                                    }
                                });
                                ui.label(
                                    egui::RichText::new(t("system-fullscreen-note"))
                                        .size(11.0)
                                        .color(egui::Color32::GRAY),
                                );
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("system-allow-draw-focus")),
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
                                        [label_w, 20.0],
                                        egui::Label::new(t("system-limit-hotkey")),
                                    );
                                    if ui
                                        .checkbox(
                                            &mut self
                                                .config
                                                .settings
                                                .restrict_hotkey_to_hebnix_or_rocket_league,
                                            "",
                                        )
                                        .changed()
                                    {
                                        self.save_config();
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("system-statsapi-rate")),
                                    );
                                    let mut show =
                                        !self.config.settings.suppress_statsapi_rate_warning;
                                    if ui.checkbox(&mut show, "").changed() {
                                        self.config.settings.suppress_statsapi_rate_warning = !show;
                                        self.save_config();
                                    }
                                });
                                ui.label(
                                    egui::RichText::new(t("system-statsapi-note"))
                                        .size(11.0)
                                        .color(egui::Color32::GRAY),
                                );
                                ui.add_space(8.0);
                                if ui
                                    .add_enabled(
                                        !self.epic_repair.running,
                                        egui::Button::new(t("action-fix-epic-connection")),
                                    )
                                    .clicked()
                                {
                                    self.epic_repair.begin(ui.ctx());
                                    if self.epic_repair.running {
                                        self.spoofer_mgr.shutdown();
                                    }
                                }
                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [label_w, 20.0],
                                        egui::Label::new(t("system-default-tab")),
                                    );
                                    let mut selected =
                                        Tab::from_config_key(&self.config.settings.default_tab);
                                    egui::ComboBox::from_id_salt("default_tab")
                                        .selected_text(selected.label())
                                        .show_ui(ui, |ui| {
                                            for tab in Tab::ALL {
                                                ui.selectable_value(
                                                    &mut selected,
                                                    tab,
                                                    tab.label(),
                                                );
                                            }
                                        });
                                    if self.config.settings.default_tab != selected.config_key() {
                                        self.config.settings.default_tab =
                                            selected.config_key().to_string();
                                        self.save_config();
                                    }
                                });
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
                self.console.write(format!(
                    "[Console] {}",
                    t_args("console-theme-changed", &[("theme", choice.into())])
                ));
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
                    "[Console] {}",
                    t_args("console-theme-failed", &[("theme", choice.into())])
                ));
            }
        }
        theme::apply_window_opacity(ctx, self.config.settings.window_opacity);
    }

    /// switch the UI language right away and remember it
    fn change_language(&mut self, ctx: &egui::Context, choice: &str) {
        crate::i18n::set_language(choice);
        self.config.settings.language = choice.to_string();
        self.save_config();

        // glyph fallback fonts depend on the language, so rebuild the fonts
        let theme_name = self.config.settings.theme.clone();
        let _ = theme::apply_theme(ctx, &self.themes_dir, &self.fonts_dir, &theme_name);
        theme::apply_window_opacity(ctx, self.config.settings.window_opacity);

        if let Some(tray) = &self.tray {
            tray.refresh_labels(self.hidden);
        }
        ctx.request_repaint();
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
                ui.label(t("plugin-settings-none"));
                ui.add_space(8.0);
                if ui.button(t("plugin-settings-go")).clicked() {
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
            .default_size(200.0)
            .size_range(200.0..=320.0)
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
                        ui.heading(t_args(
                            "plugin-settings-display-name-configuration",
                            &[("display_name", display_name.to_string().into())],
                        ));
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
            ui.heading(t("about-hebnix"));
            ui.add_space(10.0);
            ui.label(t_args(
                "about-version-app-version-a-safe-eac",
                &[
                    ("version", APP_VERSION.to_string().into()),
                    (
                        "hotkey",
                        (self.config.settings.hotkey.to_uppercase())
                            .to_string()
                            .into(),
                    ),
                ],
            ));
            ui.separator();
            ui.label(t("about-built-with-help-from-the-community"));
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
        egui::Window::new(t("statsapi-notice-statsapi-configuration"))
            .id(egui::Id::new("StatsAPI configuration"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(body);
                if let Some(err) = &self.statsapi_apply_error {
                    ui.add_space(6.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(0xe7, 0x4c, 0x3c),
                        t_args(
                            "statsapi-notice-couldn-t-write-the-file-err",
                            &[("err", err.to_string().into())],
                        ),
                    );
                    ui.label(
                        egui::RichText::new(t("statsapi-notice-edit-it-by-hand-or-restart"))
                            .size(11.0)
                            .color(egui::Color32::GRAY),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(t("statsapi-notice-set-20")).clicked() {
                        apply = true;
                    }
                    let dismiss_label = if blocking {
                        t("statsapi-notice-quit-hebnix")
                    } else {
                        t("statsapi-notice-proceed")
                    };
                    if ui.button(dismiss_label).clicked() {
                        dismiss = true;
                    }
                });
                if !blocking {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(t("statsapi-notice-turn-this-off-in-settings-system"))
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

        egui::Window::new(t("web-port-notice-webport-configuration"))
            .id(egui::Id::new("WebPort configuration"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(body);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(t("web-port-notice-set-49124")).clicked() {
                        apply = true;
                    }
                    if ui.button(t("web-port-notice-dismiss")).clicked() {
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

    fn disable_item_spawner(&mut self) {
        self.item_spawner_enabled = false;
        self.spawn_restart_pending = false;
        self.spawner_subtab = SpawnerSubTab::Tutorial;
        let _ = self.spoofer_mgr.set_item_spawner_enabled(false);
        self.evaluate_proxies();
        clear_rl_cache(&self.tx);
        if self.spoofer_mgr.http_running() || self.spoofer_mgr.socket_running() {
            self.console.write(
                "[Item Spawner] Disabled. Hosts redirects remain for other enabled Spoofer features."
            );
        } else {
            match crate::spoofer::hosts::clear() {
                Ok(()) => {
                    crate::spoofer::hosts::flush_dns();
                    self.console.write(
                        "[Item Spawner] Disabled. Hebnix hosts redirects removed and DNS flushed.",
                    );
                }
                Err(error) => self.console.write(format!(
                    "[Item Spawner] Disabled, but hosts cleanup failed: {error}"
                )),
            }
        }
    }

    fn enable_item_spawner(&mut self) {
        if hebnix_sdk::process::is_rocket_league_running() {
            self.console
                .write("[Item Spawner] Close Rocket League before enabling Item Spawner.");
            return;
        }
        if !spoofer::is_admin() {
            self.spawner_admin_requested = true;
            self.admin_prompt_open = true;
            return;
        }
        self.spoofer_master = true;
        self.save_config();
        match self.spoofer_mgr.set_item_spawner_enabled(true) {
            Ok(()) => {
                self.item_spawner_enabled = true;
                self.evaluate_proxies();
                if self.spoofer_mgr.socket_running() {
                    self.spawn_restart_pending = true;
                    self.console
                        .write("[Item Spawner] Enabled. Start Rocket League to use Item Spawner.");
                } else {
                    self.disable_item_spawner();
                    self.console.write("[Item Spawner] Could not start the PsyNet proxy. Check the Spoofer settings and certificate.");
                }
            }
            Err(error) => {
                self.item_spawner_enabled = false;
                self.console
                    .write(format!("[Item Spawner] Could not enable: {error}"));
            }
        }
    }

    fn render_spawner_enable_prompt(&mut self, ctx: &egui::Context) {
        if !self.spawner_enable_prompt_open {
            return;
        }
        let mut enable = false;
        let mut cancel = false;
        egui::Window::new(t("spawner-enable-prompt-read-the-item-spawner-tutorial"))
            .id(egui::Id::new(
                "spawner-enable-prompt-read-the-item-spawner-tutorial",
            ))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(t(
                    "spawner-enable-prompt-read-the-tutorial-tab-before-enabling",
                ));
                ui.label(t(
                    "spawner-enable-prompt-rocket-league-must-be-closed-first",
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(t("spawner-enable-prompt-continue")).clicked() {
                        enable = true;
                    }
                    if ui.button(t("spawner-enable-prompt-cancel")).clicked() {
                        cancel = true;
                    }
                });
            });
        if enable {
            self.spawner_enable_prompt_open = false;
            self.enable_item_spawner();
        } else if cancel {
            self.spawner_enable_prompt_open = false;
        }
    }

    fn render_admin_prompt(&mut self, ctx: &egui::Context) {
        if !self.admin_prompt_open {
            return;
        }
        let mut ok = false;
        let mut cancel = false;

        egui::Window::new(t("admin-prompt-administrator-required"))
            .id(egui::Id::new("admin-prompt-administrator-required"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(if self.spawner_admin_requested {
                    t("admin-prompt-item-spawner-needs-administrator-access")
                } else if self.owned_admin_requested {
                    t("admin-prompt-owned-filter-needs-proxy")
                } else {
                    t("admin-prompt-this-action-requires-hebnix-to-be")
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(t("btn-ok")).clicked() {
                        ok = true;
                    }
                    if ui.button(t("spawner-enable-prompt-cancel")).clicked() {
                        cancel = true;
                    }
                });
            });

        if ok {
            self.admin_prompt_open = false;
            self.spoofer_master = true;
            if self.spawner_admin_requested {
                let _ = std::fs::write(self.base_dir.join("enable_item_spawner.pending"), b"1");
            }
            if self.owned_admin_requested {
                let _ = std::fs::write(
                    self.base_dir.join("enable_owned_replacements.pending"),
                    b"1",
                );
            }
            self.save_config();
            if spoofer::spawn_elevated_relaunch() {
                self.spoofer_mgr.shutdown();
                self.workshop.suspend_multiplayer();
                std::process::exit(0);
            }
            self.spoofer_master = false;
            let _ = std::fs::remove_file(self.base_dir.join("enable_item_spawner.pending"));
            self.spawner_admin_requested = false;
            self.disable_item_spawner();
            self.save_config();
            self.console.write("[Spoofer] Couldn't relaunch as admin.");
        } else if cancel {
            self.admin_prompt_open = false;
            self.owned_admin_requested = false;
            self.spawner_admin_requested = false;
            self.disable_item_spawner();
            self.console
                .write("[Item Spawner] Disabled because Administrator access was declined.");
        }
    }

    fn render_replay_upload_start_prompt(&mut self, ctx: &egui::Context) {
        let Some(prompt) = self.replay_upload_start_prompt.as_ref() else {
            return;
        };
        let can_enable_and_restart = prompt.restart_required
            && !prompt.needs_admin
            && !prompt.needs_certificate
            && !prompt.missing_api_key;
        let needs_admin = prompt.needs_admin;
        let needs_certificate = prompt.needs_certificate;
        let issues = prompt.issues.clone();
        let mut enable_and_restart = false;
        let mut relaunch_admin = false;
        let mut open_spoofer = false;
        let mut close = false;

        egui::Window::new("Auto Upload Replays requirements")
            .id(egui::Id::new("replay-upload-start-requirements"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("Auto Upload Replays cannot start yet:");
                ui.add_space(4.0);
                for issue in &issues {
                    ui.label(format!("• {issue}"));
                }
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "The PsyNet capture proxy is enabled automatically; the HTTP Proxy toggle is not required.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if can_enable_and_restart
                        && ui.button("Enable and restart Rocket League").clicked()
                    {
                        enable_and_restart = true;
                    }
                    if needs_admin && ui.button("Restart Hebnix as administrator").clicked() {
                        relaunch_admin = true;
                    }
                    if needs_certificate && ui.button("Open Spoofer settings").clicked() {
                        open_spoofer = true;
                    }
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });

        if enable_and_restart {
            self.replay_upload_start_prompt = None;
            self.replay_upload_restart_after_capture = true;
            self.enable_replay_auto_upload();
        } else if relaunch_admin {
            self.replay_upload_start_prompt = None;
            if spoofer::spawn_elevated_relaunch() {
                self.spoofer_mgr.shutdown();
                self.workshop.suspend_multiplayer();
                std::process::exit(0);
            }
        } else if open_spoofer {
            self.replay_upload_start_prompt = None;
            self.tab = Tab::Spoofer;
            self.spoofer_subtab = SpooferSubTab::Settings;
        } else if close {
            self.replay_upload_start_prompt = None;
        }
    }

    fn render_colour_admin_prompt(&mut self, ctx: &egui::Context) {
        if !self.colour_admin_prompt_open {
            return;
        }
        let mut relaunch = false;
        let mut cancel = false;
        egui::Window::new(t("admin-prompt-administrator-required"))
            .id(egui::Id::new("admin-prompt-administrator-required"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(t(
                    "colour-admin-prompt-windows-denied-write-access-to-tagame",
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(t("colour-admin-prompt-restart-as-administrator"))
                        .clicked()
                    {
                        relaunch = true;
                    }
                    if ui.button(t("spawner-enable-prompt-cancel")).clicked() {
                        cancel = true;
                    }
                });
            });
        if relaunch {
            self.colour_admin_prompt_open = false;
            if spoofer::spawn_elevated_relaunch() {
                self.spoofer_mgr.shutdown();
                self.workshop.suspend_multiplayer();
                std::process::exit(0);
            }
            self.console
                .write("[Colours] Hebnix could not relaunch as Administrator.");
        } else if cancel {
            self.colour_admin_prompt_open = false;
        }
    }

    fn render_plugin_delete_prompt(&mut self, ctx: &egui::Context) {
        let Some(slug) = self.plugin_delete_prompt.clone() else {
            return;
        };
        let plugin_name = self
            .plugin_mgr
            .plugins
            .iter()
            .find(|plugin| plugin.slug == slug)
            .map(|plugin| plugin.display_name().to_string())
            .unwrap_or(slug.clone());
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new(t("plugin-delete-prompt-delete-plugin"))
            .id(egui::Id::new("plugin-delete-prompt-delete-plugin"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(t_args(
                    "plugin-delete-prompt-are-you-sure-you-want-to",
                    &[("plugin_name", plugin_name.to_string().into())],
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(t("plugin-delete-prompt-yes")).clicked() {
                        confirm = true;
                    }
                    if ui.button(t("plugin-delete-prompt-no")).clicked() {
                        cancel = true;
                    }
                });
            });
        if confirm {
            self.plugin_delete_prompt = None;
            match self.plugin_mgr.delete_plugin(&slug, &mut self.config) {
                Ok(()) => self.console.write(format!("[Plugins] Deleted {slug}.")),
                Err(error) => self.console.write(format!("[Plugins] {error}")),
            }
            self.save_config();
        } else if cancel {
            self.plugin_delete_prompt = None;
        }
    }
    fn render_item_action_prompt(&mut self, ctx: &egui::Context) {
        if !self.item_action_prompt_open {
            return;
        }
        let mut quit_rocket_league = false;
        let mut close = false;

        egui::Window::new(t("item-action-prompt-rocket-league-is-open"))
            .id(egui::Id::new("item-action-prompt-rocket-league-is-open"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(t("item-action-prompt-rocket-league-must-be-closed-to"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(t("item-action-prompt-quit-rocket-league"))
                        .clicked()
                    {
                        quit_rocket_league = true;
                    }
                    if ui.button(t("item-action-prompt-close-prompt")).clicked() {
                        close = true;
                    }
                });
            });

        if quit_rocket_league {
            self.item_action_prompt_open = false;
            match winutil::kill_rocket_league() {
                Ok(()) if !hebnix_sdk::process::is_rocket_league_running() => {
                    self.console
                        .write("[Patcher] Rocket League was closed. Try the action again.");
                    self.item_action_prompt_open = false;
                }
                Ok(()) => self
                    .console
                    .write("[Patcher] Rocket League is still running. Close it and try again."),
                Err(error) => self
                    .console
                    .write(format!("[Patcher] Could not close Rocket League: {error}")),
            }
        } else if close {
            self.item_action_prompt_open = false;
        }
    }

    fn render_owned_proxy_prompt(&mut self, ctx: &egui::Context) {
        if !self.owned_proxy_prompt_open {
            return;
        }
        let mut enable = false;
        let mut cancel = false;
        egui::Window::new(t("owned-proxy-prompt-enable-owned-item-catalog"))
            .id(egui::Id::new(
                "owned-proxy-prompt-enable-owned-item-catalog",
            ))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(t("owned-proxy-prompt-hebnix-needs-its-local-proxy-to"));
                if !self.spoofer_cert_installed {
                    ui.add_space(4.0);
                    ui.label(t(
                        "owned-proxy-prompt-the-hebnix-proxy-certificate-will-also",
                    ));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(t("owned-proxy-prompt-enable-proxy")).clicked() {
                        enable = true;
                    }
                    if ui.button(t("spawner-enable-prompt-cancel")).clicked() {
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

    fn render_spoofer_certificate_prompt(&mut self, ctx: &egui::Context) {
        if !self.spoofer_certificate_prompt_open {
            return;
        }

        let mut install = false;
        let mut close = false;
        egui::Window::new(t("spoofer-certificate-required-title"))
            .id(egui::Id::new("spoofer-certificate-required"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(t("spoofer-certificate-required-message"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(t("spoofer-install-certificate")).clicked() {
                        install = true;
                    }
                    if ui.button(t("tray-close")).clicked() {
                        close = true;
                    }
                });
            });

        if install {
            match spoofer::ca::install(&self.base_dir) {
                Ok(()) => {
                    self.spoofer_cert_installed = spoofer::ca::is_current_installed(&self.base_dir);
                    if self.spoofer_cert_installed {
                        self.spoofer_certificate_prompt_open = false;
                        self.console.write("[Spoofer] Certificate installed.");
                        self.evaluate_proxies();
                    }
                }
                Err(error) => self
                    .console
                    .write(format!("[Spoofer] Certificate install failed: {error}")),
            }
        } else if close {
            self.spoofer_certificate_prompt_open = false;
        }
    }

    fn render_fullscreen_notice(&mut self, ctx: &egui::Context) {
        if !self.fullscreen_notice || self.statsapi_notice.is_some() {
            return;
        }
        let mut close = false;
        let mut never = self.config.settings.suppress_fullscreen_warning;

        egui::Window::new(t("fullscreen-notice-overlay-unavailable"))
            .id(egui::Id::new("fullscreen-notice-overlay-unavailable"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(t("fullscreen-notice-rocket-league-is-set-to-fullscreen"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .checkbox(&mut never, t("fullscreen-notice-never-show-again"))
                        .changed()
                    {
                        self.config.settings.suppress_fullscreen_warning = never;
                        self.save_config();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(t("tray-close")).clicked() {
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

        egui::Window::new(t("update-required-title"))
            .id(egui::Id::new("update_required_window"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.heading(t_args(
                    "update-required-heading",
                    &[("version", info.version.as_str().into())],
                ));
                ui.label(t("update-locked-note"));
                ui.add_space(8.0);

                if let Some(err) = &self.update_error {
                    ui.colored_label(egui::Color32::from_rgb(0xe7, 0x4c, 0x3c), err);
                    ui.add_space(8.0);
                }

                if self.update_downloading {
                    ui.add_enabled(false, egui::Button::new(t("update-downloading")));
                    ui.spinner();
                } else if ui
                    .add(
                        egui::Button::new(t("update-button"))
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
        egui::Window::new(t("changelog-title"))
            .id(egui::Id::new("changelog_window"))
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
        egui::Window::new(t("launch-path-notice-rocket-league"))
            .id(egui::Id::new("launch-path-notice-rocket-league"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(t("launch-notice-body"));
                if ui.button(t("btn-ok")).clicked() {
                    close = true;
                }
            });
        if close {
            self.launch_path_notice = false;
        }
    }

    fn render_rl_launch_setup(&mut self, ctx: &egui::Context) {
        if !self.rl_launch_setup_open {
            return;
        }
        use crate::config::RlLaunchMode;

        let mut open = true;
        egui::Window::new(t("rl-launch-setup-rocket-league-launch-setup"))
            .id(egui::Id::new("rl-launch-setup-rocket-league-launch-setup"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.label(t("rl-launch-setup-how-do-you-actually-launch-rocket"));
                ui.add_space(8.0);

                ui.radio_value(
                    &mut self.rl_launch_draft.mode,
                    RlLaunchMode::Unconfigured,
                    t("rl-launch-setup-auto-detect-steam-or-epic-games"),
                );
                ui.radio_value(
                    &mut self.rl_launch_draft.mode,
                    RlLaunchMode::SteamNative,
                    t("settings-rl-mode-steam-native"),
                );
                ui.radio_value(
                    &mut self.rl_launch_draft.mode,
                    RlLaunchMode::EpicDirect,
                    t("settings-rl-mode-epic-direct"),
                );
                ui.radio_value(
                    &mut self.rl_launch_draft.mode,
                    RlLaunchMode::SteamShortcutToHeroic,
                    t("rl-launch-setup-non-steam-shortcut-that-opens-heroic"),
                );
                if self.rl_launch_draft.mode == RlLaunchMode::SteamShortcutToHeroic {
                    ui.label(
                        egui::RichText::new(t("rl-launch-setup-non-steam-shortcuts-can-t-pass"))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(0xe6, 0xa8, 0x3c)),
                    );
                }
                ui.radio_value(
                    &mut self.rl_launch_draft.mode,
                    RlLaunchMode::HeroicDirect,
                    t("rl-launch-setup-heroic-directly-no-steam-or-epic"),
                );
                ui.add_space(8.0);

                match self.rl_launch_draft.mode {
                    RlLaunchMode::Unconfigured | RlLaunchMode::EpicDirect => {}
                    RlLaunchMode::SteamNative => {
                        ui.label(t("rl-launch-setup-steam-app-id-252950-is-rocket"));
                        ui.text_edit_singleline(&mut self.rl_launch_draft.steam_id);
                    }
                    RlLaunchMode::SteamShortcutToHeroic => {
                        if ui
                            .button(t("rl-launch-setup-scan-steam-shortcuts-for-heroic"))
                            .clicked()
                        {
                            self.rl_launch_shortcut_candidates =
                                crate::rl_launch::find_heroic_shortcuts();
                        }
                        if self.rl_launch_shortcut_candidates.is_empty() {
                            ui.small(t("rl-launch-setup-no-candidates-found-yet-click-scan"));
                        } else {
                            for candidate in &self.rl_launch_shortcut_candidates {
                                if ui
                                    .button(format!("{}  ({})", candidate.app_name, candidate.exe))
                                    .clicked()
                                {
                                    self.rl_launch_draft.steam_id = candidate.rungameid.to_string();
                                    // the shortcut's own target IS Heroic's binary - almost every
                                    // Steam shortcut that opens Heroic points straight at it, with
                                    // the actual game picked by the shortcut's launch options, so
                                    // there's nothing left to type in by hand here.
                                    self.rl_launch_draft.heroic_binary =
                                        candidate.exe.trim().trim_matches('"').to_string();
                                }
                            }
                        }
                        ui.add_space(4.0);
                        ui.label(t("rl-launch-setup-steam-shortcut-id"));
                        ui.text_edit_singleline(&mut self.rl_launch_draft.steam_id);
                        if !self.rl_launch_draft.heroic_binary.is_empty() {
                            ui.add_space(4.0);
                            ui.label(t_args(
                                "rl-launch-setup-heroic-binary-rl-launch-draft",
                                &[(
                                    "heroic_binary",
                                    self.rl_launch_draft.heroic_binary.to_string().into(),
                                )],
                            ));
                        }
                    }
                    RlLaunchMode::HeroicDirect => {
                        ui.label(t("rl-launch-setup-heroic-binary-path-e-g-the"));
                        ui.text_edit_singleline(&mut self.rl_launch_draft.heroic_binary);
                    }
                }

                if matches!(
                    self.rl_launch_draft.mode,
                    RlLaunchMode::SteamShortcutToHeroic | RlLaunchMode::HeroicDirect
                ) {
                    ui.add_space(4.0);
                    ui.label(t("rl-launch-setup-epic-catalog-app-name-sugar-is"));
                    ui.text_edit_singleline(&mut self.rl_launch_draft.heroic_app_name);
                    ui.label(t("rl-launch-setup-runner"));
                    ui.text_edit_singleline(&mut self.rl_launch_draft.heroic_runner);
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button(t("rl-launch-setup-save")).clicked() {
                        self.config.rl_launch = self.rl_launch_draft.clone();
                        self.save_config();
                        self.console
                            .write("[Core] Rocket League launch setup saved.");
                        self.rl_launch_setup_open = false;
                    }
                    if ui.button(t("spawner-enable-prompt-cancel")).clicked() {
                        self.rl_launch_setup_open = false;
                    }
                });
            });
        if !open {
            self.rl_launch_setup_open = false;
        }
    }

    fn render_install_modal(&mut self, ctx: &egui::Context) {
        if !self.install_modal.open {
            return;
        }
        let mut open = true;
        let mut close_requested = false;

        let window = if self.install_modal.hebnix_stage {
            egui::Window::new(t("plugins-install-plugin"))
                .id(egui::Id::new("install_plugin_catalog"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .fixed_size([900.0, 550.0])
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        } else {
            egui::Window::new(t("plugins-install-plugin"))
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
                            egui::Button::new(t("install-modal-install-from-hebnix")),
                        )
                        .clicked()
                    {
                        self.install_modal.hebnix_stage = true;
                        self.fetch_hebnix_plugin();
                    }
                    if ui
                        .add_sized(
                            [160.0, 120.0],
                            egui::Button::new(t("install-modal-install-from-zip")),
                        )
                        .clicked()
                    {
                        let dialog = rfd::FileDialog::new()
                            .add_filter(t("install-modal-plugin-archives"), &["zip"]);
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
            if ui.button(t("hebnix-install-back")).clicked() {
                self.install_modal.hebnix_stage = false;
                self.install_modal.catalog.clear();
                self.install_modal.error = None;
                self.install_modal.search_query.clear();
                self.install_modal.current_page = 0;
            }
            if ui.button(t("btn-refresh")).clicked() {
                self.fetch_hebnix_plugin();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let search_resp = ui.add(
                    egui::TextEdit::singleline(&mut self.install_modal.search_query)
                        .hint_text(t("hebnix-install-search-plugins"))
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
                ui.label(t("hebnix-install-fetching-plugins"));
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
                            egui::RichText::new(t("hebnix-install-no-plugins-found"))
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
                                        ui.weak(t_args(
                                            "hebnix-install-by-author",
                                            &[("author", author.to_string().into())],
                                        ));
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
                                                if ui.button(t("hebnix-install-disable")).clicked()
                                                {
                                                    action = Some(ModalAction::Disable(slug));
                                                }
                                            } else if ui
                                                .add(
                                                    egui::Button::new(t("hebnix-install-enable"))
                                                        .fill(egui::Color32::from_rgb(
                                                            0x2e, 0xcc, 0x71,
                                                        )),
                                                )
                                                .clicked()
                                            {
                                                action = Some(ModalAction::Enable(slug));
                                            }
                                        } else if self.install_modal.downloading_id.as_deref()
                                            == Some(id)
                                        {
                                            ui.add_enabled(
                                                false,
                                                egui::Button::new(t("hebnix-install-installing")),
                                            );
                                        } else if ui.button(t("hebnix-install-install")).clicked() {
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
                        egui::Button::new(t("hebnix-install-next")),
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
                ui.label(t_args(
                    "hebnix-install-page-display-page-of-total-pages",
                    &[
                        ("display_page", display_page.to_string().into()),
                        ("total_pages", total_pages.to_string().into()),
                    ],
                ));

                if ui
                    .add_enabled(
                        self.install_modal.current_page > 0,
                        egui::Button::new(t("hebnix-install-prev")),
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

    fn download_theme(&self, theme_id: &str) {
        let theme_id = theme_id.to_string();
        let themes_dir = self.themes_dir.clone();
        let fonts_dir = self.fonts_dir.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::deep_link::install_theme(&theme_id, &themes_dir, &fonts_dir);
            let _ = tx.send(AppMsg::ThemeInstallDone { result });
        });
    }
    fn download_hebnix_plugin(&mut self, plugin_id: &str) {
        self.install_modal.downloading_id = Some(plugin_id.to_string());
        let plugin_id = plugin_id.to_string();
        let plugin_dir = self.plugin_dir.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result: Result<(String, String), String> = (|| {
                let (name, author) = crate::deep_link::plugin_identity(&plugin_id)?;
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
                Ok((
                    plugin_id.clone(),
                    format!("{name} by {author} was installed."),
                ))
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
            self.toasts.update(false);
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
        let has_toast = self
            .toasts
            .update(hebnix_sdk::process::is_rocket_league_focused());
        if (slugs.is_empty() && !webview_wants && !has_toast) || !focused {
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
        if slugs.is_empty() && !has_toast {
            self.native_overlay.hide();
        } else {
            let plugin_manager = &mut self.plugin_mgr;
            let toasts = &mut self.toasts;
            let toast_pos = self.config.settings.toast_position;
            self.native_overlay.frame(rect, |width, height| {
                for slug in &slugs {
                    if let Err(error) = plugin_manager.render_overlay_gdi(slug, width, height) {
                        errors.push(format!("[Core] Overlay error in '{slug}': {error}"));
                    }
                }
                toasts.draw(width, height, toast_pos);
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
        let focus_ok = hebnix_sdk::process::is_rocket_league_focused()
            || winutil::foreground_window_is_ours();

        for (slug, win) in active_plugins {
            let viewport_id = egui::ViewportId::from_hash_of(("plugin_window", &slug));
            let should_be_visible = win.shown(focus_ok);
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
                            .on_hover_text(t("tray-close"))
                            .clicked()
                        {
                            self.plugin_mgr.close_window(&slug);
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                    ui.separator();

                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if let Err(e) = self.plugin_mgr.render_window(&slug, ui) {
                                ui.colored_label(
                                    egui::Color32::from_rgb(0xe7, 0x4c, 0x3c),
                                    t_args(
                                        "plugin-windows-window-error-e",
                                        &[("e", e.to_string().into())],
                                    ),
                                );
                            }
                        });
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

fn install_plugin_update(
    zip_path: &std::path::Path,
    plugin_dir: &std::path::Path,
    slug: &str,
    plugin_id: &str,
) -> Result<(), String> {
    let target = plugin_dir.join(slug);
    let backup = plugin_dir.join(format!(".{slug}.updating"));
    let _ = std::fs::remove_dir_all(&backup);
    if target.exists() {
        std::fs::rename(&target, &backup).map_err(|error| error.to_string())?;
    }
    let result = (|| {
        install_zip(zip_path, plugin_dir)?;
        let extracted = std::fs::read_dir(plugin_dir)
            .map_err(|error| error.to_string())?
            .flatten()
            .map(|entry| entry.path())
            .find(|path| {
                path.is_dir()
                    && path != &backup
                    && std::fs::read_to_string(path.join("plugin.toml"))
                        .ok()
                        .and_then(|text| {
                            toml::from_str::<crate::plugins::manifest::PluginManifest>(&text).ok()
                        })
                        .and_then(|manifest| manifest.plugin_id)
                        .as_deref()
                        == Some(plugin_id)
            })
            .ok_or_else(|| format!("Updated archive did not contain plugin ID {plugin_id}"))?;
        if extracted != target {
            std::fs::rename(&extracted, &target).map_err(|error| error.to_string())?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&backup);
            Ok(())
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(&target);
            if backup.exists() {
                let _ = std::fs::rename(&backup, &target);
            }
            Err(error)
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
        if !crate::watchdog::has_live_handoff()
            && !crate::watchdog::handoff_live_spoofer(Arc::clone(&self.spoofer_mgr))
        {
            if self.item_spawner_enabled {
                self.disable_item_spawner();
            }
            self.spoofer_mgr.shutdown();
        }
        self.workshop.suspend_multiplayer();
        self.discord_presence.stop();
    }
}

impl eframe::App for HebnixApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        for plugin_id in crate::deep_link::take_pending_plugin_ids(&self.base_dir) {
            self.download_hebnix_plugin(&plugin_id);
        }
        for theme_id in crate::deep_link::take_pending_theme_ids(&self.base_dir) {
            self.download_theme(&theme_id);
        }
        if self.quitting && crate::watchdog::has_live_handoff() {
            if !hebnix_sdk::process::is_rocket_league_running() {
                crate::watchdog::finish_live_handoff();
                std::process::exit(0);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            ctx.request_repaint_after(Duration::from_secs(1));
            return;
        }
        self.handle_messages(ctx);

        // rl_path can change after startup, no-ops if unchanged
        crate::patcher::rl_font::set_install_dir(std::path::Path::new(
            &self.config.settings.rl_path,
        ));

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
            } else if self.config.settings.close_to_tray {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.set_hidden(ctx, true);
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
                    ui.heading(t("update-required-banner"));
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
                    // same order as the original hardcoded bar
                    for tab in [
                        Tab::Console,
                        Tab::Workshop,
                        Tab::Spoofer,
                        Tab::Colours,
                        Tab::Patcher,
                        Tab::Settings,
                        Tab::Plugins,
                        Tab::Notifications,
                        Tab::RlApi,
                        Tab::About,
                    ] {
                        let label = if tab == Tab::Notifications {
                            crate::ui::toasts::tab_label(&self.toasts)
                        } else {
                            tab.label()
                        };
                        ui.selectable_value(&mut self.tab, tab, label);
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(&self.status_text)
                                .strong()
                                .size(12.0)
                                .color(self.status_color),
                        );
                        self.render_action_button(ui);
                    });
                });
                ui.separator();

                match self.tab {
                    Tab::Console => self.render_console_tab(ui),
                    Tab::Workshop => {
                        let rl_path = self.config.settings.rl_path.clone();
                        let rl_launch = self.config.rl_launch.clone();
                        let tx = self.tx.clone();
                        self.workshop.render(ui, &rl_path, &rl_launch, &tx);
                    }
                    Tab::Spoofer => self.render_spoofer_tab(ui),
                    Tab::Patcher => {
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut self.items_mode, ItemsMode::Swapper, t("app-items-swapper"));
                            ui.selectable_value(&mut self.items_mode, ItemsMode::Spawner, t("app-items-spawner"));
                        });
                        ui.separator();
                        if self.items_mode == ItemsMode::Spawner {
                            ui.horizontal(|ui| {
                                let mut enabled = self.item_spawner_enabled;
                                if ui.checkbox(&mut enabled, t("app-enable-items-spawner")).changed() {
                                    if enabled {
                                        self.spawner_enable_prompt_open = true;
                                    } else {
                                        self.disable_item_spawner();
                                    }
                                }
                                ui.weak(t("app-read-tutorial-before-enabling"));
                            });
                            let cooked_pc = std::path::Path::new(&self.config.settings.rl_path)
                                .join("TAGame").join("CookedPCConsole");
                            egui::Panel::left("spawner_categories")
                                .resizable(false)
                                .default_size(200.0)
                                .size_range(200.0..=320.0)
                                .show(ui, |ui| {
                                    egui::ScrollArea::vertical().id_salt("spawner_subtabs").show(ui, |ui| {
                                        ui.selectable_value(&mut self.spawner_subtab, SpawnerSubTab::Tutorial, t("app-tutorial"));
                                        ui.separator();
                                        ui.add_enabled_ui(self.item_spawner_enabled && !self.spawn_restart_pending, |ui| {
                                            for category in crate::swapper::SwapCategory::ALL {
                                                ui.selectable_value(&mut self.spawner_subtab,
                                                    SpawnerSubTab::Category(category), category.label());
                                            }
                                        });
                                    });
                                });
                            egui::CentralPanel::default().frame(egui::Frame::new()).show(ui, |ui| {
                                if !self.item_spawner_enabled || self.spawn_restart_pending {
                                    self.spawner_subtab = SpawnerSubTab::Tutorial;
                                }
                                match self.spawner_subtab {
                                    SpawnerSubTab::Tutorial => {
                                        ui.heading(t("app-item-spawner-tutorial"));
                                        ui.add_space(8.0);
                                        ui.label(t("app-rocket-league-must-be-closed-before"));
                                        ui.add_space(8.0);
                                        ui.label(t("app-if-you-close-hebnix-while-rocket"));
                                        ui.add_space(8.0);
                                        ui.label(t("app-if-you-have-problems-connecting-to"));
                                        ui.add_space(8.0);
                                        ui.label(t("app-spawned-items-can-persist-locally-or"));
                                        ui.add_space(8.0);
                                        ui.label(t("app-disabling-item-spawner-stops-interceptio"));
                                        ui.add_space(8.0);
                                        if self.spawn_restart_pending {
                                            ui.add_space(12.0);
                                            ui.weak(t("app-start-rocket-league-to-use-the"));
                                        }
                                    }
                                    SpawnerSubTab::Category(category) => {
                                        if self.catalogs_loading {
                                            ui.horizontal(|ui| { ui.spinner(); ui.label(t("app-loading-item-catalogs")); });
                                        }
                                        if let Some(error) = self.catalogs_error.clone() {
                                            ui.colored_label(egui::Color32::from_rgb(0xe7, 0x4c, 0x3c),
                                                t_args("app-catalog-download-failed-error", &[("error", error.to_string().into())]));
                                            if ui.button(t("app-reload-catalogs")).clicked() { self.reload_catalogs(ctx); }
                                        }
                                        if self.catalogs_loaded {
                                            if let Some((product_id, paint)) = self.swapper.render_spawn_tab(
                                                ui, category, &cooked_pc, &self.tx) {
                                                let request = crate::item_spawning::ItemSpawnRequest {
                                                    product_id, series_id: 1, quality: 0, paint,
                                                    certification: 0, quantity: 1,
                                                };
                                                match self.spoofer_mgr.spawn_item(&request) {
                                                    Ok(()) => self.console.write(format!(
                                                        "[Item Spawner] Queued product ID {product_id}.")),
                                                    Err(error) => self.console.write(format!(
                                                        "[Item Spawner] Could not spawn {product_id}: {error}")),
                                                }
                                            }
                                        }
                                    }
                                }
                            });
                        } else {
                        let rl_path = self.config.settings.rl_path.clone();
                        let cooked_pc = std::path::Path::new(&rl_path)
                            .join("TAGame")
                            .join("CookedPCConsole");
                        let backups_dir = cooked_pc.join("Backups");

                        egui::Panel::left("patcher_settings_list")
                            .resizable(false)
                            .default_size(200.0)
                            .size_range(200.0..=320.0)
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt("patcher_subtabs")
                                    .show(ui, |ui| {
                                        ui.selectable_value(
                                            &mut self.patcher_subtab,
                                            PatcherSubTab::Ball,
                                            t("app-ball-patcher"),
                                        );
                                        ui.selectable_value(
                                            &mut self.patcher_subtab,
                                            PatcherSubTab::BoostMeter,
                                            t("app-boost-patcher"),
                                        );
                                        ui.selectable_value(
                                            &mut self.patcher_subtab,
                                            PatcherSubTab::Decal,
                                            t("app-decal-patcher"),
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
                                            t("app-active"),
                                        );
                                        ui.selectable_value(
                                            &mut self.patcher_subtab,
                                            PatcherSubTab::Presets,
                                            t("presets-presets"),
                                        );
                                    });
                            });

                        egui::CentralPanel::default()
                            .frame(egui::Frame::new())
                            .show(ui, |ui| {
                                let catalog_required = matches!(
                                    self.patcher_subtab,
                                    PatcherSubTab::Decal
                                        | PatcherSubTab::Swapper(_)
                                );
                                if self.catalogs_loading {
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label(t("app-loading-item-catalogs"));
                                    });
                                    ui.add_space(8.0);
                                }
                                if let Some(error) = self.catalogs_error.clone() {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(0xe7, 0x4c, 0x3c),
                                        t_args("app-catalog-download-failed-error", &[("error", error.to_string().into())]),
                                    );
                                    if ui.button(t("app-reload-catalogs")).clicked() {
                                        self.reload_catalogs(ctx);
                                    }
                                    ui.add_space(8.0);
                                }
                                if catalog_required && !self.catalogs_loaded {
                                    return;
                                }

                                match self.patcher_subtab {
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
                                    self.swapper.speed_enabled = self.config.speed_patch.items_page;
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
                                    ui.heading(t("app-active-items"));
                                    let swap_count = self.swapper.active_count(&backups_dir);
                                    let active_count = swap_count
                                        + usize::from(self.patcher_ball.active_ball.is_some())
                                        + usize::from(self.patcher_boost.active_boost.is_some())
                                        + self.patcher_decal.active_decals.len()
                                        + self.patcher_car.active_cars.len();
                                    ui.horizontal(|ui| {
                                        ui.label(t_n("items-active-changes", active_count as i64));
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui
                                                    .add_enabled(
                                                        active_count > 0,
                                                        egui::Button::new(t("app-restore-all")),
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
                                                    if !self.patcher_car.active_cars.is_empty() {
                                                        if let Err(error) = self.patcher_car.restore_all(
                                                            &cooked_pc,
                                                            &backups_dir,
                                                            &self.tx,
                                                            &mut self.config,
                                                        ) {
                                                            let _ = self.tx.send(AppMsg::Log(format!(
                                                                "[Cars] Restore failed: {error}"
                                                            )));
                                                        }
                                                    }
                                                    match self.swapper.restore_all_active(
                                                        &cooked_pc,
                                                        &backups_dir,
                                                        &self.tx,
                                                    ) {
                                                        Ok(count) if count > 0 => {
                                                            let _ = self.tx.send(AppMsg::Log(format!(
                                                                "[Swapper] {}",
                                                                t_n("console-swapper-restored", count as i64)
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
                                        ui.vertical_centered(|ui| ui.weak(t("app-no-patches-or-swaps-are-active")));
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
                                                ui.weak(t("app-replaced-the-default-ball"));
                                            });
                                            if ui.button(t("app-restore")).clicked() {
                                                self.patcher_ball.begin_restore(
                                                    &cooked_pc, &backups_dir, &self.tx, ctx,
                                                );
                                            }
                                        });
                                    }
                                    let active_boost = self.patcher_boost.active_boost.clone();
                                    if let Some(name) = active_boost {
                                        let images = self
                                            .patcher_boost
                                            .boosts
                                            .iter()
                                            .find(|item| item.name == name)
                                            .map(|item| {
                                                (
                                                    item.background_image.clone(),
                                                    item.fill_image.clone(),
                                                    item.glow_image.clone(),
                                                    item.tint_image.clone(),
                                                )
                                            });
                                        ui.horizontal(|ui| {
                                            if let Some((background, fill, glow, tint)) = images {
                                                let size = egui::vec2(90.0, 58.0);
                                                let (rect, _) = ui.allocate_exact_size(
                                                    size,
                                                    egui::Sense::hover(),
                                                );
                                                for (layer, image) in [
                                                    ("background", background),
                                                    ("fill", fill),
                                                    ("glow", glow),
                                                    ("tint", tint),
                                                ] {
                                                    if let Some(image) = image {
                                                        ui.put(
                                                            rect,
                                                            egui::Image::from_bytes(
                                                                format!(
                                                                    "bytes://active/boost/{name}/{layer}"
                                                                ),
                                                                image,
                                                            )
                                                            .fit_to_exact_size(size),
                                                        );
                                                    }
                                                }
                                            }
                                            ui.vertical(|ui| {
                                                ui.strong(&name);
                                                ui.weak(t("app-replaced-the-default-boost-meter"));
                                            });
                                            if ui.button(t("app-restore")).clicked() {
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
                                                ui.weak(t_args("app-replaced-target-label", &[("target_label", target_label.to_string().into())]));
                                            });
                                            if ui.button(t("app-restore")).clicked() {
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
                                    let active_cars = self
                                        .patcher_car
                                        .active_cars
                                        .iter()
                                        .map(|(target, name)| (target.clone(), name.clone()))
                                        .collect::<Vec<_>>();
                                    let mut restore_car = None;
                                    for (target, name) in active_cars {
                                        ui.horizontal(|ui| {
                                            ui.vertical(|ui| {
                                                ui.strong(&name);
                                                ui.weak(t_args("app-replaced-target-for-its-bodyid-car", &[("target", target.to_string().into())]));
                                            });
                                            if ui.button(t("app-restore")).clicked() {
                                                restore_car = Some(target.clone());
                                            }
                                        });
                                    }
                                    if let Some(target) = restore_car {
                                        if let Err(error) = self.patcher_car.restore(
                                            &target,
                                            &cooked_pc,
                                            &backups_dir,
                                            &self.tx,
                                            &mut self.config,
                                        ) {
                                            let _ = self.tx.send(AppMsg::Log(format!(
                                                "[Cars] Restore failed: {error}"
                                            )));
                                        }
                                    }
                                    self.swapper.render_active_swaps(
                                        ui, &cooked_pc, &backups_dir, &self.tx,
                                    );
                                        });
                                }
                                PatcherSubTab::Presets => {
                                    self.render_presets_tab(ui, &cooked_pc, &backups_dir, ctx);
                                }
                                }
                            });
                        }
                    }
                    Tab::Colours => {
                        let cooked_pc = PathBuf::from(&self.config.settings.rl_path)
                            .join("TAGame")
                            .join("CookedPCConsole");
                        let backups_dir = cooked_pc.join("Backups");
                        self.colours.poll();
                        if let Some(action) = self.colours.render(ui, &backups_dir) {
                            if crate::messages::block_item_action_if_game_running(&self.tx) {
                                self.console.write(
                                    "[Colours] Close Rocket League before changing game files.",
                                );
                            } else if !spoofer::is_admin()
                                && crate::patcher::colours::needs_admin(&cooked_pc)
                            {
                                self.colour_admin_prompt_open = true;
                            } else {
                                self.colours.begin(
                                    action,
                                    &cooked_pc,
                                    &backups_dir,
                                    &self.tx,
                                    ctx,
                                );
                            }
                        }
                    }

                    Tab::Settings => self.render_settings_tab(ui),
                    Tab::Plugins => self.render_plugins_tab(ui),
                    Tab::Notifications => {
                        self.toasts.mark_read();
                        self.toast_view.render(ui, &mut self.toasts);
                    }
                    Tab::RlApi => {
                        let cooked_pc = PathBuf::from(&self.config.settings.rl_path)
                            .join("TAGame")
                            .join("CookedPCConsole");
                        let backups_dir = cooked_pc.join("Backups");

                        egui::Panel::left("experimental_list")
                            .resizable(false)
                            .default_size(200.0)
                            .size_range(200.0..=320.0)
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt("experimental_subtabs")
                                    .show(ui, |ui| {
                                        // ui.selectable_value(
                                        //     &mut self.experimental_subtab,
                                        //     ExperimentalSubTab::RlApi,
                                        //     "RLAPI",
                                        // );
                                        ui.selectable_value(
                                            &mut self.experimental_subtab,
                                            ExperimentalSubTab::AutoUploadReplays,
                                            "Auto Upload Replays",
                                        );
                                        ui.selectable_value(
                                            &mut self.experimental_subtab,
                                            ExperimentalSubTab::BallAppearance,
                                            t("app-ball-appearance"),
                                        );
                                        ui.selectable_value(
                                            &mut self.experimental_subtab,
                                            ExperimentalSubTab::CarPatcher,
                                            t("app-car-patcher"),
                                        );
                                        ui.selectable_value(
                                            &mut self.experimental_subtab,
                                            ExperimentalSubTab::WheelAlignment,
                                            t("app-wheel-alignment"),
                                        );
                                        ui.selectable_value(
                                            &mut self.experimental_subtab,
                                            ExperimentalSubTab::SpeedPatch,
                                            t("app-speed-patch"),
                                        );
                                    });
                            });

                        egui::CentralPanel::default()
                            .frame(egui::Frame::new())
                            .show(ui, |ui| match self.experimental_subtab {
                                ExperimentalSubTab::WheelAlignment => {
                                    if self.catalogs_loading { ui.spinner(); }
                                    if let Some(error) = &self.catalogs_error { ui.label(error); }
                                    self.wheel_alignment.render(ui, &cooked_pc);
                                }
                                ExperimentalSubTab::RlApi => self.render_rlapi_tab(ui),
                                ExperimentalSubTab::AutoUploadReplays => {
                                    egui::ScrollArea::vertical().show(ui, |ui| {
                                        self.render_replay_auto_upload_tab(ui);
                                    });
                                }
                                ExperimentalSubTab::SpeedPatch => {
                                    egui::ScrollArea::vertical().show(ui, |ui| {
                                        if self.speed_patch.render(
                                            ui,
                                            &mut self.config.speed_patch,
                                            &cooked_pc,
                                            &backups_dir,
                                            &self.tx,
                                        ) {
                                            self.save_config();
                                        }
                                    });
                                }
                                ExperimentalSubTab::BallAppearance => {
                                    self.colours.poll();
                                    if let Some(action) =
                                        self.colours.render_ball_appearance(ui, &backups_dir)
                                    {
                                        if crate::messages::block_item_action_if_game_running(
                                            &self.tx,
                                        ) {
                                            self.console.write(
                                                "[Colours] Close Rocket League before changing game files.",
                                            );
                                        } else if !spoofer::is_admin()
                                            && crate::patcher::colours::needs_admin(&cooked_pc)
                                        {
                                            self.colour_admin_prompt_open = true;
                                        } else {
                                            self.colours.begin(
                                                action,
                                                &cooked_pc,
                                                &backups_dir,
                                                &self.tx,
                                                ctx,
                                            );
                                        }
                                    }
                                }
                                ExperimentalSubTab::CarPatcher => {
                                    if self.catalogs_loading {
                                        ui.horizontal(|ui| {
                                            ui.spinner();
                                            ui.label(t("app-loading-item-catalogs"));
                                        });
                                    }
                                    if let Some(error) = self.catalogs_error.clone() {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(0xe7, 0x4c, 0x3c),
                                            t_args("app-catalog-download-failed-error", &[("error", error.to_string().into())]),
                                        );
                                        if ui.button(t("app-reload-catalogs")).clicked() {
                                            self.reload_catalogs(ctx);
                                        }
                                    }
                                    if self.catalogs_loaded {
                                        self.patcher_car.render_tab(
                                            ui,
                                            ctx,
                                            &cooked_pc,
                                            &backups_dir,
                                            &self.tx,
                                            &mut self.config,
                                        );
                                    }
                                }
                            });
                    }
                    Tab::About => self.render_about_tab(ui),
                }
            });
        }

        if self.restart_notice && !self.hidden {
            let mut close = false;
            egui::Window::new(t("about-hebnix"))
                .id(egui::Id::new("Hebnix"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(t("app-hebnix-has-initialised-statsapi-please-r"));
                    ui.add_space(8.0);
                    ui.vertical_centered(|ui| {
                        if ui.button(t("action-restart-rocket-league")).clicked() {
                            let path = self.config.settings.rl_path.clone();
                            let _ = crate::winutil::restart_rocket_league(
                                &self.config.rl_launch,
                                std::path::Path::new(&path),
                            );
                            close = true;
                        }
                        if ui.button(t("btn-ok")).clicked() {
                            close = true;
                        }
                    });
                });
            if close {
                self.restart_notice = false;
            }
            self.render_colour_admin_prompt(ctx);
        }

        if !self.hidden {
            self.render_spawner_enable_prompt(ctx);
            self.render_admin_prompt(ctx);
            self.render_replay_upload_start_prompt(ctx);
            self.render_item_action_prompt(ctx);
            self.render_plugin_delete_prompt(ctx);
            self.render_owned_proxy_prompt(ctx);
            self.render_spoofer_certificate_prompt(ctx);
            self.render_statsapi_notice(ctx);
            self.render_web_port_notice(ctx);
            self.render_fullscreen_notice(ctx);
            self.render_launch_path_notice(ctx);
            self.render_changelog_popup(ctx);
            let repair_was_running = self.epic_repair.running;
            self.epic_repair.show(ctx);
            if !repair_was_running && self.epic_repair.running {
                self.spoofer_mgr.shutdown();
            } else if repair_was_running && !self.epic_repair.running {
                self.evaluate_proxies();
            }
            self.render_update_modal(ctx);
            self.render_install_modal(ctx);
            self.render_rl_launch_setup(ctx);
        }
        let screen_active = self.last_rl_open && crate::screen::is_active();
        let plugin_tick_interval = if screen_active {
            Duration::from_millis(16)
        } else if self.last_rl_open {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(500)
        };
        self.plugin_mgr.dispatch_tick_if_due(plugin_tick_interval);
        self.render_plugin_windows(ctx);
        self.plugin_mgr.flush_window_positions();
        self.render_game_overlay();

        let fast = self.last_rl_open
            && (self.plugin_mgr.has_tick_plugins()
                || !self.plugin_mgr.overlay_plugins().is_empty()
                || self.toasts.has_work());
        let heartbeat = if self.toasts.showing() {
            Duration::from_millis(16)
        } else if screen_active {
            Duration::from_millis(16)
        } else if fast {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(500)
        };
        ctx.request_repaint_after(heartbeat);
    }
}
