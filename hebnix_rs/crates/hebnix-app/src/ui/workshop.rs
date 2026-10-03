//! workshop maps tab: browse the hebnix.com catalog, download + swap maps
//! over the rocket labs placeholders.

use crate::i18n::{t, t_args};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::Sender;
use eframe::egui;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::messages::AppMsg;
use crate::multiplayer_lan::{
    HostSession, LocalInfo, MAP_SYNC_PORT, MAX_MAP_BYTES, MapFileProvider, MapProvider, PeerOffer,
    RL_LAN_PORT, RoomClient, SlotMap, TSNET_CONTROL_URL, TransferProgress, TsnetSidecarHandle,
    ensure_beacon_relay_rule, ensure_map_sync_rule, ensure_rocket_league_lan_rule,
    ensure_sidecar_rule, fetch_map_file, find_sidecar_dir, is_local_map_id, redact, valid_map_id,
};
mod archive;
mod background_changer;
mod local_import;
mod steam_download;
use archive::ArchiveBrowser;
use background_changer::BackgroundChangerState;
use steam_download::SteamDownloader;

/// common Workshop multiplayer problems (VPNs/proxies, firewalls) and fixes
const MULTIPLAYER_HELP_URL: &str = "https://github.com/xplodingeggo/hebnix-linux/blob/feature/tsnet-multiplayer/docs/workshop-multiplayer-help.md"; // TEST ONLY: point at HebnixRL/Hebnix-Public main once merged
use local_import::{
    ImportWizard, LocalMap, is_local_entry, load_local_maps, record_received_map, remove_local_map,
};

const MULTIHOME_CHECK_MAX_ATTEMPTS: u8 = 30;
const MULTIPLAYER_SETTINGS_FILE: &str = "multiplayer_settings.json";
const MULTIHOME_CHECK_INTERVAL: Duration = Duration::from_secs(2);

// api.hebnix.com flakes on connect now and then, so retry transport failures a
// few times (real http errors bail immediately).
fn get_retry(url: &str, timeout: Duration) -> Result<ureq::Response, String> {
    let mut last = String::new();
    for attempt in 0..3 {
        match ureq::get(url).timeout(timeout).call() {
            Ok(r) => return Ok(r),
            Err(e @ ureq::Error::Status(..)) => return Err(e.to_string()),
            Err(e) => {
                last = e.to_string();
                if attempt < 2 {
                    std::thread::sleep(Duration::from_millis(600 * (attempt + 1)));
                }
            }
        }
    }
    Err(last)
}

// ureq's .timeout() caps the whole request including reading the body, so a big
// map on a slow link died with "timed out reading response" even while bytes
// were still coming in. downloads only time out on connect and on a stalled read.
fn get_download(url: &str) -> Result<ureq::Response, String> {
    let mut last = String::new();
    for attempt in 0..3 {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(30))
            .build();
        match agent.get(url).call() {
            Ok(r) => return Ok(r),
            Err(e @ ureq::Error::Status(..)) => return Err(e.to_string()),
            Err(e) => {
                last = e.to_string();
                if attempt < 2 {
                    std::thread::sleep(Duration::from_millis(600 * (attempt + 1)));
                }
            }
        }
    }
    Err(last)
}

fn rocket_league_executable(rl_path: &str) -> Result<PathBuf, String> {
    let root = Path::new(rl_path);
    let candidates = [
        root.join("TAGame")
            .join("Binaries")
            .join("Win64")
            .join("RocketLeague.exe"),
        root.join("Binaries").join("Win64").join("RocketLeague.exe"),
    ];
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| "Could not find RocketLeague.exe in the configured game folder.".to_string())
}

fn multiplayer_client_token() -> String {
    use rand::RngCore;

    let path = crate::config::base_dir()
        .join("state")
        .join("multiplayer_player_token.txt");
    if let Ok(token) = std::fs::read_to_string(&path) {
        let token = token.trim();
        if token.len() == 64 && token.chars().all(|character| character.is_ascii_hexdigit()) {
            return token.to_string();
        }
    }
    let mut bytes = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, &token);
    token
}

fn rocket_league_launched_with_multihome(address: &str) -> bool {
    rocket_league_multihome_address().is_some_and(|found| found == address)
}

// no subnet filtering here anymore: tailnet addresses are assigned
// dynamically by headscale, not drawn from a fixed prefix Hebnix can
// recognize, so this just returns whatever -multihome value RL was last
// launched with and lets callers compare it against the address they
// actually expect.
fn rocket_league_multihome_address() -> Option<String> {
    let path = dirs::document_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("My Games")
        .join("Rocket League")
        .join("TAGame")
        .join("Logs")
        .join("Launch.log");
    let log = std::fs::read_to_string(path).ok()?;
    log.lines().take(300).find_map(|line| {
        let line = line.to_ascii_lowercase();
        let start = line.find("-multihome=")? + "-multihome=".len();
        let address: String = line[start..]
            .chars()
            .take_while(|character| character.is_ascii_digit() || *character == '.')
            .collect();
        (!address.is_empty()).then_some(address)
    })
}

pub const WORKSHOP_PLUGIN_ID: &str = "workshop_map_loader";
pub const WORKSHOP_MODS_DIR_NAME: &str = "mods";
pub const REMOTE_FILES_BASE: &str = "https://hebnix.com";
pub const API_ENDPOINT: &str = "https://api.hebnix.com/maps";
pub const DOWNLOAD_ENDPOINT_BASE: &str = "https://api.hebnix.com/download/map/";

pub const TARGET_MAPS: [(&str, &str); 4] = [
    ("Utopia Retro", "Labs_Utopia_P.upk"),
    ("Underpass", "Labs_Underpass_P.upk"),
    ("Roadblock", "Labs_Octagon_B2B_02_P.upk"),
    ("Hourglass", "Labs_PillarGlass_P.upk"),
];

fn target_filename(target: &str) -> Option<&'static str> {
    TARGET_MAPS
        .iter()
        .find(|(name, _)| *name == target)
        .map(|(_, file)| *file)
}

// Map manager (shared with worker threads)

#[derive(Clone)]
pub struct MapManager {
    pub cache_dir: PathBuf,
    pub runtime_dir: PathBuf,
    active_maps: Arc<Mutex<serde_json::Map<String, Value>>>,
}

impl MapManager {
    pub fn new(base_dir: &Path) -> Self {
        let cache_dir = base_dir
            .join("plugins")
            .join("cache")
            .join(WORKSHOP_PLUGIN_ID);
        let runtime_dir = base_dir
            .join("plugins")
            .join("runtime")
            .join(WORKSHOP_PLUGIN_ID);
        let _ = std::fs::create_dir_all(&cache_dir);
        let _ = std::fs::create_dir_all(&runtime_dir);

        let active_maps = std::fs::read_to_string(runtime_dir.join("active_maps.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();

        Self {
            cache_dir,
            runtime_dir,
            active_maps: Arc::new(Mutex::new(active_maps)),
        }
    }

    fn save_active_maps(&self) {
        let maps = self.active_maps.lock().unwrap();
        if let Ok(text) = serde_json::to_string(&*maps) {
            let _ = std::fs::write(self.runtime_dir.join("active_maps.json"), text);
        }
    }

    fn install_state_path(rl_path: &str) -> PathBuf {
        Path::new(rl_path)
            .join("TAGame")
            .join("CookedPCConsole")
            .join(WORKSHOP_MODS_DIR_NAME)
            .join("workshop_maps.json")
    }

    fn save_install_state(&self, rl_path: &str) {
        let path = Self::install_state_path(rl_path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(&*self.active_maps.lock().unwrap()) {
            let _ = std::fs::write(path, text);
        }
    }

    /// Adopt the active-map state saved beside the mods for the attached
    /// Rocket League installation (Steam and Epic are independent).
    pub fn reload_install_state(&self, rl_path: &str) {
        let mut maps: serde_json::Map<String, Value> =
            std::fs::read_to_string(Self::install_state_path(rl_path))
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
        let mods_dir = Path::new(rl_path)
            .join("TAGame")
            .join("CookedPCConsole")
            .join(WORKSHOP_MODS_DIR_NAME);
        maps.retain(|target, _| {
            target_filename(target).is_some_and(|filename| mods_dir.join(filename).is_file())
        });
        *self.active_maps.lock().unwrap() = maps;
        self.save_active_maps();
        self.save_install_state(rl_path);
    }

    pub fn active_maps(&self) -> serde_json::Map<String, Value> {
        self.active_maps.lock().unwrap().clone()
    }

    pub fn is_cached(&self, map_id: &str) -> bool {
        !map_id.is_empty() && self.cache_dir.join(format!("{map_id}.upk")).exists()
    }

    pub fn delete_from_cache(&self, map_id: &str) -> bool {
        if map_id.is_empty() {
            return false;
        }
        let target = self.cache_dir.join(format!("{map_id}.upk"));
        if target.exists() {
            std::fs::remove_file(&target).is_ok()
        } else {
            true
        }
    }

    pub fn get_active_targets_for_map(&self, map_id: &str) -> Vec<String> {
        self.active_maps
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, data)| id_of(data) == map_id)
            .map(|(name, _)| name.clone())
            .collect()
    }

    fn download_map_file(&self, map_id: &str, local_path: &Path) -> Result<(), String> {
        let url = format!("{DOWNLOAD_ENDPOINT_BASE}{map_id}");
        let zip_path = local_path.with_extension("zip");
        let temp_extract_dir = local_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(format!("temp_{map_id}"));

        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }

        let resp = get_download(&url)?;
        let copied = std::fs::File::create(&zip_path)
            .map_err(|e| e.to_string())
            .and_then(|mut out| {
                std::io::copy(&mut resp.into_reader(), &mut out).map_err(|e| e.to_string())
            });
        if let Err(e) = copied {
            let _ = std::fs::remove_file(&zip_path);
            return Err(e);
        }

        let file = std::fs::File::open(&zip_path).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        archive
            .extract(&temp_extract_dir)
            .map_err(|e| e.to_string())?;

        // Find the first .upk/.udk in the extracted tree.
        let extracted_map = find_map_file(&temp_extract_dir);
        let result = match extracted_map {
            Some(found) => std::fs::rename(&found, local_path)
                .or_else(|_| {
                    std::fs::copy(&found, local_path)
                        .map(|_| ())
                        .and_then(|_| std::fs::remove_file(&found))
                })
                .map_err(|e| e.to_string()),
            None => Err(
                t("download-map-file-no-valid-upk-or-udk-map").to_string(),
            ),
        };

        let _ = std::fs::remove_file(&zip_path);
        let _ = std::fs::remove_dir_all(&temp_extract_dir);
        result
    }

