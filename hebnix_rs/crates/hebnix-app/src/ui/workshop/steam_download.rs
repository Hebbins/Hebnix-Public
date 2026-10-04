//! downloads a rocket league workshop map by its steam workshop id, straight
//! from the client, so a player can grab maps that aren't in the hebnix
//! catalog. same steps as the RLWorkshopCollection map request daemon:
//!
//!  1. steam's public web api says what the item is (and that it really is
//!     a rocket league item -- nothing else is ever downloaded),
//!  2. the hubcap manifest api (needs the player's own api key) hands back
//!     the depot manifest + key for it,
//!  3. DepotDownloaderMod (needs a .net runtime) fetches the files,
//!  4. the biggest non-boilerplate file is the map, it goes through the
//!     same import as a hand-picked map (see local_import.rs).

use crate::i18n::{t, t_args};
use std::io::{BufRead, BufReader, Read};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use eframe::egui;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::local_import::{ImportMeta, LocalMap, import_map, write_item_vdf};

pub const HUBCAP_SITE: &str = "https://hubcapmanifest.com";
const HUBCAP_API: &str = "https://hubcapmanifest.com/api/v1";
const STEAM_DETAILS_URL: &str =
    "https://api.steampowered.com/ISteamRemoteStorage/GetPublishedFileDetails/v1/";
const RL_APPID: &str = "252950";
const SETTINGS_FILE: &str = "steam_download.json";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LOG_LINES: usize = 200;
const MAX_DOWNLOADS: &str = "4";

/// files the rocket league SDK bundles into every workshop map -- never the
/// map itself, however big they are
const BOILERPLATE: [&str; 3] = [
    "maptemplates.upk",
    "editorlandscaperesources.upk",
    "workshopiteminfo.json",
];
const IMAGE_EXTS: [&str; 6] = ["jpg", "jpeg", "jfif", "png", "webp", "bmp"];

#[derive(Default, Deserialize, Serialize)]
struct Settings {
    #[serde(default)]
    hubcap_api_key: String,
}

