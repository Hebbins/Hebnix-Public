//! Small, dependency-free Discord Rich Presence client.
//!
//! Discord exposes its local RPC endpoint as a Windows named pipe. Keeping the
//! client on its own thread means a missing/restarting Discord client can never
//! stall the UI or the Rocket League monitor.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RETRY_INTERVAL: Duration = Duration::from_secs(10);
const APPLICATION_ID: &str = "1517810728155746405";
const HEBNIX_DISPLAY_NAME: &str = "Hebnix";
const ROCKET_LEAGUE_DISPLAY_NAME: &str = "Rocket League with Hebnix";
const COMMUNITY_URL: &str = "https://discord.gg/yr6xXb5wQd";
const WEBSITE_URL: &str = "https://hebnix.com/";

#[derive(Clone, Debug, Default)]
pub struct MatchInfo {
    pub gamemode: String,
    pub map: String,
    pub score: String,
}

impl MatchInfo {
    pub fn from_state(
        state: &hebnix_sdk::stats::models::UpdateStateData,
        game: Option<&hebnix_sdk::log::LogGameInfo>,
    ) -> Self {
        let internal_map = if state.game.arena.is_empty() {
            game.and_then(|value| value.map_name.as_deref())
                .unwrap_or_default()
        } else {
            &state.game.arena
        };
        Self {
            gamemode: display_gamemode(game),
            map: display_map_name(internal_map),
            score: display_score(&state.game.teams),
        }
    }

    pub fn update_state(&mut self, state: &hebnix_sdk::stats::models::UpdateStateData) {
        if !state.game.arena.is_empty() {
            self.map = display_map_name(&state.game.arena);
        }
        self.score = display_score(&state.game.teams);
    }
}

#[derive(Clone, Debug, Eq)]
struct Activity {
    name: String,
    details: String,
    state: String,
    started_at: u64,
}

impl PartialEq for Activity {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.details == other.details && self.state == other.state
    }
}

enum Command {
    Configure(bool),
    Activity(Activity),
    Shutdown,
}

pub struct DiscordPresence {
    tx: Sender<Command>,
    worker: Option<JoinHandle<()>>,
}

impl DiscordPresence {
    pub fn start(enabled: bool) -> Self {
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("discord-presence".into())
            .spawn(move || run_worker(rx, enabled))
            .ok();
        Self { tx, worker }
    }

    pub fn configure(&self, enabled: bool) {
        let _ = self.tx.send(Command::Configure(enabled));
    }

    pub fn set_match(&self, settings: &crate::config::SettingsCfg, info: &MatchInfo) {
        let (details, state) = match_activity(settings, info);
        self.set_activity(ROCKET_LEAGUE_DISPLAY_NAME, details, state);
    }

    pub fn set_idle(&self, settings: &crate::config::SettingsCfg, rocket_league_open: bool) {
        let (details, state) = idle_activity(settings, rocket_league_open);
        let name = if rocket_league_open {
            ROCKET_LEAGUE_DISPLAY_NAME
        } else {
            HEBNIX_DISPLAY_NAME
        };
        self.set_activity(name, details, state);
    }

    fn set_activity(
        &self,
        name: impl Into<String>,
        details: impl Into<String>,
        state: impl Into<String>,
    ) {
        let activity = Activity {
            name: truncate_utf8(name.into().trim(), 128),
            details: truncate_utf8(details.into().trim(), 128),
            state: truncate_utf8(state.into().trim(), 128),
            started_at: unix_time(),
        };
        let _ = self.tx.send(Command::Activity(activity));
    }

