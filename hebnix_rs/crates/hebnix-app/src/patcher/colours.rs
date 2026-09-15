use aes::Aes256;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};
use crossbeam_channel::Receiver;
use eframe::egui;
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
<<<<<<< Updated upstream
use std::io::{Read, Write};
=======
use std::io::{Read, Seek, SeekFrom, Write};
>>>>>>> Stashed changes
use std::path::{Path, PathBuf};

const UPK_MAGIC: u32 = 2_653_586_369;
const BLOCK_SIZE: usize = 131_072;
const BACKUP_NAME: &str = "TAGame.upk.bak";
const STATE_NAME: &str = "active_colours.json";
<<<<<<< Updated upstream
=======
const BOOST_ARENA_BACKUP_NAME: &str = "boost_pickup_materials.json";
const BOOST_TEXTURE_BACKUP_NAME: &str = "boost_pickup_textures.json";
>>>>>>> Stashed changes

const BLUE_DEFAULT: &str = "39b4c83d79e9e63e0000803f0000803f9487053ffbcb4e3f79e9663f0000803f79e9663f79e9663f0000803f0000803f";
const BLUE_COLOUR_BLIND: &str = "cdcccc3d0000803ecdcc4c3f0000803fcdcccc3d6666263f0000803f0000803f6666663f0000803f0000803f0000803f";
const ORANGE_DEFAULT: &str = "61c3433f70cec83e39b4c83d0000803f79e9663f7cf2703e39b4c83d0000803f26e4633f26e4633f26e4633f0000803f";
const ORANGE_COLOUR_BLIND: &str = "cdcc4c3f6666e63ecdcccc3d0000803f0000803f6666263f000000000000803f0000803f0000803f6666663f0000803f";

<<<<<<< Updated upstream
=======
#[cfg(test)]
const BOOST_ARENA_PACKAGES: &[&str] = &[
    "Stadium_P",
    "Stadium_Day_P",
    "Stadium_Foggy_P",
    "Stadium_Winter_P",
    "EuroStadium_P",
    "EuroStadium_Night_P",
    "EuroStadium_Dusk_P",
    "EuroStadium_Rainy_P",
    "EuroStadium_SnowNight_P",
    "UtopiaStadium_P",
    "UtopiaStadium_Dusk_P",
    "UtopiaStadium_Snow_P",
    "UtopiaStadium_Lux_P",
    "TrainStation_P",
    "TrainStation_Night_P",
    "TrainStation_Dawn_P",
    "Park_P",
    "Park_Night_P",
    "Park_Rainy_P",
    "Park_Snowy_P",
    "Outlaw_P",
    "UF_Night_P",
    "Street_P",
    "Farm_P",
    "Farm_Night_P",
    "Farm_GRS_P",
    "UF_Day_P",
    "Paname_Dusk_P",
    "CS_P",
    "CS_Day_P",
    "Beach_P",
    "Beach_Night_P",
    "NeoTokyo_Standard_P",
    "Underwater_P",
    "Wasteland_S_P",
    "Wasteland_Night_S_P",
    "CHN_Stadium_P",
    "CHN_Stadium_Day_P",
    "ARC_Standard_P",
    "Music_P",
    "Woods_P",
    "Woods_Night_P",
    "Mall_Day_P",
    "FF_Dusk_P",
];

>>>>>>> Stashed changes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColourAction {
    Apply,
    Restore,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ColourSettings {
    pub stadium_blue: [u8; 3],
    pub stadium_orange: [u8; 3],
    pub hud_blue: [u8; 3],
    pub hud_orange: [u8; 3],
    pub extended_palette: bool,
    pub applied: bool,
}

<<<<<<< Updated upstream
=======
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BoostArenaManifest {
    packages: Vec<BoostArenaBackup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BoostArenaBackup {
    file: String,
    guid: [u8; 16],
    values: Vec<BoostMaterialValue>,
    #[serde(default)]
    emitter_values: Vec<BoostEmitterValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct BoostEmitterValue {
    key: String,
    object_ref: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BoostTextureManifest {
    regions: Vec<BoostTextureBackup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BoostTextureBackup {
    tfc: String,
    file_size: u64,
    offset: u64,
    size: usize,
    backup_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct BoostMaterialValue {
    export: String,
    parameter: String,
    instance: i32,
    rgb: [u8; 12],
}

>>>>>>> Stashed changes
impl Default for ColourSettings {
    fn default() -> Self {
        Self {
            stadium_blue: [25, 115, 255],
            stadium_orange: [195, 100, 25],
            hud_blue: [0, 46, 191],
            hud_orange: [179, 61, 0],
            extended_palette: false,
            applied: false,
        }
    }
}

pub struct ColoursState {
    pub settings: ColourSettings,
    busy: bool,
    status: String,
    error: bool,
    result_rx: Option<Receiver<Result<ColourAction, String>>>,
    loaded_backups_dir: Option<PathBuf>,
}

impl ColoursState {
    pub fn new() -> Self {
        Self {
            settings: ColourSettings::default(),
            busy: false,
<<<<<<< Updated upstream
            status: "Choose the stadium and HUD colours, then apply them together.".into(),
=======
            status: "Choose the stadium, HUD and garage palette colours, then apply them together."
                .into(),
>>>>>>> Stashed changes
            error: false,
            result_rx: None,
            loaded_backups_dir: None,
        }
    }

    fn load_active(&mut self, backups_dir: &Path) {
        if self.loaded_backups_dir.as_deref() == Some(backups_dir) {
            return;
        }
        let mut settings: ColourSettings = fs::read(backups_dir.join(STATE_NAME))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        if !backups_dir.join(BACKUP_NAME).is_file() {
            settings.applied = false;
        }
        self.settings = settings;
        self.loaded_backups_dir = Some(backups_dir.to_path_buf());
    }

    pub fn render(&mut self, ui: &mut egui::Ui, backups_dir: &Path) -> Option<ColourAction> {
        self.load_active(backups_dir);
        let mut requested = None;
        let restore_available = backups_dir.join(BACKUP_NAME).is_file();
        egui::ScrollArea::vertical()
            .id_salt("colours_page")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.heading("Colours");
                ui.label("Stadium, HUD and garage palette colours on one page.");
                ui.add_space(12.0);

                ui.vertical(|ui| {
                    ui.heading("Stadium colours");
                    ui.weak("Banners, flags and field lines.");
                    colour_row(ui, "Blue team", &mut self.settings.stadium_blue);
                    colour_row(ui, "Orange team", &mut self.settings.stadium_orange);
                    ui.horizontal(|ui| {
                        if ui.button("Defaults").clicked() {
                            self.settings.stadium_blue = [25, 115, 255];
                            self.settings.stadium_orange = [195, 100, 25];
                        }
                        if ui.button("Swap teams").clicked() {
                            std::mem::swap(
                                &mut self.settings.stadium_blue,
                                &mut self.settings.stadium_orange,
                            );
                        }
                    });
                });

                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.heading("HUD colours");
                    ui.weak("Boost meter and scoreboard.");
                    colour_row(ui, "Blue team", &mut self.settings.hud_blue);
                    colour_row(ui, "Orange team", &mut self.settings.hud_orange);
                    ui.horizontal(|ui| {
                        if ui.button("Match stadium").clicked() {
                            self.settings.hud_blue = self.settings.stadium_blue;
                            self.settings.hud_orange = self.settings.stadium_orange;
                        }
                        if ui.button("Defaults").clicked() {
                            self.settings.hud_blue = [0, 46, 191];
                            self.settings.hud_orange = [179, 61, 0];
                        }
                        if ui.button("Swap teams").clicked() {
                            std::mem::swap(
                                &mut self.settings.hud_blue,
                                &mut self.settings.hud_orange,
                            );
                        }
                    });
                });

                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.heading("Extended colour palette");
                    ui.checkbox(
                        &mut self.settings.extended_palette,
                        "Add pure white to pure black to the darkest garage row",
                    );
                    ui.weak(
                        "Only you see these colours. Disable this and apply again to put the original palette row back.",
                    );
                });

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Apply Colours"))
                        .clicked()
                    {
                        requested = Some(ColourAction::Apply);
                    }
                    if ui
                        .add_enabled(
                            !self.busy && (self.settings.applied || restore_available),
                            egui::Button::new("Restore Original"),
                        )
                        .clicked()
                    {
                        requested = Some(ColourAction::Restore);
                    }
                    if self.busy {
                        ui.spinner();
                    }
                });
                let colour = if self.error {
                    egui::Color32::from_rgb(0xe7, 0x4c, 0x3c)
                } else {
                    ui.visuals().text_color()
                };
                ui.colored_label(colour, &self.status);
                ui.weak(format!("Pristine backup: {BACKUP_NAME}"));
            });
        requested
    }

    pub fn begin(
        &mut self,
        action: ColourAction,
        cooked_pc: &Path,
        backups_dir: &Path,
        tx: &crossbeam_channel::Sender<crate::messages::AppMsg>,
        ctx: &egui::Context,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = false;
        self.status = match action {
<<<<<<< Updated upstream
            ColourAction::Apply => "Applying colours from the pristine backup…",
            ColourAction::Restore => "Restoring the original TAGame.upk…",
=======
            ColourAction::Apply => "Applying stadium, HUD and garage palette colours…",
            ColourAction::Restore => "Restoring original colours…",
>>>>>>> Stashed changes
        }
        .into();
        let cooked_pc = cooked_pc.to_path_buf();
        let backups_dir = backups_dir.to_path_buf();
        let settings = self.settings.clone();
        let (result_tx, result_rx) = crossbeam_channel::bounded(1);
        self.result_rx = Some(result_rx);
        let log_tx = tx.clone();
        let repaint = ctx.clone();
        std::thread::Builder::new()
            .name("colours-patcher".into())
            .spawn(move || {
                let result = match action {
                    ColourAction::Apply => apply(&cooked_pc, &backups_dir, &settings),
                    ColourAction::Restore => restore(&cooked_pc, &backups_dir),
                }
                .map(|_| action);
                let message = match &result {
                    Ok(ColourAction::Apply) => {
                        "[Colours] Colours applied. Start Rocket League to see them.".to_string()
                    }
                    Ok(ColourAction::Restore) => "[Colours] Original colours restored.".to_string(),
                    Err(error) => format!("[Colours] Error: {error}"),
                };
                let _ = log_tx.send(crate::messages::AppMsg::Log(message));
                let _ = result_tx.send(result);
                repaint.request_repaint();
            })
            .ok();
    }

    pub fn poll(&mut self) {
        let Some(rx) = &self.result_rx else { return };
        let Ok(result) = rx.try_recv() else { return };
        self.busy = false;
        match result {
            Ok(ColourAction::Apply) => {
                self.settings.applied = true;
                self.status = "Colours applied. Start Rocket League to see them.".into();
                self.error = false;
            }
            Ok(ColourAction::Restore) => {
                self.settings = ColourSettings::default();
<<<<<<< Updated upstream
                self.status = "Original stadium, HUD and garage colours restored.".into();
=======
                self.status = "Original stadium, HUD and garage palette colours restored.".into();
>>>>>>> Stashed changes
                self.error = false;
            }
            Err(error) => {
                self.status = error;
                self.error = true;
            }
        }
        self.result_rx = None;
    }
}