fn load_settings(runtime_dir: &Path) -> Settings {
    std::fs::read_to_string(runtime_dir.join(SETTINGS_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_settings(runtime_dir: &Path, settings: &Settings) {
    if let Ok(text) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(runtime_dir.join(SETTINGS_FILE), text);
    }
}

// pure helpers

/// a workshop id from a bare number or a steam workshop url
pub fn parse_workshop_id(text: &str) -> Option<String> {
    let text = text.trim();
    if !text.is_empty() && text.len() <= 20 && text.chars().all(|c| c.is_ascii_digit()) {
        return Some(text.to_string());
    }
    let start = text.find("?id=").or_else(|| text.find("&id="))? + 4;
    let digits: String = text[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    (!digits.is_empty() && digits.len() <= 20).then_some(digits)
}

#[derive(Debug, PartialEq, Eq)]
pub struct WorkshopDetails {
    pub title: String,
    pub creator: String,
    pub description: String,
    pub preview_url: String,
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// reads steam's GetPublishedFileDetails reply, refusing anything that isn't
/// a rocket league workshop item
pub fn parse_details(reply: &Value, wid: &str) -> Result<WorkshopDetails, String> {
    let details = reply
        .pointer("/response/publishedfiledetails/0")
        .ok_or("Steam sent back nothing for that id.")?;
    if details.get("result").map(text_of).as_deref() != Some("1") {
        return Err("Steam has no workshop item with that id.".to_string());
    }
    let app = details
        .get("consumer_app_id")
        .map(text_of)
        .unwrap_or_default();
    if app != RL_APPID {
        return Err("That workshop item isn't for Rocket League.".to_string());
    }
    let field = |key: &str| details.get(key).map(text_of).unwrap_or_default();
    let title = field("title");
    Ok(WorkshopDetails {
        title: if title.trim().is_empty() {
            format!("Workshop Item {wid}")
        } else {
            title
        },
        creator: field("creator"),
        description: strip_bbcode(&field("description")),
        preview_url: field("preview_url"),
    })
}

/// steam descriptions use bbcode ([h1], [b], [url=..]); drop the tags
pub fn strip_bbcode(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find(']') {
            Some(close)
                if after[..close]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "=\"'.:/_ -".contains(c))
                    && !after[..close].is_empty() =>
            {
                rest = &after[close + 1..];
            }
            _ => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Debug, PartialEq, Eq)]
pub struct ManifestInfo {
    pub app_id: String,
    pub manifest_id: String,
    pub depot_key: String,
}

/// the manifest api answers with the ids in headers. they're used in file
/// names and a command line, so only plain digits / a hex key are accepted.
pub fn validate_manifest(
    app_id: Option<&str>,
    manifest_id: Option<&str>,
    depot_key: Option<&str>,
) -> Result<ManifestInfo, String> {
    let digits = |v: Option<&str>| {
        v.map(str::trim)
            .filter(|v| !v.is_empty() && v.len() <= 24 && v.chars().all(|c| c.is_ascii_digit()))
            .map(str::to_string)
    };
    let app_id = digits(app_id).ok_or("The manifest reply had no valid app id.")?;
    let manifest_id = digits(manifest_id).ok_or("The manifest reply had no valid manifest id.")?;
    let depot_key = depot_key
        .map(str::trim)
        .filter(|k| !k.is_empty() && k.len() <= 128 && k.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or("The manifest reply had no valid depot key.")?
        .to_string();
    Ok(ManifestInfo {
        app_id,
        manifest_id,
        depot_key,
    })
}

fn extension_of(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn walk_files(dir: &Path, out: &mut Vec<(PathBuf, u64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_files(&path, out);
        } else if let Ok(meta) = entry.metadata() {
            out.push((path, meta.len()));
        }
    }
}

/// the map inside a downloaded workshop item: the biggest file that isn't an
/// image or SDK boilerplate, preferring real .upk/.udk files
pub fn find_map_file(dir: &Path) -> Option<PathBuf> {
    let mut files = Vec::new();
    walk_files(dir, &mut files);
    files.retain(|(path, _)| {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        !IMAGE_EXTS.contains(&extension_of(path).as_str()) && !BOILERPLATE.contains(&name.as_str())
    });
    let biggest = |candidates: Vec<&(PathBuf, u64)>| {
        candidates
            .into_iter()
            .max_by_key(|(_, size)| *size)
            .map(|(path, _)| path.clone())
    };
    let packages: Vec<_> = files
        .iter()
        .filter(|(path, _)| matches!(extension_of(path).as_str(), "upk" | "udk"))
        .collect();
    biggest(packages).or_else(|| biggest(files.iter().collect()))
}

/// a preview image that came inside the download, preferring one named
/// "preview", then the biggest. tiny icons are skipped.
pub fn find_bundled_preview(dir: &Path) -> Option<PathBuf> {
    let mut files = Vec::new();
    walk_files(dir, &mut files);
    files
        .into_iter()
        .filter(|(path, size)| *size > 1024 && IMAGE_EXTS.contains(&extension_of(path).as_str()))
        .max_by_key(|(path, size)| {
            let named = path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().contains("preview"));
            (named, *size)
        })
        .map(|(path, _)| path)
}

// api key

/// the key the player typed in: plain characters only (it goes into an http
/// header, so anything odd is treated as no key at all)
pub fn parse_key(text: &str) -> Option<String> {
    let key = text.trim();
    (!key.is_empty()
        && key.len() <= 200
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)))
    .then(|| key.to_string())
}

// environment

/// DepotDownloaderMod is extracted with the multiplayer bundle after the
/// player connects to Workshop multiplayer.
fn find_depot_downloader() -> Option<PathBuf> {
    let dll = crate::config::base_dir()
        .join("multiplayer-lan")
        .join("depotdownloader")
        .join("DepotDownloaderMod.dll");
    dll.is_file().then_some(dll)
}

/// installs the newest .net runtime through winget
pub const DOTNET_INSTALL_COMMAND: &str = "winget install --id Microsoft.DotNet.Runtime.10 \
     --exact --accept-source-agreements --accept-package-agreements";

/// opens a visible powershell window running `command`; the window stays
/// open afterwards so its output can be read
fn run_in_terminal(command: &str) {
    let _ = Command::new("cmd")
        .args([
            "/C",
            "start",
            "Install .NET",
            "powershell",
            "-NoExit",
            "-Command",
            command,
        ])
        .spawn();
}

/// the newest .net runtime version in `dotnet --list-runtimes` output
/// (lines like "Microsoft.NETCore.App 10.0.8 [C:\Program Files\dotnet\...]")
pub fn highest_runtime_major(list: &str) -> Option<u32> {
    list.lines()
        .filter_map(|line| line.strip_prefix("Microsoft.NETCore.App "))
        .filter_map(|rest| rest.split('.').next()?.trim().parse::<u32>().ok())
        .max()
}

/// the downloader needs a .net 9+ runtime. the runtimes are listed rather
/// than asking `dotnet --version`, which fails when only runtimes (no SDK)
/// are installed.
fn find_dotnet() -> Result<PathBuf, String> {
    let mut candidates = vec![PathBuf::from("dotnet")];
    for var in ["ProgramFiles", "ProgramW6432"] {
        if let Some(root) = std::env::var_os(var) {
            candidates.push(Path::new(&root).join("dotnet").join("dotnet.exe"));
        }
    }
    let mut found_older = None;
    for candidate in candidates {
        let Ok(output) = Command::new(&candidate)
            .arg("--list-runtimes")
            .creation_flags(CREATE_NO_WINDOW)
            .stderr(Stdio::null())
            .output()
        else {
            continue;
        };
        match highest_runtime_major(&String::from_utf8_lossy(&output.stdout)) {
            Some(major) if major >= 9 => return Ok(candidate),
            Some(major) => found_older = Some(major),
            None => {}
        }
    }
    Err(match found_older {
        Some(major) => format!(
            "Only .NET {major} is installed, but the downloader needs .NET 9 or newer. \
             Install the .NET 9 runtime from https://dotnet.microsoft.com/download."
        ),
        None => "The .NET runtime isn't installed. Install .NET 9 (or newer) from \
                 https://dotnet.microsoft.com/download and try again."
            .to_string(),
    })
}

// download

struct Downloaded {
    map_file: PathBuf,
    preview: Option<PathBuf>,
    details: WorkshopDetails,
    /// what the map's own WorkshopItemInfo.json says, if the download had one
    sidecar: Option<ItemInfo>,
}

/// the title/author/description from the WorkshopItemInfo.json the SDK puts
/// in every workshop upload
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ItemInfo {
    pub title: String,
    pub author: String,
    pub description: String,
}

fn json_text(object: &Value, key: &str) -> String {
    object
        .as_object()
        .and_then(|map| map.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)))
        .map(|(_, v)| text_of(v))
        .unwrap_or_default()
}

