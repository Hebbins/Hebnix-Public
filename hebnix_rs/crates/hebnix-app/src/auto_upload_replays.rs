use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use eframe::egui;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::ReplayUploadCfg;
use crate::messages::AppMsg;

const HISTORY_FILE: &str = "replay_upload_history.json";
const MATCH_HISTORY_SERVICE: &str = "Matches/GetMatchHistory v1";
const POLL_INTERVAL: Duration = Duration::from_secs(30);
const POLL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_REPLAY_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadHistoryEntry {
    pub match_guid: String,
    pub replay_id: String,
    pub location: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub warning: String,
    #[serde(default)]
    pub duplicate: bool,
}

#[derive(Debug)]
pub struct UploadResult {
    pub match_guid: String,
    pub result: Result<UploadHistoryEntry, String>,
}

#[derive(Debug, Clone)]
struct TrackedMatch {
    match_guid: String,
    player_id: String,
}

pub struct AutoUploadReplays {
    base_dir: PathBuf,
    running: bool,
    starting: bool,
    current_match: Option<TrackedMatch>,
    submitted: HashSet<String>,
    pending: HashSet<String>,
    history: Vec<UploadHistoryEntry>,
    status: String,
    generation: Arc<AtomicU64>,
}

impl AutoUploadReplays {
    pub fn new(base_dir: &Path) -> Self {
        let history = std::fs::read(base_dir.join(HISTORY_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<UploadHistoryEntry>>(&bytes).ok())
            .unwrap_or_default()
            .into_iter()
            .take(20)
            .collect();
        Self {
            base_dir: base_dir.to_path_buf(),
            running: false,
            starting: false,
            current_match: None,
            submitted: HashSet::new(),
            pending: HashSet::new(),
            history,
            status: "Stopped".to_string(),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn running(&self) -> bool {
        self.running
    }

    pub fn starting(&self) -> bool {
        self.starting
    }

    pub fn begin_start(&mut self) -> Result<(), String> {
        if self.starting || self.running {
            return Ok(());
        }
        self.starting = true;
        self.status = "Enabling RLAPI capture...".to_string();
        Ok(())
    }

    pub fn finish_start(&mut self, result: &Result<(), String>) {
        self.starting = false;
        match result {
            Ok(()) => {
                self.running = true;
                self.status = "Running · waiting for a match".to_string();
            }
            Err(error) => {
                self.running = false;
                self.status = format!("Could not start: {error}");
            }
        }
    }

    pub fn stop(&mut self) {
        self.running = false;
        self.starting = false;
        self.current_match = None;
        self.pending.clear();
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.status = "Stopped".to_string();
    }

    pub fn track_match(&mut self, match_guid: &str, player_id: Option<String>) {
        if !self.running || match_guid.trim().is_empty() {
            return;
        }
        let guid = normalize_guid(match_guid);
        if self.submitted.contains(&guid) {
            return;
        }
        let Some(player_id) = player_id.filter(|value| !value.trim().is_empty()) else {
            self.status = "Match detected, but the local PlayerID was not found".to_string();
            return;
        };
        if self
            .current_match
            .as_ref()
            .is_none_or(|tracked| tracked.match_guid != guid)
        {
            self.status = format!("Tracking match {guid}");
            self.current_match = Some(TrackedMatch {
                match_guid: guid,
                player_id,
            });
        }
    }

    pub fn submit_finished(
        &mut self,
        event_guid: Option<&str>,
        config: &ReplayUploadCfg,
        tx: Sender<AppMsg>,
        ctx: egui::Context,
    ) -> Option<String> {
        if !self.running {
            return None;
        }
        let tracked = self.current_match.as_ref()?;
        if let Some(event_guid) = event_guid {
            if normalize_guid(event_guid) != tracked.match_guid {
                return None;
            }
        }
        let tracked = self.current_match.take()?;
        if self.submitted.contains(&tracked.match_guid) {
            return None;
        }
        self.submitted.insert(tracked.match_guid.clone());
        self.pending.insert(tracked.match_guid.clone());
        self.status = format!("Waiting for replay {}", tracked.match_guid);

        let debug_message = config
            .debug_logging
            .then(|| format!("[Replay Upload] polling match {}", tracked.match_guid));
        let job = UploadJob {
            tracked,
            api_key: config.api_key.trim().to_string(),
            naming_template: config.naming_template.trim().to_string(),
            visibility: config.visibility.clone(),
            group_id: config.group_id.trim().to_string(),
            generation: Arc::clone(&self.generation),
            expected_generation: self.generation.load(Ordering::Acquire),
        };
        std::thread::Builder::new()
            .name("replay-auto-upload".to_string())
            .spawn(move || {
                let match_guid = job.tracked.match_guid.clone();
                let result = run_upload(job);
                let _ = tx.send(AppMsg::ReplayUploadFinished(UploadResult {
                    match_guid,
                    result,
                }));
                ctx.request_repaint();
            })
            .ok();
        debug_message
    }

    pub fn finish_upload(&mut self, result: UploadResult) {
        // A stopped run invalidates its workers. Ignore their eventual wake-up
        // instead of replacing the visible "Stopped" state with a stale error.
        if !self.pending.remove(&result.match_guid) {
            return;
        }
        match result.result {
            Ok(entry) => {
                self.status = if entry.duplicate {
                    format!("Replay already existed: {}", entry.match_guid)
                } else {
                    format!("Uploaded replay {}", entry.match_guid)
                };
                self.history
                    .retain(|item| item.match_guid != entry.match_guid);
                self.history.insert(0, entry);
                self.history.truncate(20);
                self.save_history();
            }
            Err(error) => self.status = format!("Upload failed for {}: {error}", result.match_guid),
        }
    }

    fn save_history(&self) {
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.history) {
            let _ = std::fs::write(self.base_dir.join(HISTORY_FILE), bytes);
        }
    }

    pub fn show(&self, ui: &mut egui::Ui, session_status: &str) {
        ui.heading("Auto Upload Replays");
        ui.label("Automatically uploads only matches detected while this feature is running.");
        ui.add_space(8.0);
        ui.label(&self.status);
        ui.small(session_status);
        if !self.pending.is_empty() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("{} replay(s) pending", self.pending.len()));
            });
        }
        ui.add_space(12.0);
        ui.heading("Last 20 uploaded matches");
        if self.history.is_empty() {
            ui.label("No replays uploaded yet.");
        } else {
            egui::ScrollArea::vertical()
                .id_salt("replay_upload_history")
                .show(ui, |ui| {
                    for entry in &self.history {
                        ui.horizontal_wrapped(|ui| {
                            ui.monospace(&entry.match_guid);
                            ui.label(if entry.duplicate {
                                "Already uploaded"
                            } else {
                                "Uploaded"
                            });
                            ui.hyperlink_to("Open on ballchasing.com", &entry.location);
                        });
                        if !entry.title.is_empty() {
                            ui.label(&entry.title);
                        }
                        if !entry.warning.is_empty() {
                            ui.colored_label(egui::Color32::YELLOW, &entry.warning);
                        }
                        ui.separator();
                    }
                });
        }
    }
}

