//! typed accessors + the SaveData container.
//!
//! keys checked against a real .save, savedata.rs --raw dumps the tree.
//! ue3 drops properties sitting at their class default, so the defaults here
//! have to match the game's.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::save_file::file_io::RawSave;
use crate::save_file::models::*;

// Helpers

fn get_object<'a>(objects: &'a [Value], type_name: &str) -> Option<&'a Value> {
    objects
        .iter()
        .find(|obj| obj.get("__type").and_then(|v| v.as_str()) == Some(type_name))
}

fn get_objects<'a>(objects: &'a [Value], type_name: &str) -> Vec<&'a Value> {
    objects
        .iter()
        .filter(|obj| obj.get("__type").and_then(|v| v.as_str()) == Some(type_name))
        .collect()
}

fn safe_i64(obj: &Value, key: &str, default: i64) -> i64 {
    obj.get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
        .unwrap_or(default)
}

fn safe_f64(obj: &Value, key: &str, default: f64) -> f64 {
    obj.get(key).and_then(|v| v.as_f64()).unwrap_or(default)
}

fn safe_str(obj: &Value, key: &str, default: &str) -> String {
    obj.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or(default)
        .to_string()
}

fn safe_bool(obj: &Value, key: &str, default: bool) -> bool {
    match obj.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64().map(|i| i != 0).unwrap_or(default),
        _ => default,
    }
}

fn resolve_ref<'a>(objects: &'a [Value], index: i64) -> Option<&'a Value> {
    if index >= 0 && (index as usize) < objects.len() {
        Some(&objects[index as usize])
    } else {
        None
    }
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn i64_list(v: Option<&Value>) -> Vec<i64> {
    v.and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}

fn instance_lower_bits(v: Option<&Value>) -> u64 {
    v.and_then(|inst| inst.get("LowerBits"))
        .and_then(|lb| lb.as_u64().or_else(|| lb.as_i64().map(|i| i as u64)))
        .unwrap_or(0)
}

fn parse_saved_player(e: &Value) -> SavedPlayer {
    let net_id = e.get("UniqueNetId");
    SavedPlayer {
        epic_account_id: net_id
            .map(|id| safe_str(id, "EpicAccountId", ""))
            .unwrap_or_default(),
        epic_puid: safe_str(e, "EpicPUID", ""),
        platform: net_id
            .map(|id| safe_str(id, "Platform", ""))
            .unwrap_or_default(),
        banner_product_id: e
            .get("BannerData")
            .map(|b| safe_i64(b, "ProductID", 0))
            .unwrap_or(0),
        avatar_border_product_id: e
            .get("AvatarBorderData")
            .map(|b| safe_i64(b, "ProductID", 0))
            .unwrap_or(0),
        raw: e.clone(),
    }
}

// Typed extractors

pub fn parse_profile_stats(raw: &Value) -> ProfileStats {
    let mut stats: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    if let Some(arr) = raw.get("StatValues").and_then(|v| v.as_array()) {
        for entry in arr {
            let id = safe_str(entry, "Id", "");
            if id.is_empty() {
                continue;
            }
            let values = entry
                .get("Values")
                .and_then(|v| v.as_array())
                .map(|vals| {
                    vals.iter()
                        .map(|v| {
                            v.as_i64()
                                .or_else(|| v.as_f64().map(|f| f as i64))
                                .unwrap_or(0)
                        })
                        .collect()
                })
                .unwrap_or_default();
            stats.insert(id, values);
        }
    }

    let product_stats = raw
        .get("ProductStats")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|e| e.is_object())
                .map(|e| ProductStat {
                    product_id: safe_i64(e, "ProductID", 0),
                    value: safe_i64(e, "Value", 0),
                })
                .collect()
        })
        .unwrap_or_default();

    ProfileStats {
        stats,
        product_stats,
        raw: raw.clone(),
    }
}

pub fn parse_client_xp(raw: &Value) -> ClientXp {
    let total_xp = safe_i64(raw, "TotalXP", 0);
    let current_threshold = safe_i64(raw, "CurrentLevelXPThreshold", 0);
    ClientXp {
        level: safe_i64(raw, "Level", 1),
        xp: (total_xp - current_threshold).max(0), // not stored

        total_xp,
        current_threshold,
        next_threshold: safe_i64(raw, "NextLevelXPThreshold", 0),
    }
}