fn colour_row(ui: &mut egui::Ui, label: &str, colour: &mut [u8; 3]) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [110.0, 20.0],
            egui::Label::new(label).halign(egui::Align::Min),
        );
        ui.color_edit_button_srgb(colour);
        ui.monospace(format!(
            "#{:02X}{:02X}{:02X}",
            colour[0], colour[1], colour[2]
        ));
    });
}

pub fn needs_admin(cooked_pc: &Path) -> bool {
    matches!(
        fs::OpenOptions::new().write(true).open(cooked_pc.join("TAGame.upk")),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied
    )
}

fn save_state(backups_dir: &Path, settings: &ColourSettings) -> Result<(), String> {
    fs::create_dir_all(backups_dir)
        .map_err(|error| format!("Could not create the state directory: {error}"))?;
    let path = backups_dir.join(STATE_NAME);
    let temp = backups_dir.join(format!("{STATE_NAME}.tmp"));
    let bytes = serde_json::to_vec_pretty(settings).map_err(|error| error.to_string())?;
    fs::write(&temp, bytes).map_err(|error| format!("Could not save colour settings: {error}"))?;
    if path.is_file() {
        fs::remove_file(&path)
            .map_err(|error| format!("Could not replace colour settings: {error}"))?;
    }
    fs::rename(&temp, &path).map_err(|error| format!("Could not save colour settings: {error}"))
}

<<<<<<< Updated upstream
=======
fn load_boost_manifest(backups_dir: &Path) -> Result<BoostArenaManifest, String> {
    let path = backups_dir.join(BOOST_ARENA_BACKUP_NAME);
    if !path.is_file() {
        return Ok(BoostArenaManifest::default());
    }
    serde_json::from_slice(
        &fs::read(&path)
            .map_err(|error| format!("Could not read boost-pad restore data: {error}"))?,
    )
    .map_err(|error| format!("Boost-pad restore data is invalid: {error}"))
}

fn load_boost_texture_manifest(backups_dir: &Path) -> Result<BoostTextureManifest, String> {
    let path = backups_dir.join(BOOST_TEXTURE_BACKUP_NAME);
    if !path.is_file() {
        return Ok(BoostTextureManifest::default());
    }
    serde_json::from_slice(
        &fs::read(&path)
            .map_err(|error| format!("Could not read boost texture restore data: {error}"))?,
    )
    .map_err(|error| format!("Boost texture restore data is invalid: {error}"))
}

fn read_file_region(path: &Path, offset: u64, size: usize) -> Result<Vec<u8>, String> {
    let mut file = fs::File::open(path)
        .map_err(|error| format!("Could not open {}: {error}", path.display()))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| format!("Could not seek {}: {error}", path.display()))?;
    let mut bytes = vec![0u8; size];
    file.read_exact(&mut bytes)
        .map_err(|error| format!("Could not read {} at {offset}: {error}", path.display()))?;
    Ok(bytes)
}

fn write_file_region(path: &Path, offset: u64, bytes: &[u8]) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("Could not open {} for writing: {error}", path.display()))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| format!("Could not seek {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("Could not write {} at {offset}: {error}", path.display()))?;
    file.flush()
        .map_err(|error| format!("Could not flush {}: {error}", path.display()))
}

fn restore_boost_texture_colours(cooked_pc: &Path, backups_dir: &Path) -> Result<(), String> {
    let manifest_path = backups_dir.join(BOOST_TEXTURE_BACKUP_NAME);
    if !manifest_path.is_file() {
        return Ok(());
    }
    let manifest = load_boost_texture_manifest(backups_dir)?;
    for saved in &manifest.regions {
        let tfc_path = cooked_pc.join(&saved.tfc);
        let file_size = fs::metadata(&tfc_path)
            .map_err(|error| format!("Could not inspect {}: {error}", tfc_path.display()))?
            .len();
        if file_size != saved.file_size {
            return Err(format!(
                "{} changed after its boost textures were backed up; it was not restored.",
                saved.tfc
            ));
        }
        let backup_path = backups_dir.join(&saved.backup_file);
        let bytes = fs::read(&backup_path)
            .map_err(|error| format!("Could not read {}: {error}", backup_path.display()))?;
        if bytes.len() != saved.size {
            return Err(format!("{} has the wrong size", backup_path.display()));
        }
        write_file_region(&tfc_path, saved.offset, &bytes)?;
        if read_file_region(&tfc_path, saved.offset, saved.size)? != bytes {
            return Err(format!("{} did not restore exactly", saved.tfc));
        }
    }
    for saved in &manifest.regions {
        let _ = fs::remove_file(backups_dir.join(&saved.backup_file));
    }
    fs::remove_file(&manifest_path)
        .map_err(|error| format!("Could not remove restored boost texture data: {error}"))
}
fn restore_arena_boost_colours(cooked_pc: &Path, backups_dir: &Path) -> Result<(), String> {
    let path = backups_dir.join(BOOST_ARENA_BACKUP_NAME);
    if !path.is_file() {
        return Ok(());
    }
    let manifest = load_boost_manifest(backups_dir)?;
    for saved in &manifest.packages {
        let live = cooked_pc.join(&saved.file);
        let raw = fs::read(&live).map_err(|error| {
            format!(
                "Could not read {} while restoring boost-pad colours: {error}",
                saved.file
            )
        })?;
        if package_guid(&raw)? != saved.guid {
            return Err(format!(
                "{} changed after its boost-pad colours were backed up; it was not restored.",
                saved.file
            ));
        }
        let mut package = Package::load(raw)
            .map_err(|error| format!("Could not parse {} while restoring: {error}", saved.file))?;
        package
            .restore_active_boost_materials(&saved.values)
            .map_err(|error| format!("Could not restore boost pads in {}: {error}", saved.file))?;
        package
            .restore_active_boost_emitters(&saved.emitter_values)
            .map_err(|error| format!("Could not restore boost orbs in {}: {error}", saved.file))?;
        let temp = cooked_pc.join(format!("{}.boost-colours.restore.tmp", saved.file));
        fs::write(&temp, package.save()?)
            .map_err(|error| format!("Could not prepare {} for restore: {error}", saved.file))?;
        Package::load(fs::read(&temp).map_err(|error| {
            format!("Could not validate {} during restore: {error}", saved.file)
        })?)
        .map_err(|error| format!("Restored {} failed validation: {error}", saved.file))?;
        if let Err(error) = fs::copy(&temp, &live) {
            let _ = fs::remove_file(&temp);
            return Err(format!(
                "Could not restore active boost pads in {}: {error}",
                saved.file
            ));
        }
        let _ = fs::remove_file(&temp);
    }
    fs::remove_file(&path)
        .map_err(|error| format!("Could not remove restored boost-pad data: {error}"))
}

