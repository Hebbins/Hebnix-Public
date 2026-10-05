//! importing your own workshop map: a small wizard that takes the map file
//! plus metadata (typed in, or read from a steam workshop item .vdf) and an
//! optional banner image, and stores it beside the downloaded cdn maps.
//!
//! imported maps get the id `local_<hash>` (see multiplayer_lan::local_map_id),
//! so the same file has the same id on every machine and a copy received
//! from a peer can be checked against it.

use crate::i18n::{t, t_args};
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
    let id =
        local_map_id(&hash_file(map_file).map_err(|e| format!("Could not read the map: {e}"))?);
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

/// true if the picture is one `import_map` can store as a banner. Used to
/// drop an odd image from a zip or download instead of failing the import.
pub fn usable_banner(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() <= MAX_BANNER_BYTES)
        && std::fs::read(path).is_ok_and(|bytes| {
            matches!(
                image::guess_format(&bytes),
                Ok(image::ImageFormat::Png
                    | image::ImageFormat::Jpeg
                    | image::ImageFormat::WebP
                    | image::ImageFormat::Bmp)
            ) && image::load_from_memory(&bytes).is_ok()
        })
}

// zip

/// files worth unpacking from a map zip; anything else is skipped
const ZIP_KEEP_EXTS: [&str; 11] = [
    "upk", "udk", "vdf", "json", "png", "jpg", "jpeg", "jfif", "webp", "bmp", "gif",
];

/// what a map zip turned out to hold, unpacked into a folder
#[derive(Debug, Default)]
pub struct ZipContents {
    pub map_file: PathBuf,
    pub meta: ImportMeta,
    pub banner: Option<PathBuf>,
    /// the steam workshop id, when the zip's .vdf has one
    pub published_file_id: String,
    /// entries left out: unrelated files, or ones that couldn't be read
    pub skipped: Vec<String>,
}

/// unpacks a map zip (map file, preview image, .vdf, maybe a
/// WorkshopItemInfo.json) into `dest` and works out the details. Unrelated
/// or unreadable entries are skipped, not treated as an error; only a zip
/// without any .upk/.udk map fails.
pub fn extract_map_zip(zip_path: &Path, dest: &Path) -> Result<ZipContents, String> {
    use std::io::Read;
    let max_entry = crate::multiplayer_lan::MAX_MAP_BYTES;
    let file = std::fs::File::open(zip_path).map_err(|e| format!("Could not open the zip: {e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("That isn't a readable zip file: {e}"))?;
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;

    let mut skipped = Vec::new();
    for index in 0..archive.len() {
        let mut entry = match archive.by_index(index) {
            Ok(entry) => entry,
            Err(error) => {
                skipped.push(format!("entry {index} ({error})"));
                continue;
            }
        };
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        // enclosed_name refuses absolute paths and "..", so nothing lands
        // outside dest
        let Some(relative) = entry.enclosed_name() else {
            skipped.push(name);
            continue;
        };
        let extension = relative
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if !ZIP_KEEP_EXTS.contains(&extension.as_str()) || entry.size() > max_entry {
            skipped.push(name);
            continue;
        }
        let target = dest.join(relative);
        let written = target
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::File::create(&target))
            .and_then(|mut out| std::io::copy(&mut (&mut entry).take(max_entry), &mut out));
        if written.is_err() {
            let _ = std::fs::remove_file(&target);
            skipped.push(name);
        }
    }

    let map_file = super::steam_download::find_map_file(dest)
        .filter(|path| {
            path.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("upk") || e.eq_ignore_ascii_case("udk"))
        })
        .ok_or("The zip has no .upk or .udk map file in it.")?;

    let mut files = Vec::new();
    collect_files(dest, &mut files);
    let vdf = files
        .iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("vdf"))
        })
        .find_map(|path| {
            let item = parse_workshop_vdf(&std::fs::read_to_string(path).ok()?).ok()?;
            Some((path.clone(), item))
        });
    let info = super::steam_download::read_item_info(dest).or_else(|| {
        files
            .iter()
            .filter(|path| {
                path.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("json"))
            })
            .find_map(|path| {
                super::steam_download::parse_item_info(&std::fs::read_to_string(path).ok()?)
            })
    });

    let pick = |from_vdf: Option<&str>, from_info: Option<&str>| {
        from_vdf
            .filter(|v| !v.trim().is_empty())
            .or(from_info.filter(|v| !v.trim().is_empty()))
            .unwrap_or_default()
            .to_string()
    };
    let item = vdf.as_ref().map(|(_, item)| item);
    let mut name = pick(
        item.map(|i| i.title.as_str()),
        info.as_ref().map(|i| i.title.as_str()),
    );
    if name.is_empty() {
        name = map_file
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
    }
    let description = super::steam_download::strip_bbcode(&pick(
        item.map(|i| i.description.as_str()),
        info.as_ref().map(|i| i.description.as_str()),
    ));
    let author = info.as_ref().map(|i| i.author.clone()).unwrap_or_default();

    let vdf_preview = vdf.as_ref().and_then(|(path, item)| {
        (!item.preview_file.is_empty())
            .then(|| resolve_beside(path, &item.preview_file))
            .filter(|p| p.is_file())
    });
    let banner = vdf_preview
        .or_else(|| super::steam_download::find_bundled_preview(dest))
        .filter(|p| usable_banner(p));

    Ok(ZipContents {
        map_file,
        meta: ImportMeta {
            name,
            author,
            description,
        },
        banner,
        published_file_id: item
            .map(|i| i.published_file_id.clone())
            .unwrap_or_default(),
        skipped,
    })
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
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

