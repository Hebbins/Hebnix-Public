//! Offline, baseline-relative alignment. Never use the swapper's stock backup.
use crate::i18n::{t, t_args};
use super::upk_package::{UpkPackage, strip};
use eframe::egui;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
};

#[derive(Clone, Copy, Default, Serialize, Deserialize, Debug, PartialEq)]
pub struct Settings {
    camber: f32,
    vertical: f32,
    lateral: f32,
}
impl Settings {
    fn validate(self) -> Result<(), String> {
        if !self.camber.is_finite()
            || !self.vertical.is_finite()
            || !self.lateral.is_finite()
            || self.camber.abs() > 30.
            || self.vertical.abs() > 15.
            || self.lateral.abs() > 15.
        {
            return Err("Alignment values are outside safe slider limits".into());
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
struct Record {
    version: u32,
    baseline_hash: String,
    installed_hash: String,
    settings: Settings,
    #[serde(default)]
    adjustments: u64,
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}
fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}
fn write_record(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temp = path.with_extension("json.pending");
    write(&temp, bytes)?;
    fs::rename(&temp, path).map_err(|e| format!("Could not commit alignment metadata: {e}"))
}
fn mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let [x, y, z, w] = a;
    let [u, v, t, s] = b;
    [
        w * u + x * s + y * t - z * v,
        w * v - x * t + y * s + z * u,
        w * t + x * v - y * u + z * s,
        w * s - x * u - y * v - z * t,
    ]
}
fn adjusted(q: [f32; 4], position: [f32; 3], left: bool, s: Settings) -> ([f32; 4], [f32; 3]) {
    let side = if left { -1. } else { 1. };
    let half = (-side * s.camber).to_radians() / 2.;
    (
        mul([half.sin(), 0., 0., half.cos()], q),
        [
            position[0],
            position[1] + side * s.lateral,
            position[2] + s.vertical,
        ],
    )
}
fn build(baseline: &Path, output: &Path, s: Settings) -> Result<(), String> {
    s.validate()?;
    let mut p = UpkPackage::load(baseline)?;
    // Resolve the usable local body mesh, including an already-swapped mesh. Some legacy
    // packages (notably Octane) also contain a development body whose Mesh is an import.
    let mut body_meshes = Vec::new();
    for body in p
        .exports
        .iter()
        .filter(|e| strip(&p.class_of(e)) == "ProductAsset_Body_TA")
    {
        let Some(mesh_prop) = p
            .serialized_props(body)?
            .0
            .into_iter()
            .find(|v| v.name == "Mesh")
        else {
            continue;
        };
        let mesh = p.read_int(body.serial_offset + mesh_prop.value_offset)?;
        let Ok(mesh_index) = usize::try_from(mesh - 1) else {
            continue;
        };
        let Some(export) = p.exports.get(mesh_index) else {
            continue;
        };
        if strip(&p.class_of(export)) == "SkeletalMesh" {
            body_meshes.push(export.clone());
        }
    }
    if body_meshes.len() != 1 {
        return Err(format!(
            "Unsupported package: expected one local skeletal body mesh, found {}",
            body_meshes.len()
        ));
    }
    let e = body_meshes.pop().unwrap();
    let (_, native) = p.serialized_props(&e)?;
    let materials_at = e.serial_offset + native + 28;
    let materials = p.read_int(materials_at)?;
    if !(1..=64).contains(&materials) {
        return Err("Unsupported mesh material layout".into());
    }
    let bones_at = materials_at + 4 + materials as usize * 4 + 24;
    let count = p.read_int(bones_at)?;
    if !(1..=256).contains(&count)
        || bones_at + 4 + count as usize * 52 > e.serial_offset + e.serial_size
    {
        return Err("Unsupported skeleton layout".into());
    }
    let before = p.image.clone();
    let mut spans = Vec::new();
    let mut found = std::collections::HashSet::new();
    let float = |at: usize| f32::from_le_bytes(before[at..at + 4].try_into().unwrap());
    for i in 0..count as usize {
        let at = bones_at + 4 + i * 52;
        let name = p
            .names
            .get(p.read_int(at)? as usize)
            .ok_or("Invalid bone name")?
            .clone();
        if ![
            "FL_WheelTranslation_jnt",
            "FR_WheelTranslation_jnt",
            "BL_WheelTranslation_jnt",
            "BR_WheelTranslation_jnt",
        ]
        .contains(&name.as_str())
        {
            continue;
        }
        if !found.insert(name.clone()) {
            return Err("Duplicate wheel anchor".into());
        }
        let parent = p.read_int(at + 44)?;
        if parent < 0 || parent >= count {
            return Err("Invalid wheel parent".into());
        }
        let root = bones_at + 4 + parent as usize * 52;
        let root_name = p
            .names
            .get(p.read_int(root)? as usize)
            .ok_or("Invalid parent name")?;
        if root_name != "root_jnt"
            || (0..3).any(|j| {
                float(root + 28 + j * 4).abs() > 0.0001 || float(root + 12 + j * 4).abs() > 0.0001
            })
            || (float(root + 24).abs() - 1.).abs() > 0.0001
        {
            return Err("Unsupported wheel parent transform; no files changed".into());
        }
        let q = std::array::from_fn(|j| float(at + 12 + j * 4));
        let pos = std::array::from_fn(|j| float(at + 28 + j * 4));
        if q.iter().chain(pos.iter()).any(|v| !v.is_finite())
            || (q.iter().map(|v| v * v).sum::<f32>() - 1.).abs() > 0.001
        {
            return Err("Invalid wheel transform".into());
        }
        let (q, pos) = adjusted(q, pos, name.starts_with("FL") || name.starts_with("BL"), s);
        let bytes = q
            .into_iter()
            .chain(pos)
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>();
        p.patch(at + 12, &bytes)?;
        spans.push(at + 12..at + 40);
    }
    if found.len() != 4 {
        return Err(
            t("build-this-car-s-skeleton-is-unsupported").into(),
        );
    }
    if before
        .iter()
        .zip(&p.image)
        .enumerate()
        .any(|(i, (a, b))| a != b && !spans.iter().any(|r| r.contains(&i)))
    {
        return Err("Unexpected non-wheel edit".into());
    }
    p.save(output)?;
    let check = UpkPackage::load(output)?;
    for e in &p.exports {
        let c = &check.exports[e.table_index];
        if p.image[e.serial_offset..e.serial_offset + e.serial_size]
            != check.image[c.serial_offset..c.serial_offset + c.serial_size]
        {
            return Err("Package verification failed".into());
        }
    }
    Ok(())
}
fn ensure_closed() -> Result<(), String> {
    if hebnix_sdk::process::is_rocket_league_running() {
        Err("Close Rocket League before changing alignment".into())
    } else {
        Ok(())
    }
}
fn record_dir(cooked: &Path, file: &str) -> PathBuf {
    cooked.join("Backups").join("WheelAlignment").join(file)
}

fn package_file(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let current = dir.join(format!("{name}.upk.bak"));
    let legacy = dir.join(format!("{name}.upk"));
    if legacy.exists() {
        if current.exists() {
            return Err(format!(
                "Both legacy and current Wheel Alignment files exist: {} and {}",
                legacy.display(),
                current.display()
            ));
        }
        fs::rename(&legacy, &current).map_err(|e| {
            format!(
                "Could not migrate {} to the .upk.bak backup convention: {e}",
                legacy.display()
            )
        })?;
    }
    Ok(current)
}

fn migrate_legacy_package_files(cooked: &Path) -> Result<(), String> {
    let root = cooked.join("Backups").join("WheelAlignment");
    let Ok(entries) = fs::read_dir(&root) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir())
            || entry.file_name().to_string_lossy().starts_with(".removed-")
        {
            continue;
        }
        for name in ["baseline", "candidate", "previous"] {
            package_file(&entry.path(), name)?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TransactionAction {
    Apply,
    Revert,
    RevertAndForget,
}

fn transaction(
    cooked: &Path,
    file: &str,
    settings: Settings,
    action: TransactionAction,
) -> Result<String, String> {
    ensure_closed()?;
    settings.validate()?;
    if Path::new(file).components().count() != 1
        || !file.to_ascii_lowercase().ends_with(".upk")
        || file.contains(['/', '\\', ':'])
    {
        return Err("Invalid package filename".into());
    }
    // Migrate every car before opening one: any other legacy baseline.upk in the same
    // CookedPCConsole tree can still collide with this package's imports.
    migrate_legacy_package_files(cooked)?;
    let live = cooked.join(file);
    let current = read(&live)?;
    let current_hash = hash(&current);
    let dir = record_dir(cooked, file);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // A directory lock protects against another Hebnix instance applying concurrently.
    let lock = dir.join("operation.lock");
    fs::create_dir(&lock).map_err(|_|"Alignment operation locked. If a previous run crashed, inspect its backup before removing operation.lock.")?;
    struct Lock(PathBuf);
    impl Drop for Lock {
        fn drop(&mut self) {
            let _ = fs::remove_dir(&self.0);
        }
    }
    let _lock = Lock(lock);
    // UPK payloads below CookedPCConsole must end in .upk.bak. A bare .upk can be
    // discovered as a second game package and make imports such as TAGame ambiguous.
    let baseline = package_file(&dir, "baseline")?;
    let metadata = dir.join("alignment.json");
    let stage = package_file(&dir, "candidate")?;
    let previous = package_file(&dir, "previous")?;
    let mut record = if metadata.exists() {
        let r: Record = serde_json::from_slice(&read(&metadata)?)
            .map_err(|e| format!("Invalid alignment backup: {e}"))?;
        if r.version != 1 || hash(&read(&baseline)?) != r.baseline_hash {
            return Err("Alignment backup failed validation".into());
        }
        if current_hash != r.installed_hash {
            return Err("This UPK changed outside Wheel Alignment. Refusing to overwrite a swap/custom body or game update. Restore that later patch first; alignment backup retained.".into());
        }
        r
    } else {
        if action != TransactionAction::Apply {
            return Err("No alignment backup exists for this car".into());
        }
        if baseline.exists() {
            return Err("Unpaired alignment backup found; retained for recovery".into());
        }
        // Validate support before creating the immutable baseline.
        build(&live, &stage, settings)?;
        write(&baseline, &current)?;
        let r = Record {
            version: 1,
            baseline_hash: current_hash.clone(),
            installed_hash: current_hash.clone(),
            settings: Settings::default(),
            adjustments: 0,
        };
        write_record(
            &metadata,
            &serde_json::to_vec_pretty(&r).map_err(|e| e.to_string())?,
        )?;
        r
    };
    let desired = if action != TransactionAction::Apply {
        read(&baseline)?
    } else {
        if settings == Settings::default() {
            read(&baseline)?
        } else {
            build(&baseline, &stage, settings)?;
            read(&stage)?
        }
    };
    ensure_closed()?;
    if hash(&read(&live)?) != current_hash {
        return Err("Package changed while preparing alignment; installation cancelled".into());
    }
    record.installed_hash = hash(&desired);
    record.settings = if action != TransactionAction::Apply {
        Settings::default()
    } else {
        settings
    };
    if action == TransactionAction::Apply && settings != Settings::default() {
        record.adjustments = record.adjustments.saturating_add(1);
    }
    let next = serde_json::to_vec_pretty(&record).map_err(|e| e.to_string())?;
    // Save the exact immediately preceding state as an additional recovery copy.
    write(&previous, &current)?;
    if let Err(error) = (|| {
        write(&live, &desired)?;
        if hash(&read(&live)?) != record.installed_hash {
            return Err("Installed hash mismatch".into());
        }
        write_record(&metadata, &next)
    })() {
        let rollback = write(&live, &current);
        return Err(format!(
            "{error}; rollback {}",
            if rollback.is_ok() {
                "completed"
            } else {
                "FAILED; use previous.upk.bak"
            }
        ));
    }
    if action == TransactionAction::RevertAndForget {
        // Rename first so the car disappears from the active record set atomically. A uniquely
        // named remnant is ignored by refresh_edited if Windows cannot remove it immediately.
        drop(_lock);
        let retired = dir.with_file_name(format!(
            ".removed-{file}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::rename(&dir, &retired)
            .map_err(|e| format!("Alignment reverted, but its record could not be removed: {e}"))?;
        if let Err(error) = fs::remove_dir_all(&retired) {
            return Ok(format!(
                "Alignment reverted and removed from the modified list. Old recovery files could not be deleted: {error}"
            ));
        }
        Ok(t("drop-alignment-reverted-and-removed-from-the").into())
    } else if action == TransactionAction::Revert {
        Ok(
            t("drop-alignment-reverted-to-captured-baseline")
                .into(),
        )
    } else {
        Ok(t("drop-alignment-applied-and-verified-test-stee").into())
    }
}

#[derive(Default)]
pub struct WheelAlignmentState {
    cars: Vec<(String, String)>,
    selected: usize,
    search: String,
    settings: Settings,
    status: String,
    receiver: Option<mpsc::Receiver<Result<String, String>>>,
    loaded: Option<PathBuf>,
    edited: Vec<(String, bool)>,
    edited_root: Option<PathBuf>,
}
impl WheelAlignmentState {
    fn refresh_edited(&mut self, cooked: &Path) {
        self.edited.clear();
        if let Ok(entries) = fs::read_dir(cooked.join("Backups").join("WheelAlignment")) {
            for entry in entries.flatten() {
                let file = entry.file_name().to_string_lossy().into_owned();
                if file.starts_with(".removed-") {
                    continue;
                }
                if let Ok(bytes) = read(&entry.path().join("alignment.json")) {
                    if let Ok(r) = serde_json::from_slice::<Record>(&bytes) {
                        if r.version == 1
                            && (r.adjustments > 0
                                || r.settings != Settings::default()
                                || r.installed_hash != r.baseline_hash)
                        {
                            self.edited
                                .push((file, r.installed_hash == r.baseline_hash));
                        }
                    }
                }
            }
        }
        self.edited.sort();
        self.edited_root = Some(cooked.to_path_buf());
    }
    pub fn set_catalog(&mut self, value: &serde_json::Value) {
        self.cars = value["bodies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| {
                Some((
                    v["name"].as_str()?.to_owned(),
                    v["upk_path"].as_str()?.to_owned(),
                ))
            })
            .collect();
        self.cars.sort();
        self.cars.dedup();
        self.selected = 0;
        self.loaded = None;
    }
    pub fn render(&mut self, ui: &mut egui::Ui, cooked: &Path) {
        if let Some(rx) = &self.receiver {
            match rx.try_recv() {
                Ok(result) => {
                    if result.is_ok() {
                        self.settings = Settings::default();
                        self.loaded = None;
                    }
                    self.status = result.unwrap_or_else(|e| format!("Error: {e}"));
                    self.receiver = None;
                    self.edited_root = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.status = t("render-alignment-worker-stopped-inspect-backup").into();
                    self.receiver = None;
                }
                _ => {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
        ui.heading(t("app-wheel-alignment"));
        if self.edited_root.as_deref() != Some(cooked) {
            self.refresh_edited(cooked);
        }
        let busy = self.receiver.is_some();
        let rl_open = hebnix_sdk::process::is_rocket_league_running();
        let mut action = None;
        ui.add_enabled_ui(!busy, |ui| {
            egui::ComboBox::from_id_salt("wheel_alignment_car")
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .selected_text(
                    self.cars
                        .get(self.selected)
                        .map(|c| c.0.as_str())
                        .unwrap_or("Load car catalog"),
                )
                .show_ui(ui, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.search).hint_text(t("render-filter-cars")));
                    ui.separator();
                    for (i, (name, _)) in self.cars.iter().enumerate() {
                        if name.to_lowercase().contains(&self.search.to_lowercase())
                            && ui.selectable_value(&mut self.selected, i, name).clicked()
                        {
                            ui.close();
                        }
                    }
                });
            let Some((_, file)) = self.cars.get(self.selected) else {
                ui.label(t("render-car-catalog-is-not-available-yet"));
                return;
            };
            let dir = record_dir(cooked, file);
            if self.loaded.as_ref() != Some(&dir) {
                self.settings = read(&dir.join("alignment.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
                    .map(|r| r.settings)
                    .unwrap_or_default();
                self.loaded = Some(dir.clone());
            }
            ui.add(
                egui::Slider::new(&mut self.settings.camber, -30.0..=30.0)
                    .step_by(0.5)
                    .text(t("render-camber-degrees")),
            );
            ui.weak(t("render-negative-tops-inward-positive-tops-outwa"));
            ui.add(
                egui::Slider::new(&mut self.settings.vertical, -15.0..=15.0)
                    .step_by(0.25)
                    .text(t("render-up-down")),
            );
            ui.weak(t("render-positive-up-negative-down-values-are"));
            ui.add(
                egui::Slider::new(&mut self.settings.lateral, -15.0..=15.0)
                    .step_by(0.25)
                    .text(t("render-left-right-track-width")),
            );
            ui.weak(
                t("render-mirrored-adjustment-positive-moves-both"),
            );
            if ui
                .add_enabled(!rl_open, egui::Button::new(t("render-apply-alignment")))
                .on_disabled_hover_text(t("render-close-rocket-league-before-changing-alig"))
                .clicked()
            {
                action = Some((file.clone(), self.settings, TransactionAction::Apply));
            }
            ui.separator();
            ui.heading(t("render-revert-settings"));
            if ui.button(t("render-revert-to-0")).clicked() {
                self.settings = Settings::default();
            }
            ui.label(t_args("render-edited-cars-edited", &[("edited", (self.edited.len()).to_string().into())]));
            egui::ScrollArea::vertical()
                .id_salt("alignment_edited_cars")
                .max_height(200.0)
                .show(ui, |ui| {
                    for (file, reverted) in &self.edited {
                        let index = self.cars.iter().position(|(_, path)| path == file);
                        let name = index.map(|i| self.cars[i].0.as_str()).unwrap_or(file);
                        let label = if *reverted {
                            format!("{name} — reverted")
                        } else {
                            name.to_string()
                        };
                        ui.horizontal(|ui| {
                            if ui
                                .selectable_label(index == Some(self.selected), label)
                                .clicked()
                            {
                                if let Some(i) = index {
                                    self.selected = i;
                                }
                            }
                            if ui
                                .add_enabled(!rl_open, egui::Button::new("X"))
                                .on_disabled_hover_text(
                                    t("render-close-rocket-league-before-restoring-ali"),
                                )
                                .clicked()
                            {
                                action = Some((
                                    file.clone(),
                                    Settings::default(),
                                    TransactionAction::RevertAndForget,
                                ));
                            }
                        });
                    }
                    if self.edited.is_empty() {
                        ui.weak(t("render-no-cars-adjusted-yet"));
                    }
                });
        });
        if let Some((file, settings, action)) = action {
            let cooked = cooked.to_path_buf();
            let (tx, rx) = mpsc::channel();
            self.receiver = Some(rx);
            self.status = t("render-preparing-alignment").into();
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let result = transaction(&cooked, &file, settings, action);
                let _ = tx.send(result);
                ctx.request_repaint();
            });
        }
        if busy {
            ui.spinner();
        }
        ui.label(&self.status);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_adjustments_and_baseline() {
        let q = [0., 0., 0., 1.];
        let p = [10., -20., -5.];
        assert_eq!(adjusted(q, p, true, Settings::default()), (q, p));
        let (left, l) = adjusted(
            q,
            p,
            true,
            Settings {
                camber: -15.,
                vertical: 2.,
                lateral: 3.,
            },
        );
        let (right, r) = adjusted(
            q,
            [10., 20., -5.],
            false,
            Settings {
                camber: -15.,
                vertical: 2.,
                lateral: 3.,
            },
        );
        assert_eq!(l, [10., -23., -3.]);
        assert_eq!(r, [10., 23., -3.]);
        assert!(left[0] < 0. && right[0] > 0.);
    }
    #[test]
    fn invalid_values_rejected() {
        assert!(
            Settings {
                camber: f32::NAN,
                ..Settings::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Settings {
                vertical: 16.,
                ..Settings::default()
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn legacy_package_files_migrate_to_backup_suffix() {
        let cooked = std::env::temp_dir().join(format!(
            "hebnix-wheel-names-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = cooked.join("Backups").join("WheelAlignment");
        let first = root.join("Body_First_SF.upk");
        let second = root.join("Body_Second_SF.upk");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir(&second).unwrap();
        fs::write(first.join("baseline.upk"), b"first").unwrap();
        fs::write(second.join("baseline.upk"), b"second").unwrap();
        fs::write(second.join("previous.upk"), b"previous").unwrap();
        migrate_legacy_package_files(&cooked).unwrap();
        assert_eq!(fs::read(first.join("baseline.upk.bak")).unwrap(), b"first");
        assert_eq!(
            fs::read(second.join("baseline.upk.bak")).unwrap(),
            b"second"
        );
        assert_eq!(
            fs::read(second.join("previous.upk.bak")).unwrap(),
            b"previous"
        );
        assert!(
            fs::read_dir(&root)
                .unwrap()
                .flat_map(|entry| fs::read_dir(entry.unwrap().path()).unwrap())
                .filter_map(Result::ok)
                .all(|entry| entry.path().extension().and_then(|v| v.to_str()) != Some("upk"))
        );
        fs::remove_dir_all(cooked).unwrap();
    }
    #[test]
    #[ignore = "Set HEBNIX_ALIGNMENT_FIXTURE to a body UPK; uses temporary copies only"]
    fn baseline_roundtrip_and_external_conflict() {
        let source = std::env::var_os("HEBNIX_ALIGNMENT_FIXTURE").expect("fixture path");
        let original = fs::read(source).unwrap();
        let root = std::env::temp_dir().join(format!(
            "hebnix-wheel-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let file = "Body_Test_SF.upk";
        let live = root.join(file);
        fs::write(&live, &original).unwrap();
        let s = Settings {
            camber: -5.,
            vertical: 1.,
            lateral: -2.,
        };
        transaction(&root, file, s, TransactionAction::Apply).unwrap();
        assert!(
            fs::read_dir(record_dir(&root, file))
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| entry.path().extension().and_then(|v| v.to_str()) != Some("upk")),
            "Wheel Alignment must not leave discoverable .upk files under CookedPCConsole"
        );
        let once = fs::read(&live).unwrap();
        assert_ne!(once, original);
        transaction(&root, file, s, TransactionAction::Apply).unwrap();
        assert_eq!(
            fs::read(&live).unwrap(),
            once,
            "reapplying the same values must not stack"
        );
        let replacement = Settings { camber: -8., ..s };
        transaction(&root, file, replacement, TransactionAction::Apply).unwrap();
        let expected = root.join("expected.upk");
        build(
            &record_dir(&root, file).join("baseline.upk.bak"),
            &expected,
            replacement,
        )
        .unwrap();
        assert_eq!(
            fs::read(&live).unwrap(),
            fs::read(expected).unwrap(),
            "-8 replaces the earlier angle"
        );
        transaction(&root, file, Settings::default(), TransactionAction::Apply).unwrap();
        assert_eq!(
            fs::read(&live).unwrap(),
            original,
            "zero restores the exact original baseline"
        );
        let r: Record =
            serde_json::from_slice(&read(&record_dir(&root, file).join("alignment.json")).unwrap())
                .unwrap();
        assert_eq!(r.adjustments, 3);
        assert_eq!(r.settings.camber, 0.);
        transaction(
            &root,
            file,
            Settings {
                camber: 10.,
                vertical: -1.,
                lateral: 2.,
            },
            TransactionAction::Apply,
        )
        .unwrap();
        transaction(&root, file, Settings::default(), TransactionAction::Revert).unwrap();
        assert_eq!(
            fs::read(&live).unwrap(),
            original,
            "exact custom baseline restored"
        );
        let mut state = WheelAlignmentState::default();
        state.refresh_edited(&root);
        assert_eq!(
            state.edited,
            vec![(file.to_string(), true)],
            "edited history survives revert and reload"
        );
        transaction(
            &root,
            file,
            Settings::default(),
            TransactionAction::RevertAndForget,
        )
        .unwrap();
        state.refresh_edited(&root);
        assert!(state.edited.is_empty());
        assert!(!record_dir(&root, file).exists());
        transaction(&root, file, s, TransactionAction::Apply).unwrap();
        fs::write(&live, b"a later swap or update").unwrap();
        assert!(
            transaction(&root, file, s, TransactionAction::Apply)
                .unwrap_err()
                .contains("outside Wheel Alignment")
        );
        assert!(
            transaction(&root, file, Settings::default(), TransactionAction::Revert,)
                .unwrap_err()
                .contains("outside Wheel Alignment")
        );
        assert_eq!(fs::read(&live).unwrap(), b"a later swap or update");
        assert_eq!(
            fs::read(record_dir(&root, file).join("baseline.upk.bak")).unwrap(),
            original
        );
        // Remove only this uniquely-created test directory, never the fixture.
        fs::remove_dir_all(root).unwrap();
    }
}