>>>>>>> Stashed changes
fn apply(cooked_pc: &Path, backups_dir: &Path, settings: &ColourSettings) -> Result<(), String> {
    let live = cooked_pc.join("TAGame.upk");
    if !live.is_file() {
        return Err(format!(
            "TAGame.upk was not found in {}",
            cooked_pc.display()
        ));
    }
    fs::create_dir_all(backups_dir)
        .map_err(|error| format!("Could not create the backup directory: {error}"))?;
<<<<<<< Updated upstream
=======
    // Clean up boost-pickup patches made by earlier builds. This path is kept
    // only for migration; the Colours tab no longer applies boost changes.
    restore_boost_texture_colours(cooked_pc, backups_dir)?;
    restore_arena_boost_colours(cooked_pc, backups_dir)?;
>>>>>>> Stashed changes
    let backup = backups_dir.join(BACKUP_NAME);
    let live_bytes = fs::read(&live).map_err(|error| error.to_string())?;
    if backup.is_file()
        && package_guid(&fs::read(&backup).map_err(|error| error.to_string())?)?
            != package_guid(&live_bytes)?
    {
        return Err(
            "Rocket League was updated and the colour backup belongs to the previous version.              Verify the game files, then remove the stale colour backup before applying again."
                .into(),
        );
    }
    if !backup.is_file() {
        fs::copy(&live, &backup).map_err(|error| {
            format!(
                "Could not create pristine backup at {}: {error}",
                backup.display()
            )
        })?;
    }
    let pristine = fs::read(&backup)
        .map_err(|error| format!("Could not read the pristine colour source: {error}"))?;
    let mut package = Package::load(pristine)?;
    package.apply_team_colours(settings)?;
    if settings.extended_palette {
        package.extend_palette()?;
    }
    let output = package.save()?;
    let temp = cooked_pc.join("TAGame.upk.colours.tmp");
    fs::write(&temp, &output)
        .map_err(|error| format!("Could not write the patched temporary file: {error}"))?;
    Package::load(fs::read(&temp).map_err(|error| error.to_string())?)
        .map_err(|error| format!("Patched file validation failed: {error}"))?;
    if let Err(error) = fs::copy(&temp, &live) {
        let _ = fs::copy(&backup, &live);
        let _ = fs::remove_file(&temp);
        return Err(format!(
            "Could not install the patched file; the pristine backup was restored: {error}"
        ));
    }
    let _ = fs::remove_file(&temp);
    let mut saved = settings.clone();
    saved.applied = true;
    save_state(backups_dir, &saved)
}

fn restore(cooked_pc: &Path, backups_dir: &Path) -> Result<(), String> {
    let live = cooked_pc.join("TAGame.upk");
    let backup = backups_dir.join(BACKUP_NAME);
    if !backup.is_file() {
        return Err(format!(
            "No colour backup was found at {}.",
            backup.display()
        ));
    }
    let original =
        fs::read(&backup).map_err(|error| format!("Could not read the colour backup: {error}"))?;
    Package::load(original.clone())
        .map_err(|error| format!("The colour backup is invalid: {error}"))?;
    if live.is_file()
        && package_guid(&original)?
            != package_guid(&fs::read(&live).map_err(|error| error.to_string())?)?
    {
        return Err(
            "The colour backup belongs to an older Rocket League version and was not restored."
                .into(),
        );
    }
<<<<<<< Updated upstream
=======
    restore_boost_texture_colours(cooked_pc, backups_dir)?;
    restore_arena_boost_colours(cooked_pc, backups_dir)?;
>>>>>>> Stashed changes
    let temp = cooked_pc.join("TAGame.upk.colours.restore.tmp");
    fs::write(&temp, &original)
        .map_err(|error| format!("Could not prepare the restore: {error}"))?;
    fs::copy(&temp, &live).map_err(|error| format!("Could not restore TAGame.upk: {error}"))?;
    let _ = fs::remove_file(temp);
    save_state(backups_dir, &ColourSettings::default())
}

#[derive(Clone, Copy)]
struct Chunk {
    u_off: usize,
    u_size: usize,
    c_off: usize,
    c_size: usize,
    table_offset: usize,
}

#[derive(Clone, Copy)]
struct FName {
    index: i32,
<<<<<<< Updated upstream
=======
    number: i32,
>>>>>>> Stashed changes
}

#[derive(Clone, Copy)]
struct Export {
    class_index: i32,
<<<<<<< Updated upstream
=======
    outer_index: i32,
>>>>>>> Stashed changes
    object_name: FName,
    serial_size: usize,
    serial_offset: usize,
}

struct Prop {
    name: String,
    size: usize,
    value_offset: usize,
}

<<<<<<< Updated upstream
=======
#[derive(Clone, Copy)]
enum BoostColourSource {
    Main,
    Secondary,
}

struct BoostMaterialTarget {
    export: &'static str,
    parameter: &'static str,
    instance: i32,
    offset: usize,
}

#[cfg_attr(not(test), allow(dead_code))]
struct BoostEmitterTarget {
    key: &'static str,
    offset: usize,
    replacement: i32,
}

>>>>>>> Stashed changes
struct Package {
    raw: Vec<u8>,
    key: [u8; 32],
    plain_header: Vec<u8>,
    image: Vec<u8>,
    chunks: Vec<Chunk>,
    names: Vec<String>,
    imports: Vec<FName>,
    exports: Vec<Export>,
    file_version: i32,
    total_header_size: usize,
    name_offset: usize,
    header_plain_end: usize,
    modified_chunks: HashSet<usize>,
}

