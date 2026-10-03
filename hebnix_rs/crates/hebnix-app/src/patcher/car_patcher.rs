use crate::i18n::{t, t_args};
use crate::config::{Config, PatchSource};
use crate::messages::AppMsg;
use crate::patcher::catalog::PatchCatalog;
use crate::patcher::{backup_guard, patch_source_selector};
use crossbeam_channel::Sender;
use eframe::egui;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

const UPK_MAGIC: [u8; 4] = 0x9E2A83C1u32.to_le_bytes();

#[derive(Clone, Debug)]
pub struct CarPatch {
    pub name: String,
    pub body_id: i32,
    pub mesh_path: String,
    pub json_path: PathBuf,
    pub upk_path: PathBuf,
    strategy: CarPatchStrategy,
    donor_mesh: String,
    target_mesh: String,
    material_map: Vec<u16>,
    thumbnail: Option<Arc<[u8]>>,
    support: Result<(), String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CarPatchStrategy {
    WholePackage,
    PreparedBody,
    EmbedGeometry,
}

#[derive(Clone, Debug)]
struct BodyTarget {
    name: String,
    upk_path: String,
}

pub struct CarPatcherState {
    base_dir: PathBuf,
    pub cars_dir: PathBuf,
    pub cars: Vec<CarPatch>,
    pub active_cars: HashMap<String, String>,
    pub(crate) source: PatchSource,
    search: String,
    bodies_catalog: Option<Value>,
    catalog_error: Option<String>,
    catalog: PatchCatalog,
}

fn catalog_id(value: Option<&Value>) -> Option<i32> {
    value.and_then(|value| {
        value
            .as_i64()
            .and_then(|id| i32::try_from(id).ok())
            .or_else(|| value.as_str()?.parse().ok())
    })
}

fn normalized_json_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn manifest_field<'a>(data: &'a Value, wanted: &str) -> Option<&'a Value> {
    let wanted = normalized_json_key(wanted);
    data.as_object()?
        .iter()
        .find_map(|(key, value)| (normalized_json_key(key) == wanted).then_some(value))
}

fn profile_material_map(data: &Value) -> Result<Vec<u16>, String> {
    let materials =
        manifest_field(data, "Materials").ok_or("The geometry profile has no Materials section")?;
    let targets = manifest_field(materials, "TargetSlots")
        .and_then(Value::as_object)
        .ok_or("The geometry profile has no Materials.TargetSlots object")?;
    let sections = manifest_field(materials, "DonorSections")
        .and_then(Value::as_object)
        .ok_or("The geometry profile has no Materials.DonorSections object")?;
    let target_slot = |label: &str| -> Option<u16> {
        let wanted = normalized_json_key(label);
        targets.iter().find_map(|(name, value)| {
            let candidate = normalized_json_key(name);
            let matches = candidate == wanted
                || (wanted.starts_with("paint") && candidate == "paint")
                || (["engine", "glass", "plate", "trim"].contains(&wanted.as_str())
                    && candidate == "chassis");
            matches
                .then(|| value.as_u64().and_then(|n| u16::try_from(n).ok()))
                .flatten()
        })
    };
    let highest = sections
        .keys()
        .filter_map(|key| key.parse::<usize>().ok())
        .max()
        .ok_or("The geometry profile has no donor material indices")?;
    let chassis = target_slot("Chassis").unwrap_or(0);
    let mut mapping = vec![chassis; highest + 1];
    for (index, label) in sections {
        let index = index
            .parse::<usize>()
            .map_err(|_| format!("Invalid donor material index '{index}'"))?;
        let label = label
            .as_str()
            .ok_or("Donor material labels must be strings")?;
        mapping[index] = target_slot(label).unwrap_or(chassis);
    }
    Ok(mapping)
}

fn upk_file_name(path: &str) -> Result<String, String> {
    let path = Path::new(path);
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "The bodies catalog returned an unsafe UPK path: {path:?}"
        ));
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "The bodies catalog returned an invalid UPK filename".to_string())?;
    if !name.to_ascii_lowercase().ends_with(".upk") {
        return Err(format!("The BodyID resolved to a non-UPK file: {name}"));
    }
    Ok(name.to_string())
}