struct UploadJob {
    tracked: TrackedMatch,
    api_key: String,
    naming_template: String,
    visibility: String,
    group_id: String,
    generation: Arc<AtomicU64>,
    expected_generation: u64,
}

impl UploadJob {
    fn cancelled(&self) -> bool {
        self.generation.load(Ordering::Acquire) != self.expected_generation
    }
}

fn run_upload(job: UploadJob) -> Result<UploadHistoryEntry, String> {
    if job.api_key.is_empty() {
        return Err("enter a ballchasing.com API key".to_string());
    }
    let deadline = Instant::now() + POLL_TIMEOUT;
    let mut first_attempt = true;
    loop {
        if job.cancelled() {
            return Err("stopped".to_string());
        }
        if !first_attempt && Instant::now() >= deadline {
            return Err("replay did not appear in match history within 5 minutes".to_string());
        }
        first_attempt = false;
        let attempt_started = Instant::now();
        let response = hebnix_sdk::rlapi::session::shared_game_session().request(
            MATCH_HISTORY_SERVICE,
            serde_json::json!({ "PlayerID": job.tracked.player_id }),
        );
        if let Ok(value) = response {
            if let Some(entry) = match_entry(&value, &job.tracked.match_guid) {
                let replay_url = entry
                    .get("ReplayUrl")
                    .and_then(Value::as_str)
                    .filter(|url| !url.is_empty())
                    .ok_or("match history entry did not include a replay URL")?;
                let title = render_replay_title(&job.naming_template, entry);
                return download_and_upload(&job, replay_url, &title);
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return Err("replay did not appear in match history within 5 minutes".to_string());
        }
        let next_attempt = (attempt_started + POLL_INTERVAL).min(deadline);
        std::thread::sleep(next_attempt.saturating_duration_since(now));
    }
}

fn match_entry<'a>(value: &'a Value, match_guid: &str) -> Option<&'a Value> {
    value.get("Matches")?.as_array()?.iter().find_map(|entry| {
        let guid = entry.get("Match")?.get("MatchGUID")?.as_str()?;
        if normalize_guid(guid) == normalize_guid(match_guid) {
            Some(entry)
        } else {
            None
        }
    })
}