impl Package {
    fn load(raw: Vec<u8>) -> Result<Self, String> {
        if read_u32(&raw, 0)? != UPK_MAGIC {
            return Err("TAGame.upk has an invalid package signature.".into());
        }
        let total_header_size = usize_from_i32(read_i32(&raw, 8)?, "total header size")?;
        let summary_end = fstring_end(&raw, 12)?;
        let name_offset = usize_from_i32(read_i32(&raw, summary_end + 8)?, "name offset")?;
        let depends_offset = read_i32(&raw, summary_end + 28)?;
        let keys = crate::upk_keys::embedded()?
            .into_iter()
            .map(|(_, key)| key)
            .collect::<Vec<_>>();
        let mut selected = None;
        for shift in [12usize, 0] {
            if name_offset < 12 + shift {
                continue;
            }
            let garbage =
                usize_from_i32(read_i32(&raw, name_offset - 12 - shift)?, "header padding")?;
            let chunk_table = usize_from_i32(
                read_i32(&raw, name_offset - 8 - shift)?,
                "chunk table offset",
            )?;
            let aligned = (total_header_size
                .checked_sub(garbage + name_offset)
                .ok_or("Invalid encrypted header size")?
                + 15)
                & !15;
            let encrypted = raw
                .get(name_offset..name_offset + aligned)
                .ok_or("Encrypted header is truncated")?;
            let probe_offset = chunk_table - chunk_table % 16;
            if probe_offset + 32 > encrypted.len() {
                continue;
            }
            for key in &keys {
                let probe = crypt(&encrypted[probe_offset..probe_offset + 32], key, false)?;
                let inner = chunk_table % 16;
                if inner + 8 <= probe.len()
                    && read_i32(&probe, inner).unwrap_or(0) >= 1
                    && read_i32(&probe, inner + 4).unwrap_or(-1) == depends_offset
                {
                    selected = Some((*key, crypt(encrypted, key, false)?, chunk_table, shift));
                    break;
                }
            }
            if selected.is_some() {
                break;
            }
        }
        let (key, plain_header, chunk_table, shift) =
            selected.ok_or("No embedded key could decrypt TAGame.upk")?;
        let chunk_stride = 24 + shift;
        let count = usize_from_i32(read_i32(&plain_header, chunk_table)?, "chunk count")?;
        let mut chunks = Vec::with_capacity(count);
        let mut row = chunk_table + 4;
        for _ in 0..count {
            chunks.push(Chunk {
                u_off: usize_from_i64(read_i64(&plain_header, row)?, "chunk virtual offset")?,
                u_size: usize_from_i32(read_i32(&plain_header, row + 8)?, "chunk size")?,
                c_off: usize_from_i64(read_i64(&plain_header, row + 12)?, "chunk file offset")?,
                c_size: usize_from_i32(
                    read_i32(&plain_header, row + 20)?,
                    "compressed chunk size",
                )?,
                table_offset: row,
            });
            row += chunk_stride;
        }
        let image_len = chunks
            .iter()
            .map(|chunk| chunk.u_off + chunk.u_size)
            .max()
            .ok_or("TAGame.upk has no compressed chunks")?;
        let mut image = vec![0u8; image_len];
        image[..name_offset].copy_from_slice(&raw[..name_offset]);
        let header_copy = plain_header.len().min(image.len() - name_offset);
        image[name_offset..name_offset + header_copy].copy_from_slice(&plain_header[..header_copy]);
        for chunk in &chunks {
            let data = decompress_chunk(&raw, *chunk)?;
            image[chunk.u_off..chunk.u_off + data.len()].copy_from_slice(&data);
        }

        let file_version = i32::from(u16::from_le_bytes(raw[4..6].try_into().unwrap()));
        let name_count = usize_from_i32(read_i32(&image, summary_end + 4)?, "name count")?;
        let export_count = usize_from_i32(read_i32(&image, summary_end + 12)?, "export count")?;
        let export_offset = usize_from_i32(read_i32(&image, summary_end + 16)?, "export offset")?;
        let import_count = usize_from_i32(read_i32(&image, summary_end + 20)?, "import count")?;
        let import_offset = usize_from_i32(read_i32(&image, summary_end + 24)?, "import offset")?;
        let mut names = Vec::with_capacity(name_count);
        let mut pos = name_offset;
        for _ in 0..name_count {
            let (name, next) = read_fstring(&image, pos)?;
            names.push(name);
            pos = next + 8;
        }
        let fname_size = if file_version >= 343 { 8 } else { 4 };
        let mut imports = Vec::with_capacity(import_count);
        pos = import_offset;
        for _ in 0..import_count {
            pos += fname_size * 2 + 4;
            let (object_name, next) = read_fname(&image, pos, file_version)?;
            imports.push(object_name);
            pos = next;
        }
        let mut exports = Vec::with_capacity(export_count);
        pos = export_offset;
        for _ in 0..export_count {
            let class_index = read_i32(&image, pos)?;
<<<<<<< Updated upstream
=======
            let outer_index = read_i32(&image, pos + 8)?;
>>>>>>> Stashed changes
            pos += 12;
            let (object_name, next) = read_fname(&image, pos, file_version)?;
            pos = next + 4 + 8;
            let serial_size = usize_from_i32(read_i32(&image, pos)?, "export size")?;
            let serial_offset = usize_from_i64(read_i64(&image, pos + 4)?, "export offset")?;
            pos += 12 + 4;
            let net_count = usize_from_i32(read_i32(&image, pos)?, "net object count")?;
            pos += 4 + net_count * 4 + 16 + 4;
            exports.push(Export {
                class_index,
<<<<<<< Updated upstream
=======
                outer_index,
>>>>>>> Stashed changes
                object_name,
                serial_size,
                serial_offset,
            });
        }
        let header_plain_end = name_offset + plain_header.len();
        Ok(Self {
            raw,
            key,
            plain_header,
            image,
            chunks,
            names,
            imports,
            exports,
            file_version,
            total_header_size,
            name_offset,
            header_plain_end,
            modified_chunks: HashSet::new(),
        })
    }

    fn name_of(&self, name: FName) -> Option<&str> {
        self.names
            .get(usize::try_from(name.index).ok()?)
            .map(String::as_str)
    }

    fn class_of(&self, export: Export) -> Option<&str> {
        if export.class_index < 0 {
            let import = self
                .imports
                .get(usize::try_from(-export.class_index - 1).ok()?)?;
            self.name_of(*import)
        } else if export.class_index > 0 {
            self.name_of(
                self.exports
                    .get(usize::try_from(export.class_index - 1).ok()?)?
                    .object_name,
            )
        } else {
            Some("Class")
        }
    }

    fn parse_props(&self, export: Export) -> Vec<Prop> {
        let Some(raw) = self
            .image
            .get(export.serial_offset..export.serial_offset + export.serial_size)
        else {
            return Vec::new();
        };
        let mut best = Vec::new();
        for start in 0..raw.len().saturating_sub(24) {
            let mut props = Vec::new();
            let mut pos = start;
            let mut seen = HashSet::new();
            for _ in 0..4096 {
                if !seen.insert(pos) {
                    break;
                }
                let Ok((name_ref, next)) = read_fname(raw, pos, self.file_version) else {
                    break;
                };
                let Some(name) = self.name_of(name_ref) else {
                    break;
                };
                if name == "None" {
                    break;
                }
                let Ok((type_ref, mut cursor)) = read_fname(raw, next, self.file_version) else {
                    break;
                };
                let Some(kind) = self.name_of(type_ref) else {
                    break;
                };
                if !valid_property(kind) {
                    break;
                }
                let Ok(size) =
                    read_i32(raw, cursor).and_then(|value| usize_from_i32(value, "property size"))
                else {
                    break;
                };
                cursor += 8;
                if kind == "StructProperty" || (kind == "ByteProperty" && self.file_version >= 633)
                {
                    let Ok((_, next)) = read_fname(raw, cursor, self.file_version) else {
                        break;
                    };
                    cursor = next;
                } else if kind == "BoolProperty" && self.file_version >= 673 {
                    cursor += 1;
                }
                if cursor + size > raw.len() {
                    break;
                }
                props.push(Prop {
                    name: name.to_string(),
                    size,
                    value_offset: cursor,
                });
                pos = cursor + size;
            }
            if props.len() > best.len() {
                best = props;
            }
        }
        best
    }

