//! Experimental animation speed for material-driven skins and decals. Scales the
//! pan (B,A) of `*Transform` vectors, every component of `*Panner` vectors and
//! `*Spinner`/`*Spin` scalars on MaterialInstanceConstants. Values are written
//! as `original * multiplier`, so repeat applies never compound.
use super::upk_package::{Prop, UpkPackage, strip};
use crate::config::SpeedPatchCfg;
use crate::i18n::{t, t_args};
use crate::messages::AppMsg;
use crossbeam_channel::Sender;
use eframe::egui;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const MIN_MULTIPLIER: f32 = 0.0;
pub const MAX_MULTIPLIER: f32 = 20.0;

pub fn clamp(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(MIN_MULTIPLIER, MAX_MULTIPLIER)
    } else {
        1.0
    }
}

/// True when a per-item speed is worth applying.
pub fn is_active(multiplier: f32) -> bool {
    (clamp(multiplier) - 1.0).abs() > 0.001
}

#[derive(Clone, Copy)]
enum Kind {
    Pan,
    All,
    Scalar,
}

fn vector_kind(name: &str) -> Option<Kind> {
    if name.ends_with("Panner") {
        Some(Kind::All)
    } else if name.ends_with("Transform") {
        Some(Kind::Pan)
    } else {
        None
    }
}

fn scalar_kind(name: &str) -> bool {
    name.ends_with("Spinner") || name.ends_with("Spin")
}

struct Slot {
    material: String,
    parameter: String,
    kind: Kind,
    offset: usize,
    values: [f32; 4],
}

fn name_at(package: &UpkPackage, prop: &Prop) -> Result<String, String> {
    if prop.size != 8 {
        return Err("Invalid parameter name".into());
    }
    let index = usize::try_from(package.read_int(prop.value_offset)?)
        .map_err(|_| "Negative parameter name index")?;
    package
        .names
        .get(index)
        .cloned()
        .ok_or_else(|| "Invalid parameter name index".into())
}

fn read_f32s(package: &UpkPackage, at: usize, n: usize) -> Result<[f32; 4], String> {
    let bytes = package
        .image
        .get(at..at + n * 4)
        .ok_or("Parameter value outside package")?;
    let mut out = [0.0f32; 4];
    for (i, chunk) in bytes.chunks_exact(4).enumerate() {
        out[i] = f32::from_le_bytes(chunk.try_into().unwrap());
    }
    Ok(out)
}

fn array_entries(
    package: &UpkPackage,
    props: &[Prop],
    base: usize,
    array_name: &str,
) -> Result<Vec<Vec<Prop>>, String> {
    let Some(array) = props
        .iter()
        .find(|p| p.name == array_name && p.tag_type == "ArrayProperty")
    else {
        return Ok(Vec::new());
    };
    if array.size < 4 {
        return Err(format!("Truncated {array_name}"));
    }
    let start = base + array.value_offset;
    let end = start.checked_add(array.size).ok_or("Array overflow")?;
    let count = package.read_int(start)?;
    if !(0..=1024).contains(&count) {
        return Err(format!("Invalid {array_name} count"));
    }
    let mut at = start + 4;
    let mut out = Vec::new();
    for _ in 0..count {
        let (entry, next) = package.nested_props(at, end)?;
        out.push(entry);
        at = next;
    }
    if at != end {
        return Err(format!("Unexpected bytes in {array_name}"));
    }
    Ok(out)
}

fn collect(package: &UpkPackage) -> Result<Vec<Slot>, String> {
    let mut slots = Vec::new();
    for export in &package.exports {
        if strip(&package.class_of(export)) != "MaterialInstanceConstant" {
            continue;
        }
        let material = strip(&package.name_of(export.object_name)).to_string();
        let (props, _) = package.serialized_props(export)?;
        for entry in array_entries(
            package,
            &props,
            export.serial_offset,
            "VectorParameterValues",
        )? {
            let Some(name) = entry.iter().find(|p| p.name == "ParameterName") else {
                continue;
            };
            let parameter = name_at(package, name)?;
            let Some(kind) = vector_kind(&parameter) else {
                continue;
            };
            let value = entry
                .iter()
                .find(|p| p.name == "ParameterValue" && p.tag_type == "StructProperty")
                .ok_or("Vector parameter has no value")?;
            if value.struct_name != "LinearColor" || value.size != 16 {
                return Err(format!("Unexpected layout for {parameter}"));
            }
            slots.push(Slot {
                material: material.clone(),
                parameter,
                kind,
                offset: value.value_offset,
                values: read_f32s(package, value.value_offset, 4)?,
            });
        }
        for entry in array_entries(
            package,
            &props,
            export.serial_offset,
            "ScalarParameterValues",
        )? {
            let Some(name) = entry.iter().find(|p| p.name == "ParameterName") else {
                continue;
            };
            let parameter = name_at(package, name)?;
            if !scalar_kind(&parameter) {
                continue;
            }
            let value = entry
                .iter()
                .find(|p| p.name == "ParameterValue" && p.tag_type == "FloatProperty")
                .ok_or("Scalar parameter has no value")?;
            slots.push(Slot {
                material: material.clone(),
                parameter,
                kind: Kind::Scalar,
                offset: value.value_offset,
                values: read_f32s(package, value.value_offset, 1)?,
            });
        }
    }
    Ok(slots)
}