    pub fn install_map(
        &self,
        map_data: &Value,
        target_name: &str,
        rl_path: &str,
    ) -> Result<(), String> {
        let map_id = id_of(map_data);
        let cached_map_path = self.cache_dir.join(format!("{map_id}.upk"));

        if !self.is_cached(&map_id) {
            self.download_map_file(&map_id, &cached_map_path)?;
        }

        let cooked_pc_dir = Path::new(rl_path).join("TAGame").join("CookedPCConsole");
        let mods_dir = cooked_pc_dir.join(WORKSHOP_MODS_DIR_NAME);
        std::fs::create_dir_all(&mods_dir).map_err(|e| e.to_string())?;

        let filename =
            target_filename(target_name).ok_or_else(|| "Invalid target map.".to_string())?;
        if target_name == "Hourglass" {
            let _ = std::fs::remove_file(mods_dir.join("Labs_Hourglass_P.upk"));
        }
        std::fs::copy(&cached_map_path, mods_dir.join(filename)).map_err(|e| e.to_string())?;

        self.active_maps
            .lock()
            .unwrap()
            .insert(target_name.to_string(), map_data.clone());
        self.save_active_maps();
        self.save_install_state(rl_path);
        Ok(())
    }

    /// Rocket League only picks up files in the mods folder that were there
    /// when it started, so a map installed mid-game into a slot with no file
    /// yet gets ignored and you load the vanilla map. before launching, put a
    /// plain copy of the vanilla map in every empty slot; installing then just
    /// overwrites it. a copy, never a link, or installing would overwrite the
    /// vanilla file.
    pub fn seed_empty_slots(&self, rl_path: &str) {
        let cooked = Path::new(rl_path).join("TAGame").join("CookedPCConsole");
        let mods_dir = cooked.join(WORKSHOP_MODS_DIR_NAME);
        if std::fs::create_dir_all(&mods_dir).is_err() {
            return;
        }
        for (_, filename) in TARGET_MAPS {
            let (vanilla, modded) = (cooked.join(filename), mods_dir.join(filename));
            if !modded.exists() && vanilla.is_file() {
                let _ = std::fs::copy(&vanilla, &modded);
            }
        }
    }

    pub fn unload_active_map(&self, target_name: &str, rl_path: &str) -> Result<(), String> {
        let filename =
            target_filename(target_name).ok_or_else(|| "Invalid target map.".to_string())?;
        let target_file = Path::new(rl_path)
            .join("TAGame")
            .join("CookedPCConsole")
            .join(WORKSHOP_MODS_DIR_NAME)
            .join(filename);
        if target_file.exists() {
            std::fs::remove_file(&target_file).map_err(|e| format!("Failed to unload map: {e}"))?;
        }
        if target_name == "Hourglass" {
            let _ = std::fs::remove_file(
                Path::new(rl_path)
                    .join("TAGame")
                    .join("CookedPCConsole")
                    .join(WORKSHOP_MODS_DIR_NAME)
                    .join("Labs_Hourglass_P.upk"),
            );
        }
        self.active_maps.lock().unwrap().remove(target_name);
        self.save_active_maps();
        self.save_install_state(rl_path);
        Ok(())
    }
}

/// the player list grows a search box past this many players
const PLAYER_SEARCH_THRESHOLD: usize = 6;
/// names listed in a "players differ" tooltip before it says "and N more"
const MAX_TOOLTIP_NAMES: usize = 10;
const OK_COLOUR: egui::Color32 = egui::Color32::LIGHT_GREEN;
const WARN_COLOUR: egui::Color32 = egui::Color32::from_rgb(0xf3, 0x9c, 0x12);

/// a player's maps in one line: "Map", "Map + 2 more" or "no map replaced"
fn offer_map_summary(maps: &[SlotMap]) -> String {
    let mut names = maps
        .iter()
        .filter(|map| target_filename(&map.slot).is_some())
        .map(|map| map.name.as_str());
    match (names.next(), names.count()) {
        (None, _) => "no map replaced".to_string(),
        (Some(first), 0) => first.to_string(),
        (Some(first), more) => format!("{first} + {more} more"),
    }
}

fn matches_filter(label: &str, filter: &str) -> bool {
    let filter = filter.trim();
    filter.is_empty() || label.to_lowercase().contains(&filter.to_lowercase())
}

/// true if any of the player's maps isn't sitting in the same slot on your side
fn player_differs(offer: &PeerOffer, mine: &serde_json::Map<String, Value>) -> bool {
    offer
        .maps
        .iter()
        .filter(|map| target_filename(&map.slot).is_some())
        .any(|map| mine.get(&map.slot).is_none_or(|have| id_of(have) != map.id))
}

/// what's off about one of your slots for one player, None if they match
fn slot_problem(offer: &PeerOffer, slot: &str, id: &str) -> Option<String> {
    let who = offer.label();
    match offer.maps.iter().find(|m| m.slot == slot) {
        Some(theirs) if theirs.id == id => None,
        Some(theirs) => Some(format!("{who} has {} here", theirs.name)),
        None => Some(match offer.maps.iter().find(|m| m.id == id) {
            Some(elsewhere) => format!("{who} put it in {} instead", elsewhere.slot),
            None => format!("{who} hasn't replaced it"),
        }),
    }
}

/// one of your replaced slots, with a short "does everyone match" status
fn render_my_slot(ui: &mut egui::Ui, slot: &str, data: &Value, offers: &[PeerOffer]) {
    let id = id_of(data);
    let name = str_of(data, "name", slot);
    let problems: Vec<String> = offers
        .iter()
        .filter_map(|offer| slot_problem(offer, slot, &id))
        .collect();
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("  {slot}: {name}"));
        if offers.is_empty() {
            return;
        }
        if problems.is_empty() {
            ui.colored_label(OK_COLOUR, t("my-slot-everyone-has-it"));
            return;
        }
        let mut tip = problems
            .iter()
            .take(MAX_TOOLTIP_NAMES)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        if problems.len() > MAX_TOOLTIP_NAMES {
            tip.push_str(&format!("\nand {} more", problems.len() - MAX_TOOLTIP_NAMES));
        }
        ui.colored_label(
            WARN_COLOUR,
            t_args("my-slot-problems-of-offers-players-differ", &[("problems", (problems.len()).to_string().into()), ("offers", (offers.len()).to_string().into())]),
        )
        .on_hover_text(tip);
    });
}

/// "Name — Map", the map part coloured by whether you match them
fn player_header(
    ui: &egui::Ui,
    offer: &PeerOffer,
    mine: &serde_json::Map<String, Value>,
) -> egui::text::LayoutJob {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let visuals = ui.visuals();
    let map_colour = if offer_map_summary(&offer.maps) == "no map replaced" {
        visuals.weak_text_color()
    } else if player_differs(offer, mine) {
        WARN_COLOUR
    } else {
        OK_COLOUR
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(
        &offer.label(),
        0.0,
        egui::TextFormat::simple(font.clone(), visuals.strong_text_color()),
    );
    job.append(
        " — ",
        0.0,
        egui::TextFormat::simple(font.clone(), visuals.weak_text_color()),
    );
    job.append(
        &offer_map_summary(&offer.maps),
        0.0,
        egui::TextFormat::simple(font, map_colour),
    );
    job
}

/// a player's maps once their row is expanded, with install options
fn render_player_maps(
    ui: &mut egui::Ui,
    offer: &PeerOffer,
    mine: &serde_json::Map<String, Value>,
    busy: bool,
    install: &mut Option<(SlotMap, IpAddr, bool)>,
) {
    let maps: Vec<&SlotMap> = offer
        .maps
        .iter()
        .filter(|map| target_filename(&map.slot).is_some())
        .collect();
    if maps.is_empty() {
        ui.small(t("player-maps-hasn-t-replaced-any-map"));
    }
    for map in maps {
        let have = mine.get(&map.slot);
        let same = have.is_some_and(|m| id_of(m) == map.id);
        let elsewhere = mine
            .iter()
            .find(|(slot, m)| **slot != map.slot && id_of(m) == map.id)
            .map(|(slot, _)| slot.clone());
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("{}: {}", map.slot, map.name));
            if map.local {
                ui.small(t("player-maps-imported-not-on-the-workshop"));
            }
            if same {
                ui.colored_label(OK_COLOUR, t("player-maps-you-have-it"));
                return;
            }
            let note = match (&elsewhere, have) {
                (Some(slot), _) => {
                    format!("you put this map in {slot}, replace {} instead", map.slot)
                }
                (None, Some(m)) => format!(
                    "you have {} in {}",
                    str_of(m, "name", "another map"),
                    map.slot
                ),
                (None, None) => format!("you haven't replaced {}", map.slot),
            };
            ui.colored_label(WARN_COLOUR, note);
            let label = format!("Install to {}", map.slot);
            if map.local {
                // imported maps aren't on the cdn, only the peer has them
                if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
                    *install = Some((map.clone(), offer.ip, true));
                }
            } else {
                // cdn maps can come from either, so the peer is the way
                // around a slow or unreachable cdn
                ui.add_enabled_ui(!busy, |ui| {
                    ui.menu_button(format!("{label} v"), |ui| {
                        if ui.button(t("player-maps-from-the-cdn")).clicked() {
                            *install = Some((map.clone(), offer.ip, false));
                            ui.close();
                        }
                        if ui.button(t("player-maps-from-this-player")).clicked() {
                            *install = Some((map.clone(), offer.ip, true));
                            ui.close();
                        }
                    });
                });
            }
        });
    }
}

fn multiplayer_help_button(ui: &mut egui::Ui) {
    if ui
        .button(t("multiplayer-help-button-using-a-vpn-proxy-or-having"))
        .on_hover_text(t("multiplayer-help-button-opens-the-workshop-multiplayer-help-page"))
        .clicked()
    {
        ui.ctx()
            .open_url(egui::OpenUrl::new_tab(MULTIPLAYER_HELP_URL));
    }
}

fn find_map_file(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if ext.eq_ignore_ascii_case("upk") || ext.eq_ignore_ascii_case("udk") {
                return Some(path);
            }
        }
    }
    dirs.iter().find_map(|d| find_map_file(d))
}