    fn apply_team_colours(&mut self, settings: &ColourSettings) -> Result<(), String> {
        let signatures = [
            (BLUE_DEFAULT, settings.stadium_blue, Some(settings.hud_blue)),
            (BLUE_COLOUR_BLIND, settings.stadium_blue, None),
            (
                ORANGE_DEFAULT,
                settings.stadium_orange,
                Some(settings.hud_orange),
            ),
            (ORANGE_COLOUR_BLIND, settings.stadium_orange, None),
        ];
        let mut found = 0;
        for (signature, stadium, hud) in signatures {
            let needle = hex::decode(signature).map_err(|error| error.to_string())?;
            let positions = find_all(&self.image, &needle);
            if let Some(offset) = positions.into_iter().find(|offset| {
                self.chunks
                    .iter()
                    .any(|chunk| *offset >= chunk.u_off && *offset < chunk.u_off + chunk.u_size)
            }) {
                if let Some(index) = self
                    .chunks
                    .iter()
                    .position(|chunk| offset >= chunk.u_off && offset < chunk.u_off + chunk.u_size)
                {
                    self.modified_chunks.insert(index);
                }
                write_colour_list(&mut self.image, offset, stadium)?;
                if let Some(hud) = hud {
                    write_rgb(
                        &mut self.image,
                        offset
                            .checked_sub(120)
                            .ok_or("HUD colour offset underflow")?,
                        hud,
                    )?;
                    write_rgb(
                        &mut self.image,
                        offset
                            .checked_sub(72)
                            .ok_or("HUD colour offset underflow")?,
                        hud,
                    )?;
                }
                found += 1;
            }
        }
        if found != 4 {
            return Err(format!(
                "Only {found}/4 team colour palettes were found.                  This Rocket League version may be unsupported."
            ));
        }
        Ok(())
    }

<<<<<<< Updated upstream
=======
    fn boost_material_targets(&self) -> Result<Vec<BoostMaterialTarget>, String> {
        let specifications = [
            (
                "BoostPad_Glowing_INST",
                "BoostPad_Color",
                0,
                BoostColourSource::Main,
                1.0,
            ),
            (
                "BoostPad_Scroll_INST",
                "BoostPad_ScrollColor",
                2,
                BoostColourSource::Main,
                1.0,
            ),
            (
                "BoostPad_Scroll_INST",
                "BoostPad_ScrollColor",
                3,
                BoostColourSource::Secondary,
                2.0,
            ),
        ];
        let mut targets = Vec::with_capacity(27);
        for (export_name, parameter, instance, _source, _intensity) in specifications {
            let export = self
                .exports
                .iter()
                .copied()
                .find(|export| {
                    self.name_of(export.object_name).map(strip_suffix) == Some(export_name)
                })
                .ok_or_else(|| format!("{export_name} was not found"))?;
            let vectors = self
                .parse_props(export)
                .into_iter()
                .find(|prop| prop.name == "VectorParameterValues")
                .ok_or_else(|| format!("{export_name}.VectorParameterValues was not found"))?;
            let start = export.serial_offset + vectors.value_offset;
            let end = start + vectors.size;
            let mut value_offset = None;
            if end >= start + 56 {
                for at in (start + 4..=end - 52).step_by(4) {
                    if self.raw_fname_matches(at, parameter, instance)
                        && self.raw_fname_matches(at + 8, "ParameterValue", 0)
                        && self.raw_fname_matches(at + 16, "StructProperty", 0)
                        && read_i32(&self.image, at + 24).ok() == Some(16)
                        && self.raw_fname_matches(at + 32, "LinearColor", 0)
                    {
                        value_offset = Some(at + 40);
                        break;
                    }
                }
            }
            targets.push(BoostMaterialTarget {
                export: export_name,
                parameter,
                instance,
                offset: value_offset.ok_or_else(|| {
                    format!("{export_name}.{parameter}_{instance} has changed layout")
                })?,
            });
        }

        for (instance, _intensity) in [(2, 7.0), (5, 10.0)] {
            let export = self
                .exports
                .iter()
                .copied()
                .find(|export| {
                    self.class_of(*export).map(strip_suffix) == Some("ParticleModuleColor")
                        && self.name_of(export.object_name).map(strip_suffix)
                            == Some("ParticleModuleColor")
                        && export.object_name.number == instance
                        && self.export_has_outer_named(*export, "BoostOrb_PS")
                })
                .ok_or_else(|| {
                    format!("BoostOrb_PS.ParticleModuleColor_{instance} was not found")
                })?;
            let start_colour = self
                .parse_props(export)
                .into_iter()
                .find(|prop| prop.name == "StartColor" && prop.size == 96)
                .ok_or_else(|| {
                    format!("BoostOrb_PS.ParticleModuleColor_{instance}.StartColor changed layout")
                })?;
            let start = export.serial_offset + start_colour.value_offset;
            if !self.raw_fname_matches(start, "Distribution", 0)
                || !self.raw_fname_matches(start + 8, "ObjectProperty", 0)
                || read_i32(&self.image, start + 16).ok() != Some(4)
                || !self.raw_fname_matches(start + 28, "LookupTable", 0)
                || !self.raw_fname_matches(start + 36, "ArrayProperty", 0)
                || read_i32(&self.image, start + 44).ok() != Some(36)
                || read_i32(&self.image, start + 52).ok() != Some(8)
            {
                return Err(format!(
                    "BoostOrb_PS.ParticleModuleColor_{instance}.StartColor lookup table changed layout"
                ));
            }
            for (parameter, offset) in [
                ("StartColorLookupA", start + 64),
                ("StartColorLookupB", start + 76),
            ] {
                targets.push(BoostMaterialTarget {
                    export: "ParticleModuleColor",
                    parameter,
                    instance,
                    offset,
                });
            }
        }

        let cone = self
            .exports
            .iter()
            .copied()
            .find(|export| {
                self.class_of(*export).map(strip_suffix)
                    == Some("MaterialExpressionVectorParameter")
                    && self.name_of(export.object_name).map(strip_suffix)
                        == Some("MaterialExpressionVectorParameter")
                    && export.object_name.number == 1
                    && self.export_has_outer_named(*export, "BoostPad_LightCone_03_Mat")
            })
            .ok_or("BoostPad_LightCone_03_Mat colour expression was not found")?;
        let cone_colour = self
            .parse_props(cone)
            .into_iter()
            .find(|prop| prop.name == "DefaultValue" && prop.size == 16)
            .ok_or("BoostPad_LightCone_03_Mat colour expression changed layout")?;
        targets.push(BoostMaterialTarget {
            export: "MaterialExpressionVectorParameter",
            parameter: "BoostPadLightConeDefaultValue",
            instance: 1,
            offset: cone.serial_offset + cone_colour.value_offset,
        });

        for (parameter, _source, _intensity) in [
            ("ColorA", BoostColourSource::Main, 0.88),
            ("ColorB", BoostColourSource::Secondary, 1.0),
            ("ColorC", BoostColourSource::Main, 1.0),
        ] {
            let expression = self
                .exports
                .iter()
                .copied()
                .find(|export| {
                    if self.class_of(*export).map(strip_suffix)
                        != Some("MaterialExpressionVectorParameter")
                        || !self.export_has_outer_named(*export, "BoostPad_02_Mat")
                    {
                        return false;
                    }
                    self.parse_props(*export)
                        .into_iter()
                        .find(|prop| prop.name == "ParameterName" && prop.size == 8)
                        .and_then(|prop| {
                            read_fname(
                                &self.image,
                                export.serial_offset + prop.value_offset,
                                self.file_version,
                            )
                            .ok()
                        })
                        .and_then(|(name, _)| self.name_of(name))
                        .map(strip_suffix)
                        == Some(parameter)
                })
                .ok_or_else(|| format!("BoostPad_02_Mat.{parameter} was not found"))?;
            let value = self
                .parse_props(expression)
                .into_iter()
                .find(|prop| prop.name == "DefaultValue" && prop.size == 16)
                .ok_or_else(|| format!("BoostPad_02_Mat.{parameter} changed layout"))?;
            targets.push(BoostMaterialTarget {
                export: "BoostPad_02_Mat",
                parameter,
                instance: 0,
                offset: expression.serial_offset + value.value_offset,
            });
        }

        for (material, parameter_name, key, _source, _intensity) in [
            (
                "BoostPad_Glowing_Mat",
                "BoostPad_Color",
                "ParentVectorDefault",
                BoostColourSource::Main,
                1.0f32,
            ),
            (
                "BoostPad_Scrolling_Mat",
                "BoostPad_ScrollColor",
                "ParentVectorDefault",
                BoostColourSource::Main,
                8.0,
            ),
        ] {
            let expression = self
                .exports
                .iter()
                .copied()
                .find(|export| {
                    if self.class_of(*export).map(strip_suffix)
                        != Some("MaterialExpressionVectorParameter")
                        || !self.export_has_outer_named(*export, material)
                    {
                        return false;
                    }
                    self.parse_props(*export)
                        .into_iter()
                        .find(|prop| prop.name == "ParameterName" && prop.size == 8)
                        .and_then(|prop| {
                            read_fname(
                                &self.image,
                                export.serial_offset + prop.value_offset,
                                self.file_version,
                            )
                            .ok()
                        })
                        .and_then(|(name, _)| self.name_of(name))
                        .map(strip_suffix)
                        == Some(parameter_name)
                })
                .ok_or_else(|| format!("{material}.{parameter_name} was not found"))?;
            let value = self
                .parse_props(expression)
                .into_iter()
                .find(|prop| prop.name == "DefaultValue" && prop.size == 16)
                .ok_or_else(|| format!("{material}.{parameter_name} changed layout"))?;
            targets.push(BoostMaterialTarget {
                export: material,
                parameter: key,
                instance: -1,
                offset: expression.serial_offset + value.value_offset,
            });
        }

        for (material, outer, parameter, _source, _intensity, relative_offsets) in [
            (
                "BoostOrb_2D_Mat",
                "Pickup_Boost",
                "BakedYellow",
                BoostColourSource::Main,
                1.0f32,
                &[0x471usize][..],
            ),
            (
                "BoostOrb_Glow_Mat",
                "Materials",
                "BakedYellow",
                BoostColourSource::Main,
                1.0,
                &[0x1e8usize][..],
            ),
            (
                "Glow01_Mat",
                "Mat",
                "BakedYellow",
                BoostColourSource::Main,
                1.0,
                &[0x200usize][..],
            ),
            (
                "Glow_Mat",
                "Materials",
                "BakedYellow",
                BoostColourSource::Main,
                1.0,
                &[0x236usize, 0x29a][..],
            ),
            (
                "BoostPad_Mat",
                "Materials",
                "BakedYellow",
                BoostColourSource::Main,
                1.0,
                &[0x302usize][..],
            ),
            (
                "BoostPad_Mat",
                "Materials",
                "BakedYellowHdr",
                BoostColourSource::Main,
                16.0,
                &[0x312usize][..],
            ),
            (
                "BoostPad_02_Mat",
                "Pickup_Boost",
                "BakedYellow",
                BoostColourSource::Main,
                1.0,
                &[0x2d3usize][..],
            ),
            (
                "BoostPad_LightCone_03_Mat",
                "Pickup_Boost",
                "BakedYellow",
                BoostColourSource::Secondary,
                1.0,
                &[0x308usize, 0x36c][..],
            ),
            (
                "BoostPad_Glowing_Mat",
                "QOL",
                "BakedYellow",
                BoostColourSource::Main,
                1.0,
                &[0x2c3usize, 0x327][..],
            ),
        ] {
            let export = self
                .exports
                .iter()
                .copied()
                .find(|export| {
                    self.class_of(*export).map(strip_suffix) == Some("Material")
                        && self.name_of(export.object_name).map(strip_suffix) == Some(material)
                        && self.export_has_outer_named(*export, outer)
                })
                .ok_or_else(|| format!("{outer}.{material} was not found"))?;
            for (instance, relative) in relative_offsets.iter().copied().enumerate() {
                if relative + 12 > export.serial_size {
                    return Err(format!(
                        "{outer}.{material}.{parameter}_{instance} changed layout"
                    ));
                }
                let offset = export.serial_offset + relative;
                let values = (0..3)
                    .map(|index| {
                        f32::from_le_bytes(
                            self.image[offset + index * 4..offset + index * 4 + 4]
                                .try_into()
                                .unwrap(),
                        )
                    })
                    .collect::<Vec<_>>();
                if values
                    .iter()
                    .any(|value| !value.is_finite() || *value < 0.0 || *value > 32.0)
                {
                    return Err(format!(
                        "{outer}.{material}.{parameter}_{instance} is not a valid colour vector"
                    ));
                }
                targets.push(BoostMaterialTarget {
                    export: material,
                    parameter,
                    instance: instance as i32,
                    offset,
                });
            }
        }

        let small_pad = self
            .exports
            .iter()
            .copied()
            .find(|export| {
                self.class_of(*export).map(strip_suffix) == Some("MaterialInstanceConstant")
                    && self.name_of(export.object_name).map(strip_suffix)
                        == Some("BoostPad_Small_MIC")
            })
            .ok_or("BoostPad_Small_MIC was not found")?;
        for (parameter, instance, relative, _intensity) in [
            ("BakedYellow", 0, 0x15dusize, 1.0f32),
            ("BakedYellowHdr", 0, 0x16dusize, 16.0f32),
        ] {
            if relative + 12 > small_pad.serial_size {
                return Err(format!("BoostPad_Small_MIC.{parameter} changed layout"));
            }
            let offset = small_pad.serial_offset + relative;
            let values = (0..3)
                .map(|index| {
                    f32::from_le_bytes(
                        self.image[offset + index * 4..offset + index * 4 + 4]
                            .try_into()
                            .unwrap(),
                    )
                })
                .collect::<Vec<_>>();
            if values
                .iter()
                .any(|value| !value.is_finite() || *value < 0.0 || *value > 32.0)
            {
                return Err(format!(
                    "BoostPad_Small_MIC.{parameter} is not a valid colour vector"
                ));
            }
            targets.push(BoostMaterialTarget {
                export: "BoostPad_Small_MIC",
                parameter,
                instance,
                offset,
            });
        }
        Ok(targets)
    }