fn scaled(kind: Kind, v: [f32; 4], m: f32) -> (Vec<u8>, usize) {
    // returns the new bytes and their byte offset into the value
    let out: Vec<f32> = match kind {
        Kind::Pan => vec![v[2] * m, v[3] * m],
        Kind::All => v.iter().map(|x| x * m).collect(),
        Kind::Scalar => vec![v[0] * m],
    };
    let skip = if matches!(kind, Kind::Pan) { 8 } else { 0 };
    (out.iter().flat_map(|x| x.to_le_bytes()).collect(), skip)
}

/// Speed-patch one package in place. `reference` is the pristine backup; when it
/// has the same materials, scaling starts from its values so the result does not
/// depend on how many times the package was patched before. Returns the number
/// of parameters changed.
pub fn apply(path: &Path, reference: Option<&Path>, multiplier: f32) -> Result<usize, String> {
    let multiplier = clamp(multiplier);
    let mut package = UpkPackage::load(path)?;
    let slots = collect(&package)?;
    if slots.is_empty() {
        return Ok(0);
    }
    let originals: HashMap<(String, String), [f32; 4]> = match reference {
        Some(r) if r.is_file() => UpkPackage::load(r)
            .and_then(|p| collect(&p))
            .map(|s| {
                s.into_iter()
                    .map(|s| ((s.material, s.parameter), s.values))
                    .collect()
            })
            .unwrap_or_default(),
        _ => HashMap::new(),
    };
    let mut patches: Vec<(usize, Vec<u8>)> = Vec::new();
    for slot in &slots {
        let source = originals
            .get(&(slot.material.clone(), slot.parameter.clone()))
            .copied()
            .unwrap_or(slot.values);
        let (bytes, skip) = scaled(slot.kind, source, multiplier);
        let finite = bytes
            .chunks_exact(4)
            .all(|b| f32::from_le_bytes(b.try_into().unwrap()).is_finite());
        if finite {
            patches.push((slot.offset + skip, bytes));
        }
    }
    for (offset, bytes) in &patches {
        package.patch(*offset, bytes)?;
    }
    let temporary = path.with_extension("speed.tmp");
    let result = (|| {
        package.save(&temporary)?;
        let check = UpkPackage::load(&temporary)?;
        for export in &package.exports {
            let start = export.serial_offset;
            let end = start + export.serial_size;
            if check.image.get(start..end) != package.image.get(start..end) {
                return Err(t("speed-patch-roundtrip-mismatch"));
            }
        }
        std::fs::copy(&temporary, path).map_err(|e| e.to_string())?;
        Ok(patches.len())
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

/// Compact speed picker used on the Items page when the experiment is enabled.
pub fn speed_slider(ui: &mut egui::Ui, speed: &mut f32) {
    ui.horizontal(|ui| {
        ui.label(t("speed-patch-multiplier"));
        ui.add(
            egui::Slider::new(speed, MIN_MULTIPLIER..=MAX_MULTIPLIER)
                .suffix("x")
                .step_by(0.25),
        );
        if ui.small_button("1x").clicked() {
            *speed = 1.0;
        }
        if ui.small_button("5x").clicked() {
            *speed = 5.0;
        }
    });
}

enum Job {
    Patch,
    Restore,
}

type JobResult = Result<String, String>;

/// Experimental tab: the Items page switch plus a one-off package patcher.
pub struct State {
    package: Option<PathBuf>,
    speed: f32,
    running: bool,
    rx: Option<crossbeam_channel::Receiver<JobResult>>,
    status: Option<JobResult>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            package: None,
            speed: 5.0,
            running: false,
            rx: None,
            status: None,
        }
    }
}

impl State {
    /// Returns true when the saved settings changed.
    pub fn render(
        &mut self,
        ui: &mut egui::Ui,
        cfg: &mut SpeedPatchCfg,
        cooked_pc: &Path,
        backups_dir: &Path,
        tx: &Sender<AppMsg>,
    ) -> bool {
        if let Some(rx) = &self.rx {
            if let Ok(result) = rx.try_recv() {
                let (Ok(message) | Err(message)) = &result;
                let _ = tx.send(AppMsg::Log(format!("[Speed] {message}")));
                self.status = Some(result);
                self.running = false;
                self.rx = None;
            } else {
                ui.ctx().request_repaint();
            }
        }
        let mut changed = false;
        ui.heading(t("speed-patch-title"));
        ui.weak(t("speed-patch-description"));
        ui.add_space(8.0);
        changed |= ui
            .checkbox(&mut cfg.items_page, t("speed-patch-items-page"))
            .changed();
        ui.weak(t("speed-patch-items-page-hint"));
        ui.add_space(12.0);
        ui.separator();
        ui.strong(t("speed-patch-oneoff-title"));
        ui.weak(t("speed-patch-oneoff-hint"));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button(t("speed-patch-choose")).clicked() {
                if let Some(file) = rfd::FileDialog::new()
                    .set_directory(cooked_pc)
                    .add_filter("UPK", &["upk"])
                    .pick_file()
                {
                    self.package = Some(file);
                    self.status = None;
                }
            }
            match &self.package {
                Some(path) => ui.label(path.display().to_string()),
                None => ui.weak(t("speed-patch-no-package")),
            };
        });
        speed_slider(ui, &mut self.speed);
        ui.add_space(4.0);
        let ready = self.package.is_some() && !self.running;
        let mut job = None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ready, egui::Button::new(t("speed-patch-patch")))
                .clicked()
            {
                job = Some(Job::Patch);
            }
            if ui
                .add_enabled(ready, egui::Button::new(t("app-restore")))
                .clicked()
            {
                job = Some(Job::Restore);
            }
            if self.running {
                ui.spinner();
            }
        });
        match &self.status {
            Some(Ok(message)) => {
                ui.colored_label(egui::Color32::from_rgb(46, 204, 113), message);
            }
            Some(Err(message)) => {
                ui.colored_label(egui::Color32::from_rgb(0xe7, 0x4c, 0x3c), message);
            }
            None => {}
        }
        if let (Some(job), Some(package)) = (job, self.package.clone()) {
            if crate::messages::block_item_action_if_game_running(tx) {
                return changed;
            }
            let (sender, receiver) = crossbeam_channel::bounded(1);
            self.rx = Some(receiver);
            self.running = true;
            self.status = None;
            let cooked = cooked_pc.to_path_buf();
            let backups = backups_dir.to_path_buf();
            let speed = self.speed;
            std::thread::spawn(move || {
                let _ = sender.send(run_job(job, &package, &cooked, &backups, speed));
            });
        }
        changed
    }
}