pub fn id_of(map_data: &Value) -> String {
    match map_data.get("id") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => "0".to_string(),
    }
}

fn str_of<'a>(map_data: &'a Value, key: &str, default: &'a str) -> &'a str {
    map_data
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or(default)
}

/// remote url + the path to cache it under
pub fn banner_url_and_cache_rel(banner_path: &str) -> (String, String) {
    let normalized = banner_path.replace('\\', "/");
    let url = format!("{REMOTE_FILES_BASE}{normalized}");
    (url, normalized.trim_start_matches('/').to_string())
}

/// fetch banner_path cached under cache_dir
pub fn spawn_image_fetch(
    key: String,
    cache_dir: PathBuf,
    tx: Sender<AppMsg>,
    ctx: eframe::egui::Context,
    done: impl FnOnce(String, Vec<u8>) -> AppMsg + Send + 'static,
) {
    std::thread::spawn(move || {
        let (url, rel) = banner_url_and_cache_rel(&key);
        let local_path = cache_dir.join(rel);
        let bytes: Option<Vec<u8>> = if local_path.exists() {
            std::fs::read(&local_path).ok()
        } else {
            let result = get_retry(&url, Duration::from_secs(10))
                .ok()
                .and_then(|resp| {
                    let mut buf = Vec::new();
                    resp.into_reader().read_to_end(&mut buf).ok()?;
                    Some(buf)
                });
            if let Some(buf) = &result {
                if let Some(parent) = local_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&local_path, buf);
            }
            result
        };
        let _ = tx.send(done(key, bytes.unwrap_or_default()));
        ctx.request_repaint();
    });
}

// Tab state

pub enum ImageState {
    Loading,
    Ready(Arc<[u8]>),
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkshopView {
    Browse,
    BackgroundChanger,
    Multiplayer,
    Import,
}

struct MultiplayerState {
    wizard_started: bool,
    host_name: String,
    identity_update_in_flight: bool,
    identity_updated: bool,
    /// one relay session regardless of whether this peer ends up hosting or
    /// joining inside Rocket League's own UI - see hosting.rs
    relay: Option<HostSession>,
    status: String,
    setup_progress: Option<String>,
    saved_host: Option<SavedHost>,
    saved_room: Option<crate::multiplayer_lan::Room>,
    saved_host_checked: bool,
    saved_host_checking: bool,
    detected_target: Option<String>,
    detected_map: Option<String>,
    /// set while a peer's map is being downloaded + installed in the background
    map_install_busy: Arc<AtomicBool>,
    map_install_result: Arc<Mutex<String>>,
    /// progress of a map being downloaded from a peer
    map_transfer: Arc<TransferProgress>,
    /// maps received from a peer, waiting to be added to the catalog
    received_maps: Arc<Mutex<Vec<LocalMap>>>,
    /// a peer install waiting on the "files from other players" warning
    pending_peer_install: Option<(SlotMap, IpAddr, bool)>,
    /// the warning was accepted once this session, don't ask again
    peer_warning_accepted: bool,
    /// Back was pressed while the tailnet was still starting; the finished
    /// connection gets dropped instead of kept
    abandon_tailnet: bool,
    /// search text for the player list
    player_filter: String,
    /// route everything through tailscale's relays so other players never
    /// see this machine's public address (saved in multiplayer_settings.json)
    relay_only: bool,

    // tsnet sidecar / tailnet state
    sidecar: Option<Arc<TsnetSidecarHandle>>,
    tailnet_requested: bool,
    tailnet_ip: Option<String>,

    // replaces the old three-phase TAP wizard gate (tap_ready no longer
    // exists as a separate step: the tailnet comes up before Rocket League
    // is ever launched, so there's only "is the tailnet up" and "did RL
    // launch with the right address")
    rl_open: bool,
    launch_ready: bool,
    multihome_check_attempts: u8,
    multihome_check_in_flight: bool,
    /// true while Hebnix itself is closing/relaunching Rocket League, so the
    /// RL-monitor's "closed" transition doesn't arm the crash grace window
    /// for a restart Hebnix initiated on purpose
    launching_rocket_league: bool,

    /// set when Rocket League closes with a session still active; if it
    /// doesn't come back before this deadline, the session is torn down for
    /// real (see CRASH_GRACE_WINDOW)
    shutdown_deadline: Option<std::time::Instant>,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
struct SavedHost {
    pin: String,
    host_secret: String,
}

impl Default for MultiplayerState {
    fn default() -> Self {
        Self {
            wizard_started: false,
            host_name: "Hebnix Workshop".to_string(),
            identity_update_in_flight: false,
            identity_updated: false,
            relay: None,
            status: t("default-connect-then-host-or-join-inside")
                .to_string(),
            setup_progress: None,
            saved_host: None,
            saved_room: None,
            saved_host_checked: false,
            saved_host_checking: false,
            detected_target: None,
            detected_map: None,
            map_install_busy: Arc::new(AtomicBool::new(false)),
            map_install_result: Arc::new(Mutex::new(String::new())),
            map_transfer: Arc::new(TransferProgress::default()),
            received_maps: Arc::new(Mutex::new(Vec::new())),
            pending_peer_install: None,
            peer_warning_accepted: false,
            player_filter: String::new(),
            abandon_tailnet: false,
            relay_only: false,
            sidecar: None,
            tailnet_requested: false,
            tailnet_ip: None,
            rl_open: false,
            launch_ready: false,
            multihome_check_attempts: 0,
            multihome_check_in_flight: false,
            launching_rocket_league: false,
            shutdown_deadline: None,
        }
    }
}

pub struct WorkshopState {
    pub manager: MapManager,
    pub catalog: Vec<Value>,
    pub valid: Vec<usize>,
    pub page: usize,
    pub page_size: usize,
    pub search: String,
    pub view_downloaded: bool,
    pub target: String,
    pub images: HashMap<String, ImageState>,
    pub busy: HashSet<String>,
    pub catalog_status: String,
    pub fetched: bool,
    pub confirm_delete: Option<Value>,
    view: WorkshopView,
    background_changer: BackgroundChangerState,
    import_wizard: ImportWizard,
    steam_downloader: SteamDownloader,
    archive_browser: ArchiveBrowser,
    multiplayer: MultiplayerState,
    rl_launch: crate::config::RlLaunchCfg,
    /// the "P2P File Sharing" system setting. read by the map sync server
    /// each time a player asks for a file, so it applies straight away.
    share_files: Arc<AtomicBool>,
}

impl WorkshopState {
    pub fn new(base_dir: &Path) -> Self {
        let manager = MapManager::new(base_dir);
        let saved_host = std::fs::read(manager.runtime_dir.join("multiplayer_host.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let mut multiplayer = MultiplayerState::default();
        multiplayer.saved_host = saved_host;
        multiplayer.relay_only = std::fs::read(manager.runtime_dir.join(MULTIPLAYER_SETTINGS_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|settings| settings.get("relay_only").and_then(Value::as_bool))
            .unwrap_or(false);
        Self {
            manager,
            catalog: Vec::new(),
            valid: Vec::new(),
            page: 0,
            page_size: 12,
            search: String::new(),
            view_downloaded: false,
            target: TARGET_MAPS[0].0.to_string(),
            images: HashMap::new(),
            busy: HashSet::new(),
            catalog_status: t("catalog-loading-catalog").to_string(),
            fetched: false,
            confirm_delete: None,
            view: WorkshopView::Browse,
            background_changer: BackgroundChangerState::default(),
            import_wizard: ImportWizard::default(),
            steam_downloader: SteamDownloader::default(),
            archive_browser: ArchiveBrowser::default(),
            multiplayer,
            rl_launch: crate::config::RlLaunchCfg::default(),
            share_files: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn finish_background_changer(&mut self, result: Result<String, String>) -> String {
        self.background_changer.finish(result)
    }

    /// installs a freshly fetched cdn catalog, keeping the maps the player
    /// imported themselves
    pub fn set_catalog(&mut self, items: Vec<Value>) {
        self.catalog = items;
        self.merge_local_maps();
        self.execute_search(true);
    }

    /// adds imported maps missing from the catalog (also what keeps them
    /// visible when the cdn can't be reached)
    pub fn merge_local_maps(&mut self) {
        let known: HashSet<String> = self.catalog.iter().map(id_of).collect();
        let missing: Vec<Value> = load_local_maps(&self.manager.runtime_dir)
            .iter()
            .filter(|map| !known.contains(&map.id))
            .map(LocalMap::to_catalog_entry)
            .collect();
        if !missing.is_empty() {
            self.catalog.extend(missing);
            self.execute_search(false);
        }
    }

    fn add_local_map(&mut self, map: LocalMap) {
        self.catalog.retain(|entry| id_of(entry) != map.id);
        self.catalog.push(map.to_catalog_entry());
        self.execute_search(false);
    }

    pub fn total_pages(&self) -> usize {
        self.valid.len().div_ceil(self.page_size).max(1)
    }

    /// kick off the async catalog fetch (once at startup)
    pub fn fetch_catalog(&mut self, tx: Sender<AppMsg>, ctx: eframe::egui::Context) {
        if self.fetched {
            return;
        }
        self.fetched = true;
        std::thread::spawn(move || {
            let result = (|| -> Result<Vec<Value>, String> {
                let resp = get_retry(API_ENDPOINT, Duration::from_secs(10))?;
                let data: Value = resp.into_json().map_err(|e| e.to_string())?;
                if let Value::Array(items) = data {
                    return Ok(items);
                }
                Ok(data
                    .get("items")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default())
            })();
            let _ = tx.send(AppMsg::WorkshopCatalog(result));
            ctx.request_repaint();
        });
    }

    pub fn execute_search(&mut self, reset_page: bool) {
        let query = self.search.to_lowercase().trim().to_string();
        self.valid.clear();
        for (i, m) in self.catalog.iter().enumerate() {
            let name = str_of(m, "name", "").to_lowercase();
            let author = str_of(m, "author", "").to_lowercase();
            let matches_query = name.contains(&query) || author.contains(&query);
            let matches_dl = !self.view_downloaded || self.manager.is_cached(&id_of(m));
            if matches_query && matches_dl {
                self.valid.push(i);
            }
        }
        if reset_page {
            self.page = 0;
        } else {
            self.page = self.page.min(self.total_pages() - 1);
        }
    }

    fn ensure_image(
        &mut self,
        banner_path: &str,
        tx: &Sender<AppMsg>,
        ctx: &eframe::egui::Context,
    ) {
        if banner_path.is_empty() || self.images.contains_key(banner_path) {
            return;
        }
        self.images
            .insert(banner_path.to_string(), ImageState::Loading);

        spawn_image_fetch(
            banner_path.to_string(),
            self.manager.cache_dir.clone(),
            tx.clone(),
            ctx.clone(),
            |key, bytes| AppMsg::WorkshopImage { key, bytes },
        );
    }

    /// render the tab. rl_path and rl_launch come from the app config.
    pub fn render(
        &mut self,
        ui: &mut egui::Ui,
        rl_path: &str,
        rl_launch: &crate::config::RlLaunchCfg,
        tx: &Sender<AppMsg>,
    ) {
        self.rl_launch = rl_launch.clone();
        let ctx = ui.ctx().clone();

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.view, WorkshopView::Browse, t("render-browse-maps"));
            ui.selectable_value(
                &mut self.view,
                WorkshopView::BackgroundChanger,
                t("render-background-changer"),
            );
            ui.selectable_value(&mut self.view, WorkshopView::Import, t("render-import-map"));
            ui.selectable_value(&mut self.view, WorkshopView::Multiplayer, t("render-multiplayer"));
        });
        ui.separator();
        if self.view == WorkshopView::Import {
            let imported =
                self.import_wizard
                    .render(ui, &self.manager.cache_dir, &self.manager.runtime_dir);
            if let Some(map) = imported {
                self.add_local_map(map);
            }
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            let downloaded = self.steam_downloader.render(
                ui,
                &self.manager.cache_dir,
                &self.manager.runtime_dir,
            );
            if let Some(map) = downloaded {
                self.add_local_map(map);
            }
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            let from_archive =
                self.archive_browser
                    .render(ui, &self.manager.cache_dir, &self.manager.runtime_dir);
            if let Some(map) = from_archive {
                self.add_local_map(map);
            }
            return;
        }
        if self.view == WorkshopView::Multiplayer {
            self.render_multiplayer(ui, rl_path, tx, &ctx);
            return;
        }
        if self.view == WorkshopView::BackgroundChanger {
            self.background_changer.render(ui, rl_path, tx);
            return;
        }

        // Toolbar
        ui.horizontal(|ui| {
            ui.strong(t("spoofer-search"));
            let search_resp = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text(t("ball-name-or-author"))
                    .desired_width(200.0),
            );
            let submitted =
                search_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button(t("ball-search")).clicked() || submitted {
                self.execute_search(true);
            }
            if ui
                .checkbox(&mut self.view_downloaded, t("render-view-downloaded"))
                .changed()
            {
                self.execute_search(true);
            }
            ui.menu_button("?", |ui| {
                ui.set_max_width(280.0);
                ui.label(
                    t("render-can-t-find-the-map-you"),
                );
                ui.label(
                    t("render-no-hubcap-api-key-the-import"),
                );
            })
            .response
            .on_hover_text(t("render-can-t-find-a-map"));

            ui.strong(t("render-map-to-replace"));
            let mut target_changed = false;
            egui::ComboBox::from_id_salt("target_map")
                .selected_text(self.target.clone())
                .show_ui(ui, |ui| {
                    for (name, _) in TARGET_MAPS {
                        if ui
                            .selectable_value(&mut self.target, name.to_string(), name)
                            .changed()
                        {
                            target_changed = true;
                        }
                    }
                });
            if target_changed {
                self.execute_search(false);
            }

            let restore_enabled = self.manager.active_maps().contains_key(&self.target);
            if ui
                .add_enabled(
                    restore_enabled,
                    egui::Button::new(t("ball-restore-original"))
                        .fill(egui::Color32::from_rgb(0xc0, 0x39, 0x2b)),
                )
                .clicked()
            {
                match self.manager.unload_active_map(&self.target, rl_path) {
                    Ok(()) => {
                        let _ = tx.send(AppMsg::Log(
                            "[Workshop] Original map restored successfully.".to_string(),
                        ));
                        self.execute_search(false);
                    }
                    Err(e) => {
                        let _ = tx.send(AppMsg::Log(format!("[Workshop] {e}")));
                    }
                }
            }
        });

        ui.add_space(4.0);

        // Pager row
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.page > 0, egui::Button::new(t("render-prev")))
                .clicked()
            {
                self.page -= 1;
            }
            let label = if self.valid.is_empty() {
                self.catalog_status.clone()
            } else {
                format!("Page {} of {}", self.page + 1, self.total_pages())
            };
            ui.add_sized([ui.available_width() - 90.0, 20.0], egui::Label::new(label));
            if ui
                .add_enabled(
                    self.page + 1 < self.total_pages(),
                    egui::Button::new(t("render-next")),
                )
                .clicked()
            {
                self.page += 1;
            }
        });

