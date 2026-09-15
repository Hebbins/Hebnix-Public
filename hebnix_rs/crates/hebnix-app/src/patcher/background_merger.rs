use std::collections::{HashMap, HashSet};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use windows::Win32::System::Threading::CREATE_NO_WINDOW;

use serde::{Deserialize, Serialize};

use super::upk_package::{ExportEntry, Prop, UpkPackage, strip};

const BACKUP_SUFFIX: &str = ".hbnx_mapbak";
const MANIFEST: &str = "background_swaps.json";
const SAFE_DONORS: &[&str] = &[
    "BG_Stadium_10A_P",
    "BG_NeoTokyo_Arcade",
    "BG_NeoTokyo_Hax",
    "BG_Woods_Day_P",
    "BG_FNI_Stadium",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SwapState {
    donor: String,
    copy_name: String,
    patched_sub_levels: Vec<String>,
    extra_copies: Vec<String>,
}

type State = HashMap<String, SwapState>;
type Patch = (usize, Vec<u8>);

pub fn run(
    command: &str,
    cooked: &Path,
    state_dir: &Path,
    host: Option<&str>,
    donor: Option<&str>,
) -> Result<String, String> {
    if !cooked.is_dir() {
        return Err(
            "The configured Rocket League folder does not contain TAGame\\CookedPCConsole.".into(),
        );
    }
    std::fs::create_dir_all(state_dir)
        .map_err(|e| format!("Could not create background state folder: {e}"))?;
    match command {
        "apply" => apply(
            cooked,
            state_dir,
            host.ok_or("Missing host arena")?,
            donor.ok_or("Missing background donor")?,
        ),
        "undo" => undo(cooked, state_dir, host.ok_or("Missing host arena")?),
        "reset" => reset(cooked, state_dir),
        _ => Err("Invalid background changer command".into()),
    }
}

fn apply(
    cooked: &Path,
    state_dir: &Path,
    host_name: &str,
    donor_name: &str,
) -> Result<String, String> {
    validate_name(host_name)?;
    validate_name(donor_name)?;
    if !SAFE_DONORS
        .iter()
        .any(|x| x.eq_ignore_ascii_case(donor_name))
    {
        return Err("That package is not a game-safe background donor.".into());
    }
    if host_name.eq_ignore_ascii_case(donor_name) {
        return Err("Pick two different arenas.".into());
    }
    ensure_game_closed("applying a background")?;
    let host_path = map_path(cooked, host_name);
    let donor_path = map_path(cooked, donor_name);
    if !host_path.is_file() {
        return Err(format!("Host arena is missing: {host_name}.upk"));
    }
    if !donor_path.is_file() {
        return Err(format!("Background arena is missing: {donor_name}.upk"));
    }
    let mut state = load_state(state_dir)?;
    undo_core(cooked, &mut state, host_name)?;
    save_state(state_dir, &state)?;
    let old_copies = matching_files(cooked, |name| {
        name.starts_with("HBNX_") && name.ends_with(".upk")
    });
    let old_backups = matching_files(cooked, |name| name.ends_with(".upk.hbnx_mapbak"));
    let backup = backup_path(&host_path);
    if backup.exists() {
        return Err("This arena has a background backup that Hebnix does not own. Restore it before changing this background.".into());
    }
    std::fs::copy(&host_path, &backup).map_err(|e| format!("Could not back up host arena: {e}"))?;
    let donor_backup = backup_path(&donor_path);
    let donor_source = if donor_backup.is_file() {
        donor_backup
    } else {
        donor_path.clone()
    };
    let result = apply_inner(cooked, &host_path, &donor_source, donor_name);
    match result {
        Ok((copy_name, patched_sub_levels)) => {
            state.insert(
                host_name.to_string(),
                SwapState {
                    donor: donor_name.to_string(),
                    copy_name,
                    patched_sub_levels,
                    extra_copies: Vec::new(),
                },
            );
            if let Err(e) = save_state(state_dir, &state) {
                let _ = undo_core(cooked, &mut state, host_name);
                return Err(e);
            }
            Ok(format!(
                "{} now uses {}'s fog, sky and background.",
                friendly(host_name),
                friendly(donor_name)
            ))
        }
        Err(error) => {
            for new_backup in matching_files(cooked, |name| name.ends_with(".upk.hbnx_mapbak"))
                .difference(&old_backups)
            {
                let live =
                    PathBuf::from(new_backup.to_string_lossy().trim_end_matches(BACKUP_SUFFIX));
                let _ = restore_file(new_backup, &live);
            }
            for new_copy in matching_files(cooked, |name| {
                name.starts_with("HBNX_") && name.ends_with(".upk")
            })
            .difference(&old_copies)
            {
                let _ = std::fs::remove_file(new_copy);
            }
            Err(error)
        }
    }
}

fn apply_inner(
    cooked: &Path,
    host_path: &Path,
    donor_path: &Path,
    donor_name: &str,
) -> Result<(String, Vec<String>), String> {
    let mut host = UpkPackage::load(host_path)?;
    let slot = host
        .find_stream_name_index()
        .ok_or("This map has no streaming slot and cannot host a background")?;
    let slot_name = host.names[slot].clone();
    let copy_name = make_copy_name(donor_name, slot_name.len())?;
    let generated = map_path(cooked, &copy_name);
    let scenery_source = if donor_name.eq_ignore_ascii_case("BG_NeoTokyo_Hax") {
        package_source(cooked, "neotokyo_hax_signs_off_p")?
    } else {
        donor_path.to_path_buf()
    };
    std::fs::copy(&scenery_source, &generated)
        .map_err(|e| format!("Could not create donor scenery package: {e}"))?;
    let donor = UpkPackage::load(donor_path)?;
    let mut generated_pkg = UpkPackage::load(&generated)?;
    let retained = patch_donor_outside_only(&mut generated_pkg)?;
    if retained == 0 {
        return Err(format!(
            "'{donor_name}' has no recognised sky or out-of-bounds scenery to transfer"
        ));
    }
    generated_pkg.save(&generated)?;

    let preserve_host_atmosphere = donor_name.eq_ignore_ascii_case("BG_NeoTokyo_Hax");
    let stream_indices = host.find_all_stream_name_indices();
    let mut patched_sub_levels = Vec::new();
    for index in stream_indices.into_iter().filter(|i| *i != slot) {
        let name = host.names[index].clone();
        if !visible_sublevel(&name) {
            continue;
        }
        let path = map_path(cooked, &name);
        if !path.is_file() {
            continue;
        }
        let backup = backup_path(&path);
        if !backup.exists() {
            std::fs::copy(&path, &backup)
                .map_err(|e| format!("Could not back up sub-level {name}: {e}"))?;
        }
        let mut sub = UpkPackage::load(&backup)?;
        let mut patches =
            host_sky_patches(&sub, donor_name.eq_ignore_ascii_case("BG_NeoTokyo_Hax"))?;
        if !preserve_host_atmosphere {
            patches.extend(disable_old_atmosphere(&sub)?);
        }
        apply_patches(&mut sub, patches)?;
        sub.save(&path)?;
        patched_sub_levels.push(name);
    }
    host.rename_name(slot, &copy_name)?;
    let mut patches = host_sky_patches(&host, donor_name.eq_ignore_ascii_case("BG_NeoTokyo_Hax"))?;
    if !preserve_host_atmosphere {
        patches.extend(disable_old_atmosphere(&host)?);
        patches.extend(world_visual_patches(&host, &donor)?);
    }
    apply_patches(&mut host, patches)?;
    host.save(host_path)?;
    Ok((copy_name, patched_sub_levels))
}

fn patch_donor_outside_only(package: &mut UpkPackage) -> Result<usize, String> {
    let mut patches = Vec::new();
    let mut retained = 0usize;
    for e in &package.exports {
        let class_name = package.class_of(e);
        let class = strip(&class_name);
        let props = package.parse_props(e);
        let mesh_prop = if class == "StaticMeshComponent" || class == "InstancedStaticMeshComponent"
        {
            props
                .iter()
                .find(|p| p.name == "StaticMesh" && p.tag_type == "ObjectProperty")
        } else if class == "SkeletalMeshComponent" {
            props
                .iter()
                .find(|p| p.name == "SkeletalMesh" && p.tag_type == "ObjectProperty")
        } else {
            None
        };
        if let Some(p) = mesh_prop {
            let off = e.serial_offset + p.value_offset;
            let reference = package.read_int(off)?;
            if reference != 0 {
                let mesh_name = package.obj_name(reference);
                let mesh = strip(&mesh_name);
                if donor_outside_scope(mesh) {
                    retained += 1
                } else {
                    patches.push((off, vec![0; 4]))
                }
            }
        }
        if class == "ParticleSystemComponent" {
            if let Some(p) = props
                .iter()
                .find(|p| p.name == "Template" && p.tag_type == "ObjectProperty")
            {
                let off = e.serial_offset + p.value_offset;
                let reference = package.read_int(off)?;
                if reference != 0 {
                    if ambient_particle(package, e, reference) {
                        retained += 1
                    } else {
                        patches.push((off, vec![0; 4]))
                    }
                }
            }
        }
        if mesh_prop.is_some() || class.contains("Collision") || class == "BrushComponent" {
            for p in &props {
                if p.tag_type == "BoolProperty"
                    && (p.name.contains("Collide")
                        || p.name.contains("Block")
                        || p.name.contains("RigidBody"))
                {
                    patches.push((e.serial_offset + p.value_offset - 1, vec![0]))
                }
            }
        }
    }
    apply_patches(package, patches)?;
    Ok(retained)
}

fn host_sky_patches(package: &UpkPackage, preserve_sky: bool) -> Result<Vec<Patch>, String> {
    let mut patches = Vec::new();
    for e in &package.exports {
        let class_name = package.class_of(e);
        let class = strip(&class_name);
        if class != "StaticMeshComponent"
            && class != "InstancedStaticMeshComponent"
            && class != "SkeletalMeshComponent"
        {
            continue;
        }
        let prop_name = if class == "SkeletalMeshComponent" {
            "SkeletalMesh"
        } else {
            "StaticMesh"
        };
        if let Some(p) = package
            .parse_props(e)
            .into_iter()
            .find(|p| p.name == prop_name && p.tag_type == "ObjectProperty")
        {
            let off = e.serial_offset + p.value_offset;
            let reference = package.read_int(off)?;
            let mesh_name = package.obj_name(reference);
            let in_replaced_scope = if preserve_sky {
                is_backdrop(&mesh_name) || is_atmosphere_visual(&mesh_name)
            } else {
                host_background_scope(&mesh_name)
            };
            if reference != 0 && in_replaced_scope {
                patches.push((off, vec![0; 4]))
            }
        }
    }
    Ok(patches)
}

fn disable_old_atmosphere(package: &UpkPackage) -> Result<Vec<Patch>, String> {
    const GLOBAL: &[&str] = &[
        "DirectionalLightComponent",
        "DominantDirectionalLightComponent",
        "SkyLightComponent",
        "SkyLightVolumeComponent_TA",
        "PointLightComponent",
        "SpotLightComponent",
        "PointLightComponent_TA",
        "SpotLightComponent_TA",
        "DominantSpotLightComponent",
    ];
    let mut patches = Vec::new();
    for e in &package.exports {
        let class_name = package.class_of(e);
        let class = strip(&class_name);
        let props = package.parse_props(e);
        if GLOBAL.contains(&class) {
            for p in &props {
                let off = e.serial_offset + p.value_offset;
                if p.tag_type == "FloatProperty" && p.name == "Brightness" {
                    patches.push((off, vec![0; 4]))
                } else if p.tag_type == "StructProperty"
                    && ["LightColor", "LowerColor", "UpperColor"].contains(&p.name.as_str())
                {
                    patches.push((off, vec![0; p.size.min(4)]))
                } else if p.tag_type == "BoolProperty" && p.name == "bEnabled" {
                    patches.push((off - 1, vec![0]))
                }
            }
        } else if class == "ExponentialHeightFogComponent" || class == "HeightFogComponent" {
            if let Some(p) = props
                .iter()
                .find(|p| p.name == "FogDensity" && p.tag_type == "FloatProperty")
            {
                patches.push((e.serial_offset + p.value_offset, vec![0; 4]))
            }
        } else if class == "LensFlareComponent" {
            if let Some(p) = props
                .iter()
                .find(|p| p.name == "Template" && p.tag_type == "ObjectProperty")
            {
                patches.push((e.serial_offset + p.value_offset, vec![0; 4]))
            }
        }
    }
    Ok(patches)
}

fn world_visual_patches(host: &UpkPackage, donor: &UpkPackage) -> Result<Vec<Patch>, String> {
    let Some(he) = host
        .exports
        .iter()
        .find(|e| strip(&host.class_of(e)) == "WorldInfo")
    else {
        return Ok(Vec::new());
    };
    let Some(de) = donor
        .exports
        .iter()
        .find(|e| strip(&donor.class_of(e)) == "WorldInfo")
    else {
        return Ok(Vec::new());
    };
    let donor_props = donor.parse_props(de);
    let mut by_name: HashMap<&str, &Prop> = HashMap::new();
    for p in &donor_props {
        by_name.insert(&p.name, p);
    }
    let mut patches = Vec::new();
    for hp in host.parse_props(he) {
        if !visual_world_property(&hp.name) {
            continue;
        }
        let Some(dp) = by_name.get(hp.name.as_str()) else {
            continue;
        };
        if hp.tag_type != dp.tag_type || hp.size != dp.size {
            continue;
        }
        if hp.tag_type == "BoolProperty" {
            patches.push((
                he.serial_offset + hp.value_offset - 1,
                vec![u8::from(dp.bool_value.unwrap_or(false))],
            ));
        } else if [
            "FloatProperty",
            "IntProperty",
            "StructProperty",
            "ByteProperty",
        ]
        .contains(&hp.tag_type.as_str())
        {
            let start = de.serial_offset + dp.value_offset;
            let data = donor
                .image
                .get(start..start + dp.size)
                .ok_or("Donor WorldInfo property is outside its export")?
                .to_vec();
            patches.push((he.serial_offset + hp.value_offset, data));
        }
    }
    Ok(patches)
}

fn visual_world_property(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "fog",
        "scene_",
        "ambient",
        "environment",
        "colorgrading",
        "colourgrading",
        "postprocess",
        "bloom",
        "dof",
        "depthoffield",
        "motionblur",
        "tonemap",
        "lut",
        "exposure",
        "sky",
        "sun",
        "lightmass",
    ]
    .iter()
    .any(|x| n.contains(x))
}
fn ambient_particle(package: &UpkPackage, e: &ExportEntry, reference: i32) -> bool {
    let mut description = package.obj_name(reference);
    let mut outer = e.outer_index;
    for _ in 0..8 {
        if outer <= 0 {
            break;
        }
        let Some(owner) = package.exports.get((outer - 1) as usize) else {
            break;
        };
        description.push(' ');
        description.push_str(&package.name_of(owner.object_name));
        outer = owner.outer_index;
    }
    let lower = description.to_ascii_lowercase();
    ![
        "boost",
        "explosion",
        "goal",
        "pickup",
        "ball",
        "centerpiece",
    ]
    .iter()
    .any(|x| lower.contains(x))
}
fn donor_outside_scope(mesh: &str) -> bool {
    if is_gameplay_structure(mesh) {
        return false;
    }
    is_core_background(mesh) || is_extended_scenery(mesh)
}