    pub fn stop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn idle_activity(
    settings: &crate::config::SettingsCfg,
    rocket_league_open: bool,
) -> (String, String) {
    if !rocket_league_open {
        return (String::new(), String::new());
    }
    if !settings.discord_game_state {
        return (
            nonempty(&settings.discord_custom_message, "Playing Rocket League"),
            String::new(),
        );
    }
    ("In Rocket League".to_string(), "Main menu".to_string())
}

fn match_activity(settings: &crate::config::SettingsCfg, info: &MatchInfo) -> (String, String) {
    let details = if settings.discord_game_state {
        "In a match".to_string()
    } else {
        nonempty(&settings.discord_custom_message, "Playing Rocket League")
    };
    let mut fields = Vec::with_capacity(3);
    if settings.discord_game_state {
        if settings.discord_show_score && !info.score.is_empty() {
            fields.push(format!("Score: {}", info.score));
        }
        if settings.discord_show_map && !info.map.is_empty() {
            fields.push(info.map.clone());
        }
        if settings.discord_show_gamemode && !info.gamemode.is_empty() {
            fields.push(info.gamemode.clone());
        }
    }
    (details, fields.join(" • "))
}

impl Drop for DiscordPresence {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_worker(rx: Receiver<Command>, mut enabled: bool) {
    let mut desired: Option<Activity> = None;
    let mut sent: Option<Activity> = None;
    let mut pipe: Option<File> = None;
    let mut nonce = 0_u64;

