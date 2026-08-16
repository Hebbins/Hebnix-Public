use crate::config::Config;
use crate::messages::AppMsg;
use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatcherSubTab {
    Ball,
}

#[allow(dead_code)]
#[derive(Clone)]
pub struct BallItem {
    pub name: String,
    pub json_path: PathBuf,
    pub image_path: PathBuf,
    pub image_bytes: Option<Arc<[u8]>>,
}

#[derive(Deserialize)]
struct BallParams {
    #[serde(rename = "Diffuse")]
    diffuse: String,
}

#[derive(Deserialize)]
struct BallDef {
    #[serde(rename = "Group")]
    _group: Option<String>,
    #[serde(rename = "Params")]
    params: BallParams,
}

enum PatcherOp {
    Applied(String),
    Restored,
    Error(String),
}

pub struct PatcherState {
    base_dir: PathBuf,
    balls_dir: PathBuf,
    #[allow(dead_code)]
    subtab: PatcherSubTab,
    pub balls: Vec<BallItem>,
    pub active_ball: Option<String>,
    pub processing_target: Option<String>,
    pub search_input: String,
    pub search_filter: String,
    pub show_applied: bool,
    pub page: usize,
    local_tx: Sender<PatcherOp>,
    local_rx: Receiver<PatcherOp>,
    pub confirm_delete: Option<BallItem>,
}

impl PatcherState {
    pub fn new(base_dir: &Path, config: &Config) -> Self {
        let balls_dir = base_dir.join("balls");
        if !balls_dir.exists() {
            let _ = fs::create_dir_all(&balls_dir);
        }

        let (local_tx, local_rx) = crossbeam_channel::unbounded();

        let mut state = Self {
            base_dir: base_dir.to_path_buf(),
            balls_dir,
            subtab: PatcherSubTab::Ball,
            balls: Vec::new(),
            active_ball: config.patcher.active_ball.clone(),
            processing_target: None,
            search_input: String::new(),
            search_filter: String::new(),
            show_applied: false,
            page: 0,
            confirm_delete: None,
            local_tx,
            local_rx,
        };
        state.refresh_balls();
        state
    }

