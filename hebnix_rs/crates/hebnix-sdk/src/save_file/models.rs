//! typed models for RL save data.
//!
//! each known save object has a struct with defaults on every field, so a
//! changed save format stays forward/backward compatible. raw tree kept in raw.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SaveHeader {
    pub engine_version: i32,
    pub licensee_version: i32,
    pub type_version: i32,
    pub foosball: String,
    pub magic: String,
}

/// a parsed save object, available for every object incl types with no model
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParsedObject {
    pub type_name: String,
    pub properties: Value,
    pub parse_error: Option<String>,
}

// Player Stats

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProductStat {
    pub product_id: i64,
    pub value: i64,
}

/// ProfileStatsSave_TA
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileStats {
    pub stats: BTreeMap<String, Vec<i64>>, // stat id -> 3 values, slots unconfirmed
    pub product_stats: Vec<ProductStat>,
    pub raw: Value,
}

impl ProfileStats {
    pub fn values(&self, id: &str) -> &[i64] {
        self.stats.get(id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn value(&self, id: &str, slot: usize) -> i64 {
        self.stats
            .get(id)
            .and_then(|v| v.get(slot))
            .copied()
            .unwrap_or(0)
    }

    pub fn stat_ids(&self) -> Vec<&str> {
        self.stats.keys().map(String::as_str).collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientXp {
    pub level: i64,
    pub xp: i64,
    pub total_xp: i64,
    pub current_threshold: i64,
    pub next_threshold: i64,
}

impl Default for ClientXp {
    fn default() -> Self {
        Self {
            level: 1,
            xp: 0,
            total_xp: 0,
            current_threshold: 0,
            next_threshold: 0,
        }
    }
}

// Camera & Controls

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraSettings {
    pub fov: f64,
    pub height: f64,
    pub angle: f64,
    pub distance: f64,
    pub stiffness: f64,
    pub swivel_speed: f64,
    pub transition_speed: f64,
    pub invert_swivel: bool,
    pub enable_camera_shake: bool,
    pub prefers_secondary_camera: bool, // ball cam on by default
    pub raw: Value,
}

impl Default for CameraSettings {
    fn default() -> Self {
        Self {
            fov: 90.0,
            height: 100.0,
            angle: -3.0,
            distance: 270.0,
            stiffness: 0.5,
            swivel_speed: 2.5,
            transition_speed: 1.0,
            invert_swivel: false,
            enable_camera_shake: true,
            prefers_secondary_camera: true,
            raw: Value::Null,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ControlsSettings {
    pub throttle: String,
    pub steer_left: String,
    pub steer_right: String,
    pub jump: String,
    pub boost: String,
    pub powerslide: String,
    pub air_roll: String,
    pub air_roll_left: String,
    pub air_roll_right: String,
    pub focus_on_ball: String,
    pub rear_view: String,
    pub scoreboard: String,
    pub skip_music: String,
    pub voice_chat: String,
    pub push_to_talk: String,
    pub use_item: String,
    pub secondary_use_item: String,
    pub raw: Value,
    pub raw_bindings: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GamepadBindings {
    pub throttle: String,
    pub steer_left: String,
    pub steer_right: String,
    pub jump: String,
    pub boost: String,
    pub powerslide: String,
    pub air_roll: String,
    pub air_roll_left: String,
    pub air_roll_right: String,
    pub focus_on_ball: String,
    pub rear_view: String,
    pub scoreboard: String,
    pub skip_music: String,
    pub voice_chat: String,
    pub push_to_talk: String,
    pub use_item: String,
    pub raw: Value,
    pub raw_bindings: Value,
}

// Skill / MMR

/// SkillData entry, no mmr in the save
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlaylistSkill {
    pub playlist_id: i64,
    pub matches_played: i64,
    pub tier: i64,
}

// Loadout & Cosmetics

/// Loadout_TA.TeamPaint
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TeamPaint {
    pub team_color_id: i64,
    pub custom_color_id: i64,
    pub team_finish_id: i64,
    pub custom_finish_id: i64,
}

/// Loadout_TA. no slot names in the save, only 0..3 are known.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayerLoadout {
    pub products: Vec<i64>,     // product id per slot, 0 = empty
    pub instance_ids: Vec<u64>, // same indexing, from OnlineProducts128
    pub team_paint: TeamPaint,
    pub raw: Value,
}

impl PlayerLoadout {
    pub fn slot(&self, index: usize) -> i64 {
        self.products.get(index).copied().unwrap_or(0)
    }

    pub fn body(&self) -> i64 {
        self.slot(0)
    }

    pub fn decal(&self) -> i64 {
        self.slot(1)
    }

    pub fn wheels(&self) -> i64 {
        self.slot(2)
    }

    pub fn boost(&self) -> i64 {
        self.slot(3)
    }
}

/// LoadoutSet_TA
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadoutSet {
    pub name: String,
    pub blue: PlayerLoadout,
    pub orange: PlayerLoadout,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayerBanner {
    pub product_id: i64,
    pub selected_color: i64,
    pub instance_id: u64,
    pub raw: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayerAvatarBorder {
    pub product_id: i64,
    pub selected_color: i64,
    pub instance_id: u64,
    pub raw: Value,
}

// Inventory

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OnlineProduct {
    pub product_id: i64,
    pub instance_id: Option<String>,
    pub series_id: i64,
    pub added_timestamp: i64,
    pub attributes: Value,
    pub raw: Value,
}

// Settings

/// ProfileGamepadSave_TA
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameplaySettings {
    pub controller_deadzone: f64,
    pub dodge_deadzone: f64,
    pub steering_sensitivity: f64,
    pub aerial_sensitivity: f64,
    pub raw: Value,
}

impl Default for GameplaySettings {
    fn default() -> Self {
        Self {
            controller_deadzone: 0.3,
            dodge_deadzone: 0.5,
            steering_sensitivity: 1.0,
            aerial_sensitivity: 1.0,
            raw: Value::Null,
        }
    }
}

/// how the game window is presented. the save omits WindowMode entirely when
/// it's Fullscreen, since ue3 drops properties that equal the class default,
/// so absent means Fullscreen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowMode {
    Fullscreen,
    Windowed,
    Borderless,
    Unknown(i64),
}

impl Default for WindowMode {
    fn default() -> Self {
        WindowMode::Fullscreen
    }
}

impl WindowMode {
    pub fn from_i64(v: i64) -> Self {
        match v {
            0 => WindowMode::Fullscreen,
            1 => WindowMode::Windowed,
            2 => WindowMode::Borderless,
            other => WindowMode::Unknown(other),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            WindowMode::Fullscreen => "fullscreen",
            WindowMode::Windowed => "windowed",
            WindowMode::Borderless => "borderless",
            WindowMode::Unknown(_) => "unknown",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VideoSettings {
    pub window_mode: WindowMode,
    pub resolution: String, // "WIDTHxHEIGHT"
    pub res_width: i64,
    pub res_height: i64,
    pub max_fps: i64,
    pub options: BTreeMap<String, String>, // VideoOptions Id -> Value
    pub show_lens_flares: bool,
    pub show_light_shafts: bool,
    pub show_weather_fx: bool,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoundSettings {
    pub master_volume: f64,
    pub sound_volume: f64,
    pub music_volume: f64,
    pub gameplay_music_volume: f64,
    pub ambient_volume: f64,
    pub crowd_volume: f64,
    pub raw: Value,
}

impl Default for SoundSettings {
    fn default() -> Self {
        Self {
            master_volume: 0.5,
            sound_volume: 0.5,
            music_volume: 0.5,
            gameplay_music_volume: 0.5,
            ambient_volume: 0.5,
            crowd_volume: 0.5,
            raw: Value::Null,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MatchmakingSettings {
    pub quick_match_playlists: Vec<String>,
    pub quick_match_regions: Vec<String>,
    pub view_tab: String,
    pub raw: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkSettings {
    pub raw: Value,
}

// Quick Chat

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuickChatBinding {
    pub slot: i64,
    pub message_id: String,
    pub raw: Value,
}

// Season / Progression

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SeasonProgress {
    pub season_level: i64,
    pub season_xp: i64,
    pub season_id: i64,
    pub raw: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayerProfile {
    pub profile_name: String,
    pub player_title: String, // from ProfileLoadoutSave_TA
    pub raw: Value,
}

// Other

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MusicPlaylistEntry {
    pub playlist: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MusicPlaylist {
    pub gameplay_music_setting: String, // e.g. GameplayMusic_TraningOnly
    pub playlists: Vec<MusicPlaylistEntry>,
    pub raw: Value,
}

/// EngagementEventNotification_TA, resolved from NotificationSave_TA indexes
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Notification {
    pub object_index: i64,
    pub notification_id: String,
    pub title: String,
    pub body: String,
    pub pop_up: bool,
    pub pop_up_shown: bool,
    pub raw: Value,
}

/// PersonaSave_TA entry, RecentPlayers or ObservedPlayerLoadouts
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SavedPlayer {
    pub epic_account_id: String,
    pub epic_puid: String, // RecentPlayers only
    pub platform: String,
    pub banner_product_id: i64,
    pub avatar_border_product_id: i64,
    pub raw: Value,
}

/// GameplaySettingsSave_TA
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GameplayDisplaySettings {
    pub effect_intensity: String,
    pub stat_event_display_level: String,
    pub force_default_colors: bool,
    pub freeplay_default_team_colors: bool,
    pub quick_drop_opening: bool,
    pub raw: Value,
}

/// AchievementSave_TA. named lifetime totals.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Achievements {
    pub game_events_played: i64,
    pub game_events_won: i64,
    pub games_won_in_a_row: i64,
    pub total_scored_goals: i64,
    pub total_shots_blocked: i64,
    pub goals_or_assists: i64,
    pub goal_shots: i64,
    pub goal_shots_any: i64,
    pub goal_saves: i64,
    pub highest_mvp_score: i64,
    pub total_boost_time: f64,
    pub total_time_on_wall: f64,
    pub total_drive_distance_km: f64,
    pub ranked_matches_played: i64,
    pub unranked_matches_played: i64,
    pub private_matches_played: i64,
    pub exhibition_matches_played: i64,
    pub completed_matches_with_clubmates: i64,
    pub random_items_dropped: i64,
    pub breakout_goals: i64,
    pub breakout_platforms_damaged: i64,
    pub highest_certified_rank: i64,
    pub levels_played: Vec<String>,
    pub labs_maps_played: Vec<String>,
    pub cars_played: Vec<String>,
    pub cars_collected: Vec<i64>, // product ids
    pub training_modes_played: Vec<String>,
    pub rumble_items_activated: Vec<String>,
    pub raw: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrainingPackProgress {
    pub pack_code: String,
    pub progress: i64,
    pub time_last_played: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoiceSettings {
    pub output_volume: f64,
    pub push_to_talk: bool,
    pub match_notifications: bool,
    pub notification_level: String,
    pub raw: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MapPrefEntry {
    pub playlist: String,
    pub override_global: bool,
    pub likes: Vec<String>,
    pub dislikes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MapPreferences {
    pub entries: Vec<MapPrefEntry>,
    pub selected_freeplay_map: Option<String>,
    pub raw: Value,
}

impl MapPreferences {
    fn global_entry(&self) -> Option<&MapPrefEntry> {
        self.entries.iter().find(|e| e.playlist == "Global")
    }

    pub fn liked(&self) -> Vec<String> {
        self.global_entry()
            .map(|g| g.likes.clone())
            .unwrap_or_default()
    }

    pub fn disliked(&self) -> Vec<String> {
        self.global_entry()
            .map(|g| g.dislikes.clone())
            .unwrap_or_default()
    }

    pub fn for_playlist(&self, playlist: &str) -> MapPrefEntry {
        for e in &self.entries {
            if e.playlist.eq_ignore_ascii_case(playlist) {
                if e.override_global {
                    return e.clone();
                }
                break;
            }
        }
        if let Some(g) = self.global_entry() {
            return g.clone();
        }
        MapPrefEntry {
            playlist: playlist.to_string(),
            ..Default::default()
        }
    }

    pub fn is_liked(&self, map_name: &str, playlist: Option<&str>) -> bool {
        match playlist {
            Some(p) => self.for_playlist(p).likes.iter().any(|m| m == map_name),
            None => self
                .global_entry()
                .map(|g| g.likes.iter().any(|m| m == map_name))
                .unwrap_or(false),
        }
    }

    pub fn is_disliked(&self, map_name: &str, playlist: Option<&str>) -> bool {
        match playlist {
            Some(p) => self.for_playlist(p).dislikes.iter().any(|m| m == map_name),
            None => self
                .global_entry()
                .map(|g| g.dislikes.iter().any(|m| m == map_name))
                .unwrap_or(false),
        }
    }
}
