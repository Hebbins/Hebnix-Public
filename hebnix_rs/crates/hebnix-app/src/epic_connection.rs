use std::time::Duration;

use crossbeam_channel::{Receiver, unbounded};
use eframe::egui;

#[derive(Default)]
pub struct RepairState {
    pub confirm: bool,
    pub running: bool,
    pub result: Option<Result<(), String>>,
    completion: Option<Receiver<Result<(), String>>>,
}

impl RepairState {
    pub fn begin(&mut self, ctx: &egui::Context) {
        if self.running {
            return;
        }
        if hebnix_sdk::process::is_rocket_league_running() {
            self.confirm = true;
        } else {
            self.run(ctx);
        }
    }

    fn run(&mut self, ctx: &egui::Context) {
        self.confirm = false;
        self.result = None;
        self.running = true;
        let (tx, rx) = unbounded();
        self.completion = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = close_rocket_league().and_then(|()| repair());
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        if let Some(result) = self.completion.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.running = false;
            self.completion = None;
            self.result = Some(result);
        }
        if self.confirm {
            egui::Window::new("Fix Epic Connection")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(
                        "Rocket League must close to repair the Epic connection. Close it now?",
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Yes").clicked() {
                            self.run(ctx);
                        }
                        if ui.button("No").clicked() {
                            self.confirm = false;
                        }
                    });
                });
        }
        if let Some(result) = &self.result {
            let title = if result.is_ok() {
                "Epic Connection Repaired"
            } else {
                "Fix Epic Connection"
            };
            let mut close = false;
            egui::Window::new(title)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    if let Err(error) = result {
                        ui.label(format!("Could not repair the Epic connection: {error}"));
                    } else {
                        ui.label("Epic Connection Repaired");
                    }
                    if ui.button("OK").clicked() {
                        close = true;
                    }
                });
            if close {
                self.result = None;
            }
        }
    }
}

fn repair() -> Result<(), String> {
    crate::hosts_file::clear()
        .map_err(|error| format!("Could not clear Hebnix hosts redirects: {error}"))?;
    crate::winutil::clear_rocket_league_web_cache()
        .map_err(|error| format!("Could not clear Rocket League WebCache: {error}"))?;
    use std::os::windows::process::CommandExt;
    let status = std::process::Command::new("ipconfig")
        .arg("/flushdns")
        .creation_flags(0x08000000)
        .status()
        .map_err(|error| format!("Could not flush DNS: {error}"))?;
    if !status.success() {
        return Err(format!("DNS flush exited with {status}"));
    }
    Ok(())
}

fn close_rocket_league() -> Result<(), String> {
    if !hebnix_sdk::process::is_rocket_league_running() {
        return Ok(());
    }
    crate::winutil::kill_rocket_league()
        .map_err(|error| format!("Could not close Rocket League: {error}"))?;
    for _ in 0..60 {
        if !hebnix_sdk::process::is_rocket_league_running() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err("Rocket League did not close within 30 seconds".into())
}