    pub fn refresh_balls(&mut self) {
        self.balls.clear();
        if !self.balls_dir.exists() {
            return;
        }

        let mut to_visit = vec![self.balls_dir.clone()];
        while let Some(dir) = to_visit.pop() {
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if path.is_dir() {
                        to_visit.push(path);
                    } else if path.extension().and_then(|s| s.to_str()) == Some("json") {
                        self.parse_ball_json(&path);
                    }
                }
            }
        }
    }

    fn parse_ball_json(&mut self, json_path: &Path) {
        if let Ok(content) = fs::read_to_string(json_path) {
            if let Ok(parsed) = serde_json::from_str::<HashMap<String, BallDef>>(&content) {
                for (name, def) in parsed {
                    if let Some(parent) = json_path.parent() {
                        let diffuse_name = &def.params.diffuse;
                        let mut img_path = parent.join(diffuse_name);

                        if !img_path.exists() {
                            let target_stem = Path::new(diffuse_name)
                                .file_stem()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_lowercase();

                            if let Ok(entries) = fs::read_dir(parent) {
                                for entry in entries.filter_map(|e| e.ok()) {
                                    let p = entry.path();
                                    if p.is_file() {
                                        if let Some(stem) = p.file_stem() {
                                            if stem.to_string_lossy().to_lowercase() == target_stem
                                            {
                                                img_path = p;
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        let bytes = if img_path.exists() {
                            fs::read(&img_path).ok().map(Arc::from)
                        } else {
                            None
                        };

                        if img_path.exists() {
                            self.balls.push(BallItem {
                                name,
                                json_path: json_path.to_path_buf(),
                                image_path: img_path,
                                image_bytes: bytes,
                            });
                        }
                    }
                }
            }
        }
    }

    fn spawn_restore_thread(
        &mut self,
        cooked_pc: &Path,
        _upk_path: &Path,
        backups_dir: &Path,
        tx: &Sender<AppMsg>,
        ctx: &egui::Context,
    ) {
        self.processing_target = Some("Global_Restore".to_string());

        let cooked_pc_clone = cooked_pc.to_path_buf();
        let backups_dir_clone = backups_dir.to_path_buf();
        let local_tx = self.local_tx.clone();
        let ctx_clone = ctx.clone();

        let _ = tx.send(AppMsg::Log(
            "[Patcher] Restoring original files...".to_string(),
        ));
        let tx_clone = tx.clone();

        std::thread::spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut restored = 0usize;
                let mut errors: Vec<String> = Vec::new();
                let mut total = 0usize;

                // The C# patcher touches every ball-bearing UPK, not only the
                // mutator and Soccar packages. Restore all corresponding
                // backups so changing/restoring a ball cannot leave stale
                // distance mips in arena or game-mode packages.
                if let Ok(entries) = fs::read_dir(&backups_dir_clone) {
                    for backup in entries
                        .filter_map(|entry| entry.ok())
                        .map(|entry| entry.path())
                    {
                        let Some(name) = backup.file_name().and_then(|name| name.to_str()) else {
                            continue;
                        };
                        let Some(live_name) = name.strip_suffix(".upk.bak") else {
                            continue;
                        };
                        let live_file_name = format!("{live_name}.upk");
                        if !crate::patch_core::gameinfo::is_ball_upk(&live_file_name) {
                            continue;
                        }
                        let live_path = cooked_pc_clone.join(&live_file_name);
                        match fs::copy(&backup, &live_path) {
                            Ok(_) => restored += 1,
                            Err(error) => errors.push(format!("{live_name}.upk: {error}")),
                        }
                    }
                }

                if let Ok(entries) = fs::read_dir(&backups_dir_clone) {
                    let bin_entries: Vec<PathBuf> = entries
                        .filter_map(|e| e.ok())
                        .map(|e| e.path())
                        .filter(|p| {
                            p.file_name()
                                .and_then(|name| name.to_str())
                                .is_some_and(crate::patch_core::standard_ball::is_ball_tfc_backup)
                        })
                        .collect();

                    total = bin_entries.len();

                    for (i, path) in bin_entries.into_iter().enumerate() {
                        if total > 0 && (i % 25 == 0 || i + 1 == total) {
                            let _ = tx_clone.send(AppMsg::Log(format!(
                                "[Patcher] Restoring texture cache: {}/{total}...",
                                i + 1
                            )));
                        }

                        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
                            continue;
                        };
                        let Some(stem) = filename.strip_suffix(".bin") else {
                            continue;
                        };
                        let Some((tfc_name, offset)) = stem.rsplit_once('_') else {
                            continue;
                        };
                        let Ok(offset) = offset.parse::<u64>() else {
                            continue;
                        };
                        let tfc_live_path = cooked_pc_clone.join(tfc_name);
                        if !tfc_live_path.exists() {
                            continue;
                        }

                        let step = (|| -> std::io::Result<()> {
                            let backup_bytes = fs::read(&path)?;
                            let mut live_file = std::fs::OpenOptions::new()
                                .write(true)
                                .open(&tfc_live_path)?;
                            std::io::Seek::seek(&mut live_file, std::io::SeekFrom::Start(offset))?;
                            std::io::Write::write_all(&mut live_file, &backup_bytes)?;
                            Ok(())
                        })();

                        match step {
                            Ok(_) => restored += 1,
                            Err(e) => errors.push(format!("{}: {e}", path.display())),
                        }
                    }
                }

                let _ = tx_clone.send(AppMsg::Log(format!(
                    "[Patcher] Texture cache restore pass finished: {restored}/{total} entries restored.",
                    total = total
                )));

                (restored, errors)
            }));

            match outcome {
                Ok((restored, errors)) => {
                    if restored > 0 {
                        let _ = local_tx.send(PatcherOp::Restored);
                        if !errors.is_empty() {
                            let _ = tx_clone.send(AppMsg::Log(format!(
                                "[Patcher] Restore completed with {} issue(s): {}",
                                errors.len(),
                                errors.join("; ")
                            )));
                        }
                    } else if !errors.is_empty() {
                        let _ = local_tx.send(PatcherOp::Error(format!(
                            "Restore failed: {}. The game may have these files open - close Rocket League and try again.",
                            errors.join("; ")
                        )));
                    } else {
                        let _ =
                            local_tx.send(PatcherOp::Error("No backups found to restore.".into()));
                    }
                }
                Err(_) => {
                    let _ = local_tx.send(PatcherOp::Error(
                        "Restore hit an unexpected internal error and was aborted.".into(),
                    ));
                }
            }
            ctx_clone.request_repaint();
        });
    }

    pub fn begin_restore(
        &mut self,
        cooked_pc: &Path,
        backups_dir: &Path,
        tx: &Sender<AppMsg>,
        ctx: &egui::Context,
    ) {
        let upk = cooked_pc.join("Mutators_Balls_SF.upk");
        self.spawn_restore_thread(cooked_pc, &upk, backups_dir, tx, ctx);
    }

    pub fn poll_ops(&mut self, tx: &Sender<AppMsg>, ctx: &egui::Context, config: &mut Config) {
        let mut received = false;
        while let Ok(op) = self.local_rx.try_recv() {
            received = true;
            self.processing_target = None;
            match op {
                PatcherOp::Applied(name) => {
                    self.active_ball = Some(name.clone());
                    config.patcher.active_ball = Some(name.clone());
                    let _ = tx.send(AppMsg::Log(format!(
                        "[Patcher] Successfully applied {name}!"
                    )));
                }
                PatcherOp::Restored => {
                    self.active_ball = None;
                    config.patcher.active_ball = None;
                    let _ = tx.send(AppMsg::Log(
                        "[Patcher] Restored original ball successfully.".into(),
                    ));
                }
                PatcherOp::Error(error) => {
                    let _ = tx.send(AppMsg::Log(format!("[Patcher] Error: {error}")));
                }
            }
            let _ = config.save(&self.base_dir);
        }
        if received {
            ctx.request_repaint();
        }
    }

    fn spawn_apply_thread(
        &mut self,
        ball: &BallItem,
        cooked_pc: &Path,
        upk_path: &Path,
        backups_dir: &Path,
        tx: &Sender<AppMsg>,
        ctx: &egui::Context,
    ) {
        self.processing_target = Some(ball.name.clone());
        let ball_name = ball.name.clone();
        let upk_clone = upk_path.to_path_buf();
        let cooked_clone = cooked_pc.to_path_buf();
        let backups_clone = backups_dir.to_path_buf();
        let img_clone = ball.image_path.clone();
        let local_tx = self.local_tx.clone();
        let ctx_clone = ctx.clone();
        let _ = tx.send(AppMsg::Log(format!("[Patcher] Patching {}...", ball.name)));
        std::thread::spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if !upk_clone.exists() {
                    return Err("Mutators_Balls_SF.upk not found in game directory.".to_string());
                }
                let img_bytes =
                    fs::read(&img_clone).map_err(|e| format!("Failed to read image: {e}"))?;
                crate::patch_core::mutators::patch_mutators(
                    &upk_clone.to_string_lossy(),
                    &cooked_clone.to_string_lossy(),
                    &backups_clone.to_string_lossy(),
                    &img_bytes,
                )?;
                crate::patch_core::standard_ball::patch_standard_tfcs(
                    &cooked_clone.to_string_lossy(),
                    &backups_clone.to_string_lossy(),
                    &img_bytes,
                )?;
                crate::patch_core::gameinfo::patch_ball_upks(
                    &cooked_clone.to_string_lossy(),
                    &backups_clone.to_string_lossy(),
                    &img_bytes,
                )?;
                Ok(())
            }));
            let op = match outcome {
                Ok(Ok(())) => PatcherOp::Applied(ball_name),
                Ok(Err(e)) => PatcherOp::Error(e),
                Err(_) => PatcherOp::Error("Fatal thread panic while patching ball".into()),
            };
            let _ = local_tx.send(op);
            ctx_clone.request_repaint();
        });
    }

    #[allow(dead_code)]
    pub fn render(
        &mut self,
        ui: &mut egui::Ui,
        rl_path: &str,
        tx: &Sender<AppMsg>,
        ctx: &egui::Context,
        config: &mut Config,
    ) {
        let mut received_patcher_op = false;

        while let Ok(op) = self.local_rx.try_recv() {
            received_patcher_op = true;
            self.processing_target = None;
            match op {
                PatcherOp::Applied(name) => {
                    self.active_ball = Some(name.clone());
                    config.patcher.active_ball = self.active_ball.clone();
                    let _ = config.save(&self.base_dir);
                    let _ = tx.send(AppMsg::Log(format!(
                        "[Patcher] Successfully applied {}!",
                        name
                    )));
                }
                PatcherOp::Restored => {
                    self.active_ball = None;
                    config.patcher.active_ball = None;
                    let _ = config.save(&self.base_dir);
                    let _ = tx.send(AppMsg::Log(
                        "[Patcher] Restored original ball successfully.".to_string(),
                    ));
                }
                PatcherOp::Error(e) => {
                    let _ = tx.send(AppMsg::Log(format!("[Patcher] Error: {}", e)));
                }
            }
        }

        if received_patcher_op {
            ctx.request_repaint();
        }

        let cooked_pc = Path::new(rl_path).join("TAGame").join("CookedPCConsole");
        let backups_dir = cooked_pc.join("Backups");
        let upk_path = cooked_pc.join("Mutators_Balls_SF.upk");

        egui::Panel::left("patcher_settings_list")
            .resizable(false)
            .exact_size(150.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("patcher_subtabs")
                    .show(ui, |ui| {
                        ui.selectable_value(&mut self.subtab, PatcherSubTab::Ball, "Ball");
                    });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ui, |ui| match self.subtab {
                PatcherSubTab::Ball => {
                    self.render_ball_tab(ui, &cooked_pc, &upk_path, &backups_dir, tx, ctx, config)
                }
            });

        if let Some(ball_to_delete) = self.confirm_delete.clone() {
            let mut close = false;
            egui::Window::new("Confirm Deletion")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Are you sure you want to delete '{}'?",
                        ball_to_delete.name
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Yes").clicked() {
                            let _ = fs::remove_file(&ball_to_delete.json_path);
                            let _ = fs::remove_file(&ball_to_delete.image_path);

                            if self.active_ball.as_deref() == Some(ball_to_delete.name.as_str()) {
                                self.active_ball = None;
                                config.patcher.active_ball = None;
                                let _ = config.save(&self.base_dir);
                            }

                            self.refresh_balls();
                            let _ = tx.send(AppMsg::Log(format!(
                                "[Patcher] Deleted ball '{}'",
                                ball_to_delete.name
                            )));
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

    pub fn render_ball_tab(
        &mut self,
        ui: &mut egui::Ui,
        cooked_pc: &Path,
        upk_path: &Path,
        backups_dir: &Path,
        tx: &Sender<AppMsg>,
        ctx: &egui::Context,
        config: &mut Config,
    ) {
        let mut received_patcher_op = false;

        while let Ok(op) = self.local_rx.try_recv() {
            received_patcher_op = true;
            self.processing_target = None;
            match op {
                PatcherOp::Applied(name) => {
                    self.active_ball = Some(name.clone());
                    config.patcher.active_ball = self.active_ball.clone();
                    let _ = config.save(&self.base_dir);
                    let _ = tx.send(AppMsg::Log(format!(
                        "[Patcher] Successfully applied {}!",
                        name
                    )));
                }
                PatcherOp::Restored => {
                    self.active_ball = None;
                    config.patcher.active_ball = None;
                    let _ = config.save(&self.base_dir);
                    let _ = tx.send(AppMsg::Log(
                        "[Patcher] Restored original ball successfully.".to_string(),
                    ));
                }
                PatcherOp::Error(e) => {
                    let _ = tx.send(AppMsg::Log(format!("[Patcher] Error: {}", e)));
                }
            }
        }

        if received_patcher_op {
            ctx.request_repaint();
        }

        ui.heading("Ball Patcher");
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.strong("Search:");
            let search_resp = ui.add(
                egui::TextEdit::singleline(&mut self.search_input)
                    .hint_text("Name or author...")
                    .desired_width(180.0),
            );
            let submitted =
                search_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Search").clicked() || submitted {
                self.search_filter = self.search_input.clone();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Refresh").clicked() {
                    self.refresh_balls();
                    let _ = tx.send(AppMsg::Log("[Patcher] Balls list refreshed.".to_string()));
                }

                let restore_enabled =
                    self.active_ball.is_some() && self.processing_target.is_none();
                if ui
                    .add_enabled(
                        restore_enabled,
                        egui::Button::new("Restore Original")
                            .fill(egui::Color32::from_rgb(180, 50, 50)),
                    )
                    .clicked()
                {
                    self.spawn_restore_thread(cooked_pc, upk_path, backups_dir, tx, ctx);
                }

                if ui
                    .add_enabled(
                        self.processing_target.is_none(),
                        egui::Button::new("Import ZIP"),
                    )
                    .clicked()
                {
                    if let Some(file) = rfd::FileDialog::new()
                        .add_filter("ZIP Archives", &["zip"])
                        .pick_file()
                    {
                        self.import_zip(&file, tx);
                    }
                }
                if ui
                    .checkbox(&mut self.show_applied, "Show Applied")
                    .changed()
                {
                    self.page = 0;
                }
            });
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);

        egui::ScrollArea::vertical()
            .id_salt("patcher_balls_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let query = self.search_filter.to_lowercase().trim().to_string();
                let filtered: Vec<BallItem> = self
                    .balls
                    .iter()
                    .filter(|b| {
                        b.name.to_lowercase().contains(&query)
                            && (!self.show_applied
                                || self.active_ball.as_deref() == Some(b.name.as_str()))
                    })
                    .cloned()
                    .collect();

                if filtered.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        if self.balls.is_empty() {
                            ui.label(
                                egui::RichText::new("No balls found in the /balls/ directory.")
                                    .color(egui::Color32::GRAY),
                            );
                        } else {
                            ui.label(
                                egui::RichText::new("No balls match your search.")
                                    .color(egui::Color32::GRAY),
                            );
                        }
                    });
                    return;
                }

                const PAGE_SIZE: usize = 20;
                let pages = filtered.len().div_ceil(PAGE_SIZE).max(1);
                self.page = self.page.min(pages - 1);
                ui.horizontal(|ui| {
                    ui.label(format!("Page {} of {}", self.page + 1, pages));
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
                let start = self.page * PAGE_SIZE;
                let visible: Vec<_> = filtered.into_iter().skip(start).take(PAGE_SIZE).collect();
                for row in visible.chunks(5) {
                    ui.columns(5, |columns| {
                        for (column, ball) in row.iter().enumerate() {
                            egui::Frame::group(columns[column].style()).show(
                                &mut columns[column],
                                |ui| {
                                    ui.set_min_height(190.0);
                                    ui.vertical_centered(|ui| {
                                        let size = egui::vec2(120.0, 90.0);
                                        if let Some(bytes) = &ball.image_bytes {
                                            ui.add(
                                                egui::Image::from_bytes(
                                                    format!("bytes://ball/{}", ball.name),
                                                    bytes.clone(),
                                                )
                                                .fit_to_exact_size(size),
                                            );
                                        } else {
                                            ui.add_sized(size, egui::Label::new("No Image"));
                                        }
                                        ui.strong(&ball.name);
                                        ui.add_space(5.0);
                                        let busy = self.processing_target.is_some();
                                        if self.processing_target.as_deref()
                                            == Some(ball.name.as_str())
                                        {
                                            ui.spinner();
                                        } else if self.active_ball.as_deref()
                                            == Some(ball.name.as_str())
                                        {
                                            if ui
                                                .add_enabled(
                                                    !busy,
                                                    egui::Button::new("Restore").min_size(
                                                        egui::vec2(ui.available_width(), 24.0),
                                                    ),
                                                )
                                                .clicked()
                                            {
                                                self.spawn_restore_thread(
                                                    cooked_pc,
                                                    upk_path,
                                                    backups_dir,
                                                    tx,
                                                    ctx,
                                                );
                                            }
                                        } else if ui
                                            .add_enabled(
                                                !busy,
                                                egui::Button::new("Apply").min_size(egui::vec2(
                                                    ui.available_width(),
                                                    24.0,
                                                )),
                                            )
                                            .clicked()
                                        {
                                            self.spawn_apply_thread(
                                                ball,
                                                cooked_pc,
                                                upk_path,
                                                backups_dir,
                                                tx,
                                                ctx,
                                            );
                                        }
                                        if ui
                                            .add_enabled(
                                                !busy,
                                                egui::Button::new("Delete")
                                                    .fill(egui::Color32::from_rgb(180, 50, 50))
                                                    .min_size(egui::vec2(
                                                        ui.available_width(),
                                                        24.0,
                                                    )),
                                            )
                                            .clicked()
                                        {
                                            self.confirm_delete = Some(ball.clone());
                                        }
                                    });
                                },
                            );
                        }
                    });
                    ui.add_space(6.0);
                }
            });
    }

    fn import_zip(&mut self, zip_path: &Path, tx: &Sender<AppMsg>) {
        let _ = tx.send(AppMsg::Log("[Patcher] Extracting ZIP...".to_string()));

        match (|| -> Result<(), String> {
            let file = fs::File::open(zip_path).map_err(|e| e.to_string())?;
            let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;

            let zip_stem = zip_path.file_stem().unwrap().to_string_lossy().to_string();
            let temp_dir = self.balls_dir.join(format!("temp_{}", zip_stem));

            fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
            archive.extract(&temp_dir).map_err(|e| e.to_string())?;

            let entries: Vec<_> = fs::read_dir(&temp_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .collect();
            if entries.len() == 1 && entries[0].file_type().unwrap().is_dir() {
                let inner_dir = entries[0].path();
                let dest = self.balls_dir.join(inner_dir.file_name().unwrap());
                let _ = fs::rename(&inner_dir, &dest);
                let _ = fs::remove_dir_all(&temp_dir);
            } else {
                let dest = self.balls_dir.join(&zip_stem);
                let _ = fs::rename(&temp_dir, &dest);
            }
            Ok(())
        })() {
            Ok(_) => {
                let _ = tx.send(AppMsg::Log("[Patcher] Imported successfully!".to_string()));
                self.refresh_balls();
            }
            Err(e) => {
                let _ = tx.send(AppMsg::Log(format!(
                    "[Patcher] Failed to import ZIP: {}",
                    e
                )));
            }
        }
    }
}
