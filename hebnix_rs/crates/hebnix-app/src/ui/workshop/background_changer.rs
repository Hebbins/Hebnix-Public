use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crossbeam_channel::Sender;
use eframe::egui;
use serde::Deserialize;

use crate::messages::AppMsg;

const ARENAS: &[(&str, &str)] = &[
    ("Stadium_P", "DFH Stadium"),
    ("Stadium_Day_P", "DFH Stadium (Day)"),
    ("Stadium_Foggy_P", "DFH Stadium (Stormy)"),
    ("Stadium_Winter_P", "DFH Stadium (Snowy)"),
    ("EuroStadium_P", "Mannfield"),
    ("EuroStadium_Night_P", "Mannfield (Night)"),
    ("EuroStadium_Dusk_P", "Mannfield (Dusk)"),
    ("EuroStadium_Rainy_P", "Mannfield (Stormy)"),
    ("EuroStadium_SnowNight_P", "Mannfield (Snowy)"),
    ("UtopiaStadium_P", "Utopia Coliseum"),
    ("UtopiaStadium_Dusk_P", "Utopia Coliseum (Dusk)"),
    ("UtopiaStadium_Snow_P", "Utopia Coliseum (Snowy)"),
    ("UtopiaStadium_Lux_P", "Utopia Coliseum (Gilded)"),
    ("TrainStation_P", "Urban Central"),
    ("TrainStation_Night_P", "Urban Central (Night)"),
    ("TrainStation_Dawn_P", "Urban Central (Dawn)"),
    ("Park_P", "Beckwith Park"),
    ("Park_Night_P", "Beckwith Park (Midnight)"),
    ("Park_Rainy_P", "Beckwith Park (Stormy)"),
    ("Park_Snowy_P", "Beckwith Park (Snowy)"),
    ("Outlaw_P", "Deadeye Canyon"),
    ("UF_Night_P", "Futura Garden (Night)"),
    ("Street_P", "Sovereign Heights"),
    ("Farm_P", "Farmstead"),
    ("Farm_Night_P", "Farmstead (Night)"),
    ("Farm_GRS_P", "Farmstead (Pitched)"),
    ("UF_Day_P", "Futura Garden (Day)"),
    ("Paname_Dusk_P", "Parc de Paris"),
    ("CS_P", "Champions Field"),
    ("CS_Day_P", "Champions Field (Day)"),
    ("Beach_P", "Salty Shores"),
    ("Beach_Night_P", "Salty Shores (Night)"),
    ("NeoTokyo_Standard_P", "Neo Tokyo"),
    ("Underwater_P", "AquaDome"),
    ("Wasteland_S_P", "Wasteland"),
    ("Wasteland_Night_S_P", "Wasteland (Night)"),
    ("CHN_Stadium_P", "Forbidden Temple"),
    ("CHN_Stadium_Day_P", "Forbidden Temple (Day)"),
    ("ARC_Standard_P", "Starbase ARC"),
    ("Music_P", "Neon Fields"),
    ("Woods_P", "Drift Woods"),
    ("Woods_Night_P", "Drift Woods (Night)"),
    ("Mall_Day_P", "Boostfield Mall"),
    ("FF_Dusk_P", "Estadio Vida"),
];

const SCENERY_DONORS: &[(&str, &str)] = &[
    ("ShatterShot_VFX", "Core 707 — scenery"),
    (
        "BG_Stadium_10A_P",
        "DFH Stadium (10th Anniversary) — scenery",
    ),
    ("BG_NeoTokyo_Arcade", "Neo Tokyo (Arcade) — scenery"),
    ("BG_NeoTokyo_Hax", "Neo Tokyo (Hacked) — scenery"),
    ("BG_Woods_Day_P", "Drift Woods (Day) — scenery"),
    ("UtopiaStadium_P", "Utopia Coliseum — scenery"),
    ("BG_FNI_Stadium", "Forbidden Temple (Fire & Ice) — scenery"),
];
const NO_BACKGROUND: &str = "__HBNX_NO_BACKGROUND__";

#[derive(Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SwapState {
    donor: String,
}

pub struct BackgroundChangerState {
    host: String,
    donor: String,
    host_search: String,
    donor_search: String,
    installed_hosts: Vec<usize>,
    installed_donors: Vec<(&'static str, &'static str)>,
    active: HashMap<String, SwapState>,
    last_rl_path: String,
    busy: bool,
    status: String,
}

impl Default for BackgroundChangerState {
    fn default() -> Self {
        Self {
            host: String::new(),
            donor: String::new(),
            host_search: String::new(),
            donor_search: String::new(),
            installed_hosts: Vec::new(),
            installed_donors: Vec::new(),
            active: HashMap::new(),
            last_rl_path: String::new(),
            busy: false,
            status:
                "Choose the arena you want to play, then choose the fog, sky, and background to borrow."
                    .to_string(),
        }
    }
}

