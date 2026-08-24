//! tracker.gg rocket league api client.
//!
//! tracker.gg blocks by tls fingerprint, so everything goes through
//! curl-impersonate (bundled, boringssl - the same engine curl_cffi uses).
//! each request picks a random real-browser fingerprint so the install base
//! doesn't collapse into one blockable client. no fallback on purpose: if the
//! binary is missing, fetches just error out.

use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::tracker::cache::{DEFAULT_TTL, TtlCache};
use crate::tracker::models::{LifetimeStats, PlayerStats, PlaylistAverage, PlaylistRank};
use crate::utils::constants::RANK_TIERS;
use crate::utils::platforms::{get_platform_slug, is_bot};

// --impersonate targets we rotate through. all verified valid (tracker.gg
// returns 200 or 429 for each, never a 403/block). throttling is per
// fingerprint, so on a 429 we just try a different one. interleaved across
// browser families so 3 consecutive tries span chrome/edge/firefox/safari.
const IMPERSONATE_TARGETS: [&str; 16] = [
    "chrome142",
    "edge101",
    "firefox147",
    "safari260",
    "chrome116",
    "edge99",
    "firefox135",
    "safari184",
    "chrome110",
    "firefox133",
    "safari180",
    "chrome131",
    "chrome124",
    "chrome136",
    "chrome120",
    "chrome146",
];

// retry shape: try this many distinct fingerprints per round, then wait and go
// again, up to this many rounds. matches the standalone's "rotate + back off".
const FPS_PER_ROUND: usize = 3;
const MAX_ROUNDS: usize = 3;
const ROUND_WAIT: Duration = Duration::from_secs(20);

// avatars are a few kb, this is only here so a bad url cant use too much memory
const AVATAR_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// find the bundled curl-impersonate exe. HEBNIX_CURL_IMPERSONATE overrides,
/// else it's in the curl-impersonate/ folder next to our exe (put there at
/// build time). cacert.pem lives right beside it.
pub fn impersonate_binary() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("HEBNIX_CURL_IMPERSONATE") {
        let path = std::path::PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    let exe_name = if cfg!(windows) {
        "curl-impersonate.exe"
    } else {
        "curl-impersonate"
    };
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let candidate = dir.join("curl-impersonate").join(exe_name);
    candidate.is_file().then_some(candidate)
}

// wall-clock derived start index. not crypto, just varies which profile we try
// first so the fleet doesn't all lead with the same one.
fn pick_impersonate_index() -> usize {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0)
        % IMPERSONATE_TARGETS.len()
}

