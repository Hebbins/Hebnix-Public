//! workshop maps tab: browse the hebnix.com catalog, download + swap maps
//! over the rocket labs placeholders.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::Sender;
use eframe::egui;
use serde_json::Value;

use crate::messages::AppMsg;

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

pub const WORKSHOP_PLUGIN_ID: &str = "workshop_map_loader";
pub const WORKSHOP_MODS_DIR_NAME: &str = "mods";
pub const REMOTE_FILES_BASE: &str = "https://hebnix.com";
pub const API_ENDPOINT: &str = "https://api.hebnix.com/maps";
pub const DOWNLOAD_ENDPOINT_BASE: &str = "https://api.hebnix.com/download/map/";

pub const TARGET_MAPS: [(&str, &str); 3] = [
    ("Utopia Retro", "Labs_Utopia_P.upk"),
    ("Underpass", "Labs_Underpass_P.upk"),
    ("Roadblock", "Labs_Octagon_B2B_02_P.upk"),
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
        let maps = std::fs::read_to_string(Self::install_state_path(rl_path))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        *self.active_maps.lock().unwrap() = maps;
        self.save_active_maps();
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
}

impl WorkshopState {
    pub fn new(base_dir: &Path) -> Self {
        Self {
            manager: MapManager::new(base_dir),
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