    fn boost_emitter_targets(&self) -> Result<Vec<BoostEmitterTarget>, String> {
        let lod_ref = |instance: i32| -> Result<(usize, i32), String> {
            let emitter = self
                .exports
                .iter()
                .copied()
                .find(|export| {
                    self.class_of(*export).map(strip_suffix) == Some("ParticleSpriteEmitter")
                        && self.name_of(export.object_name).map(strip_suffix)
                            == Some("ParticleSpriteEmitter")
                        && export.object_name.number == instance
                        && self.export_has_ancestor_named(*export, "BoostOrb_PS")
                })
                .ok_or_else(|| {
                    format!("BoostOrb_PS.ParticleSpriteEmitter_{instance} was not found")
                })?;
            let levels = self
                .parse_props(emitter)
                .into_iter()
                .find(|prop| prop.name == "LODLevels" && prop.size == 8)
                .ok_or_else(|| {
                    format!("BoostOrb_PS.ParticleSpriteEmitter_{instance}.LODLevels changed layout")
                })?;
            let start = emitter.serial_offset + levels.value_offset;
            if read_i32(&self.image, start).ok() != Some(1) {
                return Err(format!(
                    "BoostOrb_PS.ParticleSpriteEmitter_{instance} no longer has one LOD"
                ));
            }
            let object_ref = read_i32(&self.image, start + 4)?;
            let lod_index = object_ref
                .checked_sub(1)
                .and_then(|index| usize::try_from(index).ok())
                .ok_or_else(|| {
                    format!("BoostOrb_PS.ParticleSpriteEmitter_{instance} has an invalid LOD")
                })?;
            let lod = self.exports.get(lod_index).copied().ok_or_else(|| {
                format!("BoostOrb_PS.ParticleSpriteEmitter_{instance} has an invalid LOD")
            })?;
            if self.class_of(lod).map(strip_suffix) != Some("ParticleLODLevel")
                || !self.export_has_ancestor_named(lod, "BoostOrb_PS")
            {
                return Err(format!(
                    "BoostOrb_PS.ParticleSpriteEmitter_{instance} has an unexpected LOD"
                ));
            }
            Ok((start + 4, object_ref))
        };

        let (yellow_orb, _) = lod_ref(1)?;
        let (yellow_glow, _) = lod_ref(2)?;
        let (_, coloured_orb) = lod_ref(3)?;
        let (_, coloured_glow) = lod_ref(8)?;
        Ok(vec![
            BoostEmitterTarget {
                key: "BoostOrbSolidPass",
                offset: yellow_orb,
                replacement: coloured_orb,
            },
            BoostEmitterTarget {
                key: "BoostOrbGlowPass",
                offset: yellow_glow,
                replacement: coloured_glow,
            },
        ])
    }
    fn export_has_outer_named(&self, export: Export, expected: &str) -> bool {
        if export.outer_index <= 0 {
            return false;
        }
        self.exports
            .get((export.outer_index - 1) as usize)
            .and_then(|outer| self.name_of(outer.object_name))
            .map(strip_suffix)
            == Some(expected)
    }
    fn export_has_ancestor_named(&self, export: Export, expected: &str) -> bool {
        let mut outer = export.outer_index;
        for _ in 0..16 {
            if outer <= 0 {
                return false;
            }
            let Some(parent) = self.exports.get((outer - 1) as usize) else {
                return false;
            };
            if self.name_of(parent.object_name).map(strip_suffix) == Some(expected) {
                return true;
            }
            outer = parent.outer_index;
        }
        false
    }
    fn raw_fname_matches(&self, offset: usize, expected: &str, instance: i32) -> bool {
        read_fname(&self.image, offset, self.file_version)
            .ok()
            .and_then(|(name, _)| self.name_of(name))
            .map(strip_suffix)
            == Some(expected)
            && read_i32(&self.image, offset + 4).ok() == Some(instance)
    }