pub fn parse_camera_settings(raw: &Value) -> CameraSettings {
    let empty = json!({});
    let cam = match raw.get("Camera") {
        Some(c) if c.is_object() => c,
        _ => &empty,
    };
    CameraSettings {
        fov: safe_f64(cam, "FOV", 90.0),
        height: safe_f64(cam, "Height", 100.0),
        angle: safe_f64(cam, "Angle", -3.0),
        distance: safe_f64(cam, "Distance", 240.0),
        stiffness: safe_f64(cam, "Stiffness", 0.5),
        swivel_speed: safe_f64(cam, "SwivelSpeed", 2.5),
        transition_speed: safe_f64(cam, "TransitionSpeed", 1.0),
        invert_swivel: safe_bool(raw, "bInvertSwivelPitch", false),
        enable_camera_shake: safe_bool(raw, "bEnableCameraShake", false),
        prefers_secondary_camera: safe_bool(raw, "bPrefersSecondaryCamera", false),
        raw: raw.clone(),
    }
}

/// flatten a bindings array to {action: key}
fn parse_bindings_array(bindings: &Value) -> HashMap<String, String> {
    let mut result = HashMap::new();
    if let Some(arr) = bindings.as_array() {
        for b in arr {
            let action = safe_str(b, "Action", "");
            let key = safe_str(b, "Key", "");
            if !action.is_empty() {
                result.insert(action, key);
            }
        }
    }
    result
}

fn bind(bm: &HashMap<String, String>, key: &str) -> String {
    bm.get(key).cloned().unwrap_or_default()
}

fn bind_or(bm: &HashMap<String, String>, key: &str, alt: &str) -> String {
    bm.get(key)
        .or_else(|| bm.get(alt))
        .cloned()
        .unwrap_or_default()
}

/// ProfilePCSave_TA (keyboard/mouse bindings)
pub fn parse_controls_settings(raw: &Value) -> ControlsSettings {
    let bindings = raw.get("PCBindings").cloned().unwrap_or_else(|| json!([]));
    let bm = parse_bindings_array(&bindings);
    ControlsSettings {
        throttle: bind(&bm, "Throttle"),
        steer_left: bind(&bm, "SteerLeft"),
        steer_right: bind(&bm, "SteerRight"),
        jump: bind(&bm, "Jump"),
        boost: bind(&bm, "Boost"),
        powerslide: bind_or(&bm, "Powerslide", "Handbrake"),
        air_roll: bind(&bm, "AirRoll"),
        air_roll_left: bind(&bm, "AirRollLeft"),
        air_roll_right: bind(&bm, "AirRollRight"),
        focus_on_ball: bind_or(&bm, "FocusOnBall", "BallCam"),
        rear_view: bind_or(&bm, "RearView", "LookBack"),
        scoreboard: bind(&bm, "Scoreboard"),
        skip_music: bind(&bm, "SkipMusic"),
        voice_chat: bind(&bm, "VoiceChat"),
        push_to_talk: bind(&bm, "PushToTalk"),
        use_item: bind(&bm, "UseItem"),
        secondary_use_item: bind(&bm, "SecondaryUseItem"),
        raw: raw.clone(),
        raw_bindings: bindings,
    }
}

/// ProfileGamepadSave_TA (controller bindings)
pub fn parse_gamepad_bindings(raw: &Value) -> GamepadBindings {
    let bindings = raw
        .get("GamepadBindings")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let bm = parse_bindings_array(&bindings);
    GamepadBindings {
        throttle: bind(&bm, "Throttle"),
        steer_left: bind(&bm, "SteerLeft"),
        steer_right: bind(&bm, "SteerRight"),
        jump: bind(&bm, "Jump"),
        boost: bind(&bm, "Boost"),
        powerslide: bind_or(&bm, "Powerslide", "Handbrake"),
        air_roll: bind(&bm, "AirRoll"),
        air_roll_left: bind(&bm, "AirRollLeft"),
        air_roll_right: bind(&bm, "AirRollRight"),
        focus_on_ball: bind_or(&bm, "FocusOnBall", "BallCam"),
        rear_view: bind_or(&bm, "RearView", "LookBack"),
        scoreboard: bind(&bm, "Scoreboard"),
        skip_music: bind(&bm, "SkipMusic"),
        voice_chat: bind(&bm, "VoiceChat"),
        push_to_talk: bind(&bm, "PushToTalk"),
        use_item: bind(&bm, "UseItem"),
        raw: raw.clone(),
        raw_bindings: bindings,
    }
}

