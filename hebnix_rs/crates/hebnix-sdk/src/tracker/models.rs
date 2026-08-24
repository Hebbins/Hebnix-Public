//! typed models for tracker.gg responses.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// player's rank in one playlist.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlaylistRank {
    pub playlist_id: i64,
    pub playlist_name: String,
    pub tier_id: i64,
    pub tier_name: String,
    /// 1-4
    pub division_id: i64,
    pub division_name: String,
    pub mmr: i64,
    pub matches_played: i64,
    pub peak_mmr: i64,
    pub peak_tier_id: i64,
    pub peak_div_id: i64,
    /// mmr needed to promote
    pub delta_up: i64,
    /// mmr needed to demote
    pub delta_down: i64,
    pub win_streak: i64,
    /// "win" or "loss"
    pub win_streak_type: String,
    pub rank_percentile: f64,
}

/// lifetime stats from the overview segment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LifetimeStats {
    pub wins: i64,
    pub goals: i64,
    pub mvps: i64,
    pub saves: i64,
    pub assists: i64,
    pub shots: i64,
    pub goal_shot_ratio: f64,
    pub trn_score: f64,
    pub season_reward_level: i64,
    pub season_reward_wins: i64,
}

/// per-playlist averages.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlaylistAverage {
    pub playlist_id: i64,
    pub playlist_name: String,
    pub matches: i64,
    pub rating: i64,
    pub avg_goals_per_game: f64,
    pub avg_shots_per_game: f64,
    pub avg_saves_per_game: f64,
    pub avg_assists_per_game: f64,
    pub avg_mvps_per_game: f64,
    pub goals_shots_ratio: f64,
    pub goals_saves_ratio: f64,
    pub assists_goals_ratio: f64,
}

/// full player profile from tracker.gg.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayerStats {
    pub primary_id: String,
    pub display_name: String,
    pub platform: String,
    pub platform_user_handle: String,
    pub avatar_url: Option<String>,
    pub player_id: i64,
    pub ranks: HashMap<i64, PlaylistRank>,
    pub lifetime: Option<LifetimeStats>,
    pub averages: HashMap<i64, PlaylistAverage>,
    /// iso-8601 string from the api's lastUpdated field.
    pub last_updated: Option<String>,
    pub current_season: i64,
    /// unix secs when this profile was fetched.
    pub fetched_at: f64,
    pub error: Option<String>,
    pub not_found: bool,
}