/// reads a WorkshopItemInfo.json (keys in any letter case); None if it has
/// nothing useful in it
pub fn parse_item_info(text: &str) -> Option<ItemInfo> {
    let json: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let info = ItemInfo {
        title: json_text(&json, "title"),
        author: json_text(&json, "author"),
        description: strip_bbcode(&json_text(&json, "description")),
    };
    (info != ItemInfo::default()).then_some(info)
}

pub(super) fn read_item_info(dir: &Path) -> Option<ItemInfo> {
    let mut files = Vec::new();
    walk_files(dir, &mut files);
    files
        .into_iter()
        .filter(|(path, size)| {
            *size < 1024 * 1024
                && path.file_name().is_some_and(|n| {
                    n.to_string_lossy()
                        .eq_ignore_ascii_case("workshopiteminfo.json")
                })
        })
        .find_map(|(path, _)| parse_item_info(&std::fs::read_to_string(path).ok()?))
}

type Log<'a> = &'a (dyn Fn(&str) + Sync);

fn fetch_details(wid: &str) -> Result<WorkshopDetails, String> {
    let response = ureq::post(STEAM_DETAILS_URL)
        .timeout(Duration::from_secs(15))
        .send_form(&[("itemcount", "1"), ("publishedfileids[0]", wid)])
        .map_err(|e| format!("Steam lookup failed: {e}"))?;
    let mut body = String::new();
    response
        .into_reader()
        .take(2 * 1024 * 1024)
        .read_to_string(&mut body)
        .map_err(|e| e.to_string())?;
    let reply: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    parse_details(&reply, wid)
}