    loop {
        match rx.recv_timeout(RETRY_INTERVAL) {
            Ok(Command::Configure(next_enabled)) => {
                if enabled != next_enabled {
                    clear_activity(pipe.as_mut(), &mut nonce);
                    pipe = None;
                    sent = None;
                    enabled = next_enabled;
                }
            }
            Ok(Command::Activity(activity)) => {
                if desired.as_ref() != Some(&activity) {
                    desired = Some(activity);
                }
            }
            Ok(Command::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                clear_activity(pipe.as_mut(), &mut nonce);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        if !enabled {
            continue;
        }

        if pipe.is_none() {
            match connect() {
                Ok(open_pipe) => {
                    tracing::info!("Discord Rich Presence connected");
                    pipe = Some(open_pipe);
                    sent = None;
                }
                Err(_) => continue,
            }
        }

        if desired != sent {
            let result = send_activity(
                pipe.as_mut().expect("pipe was connected above"),
                desired.as_ref(),
                &mut nonce,
            );
            match result {
                Ok(()) => sent = desired.clone(),
                Err(error) => {
                    tracing::debug!(%error, "Discord Rich Presence disconnected");
                    pipe = None;
                    sent = None;
                }
            }
        }
    }
}

fn clear_activity(pipe: Option<&mut File>, nonce: &mut u64) {
    if let Some(pipe) = pipe {
        let _ = send_activity(pipe, None, nonce);
    }
}

fn connect() -> io::Result<File> {
    let mut last_error = None;
    for index in 0..10 {
        let path = format!(r"\\.\pipe\discord-ipc-{index}");
        match OpenOptions::new().read(true).write(true).open(path) {
            Ok(mut pipe) => {
                let handshake = serde_json::json!({"v": 1, "client_id": APPLICATION_ID});
                write_frame(&mut pipe, 0, &handshake)?;
                return Ok(pipe);
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error
        .unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Discord IPC pipe not found")))
}

fn send_activity(pipe: &mut File, activity: Option<&Activity>, nonce: &mut u64) -> io::Result<()> {
    *nonce = nonce.wrapping_add(1);
    let activity = activity.map(activity_json);
    let payload = serde_json::json!({
        "cmd": "SET_ACTIVITY",
        "args": {"pid": std::process::id(), "activity": activity},
        "nonce": nonce.to_string(),
    });
    write_frame(pipe, 1, &payload)
}

fn activity_json(activity: &Activity) -> serde_json::Value {
    let mut value = serde_json::json!({
        "name": activity.name,
        "type": 0,
        "details": activity.details,
        "state": activity.state,
        "timestamps": {"start": activity.started_at},
        "buttons": [
            {"label": "Hebnix Website", "url": WEBSITE_URL},
            {"label": "Hebnix Discord", "url": COMMUNITY_URL},
        ],
        "instance": false,
    });
    if activity.state.is_empty() {
        value
            .as_object_mut()
            .expect("activity payload must be an object")
            .remove("state");
    }
    if activity.details.is_empty() {
        value
            .as_object_mut()
            .expect("activity payload must be an object")
            .remove("details");
    }
    value
}

fn write_frame(pipe: &mut File, opcode: u32, payload: &serde_json::Value) -> io::Result<()> {
    let body = serde_json::to_vec(payload).map_err(io::Error::other)?;
    let length = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Discord payload is too large"))?;
    pipe.write_all(&opcode.to_le_bytes())?;
    pipe.write_all(&length.to_le_bytes())?;
    pipe.write_all(&body)?;
    pipe.flush()
}

fn nonempty(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn display_score(teams: &[hebnix_sdk::stats::models::TeamState]) -> String {
    let mut teams = teams.iter().collect::<Vec<_>>();
    teams.sort_by_key(|team| team.team_num);
    match teams.as_slice() {
        [blue, orange, ..] => {
            let blue_name = nonempty(&blue.name, "Blue");
            let orange_name = nonempty(&orange.name, "Orange");
            format!("{blue_name} {}–{} {orange_name}", blue.score, orange.score)
        }
        [team] => format!("{}", team.score),
        _ => String::new(),
    }
}

fn display_gamemode(game: Option<&hebnix_sdk::log::LogGameInfo>) -> String {
    let Some(game) = game else {
        return "Private Match".to_string();
    };
    if let Some(title) = game
        .playlist_name
        .as_deref()
        .filter(|title| !title.trim().is_empty())
    {
        return title.trim().to_string();
    }
    if let Some(name) = game.playlist_id.and_then(playlist_fallback) {
        return name.to_string();
    }
    let class = game
        .game_class
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if class.contains("basketball") {
        "Hoops"
    } else if class.contains("breakout") {
        "Dropshot"
    } else if class.contains("hockey") {
        "Snow Day"
    } else if class.contains("godball") {
        "Heatseeker"
    } else if class.contains("rumble") || class.contains("items") {
        "Rumble"
    } else if class.contains("knockout") {
        "Knockout"
    } else if class.contains("gridiron") {
        "Gridiron"
    } else if class.contains("soccar") {
        "Soccar"
    } else if game.offline {
        "Private Match"
    } else {
        "Rocket League"
    }
    .to_string()
}

fn playlist_fallback(id: i64) -> Option<&'static str> {
    Some(match id {
        1 => "Duel",
        2 => "Doubles",
        3 => "Standard",
        4 => "Chaos",
        6 => "Private Match",
        10 => "Ranked Duel",
        11 => "Ranked Doubles",
        13 => "Ranked Standard",
        15 | 30 => "Snow Day",
        16 | 35 => "Rocket Labs",
        17 | 27 => "Hoops",
        23 | 29 => "Dropshot",
        25 | 28 => "Rumble",
        34 => "Tournament Match",
        37 => "Dropshot Rumble",
        38 | 43 | 63 => "Heatseeker",
        44 => "Knockout",
        48 => "Tactical Rumble",
        49 => "Spring Loaded",
        _ => return None,
    })
}

/// Convert the cooked-package identifier emitted by StatsAPI into the arena
/// name shown in Rocket League. The standard set comes from the supported arena
/// catalog; special/Labs identifiers are supplemented by RLBot's map enum.
pub fn display_map_name(internal: &str) -> String {
    let leaf = internal
        .rsplit(|character| character == '/' || character == char::from(92))
        .next()
        .unwrap_or(internal)
        .trim_end_matches(".upk");
    let key = leaf.to_ascii_lowercase();
    let known = match key.as_str() {
        "stadium_p" => "DFH Stadium",
        "stadium_day_p" => "DFH Stadium (Day)",
        "stadium_foggy_p" => "DFH Stadium (Stormy)",
        "stadium_winter_p" => "DFH Stadium (Snowy)",
        "eurostadium_p" => "Mannfield",
        "eurostadium_night_p" => "Mannfield (Night)",
        "eurostadium_dusk_p" => "Mannfield (Dusk)",
        "eurostadium_rainy_p" => "Mannfield (Stormy)",
        "eurostadium_snownight_p" => "Mannfield (Snowy)",
        "utopiastadium_p" => "Utopia Coliseum",
        "utopiastadium_dusk_p" => "Utopia Coliseum (Dusk)",
        "utopiastadium_snow_p" => "Utopia Coliseum (Snowy)",
        "utopiastadium_lux_p" => "Utopia Coliseum (Gilded)",
        "trainstation_p" => "Urban Central",
        "trainstation_night_p" => "Urban Central (Night)",
        "trainstation_dawn_p" => "Urban Central (Dawn)",
        "haunted_trainstation_p" => "Urban Central (Haunted)",
        "park_p" => "Beckwith Park",
        "park_night_p" => "Beckwith Park (Midnight)",
        "park_rainy_p" => "Beckwith Park (Stormy)",
        "park_snowy_p" => "Beckwith Park (Snowy)",
        "park_bman_p" => "Beckwith Park (Gotham Night)",
        "outlaw_p" => "Deadeye Canyon",
        "uf_night_p" => "Futura Garden (Night)",
        "uf_day_p" => "Futura Garden (Day)",
        "street_p" => "Sovereign Heights",
        "farm_p" => "Farmstead",
        "farm_night_p" => "Farmstead (Night)",
        "farm_grs_p" => "Farmstead (Pitched)",
        "farm_hw_p" => "Farmstead (Spooky)",
        "paname_dusk_p" => "Parc de Paris",
        "cs_p" => "Champions Field",
        "cs_day_p" => "Champions Field (Day)",
        "cs_hw_p" => "Rivals Arena",
        "bb_p" => "Champions Field (NFL)",
        "beach_p" => "Salty Shores",
        "beach_night_p" => "Salty Shores (Night)",
        "neotokyo_standard_p" => "Neo Tokyo",
        "neotokyo_p" => "Tokyo Underpass",
        "underwater_p" => "AquaDome",
        "underwater_grs_p" => "AquaDome (Salty Shallows)",
        "wasteland_s_p" => "Wasteland",
        "wasteland_night_s_p" => "Wasteland (Night)",
        "wasteland_p" => "Badlands",
        "wasteland_night_p" => "Badlands (Night)",
        "chn_stadium_p" => "Forbidden Temple",
        "chn_stadium_day_p" => "Forbidden Temple (Day)",
        "arc_standard_p" => "Starbase ARC",
        "arc_darc_p" => "Starbase ARC (Aftermath)",
        "arc_p" => "Arctagon",
        "music_p" => "Neon Fields",
        "woods_p" => "Drift Woods",
        "woods_night_p" => "Drift Woods (Night)",
        "mall_day_p" => "Boostfield Mall",
        "ff_dusk_p" => "Estadio Vida",
        "hoopsstadium_p" => "Dunk House",
        "shattershot_p" => "Core 707",
        "throwbackstadium_p" => "Throwback Stadium",
        "stadium_race_day_p" => "DFH Stadium (Circuit)",
        "labs_circlepillars_p" => "Pillars",
        "labs_cosmic_v4_p" => "Cosmic",
        "labs_doublegoal_v2_p" => "Double Goal",
        "labs_octagon_02_p" => "Octagon",
        "labs_underpass_p" => "Underpass",
        "labs_utopia_p" => "Utopia Retro",
        _ => return prettify_unknown_map(leaf),
    };
    known.to_string()
}

fn prettify_unknown_map(internal: &str) -> String {
    let value = internal
        .strip_suffix("_P")
        .or_else(|| internal.strip_suffix("_p"))
        .unwrap_or(internal)
        .replace('_', " ");
    value
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_use_player_facing_names() {
        assert_eq!(display_map_name("Park_Night_P"), "Beckwith Park (Midnight)");
        assert_eq!(display_map_name("Paname_Dusk_P"), "Parc de Paris");
        assert_eq!(display_map_name("HoopsStadium_P"), "Dunk House");
        assert_eq!(display_map_name("Labs_Underpass_P.upk"), "Underpass");
        assert_eq!(display_map_name("Future_Test_P"), "Future Test");
    }

    #[test]
    fn match_info_uses_playlist_map_and_ordered_score() {
        let state = hebnix_sdk::stats::models::UpdateStateData {
            game: hebnix_sdk::stats::models::GameState {
                arena: "EuroStadium_Night_P".to_string(),
                teams: vec![
                    hebnix_sdk::stats::models::TeamState {
                        name: "Orange".to_string(),
                        team_num: 1,
                        score: 2,
                        ..Default::default()
                    },
                    hebnix_sdk::stats::models::TeamState {
                        name: "Blue".to_string(),
                        team_num: 0,
                        score: 3,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            ..Default::default()
        };
        let game = hebnix_sdk::log::LogGameInfo {
            playlist_name: Some("Ranked Doubles".to_string()),
            ..Default::default()
        };
        let info = MatchInfo::from_state(&state, Some(&game));
        assert_eq!(info.gamemode, "Ranked Doubles");
        assert_eq!(info.map, "Mannfield (Night)");
        assert_eq!(info.score, "Blue 3–2 Orange");
    }

    #[test]
    fn truncation_preserves_utf8_boundaries() {
        let value = "a".repeat(127) + "é";
        let truncated = truncate_utf8(&value, 128);
        assert_eq!(truncated.len(), 127);
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    #[test]
    fn disabled_game_state_does_not_publish_match_fields() {
        let settings = crate::config::SettingsCfg {
            discord_game_state: false,
            discord_custom_message: "My custom message".to_string(),
            ..Default::default()
        };
        let info = MatchInfo {
            gamemode: "Private Match".to_string(),
            map: "Sovereign Heights".to_string(),
            score: "Blue 9–8 Orange".to_string(),
        };

        let (details, state) = match_activity(&settings, &info);
        let payload = activity_json(&Activity {
            name: ROCKET_LEAGUE_DISPLAY_NAME.to_string(),
            details: details.clone(),
            state: state.clone(),
            started_at: 0,
        });

        assert_eq!(details, "My custom message");
        assert!(state.is_empty());
        assert!(payload.get("state").is_none());
    }

    #[test]
    fn enabled_game_state_does_not_duplicate_gamemode() {
        let settings = crate::config::SettingsCfg::default();
        let info = MatchInfo {
            gamemode: "Private Match".to_string(),
            map: "Sovereign Heights".to_string(),
            score: "Blue 9–8 Orange".to_string(),
        };
        let (details, state) = match_activity(&settings, &info);

        assert_eq!(details, "In a match");
        assert_eq!(state.matches("Private Match").count(), 1);
    }

    #[test]
    fn activity_payload_contains_fixed_buttons() {
        let payload = activity_json(&Activity {
            name: ROCKET_LEAGUE_DISPLAY_NAME.to_string(),
            details: "Playing Rocket League".to_string(),
            state: String::new(),
            started_at: 0,
        });

        assert_eq!(
            payload["buttons"],
            serde_json::json!([
                {"label": "Hebnix Website", "url": WEBSITE_URL},
                {"label": "Hebnix Discord", "url": COMMUNITY_URL},
            ])
        );
    }

    #[test]
    fn disabled_game_state_omits_main_menu() {
        let settings = crate::config::SettingsCfg {
            discord_game_state: false,
            discord_custom_message: "Developing Hebnix".to_string(),
            ..Default::default()
        };

        assert_eq!(
            idle_activity(&settings, true),
            ("Developing Hebnix".to_string(), String::new())
        );
    }

    #[test]
    fn closed_rocket_league_publishes_hebnix_only() {
        let settings = crate::config::SettingsCfg::default();
        let (details, state) = idle_activity(&settings, false);
        let payload = activity_json(&Activity {
            name: HEBNIX_DISPLAY_NAME.to_string(),
            details,
            state,
            started_at: 0,
        });

        assert_eq!(payload["name"], HEBNIX_DISPLAY_NAME);
        assert!(payload.get("details").is_none());
        assert!(payload.get("state").is_none());
    }
}