fn host_background_scope(mesh: &str) -> bool {
    if is_gameplay_structure(mesh) {
        return false;
    }
    is_core_background(mesh)
}

fn is_core_background(mesh: &str) -> bool {
    is_sky(mesh) || is_backdrop(mesh) || is_atmosphere_visual(mesh)
}

fn is_sky(mesh: &str) -> bool {
    let n = mesh.to_ascii_lowercase();
    !n.contains("skyscraper")
        && (n.contains("skysphere")
            || n.contains("skydome")
            || n.contains("skybox")
            || n.starts_with("sky")
            || n.contains("cloud")
            || n.contains("moon")
            || n.contains("aurora")
            || n.contains("starfield")
            || n.contains("sky_rift"))
}

fn is_backdrop(mesh: &str) -> bool {
    let n = mesh.to_ascii_lowercase();
    n.contains("_oob")
        || n.starts_with("oob_")
        || n.contains("mountain")
        || n.contains("skyscraper")
        || n.contains("cityground")
        || n.starts_with("city_")
        || n.contains("skyline")
        || n.contains("backdrop")
        || n.contains("distant")
        || n.contains("horizon")
        || n.contains("treeline")
}

fn is_atmosphere_visual(mesh: &str) -> bool {
    let n = mesh.to_ascii_lowercase();
    [
        "fog",
        "mist",
        "haze",
        "smoke",
        "lightcone",
        "lightbeam",
        "godray",
    ]
    .iter()
    .any(|hint| n.contains(hint))
}

