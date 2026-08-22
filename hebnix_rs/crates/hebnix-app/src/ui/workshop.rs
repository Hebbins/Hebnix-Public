//! workshop maps tab: browse the hebnix.com catalog, download + swap maps
//! over the rocket labs placeholders.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::Sender;
use eframe::egui;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::messages::AppMsg;
use crate::multiplayer_lan::{
    CreateRoomRequest, FIRST_GUEST_ADDRESS, GUEST_ADDRESS_RANGE, GuestSession, HOST_ADDRESS,
    HostSession, JoinRoomRequest, JoinedRoom, MapDescriptor, RoomClient, UpdatePlayerRequest,
    ensure_host_rule, ensure_join_rule_if_needed, ensure_rocket_league_lan_rule,
};

const MULTIHOME_CHECK_MAX_ATTEMPTS: u8 = 30;
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

fn multiplayer_player_identity(player_token: String) -> UpdatePlayerRequest {
    let info = hebnix_sdk::log::parse_launch_log(None, false, "INT");
    let detected_platform = hebnix_sdk::process::find_rocket_league()
        .map(|process| process.platform.as_str().to_string());
    let platform = detected_platform
        .or(info.session.platform.clone())
        .unwrap_or_else(|| "Unknown".to_string());
    let platform_id = info
        .session
        .primary_id
        .or(info.session.steam_id)
        .or(info.session.epic_id)
        .unwrap_or_else(|| format!("{}-{}", platform, std::process::id()));
    let display_name = info
        .session
        .username
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| {
            std::env::var("USERNAME").unwrap_or_else(|_| "Hebnix Player".to_string())
        });
    UpdatePlayerRequest {
        player_token,
        platform_id,
        platform,
        display_name,
    }
}

fn multiplayer_join_request() -> JoinRoomRequest {
    JoinRoomRequest {
        player_token: multiplayer_client_token(),
    }
}