fn fetch_manifest(api_key: &str, wid: &str, dir: &Path) -> Result<(ManifestInfo, PathBuf), String> {
    let response = ureq::get(&format!("{HUBCAP_API}/generate/workshopmanifest/{wid}"))
        .set("Authorization", &format!("Bearer {api_key}"))
        .timeout(Duration::from_secs(30))
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(401 | 403, _) => {
                t("fetch-manifest-the-hubcap-api-key-was-rejected").to_string()
            }
            ureq::Error::Status(code, _) => format!("The manifest request failed (HTTP {code})."),
            other => format!("The manifest request failed: {other}"),
        })?;
    let info = validate_manifest(
        response.header("X-App-Id"),
        response.header("X-Manifest-Id"),
        response.header("X-Depot-Key"),
    )?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_MANIFEST_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}_{}.manifest", info.app_id, info.manifest_id));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok((info, path))
}

fn run_depot_downloader(
    dotnet: &Path,
    dll: &Path,
    info: &ManifestInfo,
    wid: &str,
    manifest: &Path,
    keys: &Path,
    out_dir: &Path,
    log: Log,
) -> Result<(), String> {
    let mut child = Command::new(dotnet)
        .arg(dll)
        .args(["-app", &info.app_id, "-ugc", wid])
        .arg("-manifestfile")
        .arg(manifest)
        .arg("-depotkeys")
        .arg(keys)
        .arg("-dir")
        .arg(out_dir)
        .args(["-max-downloads", MAX_DOWNLOADS])
        // the tool targets .net 9; let a newer installed runtime run it
        .env("DOTNET_ROLL_FORWARD", "Major")
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start the downloader: {e}"))?;
    let stderr = child.stderr.take();
    std::thread::scope(|scope| {
        if let Some(stderr) = stderr {
            scope.spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if !line.trim().is_empty() {
                        log(line.trim());
                    }
                }
            });
        }
        if let Some(stdout) = child.stdout.take() {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    log(line.trim());
                }
            }
        }
    });
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "The downloader failed (exit code {}).",
            status.code().unwrap_or(-1)
        ))
    }
}

fn download_preview(url: &str, dest: &Path) -> bool {
    if !url.starts_with("https://") {
        return false;
    }
    let Ok(response) = ureq::get(url).timeout(Duration::from_secs(20)).call() else {
        return false;
    };
    if !response.content_type().starts_with("image/") {
        return false;
    }
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_IMAGE_BYTES)
        .read_to_end(&mut bytes)
        .is_ok()
        && std::fs::write(dest, bytes).is_ok()
}

fn download_item(
    dotnet: &Path,
    api_key: &str,
    wid: &str,
    work_dir: &Path,
    log: Log,
) -> Result<Downloaded, String> {
    let dll = find_depot_downloader().ok_or(
        "DepotDownloaderMod wasn't found. It should be in a 'depotdownloader' folder \
         next to hebnix.exe.",
    )?;
    log(&t("download-item-looking-up-the-workshop-item-on"));
    let details = fetch_details(wid)?;
    log(&format!("Found \"{}\".", details.title));

    log(&t("download-item-requesting-the-download-manifest"));
    let (info, manifest) = fetch_manifest(api_key, wid, work_dir)?;

    let keys = work_dir.join("depot_keys.txt");
    std::fs::write(&keys, format!("{};{}\n", info.app_id, info.depot_key))
        .map_err(|e| e.to_string())?;

    let out_dir = work_dir.join("content");
    log(&t("download-item-downloading-the-map"));
    run_depot_downloader(&dotnet, &dll, &info, wid, &manifest, &keys, &out_dir, log)?;
    // the key isn't needed past the download
    let _ = std::fs::remove_file(&keys);

    let map_file = find_map_file(&out_dir).ok_or("The download had no map file in it.")?;
    let preview = find_bundled_preview(&out_dir).or_else(|| {
        let fetched = work_dir.join("preview.jpg");
        download_preview(&details.preview_url, &fetched).then_some(fetched)
    });
    let sidecar = read_item_info(&out_dir);
    Ok(Downloaded {
        map_file,
        preview,
        details,
        sidecar,
    })
}

