//! importing your own workshop map: a small wizard that takes the map file
//! plus metadata (typed in, or read from a steam workshop item .vdf) and an
//! optional banner image, and stores it beside the downloaded cdn maps.
//!
//! imported maps get the id `local_<hash>` (see multiplayer_lan::local_map_id),
//! so the same file has the same id on every machine and a copy received
//! from a peer can be checked against it.

use std::path::{Path, PathBuf};

use eframe::egui;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::multiplayer_lan::{hash_file, local_map_id};

/// first bytes of every unreal engine package
const UPK_MAGIC: [u8; 4] = [0xC1, 0x83, 0x2A, 0x9E];
const MAX_BANNER_BYTES: u64 = 8 * 1024 * 1024;
const LOCAL_BANNER_DIR: &str = "files/maps/local";
const LOCAL_MAPS_FILE: &str = "local_maps.json";

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct LocalMap {
    pub id: String,
    pub name: String,
    pub author: String,
    pub description: String,
    /// cache-relative like the cdn's banner paths ("/files/maps/local/<id>.png"),
    /// empty when the map has no image
    pub banner_path: String,
}

impl LocalMap {
    /// same shape as a catalog entry from the cdn, so cards render it as is
    pub fn to_catalog_entry(&self) -> Value {
        let mut entry = json!({
            "id": self.id,
            "name": self.name,
            "author": self.author,
            "short_description": self.description,
            "local": true,
        });
        if !self.banner_path.is_empty() {
            entry["banner_path"] = json!(self.banner_path);
        }
        entry
    }
}

