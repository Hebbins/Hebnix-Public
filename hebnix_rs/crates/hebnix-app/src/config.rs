//! App config, stored under `%AppData%\Hebnix`. First run imports an old
//! config.ini (python version) if present so settings carry over.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_RL_PATH: &str = r"C:\Program Files\Epic Games\rocketleague";
pub const DEFAULT_STATSAPI_PATH: &str =
    r"C:\Program Files\Epic Games\rocketleague\TAGame\Config\DefaultStatsAPI.ini";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowCfg {
    pub width: u32,
    pub height: u32,
}

impl Default for WindowCfg {
    fn default() -> Self {
        Self {
            width: 1250,
            height: 700,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsCfg {
    pub hotkey: String,
    pub theme: String,
    /// Main tab selected when Hebnix starts.
    pub default_tab: String,
    /// main window bg opacity (0.5-1.0)
    pub window_opacity: f32,
    pub start_in_tray: bool,
    pub close_to_tray: bool,
    pub rl_path: String,
    pub rl_path_confirmed: bool,
    pub statsapi_path: String,
    pub suppress_left_alerts: bool,
    pub suppress_fullscreen_warning: bool,
    pub suppress_statsapi_rate_warning: bool,
    pub allow_draw_on_hebnix_focus: bool,
    pub restrict_hotkey_to_hebnix_or_rocket_league: bool,
    /// relaunch elevated on start, the hosts file needs admin
    pub run_as_admin: bool,
    /// Publish Hebnix/Rocket League activity to the local Discord client.
    pub discord_rich_presence: bool,
    /// Show presence only while Rocket League is running.
    pub discord_rocket_league_only: bool,
    /// Include the selected live match fields in Rich Presence.
    #[serde(alias = "discord_current_gamemode")]
    pub discord_game_state: bool,
    pub discord_show_score: bool,
    pub discord_show_map: bool,
    pub discord_show_gamemode: bool,
    pub discord_custom_message: String,
}

impl Default for SettingsCfg {
    fn default() -> Self {
        Self {
            hotkey: "f2".to_string(),
            theme: "Dark".to_string(),
            default_tab: "Console".to_string(),
            window_opacity: 0.96,
            start_in_tray: false,
            close_to_tray: false,
            rl_path: DEFAULT_RL_PATH.to_string(),
            rl_path_confirmed: false,
            statsapi_path: DEFAULT_STATSAPI_PATH.to_string(),
            suppress_left_alerts: false,
            suppress_fullscreen_warning: false,
            suppress_statsapi_rate_warning: false,
            allow_draw_on_hebnix_focus: true,
            restrict_hotkey_to_hebnix_or_rocket_league: true,
            run_as_admin: false,
            discord_rich_presence: true,
            discord_rocket_league_only: false,
            discord_game_state: true,
            discord_show_score: true,
            discord_show_map: true,
            discord_show_gamemode: true,
            discord_custom_message: "Playing Rocket League".to_string(),
        }
    }
}
/// How Rocket League actually gets launched/restarted - used by the
/// Restart Rocket League button and Workshop LAN's Host/Join. The default
/// preserves the original Steam-vs-Epic path detection for existing configs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RlLaunchMode {
    #[default]
    Unconfigured,
    SteamNative,
    EpicDirect,
    SteamShortcutToHeroic,
    HeroicDirect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RlLaunchCfg {
    pub mode: RlLaunchMode,
    pub steam_id: String,
    pub heroic_binary: String,
    pub heroic_app_name: String,
    pub heroic_runner: String,
}

impl Default for RlLaunchCfg {
    fn default() -> Self {
        Self {
            mode: RlLaunchMode::Unconfigured,
            steam_id: "252950".to_string(),
            heroic_binary: String::new(),
            heroic_app_name: "Sugar".to_string(),
            heroic_runner: "legendary".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatchSource {
    #[default]
    Catalog,
    Custom,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PatcherCfg {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_ball: Option<String>,
    pub active_boost: Option<String>,
    pub active_decals: std::collections::HashMap<String, String>,
    pub active_cars: std::collections::HashMap<String, String>,
    pub ball_source: PatchSource,
    pub boost_source: PatchSource,
    pub decal_source: PatchSource,
    pub car_source: PatchSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionButtonAction {
    StartRocketLeague,
    RestartRocketLeague,
    CloseRocketLeague,
    OpenHebnixFolder,
    OpenPluginsFolder,
    ReloadPlugins,
    FixEpicConnection,
}

impl ActionButtonAction {
    pub const CLOSED: [Self; 5] = [
        Self::StartRocketLeague,
        Self::OpenHebnixFolder,
        Self::OpenPluginsFolder,
        Self::ReloadPlugins,
        Self::FixEpicConnection,
    ];

    pub const OPEN: [Self; 5] = [
        Self::RestartRocketLeague,
        Self::CloseRocketLeague,
        Self::OpenHebnixFolder,
        Self::OpenPluginsFolder,
        Self::ReloadPlugins,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::StartRocketLeague => "Start Rocket League",
            Self::RestartRocketLeague => "Restart Rocket League",
            Self::CloseRocketLeague => "Close Rocket League",
            Self::OpenHebnixFolder => "Open Hebnix Folder",
            Self::OpenPluginsFolder => "Open Plugins Folder",
            Self::ReloadPlugins => "Reload Plugins",
            Self::FixEpicConnection => "Fix Epic Connection",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionButtonEntry {
    pub action: ActionButtonAction,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ActionButtonCfg {
    pub rocket_league_closed: Vec<ActionButtonEntry>,
    pub rocket_league_open: Vec<ActionButtonEntry>,
}

impl Default for ActionButtonCfg {
    fn default() -> Self {
        Self {
            rocket_league_closed: ActionButtonAction::CLOSED
                .into_iter()
                .map(|action| ActionButtonEntry {
                    enabled: action == ActionButtonAction::StartRocketLeague,
                    action,
                })
                .collect(),
            rocket_league_open: ActionButtonAction::OPEN
                .into_iter()
                .map(|action| ActionButtonEntry {
                    enabled: action == ActionButtonAction::RestartRocketLeague,
                    action,
                })
                .collect(),
        }
    }
}

impl ActionButtonCfg {
    fn normalize(&mut self) {
        normalize_action_entries(
            &mut self.rocket_league_closed,
            &ActionButtonAction::CLOSED,
            ActionButtonAction::StartRocketLeague,
        );
        normalize_action_entries(
            &mut self.rocket_league_open,
            &ActionButtonAction::OPEN,
            ActionButtonAction::RestartRocketLeague,
        );
    }
}

fn normalize_action_entries(
    entries: &mut Vec<ActionButtonEntry>,
    allowed: &[ActionButtonAction],
    default_action: ActionButtonAction,
) {
    let mut normalized = Vec::with_capacity(allowed.len());
    for entry in entries.drain(..) {
        if allowed.contains(&entry.action)
            && !normalized
                .iter()
                .any(|existing: &ActionButtonEntry| existing.action == entry.action)
        {
            normalized.push(entry);
        }
    }
    for &action in allowed {
        if !normalized.iter().any(|entry| entry.action == action) {
            normalized.push(ActionButtonEntry {
                action,
                enabled: false,
            });
        }
    }
    if !normalized.iter().any(|entry| entry.enabled) {
        if let Some(entry) = normalized
            .iter_mut()
            .find(|entry| entry.action == default_action)
        {
            entry.enabled = true;
        }
    }
    *entries = normalized;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub window: WindowCfg,
    pub settings: SettingsCfg,
    pub rl_launch: RlLaunchCfg,
    pub patcher: PatcherCfg,
    pub action_button: ActionButtonCfg,
    /// enabled state keyed by plugin slug
    pub plugins: BTreeMap<String, bool>,
    /// overlay stacking, bottom first. slugs not listed go on top in load order.
    pub overlay_order: Vec<String>,
}

impl Config {
    /// load config.toml, else import an old config.ini, else defaults
    pub fn load(base_dir: &Path) -> Self {
        let toml_path = base_dir.join("config.toml");
        if let Ok(text) = std::fs::read_to_string(&toml_path) {
            match toml::from_str::<Config>(&text) {
                Ok(mut cfg) => {
                    cfg.action_button.normalize();
                    return cfg;
                }
                Err(e) => tracing::warn!("config.toml is invalid ({e}); using defaults"),
            }
        }

        let ini_path = base_dir.join("config.ini");
        if ini_path.exists() {
            if let Some(cfg) = Self::import_ini(&ini_path) {
                tracing::info!("Imported legacy config.ini");
                let _ = cfg.save(base_dir);
                return cfg;
            }
        }

        let cfg = Config::default();
        let _ = cfg.save(base_dir);
        cfg
    }

    pub fn save(&self, base_dir: &Path) -> std::io::Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let path = base_dir.join("config.toml");
        let tmp = base_dir.join("config.toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;

        #[cfg(not(feature = "lite"))]
        // keep active patch state with the game installation
        let game_root = Path::new(&self.settings.rl_path);
        #[cfg(not(feature = "lite"))]
        if game_root.is_dir() {
            let marker = serde_json::json!({
                "game_path": game_root.to_string_lossy(),
                "patcher": &self.patcher,
            });
            let _ = std::fs::write(
                game_root.join("patcher.json"),
                serde_json::to_vec_pretty(&marker).unwrap_or_default(),
            );
        }
        Ok(())
    }

    /// one-time import of the old python config.ini
    fn import_ini(path: &Path) -> Option<Config> {
        let ini = ini::Ini::load_from_file(path).ok()?;
        let mut cfg = Config::default();

        if let Some(win) = ini.section(Some("Window")) {
            if let Some(w) = win.get("width").and_then(|v| v.parse().ok()) {
                cfg.window.width = w;
            }
            if let Some(h) = win.get("height").and_then(|v| v.parse().ok()) {
                cfg.window.height = h;
            }
        }
        if let Some(settings) = ini.section(Some("Settings")) {
            if let Some(v) = settings.get("hotkey") {
                cfg.settings.hotkey = v.to_string();
            }
            if let Some(v) = settings.get("theme") {
                cfg.settings.theme = v.to_string();
            }
            if let Some(v) = settings.get("start_in_tray") {
                cfg.settings.start_in_tray = parse_ini_bool(v, false);
            }
            if let Some(v) = settings.get("rl_path") {
                cfg.settings.rl_path = v.to_string();
            }
            if let Some(v) = settings.get("statsapi_path") {
                cfg.settings.statsapi_path = v.to_string();
            }
        }
        if let Some(plugins) = ini.section(Some("Plugins")) {
            for (name, val) in plugins.iter() {
                cfg.plugins
                    .insert(name.to_string(), parse_ini_bool(val, false));
            }
        }
        Some(cfg)
    }
}

fn parse_ini_bool(v: &str, default: bool) -> bool {
    match v.trim().to_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => true,
        "false" | "0" | "no" | "off" => false,
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::{ActionButtonAction, ActionButtonCfg, ActionButtonEntry, PatchSource, PatcherCfg};

    #[test]
    fn action_button_defaults_to_start_and_restart() {
        let config = ActionButtonCfg::default();

        let closed: Vec<_> = config
            .rocket_league_closed
            .iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.action)
            .collect();
        let open: Vec<_> = config
            .rocket_league_open
            .iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.action)
            .collect();

        assert_eq!(closed, vec![ActionButtonAction::StartRocketLeague]);
        assert_eq!(open, vec![ActionButtonAction::RestartRocketLeague]);
    }

    #[test]
    fn action_button_normalization_restores_complete_valid_lists() {
        let mut config = ActionButtonCfg {
            rocket_league_closed: vec![
                ActionButtonEntry {
                    action: ActionButtonAction::OpenPluginsFolder,
                    enabled: false,
                },
                ActionButtonEntry {
                    action: ActionButtonAction::OpenPluginsFolder,
                    enabled: true,
                },
                ActionButtonEntry {
                    action: ActionButtonAction::CloseRocketLeague,
                    enabled: true,
                },
            ],
            rocket_league_open: Vec::new(),
        };

        config.normalize();

        assert_eq!(config.rocket_league_closed.len(), 5);
        assert_eq!(
            config.rocket_league_closed[0].action,
            ActionButtonAction::OpenPluginsFolder
        );
        assert!(!config.rocket_league_closed[0].enabled);
        assert!(
            !config
                .rocket_league_closed
                .iter()
                .any(|entry| entry.action == ActionButtonAction::CloseRocketLeague)
        );
        assert_eq!(config.rocket_league_open.len(), 5);
        assert!(
            !config
                .rocket_league_open
                .iter()
                .any(|entry| entry.action == ActionButtonAction::FixEpicConnection)
        );
        assert!(
            config
                .rocket_league_open
                .iter()
                .find(|entry| entry.action == ActionButtonAction::RestartRocketLeague)
                .is_some_and(|entry| entry.enabled)
        );
    }

    #[test]
    fn older_patcher_config_defaults_sources_to_catalog() {
        let config: PatcherCfg = toml::from_str("active_boost = \"Existing\"").unwrap();

        assert_eq!(config.ball_source, PatchSource::Catalog);
        assert_eq!(config.boost_source, PatchSource::Catalog);
        assert_eq!(config.decal_source, PatchSource::Catalog);
        assert_eq!(config.car_source, PatchSource::Catalog);
    }

    #[test]
    fn patcher_sources_round_trip_independently() {
        let config = PatcherCfg {
            ball_source: PatchSource::Custom,
            boost_source: PatchSource::Catalog,
            decal_source: PatchSource::Custom,
            car_source: PatchSource::Custom,
            ..PatcherCfg::default()
        };

        let encoded = toml::to_string(&config).unwrap();
        let decoded: PatcherCfg = toml::from_str(&encoded).unwrap();

        assert_eq!(decoded.ball_source, PatchSource::Custom);
        assert_eq!(decoded.boost_source, PatchSource::Catalog);
        assert_eq!(decoded.decal_source, PatchSource::Custom);
        assert_eq!(decoded.car_source, PatchSource::Custom);
    }
}

/// App root dir: `%AppData%\Hebnix`, or `HEBNIX_BASE_DIR` for dev runs.
pub fn base_dir() -> PathBuf {
    hebnix_sdk::utils::paths::base_dir()
}
