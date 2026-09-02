//! rocket league profile client.

use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::tracker::cache::{DEFAULT_TTL, TtlCache};
use crate::tracker::models::{PlayerStats, PlaylistRank};
use crate::utils::platforms::is_bot;

const PROFILE_API: &str = "https://rlapi-serve.nixvio64.workers.dev/player";
const RESPONSE_MAX_BYTES: u64 = 2 * 1024 * 1024;

pub struct TrackerClient {
    timeout: Duration,
    cache: TtlCache<PlayerStats>,
}

impl Default for TrackerClient {
    fn default() -> Self {
        Self::new(Duration::from_secs(8), DEFAULT_TTL)
    }
}

impl TrackerClient {
    pub fn new(timeout: Duration, cache_ttl: Duration) -> Self {
        Self {
            timeout,
            cache: TtlCache::new(cache_ttl),
        }
    }

    pub fn fetch(&self, primary_id: &str, display_name: &str) -> Result<PlayerStats, String> {
        if primary_id.is_empty() || is_bot(primary_id) {
            return Err(format!("Invalid primary_id: {primary_id:?}"));
        }
        if let Some(cached) = self.cache.get(primary_id) {
            return Ok(cached);
        }

        let (platform, platform_user_id) = identity_from_primary_id(primary_id)?;
        let stats = self.fetch_uncached(primary_id, display_name, &platform, &platform_user_id);
        if stats.error.is_none() || stats.not_found {
            self.cache.set(primary_id, stats.clone(), None);
        }
        Ok(stats)
    }

    pub fn fetch_profile(&self, platform: &str, identifier: &str) -> Result<PlayerStats, String> {
        let platform = normalize_platform(platform)?;
        let platform_user_id = strip_platform_id(identifier);
        if platform_user_id.is_empty() {
            return Err("platform id must not be empty".to_string());
        }

        let cache_key = format!("{platform}:{platform_user_id}");
        if let Some(cached) = self.cache.get(&cache_key) {
            return Ok(cached);
        }
        let primary_id = format!("{platform}|{platform_user_id}|0");
        let stats = self.fetch_uncached(&primary_id, "", &platform, &platform_user_id);
        if stats.error.is_none() || stats.not_found {
            self.cache.set(cache_key, stats.clone(), None);
        }
        Ok(stats)
    }

    pub fn get_cached(&self, primary_id: &str) -> Option<PlayerStats> {
        self.cache.get(primary_id)
    }

    pub fn avatar_bytes(&self, _url: &str) -> Option<Arc<[u8]>> {
        None
    }

    pub fn clear_cache(&self) {
        self.cache.clear();
    }

    fn fetch_uncached(
        &self,
        primary_id: &str,
        display_name: &str,
        platform: &str,
        platform_user_id: &str,
    ) -> PlayerStats {
        match self.request(platform, platform_user_id) {
            Ok(data) => parse_response(&data, primary_id, display_name, platform, platform_user_id),
            Err(message) => failed_stats(
                primary_id,
                display_name,
                platform,
                platform_user_id,
                message,
            ),
        }
    }

    fn request(&self, platform: &str, platform_user_id: &str) -> Result<Value, String> {
        let url = format!(
            "{PROFILE_API}/{}/{}",
            urlencode(platform),
            urlencode(platform_user_id)
        );
        let response = match ureq::get(&url)
            .set("Accept", "application/json")
            .set("User-Agent", "Hebnix/2.1.5")
            .timeout(self.timeout)
            .call()
        {
            Ok(response) => response,
            Err(ureq::Error::Status(404, _)) => {
                return Err("NOT_FOUND_404: profile service returned 404".to_string());
            }
            Err(ureq::Error::Status(status, _)) => {
                return Err(format!("profile service returned HTTP {status}"));
            }
            Err(ureq::Error::Transport(error)) => {
                return Err(format!("profile service request failed: {error}"));
            }
        };

        let mut body = String::new();
        response
            .into_reader()
            .take(RESPONSE_MAX_BYTES)
            .read_to_string(&mut body)
            .map_err(|error| format!("failed to read profile response: {error}"))?;
        let data: Value = serde_json::from_str(&body)
            .map_err(|error| format!("invalid profile response: {error}"))?;
        if !data.is_object() {
            return Err("profile service returned an unexpected response".to_string());
        }
        if let Some(error) = data.get("error").and_then(Value::as_str) {
            return Err(error.to_string());
        }
        Ok(data)
    }
}

fn failed_stats(
    primary_id: &str,
    display_name: &str,
    platform: &str,
    platform_user_id: &str,
    message: String,
) -> PlayerStats {
    PlayerStats {
        primary_id: primary_id.to_string(),
        display_name: display_name.to_string(),
        platform: platform.to_string(),
        platform_user_handle: display_name.to_string(),
        platform_user_id: platform_user_id.to_string(),
        avatar_url: Some(String::new()),
        best: String::new(),
        fetched_at: now_unix(),
        not_found: message.contains("NOT_FOUND_404") || message.contains("404"),
        error: Some(message),
        ..Default::default()
    }
}

