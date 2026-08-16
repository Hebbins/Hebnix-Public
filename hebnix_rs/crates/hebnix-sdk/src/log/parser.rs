//! Launch.log parser.
//!
//! pulls session info (username, steam id, rich presence..) and match info
//! (playlist, mode, map, server ip/port) once the stats api confirms it.
//! playlist names come from the psynet config api.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use regex::Regex;

use crate::log::models::{LogGameInfo, LogInfo, LogSessionInfo};
use crate::utils::psynet::{fetch_psynet_config, get_online_playlists};

const STATS_HOST: &str = "127.0.0.1";
const STATS_PORT: u16 = 49123;
const STATS_VERIFY_TIMEOUT: f64 = 3.0;

macro_rules! re {
    ($name:ident, $pattern:expr) => {
        fn $name() -> &'static Regex {
            static RE: OnceLock<Regex> = OnceLock::new();
            RE.get_or_init(|| Regex::new($pattern).unwrap())
        }
    };
}

re!(re_username, r"DevOnline: Logged in as '([^']+)'");
re!(re_steam_id, r"DevOnline: Steam ID: (\d+)");
re!(re_epic_username, r#"(?i)-epicusername=(?:"([^"]+)"|(\S+))"#);
re!(re_epic_id, r"(?i)-epicuserid=([0-9a-f]+)");
re!(
    re_primary_id,
    r"Online_X\.UniqueNetIDToString\([^)]*PlayerID\)=\(([^|()]+)\|([^|()]+)\|(\d+)\)"
);
re!(re_platform, r"ScriptLog: Detected platform: (\w+)");
re!(
    re_rich_presence,
    r"(?m)DevOnline: Set rich presence to: (.+?) data: (.+?)\s*$"
);
re!(
    re_welcomed,
    r"DevNet: Welcomed by server \(Level: ([^,]+), Game: ([^,]+), GameTags: ([^)]+)\)"
);
re!(re_playlist_id, r"PlaylistId=(\d+)");
re!(re_playlist, r"Playlist=(\d+)");
re!(re_server_name, r#"ServerName="([^"]+)""#);
re!(re_region, r#"Region="([^"]+)""#);
re!(re_browse_remote, r"DevNet: Browse: ([\d.]+):(\d+)/(\S+)");
re!(re_browse_local, r"DevNet: Browse: (\S+)");
re!(re_build_id, r"Log: BuildID: (\d+) from GPsyonixBuildID");
re!(re_browse_game, r"[?&]Game=([^?&]+)");
re!(re_browse_tags, r"[?&]GameTags=([^?&]+)");

fn find_last<'t>(re: &Regex, text: &'t str) -> Option<regex::Captures<'t>> {
    re.captures_iter(text).last()
}

// Stats API verification

/// quick check if we're in a live match via the stats api socket. returns
/// (in_game, api_available). in_game: Some(true)=in match, Some(false)=not,
/// None=couldn't tell.
fn verify_via_stats_api(timeout: f64) -> (Option<bool>, bool) {
    let addr = format!("{STATS_HOST}:{STATS_PORT}");
    let Ok(parsed) = addr.parse() else {
        return (None, false);
    };
    let Ok(mut sock) = TcpStream::connect_timeout(&parsed, Duration::from_secs(2)) else {
        return (None, false);
    };
    if sock
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n")
        .is_err()
    {
        return (None, false);
    }
    let api_available = true;
    let _ = sock.set_read_timeout(Some(Duration::from_millis(250)));

    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 65536];

    while Instant::now() < deadline {
        match sock.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                if text.contains("\"UpdateState\"") || text.contains("\"event\":\"UpdateState\"") {
                    return (Some(true), api_available);
                }
                if text.contains("\"MatchEnded\"")
                    || text.contains("\"event\":\"MatchEnded\"")
                    || text.contains("\"MatchDestroyed\"")
                    || text.contains("\"event\":\"MatchDestroyed\"")
                {
                    return (Some(false), api_available);
                }
                if text.contains("\"RoundStarted\"") || text.contains("\"event\":\"RoundStarted\"")
                {
                    return (Some(true), api_available);
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => return (None, api_available),
        }
    }
    (None, api_available)
}

