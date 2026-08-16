//! parsed Launch.log models.

use serde::{Deserialize, Serialize};

/// session info, always available regardless of match state
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogSessionInfo {
    pub username: Option<String>,
    pub steam_id: Option<String>,
    /// Epic account ID obtained from the Epic launcher command line or EOS log data.
    pub epic_id: Option<String>,
    /// Canonical StatsAPI/Tracker ID, e.g. `Steam|...|0` or `Epic|...|0`.
    pub primary_id: Option<String>,
    pub platform: Option<String>,
    pub rich_presence: Option<String>,
    pub rich_presence_data: Option<String>,
}

/// match info, only valid once the stats api confirms in-game
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogGameInfo {
    pub verified: bool,
    pub playlist_id: Option<i64>,
    pub playlist_name: Option<String>,
    pub game_class: Option<String>,
    pub map_name: Option<String>,
    pub game_tags: Option<String>,
    pub server_name: Option<String>,
    pub region: Option<String>,
    pub server_ip: Option<String>,
    pub server_port: Option<u16>,
}

/// everything pulled from the log
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogInfo {
    pub session: LogSessionInfo,
    pub game: Option<LogGameInfo>,
    pub log_path: Option<String>,
    pub parse_time: f64,
    pub stats_api_available: bool,
}