    #[cfg(test)]
    fn capture_boost_material_values(&self) -> Result<Vec<BoostMaterialValue>, String> {
        self.boost_material_targets()?
            .into_iter()
            .map(|target| {
                let rgb = self
                    .image
                    .get(target.offset..target.offset + 12)
                    .ok_or("A boost-pad material colour is outside the package")?
                    .try_into()
                    .map_err(|_| "A boost-pad material colour has the wrong size")?;
                Ok(BoostMaterialValue {
                    export: target.export.into(),
                    parameter: target.parameter.into(),
                    instance: target.instance,
                    rgb,
                })
            })
            .collect()
    }

    #[cfg(test)]
    fn capture_boost_emitter_values(&self) -> Result<Vec<BoostEmitterValue>, String> {
        self.boost_emitter_targets()?
            .into_iter()
            .map(|target| {
                Ok(BoostEmitterValue {
                    key: target.key.into(),
                    object_ref: read_i32(&self.image, target.offset)?,
                })
            })
            .collect()
    }
    fn restore_active_boost_materials(
        &mut self,
        values: &[BoostMaterialValue],
    ) -> Result<(), String> {
        let targets = self.boost_material_targets()?;
        for value in values {
            let target = targets
                .iter()
                .find(|target| {
                    target.export == value.export
                        && target.parameter == value.parameter
                        && target.instance == value.instance
                })
                .ok_or_else(|| {
                    format!(
                        "{}.{}_{} was not found while restoring",
                        value.export, value.parameter, value.instance
                    )
                })?;
            self.patch_bytes(target.offset, &value.rgb)?;
        }
        Ok(())
    }

    fn restore_active_boost_emitters(
        &mut self,
        values: &[BoostEmitterValue],
    ) -> Result<(), String> {
        let targets = self.boost_emitter_targets()?;
        for value in values {
            let target = targets
                .iter()
                .find(|target| target.key == value.key)
                .ok_or_else(|| format!("{} was not found while restoring", value.key))?;
            self.patch_bytes(target.offset, &value.object_ref.to_le_bytes())?;
        }
        Ok(())
    }
    fn patch_bytes(&mut self, offset: usize, bytes: &[u8]) -> Result<(), String> {
        let end = offset
            .checked_add(bytes.len())
            .ok_or("Colour patch overflow")?;
        self.image
            .get_mut(offset..end)
            .ok_or("Colour patch is outside the package")?
            .copy_from_slice(bytes);
        let chunk = self
            .chunks
            .iter()
            .position(|chunk| offset >= chunk.u_off && end <= chunk.u_off + chunk.u_size)
            .ok_or("Colour patch is outside the package's compressed chunks")?;
        self.modified_chunks.insert(chunk);
        Ok(())
    }

>>>>>>> Stashed changes
    fn extend_palette(&mut self) -> Result<(), String> {
        let targets = ["BlueTeamV3", "OrangeTeamV3", "CustomTeam"];
        let mut sets = 0;
        let mut valid = 0;
        for export in self.exports.clone() {
            if self.class_of(export).map(strip_suffix) != Some("CarColorSet_TA") {
                continue;
            }
            let Some(object_name) = self.name_of(export.object_name) else {
                continue;
            };
            if !targets.contains(&strip_suffix(object_name)) {
                continue;
            }
            sets += 1;
            let props = self.parse_props(export);
            let hue = props.iter().find(|prop| prop.name == "HueCount");
            let values = props.iter().find(|prop| prop.name == "ValueCount");
            let colours = props.iter().find(|prop| prop.name == "Colors");
            let (Some(hue), Some(values), Some(colours)) = (hue, values, colours) else {
                continue;
            };
            let hue_count = usize_from_i32(
                read_i32(&self.image, export.serial_offset + hue.value_offset)?,
                "hue count",
            )?;
            let value_count = usize_from_i32(
                read_i32(&self.image, export.serial_offset + values.value_offset)?,
                "value count",
            )?;
            let colour_count = usize_from_i32(
                read_i32(&self.image, export.serial_offset + colours.value_offset)?,
                "colour count",
            )?;
            if hue_count == 0
                || value_count <= 1
                || colour_count != hue_count * value_count
                || colours.size != 4 + colour_count * 16
            {
                continue;
            }
            let row =
                export.serial_offset + colours.value_offset + 4 + (colour_count - hue_count) * 16;
            for index in 0..hue_count {
                let value = 1.0 - index as f32 / (hue_count - 1).max(1) as f32;
                let base = row + index * 16;
                for channel in 0..3 {
                    self.image[base + channel * 4..base + channel * 4 + 4]
                        .copy_from_slice(&value.to_le_bytes());
                }
                self.image[base + 12..base + 16].copy_from_slice(&1.0f32.to_le_bytes());
            }
            if let Some(index) = self
                .chunks
                .iter()
                .position(|chunk| row >= chunk.u_off && row < chunk.u_off + chunk.u_size)
            {
                self.modified_chunks.insert(index);
            }
            valid += 1;
        }
        if sets == 0 {
            return Err(
                "No car colour palettes were found. This Rocket League version is not supported yet."
                    .into(),
            );
        }
        if valid == 0 {
            return Err(
                "The car colour palette layout changed in this Rocket League version.".into(),
            );
        }
        Ok(())
    }

    fn save(&self) -> Result<Vec<u8>, String> {
        let mut rebuilt = Vec::with_capacity(self.chunks.len());
        for chunk in &self.chunks {
            if !self.modified_chunks.contains(&rebuilt.len()) {
                rebuilt.push(
                    self.raw
                        .get(chunk.c_off..chunk.c_off + chunk.c_size)
                        .ok_or("An original compressed chunk is truncated")?
                        .to_vec(),
                );
                continue;
            }
            rebuilt.push(pack_chunk(
                &self.image[chunk.u_off..chunk.u_off + chunk.u_size],
            )?);
        }
        let mut header = self.plain_header.clone();
        let mut next = self.total_header_size;
        let mut order = (0..self.chunks.len()).collect::<Vec<_>>();
        order.sort_by_key(|index| self.chunks[*index].c_off);
        for index in &order {
            let chunk = self.chunks[*index];
            write_i64(&mut header, chunk.table_offset + 12, next as i64)?;
            write_i32(
                &mut header,
                chunk.table_offset + 20,
                rebuilt[*index].len() as i32,
            )?;
            next += rebuilt[*index].len();
        }
        let encrypted = crypt(&header, &self.key, true)?;
        let mut output = Vec::with_capacity(next);
        output.extend_from_slice(&self.raw[..self.name_offset]);
        output.extend_from_slice(&encrypted);
        output.extend_from_slice(&self.raw[self.header_plain_end..self.total_header_size]);
        for index in order {
            output.extend_from_slice(&rebuilt[index]);
        }
        Ok(output)
    }
}

fn valid_property(kind: &str) -> bool {
    matches!(
        kind,
        "ByteProperty"
            | "IntProperty"
            | "BoolProperty"
            | "FloatProperty"
            | "ObjectProperty"
            | "NameProperty"
            | "DelegateProperty"
            | "ClassProperty"
            | "ArrayProperty"
            | "StructProperty"
            | "VectorProperty"
            | "RotatorProperty"
            | "StrProperty"
            | "MapProperty"
            | "FixedArrayProperty"
            | "InterfaceProperty"
            | "ComponentProperty"
            | "QWordProperty"
            | "PointerProperty"
            | "StringRefProperty"
            | "BioMask4Property"
            | "GuidProperty"
    )
}

fn strip_suffix(name: &str) -> &str {
    name.strip_suffix("_-2").unwrap_or(name)
}