fn check_stats_port() -> bool {
    let addr = format!("{STATS_HOST}:{STATS_PORT}");
    addr.parse()
        .ok()
        .and_then(|a| TcpStream::connect_timeout(&a, Duration::from_secs(1)).ok())
        .is_some()
}

// Main entry point

fn find_launch_log() -> PathBuf {
    dirs::document_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("My Games")
        .join("Rocket League")
        .join("TAGame")
        .join("Logs")
        .join("Launch.log")
}

/// parse Launch.log into session + game info. verify=true (recommended) checks
/// the stats api before returning game data. session is always returned.
pub fn parse_launch_log(log_path: Option<&Path>, verify: bool, lang: &str) -> LogInfo {
    let log_path = log_path
        .map(Path::to_path_buf)
        .unwrap_or_else(find_launch_log);

    if !log_path.is_file() {
        return LogInfo {
            log_path: Some(log_path.to_string_lossy().to_string()),
            ..Default::default()
        };
    }

    let bytes = std::fs::read(&log_path).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes).to_string();

    // PsyNet config for playlist names
    let build_id = re_build_id().captures(&text).map(|c| c[1].to_string());
    let online_playlists = match &build_id {
        Some(id) => get_online_playlists(&fetch_psynet_config(id, lang)),
        None => Default::default(),
    };

    // Session info (always parsed)
    let mut session = LogSessionInfo::default();
    if let Some(c) = find_last(re_username(), &text) {
        session.username = Some(c[1].to_string());
    }
    if let Some(c) = re_steam_id().captures(&text) {
        session.steam_id = Some(c[1].to_string());
    }
    if let Some(c) = find_last(re_epic_id(), &text) {
        session.epic_id = Some(c[1].to_string());
    }
    if let Some(c) = find_last(re_primary_id(), &text) {
        session.primary_id = Some(format!("{}|{}|{}", &c[1], &c[2], &c[3]));
    } else if let Some(epic_id) = &session.epic_id {
        session.primary_id = Some(format!("Epic|{epic_id}|0"));
    } else if let Some(steam_id) = &session.steam_id {
        session.primary_id = Some(format!("Steam|{steam_id}|0"));
    }
    // Epic's launcher command line is local and remains unaffected by Hebnix's
    // account-spoofer proxy. Prefer it over an externally supplied display name.
    if session.epic_id.is_some() {
        if let Some(c) = find_last(re_epic_username(), &text) {
            session.username = Some(
                c.get(1)
                    .or_else(|| c.get(2))
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default(),
            );
        }
    }
    if let Some(c) = re_platform().captures(&text) {
        session.platform = Some(c[1].to_string());
    }
    if let Some(c) = find_last(re_rich_presence(), &text) {
        session.rich_presence = Some(c[1].trim().to_string());
        session.rich_presence_data = Some(c[2].trim().to_string());
    }

    // Game info (only after verification)
    let mut game: Option<LogGameInfo> = None;
    let stats_available;
    let verified;

    if verify {
        let (result, avail) = verify_via_stats_api(STATS_VERIFY_TIMEOUT);
        verified = result == Some(true);
        stats_available = avail;
    } else {
        verified = false;
        stats_available = check_stats_port();
    }

    if verified || !verify {
        let mut g = parse_game_info(&text, verified, &online_playlists);

        // in-game but map/ip missing? wait + re-read, the log fills in during join
        if verified && g.map_name.is_none() && g.server_ip.is_none() {
            std::thread::sleep(Duration::from_secs(1));
            if let Ok(bytes2) = std::fs::read(&log_path) {
                let text2 = String::from_utf8_lossy(&bytes2).to_string();
                if text2 != text {
                    g = parse_game_info(&text2, verified, &online_playlists);
                }
            }
        }
        game = Some(g);
    }

    LogInfo {
        session,
        game,
        log_path: Some(log_path.to_string_lossy().to_string()),
        parse_time: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0),
        stats_api_available: stats_available,
    }
}

