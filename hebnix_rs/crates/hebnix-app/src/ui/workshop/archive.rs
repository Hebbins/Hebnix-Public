//! the RL Workshop Archive (xplodingeggo.github.io/RLWorkshopCollection): a
//! public list of workshop maps with direct downloads, for players without a
//! Hubcap API key. Anyone can request a Steam Workshop map on the site and it
//! is added within minutes. This lists the archive's maps.json and downloads
//! and imports a map the same way a hand-picked file or zip is imported.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use eframe::egui;
use serde::Deserialize;
use serde_json::Value;

use super::local_import::{
    ImportMeta, LocalMap, extract_map_zip, import_map, usable_banner, write_item_vdf,
};
use super::steam_download::parse_workshop_id;

pub const ARCHIVE_SITE: &str = "https://xplodingeggo.github.io/RLWorkshopCollection/";
const ARCHIVE_INDEX: &str = "https://xplodingeggo.github.io/RLWorkshopCollection/maps.json";
const MAX_INDEX_BYTES: u64 = 4 * 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const ZIP_MAGIC: [u8; 4] = *b"PK\x03\x04";

/// one entry of the archive's maps.json (keys as the site writes them)
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ArchiveMap {
    #[serde(rename = "Title", default)]
    pub title: String,
    #[serde(rename = "Author", default)]
    pub author: String,
    #[serde(rename = "Description", default)]
    pub description: String,
    #[serde(rename = "PreviewUrl", default)]
    pub preview_url: String,
    #[serde(rename = "downloadUrl", default)]
    pub download_url: String,
    #[serde(rename = "steamUrl", default)]
    pub steam_url: String,
    /// a single tag or a list of them
    #[serde(default)]
    pub category: Value,
}

impl ArchiveMap {
    fn categories(&self) -> Vec<String> {
        match &self.category {
            Value::String(tag) if !tag.is_empty() => vec![tag.clone()],
            Value::Array(tags) => tags
                .iter()
                .filter_map(Value::as_str)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        }
    }

    fn matches(&self, search: &str) -> bool {
        let search = search.trim().to_lowercase();
        search.is_empty()
            || self.title.to_lowercase().contains(&search)
            || self.author.to_lowercase().contains(&search)
            || self
                .categories()
                .iter()
                .any(|c| c.to_lowercase().contains(&search))
    }
}

/// parses the archive's maps.json, dropping entries with nothing to download
pub fn parse_index(text: &str) -> Result<Vec<ArchiveMap>, String> {
    let maps: Vec<ArchiveMap> = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("The archive's map list couldn't be read: {e}"))?;
    Ok(maps
        .into_iter()
        .filter(|m| m.download_url.starts_with("https://"))
        .collect())
}

fn fetch_index() -> Result<Vec<ArchiveMap>, String> {
    let response = ureq::get(ARCHIVE_INDEX)
        .timeout(Duration::from_secs(20))
        .call()
        .map_err(|e| format!("Couldn't reach the RL Workshop Archive: {e}"))?;
    let mut text = String::new();
    response
        .into_reader()
        .take(MAX_INDEX_BYTES)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    parse_index(&text)
}

/// streams `url` into `dest`, refusing anything larger than `limit`
fn download_to(url: &str, dest: &Path, limit: u64) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("Only https downloads are allowed.".to_string());
    }
    let response = ureq::get(url)
        .timeout(Duration::from_secs(600))
        .call()
        .map_err(|e| format!("Download failed: {e}"))?;
    let mut file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let copied = std::io::copy(&mut response.into_reader().take(limit + 1), &mut file)
        .map_err(|e| format!("Download failed: {e}"))?;
    if copied > limit {
        return Err("That download is too large to be a map.".to_string());
    }
    Ok(())
}

fn is_zip(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok_and(|_| magic == ZIP_MAGIC)
}

fn file_name_from_url(url: &str) -> String {
    url.split(['?', '#'])
        .next()
        .and_then(|path| path.rsplit('/').next())
        .filter(|name| !name.is_empty())
        .unwrap_or("map.upk")
        .to_string()
}