        ui.add_space(4.0);

        // Card grid
        let start = self.page * self.page_size;
        let indices: Vec<usize> = self
            .valid
            .iter()
            .skip(start)
            .take(self.page_size)
            .copied()
            .collect();

        // Pre-fetch images for the visible page.
        for &i in &indices {
            let banner = str_of(&self.catalog[i], "banner_path", "").to_string();
            self.ensure_image(&banner, tx, &ctx);
        }

        let mut action: Option<(usize, CardAction)> = None;

        egui::ScrollArea::vertical()
            .id_salt("workshop_grid")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for row in indices.chunks(4) {
                    ui.columns(4, |cols| {
                        for (col_idx, &map_idx) in row.iter().enumerate() {
                            let col = &mut cols[col_idx];
                            if let Some(act) = self.render_card(col, map_idx) {
                                action = Some((map_idx, act));
                            }
                        }
                    });
                    ui.add_space(6.0);
                }
                if indices.is_empty() {
                    ui.add_space(30.0);
                    ui.vertical_centered(|ui| {
                        ui.label(if self.catalog.is_empty() {
                            self.catalog_status.clone()
                        } else {
                            t("handle-messages-no-maps-found").to_string()
                        });
                    });
                }
            });

        if let Some((map_idx, act)) = action {
            self.handle_action(map_idx, act, rl_path, tx, &ctx);
        }

        // Delete-from-cache confirmation modal.
        if let Some(map_data) = self.confirm_delete.clone() {
            let name = str_of(&map_data, "name", "this map").to_string();
            let mut close = false;
            egui::Window::new(t("render-offboard-map")).id(egui::Id::new("render-offboard-map"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    if is_local_entry(&map_data) {
                        ui.label(t_args("render-remove-the-imported-map-name-its", &[("name", name.to_string().into())]));
                    } else {
                        ui.label(t_args("render-are-you-sure-you-want-to-2", &[("name", name.to_string().into())]));
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(t("plugin-delete-prompt-yes")).clicked() {
                            let id = id_of(&map_data);
                            let ok = if is_local_entry(&map_data) {
                                let removed = remove_local_map(
                                    &self.manager.cache_dir,
                                    &self.manager.runtime_dir,
                                    &id,
                                )
                                .is_ok();
                                if removed {
                                    self.catalog.retain(|entry| id_of(entry) != id);
                                }
                                removed
                            } else {
                                self.manager.delete_from_cache(&id)
                            };
                            if !ok {
                                let _ = tx.send(AppMsg::Log(
                                    "[Workshop] Failed to delete file. Ensure the game is closed or the file isn't in use.".to_string(),
                                ));
                            }
                            self.execute_search(false);
                            close = true;
                        }
                        if ui.button(t("plugin-delete-prompt-no")).clicked() {
                            close = true;
                        }
                    });
                });
            if close {
                self.confirm_delete = None;
            }
        }
    }

    /// One flow, not two: Rocket League's own UI is where someone picks
    /// Host or Join, not Hebnix's. Every peer runs the identical relay
    /// (confirmed by packet capture that joining broadcasts too, not just
    /// hosting - see hosting.rs), so there's nothing left for Hebnix to
    /// ask about up front. Connect, start Rocket League, the relay starts
    /// itself the moment the game's actually up - no separate button.
    fn render_multiplayer(
        &mut self,
        ui: &mut egui::Ui,
        rl_path: &str,
        tx: &Sender<AppMsg>,
        ctx: &eframe::egui::Context,
    ) {
        if !self.multiplayer.wizard_started {
            ui.add_space(56.0);
            ui.vertical_centered(|ui| {
                ui.heading(t("multiplayer-workshop-multiplayer"));
                ui.label(
                    t("multiplayer-connects-you-to-the-private-workshop"),
                );
                ui.add_space(12.0);
                if ui
                    .checkbox(
                        &mut self.multiplayer.relay_only,
                        t("multiplayer-hide-my-ip-from-other-players"),
                    )
                    .on_hover_text(
                        t("multiplayer-sends-all-multiplayer-traffic-through-ta"),
                    )
                    .changed()
                {
                    self.save_multiplayer_settings();
                }
                ui.add_space(12.0);
                if ui
                    .add_sized([160.0, 38.0], egui::Button::new(t("multiplayer-connect")))
                    .clicked()
                {
                    self.multiplayer.wizard_started = true;
                    self.start_tailnet(tx, ctx);
                }
                ui.add_space(10.0);
                multiplayer_help_button(ui);
            });
            return;
        }
        let mut start_relay = false;
        let mut stop = false;
        let mut launch = false;
        let mut close_game = false;
        let mut install_peer_map: Option<(SlotMap, IpAddr, bool)> = None;
        let mut block_peer: Option<(IpAddr, String)> = None;
        let mut unblock_peer: Option<IpAddr> = None;
        let is_admin = crate::spoofer::is_admin();
        let setup_in_progress = self.multiplayer.setup_progress.is_some();
        let tailnet_ready = self.multiplayer.tailnet_ip.is_some();

        if !is_admin {
            ui.colored_label(
                egui::Color32::YELLOW,
                t("multiplayer-workshop-multiplayer-requires-hebnix-to"),
            );
        }
        ui.horizontal(|ui| {
            if self.multiplayer.relay.is_none() && ui.button(t("multiplayer-back")).clicked() {
                self.leave_multiplayer_setup();
                return;
            }
            ui.strong(t("multiplayer-workshop-multiplayer"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                multiplayer_help_button(ui);
            });
        });
        ui.group(|ui| {
            ui.strong(t("multiplayer-setup"));
            if !tailnet_ready {
                ui.label(t("multiplayer-connecting-to-the-private-workshop-netwo"));
            } else if !self.multiplayer.rl_open {
                ui.label(t("multiplayer-step-1-start-rocket-league-on"));
                if ui
                    .add_enabled(
                        !setup_in_progress && is_admin,
                        egui::Button::new(t("action-start-rocket-league")),
                    )
                    .clicked()
                {
                    launch = true;
                }
            } else if !self.multiplayer.launch_ready {
                if self.waiting_for_multihome_check() {
                    ui.label(t("multiplayer-step-1-waiting-for-rocket-league"));
                    ui.small(format!("{} ", t("multiplayer-checking-launch-command")));
                } else {
                    ui.label(
                        t("multiplayer-rocket-league-was-not-started-with"),
                    );
                    ui.small(
                        t("multiplayer-rocket-league-must-restart-because-multi"),
                    );
                    if ui.button(t("action-close-rocket-league")).clicked() {
                        close_game = true;
                    }
                }
            } else if self.multiplayer.relay.is_none() {
                ui.label(t("multiplayer-starting-the-workshop-relay"));
                if !setup_in_progress {
                    start_relay = true;
                }
            } else {
                ui.label(
                    t("multiplayer-ready-host-or-join-from-rocket"),
                );
                if let Some(name) = &self.multiplayer.detected_map {
                    ui.small(t_args("multiplayer-detected-lan-match-on-map-name", &[("name", name.to_string().into())]));
                }
            }
        });
        let received: Vec<LocalMap> = self
            .multiplayer
            .received_maps
            .lock()
            .map(|mut inbox| inbox.drain(..).collect())
            .unwrap_or_default();
        for map in received {
            self.add_local_map(map);
        }
        if self.multiplayer.relay.is_some() {
            self.render_maps_in_use(ui, &mut install_peer_map, &mut block_peer);
        }
        if let Some(session) = &self.multiplayer.relay {
            let blocked = session.blocked();
            if !blocked.is_empty() {
                ui.group(|ui| {
                    egui::CollapsingHeader::new(t_args("multiplayer-blocked-players-blocked", &[("blocked", (blocked.len()).to_string().into())]))
                        .id_salt("workshop_blocked_players")
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.small(t("multiplayer-blocks-last-until-you-disconnect"));
                            for (ip, label) in &blocked {
                                ui.horizontal(|ui| {
                                    ui.label(label);
                                    if ui.small_button(t("multiplayer-unblock")).clicked() {
                                        unblock_peer = Some(*ip);
                                    }
                                });
                            }
                        });
                });
            }
        }
        if let Some(session) = &self.multiplayer.relay {
            ui.group(|ui| {
                ui.strong(t("multiplayer-relaying-to-the-workshop-network"));
                ui.small(t_args("multiplayer-tunnel-if-sent-session-received-session2", &[("state", (if session.stats.connected.load(Ordering::Relaxed) {
                        "peer connected"
                    } else {
                        "waiting for peer"
                    }).to_string().into()), ("sent", (session.stats.sent.load(Ordering::Relaxed)).to_string().into()), ("received", (session.stats.received.load(Ordering::Relaxed)).to_string().into())]));
                if let Ok(flow) = session.stats.last_beacon_relayed.lock() {
                    if !flow.is_empty() {
                        ui.small(t_args("multiplayer-latest-flow", &[("flow", flow.to_string().into())]));
                    }
                }
                if ui.button(t("multiplayer-disconnect")).clicked() {
                    stop = true;
                }
            });
        }
        ui.add_space(10.0);
        ui.label(&self.multiplayer.status);
        if let Some(progress) = &self.multiplayer.setup_progress {
            ui.add(egui::ProgressBar::new(0.5).animate(true).text(progress));
        }

        if launch {
            self.launch_multiplayer(rl_path, tx, ctx);
        }

        if let Some(session) = &self.multiplayer.relay {
            if let Some((ip, label)) = block_peer {
                session.block(ip, label);
            }
            if let Some(ip) = unblock_peer {
                session.unblock(ip);
            }
        }

        if let Some(request) = install_peer_map {
            if request.2 && !self.multiplayer.peer_warning_accepted {
                self.multiplayer.pending_peer_install = Some(request);
            } else {
                let (map, peer, from_peer) = request;
                self.install_peer_map(map, peer, from_peer, rl_path, ctx);
            }
        }
        self.render_peer_install_warning(rl_path, ctx);

        if close_game {
            self.multiplayer.status =
                t("multiplayer-closing-rocket-league-start-it-again").to_string();
            std::thread::spawn(|| {
                let _ = crate::winutil::kill_rocket_league();
            });
        }

        if stop {
            if let Some(mut session) = self.multiplayer.relay.take() {
                self.multiplayer.status = match session.stop() {
                    Ok(()) => t("multiplayer-disconnected").to_string(),
                    Err(error) => format!("Disconnected, but cleanup failed: {error}"),
                };
            }
            let _ = crate::winutil::clear_rocket_league_multihome();
            // this used to only stop the relay/beacon capture and leave the
            // tailnet connection itself running until the whole app closed
            // (self.multiplayer.sidecar was only ever cleared from
            // suspend_multiplayer, never here) -- so the tailscale virtual
            // adapter stayed up for the rest of the Hebnix session after
            // every disconnect, sitting alongside the real network adapter.
            // Confirmed live this causes real, ongoing internet flakiness
            // for other things (Rocket League's own EOS login kept dropping
            // and retrying) even with no -multihome argument left pointing
            // at it, purely from the extra adapter still being there.
            // Dropping the sidecar here runs its Drop impl, which brings
            // the tailnet down and stops the HebnixTailscale service.
            self.multiplayer.sidecar = None;
            let _ = crate::multiplayer_lan::cleanup_system_state();
        }
        if start_relay {
            if !is_admin {
                self.multiplayer.status =
                    t("multiplayer-run-hebnix-as-administrator-to-start").to_string();
            } else {
                self.start_relay(rl_path, tx, ctx);
            }
        }
    }

    /// Back before the relay is running: take down everything the Connect
    /// button set up (the tailnet, the Hebnix tailscale service, the Rocket
    /// League multihome setting, firewall rules) instead of leaving it up
    /// until Hebnix closes.
    fn leave_multiplayer_setup(&mut self) {
        self.multiplayer.wizard_started = false;
        self.multiplayer.pending_peer_install = None;
        self.multiplayer.setup_progress = None;
        self.multiplayer.tailnet_ip = None;
        self.multiplayer.launch_ready = false;
        self.multiplayer.launching_rocket_league = false;
        self.multiplayer.multihome_check_attempts = 0;
        self.multiplayer.multihome_check_in_flight = false;
        self.multiplayer.status =
            t("default-connect-then-host-or-join-inside").to_string();
        let sidecar = self.multiplayer.sidecar.take();
        if sidecar.is_none() && self.multiplayer.tailnet_requested {
            // still starting up in the background, so there's nothing to drop
            // yet: throw it away the moment it reports in (Connect again
            // before then just keeps it, see start_tailnet)
            self.multiplayer.abandon_tailnet = true;
        } else {
            self.multiplayer.tailnet_requested = false;
        }
        // dropping the handle brings the tailnet down and stops the service,
        // and the cleanup runs external commands, so none of it on the ui thread
        std::thread::Builder::new()
            .name("workshop-leave".into())
            .spawn(move || {
                drop(sidecar);
                let _ = crate::winutil::clear_rocket_league_multihome();
                let _ = crate::multiplayer_lan::cleanup_system_state();
            })
            .ok();
    }

    /// Spawns (or reuses) the tsnet sidecar and brings the tailnet up. This
    /// happens as soon as the user picks Host/Join, before Rocket League is
    /// touched at all, so the multihome address is known up front instead
    /// of being discovered after a launch-and-detect cycle.
    fn start_tailnet(&mut self, tx: &Sender<AppMsg>, ctx: &eframe::egui::Context) {
        if self.multiplayer.tailnet_requested {
            // came back before an abandoned startup finished: keep it
            self.multiplayer.abandon_tailnet = false;
            return;
        }
        self.multiplayer.tailnet_requested = true;
        self.multiplayer.status = t("start-tailnet-setting-up-the-private-workshop-network").to_string();
        let relay_only = self.multiplayer.relay_only;
        let tx = tx.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<Arc<TsnetSidecarHandle>, String> {
                let base_dir = crate::config::base_dir();
                crate::multiplayer_assets::ensure_present(&base_dir).map_err(|error| {
                    format!("could not extract multiplayer components: {error}")
                })?;
                let executable = std::env::current_exe().map_err(|error| error.to_string())?;
                let exe_dir = executable.parent().ok_or_else(|| {
                    "could not locate Hebnix's install folder".to_string()
                })?;
                // the helper programs can sit in a multiplayer-lan folder in
                // the data folder (installed) or next to the exe (portable,
                // built from source), or loose next to the exe
                let lan_dir = crate::config::base_dir().join("multiplayer-lan");
                let mut search = vec![lan_dir.clone(), exe_dir.join("multiplayer-lan")];
                search.push(exe_dir.to_path_buf());
                search.dedup();
                let sidecar_dir = find_sidecar_dir(&search)?;
                ensure_sidecar_rule(&sidecar_dir.join("tailscaled.exe"))?;
                let state_dir = lan_dir.join("tsnet-state");
                let handle =
                    TsnetSidecarHandle::spawn(&sidecar_dir, &state_dir, relay_only, tx.clone())?;
                // "host"/"guest" no longer means anything to Hebnix's own
                // relay (see hosting.rs) - every peer is the same
                let key = RoomClient::new(TSNET_CONTROL_URL).request_tsnet_authkey("peer", "")?;
                let token = multiplayer_client_token();
                let hostname = format!("hebnix-{}", &token[..token.len().min(8)]);
                handle.request_up(key.auth_key, hostname, key.control_url)?;
                Ok(Arc::new(handle))
            })();
            let _ = tx.send(AppMsg::WorkshopTailnetStarted { result });
            repaint.request_repaint();
        });
    }

    /// turns serving map files to other players on or off (system setting)
    pub fn set_share_files(&self, enabled: bool) {
        self.share_files.store(enabled, Ordering::Relaxed);
    }

    pub fn finish_tailnet_started(&mut self, result: Result<Arc<TsnetSidecarHandle>, String>) {
        match result {
            Ok(sidecar) if self.multiplayer.abandon_tailnet => {
                // the player left the screen while this was starting
                self.multiplayer.abandon_tailnet = false;
                self.multiplayer.tailnet_requested = false;
                std::thread::Builder::new()
                    .name("workshop-leave".into())
                    .spawn(move || {
                        drop(sidecar);
                        let _ = crate::winutil::clear_rocket_league_multihome();
                        let _ = crate::multiplayer_lan::cleanup_system_state();
                    })
                    .ok();
            }
            Ok(sidecar) => {
                self.multiplayer.sidecar = Some(sidecar);
                self.multiplayer.status =
                    t("finish-tailnet-started-connected-to-the-private-workshop-networ").to_string();
            }
            Err(_) if self.multiplayer.abandon_tailnet => {
                self.multiplayer.abandon_tailnet = false;
                self.multiplayer.tailnet_requested = false;
            }
            Err(error) => {
                self.multiplayer.tailnet_requested = false;
                self.multiplayer.status =
                    format!("Could not set up the Workshop network: {}", redact(&error));
            }
        }
    }

    /// called from AppMsg::TsnetUpResult once the sidecar actually finishes
    /// authenticating and reports a tailnet address
    pub fn set_tailnet_ip(&mut self, tailnet_ip: String) {
        // a late answer from a connection the player already backed out of
        if !self.multiplayer.wizard_started {
            return;
        }
        self.multiplayer.tailnet_ip = Some(tailnet_ip);
        self.multiplayer.status =
            t("set-tailnet-ip-ready-on-the-private-workshop-network").to_string();
    }

    pub fn tailnet_failed(&mut self, error: String) {
        if !self.multiplayer.wizard_started {
            return;
        }
        self.multiplayer.tailnet_requested = false;
        self.multiplayer.status =
            format!("The Workshop network connection failed: {}", redact(&error));
    }

    fn launch_multiplayer(
        &mut self,
        rl_path: &str,
        tx: &Sender<AppMsg>,
        ctx: &eframe::egui::Context,
    ) {
        let Some(tailnet_ip) = self.multiplayer.tailnet_ip.clone() else {
            self.multiplayer.status = t("launch-multiplayer-the-workshop-network-is-not-ready").to_string();
            return;
        };
        self.multiplayer.launching_rocket_league = true;
        self.multiplayer.setup_progress =
            Some(t("launch-multiplayer-starting-rocket-league-on-the-workshop").to_string());
        let rl_path = rl_path.to_string();
        let rl_launch = self.rl_launch.clone();
        let manager = self.manager.clone();
        let tx = tx.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            manager.seed_empty_slots(&rl_path);
            let result = crate::winutil::restart_rocket_league_multihome(
                &rl_launch,
                Path::new(&rl_path),
                &tailnet_ip,
            );
            let _ = tx.send(AppMsg::WorkshopMultiplayerLaunched { result });
            repaint.request_repaint();
        });
    }

    pub fn set_multiplayer_progress(&mut self, status: String) {
        self.multiplayer.setup_progress = Some(status);
    }

    pub fn finish_multiplayer_launch(&mut self, result: Result<(), String>) {
        self.multiplayer.setup_progress = None;
        match result {
            Ok(()) => {
                self.multiplayer.status =
                    t("finish-multiplayer-launch-rocket-league-is-starting-on-the").to_string();
            }
            Err(error) => {
                self.multiplayer.launching_rocket_league = false;
                self.multiplayer.status = format!("Could not start Rocket League: {error}");
            }
        }
    }

    /// starts the one relay every peer runs, regardless of whether this
    /// machine ends up hosting or joining inside Rocket League - see
    /// hosting.rs. Triggered automatically once Rocket League is up on the
    /// Workshop network, not by a separate host/join button.
    fn start_relay(&mut self, rl_path: &str, tx: &Sender<AppMsg>, ctx: &eframe::egui::Context) {
        let Some(tailnet_ip) = self.multiplayer.tailnet_ip.clone() else {
            self.multiplayer.status = t("start-relay-the-workshop-network-is-not-ready").to_string();
            return;
        };
        let Some(sidecar) = self.multiplayer.sidecar.clone() else {
            self.multiplayer.status = t("start-relay-the-workshop-network-is-not-ready").to_string();
            return;
        };
        let executable = match std::env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                self.multiplayer.status = format!("Could not locate Hebnix: {error}");
                return;
            }
        };
        let rocket_league = match rocket_league_executable(rl_path) {
            Ok(path) => path,
            Err(error) => {
                self.multiplayer.status = error;
                return;
            }
        };
        self.multiplayer.setup_progress = Some(t("start-relay-starting-the-workshop-lan-relay").to_string());
        let manager = self.manager.clone();
        // the in-game name comes from Rocket League's launch log and doesn't
        // change mid-session, so it's only looked up until one is found
        let player_name = Arc::new(Mutex::new(String::new()));
        let map_provider: MapProvider = Arc::new(move || {
            let mut name = player_name.lock().unwrap();
            if name.is_empty() {
                *name = hebnix_sdk::log::parse_launch_log(None, false, "INT")
                    .session
                    .username
                    .unwrap_or_default();
            }
            LocalInfo {
                player_name: name.clone(),
                maps: manager
                    .active_maps()
                    .iter()
                    .filter_map(|(slot, data)| {
                        let id = id_of(data);
                        valid_map_id(&id).then(|| SlotMap {
                            slot: slot.clone(),
                            name: str_of(data, "name", slot).to_string(),
                            local: is_local_map_id(&id),
                            author: str_of(data, "author", "").to_string(),
                            description: str_of(data, "short_description", "").to_string(),
                            id,
                        })
                    })
                    .collect(),
            }
        });
        // peers can fetch any map in the cache, imported or from the cdn,
        // unless P2P file sharing is turned off in the system settings
        let cache_dir = self.manager.cache_dir.clone();
        let share_files = self.share_files.clone();
        let map_files: MapFileProvider = Arc::new(move |id| {
            if !share_files.load(Ordering::Relaxed) || !valid_map_id(id) {
                return None;
            }
            let path = cache_dir.join(format!("{id}.upk"));
            path.is_file().then_some(path)
        });
        let tx = tx.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                ensure_beacon_relay_rule(&executable)?;
                ensure_map_sync_rule(&executable)?;
                // which peer(s) will actually connect isn't known up front
                // any more (no room to report joined players' addresses),
                // so this opens the whole tailnet range rather than one IP
                ensure_rocket_league_lan_rule(&rocket_league, "10.242.77.0/24")?;
                HostSession::start(sidecar, tailnet_ip, map_provider, map_files, tx.clone())
            })();
            let _ = tx.send(AppMsg::WorkshopRelayStarted { result });
            repaint.request_repaint();
        });
    }

    /// who replaced which in-game map with what, next to what you have in
    /// the same slot. every player has to replace the same in-game map, so
    /// this flags a peer's map that sits in a different slot on your side.
    /// players are a collapsed list so it stays usable with lots of them.
    fn render_maps_in_use(
        &mut self,
        ui: &mut egui::Ui,
        install: &mut Option<(SlotMap, IpAddr, bool)>,
        block: &mut Option<(IpAddr, String)>,
    ) {
        let Some(session) = &self.multiplayer.relay else {
            return;
        };
        let mine = self.manager.active_maps();
        let mut offers = session.peer_offers();
        offers.sort_by_key(|offer| offer.label().to_lowercase());
        let busy = self.multiplayer.map_install_busy.load(Ordering::Relaxed);
        ui.group(|ui| {
            ui.strong(t("maps-in-use-maps-in-use"));
            ui.small(
                t("maps-in-use-everyone-has-to-replace-the-same"),
            );
            ui.add_space(4.0);
            ui.label(egui::RichText::new(t("maps-in-use-you")).strong());
            if mine.is_empty() {
                ui.small(format!("  {}", t("maps-none-replaced-yet")));
            }
            for (slot, data) in &mine {
                render_my_slot(ui, slot, data, &offers);
            }

            ui.add_space(6.0);
            ui.label(egui::RichText::new(t_args("maps-in-use-players-offers", &[("offers", (offers.len()).to_string().into())])).strong());
            if offers.is_empty() {
                ui.small(t("maps-in-use-no-other-players-found-yet"));
            } else {
                ui.small(
                    t("maps-in-use-maps-installed-from-another-player-come"),
                );
            }
            if offers.len() > PLAYER_SEARCH_THRESHOLD {
                ui.horizontal(|ui| {
                    ui.label(t("spoofer-search"));
                    ui.text_edit_singleline(&mut self.multiplayer.player_filter);
                });
            } else {
                self.multiplayer.player_filter.clear();
            }
            let filter = self.multiplayer.player_filter.clone();
            egui::ScrollArea::vertical()
                .id_salt("workshop_players")
                .max_height(320.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let mut shown = 0;
                    for offer in offers.iter().filter(|o| matches_filter(&o.label(), &filter)) {
                        shown += 1;
                        egui::CollapsingHeader::new(player_header(ui, offer, &mine))
                            .id_salt(("workshop_player", offer.ip))
                            .default_open(false)
                            .show(ui, |ui| {
                                render_player_maps(ui, offer, &mine, busy, install);
                                ui.add_space(4.0);
                                if ui
                                    .button(t("maps-in-use-block-player"))
                                    .on_hover_text(
                                        t("maps-in-use-hide-this-player-s-maps-and"),
                                    )
                                    .clicked()
                                {
                                    *block = Some((offer.ip, offer.label()));
                                }
                            });
                    }
                    if shown == 0 && !offers.is_empty() {
                        ui.small(t("maps-in-use-no-players-match-that-search"));
                    }
                });

            let transfer = &self.multiplayer.map_transfer;
            let total = transfer.total.load(Ordering::Relaxed);
            if busy && total > 0 {
                let done = transfer.done.load(Ordering::Relaxed);
                ui.add(
                    egui::ProgressBar::new(done as f32 / total as f32).text(format!(
                        "Downloading from a player: {} / {} MB",
                        done / 1_000_000,
                        total / 1_000_000
                    )),
                );
            } else if busy {
                ui.small(t("maps-in-use-installing-the-map"));
            } else if let Ok(result) = self.multiplayer.map_install_result.lock() {
                if !result.is_empty() {
                    ui.small(result.as_str());
                }
            }
        });
    }

    /// asks once per session before installing a map straight from another
    /// player's pc
    fn render_peer_install_warning(&mut self, rl_path: &str, ctx: &eframe::egui::Context) {
        let Some((map, _, _)) = &self.multiplayer.pending_peer_install else {
            return;
        };
        let name = map.name.clone();
        let mut choice = None;
        egui::Window::new(t("peer-install-warning-install-from-another-player")).id(egui::Id::new("peer-install-warning-install-from-another-player"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(t_args("peer-install-warning-name-will-be-downloaded-straight-from", &[("name", name.to_string().into())]));
                ui.label(
                    t("peer-install-warning-hebnix-only-checks-that-the-file"),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(t("hebnix-install-install")).clicked() {
                        choice = Some(true);
                    }
                    if ui.button(t("spawner-enable-prompt-cancel")).clicked() {
                        choice = Some(false);
                    }
                });
            });
        match choice {
            Some(true) => {
                self.multiplayer.peer_warning_accepted = true;
                if let Some((map, peer, from_peer)) = self.multiplayer.pending_peer_install.take() {
                    self.install_peer_map(map, peer, from_peer, rl_path, ctx);
                }
            }
            Some(false) => self.multiplayer.pending_peer_install = None,
            None => {}
        }
    }

    fn save_multiplayer_settings(&self) {
        let settings = serde_json::json!({ "relay_only": self.multiplayer.relay_only });
        let _ = std::fs::write(
            self.manager.runtime_dir.join(MULTIPLAYER_SETTINGS_FILE),
            settings.to_string(),
        );
    }

    /// installs a map a peer has into the same slot. cdn maps are downloaded
    /// from the cdn unless `from_peer` is set; maps the peer imported
    /// themselves (not on the cdn) are always fetched from that peer and
    /// checked against their id first. the id was validated when it came off
    /// the wire (see map_sync.rs).
    fn install_peer_map(
        &mut self,
        map: SlotMap,
        peer: IpAddr,
        from_peer: bool,
        rl_path: &str,
        ctx: &eframe::egui::Context,
    ) {
        if self
            .multiplayer
            .map_install_busy
            .swap(true, Ordering::Relaxed)
        {
            return;
        }
        let map_data = self
            .catalog
            .iter()
            .find(|entry| id_of(entry) == map.id)
            .cloned()
            .unwrap_or_else(|| {
                serde_json::json!({
                    "id": map.id,
                    "name": map.name,
                    "author": if map.author.is_empty() { "Unknown" } else { map.author.as_str() },
                    "short_description": map.description,
                    "local": map.local,
                })
            });
        let manager = self.manager.clone();
        let busy = self.multiplayer.map_install_busy.clone();
        let result = self.multiplayer.map_install_result.clone();
        let transfer = self.multiplayer.map_transfer.clone();
        let received = self.multiplayer.received_maps.clone();
        transfer.done.store(0, Ordering::Relaxed);
        transfer.total.store(0, Ordering::Relaxed);
        let rl_path = rl_path.to_string();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let outcome = (|| {
                if (map.local || from_peer) && !manager.is_cached(&map.id) {
                    fetch_map_file(
                        peer,
                        MAP_SYNC_PORT,
                        &map.id,
                        &manager.cache_dir.join(format!("{}.upk", map.id)),
                        MAX_MAP_BYTES,
                        &transfer,
                    )?;
                    if map.local {
                        let entry = LocalMap {
                            id: map.id.clone(),
                            name: map.name.clone(),
                            author: if map.author.is_empty() {
                                "Unknown".to_string()
                            } else {
                                map.author.clone()
                            },
                            description: map.description.clone(),
                            banner_path: String::new(),
                        };
                        record_received_map(&manager.runtime_dir, &entry)?;
                        if let Ok(mut inbox) = received.lock() {
                            inbox.push(entry);
                        }
                    }
                }
                manager.install_map(&map_data, &map.slot, &rl_path)
            })();
            let text = match outcome {
                Ok(()) => format!("Installed {} in {}.", map.name, map.slot),
                Err(error) => format!("Could not install {}: {error}", map.name),
            };
            if let Ok(mut slot) = result.lock() {
                *slot = text;
            }
            busy.store(false, Ordering::Relaxed);
            repaint.request_repaint();
        });
    }

    pub fn refresh_launch_status(&mut self, tx: &Sender<AppMsg>, ctx: &eframe::egui::Context) {
        if !self.multiplayer.wizard_started
            || self.multiplayer.multihome_check_in_flight
            || self.multiplayer.tailnet_ip.is_none()
        {
            return;
        }
        let Some(tailnet_ip) = self.multiplayer.tailnet_ip.clone() else {
            return;
        };
        self.multiplayer.multihome_check_in_flight = true;
        let tx = tx.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let rl_open = hebnix_sdk::process::is_rocket_league_running();
            if rl_open {
                std::thread::sleep(MULTIHOME_CHECK_INTERVAL);
            }
            let rl_open = hebnix_sdk::process::is_rocket_league_running();
            let launch_ready = rl_open && rocket_league_launched_with_multihome(&tailnet_ip);
            let _ = tx.send(AppMsg::WorkshopLaunchCheck {
                rl_open,
                launch_ready,
            });
            repaint.request_repaint();
        });
    }

    pub fn finish_launch_check(&mut self, rl_open: bool, launch_ready: bool) {
        self.multiplayer.rl_open = rl_open;
        self.multiplayer.launch_ready = launch_ready;
        self.multiplayer.multihome_check_in_flight = false;
        if !rl_open || launch_ready {
            self.multiplayer.multihome_check_attempts = 0;
        } else {
            self.multiplayer.multihome_check_attempts =
                self.multiplayer.multihome_check_attempts.saturating_add(1);
        }
    }

    pub fn retry_multihome_check(&self) -> bool {
        self.multiplayer.wizard_started
            && self.multiplayer.rl_open
            && !self.multiplayer.launch_ready
            && !self.multiplayer.multihome_check_in_flight
            && self.multiplayer.multihome_check_attempts < MULTIHOME_CHECK_MAX_ATTEMPTS
    }

    fn waiting_for_multihome_check(&self) -> bool {
        self.multiplayer.rl_open
            && !self.multiplayer.launch_ready
            && self.multiplayer.multihome_check_attempts < MULTIHOME_CHECK_MAX_ATTEMPTS
    }

    pub fn update_workshop_map_from_stats(&mut self, arena: &str, _tx: &Sender<AppMsg>) {
        if !self.multiplayer.wizard_started || arena.trim().is_empty() {
            return;
        }
        // informational only now (shown once the relay's up) - starting the
        // relay itself no longer needs to know the map, host or guest
        let arena = arena.trim_end_matches(".upk");
        if let Some((target, map)) = self.manager.active_maps().into_iter().find(|(target, _)| {
            target_filename(target)
                .map(|name| name.trim_end_matches(".upk").eq_ignore_ascii_case(arena))
                .unwrap_or_else(|| target.trim_end_matches(".upk").eq_ignore_ascii_case(arena))
        }) {
            self.multiplayer.detected_map = Some(str_of(&map, "name", &target).to_string());
            self.multiplayer.detected_target = Some(target);
        }
    }

    pub fn finish_relay_started(&mut self, result: Result<HostSession, String>) {
        self.multiplayer.setup_progress = None;
        match result {
            Ok(session) => {
                self.multiplayer.identity_updated = false;
                self.multiplayer.identity_update_in_flight = false;
                self.multiplayer.status =
                    t("finish-relay-started-relaying-host-or-join-from-rocket").to_string();
                self.multiplayer.relay = Some(session);
            }
            Err(error) => {
                self.multiplayer.status = format!("Could not start the relay: {}", redact(&error))
            }
        }
    }

    pub fn finish_player_update(&mut self, result: Result<(), String>) {
        self.multiplayer.identity_update_in_flight = false;
        match result {
            Ok(()) => self.multiplayer.identity_updated = true,
            Err(error) => {
                self.multiplayer.status = format!("Could not update player details: {error}")
            }
        }
    }

    pub fn finish_host_session_check(
        &mut self,
        result: Result<crate::multiplayer_lan::Room, String>,
    ) {
        self.multiplayer.saved_host_checking = false;
        self.multiplayer.saved_host_checked = true;
        match result {
            Ok(room) => {
                self.multiplayer.saved_room = Some(room.clone());
                self.multiplayer.status = format!(
                    "Previous hosting session {} is alive for {}.",
                    room.pin, room.map.name
                );
            }
            Err(_) => {
                self.multiplayer.saved_host = None;
                self.multiplayer.saved_room = None;
                self.clear_host_state();
            }
        }
    }

    fn save_host_state(&self) {
        if let Some(saved) = &self.multiplayer.saved_host {
            if let Ok(bytes) = serde_json::to_vec(saved) {
                let _ = std::fs::write(
                    self.manager.runtime_dir.join("multiplayer_host.json"),
                    bytes,
                );
            }
        }
    }

    fn clear_host_state(&mut self) {
        self.multiplayer.saved_host = None;
        self.multiplayer.saved_room = None;
        let _ = std::fs::remove_file(self.manager.runtime_dir.join("multiplayer_host.json"));
    }

    fn map_hash(&self, map_id: &str) -> Result<String, String> {
        let path = self.manager.cache_dir.join(format!("{map_id}.upk"));
        let mut file = std::fs::File::open(&path)
            .map_err(|error| format!("Could not open the downloaded map: {error}"))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            let count = file
                .read(&mut buffer)
                .map_err(|error| format!("Could not read the downloaded map: {error}"))?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        Ok(hex::encode(hasher.finalize()))
    }

    fn render_card(&mut self, ui: &mut egui::Ui, map_idx: usize) -> Option<CardAction> {
        let map_data = &self.catalog[map_idx];
        let map_id = id_of(map_data);
        let mut name = str_of(map_data, "name", "Unknown").to_string();
        // full name + description show when hovering the title
        let hover = match str_of(map_data, "short_description", "") {
            "" => name.clone(),
            description => format!("{name}\n\n{description}"),
        };
        if name.chars().count() > 28 {
            name = format!("{}...", name.chars().take(25).collect::<String>());
        }
        let author = str_of(map_data, "author", "Unknown").to_string();
        let banner = str_of(map_data, "banner_path", "").to_string();

        let active_targets = self.manager.get_active_targets_for_map(&map_id);
        let is_active_on_current = active_targets.contains(&self.target);
        let is_cached = self.manager.is_cached(&map_id);
        let is_busy = self.busy.contains(&map_id);

        let mut result = None;

        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_min_height(230.0);
            ui.vertical_centered(|ui| {
                // Image
                let img_size = egui::vec2(160.0, 90.0);
                match self.images.get(&banner) {
                    Some(ImageState::Ready(bytes)) => {
                        ui.add(
                            egui::Image::from_bytes(
                                format!("bytes://workshop/{banner}"),
                                bytes.clone(),
                            )
                            .fit_to_exact_size(img_size),
                        );
                    }
                    Some(ImageState::Failed) => {
                        ui.add_sized(img_size, egui::Label::new(t("card-failed-to-load")));
                    }
                    _ => {
                        if banner.is_empty() {
                            ui.add_sized(img_size, egui::Label::new(t("card-no-image-available")));
                        } else {
                            ui.add_sized(img_size, egui::Label::new(t("card-loading-image")));
                        }
                    }
                }

                ui.strong(name).on_hover_text(hover);
                ui.label(
                    egui::RichText::new(t_args("hebnix-install-by-author", &[("author", author.to_string().into())]))
                        .italics()
                        .size(11.0)
                        .color(egui::Color32::GRAY),
                );

                let status = if !active_targets.is_empty() {
                    format!("🟢 Active on: {}", active_targets.join(", "))
                } else if is_local_entry(map_data) {
                    "📁 Imported".to_string()
                } else if is_cached {
                    "📦 Cached".to_string()
                } else {
                    "☁ Cloud".to_string()
                };
                ui.label(egui::RichText::new(status).size(12.0));
                ui.add_space(4.0);

                let (btn_text, btn_color) = if is_busy {
                    (t("tab-working").to_string(), None)
                } else if is_active_on_current {
                    (
                        format!("Unload {}", self.target),
                        Some(egui::Color32::from_rgb(0xc0, 0x39, 0x2b)),
                    )
                } else if is_cached {
                    (format!("Load to {}", self.target), None)
                } else {
                    (format!("Download for {}", self.target), None)
                };

                ui.horizontal(|ui| {
                    let mut button = egui::Button::new(btn_text);
                    if let Some(color) = btn_color {
                        button = button.fill(color);
                    }
                    let show_delete = is_cached && active_targets.is_empty() && !is_busy;
                    let btn_width = if show_delete {
                        ui.available_width() - 34.0
                    } else {
                        ui.available_width()
                    };
                    if ui
                        .add_enabled(!is_busy, button.min_size(egui::vec2(btn_width, 24.0)))
                        .clicked()
                    {
                        result = Some(if is_active_on_current {
                            CardAction::Unload
                        } else {
                            CardAction::InstallOrDownload
                        });
                    }
                    if show_delete
                        && ui
                            .add(
                                egui::Button::new("🗑")
                                    .fill(egui::Color32::from_rgb(0xc0, 0x39, 0x2b))
                                    .min_size(egui::vec2(28.0, 24.0)),
                            )
                            .clicked()
                    {
                        result = Some(CardAction::DeleteCache);
                    }
                });
            });
        });

        result
    }

    fn handle_action(
        &mut self,
        map_idx: usize,
        action: CardAction,
        rl_path: &str,
        tx: &Sender<AppMsg>,
        ctx: &eframe::egui::Context,
    ) {
        let map_data = self.catalog[map_idx].clone();
        let map_id = id_of(&map_data);

        match action {
            CardAction::Unload => match self.manager.unload_active_map(&self.target, rl_path) {
                Ok(()) => self.execute_search(false),
                Err(e) => {
                    let _ = tx.send(AppMsg::Log(format!("[Workshop] {e}")));
                }
            },
            CardAction::DeleteCache => {
                self.confirm_delete = Some(map_data);
            }
            CardAction::InstallOrDownload => {
                self.busy.insert(map_id.clone());
                let manager = self.manager.clone();
                let target = self.target.clone();
                let rl_path = rl_path.to_string();
                let tx = tx.clone();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let result = manager.install_map(&map_data, &target, &rl_path);
                    let msg = match &result {
                        Ok(()) => format!(
                            "[Workshop] Installed '{}' to {target}.",
                            str_of(&map_data, "name", "map")
                        ),
                        Err(e) => format!("[Workshop] Map Install Error: {e}"),
                    };
                    let _ = tx.send(AppMsg::WorkshopOpDone { message: msg });
                    ctx.request_repaint();
                });
            }
        }
    }

    /// called when a WorkshopOpDone message arrives
    pub fn finish_op(&mut self) {
        self.busy.clear();
        self.execute_search(false);
    }

    /// Rocket League closed. Rather than tearing the session down
    /// immediately (which would kick everyone out of the room over an
    /// ordinary crash or restart), this arms a grace window; see
    /// tick_shutdown_grace for the part that actually tears things down.
    pub fn shutdown_multiplayer(&mut self) {
        if self.multiplayer.launching_rocket_league {
            // this is Hebnix's own close-then-relaunch, not a real exit
            return;
        }
        if self.multiplayer.relay.is_none() || self.multiplayer.shutdown_deadline.is_some() {
            return;
        }
        self.multiplayer.shutdown_deadline =
            Some(std::time::Instant::now() + crate::multiplayer_lan::CRASH_GRACE_WINDOW);
        self.multiplayer.status =
            t("shutdown-multiplayer-rocket-league-closed-keeping-the-session").to_string();
    }

    /// call periodically (piggybacked on the existing RL-status poll) to
    /// resolve the crash grace window one way or the other
    pub fn tick_shutdown_grace(&mut self) {
        let Some(deadline) = self.multiplayer.shutdown_deadline else {
            return;
        };
        if hebnix_sdk::process::is_rocket_league_running() {
            self.multiplayer.shutdown_deadline = None;
            self.multiplayer.status = t("tick-shutdown-grace-rocket-league-reconnected").to_string();
            return;
        }
        if std::time::Instant::now() < deadline {
            return;
        }
        self.multiplayer.shutdown_deadline = None;
        let relay = self.multiplayer.relay.take();
        self.multiplayer.status =
            t("tick-shutdown-grace-workshop-multiplayer-stopped-because-roc").to_string();

        // Stopping a session can wait for a network heartbeat, while firewall
        // cleanup launches external commands. This runs from the egui
        // message handler, so doing any of that here freezes the whole app.
        std::thread::Builder::new()
            .name("workshop-shutdown".into())
            .spawn(move || {
                if let Some(mut session) = relay {
                    let _ = session.stop();
                }
                let _ = crate::winutil::clear_rocket_league_multihome();
                let _ = crate::multiplayer_lan::cleanup_system_state();
            })
            .ok();
    }

    pub fn rocket_league_reopened(&mut self) {
        self.multiplayer.launching_rocket_league = false;
        self.multiplayer.shutdown_deadline = None;
    }

    pub fn suspend_multiplayer(&mut self) {
        if let Some(session) = self.multiplayer.relay.as_mut() {
            session.suspend();
        }
        self.multiplayer.relay = None;
        self.multiplayer.sidecar = None;
        if !hebnix_sdk::process::is_rocket_league_running() {
            let _ = crate::winutil::clear_rocket_league_multihome();
            let _ = crate::multiplayer_lan::cleanup_system_state();
        }
    }
}

