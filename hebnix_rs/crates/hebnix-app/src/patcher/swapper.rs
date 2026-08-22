use crate::messages::AppMsg;
use crossbeam_channel::Sender;
use eframe::egui;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SwapCategory {
    Antennas,
    Anthems,
    Borders,
    Bodies,
    Boosts,
    Skins,
    Engines,
    Goals,
    Finishes,
    Banners,
    Toppers,
    Trails,
    Wheels,
}

impl SwapCategory {
    pub const ALL: [Self; 13] = [
        Self::Antennas,
        Self::Anthems,
        Self::Borders,
        Self::Bodies,
        Self::Boosts,
        Self::Skins,
        Self::Engines,
        Self::Goals,
        Self::Finishes,
        Self::Banners,
        Self::Toppers,
        Self::Trails,
        Self::Wheels,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Antennas => "Antennas",
            Self::Anthems => "Anthems",
            Self::Borders => "Borders",
            Self::Bodies => "Bodies",
            Self::Boosts => "Boosts",
            Self::Engines => "Engines",
            Self::Goals => "Goals",
            Self::Finishes => "Finishes",
            Self::Banners => "Banners",
            Self::Skins => "Decals",
            Self::Toppers => "Toppers",
            Self::Trails => "Trails",
            Self::Wheels => "Wheels",
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Self::Antennas => "antennas",
            Self::Anthems => "anthems",
            Self::Borders => "borders",
            Self::Bodies => "bodies",
            Self::Boosts => "boosts",
            Self::Engines => "engines",
            Self::Goals => "goals",
            Self::Finishes => "finishes",
            Self::Banners => "banners",
            Self::Skins => "skins",
            Self::Toppers => "toppers",
            Self::Trails => "trails",
            Self::Wheels => "wheels",
        }
    }
}

#[derive(Clone)]
struct SwapItem {
    name: String,
    upk: String,
    path: Option<String>,
    thumbnail: Option<String>,
    audio_bnk: Option<String>,
    upk_type: String,
    product_id: Option<i64>,
    car_key: Option<String>,
    car_name: Option<String>,
    car_product_id: Option<i64>,
}

#[derive(Clone, Serialize, Deserialize)]
struct ActiveSwap {
    category: String,
    source_name: String,
    source_upk: String,
    target_name: String,
    target_upk: String,
    #[serde(default)]
    target_bnk: Option<String>,
    #[serde(default)]
    target_thumbnail: Option<String>,
}

fn patch_boost_bnk(source: &Path, target_backup: &Path, destination: &Path) -> Result<(), String> {
    let mut donor = fs::read(source).map_err(|error| format!("{}: {error}", source.display()))?;
    let target =
        fs::read(target_backup).map_err(|error| format!("{}: {error}", target_backup.display()))?;
    if donor.len() < 16 || target.len() < 16 || &donor[..4] != b"BKHD" || &target[..4] != b"BKHD" {
        return Err("Unsupported or truncated Wwise boost audio bank".into());
    }
    donor[12..16].copy_from_slice(&target[12..16]);
    let temporary = destination.with_extension("bnk.swapping.tmp");
    fs::write(&temporary, donor)
        .map_err(|error| format!("Failed to write {}: {error}", temporary.display()))?;
    fs::copy(&temporary, destination)
        .map_err(|error| format!("Failed to install {}: {error}", destination.display()))?;
    let _ = fs::remove_file(temporary);
    Ok(())
}

fn swap_compatible(category: SwapCategory, source: &SwapItem, target: &SwapItem) -> bool {
    if category != SwapCategory::Goals {
        return true;
    }
    match target.upk_type.as_str() {
        "2parts" => source.upk_type == "2parts",
        "3parts" => source.upk_type == "3parts" && source.upk.eq_ignore_ascii_case(&target.upk),
        _ => matches!(source.upk_type.as_str(), "simple" | "2parts"),
    }
}

fn inferred_thumbnail(category: SwapCategory, item: &SwapItem, cooked_pc: &Path) -> Option<String> {
    if let Some(name) = item.thumbnail.as_ref().filter(|name| !name.is_empty()) {
        return cooked_pc.join(name).is_file().then(|| name.clone());
    }
    if matches!(category, SwapCategory::Boosts | SwapCategory::Goals)
        && (category != SwapCategory::Goals || item.upk_type == "simple")
    {
        let lower = item.upk.to_ascii_lowercase();
        if lower.ends_with("_sf.upk") {
            let candidate = format!("{}_T_SF.upk", &item.upk[..item.upk.len() - 7]);
            return cooked_pc.join(&candidate).is_file().then_some(candidate);
        }
    }
    None
}

fn explosion_thumbnail_asset(item: &SwapItem) -> Option<String> {
    let object = item.path.as_deref()?.split('.').next_back()?;
    (!object.is_empty()).then(|| {
        let thumbnail = format!("{object}_TThumbnail");
        format!("{thumbnail}.{thumbnail}")
    })
}