/// downloads one archive map (a bare .upk/.udk or a zip) and imports it.
/// The archive's own details win over whatever is inside a zip.
fn download_and_import(
    map: &ArchiveMap,
    cache_dir: &Path,
    runtime_dir: &Path,
    work_dir: &Path,
    log: &dyn Fn(&str),
) -> Result<LocalMap, String> {
    log(&format!("Downloading {}...", map.title));
    let download = work_dir.join(file_name_from_url(&map.download_url));
    download_to(
        &map.download_url,
        &download,
        crate::multiplayer_lan::MAX_MAP_BYTES,
    )?;

    let mut meta = ImportMeta {
        name: map.title.clone(),
        author: map.author.clone(),
        description: map.description.clone(),
    };
    let mut banner = None;
    let mut published_file_id = parse_workshop_id(&map.steam_url).unwrap_or_default();
    let map_file = if is_zip(&download) {
        log("Unpacking the zip...");
        let contents = extract_map_zip(&download, &work_dir.join("zip"))?;
        if !contents.skipped.is_empty() {
            log(&format!(
                "Skipped {} unrelated file(s) in the zip.",
                contents.skipped.len()
            ));
        }
        if meta.name.trim().is_empty() {
            meta.name = contents.meta.name;
        }
        if meta.author.trim().is_empty() {
            meta.author = contents.meta.author;
        }
        if meta.description.trim().is_empty() {
            meta.description = contents.meta.description;
        }
        if published_file_id.is_empty() {
            published_file_id = contents.published_file_id;
        }
        banner = contents.banner;
        contents.map_file
    } else {
        download
    };
    if meta.name.trim().is_empty() {
        meta.name = map_file
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
    }

    if !map.preview_url.is_empty() {
        log("Downloading the preview image...");
        let preview = work_dir.join("preview");
        // an image that won't download or decode just means no banner
        if download_to(&map.preview_url, &preview, MAX_IMAGE_BYTES).is_ok()
            && usable_banner(&preview)
        {
            banner = Some(preview);
        }
    }

    log("Saving the map...");
    let imported = import_map(cache_dir, runtime_dir, &map_file, &meta, banner.as_deref())?;
    if !published_file_id.is_empty() {
        let _ = write_item_vdf(cache_dir, &imported, &published_file_id);
    }
    Ok(imported)
}

#[derive(Default)]
struct Shared {
    index: Option<Result<Vec<ArchiveMap>, String>>,
    status: String,
    finished: Option<Result<LocalMap, String>>,
    delivered: bool,
}

#[derive(Default)]
pub struct ArchiveBrowser {
    search: String,
    /// the map list is shown; hiding it keeps the loaded list for later
    show_list: bool,
    loading: Arc<AtomicBool>,
    downloading: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
}