enum CardAction {
    InstallOrDownload,
    Unload,
    DeleteCache,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot_map(slot: &str, id: &str, name: &str) -> SlotMap {
        SlotMap {
            slot: slot.into(),
            id: id.into(),
            name: name.into(),
            ..Default::default()
        }
    }

    fn offer(maps: Vec<SlotMap>) -> PeerOffer {
        PeerOffer {
            ip: "10.242.77.3".parse().unwrap(),
            player_name: "Squishy".into(),
            hostname: String::new(),
            maps,
        }
    }

    #[test]
    fn a_players_maps_are_summed_up_in_one_line() {
        assert_eq!(offer_map_summary(&[]), "no map replaced");
        let one = slot_map("Utopia Retro", "1", "Rings");
        assert_eq!(offer_map_summary(std::slice::from_ref(&one)), "Rings");
        let many = vec![
            one,
            slot_map("Underpass", "2", "Dunk"),
            slot_map("Roadblock", "3", "Obstacle"),
        ];
        assert_eq!(offer_map_summary(&many), "Rings + 2 more");
        // slots hebnix doesn't know about don't count
        assert_eq!(
            offer_map_summary(&[slot_map("Nowhere", "4", "Ghost")]),
            "no map replaced"
        );
    }

    #[test]
    fn the_player_search_ignores_case_and_spaces() {
        assert!(matches_filter("Squishy", ""));
        assert!(matches_filter("Squishy", "  "));
        assert!(matches_filter("Squishy", "squ"));
        assert!(matches_filter("Squishy", " ISH "));
        assert!(!matches_filter("Squishy", "garrett"));
    }