fn is_extended_scenery(mesh: &str) -> bool {
    let n = mesh.to_ascii_lowercase();
    [
        "building",
        "tower",
        "bridge",
        "skyway",
        "tree",
        "terrain",
        "rock",
        "roof",
        "water",
        "pond",
        "moat",
        "streetsign",
        "balloon",
        "planter",
        "bush",
        "road",
        "floor",
        "station",
        "slums",
        "factory",
        "silo",
        "scraper",
        "subway",
        "track",
        "tram",
        "antenna",
        "sign",
        "loft",
        "adframe",
        "escalator",
        "satdish",
        "hub_",
        "hubbase",
        "hubpipes",
    ]
    .iter()
    .any(|hint| n.contains(hint))
}

fn is_gameplay_structure(mesh: &str) -> bool {
    let n = mesh.to_ascii_lowercase();
    [
        "goal",
        "boostpad",
        "collision",
        "centerpiece",
        "field_wall",
        "fieldwall",
        "fieldfloor",
        "grassfield",
        "stadium",
        "arena",
        "cage",
        "seat",
        "bleacher",
        "centerwall",
        "staircasewall",
        "path_lowpoly_wall",
        "ball_defaultball",
        "circle_sprite",
        "bleacheregg",
    ]
    .iter()
    .any(|hint| n.contains(hint))
        && !is_atmosphere_visual(mesh)
}