fn parse_response(
    data: &Value,
    fallback_primary_id: &str,
    fallback_display_name: &str,
    fallback_platform: &str,
    fallback_platform_user_id: &str,
) -> PlayerStats {
    let primary_id = string_value(data, "primary_id").unwrap_or(fallback_primary_id);
    let platform_user_id = string_value(data, "platform_user_id")
        .unwrap_or(fallback_platform_user_id)
        .to_string();
    let display_name = string_value(data, "display_name")
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_display_name)
        .to_string();
    let platform = string_value(data, "platform")
        .and_then(|value| normalize_platform(value).ok())
        .unwrap_or_else(|| fallback_platform.to_string());
    let avatar_url = string_value(data, "avatar_url").unwrap_or("").to_string();

    let platform_user_handle = if display_name.is_empty() {
        platform_user_id.clone()
    } else {
        display_name.clone()
    };

    let mut ranks = std::collections::HashMap::new();
    if let Some(entries) = data.get("ranks").and_then(Value::as_object) {
        for (key, value) in entries {
            let playlist_id = value
                .get("playlist_id")
                .and_then(number_i64)
                .or_else(|| key.parse().ok())
                .unwrap_or(0);
            ranks.insert(playlist_id, parse_rank(value, playlist_id));
        }
    }

    PlayerStats {
        primary_id: primary_id.to_string(),
        display_name,
        platform,
        platform_user_handle,
        platform_user_id,
        avatar_url: Some(avatar_url),
        best: String::new(),
        ranks,
        fetched_at: data
            .get("fetched_at")
            .and_then(number_f64)
            .unwrap_or_else(now_unix),
        cached: data.get("cached").and_then(Value::as_bool).unwrap_or(false),
        season_reward_level: data
            .get("season_reward_level")
            .and_then(number_i64)
            .unwrap_or(0),
        season_reward_wins: data
            .get("season_reward_wins")
            .and_then(number_i64)
            .unwrap_or(0),
        error: None,
        not_found: false,
        ..Default::default()
    }
}

fn parse_rank(value: &Value, playlist_id: i64) -> PlaylistRank {
    PlaylistRank {
        playlist_id,
        playlist_name: string_value(value, "playlist_name")
            .unwrap_or("")
            .to_string(),
        tier_id: value.get("tier_id").and_then(number_i64).unwrap_or(0),
        tier_name: string_value(value, "tier_name")
            .unwrap_or("Unranked")
            .to_string(),
        division_id: value.get("division_id").and_then(number_i64).unwrap_or(0),
        division_name: string_value(value, "division_name")
            .unwrap_or("I")
            .to_string(),
        mmr: value
            .get("mmr")
            .and_then(number_f64)
            .map(|mmr| mmr.round() as i64)
            .unwrap_or(0),
        matches_played: value
            .get("matches_played")
            .and_then(number_i64)
            .unwrap_or(0),
        placement_matches_played: value
            .get("placement_matches_played")
            .and_then(number_i64)
            .unwrap_or(0),
        win_streak: value.get("win_streak").and_then(number_i64).unwrap_or(0),
        win_streak_type: string_value(value, "win_streak_type")
            .unwrap_or("")
            .to_string(),
        ..Default::default()
    }
}

fn identity_from_primary_id(primary_id: &str) -> Result<(String, String), String> {
    let mut parts = primary_id.split('|');
    let platform = normalize_platform(parts.next().unwrap_or(""))?;
    let platform_user_id = parts.next().unwrap_or("").trim().to_string();
    if platform_user_id.is_empty() {
        return Err(format!("Invalid primary_id: {primary_id:?}"));
    }
    Ok((platform, platform_user_id))
}

fn strip_platform_id(identifier: &str) -> String {
    let trimmed = identifier.trim();
    if trimmed.contains('|') {
        trimmed.split('|').nth(1).unwrap_or("").trim().to_string()
    } else {
        trimmed.to_string()
    }
}

fn normalize_platform(platform: &str) -> Result<String, String> {
    match platform.trim().to_ascii_lowercase().as_str() {
        "epic" | "epicgames" => Ok("epic".to_string()),
        "steam" => Ok("steam".to_string()),
        "psn" | "ps4" | "ps5" | "playstation" => Ok("psn".to_string()),
        "xbl" | "xbox" | "xboxone" => Ok("xboxone".to_string()),
        "switch" | "nintendo" => Ok("switch".to_string()),
        value => Err(format!("Unsupported platform: {value:?}")),
    }
}

fn string_value<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn number_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|number| i64::try_from(number).ok()))
        .or_else(|| value.as_f64().map(|number| number as i64))
}

fn number_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|number| number as f64))
        .or_else(|| value.as_u64().map(|number| number as f64))
}

fn urlencode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len() * 3);
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_ids_use_only_the_platform_account_id() {
        assert_eq!(
            identity_from_primary_id("Epic|abc1234|0").unwrap(),
            ("epic".to_string(), "abc1234".to_string())
        );
        assert_eq!(
            identity_from_primary_id("XboxOne|98765|1").unwrap(),
            ("xboxone".to_string(), "98765".to_string())
        );
    }

    #[test]
    fn worker_rank_response_exposes_whole_number_mmr() {
        let data = serde_json::json!({
            "primary_id": "Switch|5025341852990338613|0",
            "platform": "Switch",
            "platform_user_id": "5025341852990338613",
            "display_name": null,
            "avatar_url": "https://avatars.cloudflare.steamstatic.com/example_full.jpg",
            "ranks": {
                "63": {
                    "playlist_id": 63,
                    "playlist_name": "do not use this for matching",
                    "tier_id": 19,
                    "tier_name": "Grand Champion I",
                    "division_id": 2,
                    "division_name": "III",
                    "mmr": 1164.0,
                    "matches_played": 96,
                    "placement_matches_played": 10,
                    "win_streak": 4,
                    "win_streak_type": "win"
                }
            }
        });
        let stats = parse_response(
            &data,
            "Switch|5025341852990338613|0",
            "",
            "switch",
            "5025341852990338613",
        );
        let rank = stats.ranks.get(&63).unwrap();
        assert_eq!(rank.playlist_id, 63);
        assert_eq!(rank.mmr, 1164);
        assert_eq!(rank.division_id, 2);
        assert_eq!(
            stats.avatar_url.as_deref(),
            Some("https://avatars.cloudflare.steamstatic.com/example_full.jpg")
        );
        assert_eq!(stats.best, "");
    }
}