impl ArchiveBrowser {
    fn load(&self, ctx: &egui::Context) {
        if self.loading.swap(true, Ordering::Relaxed) {
            return;
        }
        let loading = self.loading.clone();
        let shared = self.shared.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let index = fetch_index();
            if let Ok(mut shared) = shared.lock() {
                shared.index = Some(index);
            }
            loading.store(false, Ordering::Relaxed);
            repaint.request_repaint();
        });
    }

    fn start(
        &self,
        map: ArchiveMap,
        cache_dir: PathBuf,
        runtime_dir: PathBuf,
        ctx: &egui::Context,
    ) {
        if self.downloading.swap(true, Ordering::Relaxed) {
            return;
        }
        if let Ok(mut shared) = self.shared.lock() {
            shared.finished = None;
            shared.delivered = false;
            shared.status.clear();
        }
        let downloading = self.downloading.clone();
        let shared = self.shared.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let log = |line: &str| {
                if let Ok(mut shared) = shared.lock() {
                    shared.status = line.to_string();
                }
                repaint.request_repaint();
            };
            let work_dir =
                std::env::temp_dir().join(format!("hebnix_archive_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&work_dir);
            let result = std::fs::create_dir_all(&work_dir)
                .map_err(|e| e.to_string())
                .and_then(|_| download_and_import(&map, &cache_dir, &runtime_dir, &work_dir, &log));
            let _ = std::fs::remove_dir_all(&work_dir);
            if let Ok(mut shared) = shared.lock() {
                shared.status.clear();
                shared.finished = Some(result);
            }
            downloading.store(false, Ordering::Relaxed);
            repaint.request_repaint();
        });
    }

    /// draws the archive section. returns a map once it has been downloaded
    /// and imported.
    pub fn render(
        &mut self,
        ui: &mut egui::Ui,
        cache_dir: &Path,
        runtime_dir: &Path,
    ) -> Option<LocalMap> {
        let loading = self.loading.load(Ordering::Relaxed);
        let downloading = self.downloading.load(Ordering::Relaxed);

        ui.heading("No API key? Use the RL Workshop Archive");
        ui.label(
            "If you can't get a Hubcap API key (or can't be bothered), request the map on the \
             RL Workshop Archive instead. Paste its Steam Workshop link there and it's usually \
             added within 5-10 minutes. Then download it below, or download the .zip from the \
             site and import it with \"Choose map file...\" above.",
        );
        ui.hyperlink_to("Open the RL Workshop Archive (request a map)", ARCHIVE_SITE);
        ui.add_space(6.0);

        let mut download = None;
        let mut imported = None;
        let loaded = self.shared.lock().is_ok_and(|s| s.index.is_some());
        ui.horizontal(|ui| {
            if !self.show_list {
                if ui
                    .add_enabled(!loading, egui::Button::new("Show archive maps"))
                    .clicked()
                {
                    self.show_list = true;
                    if !loaded {
                        self.load(ui.ctx());
                    }
                }
            } else {
                if ui.button("Hide list").clicked() {
                    self.show_list = false;
                }
                if ui
                    .add_enabled(!loading, egui::Button::new("Refresh list"))
                    .clicked()
                {
                    self.load(ui.ctx());
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Search name, author or tag...")
                        .desired_width(220.0),
                );
            }
            if loading {
                ui.spinner();
            }
        });

        if let Ok(mut shared) = self.shared.lock() {
            match &shared.index {
                Some(Ok(_)) if !self.show_list => {}
                Some(Ok(maps)) => {
                    let shown: Vec<&ArchiveMap> =
                        maps.iter().filter(|m| m.matches(&self.search)).collect();
                    ui.small(format!("{} of {} maps", shown.len(), maps.len()));
                    egui::ScrollArea::vertical()
                        .id_salt("archive_maps")
                        .max_height(260.0)
                        .show(ui, |ui| {
                            for map in shown {
                                ui.horizontal(|ui| {
                                    if ui
                                        .add_enabled(!downloading, egui::Button::new("Download"))
                                        .clicked()
                                    {
                                        download = Some(map.clone());
                                    }
                                    ui.strong(&map.title);
                                    if !map.author.is_empty() {
                                        ui.small(format!("by {}", map.author));
                                    }
                                    let tags = map.categories();
                                    if !tags.is_empty() {
                                        ui.small(format!("[{}]", tags.join(", ")));
                                    }
                                });
                            }
                        });
                }
                Some(Err(error)) => {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                }
                None => {}
            }
            if downloading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.small(&shared.status);
                });
            }
            match &shared.finished {
                Some(Ok(map)) => {
                    ui.colored_label(
                        egui::Color32::LIGHT_GREEN,
                        format!(
                            "Imported {}. Find it under Browse Maps, in View Downloaded.",
                            map.name
                        ),
                    );
                    if !downloading && !shared.delivered {
                        imported = Some(map.clone());
                        shared.delivered = true;
                    }
                }
                Some(Err(error)) => {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                }
                None => {}
            }
        }

        if let Some(map) = download {
            self.start(
                map,
                cache_dir.to_path_buf(),
                runtime_dir.to_path_buf(),
                ui.ctx(),
            );
        }
        imported
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_archive_index() {
        let maps = parse_index(
            r#"[
              {"Title": "Aim training", "Author": "CoCo", "Description": "Shoot",
               "category": "training", "PreviewUrl": "https://files.example/a.jfif",
               "downloadUrl": "https://files.example/a.upk",
               "steamUrl": "https://steamcommunity.com/sharedfiles/filedetails/?id=1906378036",
               "format": "upk"},
              {"Title": "Bees", "category": ["fun", "other"], "downloadUrl": "https://files.example/b.zip"},
              {"Title": "No download", "downloadUrl": ""}
            ]"#,
        )
        .unwrap();
        assert_eq!(maps.len(), 2);
        assert_eq!(maps[0].categories(), vec!["training"]);
        assert_eq!(maps[1].categories(), vec!["fun", "other"]);
        assert!(maps[1].matches("OTHER"));
        assert!(!maps[0].matches("bees"));
        assert_eq!(
            parse_workshop_id(&maps[0].steam_url).as_deref(),
            Some("1906378036")
        );
    }

    /// hits the real archive: cargo test -- --ignored archive_live
    #[test]
    #[ignore]
    fn archive_live_download() {
        let maps = fetch_index().unwrap();
        let map = maps
            .iter()
            .find(|m| m.title.contains("Aim"))
            .unwrap_or(&maps[0])
            .clone();
        let root = std::env::temp_dir().join(format!("hebnix_archive_live_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (cache, runtime, work) = (root.join("cache"), root.join("runtime"), root.join("work"));
        for dir in [&cache, &runtime, &work] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let imported =
            download_and_import(&map, &cache, &runtime, &work, &|line| println!("{line}")).unwrap();
        println!("{imported:?}");
        assert!(cache.join(format!("{}.upk", imported.id)).is_file());
        assert!(
            !imported.banner_path.is_empty(),
            "preview should have been stored"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn names_the_download_after_the_url() {
        assert_eq!(
            file_name_from_url("https://a.example/x/Map.udk?v=2"),
            "Map.udk"
        );
        assert_eq!(file_name_from_url("https://a.example/"), "map.upk");
    }
}