fn parse_block(
    tokens: &mut std::iter::Peekable<std::vec::IntoIter<Token>>,
    top: bool,
) -> Result<Vec<(String, Node)>, String> {
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
    /// where a picked zip was unpacked, removed again on reset
    zip_dir: Option<PathBuf>,
    /// steam workshop id from a zip's .vdf, saved with the map
    published_file_id: String,
}

impl ImportWizard {
    fn reset(&mut self) {
        if let Some(dir) = self.zip_dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
        *self = Self::default();
    }

    fn load_zip(&mut self, zip: &Path) {
        self.reset();
        let dir = std::env::temp_dir().join(format!("hebnix_zip_import_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        self.zip_dir = Some(dir.clone());
        match extract_map_zip(zip, &dir) {
            Ok(contents) => {
                let mut notice = format!(
                    "Filled in from the zip{}.",
                    if contents.banner.is_some() {
                        ", including the image"
                    } else {
                        ""
                    }
                );
                if !contents.skipped.is_empty() {
                    notice.push_str(&format!(
                        " Skipped {} unrelated file(s): {}.",
                        contents.skipped.len(),
                        contents.skipped.join(", ")
                    ));
                }
                if !looks_like_package(&contents.map_file) {
                    notice.push_str(" The map file doesn't look like an Unreal package, check it's the right one.");
                }
                self.map_file = Some(contents.map_file);
                self.meta = contents.meta;
                self.banner = contents.banner;
                self.published_file_id = contents.published_file_id;
                self.notice = notice;
                self.step = Step::Details;
            }
            Err(error) => {
                self.reset();
                self.error = error;
            }
        }
    }

    fn pick_map_file(&mut self) {
        let dialog = rfd::FileDialog::new().add_filter(
            t("pick-map-file-rocket-league-map-or-map-zip"),
            &["upk", "udk", "zip"],
        );
        if let Some(file) = crate::winutil::parent_file_dialog(dialog).pick_file() {
            if file
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
            {
                self.load_zip(&file);
                return;
            }
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
        let dialog =
            rfd::FileDialog::new().add_filter(t("import-vdf-steam-workshop-item"), &["vdf", "txt"]);
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
        ui.heading(t("render-import-a-map"));
        ui.label(t("render-add-a-map-that-isn-t"));
        ui.add_space(8.0);
        match self.step {
            Step::File => {
                ui.strong(t("render-step-1-choose-the-map-file"));
                ui.small(t("render-a-upk-or-udk-file-or"));
                ui.add_space(6.0);
                if let Some(name) = &self.done {
                    ui.colored_label(
                        egui::Color32::LIGHT_GREEN,
                        t_args("render-imported-name", &[("name", name.to_string().into())]),
                    );
                    ui.small(t("render-find-it-under-browse-maps-in"));
                    ui.add_space(6.0);
                }
                if ui.button(t("render-choose-map-file")).clicked() {
                    self.pick_map_file();
                }
            }
            Step::Details => {
                ui.strong(t("render-step-2-details"));
                if let Some(file) = &self.map_file {
                    ui.small(t_args(
                        "render-file-file",
                        &[("file", (file.display()).to_string().into())],
                    ));
                }
                ui.add_space(6.0);
                if ui
                    .button(t("render-import-details-from-a-vdf-file"))
                    .clicked()
                {
                    self.import_vdf();
                }
                ui.add_space(6.0);
                egui::Grid::new("import_details")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label(t("render-name"));
                        ui.text_edit_singleline(&mut self.meta.name);
                        ui.end_row();
                        ui.label(t("render-author"));
                        ui.text_edit_singleline(&mut self.meta.author);
                        ui.end_row();
                        ui.label(t("render-description"));
                        ui.text_edit_multiline(&mut self.meta.description);
                        ui.end_row();
                    });
                ui.horizontal(|ui| {
                    if ui.button(t("render-choose-image")).clicked() {
                        let dialog = rfd::FileDialog::new()
                            .add_filter(t("render-image"), &["png", "jpg", "jpeg", "webp", "bmp"]);
                        if let Some(file) = crate::winutil::parent_file_dialog(dialog).pick_file() {
                            self.banner = Some(file);
                        }
                    }
                    match &self.banner {
                        Some(file) => {
                            ui.small(file.display().to_string());
                            if ui.small_button(t("spoofer-clear")).clicked() {
                                self.banner = None;
                            }
                        }
                        None => {
                            ui.small(t("render-optional-banner-shown-on-the-map"));
                        }
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(t("multiplayer-back")).clicked() {
                        self.reset();
                    }
                    let ready = self.map_file.is_some() && !self.meta.name.trim().is_empty();
                    if ui
                        .add_enabled(ready, egui::Button::new(t("render-import-map-2")))
                        .clicked()
                    {
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
                                    if !self.published_file_id.is_empty() {
                                        let _ = write_item_vdf(
                                            cache_dir,
                                            &map,
                                            &self.published_file_id,
                                        );
                                    }
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