fn download_and_upload(
    job: &UploadJob,
    replay_url: &str,
    title: &str,
) -> Result<UploadHistoryEntry, String> {
    let replay_url = validate_replay_url(replay_url)?;
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(90))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(replay_url)
        .send()
        .map_err(|error| format!("could not download replay: {error}"))?
        .error_for_status()
        .map_err(|error| format!("could not download replay: {error}"))?;
    if response
        .content_length()
        .is_some_and(|size| size > MAX_REPLAY_BYTES)
    {
        return Err("replay download was unexpectedly large".to_string());
    }
    let replay = response
        .bytes()
        .map_err(|error| format!("could not read replay: {error}"))?;
    if replay.len() as u64 > MAX_REPLAY_BYTES {
        return Err("replay download was unexpectedly large".to_string());
    }
    if job.cancelled() {
        return Err("stopped".to_string());
    }

    let mut url = reqwest::Url::parse("https://ballchasing.com/api/v2/upload")
        .map_err(|error| error.to_string())?;
    url.query_pairs_mut()
        .append_pair("visibility", visibility(&job.visibility));
    if !job.group_id.is_empty() {
        url.query_pairs_mut().append_pair("group", &job.group_id);
    }
    let part = reqwest::blocking::multipart::Part::bytes(replay.to_vec())
        .file_name(format!("{}.replay", job.tracked.match_guid))
        .mime_str("application/octet-stream")
        .map_err(|error| error.to_string())?;
    let response = client
        .post(url)
        .header(reqwest::header::AUTHORIZATION, &job.api_key)
        .multipart(reqwest::blocking::multipart::Form::new().part("file", part))
        .send()
        .map_err(|error| format!("ballchasing upload failed: {error}"))?;
    let status = response.status();
    let response_text = response
        .text()
        .map_err(|error| format!("could not read ballchasing response ({status}): {error}"))?;
    let body: Value = serde_json::from_str(&response_text)
        .map_err(|error| format!("ballchasing returned invalid JSON ({status}): {error}"))?;
    if status.as_u16() != 201 && status.as_u16() != 409 {
        let detail = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("request rejected");
        return Err(format!("ballchasing returned {status}: {detail}"));
    }
    let replay_id = body
        .get("id")
        .and_then(Value::as_str)
        .ok_or("ballchasing response did not include a replay id")?
        .to_string();
    let location = body
        .get("location")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://ballchasing.com/replay/{replay_id}"));
    let warning = if title.is_empty() {
        String::new()
    } else {
        patch_replay_title(&client, &job.api_key, &replay_id, title)
            .err()
            .map(|error| format!("Replay uploaded, but its name could not be set: {error}"))
            .unwrap_or_default()
    };
    Ok(UploadHistoryEntry {
        match_guid: job.tracked.match_guid.clone(),
        replay_id,
        location,
        title: title.to_string(),
        warning,
        duplicate: status.as_u16() == 409,
    })
}

fn patch_replay_title(
    client: &reqwest::blocking::Client,
    api_key: &str,
    replay_id: &str,
    title: &str,
) -> Result<(), String> {
    let response = client
        .patch(format!("https://ballchasing.com/api/replays/{replay_id}"))
        .header(reqwest::header::AUTHORIZATION, api_key)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "title": title }).to_string())
        .send()
        .map_err(|error| error.to_string())?;
    if response.status().as_u16() == 204 {
        Ok(())
    } else {
        Err(format!("ballchasing returned {}", response.status()))
    }
}

fn render_replay_title(template: &str, entry: &Value) -> String {
    let details = entry.get("Match").unwrap_or(&Value::Null);
    let timestamp = details
        .get("RecordStartTimestamp")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let winning_team = match details.get("WinningTeam").and_then(Value::as_i64) {
        Some(0) => "Blue",
        Some(1) => "Orange",
        _ => "Unknown",
    };
    let playlist = details
        .get("Playlist")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let match_guid = details
        .get("MatchGUID")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let map = details
        .get("MapName")
        .and_then(Value::as_str)
        .unwrap_or("Unknown Map");
    let (time, time24) = local_times(timestamp);
    template
        .replace("{date}", &unix_date(timestamp))
        .replace("{time24}", &time24)
        .replace("{time}", &time)
        .replace("{winning_team}", winning_team)
        .replace("{gamemode}", gamemode_name(playlist))
        .replace("{map}", map)
        .replace("{match_guid}", match_guid)
        .trim()
        .chars()
        .take(200)
        .collect()
}