pub fn parse_playlist_skills(raw: &Value) -> HashMap<i64, PlaylistSkill> {
    let mut skills = HashMap::new();
    if let Some(skill_data) = raw.get("SkillData").and_then(|v| v.as_array()) {
        for item in skill_data {
            if !item.is_object() {
                continue;
            }
            let pid = safe_i64(item, "Playlist", 0);
            skills.insert(
                pid,
                PlaylistSkill {
                    playlist_id: pid,
                    matches_played: safe_i64(item, "MatchesPlayed", 0),
                    tier: safe_i64(item, "Tier", 0),
                },
            );
        }
    }
    skills
}

pub fn parse_player_loadout(raw: &Value) -> PlayerLoadout {
    let products = raw
        .get("Products")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(|p| p.as_i64().unwrap_or(0)).collect())
        .unwrap_or_default();

    let instance_ids = raw
        .get("OnlineProducts128")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(|e| instance_lower_bits(Some(e))).collect())
        .unwrap_or_default();

    let empty = json!({});
    let tp = match raw.get("TeamPaint") {
        Some(t) if t.is_object() => t,
        _ => &empty,
    };

    PlayerLoadout {
        products,
        instance_ids,
        team_paint: TeamPaint {
            team_color_id: safe_i64(tp, "TeamColorID", 0),
            custom_color_id: safe_i64(tp, "CustomColorID", 0),
            team_finish_id: safe_i64(tp, "TeamFinishID", 0),
            custom_finish_id: safe_i64(tp, "CustomFinishID", 0),
        },
        raw: raw.clone(),
    }
}

pub fn parse_online_product(raw: &Value) -> OnlineProduct {
    OnlineProduct {
        product_id: safe_i64(raw, "ProductID", 0),
        instance_id: {
            let bits = instance_lower_bits(raw.get("InstanceID"));
            if bits == 0 {
                None
            } else {
                Some(bits.to_string())
            }
        },
        series_id: safe_i64(raw, "SeriesID", 0),
        added_timestamp: safe_i64(raw, "AddedTimestamp", 0),
        attributes: raw.get("Attributes").cloned().unwrap_or_else(|| json!([])),
        raw: raw.clone(),
    }
}

pub fn parse_gameplay_settings(raw: &Value) -> GameplaySettings {
    GameplaySettings {
        controller_deadzone: safe_f64(raw, "ControllerDeadzone", 0.3),
        dodge_deadzone: safe_f64(raw, "DodgeInputThreshold", 0.5),
        steering_sensitivity: safe_f64(raw, "SteeringSensitivity", 1.0),
        aerial_sensitivity: safe_f64(raw, "AirControlSensitivity", 1.0),
        raw: raw.clone(),
    }
}

/// WindowMode is absent when fullscreen
pub fn parse_video_settings(raw: &Value) -> VideoSettings {
    let resolution = safe_str(raw, "Resolution", "");
    let (res_width, res_height) = parse_resolution(&resolution);

    let mut options: BTreeMap<String, String> = BTreeMap::new();
    if let Some(arr) = raw.get("VideoOptions").and_then(|v| v.as_array()) {
        for opt in arr {
            let id = safe_str(opt, "Id", "");
            if !id.is_empty() {
                options.insert(id, safe_str(opt, "Value", ""));
            }
        }
    }

    VideoSettings {
        window_mode: WindowMode::from_i64(safe_i64(raw, "WindowMode", 0)),
        resolution,
        res_width,
        res_height,
        max_fps: safe_i64(raw, "MaxFPS", 0),
        options,
        show_lens_flares: safe_bool(raw, "bShowLensFlares", true),
        show_light_shafts: safe_bool(raw, "bShowLightShafts", true),
        show_weather_fx: safe_bool(raw, "bShowWeatherFX", true),
        raw: raw.clone(),
    }
}

fn parse_resolution(s: &str) -> (i64, i64) {
    let lower = s.to_lowercase();
    match lower.split_once('x') {
        Some((w, h)) => (w.trim().parse().unwrap_or(0), h.trim().parse().unwrap_or(0)),
        None => (0, 0),
    }
}