// Game info sub-parser

fn parse_game_info(
    text: &str,
    verified: bool,
    online_playlists: &std::collections::HashMap<i64, String>,
) -> LogGameInfo {
    let mut game = LogGameInfo {
        verified,
        ..Default::default()
    };

    // playlist id
    if let Some(c) = find_last(re_playlist_id(), text) {
        game.playlist_id = c[1].parse().ok();
    } else if let Some(c) = find_last(re_playlist(), text) {
        game.playlist_id = c[1].parse().ok();
    }
    if let Some(pid) = game.playlist_id {
        game.playlist_name = online_playlists.get(&pid).cloned();
    }

    // welcomed by server (map + game class + tags)
    let mut game_class = String::new();
    if let Some(c) = find_last(re_welcomed(), text) {
        game.map_name = Some(c[1].to_string());
        game_class = c[2].to_string();
        let tags = c[3].to_string();
        if !tags.is_empty() {
            game.game_tags = Some(tags);
        }
    }

    // browse urls (server ip/port for online, map for training/freeplay)
    let m_remote = find_last(re_browse_remote(), text);
    if let Some(c) = &m_remote {
        game.server_ip = Some(c[1].to_string());
        game.server_port = c[2].parse().ok();
    }

    // no welcomed-by-server map? try the browse url path
    if game.map_name.is_none() {
        if let Some(c) = &m_remote {
            let path = c[3].to_string();
            game.map_name = extract_map_from_browse_path(&path);
            if game_class.is_empty() {
                if let Some(g) = extract_from_browse_query(&path, re_browse_game()) {
                    game_class = g;
                }
            }
            if game.game_tags.is_none() {
                game.game_tags = extract_from_browse_query(&path, re_browse_tags());
            }
        }

        let needs_local = match &game.map_name {
            None => true,
            Some(m) => m.starts_with("JoinGameTransition"),
        };
        if needs_local {
            // last local browse line (no ip:port prefix)
            let local = re_browse_local()
                .captures_iter(text)
                .filter(|c| {
                    let path = &c[1];
                    // skip remote-style ip:port paths handled above
                    !re_browse_remote().is_match(&format!("DevNet: Browse: {path}"))
                })
                .last();
            if let Some(c) = local {
                let path = c[1].to_string();
                if let Some(map_name) = extract_map_from_browse_path(&path) {
                    if map_name != path {
                        game.map_name = Some(map_name);
                    }
                }
                if game_class.is_empty() {
                    if let Some(g) = extract_from_browse_query(&path, re_browse_game()) {
                        game_class = g;
                    }
                }
                if game.game_tags.is_none() {
                    game.game_tags = extract_from_browse_query(&path, re_browse_tags());
                }
            }
        }
    }

    if !game_class.is_empty() {
        game.game_class = Some(game_class);
    }

    if let Some(c) = find_last(re_server_name(), text) {
        game.server_name = Some(c[1].to_string());
    }
    if let Some(c) = find_last(re_region(), text) {
        game.region = Some(c[1].to_string());
    }

    game
}

/// map name from a browse url path
fn extract_map_from_browse_path(path: &str) -> Option<String> {
    let path_only = path.split('?').next().unwrap_or("");
    let lower = path_only.to_lowercase();
    if lower.starts_with("joingametransition") || lower.starts_with("menu_") {
        return None;
    }
    if path_only.is_empty() || (path_only.contains(':') && !path_only.contains('/')) {
        return None;
    }
    Some(path_only.to_string())
}

fn extract_from_browse_query(path: &str, re: &Regex) -> Option<String> {
    re.captures(path).map(|c| c[1].to_string())
}