fn upk_versions(path: &Path) -> Result<(u16, u16), String> {
    let mut file = fs::File::open(path)
        .map_err(|error| format!("Could not open {}: {error}", path.display()))?;
    let mut summary = [0u8; 8];
    file.read_exact(&mut summary)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    if summary[..4] != UPK_MAGIC {
        return Err(format!("{} is not a valid UPK file", path.display()));
    }
    Ok((
        u16::from_le_bytes([summary[4], summary[5]]),
        u16::from_le_bytes([summary[6], summary[7]]),
    ))
}

fn validate_replacement_upk(path: &Path, target: &BodyTarget) -> Result<(), String> {
    let (file_version, licensee_version) = upk_versions(path)?;
    if licensee_version < 33 {
        return Err(format!(
            "{} is a standalone CustomCar mesh package (UPK {file_version}, licensee {licensee_version}), not a complete current Rocket League body package. Replacing {} with it would remove the required {} body asset and can crash the game. This package needs mesh-reference injection; no files were changed.",
            path.file_name().unwrap_or_default().to_string_lossy(),
            target.upk_path,
            target.name,
        ));
    }
    Ok(())
}

fn files_with_extension(root: &Path, extension: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case(extension))
            {
                found.push(path);
            }
        }
    }
    found
}

impl CarPatcherState {
    pub fn new(base_dir: &Path, config: &Config) -> Self {
        let cars_dir = base_dir.join("cars");
        let _ = fs::create_dir_all(&cars_dir);
        let mut state = Self {
            base_dir: base_dir.to_path_buf(),
            cars_dir,
            cars: Vec::new(),
            active_cars: config.patcher.active_cars.clone(),
            source: config.patcher.car_source,
            search: String::new(),
            bodies_catalog: None,
            catalog_error: None,
            catalog: PatchCatalog::new(base_dir, "car"),
        };
        state.refresh_cars();
        state
    }

    pub fn set_catalog(&mut self, bodies: Value) -> Result<(), String> {
        if !bodies.get("bodies").is_some_and(Value::is_array) {
            return Err("The bodies catalog has no 'bodies' array".to_string());
        }
        self.bodies_catalog = Some(bodies);
        self.catalog_error = None;
        Ok(())
    }