pub fn parse_sound_settings(raw: &Value) -> SoundSettings {
    SoundSettings {
        master_volume: safe_f64(raw, "MasterVolume", 0.5),
        sound_volume: safe_f64(raw, "SoundVolume", 0.5),
        music_volume: safe_f64(raw, "MusicVolume", 0.5),
        gameplay_music_volume: safe_f64(raw, "GameplayMusicVolume", 0.5),
        ambient_volume: safe_f64(raw, "AmbientVolume", 0.5),
        crowd_volume: safe_f64(raw, "CrowdVolume", 0.5),
        raw: raw.clone(),
    }
}

pub fn parse_matchmaking_settings(raw: &Value) -> MatchmakingSettings {
    MatchmakingSettings {
        quick_match_playlists: str_list(raw.get("QuickMatchPlaylists")),
        quick_match_regions: str_list(raw.get("QuickMatchRegions")),
        view_tab: safe_str(raw, "MatchmakingViewTab", ""),
        raw: raw.clone(),
    }
}

/// bindings are flat strings like "Group1Message1"
pub fn parse_quick_chats(raw: &Value) -> Vec<QuickChatBinding> {
    raw.get("QuickChatBindings")
        .and_then(|v| v.as_array())
        .map(|binds| {
            binds
                .iter()
                .enumerate()
                .map(|(i, msg)| {
                    let msg_str = match msg {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    QuickChatBinding {
                        slot: i as i64,
                        message_id: msg_str.clone(),
                        raw: json!({ "message": msg_str }),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_season_progress(raw: &Value) -> SeasonProgress {
    SeasonProgress {
        season_level: safe_i64(raw, "SeasonLevel", 0),
        season_xp: safe_i64(raw, "SeasonXP", 0),
        season_id: safe_i64(raw, "SeasonID", 0),
        raw: raw.clone(),
    }
}

pub fn parse_music_playlist(raw: &Value) -> MusicPlaylist {
    let playlists = raw
        .get("PlaylistsUpdate22_1")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|e| e.is_object())
                .map(|e| MusicPlaylistEntry {
                    playlist: safe_str(e, "Playlist", ""),
                    enabled: safe_bool(e, "bEnabled", false),
                })
                .collect()
        })
        .unwrap_or_default();
    MusicPlaylist {
        gameplay_music_setting: safe_str(raw, "GameplayMusicSetting", ""),
        playlists,
        raw: raw.clone(),
    }
}

pub fn parse_gameplay_display(raw: &Value) -> GameplayDisplaySettings {
    GameplayDisplaySettings {
        effect_intensity: safe_str(raw, "EffectIntensity", ""),
        stat_event_display_level: safe_str(raw, "StatEventDisplayLevel", ""),
        force_default_colors: safe_bool(raw, "bForceDefaultColors", false),
        freeplay_default_team_colors: safe_bool(raw, "bFreeplayDefaultTeamColors", false),
        quick_drop_opening: safe_bool(raw, "bQuickDropOpening", false),
        raw: raw.clone(),
    }
}

pub fn parse_voice_settings(raw: &Value) -> VoiceSettings {
    VoiceSettings {
        output_volume: safe_f64(raw, "OutputVolume", 1.0),
        push_to_talk: safe_bool(raw, "bPushToTalk", false),
        match_notifications: safe_bool(raw, "bMatchNotifications", false),
        notification_level: safe_str(raw, "NotificationLevel", ""),
        raw: raw.clone(),
    }
}

pub fn parse_map_prefs(raw: &Value) -> MapPreferences {
    let entries = raw
        .get("MapPrefs")
        .and_then(|v| v.as_array())
        .map(|prefs| {
            prefs
                .iter()
                .filter(|e| e.is_object())
                .map(|entry| MapPrefEntry {
                    playlist: safe_str(entry, "Playlist", ""),
                    override_global: safe_bool(entry, "bOverrideGlobal", false),
                    likes: str_list(entry.get("Likes")),
                    dislikes: str_list(entry.get("Dislikes")),
                })
                .collect()
        })
        .unwrap_or_default();

    MapPreferences {
        entries,
        selected_freeplay_map: raw
            .get("SelectedFreeplayMap")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        raw: raw.clone(),
    }
}

// SaveData container

#[derive(Debug, Clone)]
pub struct SaveData {
    pub source_file: PathBuf,
    pub header: SaveHeader,
    pub raw_properties: Value,
    pub objects: Vec<Value>,
}

impl SaveData {
    pub fn from_raw(raw: RawSave, filepath: PathBuf) -> Self {
        Self {
            source_file: filepath,
            header: SaveHeader {
                engine_version: raw.engine_version,
                licensee_version: raw.licensee_version,
                type_version: raw.type_version,
                foosball: format!("0x{:08X}", raw.foosball),
                magic: format!("0x{:08X}", raw.magic),
            },
            raw_properties: raw.properties,
            objects: raw.objects,
        }
    }

    // Player Stats

    pub fn stats(&self) -> Option<ProfileStats> {
        get_object(&self.objects, "TAGame.ProfileStatsSave_TA").map(parse_profile_stats)
    }

    pub fn xp(&self) -> Option<ClientXp> {
        get_object(&self.objects, "TAGame.ClientXPSave_TA").map(parse_client_xp)
    }

    // Camera & Controls

    pub fn camera(&self) -> Option<CameraSettings> {
        get_object(&self.objects, "TAGame.ProfileCameraSave_TA").map(parse_camera_settings)
    }

    /// keyboard/mouse bindings from ProfilePCSave_TA
    pub fn controls(&self) -> Option<ControlsSettings> {
        get_object(&self.objects, "TAGame.ProfilePCSave_TA").map(parse_controls_settings)
    }

    pub fn gamepad_bindings(&self) -> Option<GamepadBindings> {
        get_object(&self.objects, "TAGame.ProfileGamepadSave_TA").map(parse_gamepad_bindings)
    }

    /// 0 = off
    pub fn force_feedback_scale(&self) -> Option<f64> {
        get_object(&self.objects, "TAGame.ProfileControlsSave_TA")
            .map(|raw| safe_f64(raw, "ForceFeedbackScale", 0.0))
    }

    // Skill / MMR

    pub fn skills(&self) -> HashMap<i64, PlaylistSkill> {
        get_object(&self.objects, "TAGame.PlaylistSkillDataSave_TA")
            .map(parse_playlist_skills)
            .unwrap_or_default()
    }

    // Loadout & Cosmetics

    pub fn loadout(&self) -> Option<PlayerLoadout> {
        self.equipped_loadout_set().map(|set| set.blue)
    }

    pub fn equipped_loadout_set(&self) -> Option<LoadoutSet> {
        let profile_lo = get_object(&self.objects, "TAGame.ProfileLoadoutSave_TA")?;
        let set_ref = profile_lo.get("EquippedLoadoutSet")?.as_i64()?;
        let set_obj = resolve_ref(&self.objects, set_ref)?;
        Some(self.parse_loadout_set(set_obj))
    }

    pub fn loadout_sets(&self) -> Vec<LoadoutSet> {
        get_objects(&self.objects, "TAGame.LoadoutSet_TA")
            .into_iter()
            .map(|set| self.parse_loadout_set(set))
            .collect()
    }

    fn parse_loadout_set(&self, set: &Value) -> LoadoutSet {
        let refs: Vec<i64> = set
            .get("Loadouts")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|r| r.as_i64()).collect())
            .unwrap_or_default();
        let side = |i: usize| {
            refs.get(i)
                .and_then(|r| resolve_ref(&self.objects, *r))
                .map(parse_player_loadout)
                .unwrap_or_default()
        };
        LoadoutSet {
            name: safe_str(set, "LoadoutSetName", "").trim().to_string(),
            blue: side(0),
            orange: side(1),
        }
    }

    pub fn banner(&self) -> Option<PlayerBanner> {
        let raw = get_object(&self.objects, "TAGame.PlayerBannerSave_TA")?;
        Some(PlayerBanner {
            product_id: safe_i64(raw, "ProductID", 0),
            selected_color: safe_i64(raw, "SelectedColorValue", 0),
            instance_id: instance_lower_bits(raw.get("InstanceID")),
            raw: raw.clone(),
        })
    }

    pub fn avatar_border(&self) -> Option<PlayerAvatarBorder> {
        let raw = get_object(&self.objects, "TAGame.PlayerAvatarBorderSave_TA")?;
        Some(PlayerAvatarBorder {
            product_id: safe_i64(raw, "ProductID", 0),
            selected_color: safe_i64(raw, "SelectedColorValue", 0),
            instance_id: instance_lower_bits(raw.get("InstanceID")),
            raw: raw.clone(),
        })
    }

    // Inventory

    pub fn inventory(&self) -> Vec<OnlineProduct> {
        get_objects(&self.objects, "TAGame.OnlineProduct_TA")
            .into_iter()
            .map(parse_online_product)
            .collect()
    }

    pub fn favorite_instance_ids(&self) -> Vec<u64> {
        get_object(&self.objects, "TAGame.ProductsFavoriteSave_TA")
            .and_then(|raw| raw.get("InstanceIDs128"))
            .and_then(|v| v.as_array())
            .map(|ids| ids.iter().map(|e| instance_lower_bits(Some(e))).collect())
            .unwrap_or_default()
    }

    // Settings

    /// deadzones + sensitivities
    pub fn gameplay(&self) -> Option<GameplaySettings> {
        get_object(&self.objects, "TAGame.ProfileGamepadSave_TA").map(parse_gameplay_settings)
    }

    /// fx + colour toggles
    pub fn gameplay_display(&self) -> Option<GameplayDisplaySettings> {
        get_object(&self.objects, "TAGame.GameplaySettingsSave_TA").map(parse_gameplay_display)
    }

    pub fn video(&self) -> Option<VideoSettings> {
        get_object(&self.objects, "TAGame.VideoSettingsSavePC_TA").map(parse_video_settings)
    }

    /// stale until the game exits, prefer utils::system_settings
    pub fn window_mode(&self) -> Option<WindowMode> {
        self.video().map(|v| v.window_mode)
    }

    pub fn sound(&self) -> Option<SoundSettings> {
        get_object(&self.objects, "TAGame.SoundSettingsSave_TA").map(parse_sound_settings)
    }

    pub fn voice(&self) -> Option<VoiceSettings> {
        get_object(&self.objects, "TAGame.EOSVoiceSettingsSave_TA").map(parse_voice_settings)
    }

    pub fn matchmaking(&self) -> Option<MatchmakingSettings> {
        get_object(&self.objects, "TAGame.MatchmakingSettingsSave_TA")
            .map(parse_matchmaking_settings)
    }

    pub fn network(&self) -> Option<NetworkSettings> {
        get_object(&self.objects, "TAGame.NetworkSave_TA")
            .map(|raw| NetworkSettings { raw: raw.clone() })
    }

    // Quick Chat

    pub fn quick_chats(&self) -> Vec<QuickChatBinding> {
        get_object(&self.objects, "TAGame.ProfileQuickChatSave_TA")
            .map(parse_quick_chats)
            .unwrap_or_default()
    }

    // Season / Progression

    pub fn season(&self) -> Option<SeasonProgress> {
        get_object(&self.objects, "TAGame.SeasonSave_TA").map(parse_season_progress)
    }

    // Profile

    pub fn profile(&self) -> Option<PlayerProfile> {
        let raw = get_object(&self.objects, "TAGame.Profile_TA")?;
        Some(PlayerProfile {
            profile_name: safe_str(raw, "ProfileName", ""),
            player_title: get_object(&self.objects, "TAGame.ProfileLoadoutSave_TA")
                .map(|lo| safe_str(lo, "PlayerTitle", ""))
                .unwrap_or_default(),
            raw: raw.clone(),
        })
    }

    pub fn achievements(&self) -> Option<Achievements> {
        let raw = get_object(&self.objects, "TAGame.AchievementSave_TA")?;
        Some(Achievements {
            game_events_played: safe_i64(raw, "GameEventsPlayed", 0),
            game_events_won: safe_i64(raw, "GameEventsWon", 0),
            games_won_in_a_row: safe_i64(raw, "GamesWonInARow", 0),
            total_scored_goals: safe_i64(raw, "TotalScoredGoals", 0),
            total_shots_blocked: safe_i64(raw, "TotalShotsBlocked", 0),
            goals_or_assists: safe_i64(raw, "GoalsOrAssists", 0),
            goal_shots: safe_i64(raw, "GoalShots", 0),
            goal_shots_any: safe_i64(raw, "GoalShotsAny", 0),
            goal_saves: safe_i64(raw, "GoalSaves", 0),
            highest_mvp_score: safe_i64(raw, "HighestMVPScore", 0),
            total_boost_time: safe_f64(raw, "TotalBoostTime", 0.0),
            total_time_on_wall: safe_f64(raw, "TotalTimeOnWall", 0.0),
            total_drive_distance_km: safe_f64(raw, "TotalDriveDistanceKM", 0.0),
            ranked_matches_played: safe_i64(raw, "RankedMatchesPlayed", 0),
            unranked_matches_played: safe_i64(raw, "UnrankedMatchesPlayed", 0),
            private_matches_played: safe_i64(raw, "PrivateMatchesPlayed", 0),
            exhibition_matches_played: safe_i64(raw, "ExhibitionMatchesPlayed", 0),
            completed_matches_with_clubmates: safe_i64(raw, "CompletedMatchesWithClubmates", 0),
            random_items_dropped: safe_i64(raw, "RandomItemsDropped", 0),
            breakout_goals: safe_i64(raw, "BreakoutGoals", 0),
            breakout_platforms_damaged: safe_i64(raw, "BreakoutPlatformsDamaged", 0),
            highest_certified_rank: safe_i64(raw, "HighestRecordedCertifiedRank", 0),
            levels_played: str_list(raw.get("LevelsPlayed")),
            labs_maps_played: str_list(raw.get("LabsMapsPlayed")),
            cars_played: str_list(raw.get("CarsPlayed")),
            cars_collected: i64_list(raw.get("CarsCollected")),
            training_modes_played: str_list(raw.get("TrainingModesPlayed")),
            rumble_items_activated: str_list(raw.get("RumbleItemsActivated")),
            raw: raw.clone(),
        })
    }

    pub fn training_packs(&self) -> Vec<TrainingPackProgress> {
        get_objects(&self.objects, "TAGame.TrainingPackProgress_TA")
            .into_iter()
            .map(|raw| TrainingPackProgress {
                pack_code: safe_str(raw, "PackCode", ""),
                progress: safe_i64(raw, "Progress", 0),
                time_last_played: safe_i64(raw, "TimeLastPlayed", 0),
            })
            .collect()
    }

    pub fn club_id(&self) -> Option<i64> {
        get_object(&self.objects, "TAGame.ClubSave_TA").map(|raw| safe_i64(raw, "ClubID", 0))
    }

    fn persona_players(&self, key: &str) -> Vec<SavedPlayer> {
        get_object(&self.objects, "TAGame.PersonaSave_TA")
            .and_then(|raw| raw.get(key))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|e| e.is_object())
                    .map(parse_saved_player)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn recent_players(&self) -> Vec<SavedPlayer> {
        self.persona_players("RecentPlayers")
    }

    pub fn observed_players(&self) -> Vec<SavedPlayer> {
        self.persona_players("ObservedPlayerLoadouts")
    }

    pub fn recent_game_ids(&self) -> Vec<String> {
        get_object(&self.objects, "TAGame.PersonaSave_TA")
            .and_then(|raw| raw.get("RecentGameIDs"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    // Other

    pub fn music(&self) -> Option<MusicPlaylist> {
        get_object(&self.objects, "TAGame.MusicPlayerSave_TA").map(parse_music_playlist)
    }

    /// Notifications is a list of object indexes
    pub fn notifications(&self) -> Vec<Notification> {
        get_object(&self.objects, "TAGame.NotificationSave_TA")
            .and_then(|raw| raw.get("Notifications"))
            .and_then(|v| v.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|r| r.as_i64())
                    .map(|idx| {
                        let empty = json!({});
                        let target = resolve_ref(&self.objects, idx).unwrap_or(&empty);
                        Notification {
                            object_index: idx,
                            notification_id: safe_str(target, "NotificationID", ""),
                            title: safe_str(target, "Title", ""),
                            body: safe_str(target, "Body", ""),
                            pop_up: safe_bool(target, "bPopUp", false),
                            pop_up_shown: safe_bool(target, "bPopUpShown", false),
                            raw: target.clone(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn ui_values(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        if let Some(arr) = get_object(&self.objects, "TAGame.UISavedValues_TA")
            .and_then(|raw| raw.get("Values"))
            .and_then(|v| v.as_array())
        {
            for entry in arr {
                let key = safe_str(entry, "Key", "");
                if !key.is_empty() {
                    out.insert(key, safe_str(entry, "Value", ""));
                }
            }
        }
        out
    }

    pub fn map_prefs(&self) -> Option<MapPreferences> {
        get_object(&self.objects, "TAGame.MapPrefsSave_TA").map(parse_map_prefs)
    }

    // Parsed objects (custom access)

    pub fn parsed_objects(&self) -> Vec<ParsedObject> {
        self.objects
            .iter()
            .map(|obj| {
                let type_name = obj
                    .get("__type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown")
                    .to_string();
                let parse_error = obj
                    .get("__parse_error")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let props: serde_json::Map<String, Value> = obj
                    .as_object()
                    .map(|o| {
                        o.iter()
                            .filter(|(k, _)| !k.starts_with("__"))
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                ParsedObject {
                    type_name,
                    properties: Value::Object(props),
                    parse_error,
                }
            })
            .collect()
    }

    /// first raw object matching type_name
    pub fn object_by_type(&self, type_name: &str) -> Option<&Value> {
        get_object(&self.objects, type_name)
    }

    /// all raw objects matching type_name
    pub fn objects_by_type(&self, type_name: &str) -> Vec<&Value> {
        get_objects(&self.objects, type_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_parsing() {
        assert_eq!(parse_resolution("2560x1440"), (2560, 1440));
        assert_eq!(parse_resolution("1920X1080"), (1920, 1080));
        assert_eq!(parse_resolution(""), (0, 0));
        assert_eq!(parse_resolution("garbage"), (0, 0));
    }

    #[test]
    fn window_mode_defaults_to_fullscreen_when_absent() {
        let raw = json!({ "Resolution": "1920x1080" });
        let v = parse_video_settings(&raw);
        assert_eq!(v.window_mode, WindowMode::Fullscreen);
        assert_eq!(v.res_width, 1920);

        let raw = json!({ "WindowMode": 2 });
        assert_eq!(
            parse_video_settings(&raw).window_mode,
            WindowMode::Borderless
        );
        let raw = json!({ "WindowMode": 1 });
        assert_eq!(parse_video_settings(&raw).window_mode, WindowMode::Windowed);
        let raw = json!({ "WindowMode": 9 });
        assert_eq!(
            parse_video_settings(&raw).window_mode,
            WindowMode::Unknown(9)
        );
    }

    #[test]
    fn video_options_flatten() {
        let raw = json!({
            "VideoOptions": [
                { "Id": "RenderDetail", "Value": "Custom" },
                { "Id": "AntiAlias", "Value": "6" }
            ]
        });
        let v = parse_video_settings(&raw);
        assert_eq!(
            v.options.get("RenderDetail").map(String::as_str),
            Some("Custom")
        );
        assert_eq!(v.options.get("AntiAlias").map(String::as_str), Some("6"));
    }

    #[test]
    fn loadout_slots_read_from_products() {
        let raw = json!({
            "Products": [4284, 5307, 5636, 2789, 0, 0],
            "TeamPaint": { "TeamColorID": 9, "CustomColorID": 89 }
        });
        let lo = parse_player_loadout(&raw);
        assert_eq!(lo.body(), 4284);
        assert_eq!(lo.decal(), 5307);
        assert_eq!(lo.wheels(), 5636);
        assert_eq!(lo.boost(), 2789);
        assert_eq!(lo.slot(99), 0);
        assert_eq!(lo.team_paint.team_color_id, 9);
    }

    #[test]
    fn stat_values_and_product_stats_both_parse() {
        let raw = json!({
            "StatValues": [
                { "Id": "Win", "Values": [125, 4198, 918] },
                { "Id": "Goal", "Values": [859, 11411, 3440] }
            ],
            "ProductStats": [ { "ProductID": 23, "Value": 3804 } ]
        });
        let s = parse_profile_stats(&raw);
        assert_eq!(s.values("Win"), &[125, 4198, 918]);
        assert_eq!(s.value("Goal", 1), 11411);
        assert_eq!(s.value("Goal", 9), 0);
        assert_eq!(s.value("Nope", 0), 0);
        assert_eq!(s.product_stats.len(), 1);
        assert_eq!(s.stat_ids(), vec!["Goal", "Win"]);
    }

    #[test]
    fn xp_derives_progress_into_level() {
        let raw = json!({
            "Level": 1747,
            "TotalXP": 34740718,
            "CurrentLevelXPThreshold": 34730000,
            "NextLevelXPThreshold": 34750000
        });
        let xp = parse_client_xp(&raw);
        assert_eq!(xp.level, 1747);
        assert_eq!(xp.xp, 10718);
    }
}