pub fn load_local_maps(runtime_dir: &Path) -> Vec<LocalMap> {
    std::fs::read_to_string(runtime_dir.join(LOCAL_MAPS_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_local_maps(runtime_dir: &Path, maps: &[LocalMap]) -> Result<(), String> {
    let text = serde_json::to_string_pretty(maps).map_err(|e| e.to_string())?;
    std::fs::write(runtime_dir.join(LOCAL_MAPS_FILE), text).map_err(|e| e.to_string())
}

pub fn is_local_entry(entry: &Value) -> bool {
    entry.get("local").and_then(Value::as_bool).unwrap_or(false)
}

/// what the user typed in on the metadata step
#[derive(Clone, Debug, Default)]
pub struct ImportMeta {
    pub name: String,
    pub author: String,
    pub description: String,
}

/// copies the map (and banner) into the cache and records it. importing the
/// same file again just updates its metadata.
pub fn import_map(
    cache_dir: &Path,
    runtime_dir: &Path,
    map_file: &Path,
    meta: &ImportMeta,
    banner: Option<&Path>,
) -> Result<LocalMap, String> {
    if meta.name.trim().is_empty() {
        return Err("Give the map a name.".to_string());
    }
    let id = local_map_id(&hash_file(map_file).map_err(|e| format!("Could not read the map: {e}"))?);
    let cached = cache_dir.join(format!("{id}.upk"));
    if !cached.exists() {
        std::fs::create_dir_all(cache_dir).map_err(|e| e.to_string())?;
        std::fs::copy(map_file, &cached).map_err(|e| format!("Could not copy the map: {e}"))?;
    }

    let mut maps = load_local_maps(runtime_dir);
    let banner_path = match banner {
        Some(source) => store_banner(cache_dir, &id, source)?,
        // re-importing without a new image keeps the old one
        None => maps
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.banner_path.clone())
            .unwrap_or_default(),
    };
    let entry = LocalMap {
        id,
        name: meta.name.trim().to_string(),
        author: if meta.author.trim().is_empty() {
            "Unknown".to_string()
        } else {
            meta.author.trim().to_string()
        },
        description: meta.description.trim().to_string(),
        banner_path,
    };
    maps.retain(|m| m.id != entry.id);
    maps.push(entry.clone());
    save_local_maps(runtime_dir, &maps)?;
    Ok(entry)
}

/// validates the picture and stores it under the cache with a name that
/// includes a hash of its contents -- a changed image then gets a new
/// path, so egui's image cache never shows the old one
fn store_banner(cache_dir: &Path, id: &str, source: &Path) -> Result<String, String> {
    let size = std::fs::metadata(source).map_err(|e| e.to_string())?.len();
    if size > MAX_BANNER_BYTES {
        return Err("That image is too large (8 MB max).".to_string());
    }
    let bytes = std::fs::read(source).map_err(|e| format!("Could not read the image: {e}"))?;
    let extension = match image::guess_format(&bytes) {
        Ok(image::ImageFormat::Png) => "png",
        Ok(image::ImageFormat::Jpeg) => "jpg",
        Ok(image::ImageFormat::WebP) => "webp",
        Ok(image::ImageFormat::Bmp) => "bmp",
        _ => return Err("The banner must be a PNG, JPG, WebP or BMP image.".to_string()),
    };
    image::load_from_memory(&bytes).map_err(|e| format!("That image could not be decoded: {e}"))?;
    let tag = {
        use sha2::{Digest, Sha256};
        hex::encode(&Sha256::digest(&bytes)[..4])
    };
    let relative = format!("{LOCAL_BANNER_DIR}/{id}_{tag}.{extension}");
    let target = cache_dir.join(&relative);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&target, &bytes).map_err(|e| e.to_string())?;
    Ok(format!("/{relative}"))
}

/// remembers a map received from a peer so it shows up as a card. an
/// existing record (which may have a banner) is left alone.
pub fn record_received_map(runtime_dir: &Path, map: &LocalMap) -> Result<(), String> {
    let mut maps = load_local_maps(runtime_dir);
    if maps.iter().any(|m| m.id == map.id) {
        return Ok(());
    }
    maps.push(map.clone());
    save_local_maps(runtime_dir, &maps)
}

/// deletes an imported map's file, banner and record
pub fn remove_local_map(cache_dir: &Path, runtime_dir: &Path, id: &str) -> Result<(), String> {
    let mut maps = load_local_maps(runtime_dir);
    if let Some(map) = maps.iter().find(|m| m.id == id) {
        if !map.banner_path.is_empty() {
            let _ = std::fs::remove_file(cache_dir.join(map.banner_path.trim_start_matches('/')));
        }
    }
    maps.retain(|m| m.id != id);
    save_local_maps(runtime_dir, &maps)?;
    let _ = std::fs::remove_file(vdf_path(cache_dir, id));
    let file = cache_dir.join(format!("{id}.upk"));
    if file.exists() {
        std::fs::remove_file(file).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// where a map's metadata file is kept, next to its banner
pub fn vdf_path(cache_dir: &Path, id: &str) -> PathBuf {
    cache_dir.join(LOCAL_BANNER_DIR).join(format!("{id}.vdf"))
}

fn vdf_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

/// saves a map's metadata as a steam workshop item .vdf (the same format the
/// import wizard reads back), beside its banner image
pub fn write_item_vdf(
    cache_dir: &Path,
    map: &LocalMap,
    published_file_id: &str,
) -> Result<PathBuf, String> {
    let preview = Path::new(&map.banner_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut text = String::from("\"workshopitem\"\n{\n");
    let mut field = |key: &str, value: &str| {
        if !value.is_empty() {
            text.push_str(&format!("\t\"{key}\"\t\t\"{}\"\n", vdf_escape(value)));
        }
    };
    field("appid", "252950");
    field("publishedfileid", published_file_id);
    field("title", &map.name);
    field("description", &map.description);
    field("previewfile", &preview);
    text.push_str("}\n");
    let path = vdf_path(cache_dir, &map.id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(path)
}

/// true if the file looks like an unreal package. only used for a warning:
/// a cooked map that doesn't start with the magic might still be valid.
pub fn looks_like_package(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .map(|_| magic == UPK_MAGIC)
        .unwrap_or(false)
}

// vdf

/// the fields of a steam workshop item vdf that matter for a map
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VdfItem {
    pub title: String,
    pub description: String,
    pub preview_file: String,
    pub content_folder: String,
    pub published_file_id: String,
}

#[derive(Debug)]
enum Node {
    Value(String),
    Block(Vec<(String, Node)>),
}

#[derive(Debug, PartialEq)]
enum Token {
    Text(String),
    Open,
    Close,
}

fn tokenize(text: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {}
            '/' if chars.peek() == Some(&'/') => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        break;
                    }
                }
            }
            '{' => tokens.push(Token::Open),
            '}' => tokens.push(Token::Close),
            '"' => {
                let mut value = String::new();
                loop {
                    match chars.next() {
                        None => return Err("unterminated quote".to_string()),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('n') => value.push('\n'),
                            Some('t') => value.push('\t'),
                            Some('\\') => value.push('\\'),
                            Some('"') => value.push('"'),
                            // windows paths often have single backslashes,
                            // keep those as they are
                            Some(other) => {
                                value.push('\\');
                                value.push(other);
                            }
                            None => return Err("unterminated quote".to_string()),
                        },
                        Some(other) => value.push(other),
                    }
                }
                tokens.push(Token::Text(value));
            }
            other => {
                // unquoted token, ends at whitespace or a brace
                let mut value = String::from(other);
                while let Some(&next) = chars.peek() {
                    if next.is_whitespace() || next == '{' || next == '}' || next == '"' {
                        break;
                    }
                    value.push(next);
                    chars.next();
                }
                tokens.push(Token::Text(value));
            }
        }
    }
    Ok(tokens)
}

