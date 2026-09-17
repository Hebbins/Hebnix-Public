use super::upk_package::{UpkPackage, strip};
use std::fs;
use std::path::Path;

const PACKAGE_NAME: &str = "Mutators_Balls_SF.upk";
const BACKUP_NAME: &str = "Mutators_Balls_SF.upk.heatseeker-glow.bak";

fn owner_name(package: &UpkPackage, index: i32) -> Option<String> {
    (index > 0)
        .then(|| package.exports.get((index - 1) as usize))
        .flatten()
        .map(|export| package.name_of(export.object_name))
}

fn rgb(colour: [u8; 3], intensity: f32) -> [u8; 12] {
    let mut bytes = [0u8; 12];
    for (index, channel) in colour.into_iter().enumerate() {
        bytes[index * 4..index * 4 + 4]
            .copy_from_slice(&(channel as f32 / 255.0 * intensity).to_le_bytes());
    }
    bytes
}

/// Replaces only local high-speed Heatseeker effects. It always rebuilds from
/// a pristine backup, so it cannot alter the team-coloured normal glow.
pub fn apply(cooked_pc: &Path, backups_dir: &Path, max_speed: [u8; 3]) -> Result<(), String> {
    let live = cooked_pc.join(PACKAGE_NAME);
    if !live.is_file() {
        return Err(format!(
            "{PACKAGE_NAME} was not found in {}",
            cooked_pc.display()
        ));
    }
    fs::create_dir_all(backups_dir)
        .map_err(|error| format!("Could not create the backup directory: {error}"))?;
    let backup = backups_dir.join(BACKUP_NAME);
    if !backup.exists() {
        fs::copy(&live, &backup)
            .map_err(|error| format!("Could not create Heatseeker glow backup: {error}"))?;
    }
    let mut package = UpkPackage::load(&backup)?;
    let mut speed_count = 0;
    for export in package.exports.clone() {
        if strip(&package.class_of(&export)) != "DistributionVectorParticleParameter" {
            continue;
        }
        let Some(module) = owner_name(&package, export.outer_index) else {
            continue;
        };
        if strip(&module) != "ParticleModuleColor" {
            continue;
        }
        let Some(system) = package
            .exports
            .get((export.outer_index - 1) as usize)
            .and_then(|module| owner_name(&package, module.outer_index))
        else {
            continue;
        };
        if strip(&system) != "GodBall_SpeedTrail_PS" {
            continue;
        }
        let constant = package
            .parse_props(&export)
            .into_iter()
            .find(|property| property.name == "Constant" && property.size == 12)
            .ok_or_else(|| format!("{system}.Constant changed layout"))?;
        package.patch(
            export.serial_offset + constant.value_offset,
            &rgb(max_speed, 2.0),
        )?;
        speed_count += 1;
    }
    let mut dissolve_count = 0;
    for export in package.exports.clone() {
        if strip(&package.class_of(&export)) != "MaterialExpressionVectorParameter" {
            continue;
        }
        let Some(material) = owner_name(&package, export.outer_index) else {
            continue;
        };
        if strip(&material) != "BallDissolve_MAT" {
            continue;
        }
        let properties = package.parse_props(&export);
        let parameter = properties
            .iter()
            .find(|property| property.name == "ParameterName" && property.size == 8)
            .and_then(|property| {
                package
                    .read_int(export.serial_offset + property.value_offset)
                    .ok()
            })
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| package.names.get(index))
            .map(String::as_str);
        if parameter != Some("DissolveColor") {
            continue;
        }
        let value = properties
            .iter()
            .find(|property| property.name == "DefaultValue" && property.size == 16)
            .ok_or("BallDissolve_MAT.DissolveColor changed layout")?;
        package.patch(
            export.serial_offset + value.value_offset,
            &rgb(max_speed, 1.0),
        )?;
        dissolve_count += 1;
    }
    if speed_count != 1 || dissolve_count != 1 {
        return Err(format!(
            "Heatseeker glow layout changed (found {speed_count}/1 speed and {dissolve_count}/1 dissolve targets)"
        ));
    }
    let temp = cooked_pc.join(format!("{PACKAGE_NAME}.heatseeker-glow.tmp"));
    package.save(&temp)?;
    UpkPackage::load(&temp)
        .map_err(|error| format!("Patched Heatseeker package failed validation: {error}"))?;
    fs::copy(&temp, &live)
        .map_err(|error| format!("Could not install Heatseeker glow patch: {error}"))?;
    let _ = fs::remove_file(temp);
    Ok(())
}

pub fn restore(cooked_pc: &Path, backups_dir: &Path) -> Result<(), String> {
    let backup = backups_dir.join(BACKUP_NAME);
    if !backup.exists() {
        return Ok(());
    }
    UpkPackage::load(&backup)
        .map_err(|error| format!("Heatseeker glow backup is invalid: {error}"))?;
    fs::copy(&backup, cooked_pc.join(PACKAGE_NAME))
        .map_err(|error| format!("Could not restore Heatseeker glow: {error}"))?;
    fs::remove_file(backup)
        .map_err(|error| format!("Could not remove Heatseeker glow backup: {error}"))
}