fn run_job(
    job: Job,
    package: &Path,
    cooked_pc: &Path,
    backups_dir: &Path,
    speed: f32,
) -> JobResult {
    let name = package
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid package path")?
        .to_string();
    if package.parent() != Some(cooked_pc) {
        return Err(t_args(
            "speed-patch-outside-cooked",
            &[("folder", cooked_pc.display().to_string().into())],
        ));
    }
    let backup = backups_dir.join(format!("{name}.bak"));
    match job {
        Job::Patch => {
            crate::patcher::backup_guard::synchronize_install(cooked_pc, backups_dir)?;
            std::fs::create_dir_all(backups_dir).map_err(|e| e.to_string())?;
            if !backup.is_file() {
                std::fs::copy(package, &backup)
                    .map_err(|e| format!("Failed to back up {name}: {e}"))?;
            }
            let count = apply(package, Some(&backup), speed)?;
            Ok(if count == 0 {
                t_args("speed-patch-nothing", &[("file", name.into())])
            } else {
                t_args(
                    "speed-patch-done",
                    &[
                        ("file", name.into()),
                        ("count", (count as i64).into()),
                        ("speed", format!("{:.2}", clamp(speed)).into()),
                    ],
                )
            })
        }
        Job::Restore => {
            crate::patcher::backup_guard::check_install(cooked_pc, backups_dir)?;
            if !backup.is_file() {
                return Err(t_args("speed-patch-no-backup", &[("file", name.into())]));
            }
            std::fs::copy(&backup, package)
                .map_err(|e| format!("Failed to restore {name}: {e}"))?;
            Ok(t_args("speed-patch-restored", &[("file", name.into())]))
        }
    }
}