    fn resolve_body(&self, body_id: i32) -> Result<BodyTarget, String> {
        let catalog = self
            .bodies_catalog
            .as_ref()
            .ok_or_else(|| "The bodies catalog has not been downloaded".to_string())?;
        let body = catalog["bodies"]
            .as_array()
            .and_then(|bodies| {
                bodies
                    .iter()
                    .find(|body| catalog_id(body.get("id")) == Some(body_id))
            })
            .ok_or_else(|| format!("BodyID {body_id} was not found in bodies.json"))?;
        let name = body
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Unknown car")
            .to_string();
        let upk_path = body
            .get("upk_path")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("BodyID {body_id} has no upk_path in bodies.json"))?;
        Ok(BodyTarget {
            name,
            upk_path: upk_file_name(upk_path)?,
        })
    }

    pub fn refresh_cars(&mut self) {
        self.cars.clear();
        for json_path in files_with_extension(&self.cars_dir, "json") {
            let Ok(bytes) = fs::read(&json_path) else {
                continue;
            };
            let Ok(root) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            let Some(entries) = root.as_object() else {
                continue;
            };
            let parent = json_path.parent().unwrap_or(&self.cars_dir);
            let upks = files_with_extension(parent, "upk");
            for (name, data) in entries {
                let Some(body_id) = catalog_id(manifest_field(data, "bodyid")) else {
                    continue;
                };
                let Some(mesh_path) = manifest_field(data, "meshpath").and_then(Value::as_str)
                else {
                    continue;
                };
                let package_name = mesh_path.split('.').next().unwrap_or(mesh_path);
                let matching = upks.iter().find(|path| {
                    path.file_stem()
                        .and_then(|stem| stem.to_str())
                        .is_some_and(|stem| stem.eq_ignore_ascii_case(package_name))
                });
                let upk_path = matching
                    .cloned()
                    .or_else(|| (upks.len() == 1).then(|| upks[0].clone()));
                let Some(upk_path) = upk_path else {
                    continue;
                };
                let declared_strategy = manifest_field(data, "Patch")
                    .and_then(|patch| manifest_field(patch, "Strategy"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let strategy = if declared_strategy.eq_ignore_ascii_case("embed_geometry") {
                    CarPatchStrategy::EmbedGeometry
                } else if declared_strategy.eq_ignore_ascii_case("prepared_body") {
                    CarPatchStrategy::PreparedBody
                } else {
                    CarPatchStrategy::WholePackage
                };
                let upk_path = if strategy == CarPatchStrategy::PreparedBody {
                    let Some(package) = manifest_field(data, "Patch")
                        .and_then(|patch| manifest_field(patch, "PreparedPackage"))
                        .and_then(Value::as_str)
                        .and_then(|name| upk_file_name(name).ok())
                    else {
                        continue;
                    };
                    parent.join(package)
                } else {
                    upk_path
                };
                let declared_status = manifest_field(data, "Status")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let donor_mesh = mesh_path
                    .split_once('.')
                    .map(|(_, mesh)| mesh)
                    .unwrap_or(mesh_path)
                    .to_string();
                let target_mesh = manifest_field(data, "Target")
                    .and_then(|target| manifest_field(target, "Mesh"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let material_map = if strategy == CarPatchStrategy::EmbedGeometry {
                    profile_material_map(data)
                } else {
                    Ok(Vec::new())
                };
                let support = if declared_status.eq_ignore_ascii_case("unsupported") {
                    let reason = manifest_field(data, "UnsupportedReason")
                        .and_then(Value::as_str)
                        .unwrap_or("This patch profile is marked unsupported.");
                    Err(format!("Not supported: {reason}"))
                } else if strategy == CarPatchStrategy::EmbedGeometry {
                    if !declared_status.eq_ignore_ascii_case("supported") {
                        Err("Not supported: geometry profiles must explicitly set Status to supported.".to_string())
                    } else if target_mesh.is_empty() {
                        Err("Not supported: the geometry profile has no Target.Mesh.".to_string())
                    } else {
                        material_map.as_ref().map(|_| ()).map_err(Clone::clone)
                    }
                } else {
                    match upk_versions(&upk_path) {
                        Ok((_, licensee)) if licensee >= 33 => Ok(()),
                        Ok((file_version, licensee)) => Err(format!(
                            "Not supported: standalone CustomCar mesh package (UPK {file_version}, licensee {licensee}). It cannot safely replace a complete Rocket League body package."
                        )),
                        Err(error) => Err(format!("Not supported: {error}")),
                    }
                };
                let thumbnail = fs::read(parent.join("thumbnail.png"))
                    .ok()
                    .filter(|bytes| !bytes.is_empty())
                    .map(Arc::from);
                self.cars.push(CarPatch {
                    name: name.clone(),
                    body_id,
                    mesh_path: mesh_path.to_string(),
                    json_path: json_path.clone(),
                    upk_path,
                    strategy,
                    donor_mesh,
                    target_mesh,
                    material_map: material_map.unwrap_or_default(),
                    thumbnail,
                    support,
                });
            }
        }
        self.cars.sort_by(|left, right| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
        });
    }

    fn delete_local_patch(&mut self, car: &CarPatch) -> Result<(), String> {
        if !car.json_path.starts_with(&self.cars_dir) {
            return Err("Refusing to delete a patch outside Hebnix's cars folder".to_string());
        }
        let parent = car
            .json_path
            .parent()
            .ok_or_else(|| "Patch manifest has no parent folder".to_string())?;
        for path in [&car.json_path, &car.upk_path, &parent.join("thumbnail.png")] {
            if path.is_file() {
                fs::remove_file(path)
                    .map_err(|error| format!("Could not delete {}: {error}", path.display()))?;
            }
        }
        let _ = fs::remove_dir(parent);
        self.refresh_cars();
        Ok(())
    }

    pub fn import_zip(&mut self, zip_path: &Path, tx: &Sender<AppMsg>) -> Result<usize, String> {
        let before = self.cars.len();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos();
        let destination = self
            .cars_dir
            .join(format!("{nonce:x}-{:x}", std::process::id()));
        fs::create_dir(&destination)
            .map_err(|error| format!("Could not create import folder: {error}"))?;

        let result = (|| {
            let file = fs::File::open(zip_path)
                .map_err(|error| format!("Could not open {}: {error}", zip_path.display()))?;
            let mut archive = zip::ZipArchive::new(file)
                .map_err(|error| format!("Could not read ZIP: {error}"))?;
            for index in 0..archive.len() {
                let mut entry = archive
                    .by_index(index)
                    .map_err(|error| format!("Could not read ZIP entry: {error}"))?;
                let enclosed = entry
                    .enclosed_name()
                    .ok_or_else(|| format!("ZIP contains an unsafe path: {}", entry.name()))?;
                let output = destination.join(enclosed);
                if entry.is_dir() {
                    fs::create_dir_all(&output).map_err(|error| error.to_string())?;
                    continue;
                }
                if let Some(parent) = output.parent() {
                    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                let mut output_file = fs::File::create(&output)
                    .map_err(|error| format!("Could not create {}: {error}", output.display()))?;
                std::io::copy(&mut entry, &mut output_file)
                    .map_err(|error| format!("Could not extract {}: {error}", output.display()))?;
                output_file.flush().map_err(|error| error.to_string())?;
            }
            Ok::<(), String>(())
        })();

        if let Err(error) = result {
            let _ = fs::remove_dir_all(&destination);
            return Err(error);
        }

        self.refresh_cars();
        let imported = self.cars.len().saturating_sub(before);
        if imported == 0 {
            let _ = fs::remove_dir_all(&destination);
            self.refresh_cars();
            return Err(
                t("import-zip-no-usable-custom-cars-were-found")
                    .to_string(),
            );
        }
        let _ = tx.send(AppMsg::Log(format!(
            "[Cars] Imported {imported} custom car patch(es)."
        )));
        Ok(imported)
    }

    fn backup_dir(backups_dir: &Path) -> PathBuf {
        backups_dir.join("CarPatcher")
    }

    fn apply(
        &mut self,
        car: &CarPatch,
        cooked_pc: &Path,
        backups_dir: &Path,
        tx: &Sender<AppMsg>,
        config: &mut Config,
    ) -> Result<(), String> {
        if crate::messages::block_item_action_if_game_running(tx) {
            return Ok(());
        }
        if let Err(reason) = &car.support {
            return Err(reason.clone());
        }
        let target = self.resolve_body(car.body_id)?;
        if matches!(
            car.strategy,
            CarPatchStrategy::WholePackage | CarPatchStrategy::PreparedBody
        ) {
            validate_replacement_upk(&car.upk_path, &target)?;
        }
        if self.active_cars.contains_key(&target.upk_path) {
            return Err(format!(
                "{} is already patched. Restore it before applying another custom car.",
                target.name
            ));
        }
        let live_path = cooked_pc.join(&target.upk_path);
        if !live_path.is_file() {
            return Err(format!(
                "BodyID {} resolved to {}, but {} does not exist",
                car.body_id,
                target.upk_path,
                live_path.display()
            ));
        }

        let car_backups = Self::backup_dir(backups_dir);
        let backup_name = format!("{}.bak", target.upk_path);
        backup_guard::prepare(cooked_pc, &car_backups, "car-build.sha256", |name| {
            name.to_ascii_lowercase().ends_with(".upk.bak")
        })?;
        let backup_path = car_backups.join(&backup_name);
        if !backup_path.is_file() {
            fs::copy(&live_path, &backup_path)
                .map_err(|error| format!("Could not back up {}: {error}", live_path.display()))?;
        }

        let staged = live_path.with_extension("upk.hebnix-car.tmp");
        let _ = fs::remove_file(&staged);
        match car.strategy {
            CarPatchStrategy::WholePackage | CarPatchStrategy::PreparedBody => {
                fs::copy(&car.upk_path, &staged).map_err(|error| {
                    format!(
                        "Could not stage custom UPK {}: {error}",
                        car.upk_path.display()
                    )
                })?;
            }
            CarPatchStrategy::EmbedGeometry => {
                crate::patcher::car_geometry::transplant_profile(
                    &live_path,
                    &car.upk_path,
                    &staged,
                    &car.target_mesh,
                    &car.donor_mesh,
                    &car.material_map,
                )?;
            }
        }
        let replace_result = fs::copy(&staged, &live_path);
        let _ = fs::remove_file(&staged);
        if let Err(error) = replace_result {
            let _ = fs::copy(&backup_path, &live_path);
            return Err(format!(
                "Could not replace {}: {error}",
                live_path.display()
            ));
        }

        self.active_cars
            .insert(target.upk_path.clone(), car.name.clone());
        config.patcher.active_cars = self.active_cars.clone();
        config
            .save(&self.base_dir)
            .map_err(|error| error.to_string())?;
        let _ = tx.send(AppMsg::Log(format!(
            "[Cars] Applied '{}' to {} (BodyID {}) using {:?}.",
            car.name, target.name, car.body_id, car.strategy
        )));
        Ok(())
    }

    pub fn restore(
        &mut self,
        target_upk: &str,
        cooked_pc: &Path,
        backups_dir: &Path,
        tx: &Sender<AppMsg>,
        config: &mut Config,
    ) -> Result<(), String> {
        if crate::messages::block_item_action_if_game_running(tx) {
            return Ok(());
        }
        let target_upk = upk_file_name(target_upk)?;
        let car_backups = Self::backup_dir(backups_dir);
        backup_guard::check(cooked_pc, &car_backups, "car-build.sha256", |name| {
            name.to_ascii_lowercase().ends_with(".upk.bak")
        })?;
        let backup = car_backups.join(format!("{target_upk}.bak"));
        if !backup.is_file() {
            return Err(format!("No original backup exists for {target_upk}"));
        }
        fs::copy(&backup, cooked_pc.join(&target_upk))
            .map_err(|error| format!("Could not restore {target_upk}: {error}"))?;
        let patch_name = self.active_cars.remove(&target_upk).unwrap_or_default();
        config.patcher.active_cars = self.active_cars.clone();
        config
            .save(&self.base_dir)
            .map_err(|error| error.to_string())?;
        let _ = tx.send(AppMsg::Log(format!(
            "[Cars] Restored {target_upk} from the original backup{}.",
            if patch_name.is_empty() {
                String::new()
            } else {
                format!(" (removed '{patch_name}')")
            }
        )));
        Ok(())
    }

    pub fn restore_all(
        &mut self,
        cooked_pc: &Path,
        backups_dir: &Path,
        tx: &Sender<AppMsg>,
        config: &mut Config,
    ) -> Result<usize, String> {
        let targets = self.active_cars.keys().cloned().collect::<Vec<_>>();
        let mut restored = 0;
        for target in targets {
            self.restore(&target, cooked_pc, backups_dir, tx, config)?;
            restored += 1;
        }
        Ok(restored)
    }

    pub fn render_tab(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        cooked_pc: &Path,
        backups_dir: &Path,
        tx: &Sender<AppMsg>,
        config: &mut Config,
    ) {
        ui.heading(t("app-car-patcher"));
        ui.add_space(10.0);
        if patch_source_selector(ui, &mut self.source) {
            config.patcher.car_source = self.source;
            let _ = config.save(&self.base_dir);
        }
        ui.add_space(8.0);

        if self.source == PatchSource::Catalog {
            if let Some(zip) = self.catalog.render(ui, &self.search, ctx, tx, 4) {
                match self.import_zip(&zip, tx) {
                    Ok(_) => {
                        self.source = PatchSource::Custom;
                        config.patcher.car_source = self.source;
                        let _ = config.save(&self.base_dir);
                    }
                    Err(error) => {
                        let _ = tx.send(AppMsg::Log(format!(
                            "[Cars] Catalog import failed: {error}"
                        )));
                    }
                }
            }
            return;
        }

        ui.horizontal(|ui| {
            if ui.button(t("ball-import-zip")).clicked() {
                let dialog = rfd::FileDialog::new().add_filter(t("ball-zip-archives"), &["zip"]);
                if let Some(file) = crate::winutil::parent_file_dialog(dialog).pick_file() {
                    if let Err(error) = self.import_zip(&file, tx) {
                        let _ = tx.send(AppMsg::Log(format!("[Cars] Import failed: {error}")));
                    }
                }
            }
            if ui.button(t("btn-refresh")).clicked() {
                self.refresh_cars();
            }
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text(t("tab-search-local-patches")));
        });
        ui.separator();

        if let Some(error) = &self.catalog_error {
            ui.colored_label(egui::Color32::from_rgb(0xe7, 0x4c, 0x3c), error);
        }
        let query = self.search.trim().to_ascii_lowercase();
        let visible = self
            .cars
            .iter()
            .filter(|car| query.is_empty() || car.name.to_ascii_lowercase().contains(&query))
            .cloned()
            .collect::<Vec<_>>();
        if visible.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(30.0);
                ui.weak(t("tab-no-local-car-patches-import-a"));
            });
            return;
        }

        egui::ScrollArea::vertical()
            .id_salt("car_patcher_local")
            .show(ui, |ui| {
                let (supported, unsupported): (Vec<_>, Vec<_>) =
                    visible.into_iter().partition(|car| car.support.is_ok());
                if !supported.is_empty() {
                    ui.heading(t_args("tab-supported-patches-supported", &[("supported", (supported.len()).to_string().into())]));
                    ui.weak(t("tab-these-packages-can-safely-replace-their"));
                    ui.add_space(6.0);
                }
                let supported_count = supported.len();
                let mut ordered = supported;
                ordered.extend(unsupported);
                for (index, car) in ordered.into_iter().enumerate() {
                    if index == supported_count {
                        if index > 0 {
                            ui.add_space(10.0);
                        }
                        ui.heading(t("tab-not-supported"));
                        ui.weak(t("tab-kept-for-reference-applying-these-profil"));
                        ui.add_space(6.0);
                    }
                    let resolved = self.resolve_body(car.body_id);
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let bytes = car
                                .thumbnail
                                .clone()
                                .unwrap_or_else(|| Arc::from(crate::tray::EMBEDDED_ICON));
                            ui.add(
                                egui::Image::from_bytes(
                                    format!("bytes://car-patch/{}", car.json_path.display()),
                                    bytes,
                                )
                                .fit_to_exact_size(egui::vec2(108.0, 72.0)),
                            );
                            ui.vertical(|ui| match (&car.support, &resolved) {
                                (Ok(()), Ok(target)) => {
                                    ui.strong(t_args("tab-patching-target-to-car", &[("target", target.name.to_string().into()), ("car", car.name.to_string().into())]));
                                }
                                (Err(error), _) | (_, Err(error)) => {
                                    ui.strong(&car.name);
                                    ui.colored_label(egui::Color32::from_rgb(230, 160, 60), error);
                                }
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if let Ok(target) = &resolved {
                                        if self.active_cars.contains_key(&target.upk_path) {
                                            if ui.button(t("ball-restore-original")).clicked() {
                                                if let Err(error) = self.restore(
                                                    &target.upk_path,
                                                    cooked_pc,
                                                    backups_dir,
                                                    tx,
                                                    config,
                                                ) {
                                                    let _ = tx.send(AppMsg::Log(format!(
                                                        "[Cars] Restore failed: {error}"
                                                    )));
                                                }
                                            }
                                        } else if ui
                                            .add_enabled(
                                                car.support.is_ok(),
                                                egui::Button::new(if car.support.is_ok() {
                                                    t("ball-apply")
                                                } else {
                                                    t("tab-not-supported")
                                                }),
                                            )
                                            .clicked()
                                        {
                                            if let Err(error) =
                                                self.apply(&car, cooked_pc, backups_dir, tx, config)
                                            {
                                                let _ = tx.send(AppMsg::Log(format!(
                                                    "[Cars] Patch failed: {error}"
                                                )));
                                            }
                                        }
                                    }
                                    if ui
                                        .add_enabled(
                                            !resolved.as_ref().is_ok_and(|target| {
                                                self.active_cars.contains_key(&target.upk_path)
                                            }),
                                            egui::Button::new(t("presets-delete")),
                                        )
                                        .clicked()
                                    {
                                        if let Err(error) = self.delete_local_patch(&car) {
                                            let _ = tx.send(AppMsg::Log(format!(
                                                "[Cars] Delete failed: {error}"
                                            )));
                                        } else {
                                            let _ = tx.send(AppMsg::Log(format!(
                                                "[Cars] Deleted local patch '{}'.",
                                                car.name
                                            )));
                                        }
                                    }
                                },
                            );
                        });
                    });
                    ui.add_space(6.0);
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn rejects_traversal_in_catalog_upk_path() {
        assert!(upk_file_name("../Body_Octane.upk").is_err());
        assert!(upk_file_name("C:\\Body_Octane.upk").is_err());
        assert_eq!(upk_file_name("Body_Octane.upk").unwrap(), "Body_Octane.upk");
    }

    #[test]
    fn discovers_custom_car_manifest_name_body_and_matching_upk() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("hebnix-car-patcher-{}-{nonce}", std::process::id()));
        let cars = root.join("cars").join("sample");
        fs::create_dir_all(&cars).unwrap();
        fs::write(
            cars.join("Evo.json"),
            br#"{
                "Evo5f repl endo": {
                    "MeshPath": "EVO_5F.EVO_5F_SK",
                    "BodyId": 1624,
                    "WheelScale": 0.75,
                    "ChassisMaterialIndex": 3,
                    "SkinMaterialIndex": 0
                }
            }"#,
        )
        .unwrap();
        fs::write(cars.join("EVO_5F.upk"), UPK_MAGIC).unwrap();

        let config = Config::default();
        let state = CarPatcherState::new(&root, &config);
        assert_eq!(state.cars.len(), 1);
        assert_eq!(state.cars[0].name, "Evo5f repl endo");
        assert_eq!(state.cars[0].body_id, 1624);
        assert_eq!(state.cars[0].mesh_path, "EVO_5F.EVO_5F_SK");
        assert_eq!(state.cars[0].upk_path, cars.join("EVO_5F.upk"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_fields_are_case_and_separator_insensitive() {
        let value = serde_json::json!({
            "mesh_path": "AE86.ae86_SK",
            "BodyId": 22
        });
        assert_eq!(
            manifest_field(&value, "MeshPath").and_then(Value::as_str),
            Some("AE86.ae86_SK")
        );
        assert_eq!(catalog_id(manifest_field(&value, "BODY_ID")), Some(22));
    }

    #[test]
    fn rejects_legacy_customcar_mesh_package_as_whole_body_replacement() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "hebnix-legacy-custom-car-{}-{nonce}.upk",
            std::process::id()
        ));
        let mut summary = Vec::from(UPK_MAGIC);
        summary.extend_from_slice(&868u16.to_le_bytes());
        summary.extend_from_slice(&0u16.to_le_bytes());
        fs::write(&path, summary).unwrap();

        let target = BodyTarget {
            name: "Endo".to_string(),
            upk_path: "body_endo_SF.upk".to_string(),
        };
        let error = validate_replacement_upk(&path, &target).unwrap_err();
        assert!(error.contains("standalone CustomCar mesh package"));
        assert!(error.contains("body_endo_SF.upk"));

        fs::remove_file(path).unwrap();
    }
}
