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
    CreateRoomRequest, GuestSession, HostSession, JoinRoomRequest, JoinedRoom, MapDescriptor,
    ROOM_API_BASE_URL, RL_LAN_PORT, RoomClient, TSNET_CONTROL_URL, TsnetSidecarHandle,
    UpdatePlayerRequest, ensure_beacon_relay_rule, ensure_rocket_league_lan_rule,
    ensure_sidecar_rule,
};
mod background_changer;
use background_changer::BackgroundChangerState;

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

fn multiplayer_player_identity(player_token: String, tailnet_ip: String) -> UpdatePlayerRequest {
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
        tailnet_ip,
    }
}

fn multiplayer_join_request() -> JoinRoomRequest {
    JoinRoomRequest {
        player_token: multiplayer_client_token(),
    }
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
    BackgroundChanger,
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
    join_pin: String,
    identity_update_in_flight: bool,
    identity_updated: bool,
    hosted: Option<HostSession>,
    joined: Option<GuestSession>,
    /// guest: the room joined during launch_multiplayer, waiting for the
    /// "Join by PIN" button to actually start the GuestSession
    pending_join: Option<JoinedRoom>,
    status: String,
    setup_progress: Option<String>,
    saved_host: Option<SavedHost>,
    saved_room: Option<crate::multiplayer_lan::Room>,
    saved_host_checked: bool,
    saved_host_checking: bool,
    detected_target: Option<String>,
    detected_map: Option<String>,

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
            mode: MultiplayerMode::Host,
            host_name: "Hebnix Workshop".to_string(),
            join_pin: String::new(),
            identity_update_in_flight: false,
            identity_updated: false,
            hosted: None,
            joined: None,
            pending_join: None,
            status: "Choose a downloaded map to host, or enter a four-digit pin to join."
                .to_string(),
            setup_progress: None,
            saved_host: None,
            saved_room: None,
            saved_host_checked: false,
            saved_host_checking: false,
            detected_target: None,
            detected_map: None,
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
    multiplayer: MultiplayerState,
    rl_launch: crate::config::RlLaunchCfg,
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
            background_changer: BackgroundChangerState::default(),
            multiplayer,
            rl_launch: crate::config::RlLaunchCfg::default(),
        }
    }

    pub fn finish_background_changer(&mut self, result: Result<String, String>) -> String {
        self.background_changer.finish(result)
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
            ui.selectable_value(&mut self.view, WorkshopView::Browse, "Browse Maps");
            ui.selectable_value(
                &mut self.view,
                WorkshopView::BackgroundChanger,
                "Background Changer",
            );
            // ui.selectable_value(&mut self.view, WorkshopView::Multiplayer, "Multiplayer");
        });
        ui.separator();
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
                        self.start_tailnet(tx, ctx);
                    }
                    if ui
                        .add_sized([width, 38.0], egui::Button::new("Join"))
                        .clicked()
                    {
                        self.multiplayer.mode = MultiplayerMode::Join;
                        self.multiplayer.wizard_started = true;
                        self.start_tailnet(tx, ctx);
                    }
                });
            });
            return;
        }
        let mut host = false;
        let mut stop = false;
        let mut join = false;
        let mut launch = false;
        let mut close_game = false;
        let is_admin = crate::spoofer::is_admin();
        let setup_in_progress = self.multiplayer.setup_progress.is_some();
        let tailnet_ready = self.multiplayer.tailnet_ip.is_some();
        let wizard_ready = tailnet_ready && self.multiplayer.rl_open && self.multiplayer.launch_ready;

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
                let result = RoomClient::new(ROOM_API_BASE_URL).get_room(&pin);
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
            if !tailnet_ready {
                ui.label("Connecting to the private Workshop network...");
            } else if !self.multiplayer.rl_open {
                let pin_ready = self.multiplayer.mode != MultiplayerMode::Join
                    || self.multiplayer.join_pin.len() == 4;
                ui.label(match self.multiplayer.mode {
                    MultiplayerMode::Host => "Step 1: Start Rocket League on the Workshop network.",
                    MultiplayerMode::Join => "Step 2: Join and start Rocket League on the Workshop network.",
                });
                if ui
                    .add_enabled(
                        !setup_in_progress && is_admin && pin_ready,
                        egui::Button::new(match self.multiplayer.mode {
                            MultiplayerMode::Host => "Start Rocket League",
                            MultiplayerMode::Join => "Join & start Rocket League",
                        }),
                    )
                    .clicked()
                {
                    launch = true;
                }
            } else if !self.multiplayer.launch_ready {
                if self.waiting_for_multihome_check() {
                    ui.label(match self.multiplayer.mode {
                        MultiplayerMode::Host => {
                            "Step 1: Waiting for Rocket League to apply the Workshop address."
                        }
                        MultiplayerMode::Join => {
                            "Step 2: Waiting for Rocket League to apply the Workshop address."
                        }
                    });
                    ui.small("Checking the Rocket League launch command... ");
                } else {
                    ui.label(
                        "Rocket League was not started with the Workshop network address.",
                    );
                    ui.small(
                        "Rocket League must restart because multihome is fixed when the game starts.",
                    );
                    if ui.button("Close Rocket League").clicked() {
                        close_game = true;
                    }
                }
            } else {
                ui.label("Rocket League is ready on the Workshop network.");
                match self.multiplayer.mode {
                    MultiplayerMode::Host => match &self.multiplayer.detected_map {
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
                            ui.label("Step 2: Waiting for LAN Match.");
                        }
                    },
                    MultiplayerMode::Join => {
                        if self.multiplayer.joined.is_none()
                            && ui
                                .add_enabled(
                                    is_admin && !setup_in_progress,
                                    egui::Button::new("Join by PIN"),
                                )
                                .clicked()
                        {
                            join = true;
                        }
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
                    if let Some(session) = &self.multiplayer.hosted {
                        ui.add_space(8.0);
                        ui.strong(format!("Hosting PIN: {}", session.credentials.pin));
                        ui.label("This session refreshes every five minutes.");
                        ui.small(format!(
                            "Tunnel: {} · sent {} · received {}",
                            if session.stats.connected.load(Ordering::Relaxed) {
                                "peer connected"
                            } else {
                                "waiting for peer"
                            },
                            session.stats.sent.load(Ordering::Relaxed),
                            session.stats.received.load(Ordering::Relaxed),
                        ));
                        if let Ok(flow) = session.stats.last_beacon_relayed.lock() {
                            if !flow.is_empty() {
                                ui.small(format!("Latest: {flow}"));
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
                    if let Some(session) = &self.multiplayer.joined {
                        let room = &session.joined.room;
                        ui.add_space(8.0);
                        ui.strong(&room.host_name);
                        ui.label(format!("Map: {}", room.map.name));
                        ui.label("Install the matching Workshop map before connecting.");
                        ui.small(format!(
                            "Tunnel: {} · sent {} · received {}",
                            if session.stats.connected.load(Ordering::Relaxed) {
                                "host connected"
                            } else if session.stats.join_failed.load(Ordering::Relaxed) {
                                "could not reach host"
                            } else {
                                "waiting for host"
                            },
                            session.stats.sent.load(Ordering::Relaxed),
                            session.stats.received.load(Ordering::Relaxed),
                        ));
                        if let Ok(flow) = session.stats.last_beacon_relayed.lock() {
                            if !flow.is_empty() {
                                ui.small(format!("Latest: {flow}"));
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
                    } else if self.multiplayer.pending_join.is_none() {
                        ui.small("Complete the setup steps above before joining.");
                    }
                });
            }
        });
        ui.add_space(10.0);
        ui.label(&self.multiplayer.status);
        if let Some(progress) = &self.multiplayer.setup_progress {
            ui.add(egui::ProgressBar::new(0.5).animate(true).text(progress));
        }

        if launch {
            self.launch_multiplayer(rl_path, tx, ctx);
        }

        if close_game {
            self.multiplayer.status =
                "Closing Rocket League. Start it again once it has exited.".to_string();
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
            } else {
                self.join_multiplayer(tx, ctx);
            }
        }
        let _ = wizard_ready;
    }

    /// Spawns (or reuses) the tsnet sidecar and brings the tailnet up. This
    /// happens as soon as the user picks Host/Join, before Rocket League is
    /// touched at all, so the multihome address is known up front instead
    /// of being discovered after a launch-and-detect cycle.
    fn start_tailnet(&mut self, tx: &Sender<AppMsg>, ctx: &eframe::egui::Context) {
        if self.multiplayer.tailnet_requested {
            return;
        }
        self.multiplayer.tailnet_requested = true;
        self.multiplayer.status = "Setting up the private Workshop network...".to_string();
        let role = match self.multiplayer.mode {
            MultiplayerMode::Host => "host",
            MultiplayerMode::Join => "guest",
        }
        .to_string();
        let tx = tx.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<Arc<TsnetSidecarHandle>, String> {
                let executable = std::env::current_exe().map_err(|error| error.to_string())?;
                let exe_dir = executable.parent().ok_or_else(|| {
                    "could not locate Hebnix's install folder".to_string()
                })?;
                ensure_sidecar_rule(&exe_dir.join("tailscaled.exe"))?;
                let state_dir = crate::config::base_dir()
                    .join("multiplayer-lan")
                    .join("tsnet-state");
                let handle = TsnetSidecarHandle::spawn(exe_dir, &state_dir, tx.clone())?;
                let key = RoomClient::new(TSNET_CONTROL_URL).request_tsnet_authkey(&role, "")?;
                let token = multiplayer_client_token();
                let hostname = format!("hebnix-{}", &token[..token.len().min(8)]);
                handle.request_up(key.auth_key, hostname, key.control_url)?;
                Ok(Arc::new(handle))
            })();
            let _ = tx.send(AppMsg::WorkshopTailnetStarted { result });
            repaint.request_repaint();
        });
    }

    pub fn finish_tailnet_started(&mut self, result: Result<Arc<TsnetSidecarHandle>, String>) {
        match result {
            Ok(sidecar) => {
                self.multiplayer.sidecar = Some(sidecar);
                self.multiplayer.status =
                    "Connected to the private Workshop network.".to_string();
            }
            Err(error) => {
                self.multiplayer.tailnet_requested = false;
                self.multiplayer.status = format!("Could not set up the Workshop network: {error}");
            }
        }
    }

    /// called from AppMsg::TsnetUpResult once the sidecar actually finishes
    /// authenticating and reports a tailnet address
    pub fn set_tailnet_ip(&mut self, tailnet_ip: String) {
        self.multiplayer.tailnet_ip = Some(tailnet_ip);
        self.multiplayer.status =
            "Ready on the private Workshop network.".to_string();
    }

    pub fn tailnet_failed(&mut self, error: String) {
        self.multiplayer.tailnet_requested = false;
        self.multiplayer.status = format!("The Workshop network connection failed: {error}");
    }

    fn launch_multiplayer(
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
        let Some(tailnet_ip) = self.multiplayer.tailnet_ip.clone() else {
            self.multiplayer.status = "The Workshop network is not ready yet.".to_string();
            return;
        };
        self.multiplayer.launching_rocket_league = true;
        self.multiplayer.setup_progress =
            Some("Starting Rocket League on the Workshop network...".to_string());
        let rl_path = rl_path.to_string();
        let rl_launch = self.rl_launch.clone();
        let mode = self.multiplayer.mode;
        let pin = self.multiplayer.join_pin.clone();
        let tx = tx.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<Option<JoinedRoom>, String> {
                let client = RoomClient::new(ROOM_API_BASE_URL);
                let joined = if mode == MultiplayerMode::Join {
                    let joined = client.join_room(&pin, &multiplayer_join_request())?;
                    let identity =
                        multiplayer_player_identity(multiplayer_client_token(), tailnet_ip.clone());
                    let _ = client.update_player(&joined.room.pin, &identity);
                    ensure_rocket_league_lan_rule(&rocket_league, &joined.room.endpoint.host)?;
                    Some(joined)
                } else {
                    None
                };
                let _ = &executable;
                crate::winutil::restart_rocket_league_multihome(
                    &rl_launch,
                    Path::new(&rl_path),
                    &tailnet_ip,
                )?;
                Ok(joined)
            })();
            let _ = tx.send(AppMsg::WorkshopMultiplayerLaunched { result });
            repaint.request_repaint();
        });
    }

    pub fn set_multiplayer_progress(&mut self, status: String) {
        self.multiplayer.setup_progress = Some(status);
    }

    pub fn finish_multiplayer_launch(&mut self, result: Result<Option<JoinedRoom>, String>) {
        self.multiplayer.setup_progress = None;
        match result {
            Ok(joined) => {
                self.multiplayer.pending_join = joined;
                self.multiplayer.status =
                    "Rocket League is starting on the Workshop network.".to_string();
            }
            Err(error) => {
                self.multiplayer.launching_rocket_league = false;
                self.multiplayer.status = format!("Could not start Rocket League: {error}");
            }
        }
    }

    fn join_multiplayer(&mut self, tx: &Sender<AppMsg>, ctx: &eframe::egui::Context) {
        let Some(joined) = self.multiplayer.pending_join.clone() else {
            self.multiplayer.status = "Join & start Rocket League first.".to_string();
            return;
        };
        let Some(sidecar) = self.multiplayer.sidecar.clone() else {
            self.multiplayer.status = "The Workshop network is not ready.".to_string();
            return;
        };
        self.multiplayer.setup_progress = Some("Joining the Workshop LAN session...".to_string());
        let identity = multiplayer_join_request();
        let tx = tx.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let result = GuestSession::start(joined, identity, sidecar);
            let _ = tx.send(AppMsg::WorkshopGuestJoined { result });
            repaint.request_repaint();
        });
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
        let Some(tailnet_ip) = self.multiplayer.tailnet_ip.clone() else {
            self.multiplayer.status = "The Workshop network is not ready.".to_string();
            return;
        };
        let Some(sidecar) = self.multiplayer.sidecar.clone() else {
            self.multiplayer.status = "The Workshop network is not ready.".to_string();
            return;
        };
        let request = CreateRoomRequest {
            host_name: self.multiplayer.host_name.trim().to_string(),
            port: RL_LAN_PORT,
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
        let previous_host = self.multiplayer.saved_host.clone();
        self.multiplayer.setup_progress = Some("Creating Workshop LAN session...".to_string());
        let tx = tx.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                ensure_beacon_relay_rule(&executable)?;
                // the exact guest tailnet address isn't known until they
                // join, so this is scoped to headscale's default CGNAT
                // range rather than a single IP -- narrow this once the
                // room API can report joined players' addresses up front.
                ensure_rocket_league_lan_rule(&rocket_league, "100.64.0.0/10")?;
                let client = RoomClient::new(ROOM_API_BASE_URL);
                if let Some(previous) = previous_host {
                    let _ = client.close_room(&previous.pin, &previous.host_secret);
                }
                HostSession::start(client, request, sidecar, tailnet_ip)
            })();
            let _ = tx.send(AppMsg::WorkshopHostStarted { result });
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
            let tailnet_ip = self.multiplayer.tailnet_ip.clone().unwrap_or_default();
            self.multiplayer.identity_update_in_flight = true;
            let tx = tx.clone();
            std::thread::spawn(move || {
                let request = multiplayer_player_identity(token, tailnet_ip);
                let result = RoomClient::new(ROOM_API_BASE_URL).update_player(&pin, &request);
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
            self.multiplayer.detected_map = Some(str_of(&map, "name", &target).to_string());
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
                self.multiplayer.status = format!("Joined session {}.", session.joined.room.pin);
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

    /// Rocket League closed. Rather than tearing the session down
    /// immediately (which would kick everyone out of the room over an
    /// ordinary crash or restart), this arms a grace window; see
    /// tick_shutdown_grace for the part that actually tears things down.
    pub fn shutdown_multiplayer(&mut self) {
        if self.multiplayer.launching_rocket_league {
            // this is Hebnix's own close-then-relaunch, not a real exit
            return;
        }
        let has_session = self.multiplayer.hosted.is_some() || self.multiplayer.joined.is_some();
        if !has_session || self.multiplayer.shutdown_deadline.is_some() {
            return;
        }
        self.multiplayer.shutdown_deadline =
            Some(std::time::Instant::now() + crate::multiplayer_lan::CRASH_GRACE_WINDOW);
        self.multiplayer.status =
            "Rocket League closed. Keeping the session open in case it comes back...".to_string();
    }

    /// call periodically (piggybacked on the existing RL-status poll) to
    /// resolve the crash grace window one way or the other
    pub fn tick_shutdown_grace(&mut self) {
        let Some(deadline) = self.multiplayer.shutdown_deadline else {
            return;
        };
        if hebnix_sdk::process::is_rocket_league_running() {
            self.multiplayer.shutdown_deadline = None;
            self.multiplayer.status = "Rocket League reconnected.".to_string();
            return;
        }
        if std::time::Instant::now() < deadline {
            return;
        }
        self.multiplayer.shutdown_deadline = None;
        let hosted = self.multiplayer.hosted.take();
        let joined = self.multiplayer.joined.take();
        self.clear_host_state();
        self.multiplayer.pending_join = None;
        self.multiplayer.status =
            "Workshop multiplayer stopped because Rocket League closed.".to_string();

        // Stopping a session can wait for a network heartbeat, while firewall
        // cleanup launches external commands. This runs from the egui
        // message handler, so doing any of that here freezes the whole app.
        std::thread::Builder::new()
            .name("workshop-shutdown".into())
            .spawn(move || {
                if let Some(mut session) = hosted {
                    let _ = session.stop();
                }
                if let Some(mut session) = joined {
                    let _ = session.leave();
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
        if let Some(session) = self.multiplayer.hosted.as_mut() {
            session.suspend();
        }
        self.multiplayer.hosted = None;
        if let Some(session) = self.multiplayer.joined.as_mut() {
            session.stop();
        }
        self.multiplayer.joined = None;
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