fn visible_sublevel(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    !["sfx", "audio", "sound", "ambient", "tutorial"]
        .iter()
        .any(|x| n.contains(x))
}
fn apply_patches(package: &mut UpkPackage, patches: Vec<Patch>) -> Result<(), String> {
    for (offset, data) in patches {
        package.patch(offset, &data)?
    }
    Ok(())
}
fn make_copy_name(donor: &str, len: usize) -> Result<String, String> {
    if len < 6 {
        return Err("This host's streaming slot is too short".into());
    }
    let mut name = format!("HBNX_{donor}");
    if name.len() < len {
        name.push_str(&"_".repeat(len - name.len()))
    }
    name.truncate(len);
    Ok(name)
}

fn undo(cooked: &Path, state_dir: &Path, host: &str) -> Result<String, String> {
    validate_name(host)?;
    ensure_game_closed("restoring an arena")?;
    let mut state = load_state(state_dir)?;
    if !state.contains_key(host) {
        return Err("That arena does not have an active background change.".into());
    }
    undo_core(cooked, &mut state, host)?;
    save_state(state_dir, &state)?;
    Ok(format!(
        "Restored {}'s original background.",
        friendly(host)
    ))
}
fn reset(cooked: &Path, state_dir: &Path) -> Result<String, String> {
    ensure_game_closed("restoring arenas")?;
    let mut state = load_state(state_dir)?;
    let hosts: Vec<String> = state.keys().cloned().collect();
    for host in hosts {
        undo_core(cooked, &mut state, &host)?
    }
    save_state(state_dir, &state)?;
    Ok("Restored all original arena backgrounds.".into())
}
fn undo_core(cooked: &Path, state: &mut State, host: &str) -> Result<(), String> {
    let Some(swap) = state.remove(host) else {
        return Ok(());
    };
    let live = map_path(cooked, host);
    let backup = backup_path(&live);
    if backup.is_file() {
        restore_file(&backup, &live)?
    }
    for sub in &swap.patched_sub_levels {
        let live = map_path(cooked, sub);
        let backup = backup_path(&live);
        if backup.is_file() {
            restore_file(&backup, &live)?
        }
    }
    for generated_name in std::iter::once(&swap.copy_name).chain(&swap.extra_copies) {
        let still_needed = state.values().any(|other| {
            other.copy_name.eq_ignore_ascii_case(generated_name)
                || other
                    .extra_copies
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(generated_name))
        });
        if !still_needed {
            let generated = map_path(cooked, generated_name);
            if generated.exists() {
                std::fs::remove_file(&generated)
                    .map_err(|e| format!("Could not remove {}: {e}", generated.display()))?;
            }
        }
    }
    Ok(())
}
fn restore_file(backup: &Path, live: &Path) -> Result<(), String> {
    std::fs::copy(backup, live)
        .map_err(|e| format!("Could not restore {}: {e}", live.display()))?;
    std::fs::remove_file(backup)
        .map_err(|e| format!("Could not remove backup {}: {e}", backup.display()))
}
fn load_state(dir: &Path) -> Result<State, String> {
    let path = dir.join(MANIFEST);
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let bytes =
        std::fs::read(&path).map_err(|e| format!("Could not read background state: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("Background state is invalid: {e}"))
}
fn save_state(dir: &Path, state: &State) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not create state folder: {e}"))?;
    let path = dir.join(MANIFEST);
    let temp = dir.join(format!("{MANIFEST}.tmp"));
    let bytes = serde_json::to_vec(state).map_err(|e| format!("Could not serialize state: {e}"))?;
    std::fs::write(&temp, bytes).map_err(|e| format!("Could not write state: {e}"))?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("Could not replace state: {e}"))?
    }
    std::fs::rename(temp, path).map_err(|e| format!("Could not finish state update: {e}"))
}
fn ensure_game_closed(action: &str) -> Result<(), String> {
    let output = Command::new("tasklist.exe")
        .args(["/FI", "IMAGENAME eq RocketLeague.exe", "/NH"])
        .creation_flags(CREATE_NO_WINDOW.0)
        .output();
    if output.ok().is_some_and(|o| {
        String::from_utf8_lossy(&o.stdout)
            .to_ascii_lowercase()
            .contains("rocketleague.exe")
    }) {
        Err(format!("Close Rocket League before {action}."))
    } else {
        Ok(())
    }
}
fn matching_files(cooked: &Path, predicate: impl Fn(&str) -> bool) -> HashSet<PathBuf> {
    std::fs::read_dir(cooked)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(&predicate)
        })
        .collect()
}
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        Err("Invalid arena package name.".into())
    } else {
        Ok(())
    }
}
fn package_source(cooked: &Path, name: &str) -> Result<PathBuf, String> {
    let live = map_path(cooked, name);
    let backup = backup_path(&live);
    let source = if backup.is_file() { backup } else { live };
    if source.is_file() {
        Ok(source)
    } else {
        Err(format!(
            "Required background scenery package is missing: {name}.upk"
        ))
    }
}
fn map_path(cooked: &Path, name: &str) -> PathBuf {
    cooked.join(format!("{name}.upk"))
}
fn backup_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}{BACKUP_SUFFIX}", path.display()))
}
fn friendly(name: &str) -> String {
    name.replace('_', " ").trim().to_string()
}
