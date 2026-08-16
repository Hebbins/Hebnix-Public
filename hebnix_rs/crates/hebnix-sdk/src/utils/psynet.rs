//! psynet config api wrapper: playlists/maps/events from the game's public
//! config endpoint. this is the public http config api, not the psynet bridge.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

static CACHE: Mutex<Option<HashMap<String, Value>>> = Mutex::new(None);

fn config_url(build_id: &str, lang: &str) -> String {
    format!("https://config.psynet.gg/v2/Config/BattleCars/{build_id}/Prod/Steam/{lang}/")
}

/// fetch the psynet config json, cached per build id + lang. empty obj on fail.
pub fn fetch_psynet_config(build_id: &str, lang: &str) -> Value {
    let cache_key = format!("{build_id}:{lang}");
    {
        let guard = CACHE.lock().unwrap();
        if let Some(map) = guard.as_ref() {
            if let Some(v) = map.get(&cache_key) {
                return v.clone();
            }
        }
    }

    let url = config_url(build_id, lang);
    let data: Value = ureq::get(&url)
        .set("User-Agent", "hebnix/2.0")
        .timeout(std::time::Duration::from_secs(15))
        .call()
        .ok()
        .and_then(|resp| resp.into_json().ok())
        .unwrap_or_else(|| Value::Object(Default::default()));

    let mut guard = CACHE.lock().unwrap();
    guard
        .get_or_insert_with(HashMap::new)
        .insert(cache_key, data.clone());
    data
}

/// {playlist_id: title} from the config. null Title falls back to the section
/// key name ("PrivateMatch" -> "Private Match"). only Class=PlaylistSettings_TA.
pub fn get_online_playlists(config: &Value) -> HashMap<i64, String> {
    let mut result = HashMap::new();
    let Some(obj) = config.as_object() else {
        return result;
    };
    for (key, section) in obj {
        let Some(section) = section.as_object() else {
            continue;
        };
        if section.get("Class").and_then(|v| v.as_str()) != Some("PlaylistSettings_TA") {
            continue;
        }
        let Some(pid) = section.get("PlaylistID").and_then(|v| v.as_i64()) else {
            continue;
        };
        let title = match section.get("Title").and_then(|v| v.as_str()) {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => key_to_title(key),
        };
        result.insert(pid, title);
    }
    result
}

/// "PrivateMatch" -> "Private Match", "RankedSoloDuel" -> "Ranked Solo Duel"
fn key_to_title(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    let chars: Vec<char> = key.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && c.is_ascii_uppercase() && chars[i - 1].is_ascii_lowercase() {
            out.push(' ');
        }
        out.push(*c);
    }
    out.trim().to_string()
}

/// MapSetName for a playlist id, if any
pub fn get_playlist_map_set_name(config: &Value, playlist_id: i64) -> Option<String> {
    let obj = config.as_object()?;
    for section in obj.values() {
        let Some(section) = section.as_object() else {
            continue;
        };
        if section.get("PlaylistID").and_then(|v| v.as_i64()) == Some(playlist_id) {
            return section
                .get("MapSetName")
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }
    }
    None
}

/// PlayerCount for a playlist id, if any
pub fn get_playlist_player_count(config: &Value, playlist_id: i64) -> Option<i64> {
    let obj = config.as_object()?;
    for section in obj.values() {
        let Some(section) = section.as_object() else {
            continue;
        };
        if section.get("PlaylistID").and_then(|v| v.as_i64()) == Some(playlist_id) {
            return section.get("PlayerCount").and_then(|v| v.as_i64());
        }
    }
    None
}

/// map set name to a game type. "SoccarStandard"/"RankedSoccarStandard" give
/// "Soccar", "Hoops" gives "Hoops".
pub fn resolve_game_type_from_mapset(map_set_name: Option<&str>) -> Option<String> {
    let map_set_name = map_set_name?;
    if map_set_name.is_empty() {
        return None;
    }

    let direct: [(&str, &str); 9] = [
        ("SnowDay", "Snow Day"),
        ("Labs", "Rocket Labs"),
        ("GhostHunt", "Ghost Hunt"),
        ("BeachBall", "Beach Ball"),
        ("Heatseeker", "Heatseeker"),
        ("Knockout", "Knockout"),
        ("Gridiron", "Gridiron"),
        ("Volleyball", "Volleyball"),
        ("SuperCube", "Super Cube"),
    ];
    if let Some((_, v)) = direct.iter().find(|(k, _)| *k == map_set_name) {
        return Some(v.to_string());
    }

    let mut name = map_set_name;
    if let Some(stripped) = name.strip_prefix("Ranked") {
        name = stripped;
    }
    for suffix in ["Standard", "Doubles", "Duel", "Quads", "Tournament"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            name = stripped;
            break;
        }
    }
    if name.starts_with("Soccar") {
        name = "Soccar";
    }

    if name.is_empty() {
        Some(map_set_name.to_string())
    } else {
        Some(name.to_string())
    }
}

/// internal map names for a playlist, MapList.Maps. prefix stripped
pub fn get_maps_for_playlist(config: &Value, playlist_id: i64) -> Vec<String> {
    let Some(map_set_name) = get_playlist_map_set_name(config, playlist_id) else {
        return Vec::new();
    };
    let Some(online_sets) = config
        .get("MapsConfig")
        .and_then(|mc| mc.get("OnlineMapSets"))
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    for map_set in online_sets {
        if map_set.get("SetName").and_then(|v| v.as_str()) == Some(map_set_name.as_str()) {
            return map_set
                .get("Maps")
                .and_then(|v| v.as_array())
                .map(|maps| {
                    maps.iter()
                        .filter_map(|m| m.get("Map").and_then(|v| v.as_str()))
                        .map(strip_map_prefix)
                        .collect()
                })
                .unwrap_or_default();
        }
    }
    Vec::new()
}

/// strips the MapList.Maps. prefix
fn strip_map_prefix(full_name: &str) -> String {
    full_name
        .strip_prefix("MapList.Maps.")
        .unwrap_or(full_name)
        .to_string()
}

/// raw special events (active + upcoming)
pub fn get_special_events(config: &Value) -> Vec<Value> {
    config
        .get("SpecialEventsConfig")
        .and_then(|ec| ec.get("Events"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

/// events active right now (StartTime <= now <= EndTime)
pub fn get_active_events(config: &Value) -> Vec<Value> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    get_special_events(config)
        .into_iter()
        .filter(|e| {
            let start = e.get("StartTime").and_then(|v| v.as_i64()).unwrap_or(0);
            let end = e
                .get("EndTime")
                .and_then(|v| v.as_i64())
                .unwrap_or(i64::MAX);
            start <= now && now <= end
        })
        .collect()
}