fn pack_chunk(data: &[u8]) -> Result<Vec<u8>, String> {
    let blocks = data
        .chunks(BLOCK_SIZE)
        .map(|block| {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
            encoder
                .write_all(block)
                .map_err(|error| error.to_string())?;
            encoder.finish().map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut output = Vec::new();
    output.extend_from_slice(&UPK_MAGIC.to_le_bytes());
    output.extend_from_slice(&(BLOCK_SIZE as i32).to_le_bytes());
    output.extend_from_slice(&(blocks.iter().map(Vec::len).sum::<usize>() as i32).to_le_bytes());
    output.extend_from_slice(&(data.len() as i32).to_le_bytes());
    for (block, source) in blocks.iter().zip(data.chunks(BLOCK_SIZE)) {
        output.extend_from_slice(&(block.len() as i32).to_le_bytes());
        output.extend_from_slice(&(source.len() as i32).to_le_bytes());
    }
    for block in blocks {
        output.extend_from_slice(&block);
    }
    Ok(output)
}

fn decompress_chunk(raw: &[u8], chunk: Chunk) -> Result<Vec<u8>, String> {
    if read_u32(raw, chunk.c_off)? != UPK_MAGIC {
        return Err("A TAGame.upk compressed chunk has an invalid signature.".into());
    }
    let total = usize_from_i32(read_i32(raw, chunk.c_off + 12)?, "chunk uncompressed size")?;
    let mut header = chunk.c_off + 16;
    let mut uncompressed = 0usize;
    let mut rows = Vec::new();
    while uncompressed < total {
        let c_size = usize_from_i32(read_i32(raw, header)?, "block size")?;
        let u_size = usize_from_i32(read_i32(raw, header + 4)?, "block output size")?;
        rows.push((c_size, u_size));
        uncompressed += u_size;
        header += 8;
    }
    let mut output = Vec::with_capacity(chunk.u_size);
    for (c_size, u_size) in rows {
        let bytes = raw
            .get(header..header + c_size)
            .ok_or("Compressed block is truncated")?;
        let mut decoder = ZlibDecoder::new(bytes);
        let before = output.len();
        decoder
            .read_to_end(&mut output)
            .map_err(|error| error.to_string())?;
        if output.len() - before != u_size {
            return Err("A compressed block expanded to the wrong size.".into());
        }
        header += c_size;
    }
    if output.len() != chunk.u_size {
        return Err("A compressed chunk expanded to the wrong size.".into());
    }
    Ok(output)
}

fn write_colour_list(image: &mut [u8], offset: usize, colour: [u8; 3]) -> Result<(), String> {
    for index in 0..3 {
        write_rgb(image, offset + index * 16, colour)?;
    }
    Ok(())
}

fn write_rgb(image: &mut [u8], offset: usize, colour: [u8; 3]) -> Result<(), String> {
    for (index, channel) in colour.into_iter().enumerate() {
        let value = channel as f32 / 255.0;
        image
            .get_mut(offset + index * 4..offset + index * 4 + 4)
            .ok_or("Colour patch is outside TAGame.upk")?
            .copy_from_slice(&value.to_le_bytes());
    }
    Ok(())
}

fn find_all(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }
    haystack
        .windows(needle.len())
        .enumerate()
        .filter_map(|(index, value)| (value == needle).then_some(index))
        .collect()
}

fn package_guid(data: &[u8]) -> Result<[u8; 16], String> {
    let offset = fstring_end(data, 12)? + 48;
    data.get(offset..offset + 16)
        .ok_or("Package GUID is truncated")?
        .try_into()
        .map_err(|_| "Package GUID is invalid".into())
}

fn crypt(data: &[u8], key: &[u8; 32], encrypt: bool) -> Result<Vec<u8>, String> {
    if !data.len().is_multiple_of(16) {
        return Err("Encrypted UPK header is not block aligned".into());
    }
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut output = data.to_vec();
    for block in output.chunks_exact_mut(16) {
        if encrypt {
            cipher.encrypt_block(GenericArray::from_mut_slice(block));
        } else {
            cipher.decrypt_block(GenericArray::from_mut_slice(block));
        }
    }
    Ok(output)
}

fn read_fname(data: &[u8], offset: usize, version: i32) -> Result<(FName, usize), String> {
    let index = read_i32(data, offset)?;
    if version >= 343 {
<<<<<<< Updated upstream
        Ok((FName { index }, offset + 8))
    } else {
        Ok((FName { index }, offset + 4))
=======
        Ok((
            FName {
                index,
                number: read_i32(data, offset + 4)?,
            },
            offset + 8,
        ))
    } else {
        Ok((FName { index, number: 0 }, offset + 4))
>>>>>>> Stashed changes
    }
}

fn read_fstring(data: &[u8], offset: usize) -> Result<(String, usize), String> {
    let length = read_i32(data, offset)?;
    if length == 0 {
        return Ok((String::new(), offset + 4));
    }
    if length < 0 {
        let units = usize_from_i64(-i64::from(length), "wide string length")?;
        let bytes = data
            .get(offset + 4..offset + 4 + units * 2)
            .ok_or("Wide string is truncated")?;
        let words = bytes
            .chunks_exact(2)
            .take(units.saturating_sub(1))
            .map(|pair| u16::from_le_bytes(pair.try_into().unwrap()))
            .collect::<Vec<_>>();
        Ok((String::from_utf16_lossy(&words), offset + 4 + units * 2))
    } else {
        let bytes = usize_from_i32(length, "string length")?;
        let value = data
            .get(offset + 4..offset + 4 + bytes.saturating_sub(1))
            .ok_or("String is truncated")?;
        Ok((
            String::from_utf8_lossy(value).into_owned(),
            offset + 4 + bytes,
        ))
    }
}

fn fstring_end(data: &[u8], offset: usize) -> Result<usize, String> {
    read_fstring(data, offset).map(|(_, end)| end)
}

fn read_i32(data: &[u8], offset: usize) -> Result<i32, String> {
    data.get(offset..offset + 4)
        .ok_or_else(|| "UPK data is truncated".into())
        .map(|value| i32::from_le_bytes(value.try_into().unwrap()))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, String> {
    data.get(offset..offset + 4)
        .ok_or_else(|| "UPK data is truncated".into())
        .map(|value| u32::from_le_bytes(value.try_into().unwrap()))
}

fn read_i64(data: &[u8], offset: usize) -> Result<i64, String> {
    data.get(offset..offset + 8)
        .ok_or_else(|| "UPK data is truncated".into())
        .map(|value| i64::from_le_bytes(value.try_into().unwrap()))
}

fn write_i32(data: &mut [u8], offset: usize, value: i32) -> Result<(), String> {
    data.get_mut(offset..offset + 4)
        .ok_or("UPK header write is out of bounds")?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_i64(data: &mut [u8], offset: usize, value: i64) -> Result<(), String> {
    data.get_mut(offset..offset + 8)
        .ok_or("UPK header write is out of bounds")?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn usize_from_i32(value: i32, label: &str) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| format!("Invalid negative {label}"))
}

fn usize_from_i64(value: i64, label: &str) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| format!("Invalid negative {label}"))
}
<<<<<<< Updated upstream

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_overlapping_matches() {
        assert_eq!(find_all(b"aaaa", b"aa"), vec![0, 1, 2]);
    }

    #[test]
    fn rgb_is_written_as_normalised_little_endian_floats() {
        let mut bytes = [0u8; 12];
        write_rgb(&mut bytes, 0, [255, 0, 128]).unwrap();
        assert_eq!(f32::from_le_bytes(bytes[0..4].try_into().unwrap()), 1.0);
        assert_eq!(f32::from_le_bytes(bytes[4..8].try_into().unwrap()), 0.0);
        assert_eq!(
            f32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            128.0 / 255.0
        );
    }

    #[test]
    fn active_colours_round_trip_between_instances() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let backups = std::env::temp_dir().join(format!(
            "hebnix-active-colours-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&backups).unwrap();
        fs::write(backups.join(BACKUP_NAME), b"test backup marker").unwrap();
        let expected = ColourSettings {
            stadium_blue: [1, 2, 3],
            stadium_orange: [4, 5, 6],
            hud_blue: [7, 8, 9],
            hud_orange: [10, 11, 12],
            extended_palette: true,
            applied: true,
        };
        save_state(&backups, &expected).unwrap();
        let manifest = backups.join(STATE_NAME);
        assert!(manifest.is_file());
        let mut next_instance = ColoursState::new();
        next_instance.load_active(&backups);
        assert_eq!(
            serde_json::to_value(&next_instance.settings).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        fs::remove_dir_all(backups).unwrap();
    }

    #[test]
    #[ignore = "requires HEBNIX_TEST_TAGAME"]
    fn rebuilds_an_installed_tagame_without_writing_it() {
        let path = std::env::var("HEBNIX_TEST_TAGAME").expect("HEBNIX_TEST_TAGAME");
        let mut package = Package::load(fs::read(path).unwrap()).unwrap();
        let settings = ColourSettings {
            extended_palette: true,
            ..ColourSettings::default()
        };
        package.apply_team_colours(&settings).unwrap();
        package.extend_palette().unwrap();
        assert!(!package.modified_chunks.is_empty());
        Package::load(package.save().unwrap()).unwrap();
    }
}
=======
>>>>>>> Stashed changes