impl BackgroundChangerState {
    fn cooked_dir(rl_path: &str) -> PathBuf {
        Path::new(rl_path).join("TAGame").join("CookedPCConsole")
    }

    fn state_dir() -> PathBuf {
        crate::config::base_dir()
            .join("state")
            .join("background_changer")
    }

    fn manifest_path() -> PathBuf {
        Self::state_dir().join("background_swaps.json")
    }

    fn display_name(package: &str) -> &str {
        if package == NO_BACKGROUND {
            return "No background";
        }
        ARENAS
            .iter()
            .chain(SCENERY_DONORS.iter())
            .find_map(|(pkg, display)| (*pkg == package).then_some(*display))
            .unwrap_or(package)
    }

    fn message_name(package: &str) -> &str {
        let display = Self::display_name(package);
        display.strip_suffix(" — scenery").unwrap_or(display)
    }

    fn refresh(&mut self, rl_path: &str) {
        self.last_rl_path = rl_path.to_string();
        let cooked = Self::cooked_dir(rl_path);
        self.installed_hosts = ARENAS
            .iter()
            .enumerate()
            .filter_map(|(index, (package, _))| {
                (cooked.join(format!("{package}.upk")).is_file()
                    || cooked.join(format!("{package}.upk.hbnx_mapbak")).is_file())
                .then_some(index)
            })
            .collect();
        self.installed_hosts
            .sort_by_key(|index| ARENAS[*index].1.to_ascii_lowercase());
        // Approved packages are filtered into portable exterior-only layers.
        // Full arena sources never stream their gameplay geometry or collision.
        self.installed_donors = SCENERY_DONORS
            .iter()
            .copied()
            .filter(|(package, _)| {
                cooked.join(format!("{package}.upk")).is_file()
                    || cooked.join(format!("{package}.upk.hbnx_mapbak")).is_file()
            })
            .collect();
        self.installed_donors
            .sort_by_key(|(_, display)| display.to_ascii_lowercase());
        if !self
            .installed_hosts
            .iter()
            .any(|index| ARENAS[*index].0 == self.host)
        {
            self.host = self
                .installed_hosts
                .first()
                .map(|index| ARENAS[*index].0)
                .unwrap_or("")
                .to_string();
        }
        if !self
            .installed_donors
            .iter()
            .any(|(package, _)| *package == self.donor)
            && self.donor != NO_BACKGROUND
        {
            self.donor = self
                .installed_donors
                .first()
                .map(|(package, _)| *package)
                .unwrap_or("")
                .to_string();
        }
        self.active = std::fs::read(Self::manifest_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
    }

    fn launch(
        &mut self,
        command: &'static str,
        rl_path: &str,
        host: Option<String>,
        donor: Option<String>,
        tx: &Sender<AppMsg>,
        ctx: &egui::Context,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.status = match command {
            "apply" => "Applying fog, sky, and background…".to_string(),
            "remove" => "Removing fog, sky, and background…".to_string(),
            "undo" => "Restoring the original arena…".to_string(),
            _ => "Restoring all original arenas…".to_string(),
        };
        let cooked = Self::cooked_dir(rl_path);
        let state = Self::state_dir();
        let tx = tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = crate::patcher::background_merger::run(
                command,
                &cooked,
                &state,
                host.as_deref(),
                donor.as_deref(),
            )
            .map(
                |helper_message| match (command, host.as_deref(), donor.as_deref()) {
                    ("apply", Some(host), Some(donor)) => format!(
                        "{} now uses {}'s fog, sky, and background.",
                        Self::message_name(host),
                        Self::message_name(donor)
                    ),
                    ("undo", Some(host), _) => format!(
                        "Restored {}'s original background.",
                        Self::message_name(host)
                    ),
                    _ => helper_message,
                },
            );
            let _ = tx.send(AppMsg::BackgroundChangerDone(result));
            ctx.request_repaint();
        });
    }

    pub fn finish(&mut self, result: Result<String, String>) -> String {
        self.busy = false;
        self.refresh(&self.last_rl_path.clone());
        match result {
            Ok(message) => {
                self.status = message.clone();
                format!("[Maps] {message}")
            }
            Err(error) => {
                self.status = error.clone();
                format!("[Maps] Background changer failed: {error}")
            }
        }
    }

    pub fn render(&mut self, ui: &mut egui::Ui, rl_path: &str, tx: &Sender<AppMsg>) {
        if self.last_rl_path != rl_path {
            self.refresh(rl_path);
        }
        let ctx = ui.ctx().clone();
        ui.heading("Background Changer");
        ui.label("Keep an arena's gameplay and networking, but borrow another arena's fog, sky, buildings, and distant scenery.");
        ui.small("Approved sources are filtered to keep only sky, atmosphere, buildings, and distant scenery; donor arena geometry is removed.");
        ui.add_space(8.0);
        ui.colored_label(egui::Color32::from_rgb(230, 170, 60), "Close Rocket League before applying or restoring a background. Changes load when the game starts.");
        ui.add_space(12.0);

        if self.installed_hosts.is_empty() {
            ui.label(
                "No supported arena packages were found in the configured Rocket League folder.",
            );
            if ui.button("Scan Again").clicked() {
                self.refresh(rl_path);
            }
            return;
        }

        ui.group(|ui| {
            ui.set_min_width(520.0);
            ui.label("When you play:");
            egui::ComboBox::from_id_salt("background_host")
                .selected_text(Self::display_name(&self.host))
                .width(360.0)
                .height(320.0)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show_ui(ui, |ui| {
                    ui.set_min_height(300.0);
                    ui.horizontal(|ui| {
                        ui.label("Filter:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.host_search)
                                .hint_text("Search maps...")
                                .desired_width(180.0),
                        );
                        if ui.small_button("Clear").clicked() {
                            self.host_search.clear();
                        }
                    });
                    ui.separator();
                    let query = self.host_search.trim().to_ascii_lowercase();
                    let mut found = false;
                    for index in &self.installed_hosts {
                        let (package, display) = ARENAS[*index];
                        if query.is_empty()
                            || display.to_ascii_lowercase().contains(&query)
                            || package.to_ascii_lowercase().contains(&query)
                        {
                            found = true;
                            ui.selectable_value(&mut self.host, package.to_string(), display);
                        }
                    }
                    if !found {
                        ui.weak("No maps match the filter.");
                    }
                });
            ui.add_space(8.0);
            ui.label("Use the fog + sky + background from:");
            egui::ComboBox::from_id_salt("background_donor")
                .selected_text(Self::display_name(&self.donor))
                .width(360.0)
                .height(320.0)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show_ui(ui, |ui| {
                    ui.set_min_height(300.0);
                    ui.horizontal(|ui| {
                        ui.label("Filter:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.donor_search)
                                .hint_text("Search backgrounds...")
                                .desired_width(180.0),
                        );
                        if ui.small_button("Clear").clicked() {
                            self.donor_search.clear();
                        }
                    });
                    ui.separator();
                    ui.selectable_value(
                        &mut self.donor,
                        NO_BACKGROUND.to_string(),
                        "No background",
                    );
                    ui.separator();
                    let query = self.donor_search.trim().to_ascii_lowercase();
                    let mut found = false;
                    for (package, display) in &self.installed_donors {
                        if query.is_empty()
                            || display.to_ascii_lowercase().contains(&query)
                            || package.to_ascii_lowercase().contains(&query)
                        {
                            found = true;
                            ui.selectable_value(&mut self.donor, (*package).to_string(), *display);
                        }
                    }
                    if !found {
                        ui.weak("No backgrounds match the filter.");
                    }
                });
            ui.add_space(10.0);
            let valid = !self.busy
                && !self.host.is_empty()
                && !self.donor.is_empty()
                && (self.donor == NO_BACKGROUND || self.host != self.donor);
            if ui
                .add_enabled(
                    valid,
                    egui::Button::new(if self.donor == NO_BACKGROUND {
                        "Remove Background"
                    } else {
                        "Apply Background"
                    }),
                )
                .clicked()
            {
                self.launch(
                    if self.donor == NO_BACKGROUND {
                        "remove"
                    } else {
                        "apply"
                    },
                    rl_path,
                    Some(self.host.clone()),
                    Some(self.donor.clone()),
                    tx,
                    &ctx,
                );
            }
            if self.host == self.donor && self.donor != NO_BACKGROUND {
                ui.small("Choose two different arenas.");
            }
        });

        ui.add_space(10.0);
        if self.busy {
            ui.spinner();
        }
        ui.label(&self.status);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.heading("Active Changes");
            if ui.button("Refresh").clicked() {
                self.refresh(rl_path);
            }
        });

        let mut restore = None;
        let mut rows: Vec<_> = self
            .active
            .iter()
            .map(|(host, swap)| (host.clone(), swap.donor.clone()))
            .collect();
        rows.sort_by(|a, b| Self::display_name(&a.0).cmp(Self::display_name(&b.0)));
        if rows.is_empty() {
            ui.label("No arena backgrounds are changed.");
        } else {
            egui::Grid::new("active_background_changes")
                .striped(true)
                .show(ui, |ui| {
                    ui.strong("Arena");
                    ui.strong("Borrowed background");
                    ui.end_row();
                    for (host, donor) in rows {
                        ui.label(Self::display_name(&host));
                        ui.label(Self::display_name(&donor));
                        if ui
                            .add_enabled(!self.busy, egui::Button::new("Restore"))
                            .clicked()
                        {
                            restore = Some(host);
                        }
                        ui.end_row();
                    }
                });
            ui.add_space(8.0);
            if ui
                .add_enabled(!self.busy, egui::Button::new("Restore All"))
                .clicked()
            {
                self.launch("reset", rl_path, None, None, tx, &ctx);
            }
        }
        if let Some(host) = restore {
            self.launch("undo", rl_path, Some(host), None, tx, &ctx);
        }
    }
}