fn multiplayer_client_token() -> String {
    use rand::RngCore;

    let path = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Hebnix")
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
        address
            .starts_with(&format!("{}.", crate::multiplayer_lan::VPN_SUBNET))
            .then_some(address)
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

        let resp = get_retry(&url, Duration::from_secs(25))?;
        let mut bytes: Vec<u8> = Vec::new();
        resp.into_reader()
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        std::fs::write(&zip_path, &bytes).map_err(|e| e.to_string())?;

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
                "No valid .upk or .udk map file found inside the downloaded archive.".to_string(),
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
    Multiplayer,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MultiplayerMode {
    Host,
    Join,
}

struct MultiplayerState {
    wizard_started: bool,
    mode: MultiplayerMode,
    host_name: String,
    port: u16,
    join_pin: String,
    join_after_start: bool,
    identity_update_in_flight: bool,
    identity_updated: bool,
    prepared_tap: Option<crate::multiplayer_lan::TapSession>,
    pending_join: Option<JoinedRoom>,
    hosted: Option<HostSession>,
    joined: Option<GuestSession>,
    status: String,
    binding_status: String,
    tap_ready_before_launch: bool,
    restarting_rocket_league: bool,
    setup_progress: Option<String>,
    saved_host: Option<SavedHost>,
    saved_room: Option<crate::multiplayer_lan::Room>,
    saved_host_checked: bool,
    saved_host_checking: bool,
    detected_target: Option<String>,
    wizard_check: WizardCheck,
    multihome_check_attempts: u8,
}

struct WizardCheck {
    rl_open: bool,
    tap_ready: bool,
    launch_ready: bool,
    detected_map: Option<String>,
    in_flight: bool,
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
            mode: MultiplayerMode::Host,
            host_name: "Hebnix Workshop".to_string(),
            port: 47777,
            join_pin: String::new(),
            join_after_start: false,
            identity_update_in_flight: false,
            identity_updated: false,
            prepared_tap: None,
            pending_join: None,
            hosted: None,
            joined: None,
            status: "Choose a downloaded map to host, or enter a four-digit pin to join."
                .to_string(),
            binding_status: String::new(),
            tap_ready_before_launch: false,
            restarting_rocket_league: false,
            setup_progress: None,
            saved_host: None,
            saved_room: None,
            saved_host_checked: false,
            saved_host_checking: false,
            detected_target: None,
            wizard_check: WizardCheck {
                rl_open: false,
                tap_ready: false,
                launch_ready: false,
                detected_map: None,
                in_flight: false,
            },
            multihome_check_attempts: 0,
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
    multiplayer: MultiplayerState,
}

impl WorkshopState {
    pub fn new(base_dir: &Path) -> Self {
        let manager = MapManager::new(base_dir);
        let saved_host = std::fs::read(manager.runtime_dir.join("multiplayer_host.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let mut multiplayer = MultiplayerState::default();
        multiplayer.saved_host = saved_host;
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
            catalog_status: "Loading catalog...".to_string(),
            fetched: false,
            confirm_delete: None,
            view: WorkshopView::Browse,
            multiplayer,
        }
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

    /// render the tab. rl_path comes from the app config.
    pub fn render(&mut self, ui: &mut egui::Ui, rl_path: &str, tx: &Sender<AppMsg>) {
        let ctx = ui.ctx().clone();

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.view, WorkshopView::Browse, "Browse Maps");
            ui.selectable_value(&mut self.view, WorkshopView::Multiplayer, "Multiplayer");
        });
        ui.separator();
        if self.view == WorkshopView::Multiplayer {
            self.render_multiplayer(ui, rl_path, tx, &ctx);
            return;
        }

        // Toolbar
        ui.horizontal(|ui| {
            ui.strong("Search:");
            let search_resp = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Name or author...")
                    .desired_width(200.0),
            );
            let submitted =
                search_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Search").clicked() || submitted {
                self.execute_search(true);
            }
            if ui
                .checkbox(&mut self.view_downloaded, "View Downloaded")
                .changed()
            {
                self.execute_search(true);
            }

            ui.strong("Map To Replace:");
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
                    egui::Button::new("Restore Original")
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
                .add_enabled(self.page > 0, egui::Button::new("<< Prev"))
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
                    egui::Button::new("Next >>"),
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
                            "No maps found.".to_string()
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
            egui::Window::new("Offboard Map")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.label(format!(
                        "Are you sure you want to delete '{name}' from your downloaded cache?"
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Yes").clicked() {
                            let ok = self.manager.delete_from_cache(&id_of(&map_data));
                            if !ok {
                                let _ = tx.send(AppMsg::Log(
                                    "[Workshop] Failed to delete file. Ensure the game is closed or the file isn't in use.".to_string(),
                                ));
                            }
                            self.execute_search(false);
                            close = true;
                        }
                        if ui.button("No").clicked() {
                            close = true;
                        }
                    });
                });
            if close {
                self.confirm_delete = None;
            }
        }
    }

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
                ui.heading("Workshop Multiplayer");
                ui.label("Choose how you want to connect.");
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    let width = 140.0;
                    if ui
                        .add_sized([width, 38.0], egui::Button::new("Host"))
                        .clicked()
                    {
                        self.multiplayer.mode = MultiplayerMode::Host;
                        self.multiplayer.wizard_started = true;
                        self.refresh_wizard_status(tx, ctx);
                    }
                    if ui
                        .add_sized([width, 38.0], egui::Button::new("Join"))
                        .clicked()
                    {
                        self.multiplayer.mode = MultiplayerMode::Join;
                        self.multiplayer.wizard_started = true;
                        self.refresh_wizard_status(tx, ctx);
                    }
                });
            });
            return;
        }
        let mut host = false;
        let mut stop = false;
        let mut join = false;
        let mut prepare_host = false;
        let mut prepare_guest = false;
        let mut close_game = false;
        let is_admin = crate::spoofer::is_admin();
        let setup_in_progress = self.multiplayer.setup_progress.is_some();
        let rl_open = self.multiplayer.wizard_check.rl_open;
        let tap_ready = self.multiplayer.wizard_check.tap_ready;
        let wizard_ready = rl_open && tap_ready && self.multiplayer.wizard_check.launch_ready;

        if self.multiplayer.mode == MultiplayerMode::Host
            && self.multiplayer.saved_host.is_some()
            && !self.multiplayer.saved_host_checked
            && !self.multiplayer.saved_host_checking
        {
            self.multiplayer.saved_host_checking = true;
            let pin = self.multiplayer.saved_host.as_ref().unwrap().pin.clone();
            let tx = tx.clone();
            let repaint = ctx.clone();
            std::thread::spawn(move || {
                let result = RoomClient::new("https://api.hebnix.com").get_room(&pin);
                let _ = tx.send(AppMsg::WorkshopHostSessionCheck { result });
                repaint.request_repaint();
            });
        }

        if !is_admin {
            ui.colored_label(
                egui::Color32::YELLOW,
                "Workshop multiplayer requires Hebnix to run as administrator.",
            );
        }
        ui.horizontal(|ui| {
            if self.multiplayer.hosted.is_none()
                && self.multiplayer.joined.is_none()
                && ui.button("Back").clicked()
            {
                self.multiplayer.wizard_started = false;
                return;
            }
            ui.strong(match self.multiplayer.mode {
                MultiplayerMode::Host => "Hosting a Workshop LAN match",
                MultiplayerMode::Join => "Joining a Workshop LAN match",
            });
        });
        ui.group(|ui| {
            ui.strong("Workshop Multiplayer setup");
            if self.multiplayer.saved_host_checking {
                ui.small("Checking the previous hosting session...");
            } else if let Some(saved) = &self.multiplayer.saved_host {
                ui.small(format!(
                    "Previous hosting PIN {} is still available.",
                    saved.pin
                ));
            }
            if self.multiplayer.mode == MultiplayerMode::Join {
                ui.label("Step 1: Enter the host PIN.");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.multiplayer.join_pin)
                        .hint_text("Four-digit PIN")
                        .desired_width(130.0),
                );
                if response.changed() {
                    self.multiplayer
                        .join_pin
                        .retain(|character| character.is_ascii_digit());
                    self.multiplayer.join_pin.truncate(4);
                }
            }
            if !rl_open {
                ui.label(match self.multiplayer.mode {
                    MultiplayerMode::Host => "Step 1: Rocket League is closed.",
                    MultiplayerMode::Join => "Step 2: Rocket League is closed.",
                });
                let pin_ready = self.multiplayer.mode != MultiplayerMode::Join
                    || self.multiplayer.join_pin.len() == 4;
                if ui
                    .add_enabled(
                        !setup_in_progress && is_admin && pin_ready,
                        egui::Button::new(match self.multiplayer.mode {
                            MultiplayerMode::Host => "Set up TAP & start Rocket League",
                            MultiplayerMode::Join => "Join & start Rocket League",
                        }),
                    )
                    .clicked()
                {
                    match self.multiplayer.mode {
                        MultiplayerMode::Host => prepare_host = true,
                        MultiplayerMode::Join => {
                            self.multiplayer.join_after_start = true;
                            prepare_guest = true;
                        }
                    }
                }
            } else if !tap_ready {
                ui.label(match self.multiplayer.mode {
                    MultiplayerMode::Host => {
                        "Step 1: The Workshop LAN adapter is not configured for hosting."
                    }
                    MultiplayerMode::Join => {
                        "Step 2: The Workshop LAN adapter is not configured for joining."
                    }
                });
                if ui.button("Close Rocket League").clicked() {
                    close_game = true;
                }
            } else if !self.multiplayer.wizard_check.launch_ready {
                if self.waiting_for_multihome_check() {
                    ui.label(match self.multiplayer.mode {
                        MultiplayerMode::Host => {
                            "Step 1: Waiting for Rocket League to apply the Workshop host address."
                        }
                        MultiplayerMode::Join => {
                            "Step 2: Waiting for Rocket League to apply your Workshop address."
                        }
                    });
                    ui.small("Checking the Rocket League launch command... ");
                } else {
                    ui.label(match self.multiplayer.mode {
                    MultiplayerMode::Host => {
                        "Step 1: Rocket League was not started with the Workshop host address."
                    }
                    MultiplayerMode::Join => {
                        "Step 2: Rocket League was not started with your assigned Workshop address."
                    }
                });
                    ui.small(
                    "Rocket League must restart because multihome is fixed when the game starts.",
                );
                    if ui.button("Close Rocket League").clicked() {
                        close_game = true;
                    }
                }
            } else {
                ui.label(match self.multiplayer.mode {
                    MultiplayerMode::Host => {
                        "Step 1: Rocket League and the Workshop LAN adapter are ready."
                    }
                    MultiplayerMode::Join => {
                        "Step 2: Rocket League and the Workshop LAN adapter are ready."
                    }
                });
                match self.multiplayer.mode {
                    MultiplayerMode::Host => match &self.multiplayer.wizard_check.detected_map {
                        Some(name) => {
                            ui.label(format!("Step 2: Detected LAN Match on map {name}."));
                            if self.multiplayer.hosted.is_none()
                                && ui
                                    .add_enabled(
                                        !setup_in_progress,
                                        egui::Button::new("Create PIN"),
                                    )
                                    .clicked()
                            {
                                host = true;
                            }
                        }
                        None => {
                            ui.label(if self.multiplayer.wizard_check.in_flight {
                                "Step 2: Checking for a LAN Match..."
                            } else {
                                "Step 2: Waiting for LAN Match."
                            });
                        }
                    },
                    MultiplayerMode::Join => {
                        ui.label("Step 3: Join the host session below.");
                    }
                }
            }
        });
        ui.columns(2, |columns| {
            if self.multiplayer.mode == MultiplayerMode::Host {
                columns[0].group(|ui| {
                    ui.heading("Host workshop map");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.multiplayer.host_name)
                            .hint_text("Host name"),
                    );
                    ui.horizontal(|ui| {
                        ui.label("Tunnel UDP port");
                        ui.add(egui::DragValue::new(&mut self.multiplayer.port).range(1..=65535));
                    });
                    if self.multiplayer.hosted.is_some() {
                        let pin = &self.multiplayer.hosted.as_ref().unwrap().credentials.pin;
                        ui.add_space(8.0);
                        ui.strong(format!("Hosting PIN: {pin}"));
                        ui.label("This session refreshes every five minutes.");
                        let stats = &self.multiplayer.hosted.as_ref().unwrap().stats;
                        ui.small(format!(
                            "Tunnel: {} · sent {} · received {} · delivered {}",
                            if stats.connected.load(Ordering::Relaxed) {
                                "peer connected"
                            } else {
                                "waiting for peer"
                            },
                            stats.sent.load(Ordering::Relaxed),
                            stats.received.load(Ordering::Relaxed),
                            stats.delivered.load(Ordering::Relaxed)
                        ));
                        if let Ok(flow) = stats.last_sent_lan_udp.lock() {
                            if !flow.is_empty() {
                                ui.small(format!("Rocket League LAN UDP out: {flow}"));
                            } else {
                                ui.small("Rocket League LAN UDP out: not observed");
                            }
                        }
                        if let Ok(flow) = stats.last_received_lan_udp.lock() {
                            if !flow.is_empty() {
                                ui.small(format!("Rocket League LAN UDP in: {flow}"));
                            } else {
                                ui.small("Rocket League LAN UDP in: not observed");
                            }
                        }
                        if let Ok(flow) = stats.last_sent_udp.lock() {
                            if !flow.is_empty() {
                                ui.small(format!("Latest TAP UDP out: {flow}"));
                            }
                        }
                        if let Ok(flow) = stats.last_received_udp.lock() {
                            if !flow.is_empty() {
                                ui.small(format!("Latest TAP UDP in: {flow}"));
                            }
                        }
                        if let Ok(flow) = stats.last_sent_broadcast_udp.lock() {
                            if !flow.is_empty() {
                                ui.small(format!("Latest non-system TAP broadcast out: {flow}"));
                            }
                        }
                        if let Ok(flow) = stats.last_received_broadcast_udp.lock() {
                            if !flow.is_empty() {
                                ui.small(format!("Latest non-system TAP broadcast in: {flow}"));
                            }
                        }
                        if ui.button("Stop hosting").clicked() {
                            stop = true;
                        }
                    }
                });
            }
            if self.multiplayer.mode == MultiplayerMode::Join {
                columns[0].group(|ui| {
                    ui.heading("Join workshop map");
                    if wizard_ready
                        && self.multiplayer.mode == MultiplayerMode::Join
                        && !self.multiplayer.join_after_start
                    {
                        if ui
                            .add_enabled(
                                is_admin
                                    && !setup_in_progress
                                    && self.multiplayer.join_pin.len() == 4,
                                egui::Button::new("Join by PIN"),
                            )
                            .clicked()
                        {
                            join = true;
                        }
                    } else if self.multiplayer.joined.is_none() {
                        ui.small("Complete the setup steps above before joining.");
                    }
                    if let Some(session) = &self.multiplayer.joined {
                        let room = &session.joined.room;
                        ui.add_space(8.0);
                        ui.strong(&room.host_name);
                        ui.label(format!("{}:{}", room.endpoint.host, room.endpoint.port));
                        ui.label(format!("Map: {}", room.map.name));
                        ui.label("Install the matching Workshop map before connecting.");
                        ui.small(format!(
                            "Tunnel: {} · sent {} · received {} · delivered {}",
                            if session.stats.connected.load(Ordering::Relaxed) {
                                "host connected"
                            } else {
                                "waiting for host"
                            },
                            session.stats.sent.load(Ordering::Relaxed),
                            session.stats.received.load(Ordering::Relaxed),
                            session.stats.delivered.load(Ordering::Relaxed)
                        ));
                        if let Ok(flow) = session.stats.last_sent_lan_udp.lock() {
                            if !flow.is_empty() {
                                ui.small(format!("Rocket League LAN UDP out: {flow}"));
                            } else {
                                ui.small("Rocket League LAN UDP out: not observed");
                            }
                        }
                        if let Ok(flow) = session.stats.last_received_lan_udp.lock() {
                            if !flow.is_empty() {
                                ui.small(format!("Rocket League LAN UDP in: {flow}"));
                            } else {
                                ui.small("Rocket League LAN UDP in: not observed");
                            }
                        }
                        if let Ok(flow) = session.stats.last_sent_udp.lock() {
                            if !flow.is_empty() {
                                ui.small(format!("Latest TAP UDP out: {flow}"));
                            }
                        }
                        if let Ok(flow) = session.stats.last_received_udp.lock() {
                            if !flow.is_empty() {
                                ui.small(format!("Latest TAP UDP in: {flow}"));
                            }
                        }
                        if let Ok(flow) = session.stats.last_sent_broadcast_udp.lock() {
                            if !flow.is_empty() {
                                ui.small(format!("Latest non-system TAP broadcast out: {flow}"));
                            }
                        }
                        if let Ok(flow) = session.stats.last_received_broadcast_udp.lock() {
                            if !flow.is_empty() {
                                ui.small(format!("Latest non-system TAP broadcast in: {flow}"));
                            }
                        }
                        if ui.button("Leave").clicked() {
                            if let Some(mut session) = self.multiplayer.joined.take() {
                                let _ = session.leave();
                            }
                            self.multiplayer.pending_join = None;
                            let _ = crate::winutil::clear_rocket_league_multihome();
                            self.multiplayer.status = "Left session.".to_string();
                        }
                    }
                });
            }
        });
        ui.add_space(10.0);
        if !self.multiplayer.binding_status.is_empty() {
            ui.small(&self.multiplayer.binding_status);
        }
        if !self.multiplayer.tap_ready_before_launch
            && (self.multiplayer.hosted.is_some() || self.multiplayer.joined.is_some())
        {
            ui.colored_label(
                egui::Color32::YELLOW,
                "TAP opened after Rocket League launched; restart through Set up TAP & restart Rocket League for LAN discovery.",
            );
        }
        ui.label(&self.multiplayer.status);
        if let Some(progress) = &self.multiplayer.setup_progress {
            ui.add(egui::ProgressBar::new(0.5).animate(true).text(progress));
        }

        if prepare_host {
            self.prepare_multiplayer(rl_path, HOST_ADDRESS, None, tx, ctx);
        }
        if prepare_guest {
            self.prepare_multiplayer(
                rl_path,
                FIRST_GUEST_ADDRESS,
                Some(self.multiplayer.join_pin.clone()),
                tx,
                ctx,
            );
        }
        if self.multiplayer.mode == MultiplayerMode::Join
            && wizard_ready
            && self.multiplayer.join_after_start
            && self.multiplayer.joined.is_none()
            && !setup_in_progress
        {
            self.multiplayer.join_after_start = false;
            join = true;
        }

        if close_game {
            self.multiplayer.status =
                "Closing Rocket League. Set up the Workshop LAN adapter once it has exited."
                    .to_string();
            std::thread::spawn(|| {
                let _ = crate::winutil::kill_rocket_league();
            });
        }

        if stop {
            if let Some(mut session) = self.multiplayer.hosted.take() {
                self.multiplayer.status = match session.stop() {
                    Ok(()) => "Hosting stopped and session closed.".to_string(),
                    Err(error) => format!(
                        "Hosting stopped locally, but the API could not close the session: {error}"
                    ),
                };
            }
            self.clear_host_state();
            let _ = crate::winutil::clear_rocket_league_multihome();
        }
        if host {
            if !is_admin {
                self.multiplayer.status = "Run Hebnix as administrator before hosting.".to_string();
            } else {
                self.start_hosting(rl_path, tx, ctx);
            }
        }
        if join {
            if !is_admin {
                self.multiplayer.status = "Run Hebnix as administrator before joining.".to_string();
                return;
            }
            self.join_multiplayer(rl_path, tx, ctx);
        }
    }

    fn join_multiplayer(
        &mut self,
        rl_path: &str,
        tx: &Sender<AppMsg>,
        ctx: &eframe::egui::Context,
    ) {
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
        let prepared_tap = self.multiplayer.prepared_tap.take();
        let pending_join = self.multiplayer.pending_join.take();
        if prepared_tap.is_none() {
            self.multiplayer.tap_ready_before_launch = false;
        }
        self.multiplayer.setup_progress = Some("Joining the Workshop LAN session...".to_string());
        let pin = self.multiplayer.join_pin.clone();
        let tx = tx.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let client = RoomClient::new("https://api.hebnix.com");
                let join_request = multiplayer_join_request();
                let joined = match pending_join {
                    Some(joined) => joined,
                    None => client.join_room(&pin, &join_request)?,
                };
                if let Some(launched_address) = rocket_league_multihome_address()
                    && launched_address != joined.assigned_ip
                {
                    let _ = client.leave_room(&joined.room.pin, &joined.leave_token);
                    return Err(format!(
                        "This player was assigned {}, but Rocket League is using {launched_address}. Restart it through Join & start Rocket League.",
                        joined.assigned_ip
                    ));
                }
                let tunnel = match prepared_tap {
                    Some(tunnel) => tunnel,
                    None => crate::multiplayer_lan::configure_existing(&joined.assigned_ip)
                        .and_then(|_| crate::multiplayer_lan::TapSession::open())?,
                };
                ensure_join_rule_if_needed(
                    &executable,
                    &joined.room.endpoint.host,
                    joined.room.endpoint.port,
                )?;
                ensure_rocket_league_lan_rule(&rocket_league, HOST_ADDRESS)?;
                GuestSession::start(joined, join_request, tunnel)
            })();
            let _ = tx.send(AppMsg::WorkshopGuestJoined { result });
            repaint.request_repaint();
        });
    }

    fn prepare_multiplayer(
        &mut self,
        rl_path: &str,
        address: &str,
        join_pin: Option<String>,
        tx: &Sender<AppMsg>,
        ctx: &eframe::egui::Context,
    ) {
        self.multiplayer.prepared_tap.take();
        self.multiplayer.restarting_rocket_league = true;
        self.multiplayer.setup_progress = Some("Preparing the Workshop LAN adapter...".to_string());
        let rl_path = rl_path.to_string();
        let address = address.to_string();
        let join_pin = join_pin.filter(|pin| pin.len() == 4);
        let tx = tx.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let joined = join_pin
                .as_deref()
                .map(|pin| {
                    RoomClient::new("https://api.hebnix.com")
                        .join_room(pin, &multiplayer_join_request())
                })
                .transpose();
            let joined = match joined {
                Ok(joined) => joined,
                Err(error) => {
                    let _ = tx.send(AppMsg::WorkshopMultiplayerPrepared { result: Err(error) });
                    repaint.request_repaint();
                    return;
                }
            };
            let address = joined
                .as_ref()
                .map(|joined| joined.assigned_ip.clone())
                .unwrap_or(address);
            let cleanup_join = joined.clone();
            let _ = tx.send(AppMsg::WorkshopMultiplayerProgress(
                "Opening the Workshop LAN adapter...".to_string(),
            ));
            let result = crate::multiplayer_lan::ensure_adapter(&address)
                .and_then(|_| crate::multiplayer_lan::TapSession::open());
            let result = result.and_then(|tunnel| {
                let _ = tx.send(AppMsg::WorkshopMultiplayerProgress(
                    "Starting Rocket League with the Workshop LAN address...".to_string(),
                ));
                crate::winutil::restart_rocket_league_multihome(Path::new(&rl_path), &address)
                    .map(|_| (tunnel, joined))
            });
            if result.is_err()
                && let Some(joined) = cleanup_join
            {
                let _ = RoomClient::new("https://api.hebnix.com")
                    .leave_room(&joined.room.pin, &joined.leave_token);
            }
            let _ = tx.send(AppMsg::WorkshopMultiplayerPrepared { result });
            repaint.request_repaint();
        });
    }

    pub fn set_multiplayer_progress(&mut self, status: String) {
        self.multiplayer.setup_progress = Some(status);
    }

    pub fn finish_multiplayer_prepare(
        &mut self,
        result: Result<
            (
                crate::multiplayer_lan::TapSession,
                Option<crate::multiplayer_lan::JoinedRoom>,
            ),
            String,
        >,
    ) {
        self.multiplayer.setup_progress = None;
        match result {
            Ok((tunnel, joined)) => {
                self.multiplayer.prepared_tap = Some(tunnel);
                self.multiplayer.pending_join = joined;
                self.multiplayer.tap_ready_before_launch = true;
                self.multiplayer.status =
                    "TAP is ready and Rocket League is restarting with its virtual LAN address."
                        .to_string();
            }
            Err(error) => {
                self.multiplayer.restarting_rocket_league = false;
                self.multiplayer.join_after_start = false;
                self.multiplayer.status = format!("Could not set up Workshop LAN: {error}");
            }
        }
    }

    fn start_hosting(&mut self, rl_path: &str, tx: &Sender<AppMsg>, ctx: &eframe::egui::Context) {
        let Some(target) = self.multiplayer.detected_target.clone() else {
            self.multiplayer.status =
                "Start the LAN match first, then wait for Stats API to report its Workshop map."
                    .to_string();
            return;
        };
        let active = self.manager.active_maps();
        let Some((_, map)) = active
            .into_iter()
            .find(|(active_target, _)| *active_target == target)
        else {
            self.multiplayer.status =
                "The detected Workshop map is no longer installed.".to_string();
            return;
        };
        let map_id = id_of(&map);
        let hash = match self.map_hash(&map_id) {
            Ok(hash) => hash,
            Err(error) => {
                self.multiplayer.status = error;
                return;
            }
        };
        let request = CreateRoomRequest {
            host_name: self.multiplayer.host_name.trim().to_string(),
            port: self.multiplayer.port,
            map: MapDescriptor {
                id: map_id.clone(),
                name: str_of(&map, "name", &target).to_string(),
                sha256: hash,
                download_url: format!("https://api.hebnix.com/download/map/{map_id}"),
            },
            protocol_version: 2,
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
        let prepared_tap = self.multiplayer.prepared_tap.take();
        let previous_host = self.multiplayer.saved_host.clone();
        if prepared_tap.is_none() {
            self.multiplayer.tap_ready_before_launch = false;
        }
        self.multiplayer.setup_progress = Some("Creating Workshop LAN session...".to_string());
        let tx = tx.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                ensure_host_rule(&executable, request.port)?;
                ensure_rocket_league_lan_rule(&rocket_league, GUEST_ADDRESS_RANGE)?;
                let tunnel = match prepared_tap {
                    Some(tunnel) => tunnel,
                    None => crate::multiplayer_lan::configure_existing(HOST_ADDRESS)
                        .and_then(|_| crate::multiplayer_lan::TapSession::open())?,
                };
                let client = RoomClient::new("https://api.hebnix.com");
                if let Some(previous) = previous_host {
                    let _ = client.close_room(&previous.pin, &previous.host_secret);
                }
                HostSession::start(client, request, tunnel)
            })();
            let _ = tx.send(AppMsg::WorkshopHostStarted { result });
            repaint.request_repaint();
        });
    }

    pub fn refresh_wizard_status(&mut self, tx: &Sender<AppMsg>, ctx: &eframe::egui::Context) {
        if !self.multiplayer.wizard_started || self.multiplayer.wizard_check.in_flight {
            return;
        }
        let address = match self.multiplayer.mode {
            MultiplayerMode::Host => HOST_ADDRESS.to_string(),
            MultiplayerMode::Join => self
                .multiplayer
                .pending_join
                .as_ref()
                .map(|joined| joined.assigned_ip.clone())
                .or_else(rocket_league_multihome_address)
                .unwrap_or_else(|| FIRST_GUEST_ADDRESS.to_string()),
        };
        self.multiplayer.wizard_check.in_flight = true;
        let tx = tx.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let rl_open = hebnix_sdk::process::is_rocket_league_running();
            let tap_ready =
                rl_open && crate::multiplayer_lan::is_configured(&address).unwrap_or(false);
            if rl_open && tap_ready {
                std::thread::sleep(MULTIHOME_CHECK_INTERVAL);
            }
            let rl_open = hebnix_sdk::process::is_rocket_league_running();
            let launch_ready = rl_open && rocket_league_launched_with_multihome(&address);
            let _ = tx.send(AppMsg::WorkshopWizardCheck {
                rl_open,
                tap_ready,
                launch_ready,
                detected_map: None,
            });
            repaint.request_repaint();
        });
    }

    pub fn finish_wizard_check(
        &mut self,
        rl_open: bool,
        tap_ready: bool,
        launch_ready: bool,
        detected_map: Option<String>,
    ) {
        self.multiplayer.wizard_check.rl_open = rl_open;
        self.multiplayer.wizard_check.tap_ready = tap_ready;
        self.multiplayer.wizard_check.launch_ready = launch_ready;
        if detected_map.is_some() {
            self.multiplayer.wizard_check.detected_map = detected_map;
        }
        self.multiplayer.wizard_check.in_flight = false;
        if !rl_open || launch_ready {
            self.multiplayer.multihome_check_attempts = 0;
        } else if tap_ready {
            self.multiplayer.multihome_check_attempts =
                self.multiplayer.multihome_check_attempts.saturating_add(1);
        }
    }

    pub fn retry_multihome_check(&self) -> bool {
        self.multiplayer.wizard_started
            && self.multiplayer.wizard_check.rl_open
            && self.multiplayer.wizard_check.tap_ready
            && !self.multiplayer.wizard_check.launch_ready
            && !self.multiplayer.wizard_check.in_flight
            && self.multiplayer.multihome_check_attempts < MULTIHOME_CHECK_MAX_ATTEMPTS
    }

    fn waiting_for_multihome_check(&self) -> bool {
        self.multiplayer.wizard_check.rl_open
            && self.multiplayer.wizard_check.tap_ready
            && !self.multiplayer.wizard_check.launch_ready
            && self.multiplayer.multihome_check_attempts < MULTIHOME_CHECK_MAX_ATTEMPTS
    }

    pub fn update_workshop_map_from_stats(&mut self, arena: &str, tx: &Sender<AppMsg>) {
        if !self.multiplayer.wizard_started || arena.trim().is_empty() {
            return;
        }
        if self.multiplayer.mode == MultiplayerMode::Join
            && self.multiplayer.joined.is_some()
            && !self.multiplayer.identity_updated
            && !self.multiplayer.identity_update_in_flight
        {
            let session = self.multiplayer.joined.as_ref().unwrap();
            let pin = session.joined.room.pin.clone();
            let token = session.joined.leave_token.clone();
            self.multiplayer.identity_update_in_flight = true;
            let tx = tx.clone();
            std::thread::spawn(move || {
                let request = multiplayer_player_identity(token);
                let result =
                    RoomClient::new("https://api.hebnix.com").update_player(&pin, &request);
                let _ = tx.send(AppMsg::WorkshopPlayerUpdated { result });
            });
            return;
        }
        if self.multiplayer.mode != MultiplayerMode::Host {
            return;
        }
        let arena = arena.trim_end_matches(".upk");
        if let Some((target, map)) = self.manager.active_maps().into_iter().find(|(target, _)| {
            target_filename(target)
                .map(|name| name.trim_end_matches(".upk").eq_ignore_ascii_case(arena))
                .unwrap_or_else(|| target.trim_end_matches(".upk").eq_ignore_ascii_case(arena))
        }) {
            self.multiplayer.wizard_check.detected_map =
                Some(str_of(&map, "name", &target).to_string());
            self.multiplayer.detected_target = Some(target);
        }
    }

    pub fn finish_hosting(&mut self, result: Result<HostSession, String>) {
        self.multiplayer.setup_progress = None;
        match result {
            Ok(session) => {
                self.multiplayer.status = format!("Hosting session {}.", session.credentials.pin);
                self.multiplayer.saved_host = Some(SavedHost {
                    pin: session.credentials.pin.clone(),
                    host_secret: session.credentials.host_secret.clone(),
                });
                self.multiplayer.saved_host_checked = true;
                self.save_host_state();
                self.multiplayer.hosted = Some(session);
            }
            Err(error) => self.multiplayer.status = format!("Could not create session: {error}"),
        }
    }

    pub fn finish_joining(&mut self, result: Result<GuestSession, String>) {
        self.multiplayer.setup_progress = None;
        match result {
            Ok(session) => {
                self.multiplayer.identity_updated = false;
                self.multiplayer.identity_update_in_flight = false;
                self.multiplayer.status = format!(
                    "Joined session {} as {}.",
                    session.joined.room.pin, session.joined.assigned_ip
                );
                self.multiplayer.joined = Some(session);
            }
            Err(error) => self.multiplayer.status = format!("Could not join session: {error}"),
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
                        ui.add_sized(img_size, egui::Label::new("Failed to load"));
                    }
                    _ => {
                        if banner.is_empty() {
                            ui.add_sized(img_size, egui::Label::new("No Image Available"));
                        } else {
                            ui.add_sized(img_size, egui::Label::new("Loading image..."));
                        }
                    }
                }

                ui.strong(name);
                ui.label(
                    egui::RichText::new(format!("by {author}"))
                        .italics()
                        .size(11.0)
                        .color(egui::Color32::GRAY),
                );

                let status = if !active_targets.is_empty() {
                    format!("🟢 Active on: {}", active_targets.join(", "))
                } else if is_cached {
                    "📦 Cached".to_string()
                } else {
                    "☁ Cloud".to_string()
                };
                ui.label(egui::RichText::new(status).size(12.0));
                ui.add_space(4.0);

                let (btn_text, btn_color) = if is_busy {
                    ("Working...".to_string(), None)
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

    pub fn shutdown_multiplayer(&mut self) {
        if self.multiplayer.restarting_rocket_league {
            return;
        }
        if let Some(mut session) = self.multiplayer.hosted.take() {
            let _ = session.stop();
        }
        self.clear_host_state();
        if let Some(mut session) = self.multiplayer.joined.take() {
            let _ = session.leave();
        }
        self.multiplayer.prepared_tap.take();
        let _ = crate::winutil::clear_rocket_league_multihome();
        let _ = crate::multiplayer_lan::cleanup_system_state();
        self.multiplayer.status =
            "Workshop multiplayer stopped because Rocket League closed.".to_string();
    }

    pub fn rocket_league_reopened(&mut self) {
        self.multiplayer.restarting_rocket_league = false;
    }

    pub fn suspend_multiplayer(&mut self) {
        if let Some(session) = self.multiplayer.hosted.as_mut() {
            session.suspend();
        }
        self.multiplayer.hosted = None;
        if let Some(session) = self.multiplayer.joined.as_mut() {
            session.stop();
        }
        self.multiplayer.joined = None;
        self.multiplayer.prepared_tap.take();
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