    #[test]
    fn a_player_differs_unless_you_have_their_maps_in_the_same_slots() {
        let mut mine = serde_json::Map::new();
        mine.insert("Utopia Retro".into(), serde_json::json!({"id": "1"}));
        assert!(!player_differs(&offer(vec![]), &mine));
        assert!(!player_differs(&offer(vec![slot_map("Utopia Retro", "1", "Rings")]), &mine));
        assert!(player_differs(&offer(vec![slot_map("Utopia Retro", "9", "Other")]), &mine));
        assert!(player_differs(&offer(vec![slot_map("Underpass", "1", "Rings")]), &mine));
    }

    #[test]
    fn reads_the_api_shape() {
        let m: Value = serde_json::from_str(
            r#"{"id":"3","name":"Rings Of Death","author":"fractalrl",
                "banner_path":"/files/maps/3/3.jpg","short_description":"x",
                "version_number":"1","download_count":"0"}"#,
        )
        .unwrap();
        assert_eq!(id_of(&m), "3");
        assert_eq!(str_of(&m, "name", ""), "Rings Of Death");
        assert_eq!(str_of(&m, "author", "Unknown"), "fractalrl");
        assert_eq!(str_of(&m, "banner_path", ""), "/files/maps/3/3.jpg");
    }

    #[test]
    fn banner_path_to_url_and_cache_rel() {
        let (url, rel) = banner_url_and_cache_rel("/files/maps/3/3.jpg");
        assert_eq!(url, "https://hebnix.com/files/maps/3/3.jpg");
        assert_eq!(rel, "files/maps/3/3.jpg");
        assert!(
            !std::path::Path::new(&rel).has_root(),
            "must stay relative or the cache write escapes cache_dir"
        );
    }

    #[test]
    fn id_of_takes_string_or_number() {
        assert_eq!(id_of(&serde_json::json!({ "id": "12" })), "12");
        assert_eq!(id_of(&serde_json::json!({ "id": 12 })), "12");
        assert_eq!(id_of(&serde_json::json!({})), "0");
    }
}