fn prettify_car_key(key: &str) -> String {
    key.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let lower = part.to_ascii_lowercase();
            let mut chars = lower.chars();
            chars
                .next()
                .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const CARD_NAME_LIMIT: usize = 30;

fn shorten_for_card(text: &str) -> String {
    let mut chars = text.chars();
    let shortened: String = chars.by_ref().take(CARD_NAME_LIMIT).collect();
    if chars.next().is_some() {
        format!("{shortened}…")
    } else {
        shortened
    }
}

/// A decal name on its own is ambiguous: multiple cars can have the same
/// skin label. Keep the car in every selection label and retain the UPK as a
/// final disambiguator when the catalogue has duplicate display names.
fn item_label(category: SwapCategory, item: &SwapItem) -> String {
    if category == SwapCategory::Skins {
        let car = item.car_name.as_deref().unwrap_or("Unknown car");
        format!("{car} · {}", item.name)
    } else {
        item.name.clone()
    }
}

pub struct SwapperState {
    base_dir: PathBuf,
    catalogs: HashMap<SwapCategory, Vec<SwapItem>>,
    errors: HashMap<SwapCategory, String>,
    target_index: HashMap<String, usize>,
    target_search: HashMap<String, String>,
    selected_car: Option<String>,
    car_search: String,
    search_input: HashMap<SwapCategory, String>,
    page: HashMap<SwapCategory, usize>,
    active: Vec<ActiveSwap>,
    view_patched: bool,
    owned_only: bool,
    thumbnails: HashMap<String, Option<Arc<[u8]>>>,
}

impl SwapperState {
    pub fn new(base_dir: &Path) -> Self {
        let mut state = Self {
            base_dir: base_dir.to_path_buf(),
            catalogs: HashMap::new(),
            errors: HashMap::new(),
            target_index: HashMap::new(),
            target_search: HashMap::new(),
            selected_car: None,
            car_search: String::new(),
            search_input: HashMap::new(),
            page: HashMap::new(),
            active: Vec::new(),
            view_patched: false,
            owned_only: false,
            thumbnails: HashMap::new(),
        };
        state.refresh_catalogs();
        state
    }

    pub fn owned_only(&self) -> bool {
        self.owned_only
    }

    pub fn set_owned_only(&mut self, enabled: bool) {
        self.owned_only = enabled;
    }

    fn catalog_path(&self, category: SwapCategory) -> Option<PathBuf> {
        let regular = self.base_dir.join(format!("{}.json", category.slug()));
        regular.is_file().then_some(regular)
    }

    fn embedded_catalog(category: SwapCategory) -> &'static str {
        match category {
            SwapCategory::Antennas => include_str!("../../assets/catalogs/antennas.json"),
            SwapCategory::Anthems => include_str!("../../assets/catalogs/anthems.json"),
            SwapCategory::Borders => include_str!("../../assets/catalogs/borders.json"),
            SwapCategory::Bodies => include_str!("../../assets/catalogs/bodies.json"),
            SwapCategory::Boosts => include_str!("../../assets/catalogs/boosts.json"),
            SwapCategory::Engines => include_str!("../../assets/catalogs/engines.json"),
            SwapCategory::Goals => include_str!("../../assets/catalogs/goals.json"),
            SwapCategory::Finishes => include_str!("../../assets/catalogs/finishes.json"),
            SwapCategory::Banners => include_str!("../../assets/catalogs/banners.json"),
            SwapCategory::Skins => include_str!("../../assets/catalogs/skins.json"),
            SwapCategory::Toppers => include_str!("../../assets/catalogs/toppers.json"),
            SwapCategory::Trails => include_str!("../../assets/catalogs/trails.json"),
            SwapCategory::Wheels => include_str!("../../assets/catalogs/wheels.json"),
        }
    }

    pub fn refresh_catalogs(&mut self) {
        self.catalogs.clear();
        self.errors.clear();
        self.thumbnails.clear();
        for category in SwapCategory::ALL {
            match self.load_catalog(category) {
                Ok(items) => {
                    self.catalogs.insert(category, items);
                }
                Err(error) => {
                    self.errors.insert(category, error);
                }
            }
        }
    }

    fn load_catalog(&self, category: SwapCategory) -> Result<Vec<SwapItem>, String> {
        let external = self.catalog_path(category);
        let (bytes, source) = if let Some(path) = external {
            (
                fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?,
                path.display().to_string(),
            )
        } else {
            (
                Self::embedded_catalog(category).as_bytes().to_vec(),
                format!("embedded {} catalog", category.label()),
            )
        };
        let root: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Failed to parse {source}: {error}"))?;
        let mut items = Vec::new();
        if category == SwapCategory::Skins {
            let bodies: Value = serde_json::from_str(Self::embedded_catalog(SwapCategory::Bodies))
                .unwrap_or_default();
            let body_ids = bodies
                .get("bodies")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|body| {
                    let name = body.get("name")?.as_str()?.to_ascii_lowercase();
                    let id = body.get("id").and_then(|id| {
                        id.as_i64()
                            .or_else(|| id.as_str().and_then(|text| text.parse().ok()))
                    })?;
                    Some((name, id))
                })
                .collect::<HashMap<_, _>>();
            if let Some(cars) = root.get("cars").and_then(Value::as_object) {
                for (car_name, car) in cars {
                    if let Some(skins) = car.get("skins").and_then(Value::as_array) {
                        let display_name = skins
                            .iter()
                            .filter_map(|skin| skin.get("name").and_then(Value::as_str))
                            .find_map(|name| name.split_once(':').map(|(car, _)| car.trim()))
                            .map(str::to_string)
                            .unwrap_or_else(|| prettify_car_key(car_name));
                        let car_product_id =
                            body_ids.get(&display_name.to_ascii_lowercase()).copied();
                        for skin in skins {
                            Self::push_item(
                                &mut items,
                                skin,
                                Some((car_name, &display_name, car_product_id)),
                            );
                        }
                    }
                }
            }
        } else if let Some(array) = root.get(category.slug()).and_then(Value::as_array) {
            for value in array {
                Self::push_item(&mut items, value, None);
            }
        }
        let mut seen = HashSet::new();
        items.retain(|item| seen.insert(item.upk.to_ascii_lowercase()));
        items.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        });
        if items.is_empty() {
            Err(format!("No swappable UPKs found in {source}"))
        } else {
            Ok(items)
        }
    }

    fn push_item(items: &mut Vec<SwapItem>, value: &Value, car: Option<(&str, &str, Option<i64>)>) {
        let Some(name) = value.get("name").and_then(Value::as_str) else {
            return;
        };
        let Some(upk) = value.get("upk_path").and_then(Value::as_str) else {
            return;
        };
        if !upk.to_ascii_lowercase().ends_with(".upk") {
            return;
        }
        items.push(SwapItem {
            name: name.to_string(),
            upk: upk.to_string(),
            path: value
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_string),
            thumbnail: value
                .get("thumbnail")
                .and_then(Value::as_str)
                .map(str::to_string),
            audio_bnk: value
                .get("audio_bnk")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            upk_type: value
                .get("upk_type")
                .and_then(Value::as_str)
                .unwrap_or("simple")
                .to_string(),
            product_id: value.get("id").and_then(|id| {
                id.as_i64()
                    .or_else(|| id.as_str().and_then(|text| text.parse().ok()))
            }),
            car_key: car.map(|(key, _, _)| key.to_string()),
            car_name: car.map(|(_, name, _)| name.to_string()),
            car_product_id: car.and_then(|(_, _, id)| id),
        });
    }

    fn manifest_path(backups_dir: &Path) -> PathBuf {
        backups_dir.join("swapper_swaps.json")
    }

    fn load_active(&mut self, backups_dir: &Path) {
        self.active = fs::read(Self::manifest_path(backups_dir))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        self.active.retain(|swap| {
            backups_dir
                .join(format!("{}.bak", swap.target_upk))
                .is_file()
        });
    }

    fn save_active(&self, backups_dir: &Path) -> Result<(), String> {
        fs::create_dir_all(backups_dir).map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec_pretty(&self.active).map_err(|error| error.to_string())?;
        fs::write(Self::manifest_path(backups_dir), bytes).map_err(|error| error.to_string())
    }

    fn apply_swap(
        &mut self,
        category: SwapCategory,
        source: &SwapItem,
        target: &SwapItem,
        cooked_pc: &Path,
        backups_dir: &Path,
    ) -> Result<(), String> {
        if source.upk.eq_ignore_ascii_case(&target.upk) {
            return Err("Choose two different items".into());
        }
        if !swap_compatible(category, source, target) {
            return Err(format!(
                "{} ({}) is not compatible with the {} target layout ({})",
                source.name, source.upk_type, target.name, target.upk_type
            ));
        }
        fs::create_dir_all(backups_dir).map_err(|error| error.to_string())?;
        let source_live = cooked_pc.join(&source.upk);
        let target_live = cooked_pc.join(&target.upk);
        let source_backup = backups_dir.join(format!("{}.bak", source.upk));
        let target_backup = backups_dir.join(format!("{}.bak", target.upk));
        // A source may already be replaced in this install.  Its .bak is the
        // pristine donor and is sufficient even when the live file is absent.
        if !source_live.is_file() && !source_backup.is_file() {
            return Err(format!("Source UPK not found: {}", source_live.display()));
        }
        if !target_live.is_file() && !target_backup.is_file() {
            return Err(format!("Target UPK not found: {}", target_live.display()));
        }
        if !target_backup.exists() {
            fs::copy(&target_live, &target_backup)
                .map_err(|error| format!("Failed to back up {}: {error}", target.upk))?;
        }
        // If the source is itself an active target, copy its pristine backup.
        let copy_source = if source_backup.is_file() {
            &source_backup
        } else {
            &source_live
        };
        crate::cosmetic_upk::patch_for_target(
            copy_source,
            &target_backup,
            &target_live,
            &self.base_dir,
            source.path.as_deref(),
            target.path.as_deref(),
        )
        .map_err(|error| format!("Failed to patch {} for {}: {error}", source.upk, target.upk))?;
        let mut target_bnk = None;
        if category == SwapCategory::Boosts {
            if let (Some(source_name), Some(target_name)) =
                (source.audio_bnk.as_deref(), target.audio_bnk.as_deref())
            {
                let source_live = cooked_pc.join(source_name);
                let target_live = cooked_pc.join(target_name);
                let target_backup = backups_dir.join(format!("{target_name}.bak"));
                if !source_live.is_file() || !target_live.is_file() {
                    let _ = fs::copy(
                        backups_dir.join(format!("{}.bak", target.upk)),
                        cooked_pc.join(&target.upk),
                    );
                    return Err(format!(
                        "Boost audio bank missing: {} or {}",
                        source_live.display(),
                        target_live.display()
                    ));
                }
                if !target_backup.is_file() {
                    fs::copy(&target_live, &target_backup)
                        .map_err(|error| format!("Failed to back up {target_name}: {error}"))?;
                }
                if let Err(error) = patch_boost_bnk(&source_live, &target_backup, &target_live) {
                    let _ = fs::copy(
                        backups_dir.join(format!("{}.bak", target.upk)),
                        cooked_pc.join(&target.upk),
                    );
                    return Err(error);
                }
                target_bnk = Some(target_name.to_string());
            }
        }
        let mut target_thumbnail = None;
        if let (Some(source_name), Some(target_name)) = (
            inferred_thumbnail(category, source, cooked_pc),
            inferred_thumbnail(category, target, cooked_pc),
        ) {
            if !source_name.eq_ignore_ascii_case(&target_name) {
                let source_live = cooked_pc.join(&source_name);
                let target_live = cooked_pc.join(&target_name);
                let source_backup = backups_dir.join(format!("{source_name}.bak"));
                let target_backup = backups_dir.join(format!("{target_name}.bak"));
                if !target_backup.is_file() {
                    fs::copy(&target_live, &target_backup).map_err(|error| {
                        format!("Failed to back up thumbnail {target_name}: {error}")
                    })?;
                }
                let thumbnail_source = if source_backup.is_file() {
                    &source_backup
                } else {
                    &source_live
                };
                let donor_thumbnail_asset = (category == SwapCategory::Goals)
                    .then(|| explosion_thumbnail_asset(source))
                    .flatten();
                let target_thumbnail_asset = (category == SwapCategory::Goals)
                    .then(|| explosion_thumbnail_asset(target))
                    .flatten();
                if let Err(error) = crate::cosmetic_upk::patch_for_target(
                    thumbnail_source,
                    &target_backup,
                    &target_live,
                    &self.base_dir,
                    donor_thumbnail_asset.as_deref(),
                    target_thumbnail_asset.as_deref(),
                ) {
                    let _ = fs::copy(
                        backups_dir.join(format!("{}.bak", target.upk)),
                        cooked_pc.join(&target.upk),
                    );
                    if let Some(target_bnk) = target_bnk.as_deref() {
                        let _ = fs::copy(
                            backups_dir.join(format!("{target_bnk}.bak")),
                            cooked_pc.join(target_bnk),
                        );
                    }
                    let _ = fs::copy(&target_backup, &target_live);
                    return Err(format!("Thumbnail patch failed: {error}"));
                }
                target_thumbnail = Some(target_name);
            }
        }
        self.active
            .retain(|swap| !swap.target_upk.eq_ignore_ascii_case(&target.upk));
        self.active.push(ActiveSwap {
            category: category.slug().to_string(),
            source_name: source.name.clone(),
            source_upk: source.upk.clone(),
            target_name: target.name.clone(),
            target_upk: target.upk.clone(),
            target_bnk,
            target_thumbnail,
        });
        self.save_active(backups_dir)
    }

    fn restore_all(
        &mut self,
        category: SwapCategory,
        cooked_pc: &Path,
        backups_dir: &Path,
    ) -> Result<usize, String> {
        self.load_active(backups_dir);
        let mut restored = 0;
        let mut errors = Vec::new();
        for swap in self
            .active
            .clone()
            .into_iter()
            .filter(|swap| swap.category == category.slug())
        {
            let backup = backups_dir.join(format!("{}.bak", swap.target_upk));
            let live = cooked_pc.join(&swap.target_upk);
            if !backup.is_file() {
                continue;
            }
            let result = (|| -> std::io::Result<()> {
                if live.exists() {
                    fs::remove_file(&live)?;
                }
                fs::copy(&backup, &live)?;
                fs::remove_file(&backup)?;
                if let Some(target_bnk) = swap.target_bnk.as_deref() {
                    let bnk_backup = backups_dir.join(format!("{target_bnk}.bak"));
                    let bnk_live = cooked_pc.join(target_bnk);
                    if bnk_backup.is_file() {
                        fs::copy(&bnk_backup, &bnk_live)?;
                        fs::remove_file(&bnk_backup)?;
                    }
                }
                if let Some(thumbnail) = swap.target_thumbnail.as_deref() {
                    let thumb_backup = backups_dir.join(format!("{thumbnail}.bak"));
                    let thumb_live = cooked_pc.join(thumbnail);
                    if thumb_backup.is_file() {
                        fs::copy(&thumb_backup, &thumb_live)?;
                        fs::remove_file(&thumb_backup)?;
                    }
                }
                Ok(())
            })();
            match result {
                Ok(()) => restored += 1,
                Err(error) => errors.push(format!("{}: {error}", swap.target_upk)),
            }
        }
        self.active.retain(|swap| {
            backups_dir
                .join(format!("{}.bak", swap.target_upk))
                .is_file()
        });
        self.save_active(backups_dir)?;
        if errors.is_empty() {
            Ok(restored)
        } else {
            Err(format!(
                "Restored {restored} swap(s), but {}",
                errors.join("; ")
            ))
        }
    }

    fn restore_swap(
        &mut self,
        target_upk: &str,
        cooked_pc: &Path,
        backups_dir: &Path,
    ) -> Result<(), String> {
        let backup = backups_dir.join(format!("{target_upk}.bak"));
        let live = cooked_pc.join(target_upk);
        if !backup.is_file() {
            return Err(format!("Backup not found: {}", backup.display()));
        }
        if live.exists() {
            fs::remove_file(&live)
                .map_err(|error| format!("Failed to remove {}: {error}", live.display()))?;
        }
        if let Err(error) = fs::copy(&backup, &live) {
            return Err(format!("Failed to restore {target_upk}: {error}"));
        }
        fs::remove_file(&backup)
            .map_err(|error| format!("Failed to remove {}: {error}", backup.display()))?;
        if let Some(target_bnk) = self
            .active
            .iter()
            .find(|swap| swap.target_upk.eq_ignore_ascii_case(target_upk))
            .and_then(|swap| swap.target_bnk.as_deref())
        {
            let bnk_backup = backups_dir.join(format!("{target_bnk}.bak"));
            let bnk_live = cooked_pc.join(target_bnk);
            if bnk_backup.is_file() {
                fs::copy(&bnk_backup, &bnk_live)
                    .map_err(|error| format!("Failed to restore {target_bnk}: {error}"))?;
                fs::remove_file(&bnk_backup).map_err(|error| {
                    format!("Failed to remove {}: {error}", bnk_backup.display())
                })?;
            }
        }
        if let Some(thumbnail) = self
            .active
            .iter()
            .find(|swap| swap.target_upk.eq_ignore_ascii_case(target_upk))
            .and_then(|swap| swap.target_thumbnail.as_deref())
        {
            let thumb_backup = backups_dir.join(format!("{thumbnail}.bak"));
            let thumb_live = cooked_pc.join(thumbnail);
            if thumb_backup.is_file() {
                fs::copy(&thumb_backup, &thumb_live)
                    .map_err(|error| format!("Failed to restore {thumbnail}: {error}"))?;
                fs::remove_file(&thumb_backup).map_err(|error| {
                    format!("Failed to remove {}: {error}", thumb_backup.display())
                })?;
            }
        }
        self.active
            .retain(|swap| !swap.target_upk.eq_ignore_ascii_case(target_upk));
        self.save_active(backups_dir)
    }

    pub fn active_count(&mut self, backups_dir: &Path) -> usize {
        self.load_active(backups_dir);
        self.active.len()
    }

    pub fn restore_all_active(
        &mut self,
        cooked_pc: &Path,
        backups_dir: &Path,
    ) -> Result<usize, String> {
        self.load_active(backups_dir);
        let targets = self
            .active
            .iter()
            .map(|swap| swap.target_upk.clone())
            .collect::<Vec<_>>();
        let mut restored = 0;
        for target in targets {
            self.restore_swap(&target, cooked_pc, backups_dir)?;
            restored += 1;
        }
        Ok(restored)
    }

    pub fn render_active_swaps(
        &mut self,
        ui: &mut egui::Ui,
        cooked_pc: &Path,
        backups_dir: &Path,
        tx: &Sender<AppMsg>,
    ) {
        self.load_active(backups_dir);
        if self.active.is_empty() {
            return;
        }
        ui.strong("Item swaps");
        ui.add_space(4.0);
        let fallback: Arc<[u8]> = fs::read(self.base_dir.join("assets").join("hebnix.png"))
            .unwrap_or_else(|_| include_bytes!("../../assets/hebnix.png").to_vec())
            .into();
        let active = self.active.clone();
        let mut restore = None;
        for row in active.chunks(5) {
            ui.columns(5, |columns| {
                for (column, swap) in row.iter().enumerate() {
                    let category = SwapCategory::ALL
                        .into_iter()
                        .find(|category| category.slug() == swap.category);
                    let source_item = category.and_then(|category| {
                        self.catalogs.get(&category).and_then(|items| {
                            items
                                .iter()
                                .find(|item| item.upk.eq_ignore_ascii_case(&swap.source_upk))
                        })
                    });
                    let target_item = category.and_then(|category| {
                        self.catalogs.get(&category).and_then(|items| {
                            items
                                .iter()
                                .find(|item| item.upk.eq_ignore_ascii_case(&swap.target_upk))
                        })
                    });
                    let image = source_item
                        .and_then(|item| item.thumbnail.as_deref())
                        .and_then(|filename| {
                            crate::cosmetic_thumbnail::extract_png(
                                &cooked_pc.join(filename),
                                &swap.category,
                            )
                            .ok()
                        })
                        .map(Arc::<[u8]>::from)
                        .unwrap_or_else(|| fallback.clone());
                    let target_image = target_item
                        .and_then(|item| item.thumbnail.as_deref())
                        .and_then(|filename| {
                            let backup = backups_dir.join(format!("{filename}.bak"));
                            let source = backup
                                .is_file()
                                .then_some(backup)
                                .unwrap_or_else(|| cooked_pc.join(filename));
                            crate::cosmetic_thumbnail::extract_png(&source, &swap.category).ok()
                        })
                        .map(Arc::<[u8]>::from)
                        .unwrap_or_else(|| fallback.clone());
                    let source_name = source_item
                        .map(|item| item.name.as_str())
                        .unwrap_or(&swap.source_name);
                    let target_name = target_item
                        .map(|item| item.name.as_str())
                        .unwrap_or(&swap.target_name);
                    egui::Frame::group(columns[column].style()).show(&mut columns[column], |ui| {
                        ui.vertical_centered(|ui| {
                            if category == Some(SwapCategory::Skins) {
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::Image::from_bytes(
                                            format!("bytes://active/target/{}", swap.target_upk),
                                            target_image,
                                        )
                                        .fit_to_exact_size(egui::vec2(48.0, 48.0)),
                                    );
                                    ui.label("→");
                                    ui.add(
                                        egui::Image::from_bytes(
                                            format!("bytes://active/source/{}", swap.target_upk),
                                            image,
                                        )
                                        .fit_to_exact_size(egui::vec2(48.0, 48.0)),
                                    );
                                });
                                ui.strong(format!("{target_name} → {source_name}"));
                                ui.weak("Original → replacement");
                            } else {
                                ui.add(
                                    egui::Image::from_bytes(
                                        format!("bytes://active/{}", swap.target_upk),
                                        image,
                                    )
                                    .fit_to_exact_size(egui::vec2(120.0, 76.0)),
                                );
                                ui.strong(source_name);
                                ui.weak(format!("Replaced {target_name}"));
                            }
                            if ui
                                .add_sized(
                                    [ui.available_width(), 24.0],
                                    egui::Button::new("Restore"),
                                )
                                .clicked()
                            {
                                restore = Some(swap.target_upk.clone());
                            }
                        });
                    });
                }
            });
            ui.add_space(6.0);
        }
        if let Some(target) = restore {
            match self.restore_swap(&target, cooked_pc, backups_dir) {
                Ok(()) => {
                    let _ = tx.send(AppMsg::Log(format!("[Swapper] Restored {target}")));
                }
                Err(error) => {
                    let _ = tx.send(AppMsg::Log(format!("[Swapper] Error: {error}")));
                }
            }
        }
    }

    pub fn render_tab(
        &mut self,
        ui: &mut egui::Ui,
        category: SwapCategory,
        cooked_pc: &Path,
        backups_dir: &Path,
        tx: &Sender<AppMsg>,
        owned_ids: &HashSet<i64>,
    ) -> bool {
        let mut owned_filter_requested = false;
        self.load_active(backups_dir);
        ui.horizontal(|ui| {
            ui.heading(category.label());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Refresh").clicked() {
                    self.refresh_catalogs();
                    self.load_active(backups_dir);
                }
                if ui.button("Restore All").clicked() {
                    match self.restore_all(category, cooked_pc, backups_dir) {
                        Ok(count) => {
                            let _ = tx.send(AppMsg::Log(format!(
                                "[Swapper] Restored {count} {} swap(s).",
                                category.label()
                            )));
                        }
                        Err(error) => {
                            let _ = tx.send(AppMsg::Log(format!("[Swapper] Error: {error}")));
                        }
                    }
                }
                if ui
                    .checkbox(&mut self.view_patched, "Show Applied")
                    .changed()
                {
                    self.page.insert(category, 0);
                }
            });
        });
        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut self.owned_only, "Show only owned replacements")
                .changed()
            {
                owned_filter_requested = self.owned_only;
            }
            if self.owned_only {
                if owned_ids.is_empty() {
                    ui.weak("Waiting for Rocket League inventory...");
                } else {
                    ui.weak(format!("{} owned product IDs captured", owned_ids.len()));
                }
            }
        });
        ui.horizontal(|ui| {
            ui.strong("Search:");
            let input = self.search_input.entry(category).or_default();
            if ui
                .add(
                    egui::TextEdit::singleline(input)
                        .hint_text(format!("Search {}...", category.label().to_lowercase()))
                        .desired_width(300.0),
                )
                .changed()
            {
                self.page.insert(category, 0);
            }
            if ui.button("Clear").clicked() {
                input.clear();
                self.page.insert(category, 0);
            }
        });
        ui.separator();
        ui.add_space(10.0);

        let Some(items) = self.catalogs.get(&category).cloned() else {
            ui.colored_label(
                egui::Color32::from_rgb(231, 76, 60),
                self.errors
                    .get(&category)
                    .map(String::as_str)
                    .unwrap_or("Catalog could not be loaded"),
            );
            return owned_filter_requested;
        };
        if category == SwapCategory::Skins {
            let mut cars = items
                .iter()
                .filter_map(|item| {
                    Some((
                        item.car_key.clone()?,
                        item.car_name.clone()?,
                        item.car_product_id,
                    ))
                })
                .collect::<Vec<_>>();
            cars.sort_by(|left, right| {
                left.1
                    .to_ascii_lowercase()
                    .cmp(&right.1.to_ascii_lowercase())
            });
            cars.dedup_by(|left, right| left.0 == right.0);
            let car_allowed = |car: &(String, String, Option<i64>)| {
                !self.owned_only || car.2.is_some_and(|id| owned_ids.contains(&id))
            };
            if self.selected_car.as_ref().is_some_and(|selected| {
                !cars
                    .iter()
                    .any(|car| &car.0 == selected && car_allowed(car))
            }) {
                self.selected_car = None;
            }
            let selected_text = self
                .selected_car
                .as_ref()
                .and_then(|selected| cars.iter().find(|car| &car.0 == selected))
                .map(|car| car.1.as_str())
                .unwrap_or("All cars");
            ui.horizontal(|ui| {
                ui.strong("Car:");
                egui::ComboBox::from_id_salt("swapper_decal_car")
                    .width(280.0)
                    .height(320.0)
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Filter:");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.car_search)
                                    .hint_text("Search cars...")
                                    .desired_width(180.0),
                            );
                            if ui.small_button("Clear").clicked() {
                                self.car_search.clear();
                            }
                        });
                        ui.separator();
                        ui.selectable_value(&mut self.selected_car, None, "All cars");
                        let query = self.car_search.trim().to_ascii_lowercase();
                        for (key, name, id) in &cars {
                            if (!self.owned_only || id.is_some_and(|id| owned_ids.contains(&id)))
                                && (query.is_empty() || name.to_ascii_lowercase().contains(&query))
                            {
                                ui.selectable_value(
                                    &mut self.selected_car,
                                    Some(key.clone()),
                                    name,
                                );
                            }
                        }
                    });
            });
            ui.add_space(6.0);
        }
        let query = self
            .search_input
            .get(&category)
            .cloned()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let filtered: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                let matches_search = query.is_empty()
                    || item_label(category, item)
                        .to_ascii_lowercase()
                        .contains(&query)
                    || item.upk.to_ascii_lowercase().contains(&query);
                let matches_car = category != SwapCategory::Skins
                    || self
                        .selected_car
                        .as_ref()
                        .is_none_or(|car| item.car_key.as_ref() == Some(car));
                let is_applied = self.active.iter().any(|swap| {
                    swap.category == category.slug()
                        && swap.source_upk.eq_ignore_ascii_case(&item.upk)
                });
                matches_search && matches_car && (!self.view_patched || is_applied)
            })
            .map(|(index, _)| index)
            .collect();
        if filtered.is_empty() {
            ui.vertical_centered(|ui| {
                ui.weak(if self.view_patched {
                    "No applied items match the search."
                } else {
                    "No items match the search."
                })
            });
            return owned_filter_requested;
        }

        const PAGE_SIZE: usize = 20;
        let total_pages = filtered.len().div_ceil(PAGE_SIZE).max(1);
        let page = self.page.entry(category).or_insert(0);
        *page = (*page).min(total_pages - 1);
        ui.horizontal(|ui| {
            ui.label(format!(
                "Page {} of {}  ({} items)",
                *page + 1,
                total_pages,
                filtered.len()
            ));
            if ui
                .add_enabled(*page > 0, egui::Button::new("Previous"))
                .clicked()
            {
                *page -= 1;
            }
            if ui
                .add_enabled(*page + 1 < total_pages, egui::Button::new("Next"))
                .clicked()
            {
                *page += 1;
            }
        });
        ui.add_space(6.0);
        let visible = &filtered[*page * PAGE_SIZE..((*page + 1) * PAGE_SIZE).min(filtered.len())];
        let fallback_thumbnail: Arc<[u8]> =
            fs::read(self.base_dir.join("assets").join("hebnix.png"))
                .unwrap_or_else(|_| include_bytes!("../../assets/hebnix.png").to_vec())
                .into();
        for &source_index in visible {
            if let Some(filename) = items[source_index].thumbnail.as_ref() {
                let cache_key = format!("{}|{}", category.slug(), filename.to_ascii_lowercase());
                let fallback = fs::read(self.base_dir.join("assets").join("hebnix.png"))
                    .unwrap_or_else(|_| include_bytes!("../../assets/hebnix.png").to_vec());
                self.thumbnails.entry(cache_key).or_insert_with(|| {
                    Some(
                        crate::cosmetic_thumbnail::extract_png(
                            &cooked_pc.join(filename),
                            category.slug(),
                        )
                        .unwrap_or(fallback)
                        .into(),
                    )
                });
            }
        }
        let mut action: Option<(usize, usize, bool)> = None;
        egui::ScrollArea::vertical()
            .id_salt(("swapper_grid", category))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for row in visible.chunks(5) {
                    ui.columns(5, |columns| {
                        for (column, &source_index) in row.iter().enumerate() {
                            let source = &items[source_index];
                            let key =
                                format!("{}|{}", category.slug(), source.upk.to_ascii_lowercase());
                            let thumbnail = source.thumbnail.as_ref().and_then(|filename| {
                                self.thumbnails
                                    .get(&format!(
                                        "{}|{}",
                                        category.slug(),
                                        filename.to_ascii_lowercase()
                                    ))
                                    .and_then(Clone::clone)
                            });
                            let target_index = self.target_index.entry(key.clone()).or_insert(0);
                            let target_allowed = |target: &SwapItem| {
                                swap_compatible(category, source, target)
                                    && (!self.owned_only
                                        || target
                                            .product_id
                                            .is_some_and(|id| owned_ids.contains(&id)))
                            };
                            if *target_index >= items.len()
                                || !target_allowed(&items[*target_index])
                            {
                                *target_index = items.iter().position(target_allowed).unwrap_or(0);
                            }
                            let has_target = items.get(*target_index).is_some_and(target_allowed);
                            egui::Frame::group(columns[column].style()).show(
                                &mut columns[column],
                                |ui| {
                                    ui.set_min_height(238.0);
                                    ui.vertical_centered(|ui| {
                                        let source_label = item_label(category, source);
                                        ui.add(
                                            egui::Image::from_bytes(
                                                format!("bytes://swapper/{key}"),
                                                thumbnail
                                                    .unwrap_or_else(|| fallback_thumbnail.clone()),
                                            )
                                            .fit_to_exact_size(egui::vec2(120.0, 76.0)),
                                        );
                                        ui.strong(shorten_for_card(&source_label)).on_hover_text(
                                            format!("{source_label}\n{}", source.upk),
                                        );
                                        if category == SwapCategory::Skins {
                                            ui.weak(shorten_for_card(&source.upk))
                                                .on_hover_text(&source.upk);
                                        }
                                        ui.add_space(4.0);
                                        ui.label(
                                            egui::RichText::new(
                                                if category == SwapCategory::Skins {
                                                    "Replace with decal"
                                                } else {
                                                    "Replace item"
                                                },
                                            )
                                            .size(11.0)
                                            .color(egui::Color32::GRAY),
                                        );
                                        ui.add_enabled_ui(has_target, |ui| {
                                            egui::ComboBox::from_id_salt((
                                                "swap_target_card",
                                                &key,
                                            ))
                                            .width(ui.available_width())
                                            .height(300.0)
                                            .close_behavior(
                                                egui::PopupCloseBehavior::CloseOnClickOutside,
                                            )
                                            .selected_text(shorten_for_card(&item_label(
                                                category,
                                                &items[*target_index],
                                            )))
                                            .show_ui(
                                                ui,
                                                |ui| {
                                                    let target_filter = self
                                                        .target_search
                                                        .entry(key.clone())
                                                        .or_default();
                                                    ui.horizontal(|ui| {
                                                        ui.label("Filter:");
                                                        ui.add(
                                                            egui::TextEdit::singleline(
                                                                target_filter,
                                                            )
                                                            .hint_text("Search items...")
                                                            .desired_width(150.0),
                                                        );
                                                        if ui.small_button("Clear").clicked() {
                                                            target_filter.clear();
                                                        }
                                                    });
                                                    ui.separator();
                                                    let target_query =
                                                        target_filter.to_ascii_lowercase();
                                                    for (index, item) in items
                                                        .iter()
                                                        .enumerate()
                                                        .filter(|(_, item)| {
                                                            swap_compatible(category, source, item)
                                                                && (!self.owned_only
                                                                    || item.product_id.is_some_and(
                                                                        |id| {
                                                                            owned_ids.contains(&id)
                                                                        },
                                                                    ))
                                                                && (target_query.is_empty()
                                                                    || item_label(category, item)
                                                                        .to_ascii_lowercase()
                                                                        .contains(&target_query)
                                                                    || item
                                                                        .upk
                                                                        .to_ascii_lowercase()
                                                                        .contains(&target_query))
                                                        })
                                                    {
                                                        let label = item_label(category, item);
                                                        ui.selectable_value(
                                                            target_index,
                                                            index,
                                                            &label,
                                                        )
                                                        .on_hover_text(format!(
                                                            "{label}\n{}",
                                                            item.upk
                                                        ));
                                                    }
                                                },
                                            );
                                        });
                                        if !has_target {
                                            ui.weak("No owned replacement is available");
                                            return;
                                        }
                                        let active = self.active.iter().find(|swap| {
                                            swap.category == category.slug()
                                                && swap.source_upk.eq_ignore_ascii_case(&source.upk)
                                                && swap
                                                    .target_upk
                                                    .eq_ignore_ascii_case(&items[*target_index].upk)
                                        });
                                        if let Some(active) = active {
                                            ui.weak(format!("Set as {}", active.target_name));
                                        }
                                        if ui
                                            .add_sized(
                                                [ui.available_width(), 24.0],
                                                egui::Button::new(if active.is_some() {
                                                    "Restore"
                                                } else {
                                                    "Apply"
                                                }),
                                            )
                                            .clicked()
                                        {
                                            action = Some((
                                                source_index,
                                                *target_index,
                                                active.is_some(),
                                            ));
                                        }
                                    });
                                },
                            );
                        }
                    });
                    ui.add_space(6.0);
                }
            });
        if let Some((source_index, target_index, restoring)) = action {
            let source = items[source_index].clone();
            let target = items[target_index].clone();
            let result = if restoring {
                self.restore_swap(&target.upk, cooked_pc, backups_dir)
            } else {
                self.apply_swap(category, &source, &target, cooked_pc, backups_dir)
            };
            match result {
                Ok(()) => {
                    let message = if restoring {
                        format!("[Swapper] Restored {} ({})", target.name, target.upk)
                    } else {
                        format!(
                            "[Swapper] {} -> {} (replaced {})",
                            source.name, target.name, target.upk
                        )
                    };
                    let _ = tx.send(AppMsg::Log(message));
                }
                Err(error) => {
                    let _ = tx.send(AppMsg::Log(format!("[Swapper] Error: {error}")));
                }
            }
        }
        owned_filter_requested
    }
}
