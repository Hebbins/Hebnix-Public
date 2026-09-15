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
            discord_game_state: true,
            discord_show_score: true,
            discord_show_map: true,
            discord_show_gamemode: true,
            discord_custom_message: "Playing Rocket League".to_string(),
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
    pub ball_source: PatchSource,
    pub boost_source: PatchSource,
    pub decal_source: PatchSource,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub window: WindowCfg,
    pub settings: SettingsCfg,
    pub patcher: PatcherCfg,
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
                Ok(cfg) => return cfg,
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
    use super::{PatchSource, PatcherCfg};

    #[test]
    fn older_patcher_config_defaults_sources_to_catalog() {
        let config: PatcherCfg = toml::from_str("active_boost = \"Existing\"").unwrap();

        assert_eq!(config.ball_source, PatchSource::Catalog);
        assert_eq!(config.boost_source, PatchSource::Catalog);
        assert_eq!(config.decal_source, PatchSource::Catalog);
    }

    #[test]
    fn patcher_sources_round_trip_independently() {
        let config = PatcherCfg {
            ball_source: PatchSource::Custom,
            boost_source: PatchSource::Catalog,
            decal_source: PatchSource::Custom,
            ..PatcherCfg::default()
        };

        let encoded = toml::to_string(&config).unwrap();
        let decoded: PatcherCfg = toml::from_str(&encoded).unwrap();

        assert_eq!(decoded.ball_source, PatchSource::Custom);
        assert_eq!(decoded.boost_source, PatchSource::Catalog);
        assert_eq!(decoded.decal_source, PatchSource::Custom);
    }
}

/// App root dir: `%AppData%\Hebnix`, or `HEBNIX_BASE_DIR` for dev runs.
pub fn base_dir() -> PathBuf {
    hebnix_sdk::utils::paths::base_dir()
}