pub struct TrackerClient {
    timeout: Duration,
    cache: TtlCache<PlayerStats>,
    /// avatar bytes by url. only filled from avatarUrl on a fetched profile
    avatars: TtlCache<Arc<[u8]>>,
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
            avatars: TtlCache::new(cache_ttl),
        }
    }

    /// fetch player stats (cache hit returns instantly).
    ///
    /// always returns a PlayerStats, check .error / .not_found for failures.
    /// only Errs on bot or empty primary_id.
    pub fn fetch(&self, primary_id: &str, display_name: &str) -> Result<PlayerStats, String> {
        if primary_id.is_empty() || is_bot(primary_id) {
            return Err(format!("Invalid primary_id: {primary_id:?}"));
        }

        if let Some(cached) = self.cache.get(primary_id) {
            return Ok(cached);
        }

        let slug = get_platform_slug(primary_id);
        let parts: Vec<&str> = primary_id.split('|').collect();
        let platform = parts[0].to_lowercase();
        let user_id = parts.get(1).unwrap_or(&"");

        // non-steam platforms look up by display name
        let target_user = if slug == "steam" {
            user_id.to_string()
        } else {
            urlencode(display_name)
        };

        match self.request(slug, &target_user) {
            Ok(data) => {
                let (stats, expiry_ttl) =
                    parse_response(&data, primary_id, display_name, &platform);
                self.cache.set(primary_id, stats.clone(), expiry_ttl);
                self.cache_avatar(&stats, expiry_ttl);
                Ok(stats)
            }
            Err(err_msg) => {
                let not_found = err_msg.contains("NOT_FOUND_404") || err_msg.contains("404");
                let stats = PlayerStats {
                    primary_id: primary_id.to_string(),
                    display_name: display_name.to_string(),
                    platform,
                    error: Some(err_msg),
                    not_found,
                    fetched_at: now_unix(),
                    ..Default::default()
                };
                // cache 404s so we don't keep hammering for unknown players
                if not_found {
                    self.cache.set(primary_id, stats.clone(), None);
                }
                Ok(stats)
            }
        }
    }

    /// fetch a profile by slug + identifier, e.g. ("steam", "76561198...") or
    /// ("epic", "SomeDisplayName"). display names work everywhere, steam also
    /// takes the id64.
    pub fn fetch_profile(&self, slug: &str, identifier: &str) -> Result<PlayerStats, String> {
        if slug.is_empty() || identifier.is_empty() {
            return Err("platform and identifier must not be empty".to_string());
        }
        let cache_key = format!("{slug}:{identifier}");
        if let Some(cached) = self.cache.get(&cache_key) {
            return Ok(cached);
        }

        let target = urlencode(identifier);
        match self.request(slug, &target) {
            Ok(data) => {
                let (stats, expiry_ttl) = parse_response(&data, &cache_key, identifier, slug);
                self.cache.set(&cache_key, stats.clone(), expiry_ttl);
                self.cache_avatar(&stats, expiry_ttl);
                Ok(stats)
            }
            Err(err_msg) => {
                let not_found = err_msg.contains("NOT_FOUND_404") || err_msg.contains("404");
                let stats = PlayerStats {
                    primary_id: cache_key.clone(),
                    display_name: identifier.to_string(),
                    platform: slug.to_string(),
                    error: Some(err_msg),
                    not_found,
                    fetched_at: now_unix(),
                    ..Default::default()
                };
                if not_found {
                    self.cache.set(&cache_key, stats.clone(), None);
                }
                Ok(stats)
            }
        }
    }

    /// cached stats only, no request.
    pub fn get_cached(&self, primary_id: &str) -> Option<PlayerStats> {
        self.cache.get(primary_id)
    }

    pub fn avatar_bytes(&self, url: &str) -> Option<Arc<[u8]>> {
        self.avatars.get(url)
    }

    pub fn clear_cache(&self) {
        self.cache.clear();
        self.avatars.clear();
    }

    fn cache_avatar(&self, stats: &PlayerStats, ttl: Option<Duration>) {
        let Some(url) = stats.avatar_url.as_deref() else {
            return;
        };
        if self.avatars.get(url).is_some() {
            return;
        }
        let Ok(response) = ureq::get(url).timeout(self.timeout).call() else {
            return;
        };
        let mut bytes = Vec::new();
        if response
            .into_reader()
            .take(AVATAR_MAX_BYTES)
            .read_to_end(&mut bytes)
            .is_err()
        {
            return;
        }
        if !bytes.is_empty() {
            self.avatars.set(url, Arc::from(bytes), ttl);
        }
    }

    fn request(&self, slug: &str, target_user: &str) -> Result<Value, String> {
        let url = format!(
            "https://api.tracker.gg/api/v2/rocket-league/standard/profile/{slug}/{target_user}"
        );
        let Some(bin) = impersonate_binary() else {
            return Err(
                "curl-impersonate not found (should be bundled at curl-impersonate/)".to_string(),
            );
        };
        // cloudflare throttles per fingerprint, so on a fail we try a different
        // one. FPS_PER_ROUND distinct fingerprints per round, wait ROUND_WAIT
        // between rounds, up to MAX_ROUNDS. 404 stops early (really not found).
        let n = IMPERSONATE_TARGETS.len();
        let mut idx = pick_impersonate_index();
        let mut last = String::from("no fingerprint worked");
        for round in 0..MAX_ROUNDS {
            for _ in 0..FPS_PER_ROUND {
                let target = IMPERSONATE_TARGETS[idx % n];
                idx += 1;
                match self.run_curl(&bin, target, &url) {
                    Ok(data) => {
                        tracing::debug!("tracker fetch ok ({target})");
                        return Ok(data);
                    }
                    Err(CurlError::NoCurl) => {
                        return Err("couldn't launch curl-impersonate".to_string());
                    }
                    Err(CurlError::Http(msg)) if msg.contains("NOT_FOUND_404") => return Err(msg),
                    Err(CurlError::Http(msg)) => {
                        tracing::debug!("tracker {target} failed ({msg}), next fingerprint");
                        last = msg;
                    }
                }
            }
            if round + 1 < MAX_ROUNDS {
                tracing::debug!("all {FPS_PER_ROUND} fingerprints failed, waiting {ROUND_WAIT:?}");
                std::thread::sleep(ROUND_WAIT);
            }
        }
        Err(last)
    }

    // run curl-impersonate with the picked fingerprint + our bundled cacert
    // (boringssl doesn't touch the windows cert store, so it needs the bundle).
    fn run_curl(&self, bin: &std::path::Path, target: &str, url: &str) -> Result<Value, CurlError> {
        const MARKER: &str = "\n__HTTP_STATUS__:";
        let timeout_secs = self.timeout.as_secs().max(1).to_string();
        let write_out = format!("{MARKER}%{{http_code}}");

        let mut args: Vec<String> = vec![
            "-s".into(),
            // impersonate sends browser Accept-Encoding, so we need
            // --compressed or the body comes back br/zstd and won't parse.
            "--compressed".into(),
            "-m".into(),
            timeout_secs,
            "-w".into(),
            write_out,
            "--impersonate".into(),
            target.into(),
        ];
        if let Some(cacert) = bin.parent().map(|d| d.join("cacert.pem")) {
            if cacert.is_file() {
                args.push("--cacert".into());
                args.push(cacert.to_string_lossy().into_owned());
            }
        }
        args.push(url.into());

        #[cfg(windows)]
        let output = {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            std::process::Command::new(bin)
                .args(&args)
                .creation_flags(CREATE_NO_WINDOW)
                .output()
        };
        #[cfg(not(windows))]
        let output = std::process::Command::new(bin).args(&args).output();

        let output = output.map_err(|_| CurlError::NoCurl)?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let Some(pos) = stdout.rfind(MARKER) else {
            return Err(CurlError::Http(format!(
                "curl failed (exit {:?})",
                output.status.code()
            )));
        };
        let body = &stdout[..pos];
        let status: u16 = stdout[pos + MARKER.len()..].trim().parse().unwrap_or(0);

        match status {
            404 => Err(CurlError::Http(
                "NOT_FOUND_404: tracker.gg returned 404".to_string(),
            )),
            200 => {
                let data: Value =
                    serde_json::from_str(body).map_err(|e| CurlError::Http(e.to_string()))?;
                if !data.get("data").map(|d| d.is_object()).unwrap_or(false) {
                    return Err(CurlError::Http(
                        "Tracker API returned unexpected structure".to_string(),
                    ));
                }
                Ok(data)
            }
            other => Err(CurlError::Http(format!("tracker.gg returned HTTP {other}"))),
        }
    }
}