fn parse_block(tokens: &mut std::iter::Peekable<std::vec::IntoIter<Token>>, top: bool)
    -> Result<Vec<(String, Node)>, String>
{
    let mut entries = Vec::new();
    loop {
        match tokens.next() {
            None if top => return Ok(entries),
            None => return Err("missing closing brace".to_string()),
            Some(Token::Close) if !top => return Ok(entries),
            Some(Token::Close) => return Err("unexpected closing brace".to_string()),
            Some(Token::Open) => return Err("unexpected opening brace".to_string()),
            Some(Token::Text(key)) => match tokens.next() {
                Some(Token::Text(value)) => entries.push((key, Node::Value(value))),
                Some(Token::Open) => entries.push((key, Node::Block(parse_block(tokens, false)?))),
                _ => return Err(format!("no value for \"{key}\"")),
            },
        }
    }
}

fn find_value(entries: &[(String, Node)], key: &str) -> String {
    entries
        .iter()
        .find_map(|(k, node)| match node {
            Node::Value(v) if k.eq_ignore_ascii_case(key) => Some(v.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// parses a steam workshop item vdf ("workshopitem" { "title" "..." ... }).
/// a file with the fields at the top level and no wrapping block works too.
pub fn parse_workshop_vdf(text: &str) -> Result<VdfItem, String> {
    let mut tokens = tokenize(text)?.into_iter().peekable();
    let root = parse_block(&mut tokens, true)?;
    let item = root
        .iter()
        .find_map(|(key, node)| match node {
            Node::Block(inner) if key.eq_ignore_ascii_case("workshopitem") => Some(inner),
            _ => None,
        })
        .unwrap_or(&root);
    let parsed = VdfItem {
        title: find_value(item, "title"),
        description: find_value(item, "description"),
        preview_file: find_value(item, "previewfile"),
        content_folder: find_value(item, "contentfolder"),
        published_file_id: find_value(item, "publishedfileid"),
    };
    if parsed == VdfItem::default() {
        return Err("no workshop item fields found in that file".to_string());
    }
    Ok(parsed)
}

/// resolves a path from the vdf, which is usually relative to the vdf itself
fn resolve_beside(vdf: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value.replace('\\', "/"));
    if path.is_absolute() {
        path
    } else {
        vdf.parent().unwrap_or(Path::new(".")).join(path)
    }
}

// wizard

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Step {
    #[default]
    File,
    Details,
}

#[derive(Default)]
pub struct ImportWizard {
    step: Step,
    map_file: Option<PathBuf>,
    meta: ImportMeta,
    banner: Option<PathBuf>,
    notice: String,
    error: String,
    /// last successfully imported map's name, shown once on the first step
    done: Option<String>,
}

impl ImportWizard {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn pick_map_file(&mut self) {
        let dialog = rfd::FileDialog::new().add_filter("Rocket League map", &["upk", "udk"]);
        if let Some(file) = crate::winutil::parent_file_dialog(dialog).pick_file() {
            if self.meta.name.is_empty() {
                self.meta.name = file
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
            }
            self.notice = if looks_like_package(&file) {
                String::new()
            } else {
                "This file doesn't look like an Unreal package. It can still be imported, \
                 but check it's the right file."
                    .to_string()
            };
            self.map_file = Some(file);
            self.error.clear();
            self.done = None;
            self.step = Step::Details;
        }
    }

    fn import_vdf(&mut self) {
        let dialog = rfd::FileDialog::new().add_filter("Steam workshop item", &["vdf", "txt"]);
        let Some(file) = crate::winutil::parent_file_dialog(dialog).pick_file() else {
            return;
        };
        let item = match std::fs::read_to_string(&file)
            .map_err(|e| e.to_string())
            .and_then(|text| parse_workshop_vdf(&text))
        {
            Ok(item) => item,
            Err(error) => {
                self.error = format!("Could not read that VDF: {error}");
                return;
            }
        };
        self.error.clear();
        if !item.title.is_empty() {
            self.meta.name = item.title;
        }
        if !item.description.is_empty() {
            self.meta.description = item.description;
        }
        let mut filled = vec!["title", "description"];
        if !item.preview_file.is_empty() {
            let preview = resolve_beside(&file, &item.preview_file);
            if preview.is_file() {
                self.banner = Some(preview);
                filled.push("image");
            }
        }
        if self.map_file.is_none() && !item.content_folder.is_empty() {
            let folder = resolve_beside(&file, &item.content_folder);
            if let Some(found) = super::find_map_file(&folder) {
                self.map_file = Some(found);
                filled.push("map file");
            }
        }
        self.notice = format!("Filled in from the VDF: {}.", filled.join(", "));
    }

    /// draws the wizard. returns the map once the import succeeded.
    pub fn render(
        &mut self,
        ui: &mut egui::Ui,
        cache_dir: &Path,
        runtime_dir: &Path,
    ) -> Option<LocalMap> {
        let mut imported = None;
        ui.heading("Import a map");
        ui.label(
            "Add a map that isn't on the Workshop. Players on your Workshop network \
             can download it from you.",
        );
        ui.add_space(8.0);
        match self.step {
            Step::File => {
                ui.strong("Step 1: Choose the map file");
                ui.small("A .upk or .udk file.");
                ui.add_space(6.0);
                if let Some(name) = &self.done {
                    ui.colored_label(egui::Color32::LIGHT_GREEN, format!("Imported {name}."));
                    ui.small("Find it under Browse Maps, in View Downloaded.");
                    ui.add_space(6.0);
                }
                if ui.button("Choose map file...").clicked() {
                    self.pick_map_file();
                }
            }
            Step::Details => {
                ui.strong("Step 2: Details");
                if let Some(file) = &self.map_file {
                    ui.small(format!("File: {}", file.display()));
                }
                ui.add_space(6.0);
                if ui.button("Import details from a VDF file...").clicked() {
                    self.import_vdf();
                }
                ui.add_space(6.0);
                egui::Grid::new("import_details").num_columns(2).show(ui, |ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut self.meta.name);
                    ui.end_row();
                    ui.label("Author");
                    ui.text_edit_singleline(&mut self.meta.author);
                    ui.end_row();
                    ui.label("Description");
                    ui.text_edit_multiline(&mut self.meta.description);
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    if ui.button("Choose image...").clicked() {
                        let dialog = rfd::FileDialog::new()
                            .add_filter("Image", &["png", "jpg", "jpeg", "webp", "bmp"]);
                        if let Some(file) = crate::winutil::parent_file_dialog(dialog).pick_file() {
                            self.banner = Some(file);
                        }
                    }
                    match &self.banner {
                        Some(file) => {
                            ui.small(file.display().to_string());
                            if ui.small_button("Clear").clicked() {
                                self.banner = None;
                            }
                        }
                        None => {
                            ui.small("Optional banner shown on the map card.");
                        }
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Back").clicked() {
                        self.reset();
                    }
                    let ready = self.map_file.is_some() && !self.meta.name.trim().is_empty();
                    if ui.add_enabled(ready, egui::Button::new("Import map")).clicked() {
                        if let Some(file) = self.map_file.clone() {
                            match import_map(
                                cache_dir,
                                runtime_dir,
                                &file,
                                &self.meta,
                                self.banner.as_deref(),
                            ) {
                                Ok(map) => {
                                    let name = map.name.clone();
                                    imported = Some(map);
                                    self.reset();
                                    self.done = Some(name);
                                }
                                Err(error) => self.error = error,
                            }
                        }
                    }
                });
            }
        }
        if !self.notice.is_empty() {
            ui.small(&self.notice);
        }
        if !self.error.is_empty() {
            ui.colored_label(egui::Color32::LIGHT_RED, &self.error);
        }
        imported
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hebnix_import_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_a_workshop_item_vdf() {
        let text = r#"
            // uploaded with steamcmd
            "workshopitem"
            {
                "appid"          "252950"
                "publishedfileid" "123456"
                "contentfolder"  "C:\Maps\Cool"
                "previewfile"    "preview.png"
                "title"          "Cool \"Arena\""
                "description"    "line one\nline two"
                "tags"
                {
                    "0" "Map"
                }
            }
        "#;
        let item = parse_workshop_vdf(text).unwrap();
        assert_eq!(item.title, "Cool \"Arena\"");
        assert_eq!(item.description, "line one\nline two");
        assert_eq!(item.preview_file, "preview.png");
        assert_eq!(item.content_folder, "C:\\Maps\\Cool");
        assert_eq!(item.published_file_id, "123456");
    }

    #[test]
    fn vdf_fields_are_case_insensitive_and_can_be_top_level() {
        let item = parse_workshop_vdf("\"Title\" \"Top\"\n\"PreviewFile\" pic.jpg").unwrap();
        assert_eq!(item.title, "Top");
        assert_eq!(item.preview_file, "pic.jpg");
    }

    #[test]
    fn bad_vdf_is_an_error() {
        assert!(parse_workshop_vdf("\"workshopitem\" { \"title\" \"x\"").is_err());
        assert!(parse_workshop_vdf("\"workshopitem\" { \"unrelated\" \"x\" }").is_err());
        assert!(parse_workshop_vdf("").is_err());
    }

    #[test]
    fn a_written_vdf_reads_back_the_same() {
        let dir = temp_dir("vdfwrite");
        let map = LocalMap {
            id: "local_abc".into(),
            name: "Cool \"Arena\" \\ 2".into(),
            author: "Someone".into(),
            description: "line one\nline two\ttabbed".into(),
            banner_path: "/files/maps/local/local_abc_1234.png".into(),
        };
        let path = write_item_vdf(&dir, &map, "2968144588").unwrap();
        assert_eq!(path, vdf_path(&dir, "local_abc"));
        let item = parse_workshop_vdf(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(item.title, map.name);
        assert_eq!(item.description, map.description);
        assert_eq!(item.preview_file, "local_abc_1234.png");
        assert_eq!(item.published_file_id, "2968144588");
    }

    #[test]
    fn vdf_paths_resolve_beside_the_file() {
        let vdf = Path::new("C:/maps/item.vdf");
        assert_eq!(resolve_beside(vdf, "art\\p.png"), PathBuf::from("C:/maps/art/p.png"));
    }

    #[test]
    fn importing_stores_the_map_and_is_repeatable() {
        let dir = temp_dir("store");
        let cache = dir.join("cache");
        let runtime = dir.join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        let source = dir.join("m.upk");
        std::fs::write(&source, b"map bytes").unwrap();

        let meta = ImportMeta {
            name: "  My Map ".into(),
            author: String::new(),
            description: "d".into(),
        };
        let first = import_map(&cache, &runtime, &source, &meta, None).unwrap();
        assert!(first.id.starts_with("local_"));
        assert_eq!(first.name, "My Map");
        assert_eq!(first.author, "Unknown");
        assert!(cache.join(format!("{}.upk", first.id)).exists());

        let renamed = ImportMeta {
            name: "Renamed".into(),
            ..meta
        };
        let second = import_map(&cache, &runtime, &source, &renamed, None).unwrap();
        assert_eq!(second.id, first.id);
        let stored = load_local_maps(&runtime);
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].name, "Renamed");

        remove_local_map(&cache, &runtime, &first.id).unwrap();
        assert!(load_local_maps(&runtime).is_empty());
        assert!(!cache.join(format!("{}.upk", first.id)).exists());
    }

    #[test]
    fn a_map_needs_a_name_and_a_real_image() {
        let dir = temp_dir("checks");
        let source = dir.join("m.upk");
        std::fs::write(&source, b"x").unwrap();
        let unnamed = ImportMeta::default();
        assert!(import_map(&dir, &dir, &source, &unnamed, None).is_err());

        let fake_image = dir.join("fake.png");
        std::fs::write(&fake_image, b"not an image").unwrap();
        let named = ImportMeta {
            name: "n".into(),
            ..Default::default()
        };
        let error = import_map(&dir, &dir, &source, &named, Some(&fake_image)).unwrap_err();
        assert!(error.contains("PNG"), "{error}");
    }
}