// ui

#[derive(Default)]
struct Shared {
    log: Vec<String>,
    finished: Option<Result<LocalMap, String>>,
    /// the finished map has been handed to the catalog
    delivered: bool,
    /// the last attempt failed because no usable .net runtime was found
    needs_dotnet: bool,
}

pub struct SteamDownloader {
    settings: Option<Settings>,
    input: String,
    show_key: bool,
    busy: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
}

impl Default for SteamDownloader {
    fn default() -> Self {
        Self {
            settings: None,
            input: String::new(),
            show_key: false,
            busy: Arc::new(AtomicBool::new(false)),
            shared: Arc::new(Mutex::new(Shared::default())),
        }
    }
}

impl SteamDownloader {
    fn start(&self, wid: String, cache_dir: PathBuf, runtime_dir: PathBuf, ctx: &egui::Context) {
        let api_key = parse_key(
            self.settings
                .as_ref()
                .map(|s| s.hubcap_api_key.as_str())
                .unwrap_or_default(),
        )
        .unwrap_or_default();
        self.busy.store(true, Ordering::Relaxed);
        if let Ok(mut shared) = self.shared.lock() {
            shared.log.clear();
            shared.finished = None;
            shared.delivered = false;
            shared.needs_dotnet = false;
        }
        let busy = self.busy.clone();
        let shared = self.shared.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let log = {
                let shared = shared.clone();
                let repaint = repaint.clone();
                move |line: &str| {
                    if let Ok(mut shared) = shared.lock() {
                        shared.log.push(line.to_string());
                        if shared.log.len() > MAX_LOG_LINES {
                            shared.log.remove(0);
                        }
                    }
                    repaint.request_repaint();
                }
            };
            let dotnet = match find_dotnet() {
                Ok(path) => path,
                Err(message) => {
                    if let Ok(mut shared) = shared.lock() {
                        shared.finished = Some(Err(message));
                        shared.needs_dotnet = true;
                    }
                    busy.store(false, Ordering::Relaxed);
                    repaint.request_repaint();
                    return;
                }
            };
            let work_dir = std::env::temp_dir().join(format!("hebnix_workshop_{wid}"));
            let _ = std::fs::remove_dir_all(&work_dir);
            let result = std::fs::create_dir_all(&work_dir)
                .map_err(|e| e.to_string())
                .and_then(|_| download_item(&dotnet, &api_key, &wid, &work_dir, &log))
                .and_then(|downloaded| {
                    log(&t("start-saving-the-map-its-details-and"));
                    let sidecar = downloaded.sidecar.as_ref();
                    // the author's own name from the map's info file beats
                    // steam's bare creator id
                    let author = sidecar
                        .map(|s| s.author.clone())
                        .filter(|a| !a.trim().is_empty())
                        .unwrap_or_else(|| {
                            if downloaded.details.creator.is_empty() {
                                String::new()
                            } else {
                                format!("Steam user {}", downloaded.details.creator)
                            }
                        });
                    let description = if downloaded.details.description.is_empty() {
                        sidecar.map(|s| s.description.clone()).unwrap_or_default()
                    } else {
                        downloaded.details.description.clone()
                    };
                    let map = import_map(
                        &cache_dir,
                        &runtime_dir,
                        &downloaded.map_file,
                        &ImportMeta {
                            name: downloaded.details.title.clone(),
                            author,
                            description,
                        },
                        downloaded.preview.as_deref(),
                    )?;
                    // the details also go next to the map as a workshop .vdf
                    write_item_vdf(&cache_dir, &map, &wid)?;
                    Ok(map)
                });
            let _ = std::fs::remove_dir_all(&work_dir);
            if let Ok(mut shared) = shared.lock() {
                shared.finished = Some(result);
            }
            busy.store(false, Ordering::Relaxed);
            repaint.request_repaint();
        });
    }

    /// draws the download section. returns the map once it has been
    /// downloaded and imported.
    pub fn render(
        &mut self,
        ui: &mut egui::Ui,
        cache_dir: &Path,
        runtime_dir: &Path,
    ) -> Option<LocalMap> {
        let settings = self
            .settings
            .get_or_insert_with(|| load_settings(runtime_dir));
        let busy = self.busy.load(Ordering::Relaxed);

        ui.heading(t("render-download-from-the-steam-workshop"));
        ui.label(
            t("render-paste-a-rocket-league-workshop-link"),
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(t("render-hubcap-api-key"));
            let key_edit = egui::TextEdit::singleline(&mut settings.hubcap_api_key)
                .password(!self.show_key)
                .hint_text(t("render-paste-your-key"))
                .desired_width(260.0);
            if ui.add(key_edit).lost_focus() {
                save_settings(runtime_dir, settings);
            }
            ui.checkbox(&mut self.show_key, t("tray-show"));
            ui.hyperlink_to(t("render-get-a-key"), HUBCAP_SITE);
        });
        ui.horizontal(|ui| {
            ui.label(t("render-workshop-link-or-id"));
            ui.add(
                egui::TextEdit::singleline(&mut self.input)
                    .hint_text(t("render-https-steamcommunity-com-sharedfiles-fil"))
                    .desired_width(360.0),
            );
        });

        let wid = parse_workshop_id(&self.input);
        let has_key = parse_key(&settings.hubcap_api_key).is_some();
        let ready = wid.is_some() && has_key && !busy;
        let mut start_with = None;
        ui.horizontal(|ui| {
            if ui.add_enabled(ready, egui::Button::new(t("render-download-map"))).clicked() {
                save_settings(runtime_dir, settings);
                start_with = wid.clone();
            }
            if busy {
                ui.spinner();
            } else if !has_key {
                ui.small(t("render-enter-a-hubcap-api-key-first"));
            } else if wid.is_none() && !self.input.trim().is_empty() {
                ui.small(t("render-that-doesn-t-look-like-a"));
            }
        });

        let mut imported = None;
        if let Ok(shared) = self.shared.lock() {
            match &shared.finished {
                Some(Ok(map)) => {
                    ui.colored_label(
                        egui::Color32::LIGHT_GREEN,
                        t_args("render-imported-map-find-it-under-browse", &[("map", map.name.to_string().into())]),
                    );
                }
                Some(Err(error)) => {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                    if shared.needs_dotnet {
                        ui.small(t("render-to-install-it-with-winget-run"));
                        ui.code(DOTNET_INSTALL_COMMAND);
                        ui.horizontal(|ui| {
                            if ui.button(t("render-run-in-terminal")).clicked() {
                                run_in_terminal(DOTNET_INSTALL_COMMAND);
                            }
                            if ui.button(t("render-copy-command")).clicked() {
                                ui.ctx().copy_text(DOTNET_INSTALL_COMMAND.to_string());
                            }
                            ui.small(t("render-when-it-finishes-click-download-map"));
                        });
                    }
                }
                None => {}
            }
            if !shared.log.is_empty() {
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &shared.log {
                            ui.small(line);
                        }
                    });
            }
        }
        // hand a finished map over exactly once
        if !busy {
            if let Ok(mut shared) = self.shared.lock() {
                if !shared.delivered {
                    if let Some(Ok(map)) = &shared.finished {
                        imported = Some(map.clone());
                        shared.delivered = true;
                    }
                }
            }
        }
        if let Some(wid) = start_with {
            self.start(
                wid,
                cache_dir.to_path_buf(),
                runtime_dir.to_path_buf(),
                ui.ctx(),
            );
        }
        imported
    }
}