enum CurlError {
    // couldn't even launch the binary
    NoCurl,
    // it ran, request failed
    Http(String),
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for byte in s.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// response parsing

fn parse_response(
    data: &Value,
    primary_id: &str,
    display_name: &str,
    platform: &str,
) -> (PlayerStats, Option<Duration>) {
    let empty = Value::Object(Default::default());
    let inner = data.get("data").unwrap_or(&empty);
    let expiry_ttl = parse_expiry_ttl(
        inner
            .get("expiryDate")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );

    let pinfo = inner.get("platformInfo").unwrap_or(&empty);
    let platform_user_handle = pinfo
        .get("platformUserHandle")
        .and_then(|v| v.as_str())
        .unwrap_or(display_name)
        .to_string();
    let avatar_url = pinfo
        .get("avatarUrl")
        .and_then(|v| v.as_str())
        .filter(|url| !url.is_empty())
        .map(str::to_string);

    let meta = inner.get("metadata").unwrap_or(&empty);
    let player_id = meta.get("playerId").and_then(|v| v.as_i64()).unwrap_or(0);
    let current_season = meta
        .get("currentSeason")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let last_updated = meta
        .get("lastUpdated")
        .and_then(|lu| lu.get("value"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let mut ranks: std::collections::HashMap<i64, PlaylistRank> = Default::default();
    let mut lifetime: Option<LifetimeStats> = None;
    let mut averages: std::collections::HashMap<i64, PlaylistAverage> = Default::default();

    if let Some(segments) = inner.get("segments").and_then(|v| v.as_array()) {
        for seg in segments {
            let stype = seg.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match stype {
                "overview" => {
                    lifetime = Some(parse_lifetime(seg.get("stats").unwrap_or(&empty)));
                }
                "playlist" => {
                    let pid = seg
                        .get("attributes")
                        .and_then(|a| a.get("playlistId"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    ranks.insert(pid, parse_playlist(pid, seg));
                }
                "playlistAverage" => {
                    let pid = seg
                        .get("attributes")
                        .and_then(|a| a.get("playlist"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    averages.insert(pid, parse_average(pid, seg));
                }
                "peak-rating" => {
                    let pid = seg
                        .get("attributes")
                        .and_then(|a| a.get("playlistId"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    if let Some(rank) = ranks.get_mut(&pid) {
                        merge_peak(rank, seg);
                    }
                }
                _ => {}
            }
        }
    }

    let stats = PlayerStats {
        primary_id: primary_id.to_string(),
        display_name: display_name.to_string(),
        platform: platform.to_string(),
        platform_user_handle,
        avatar_url,
        player_id,
        ranks,
        lifetime,
        averages,
        last_updated,
        current_season,
        fetched_at: now_unix(),
        error: None,
        not_found: false,
    };
    (stats, expiry_ttl)
}

fn stat_i64(stats: &Value, key: &str) -> i64 {
    stats
        .get(key)
        .and_then(|v| v.get("value"))
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
        .unwrap_or(0)
}

fn stat_f64(stats: &Value, key: &str) -> f64 {
    stats
        .get(key)
        .and_then(|v| v.get("value"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

fn parse_playlist(pid: i64, seg: &Value) -> PlaylistRank {
    let empty = Value::Object(Default::default());
    let meta = seg.get("metadata").unwrap_or(&empty);
    let stats = seg.get("stats").unwrap_or(&empty);

    let tier_data = stats.get("tier").unwrap_or(&empty);
    let tier_meta = tier_data.get("metadata").unwrap_or(&empty);
    let div_data = stats.get("division").unwrap_or(&empty);
    let div_meta = div_data.get("metadata").unwrap_or(&empty);
    let rating_data = stats.get("rating").unwrap_or(&empty);
    let ws_data = stats.get("winStreak").unwrap_or(&empty);
    let ws_meta = ws_data.get("metadata").unwrap_or(&empty);

    PlaylistRank {
        playlist_id: pid,
        playlist_name: meta
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        tier_id: tier_data.get("value").and_then(|v| v.as_i64()).unwrap_or(0),
        tier_name: tier_meta
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Unranked")
            .to_string(),
        // 0-based to 1-based
        division_id: div_data.get("value").and_then(|v| v.as_i64()).unwrap_or(0) + 1,
        division_name: div_meta
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Division I")
            .to_string(),
        mmr: rating_data
            .get("value")
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        matches_played: stat_i64(stats, "matchesPlayed"),
        peak_mmr: stat_i64(stats, "peakRating"),
        peak_tier_id: stat_i64(stats, "peakTier"),
        peak_div_id: stat_i64(stats, "peakDivision"),
        delta_up: div_meta
            .get("deltaUp")
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        delta_down: div_meta
            .get("deltaDown")
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        win_streak: ws_data.get("value").and_then(|v| v.as_i64()).unwrap_or(0),
        win_streak_type: ws_meta
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        rank_percentile: rating_data
            .get("percentile")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
    }
}

fn parse_lifetime(stats: &Value) -> LifetimeStats {
    LifetimeStats {
        wins: stat_i64(stats, "wins"),
        goals: stat_i64(stats, "goals"),
        mvps: stat_i64(stats, "mVPs"),
        saves: stat_i64(stats, "saves"),
        assists: stat_i64(stats, "assists"),
        shots: stat_i64(stats, "shots"),
        goal_shot_ratio: stat_f64(stats, "goalShotRatio"),
        trn_score: stat_f64(stats, "score"),
        season_reward_level: stat_i64(stats, "seasonRewardLevel"),
        season_reward_wins: stat_i64(stats, "seasonRewardWins"),
    }
}

fn parse_average(pid: i64, seg: &Value) -> PlaylistAverage {
    let empty = Value::Object(Default::default());
    let meta = seg.get("metadata").unwrap_or(&empty);
    let stats = seg.get("stats").unwrap_or(&empty);
    PlaylistAverage {
        playlist_id: pid,
        playlist_name: meta
            .get("playlistName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        matches: stat_i64(stats, "matches"),
        rating: stat_i64(stats, "rating"),
        avg_goals_per_game: stat_f64(stats, "avgGoalsPerGame"),
        avg_shots_per_game: stat_f64(stats, "avgShotsPerGame"),
        avg_saves_per_game: stat_f64(stats, "avgSavesPerGame"),
        avg_assists_per_game: stat_f64(stats, "avgAssistsPerGame"),
        avg_mvps_per_game: stat_f64(stats, "avgMVPsPerGame"),
        goals_shots_ratio: stat_f64(stats, "goalsShotsRatio"),
        goals_saves_ratio: stat_f64(stats, "goalsSavesRatio"),
        assists_goals_ratio: stat_f64(stats, "assistsGoalsRatio"),
    }
}

fn merge_peak(rank: &mut PlaylistRank, seg: &Value) {
    let empty = Value::Object(Default::default());
    let stats = seg.get("stats").unwrap_or(&empty);
    let peak = stats.get("peakRating").unwrap_or(&empty);
    let meta = peak.get("metadata").unwrap_or(&empty);

    if let Some(value) = peak.get("value").and_then(|v| v.as_i64()) {
        rank.peak_mmr = value;
    }
    if let Some(name) = meta.get("name").and_then(|v| v.as_str()) {
        if !name.is_empty() {
            rank.peak_tier_id = RANK_TIERS.iter().position(|t| *t == name).unwrap_or(0) as i64;
        }
    }
    if let Some(div_str) = meta.get("division").and_then(|v| v.as_str()) {
        let div = match div_str {
            "Division I" => Some(1),
            "Division II" => Some(2),
            "Division III" => Some(3),
            "Division IV" => Some(4),
            _ => None,
        };
        if let Some(d) = div {
            rank.peak_div_id = d;
        }
    }
}

/// turn the api's expiryDate ("2026-06-13T17:16:06.3421324+00:00") into a
/// cache ttl. None if missing, unparseable, or already past.
fn parse_expiry_ttl(expiry_str: &str) -> Option<Duration> {
    if expiry_str.is_empty() {
        return None;
    }
    let unix = parse_iso8601_to_unix(expiry_str)?;
    let now = now_unix();
    let delta = unix - now;
    if delta <= 0.0 {
        None
    } else {
        Some(Duration::from_secs_f64(delta))
    }
}

/// minimal iso-8601 to unix seconds parser (utc / +-HH:MM offsets).
fn parse_iso8601_to_unix(s: &str) -> Option<f64> {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: i64 = s.get(5..7)?.parse().ok()?;
    let day: i64 = s.get(8..10)?.parse().ok()?;
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    let min: i64 = s.get(14..16)?.parse().ok()?;
    let sec: i64 = s.get(17..19)?.parse().ok()?;

    // fractional secs + tz offset
    let rest = &s[19..];
    let mut offset_secs: i64 = 0;
    let tz_start = rest.find(['+', '-', 'Z']);
    if let Some(idx) = tz_start {
        let tz = &rest[idx..];
        if tz != "Z" && tz.len() >= 6 {
            let sign = if tz.starts_with('-') { -1 } else { 1 };
            let h: i64 = tz.get(1..3)?.parse().ok()?;
            let m: i64 = tz.get(4..6)?.parse().ok()?;
            offset_secs = sign * (h * 3600 + m * 60);
        }
    }

    // days since epoch (howard hinnant's civil-from-days)
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;

    Some((days * 86400 + hour * 3600 + min * 60 + sec - offset_secs) as f64)
}
