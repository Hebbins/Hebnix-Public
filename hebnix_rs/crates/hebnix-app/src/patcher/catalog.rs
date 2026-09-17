use crate::{messages::AppMsg, ui::workshop::ImageState};
use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const API: &str = "https://api.hebnix.com";
const FILES: &str = "https://hebnix.com";
enum Event {
    Catalog(Result<Vec<Value>, String>),
    Image(String, Option<Vec<u8>>),
    Download(String, Result<PathBuf, String>),
}
pub(crate) struct PatchCatalog {
    kind: &'static str,
    cache: PathBuf,
    items: Vec<Value>,
    images: HashMap<String, ImageState>,
    preview_page: HashMap<String, usize>,
    busy: HashSet<String>,
    page: usize,
    status: String,
    fetched: bool,
    fetching: bool,
    pub view_downloaded: bool,
    tx: Sender<Event>,
    rx: Receiver<Event>,
}
impl PatchCatalog {
    pub fn new(base: &Path, kind: &'static str) -> Self {
        let cache = base
            .join("plugins")
            .join("cache")
            .join("patcher_catalog")
            .join(kind);
        let _ = std::fs::create_dir_all(cache.join("thumbnails"));
        let _ = std::fs::create_dir_all(cache.join("downloads"));
        let items = cached_catalog(&cache).unwrap_or_default();
        let status = if items.is_empty() {
            "Loading catalog..."
        } else {
            "Showing cached catalog."
        }
        .into();
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            kind,
            cache,
            items,
            images: HashMap::new(),
            preview_page: HashMap::new(),
            busy: HashSet::new(),
            page: 0,
            status,
            fetched: false,
            fetching: false,
            view_downloaded: false,
            tx,
            rx,
        }
    }
    pub fn refresh(&mut self, ctx: &egui::Context) {
        if self.fetching {
            return;
        }
        self.fetched = true;
        self.fetching = true;
        let (kind, cache, tx, ctx) = (self.kind, self.cache.clone(), self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = fetch(kind).and_then(|items| {
                save_catalog(&cache, &items)?;
                Ok(items)
            });
            let _ = tx.send(Event::Catalog(result));
            ctx.request_repaint();
        });
    }
    fn poll(&mut self, logs: &Sender<AppMsg>) -> Option<PathBuf> {
        let mut ready = None;
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Catalog(Ok(items)) => {
                    self.fetching = false;
                    self.items = items;
                    self.page = 0;
                    self.status = if self.items.is_empty() {
                        "No catalog items found."
                    } else {
                        ""
                    }
                    .into();
                }
                Event::Catalog(Err(error)) => {
                    self.fetching = false;
                    self.status = if self.items.is_empty() {
                        "Failed to load catalog."
                    } else {
                        "Showing cached catalog."
                    }
                    .into();
                    let _ = logs.send(AppMsg::Log(format!(
                        "[{} Catalog] {error}",
                        title(self.kind)
                    )));
                }
                Event::Image(key, bytes) => {
                    let state = bytes
                        .filter(|b| !b.is_empty())
                        .map(|b| ImageState::Ready(Arc::from(b)))
                        .unwrap_or(ImageState::Failed);
                    self.images.insert(key, state);
                }
                Event::Download(item_id, result) => {
                    self.busy.remove(&item_id);
                    match result {
                        Ok(path) => ready = Some(path),
                        Err(error) => {
                            let _ = logs.send(AppMsg::Log(format!(
                                "[{} Catalog] Download failed: {error}",
                                title(self.kind)
                            )));
                        }
                    }
                }
            }
        }
        ready
    }
    pub fn render(
        &mut self,
        ui: &mut egui::Ui,
        query: &str,
        ctx: &egui::Context,
        logs: &Sender<AppMsg>,
        columns: usize,
    ) -> Option<PathBuf> {
        if !self.fetched {
            self.refresh(ctx);
        }
        let ready = self.poll(logs);
        if self.fetching {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Refreshing catalog...");
            });
        }
        if ui
            .checkbox(&mut self.view_downloaded, "View Downloaded")
            .changed()
        {
            self.page = 0;
        }
        let query = query.trim().to_lowercase();
        let matches: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                let text =
                    format!("{} {}", field(item, "name"), field(item, "author")).to_lowercase();
                text.contains(&query) && (!self.view_downloaded || self.downloaded(&id(item)))
            })
            .map(|(i, _)| i)
            .collect();
        let pages = matches.len().div_ceil(16).max(1);
        self.page = self.page.min(pages - 1);
        ui.horizontal(|ui| {
            ui.label(if matches.is_empty() {
                self.status.clone()
            } else {
                format!("Page {} of {pages}", self.page + 1)
            });
            if ui
                .add_enabled(self.page > 0, egui::Button::new("Previous"))
                .clicked()
            {
                self.page -= 1;
            }
            if ui
                .add_enabled(self.page + 1 < pages, egui::Button::new("Next"))
                .clicked()
            {
                self.page += 1;
            }
        });
        let visible: Vec<_> = matches.into_iter().skip(self.page * 16).take(16).collect();
        for &i in &visible {
            let banners: Vec<String> = catalog_images(self.kind, &self.items[i])
                .into_iter()
                .map(str::to_owned)
                .collect();
            for banner in banners {
                self.image(&banner, ctx);
            }
        }
        let mut action = None;
        egui::ScrollArea::vertical()
            .id_salt(format!("{}_catalog", self.kind))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if visible.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        ui.weak(if self.items.is_empty() {
                            self.status.as_str()
                        } else if self.view_downloaded {
                            "No downloaded items match your search."
                        } else {
                            "No catalog items match your search."
                        });
                    });
                    return;
                }
                let columns = columns.max(1);
                for row in visible.chunks(columns) {
                    ui.columns(columns, |uis| {
                        for (col, &index) in row.iter().enumerate() {
                            if self.card(&mut uis[col], index) {
                                action = Some(id(&self.items[index]));
                            }
                        }
                    });
                    ui.add_space(6.0);
                }
            });
        if let Some(item_id) = action {
            if self.downloaded(&item_id) {
                return Some(self.zip(&item_id));
            }
            self.start_download(item_id, ctx);
        }
        ready
    }
    fn card(&mut self, ui: &mut egui::Ui, index: usize) -> bool {
        let item = &self.items[index];
        let item_id = id(item);
        let previews: Vec<String> = catalog_images(self.kind, item)
            .into_iter()
            .map(str::to_owned)
            .collect();
        let preview_index =
            self.preview_page.get(&item_id).copied().unwrap_or(0) % previews.len().max(1);
        let banner = previews
            .get(preview_index)
            .map(String::as_str)
            .unwrap_or("");
        let has_multiple = previews.len() > 1;
        let is_pack = boolean(item, "is_pack").unwrap_or(self.kind == "boost" && has_multiple);
        let (downloaded, busy) = (self.downloaded(&item_id), self.busy.contains(&item_id));
        let mut clicked = false;
        let mut preview_delta = 0_i8;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_min_height(205.0);
            ui.vertical_centered(|ui| {
                let size = egui::vec2(ui.available_width().min(160.0), 90.0);
                let (image_rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                match self.images.get(banner) {
                    Some(ImageState::Ready(bytes)) => {
                        ui.put(
                            image_rect,
                            egui::Image::from_bytes(
                                format!("bytes://patch-catalog/{}/{banner}", self.kind),
                                bytes.clone(),
                            )
                            .fit_to_exact_size(size),
                        );
                    }
                    Some(ImageState::Failed) => {
                        ui.put(image_rect, egui::Label::new("Failed to load image"));
                    }
                    _ if banner.is_empty() => {
                        ui.put(image_rect, egui::Label::new("No image"));
                    }
                    _ => {
                        ui.put(image_rect, egui::Label::new("Loading image..."));
                    }
                }
                if has_multiple {
                    let arrow_size = egui::vec2(24.0, 28.0);
                    let left = egui::Rect::from_center_size(
                        egui::pos2(image_rect.left() + 16.0, image_rect.center().y),
                        arrow_size,
                    );
                    let right = egui::Rect::from_center_size(
                        egui::pos2(image_rect.right() - 16.0, image_rect.center().y),
                        arrow_size,
                    );
                    let left_response = ui.interact(
                        left,
                        ui.id().with(("preview-left", item_id.as_str())),
                        egui::Sense::click(),
                    );
                    let right_response = ui.interact(
                        right,
                        ui.id().with(("preview-right", item_id.as_str())),
                        egui::Sense::click(),
                    );
                    if left_response.clicked() {
                        preview_delta = -1;
                    }
                    if right_response.clicked() {
                        preview_delta = 1;
                    }
                    let paint_arrow = |rect: egui::Rect, text, hovered| {
                        let fill = if hovered {
                            egui::Color32::from_black_alpha(205)
                        } else {
                            egui::Color32::from_black_alpha(150)
                        };
                        ui.painter().rect_filled(rect, 3.0, fill);
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            text,
                            egui::FontId::proportional(18.0),
                            egui::Color32::WHITE,
                        );
                    };
                    paint_arrow(left, "‹", left_response.hovered());
                    paint_arrow(right, "›", right_response.hovered());
                }
                if is_pack {
                    let badge = egui::Rect::from_min_size(
                        egui::pos2(image_rect.right() - 42.0, image_rect.top() + 4.0),
                        egui::vec2(38.0, 18.0),
                    );
                    ui.painter()
                        .rect_filled(badge, 3.0, egui::Color32::from_rgb(205, 38, 38));
                    ui.painter().text(
                        badge.center(),
                        egui::Align2::CENTER_CENTER,
                        "PACK",
                        egui::FontId::proportional(10.0),
                        egui::Color32::WHITE,
                    );
                }
                ui.strong(short(field(item, "name"), 28));
                ui.label(
                    egui::RichText::new(format!("by {}", field(item, "author")))
                        .italics()
                        .size(11.0)
                        .color(egui::Color32::GRAY),
                );
                ui.weak(format!("{} downloads", number(item, "download_count")));
                let label = if busy {
                    "Downloading..."
                } else if downloaded {
                    "Import"
                } else {
                    "Download"
                };
                clicked = ui
                    .add_enabled(
                        !busy && item_id != "0",
                        egui::Button::new(label).min_size(egui::vec2(ui.available_width(), 24.0)),
                    )
                    .clicked();
            });
        });
        if preview_delta != 0 {
            let next = if preview_delta < 0 {
                (preview_index + previews.len() - 1) % previews.len()
            } else {
                (preview_index + 1) % previews.len()
            };
            self.preview_page.insert(item_id, next);
        }
        clicked
    }
    fn image(&mut self, banner: &str, ctx: &egui::Context) {
        if banner.is_empty() || self.images.contains_key(banner) {
            return;
        }
        self.images.insert(banner.into(), ImageState::Loading);
        let key = banner.to_string();
        let path = self.cache.join("thumbnails").join(format!(
            "{}.img",
            hex::encode(Sha256::digest(key.as_bytes()))
        ));
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let bytes = if path.is_file() {
                std::fs::read(&path).ok()
            } else {
                let result = get(&format!("{FILES}/{}", key.trim_start_matches('/')), 10)
                    .and_then(|response| {
                        let mut b = Vec::new();
                        response
                            .into_reader()
                            .read_to_end(&mut b)
                            .map_err(|e| e.to_string())?;
                        Ok(b)
                    })
                    .ok();
                if let Some(bytes) = &result {
                    let _ = std::fs::write(path, bytes);
                }
                result
            };
            let _ = tx.send(Event::Image(key, bytes));
            ctx.request_repaint();
        });
    }
    fn start_download(&mut self, item_id: String, ctx: &egui::Context) {
        if item_id == "0" || !self.busy.insert(item_id.clone()) {
            return;
        }
        let (kind, path, tx, ctx) = (self.kind, self.zip(&item_id), self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = download(kind, &item_id, &path).map(|_| path);
            let _ = tx.send(Event::Download(item_id, result));
            ctx.request_repaint();
        });
    }
    fn zip(&self, item_id: &str) -> PathBuf {
        self.cache.join("downloads").join(format!("{item_id}.zip"))
    }
    fn downloaded(&self, item_id: &str) -> bool {
        item_id != "0" && self.zip(item_id).is_file()
    }
}
fn fetch(kind: &str) -> Result<Vec<Value>, String> {
    let value: Value = get(&format!("{API}/patches/{kind}"), 10)?
        .into_json()
        .map_err(|e| e.to_string())?;
    match value {
        Value::Array(items) => Ok(items),
        Value::Object(mut o) => Ok(o
            .remove("items")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()),
        _ => Err("Unexpected catalog response.".into()),
    }
}
fn download(kind: &str, item_id: &str, path: &Path) -> Result<(), String> {
    let mut bytes = Vec::new();
    get(&format!("{API}/download/{kind}/{item_id}"), 30)?
        .into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.is_empty() {
        return Err("Downloaded archive was empty.".into());
    }
    let temp = path.with_extension("zip.tmp");
    std::fs::write(&temp, bytes).map_err(|e| e.to_string())?;
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    std::fs::rename(temp, path).map_err(|e| e.to_string())
}
fn get(url: &str, seconds: u64) -> Result<ureq::Response, String> {
    let mut last = String::new();
    for attempt in 0..3 {
        match ureq::get(url).timeout(Duration::from_secs(seconds)).call() {
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
fn cached_catalog(cache: &Path) -> Option<Vec<Value>> {
    serde_json::from_slice(&std::fs::read(cache.join("catalog.json")).ok()?).ok()
}
fn save_catalog(cache: &Path, items: &[Value]) -> Result<(), String> {
    std::fs::write(
        cache.join("catalog.json"),
        serde_json::to_vec(items).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
fn id(item: &Value) -> String {
    let v = match item.get("id") {
        Some(Value::String(v)) => v.clone(),
        Some(Value::Number(v)) => v.to_string(),
        _ => return "0".into(),
    };
    if v.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        v
    } else {
        "0".into()
    }
}
fn field<'a>(item: &'a Value, key: &str) -> &'a str {
    item.get(key).and_then(Value::as_str).unwrap_or("")
}
fn preview_images(item: &Value) -> Vec<&str> {
    item.get("preview_images")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|image| image.get("path").and_then(Value::as_str))
        .filter(|path| !path.is_empty())
        .collect()
}
fn boolean(item: &Value, key: &str) -> Option<bool> {
    match item.get(key) {
        Some(Value::Bool(value)) => Some(*value),
        Some(Value::Number(value)) => value.as_i64().map(|value| value != 0),
        Some(Value::String(value)) => match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    }
}
fn catalog_images<'a>(_kind: &str, item: &'a Value) -> Vec<&'a str> {
    let previews = preview_images(item);
    if !previews.is_empty() {
        return previews;
    }
    vec![field(item, "banner_path")]
}

fn number(item: &Value, key: &str) -> String {
    match item.get(key) {
        Some(Value::String(v)) => v.clone(),
        Some(Value::Number(v)) => v.to_string(),
        _ => "0".into(),
    }
}
fn short(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.into()
    } else {
        format!("{}...", value.chars().take(max - 3).collect::<String>())
    }
}
fn title(value: &str) -> String {
    let mut c = value.chars();
    c.next()
        .map(|x| x.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}