fn local_times(timestamp: i64) -> (String, String) {
    let Ok(utc) = time::OffsetDateTime::from_unix_timestamp(timestamp) else {
        return ("12:00 AM".to_string(), "00:00".to_string());
    };
    let offset = time::UtcOffset::local_offset_at(utc).unwrap_or(time::UtcOffset::UTC);
    let local = utc.to_offset(offset);
    let hour24 = local.hour();
    let minute = local.minute();
    let suffix = if hour24 < 12 { "AM" } else { "PM" };
    let hour12 = match hour24 % 12 {
        0 => 12,
        hour => hour,
    };
    (
        format!("{hour12}:{minute:02} {suffix}"),
        format!("{hour24:02}:{minute:02}"),
    )
}

fn gamemode_name(playlist: i64) -> &'static str {
    match playlist {
        1 | 10 => "1v1",
        2 | 11 => "2v2",
        3 | 13 => "3v3",
        4 => "Chaos",
        6 => "Private Match",
        7 => "Season",
        12 => "Solo Standard",
        15 | 30 => "Snow Day",
        16 | 27 => "Hoops",
        17 | 28 => "Rumble",
        18 | 29 => "Dropshot",
        22 => "Tournament",
        63 => "Heatseeker",
        _ => "Unknown Mode",
    }
}

fn unix_date(timestamp: i64) -> String {
    // Convert Unix days to a proleptic Gregorian date without pulling a date
    // library into the desktop binary.
    let days = timestamp.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn validate_replay_url(value: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| "PsyNet returned an invalid replay URL".to_string())?;
    let trusted_host = url
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("api.rlpp.psynet.gg"));
    if !trusted_host || !matches!(url.scheme(), "http" | "https") {
        return Err("PsyNet returned an invalid replay URL".to_string());
    }
    Ok(url)
}

fn visibility(value: &str) -> &'static str {
    match value {
        "public" => "public",
        "unlisted" => "unlisted",
        _ => "private",
    }
}

fn normalize_guid(value: &str) -> String {
    value.trim().to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_only_the_requested_match() {
        let value = serde_json::json!({
            "Matches": [
                {"ReplayUrl": "https://wrong", "Match": {"MatchGUID": "OLD"}},
                {"ReplayUrl": "https://right", "Match": {"MatchGUID": "abc123"}}
            ]
        });
        assert_eq!(
            match_entry(&value, "ABC123")
                .and_then(|entry| entry.get("ReplayUrl"))
                .and_then(Value::as_str),
            Some("https://right")
        );
        assert!(match_entry(&value, "missing").is_none());
    }

    #[test]
    fn visibility_is_fail_closed() {
        assert_eq!(visibility("public"), "public");
        assert_eq!(visibility("unlisted"), "unlisted");
        assert_eq!(visibility("anything-else"), "private");
    }

    #[test]
    fn accepts_psynet_http_replay_urls_only() {
        assert!(
            validate_replay_url(
                "http://api.rlpp.psynet.gg/Match.replay?MatchGUID=ABC&Timestamp=123"
            )
            .is_ok()
        );
        assert!(validate_replay_url("https://api.rlpp.psynet.gg/Match.replay").is_ok());
        assert!(validate_replay_url("http://example.com/Match.replay").is_err());
        assert!(validate_replay_url("file:///C:/secret").is_err());
    }

    #[test]
    fn expands_replay_name_placeholders() {
        let entry = serde_json::json!({
            "Match": {
                "MatchGUID": "ABC123",
                "RecordStartTimestamp": 1_790_685_544_i64,
                "WinningTeam": 1,
                "Playlist": 11,
                "MapName": "Stadium_P"
            }
        });
        let (time, time24) = local_times(1_790_685_544_i64);
        let title = render_replay_title(
            "{date} {time} {time24} {winning_team} My Replay - {gamemode} {map} {match_guid}",
            &entry,
        );
        assert_eq!(
            title,
            format!("2026-09-29 {time} {time24} Orange My Replay - 2v2 Stadium_P ABC123")
        );
    }
}
