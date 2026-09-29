//! Experimental baked paint for known cosmetic packages. This changes local asset
//! defaults, not inventory paint attributes. Runtime materials/decals may override
//! these defaults; each supported asset still needs an in-game visual check.
use super::upk_package::{strip, Prop, UpkPackage};
use std::path::Path;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SwapPaint {
    #[default]
    Default,
    Crimson,
    Red,
    TitaniumWhite,
}

impl SwapPaint {
    pub const ALL: [Self; 4] = [Self::Default, Self::Crimson, Self::Red, Self::TitaniumWhite];
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Crimson => "Crimson",
            Self::Red => "Red (#FF0000)",
            Self::TitaniumWhite => "Titanium White",
        }
    }
    // ProductPaint_TA Red_00 / White_00 Colors[0] in the installed TAGame.upk.
    fn material(self) -> [f32; 4] {
        match self {
            Self::Crimson => [0.6, 0.0, 0.0, 1.0],
            Self::Red => [1.0, 0.0, 0.0, 1.0],
            Self::TitaniumWhite => [0.8, 0.8, 0.8, 1.0],
            Self::Default => unreachable!(),
        }
    }
}

pub fn supports(donor: &str) -> bool {
    matches!(donor.to_ascii_lowercase().as_str(), "body_grain_sf.upk" | "boost_standard_sf.upk")
}

fn named<'a>(props: &'a [Prop], name: &str, ty: &str) -> Result<&'a Prop, String> {
    props.iter().find(|p| p.name == name && p.tag_type == ty)
        .ok_or_else(|| format!("Missing {name} ({ty}) in paint template"))
}

fn parameter_name(package: &UpkPackage, prop: &Prop, base: usize) -> Result<String, String> {
    if prop.size != 8 { return Err("Invalid paint parameter FName".into()); }
    let index = usize::try_from(package.read_int(base + prop.value_offset)?)
        .map_err(|_| "Negative paint parameter name")?;
    package.names.get(index).cloned().ok_or_else(|| "Invalid paint parameter name".into())
}

fn vector_slot(package: &UpkPackage, props: &[Prop], base: usize, parameter: &str) -> Result<usize, String> {
    let array = named(props, "VectorParameterValues", "ArrayProperty")?;
    let start = base + array.value_offset;
    let end = start.checked_add(array.size).ok_or("Paint array overflow")?;
    if array.size < 4 { return Err("Truncated paint vector array".into()); }
    let count = package.read_int(start)?;
    if !(0..=1024).contains(&count) { return Err("Invalid paint vector count".into()); }
    let mut at = start + 4;
    let mut found = None;
    for _ in 0..count {
        let (entry, next) = package.nested_props(at, end)?;
        let name = named(&entry, "ParameterName", "NameProperty")?;
        if parameter_name(package, name, 0)? == parameter {
            let value = named(&entry, "ParameterValue", "StructProperty")?;
            if value.struct_name != "LinearColor" || value.size != 16 || found.is_some() {
                return Err("Ambiguous or invalid paint vector".into());
            }
            found = Some(value.value_offset);
        }
        at = next;
    }
    if at != end { return Err("Unexpected bytes in paint vector array".into()); }
    found.ok_or_else(|| format!("Material has no {parameter} vector"))
}

/// Paint an already identity-swapped package in staging. The donor name is the
/// original asset, even though the staged filename now identifies the owned item.
/// Known structures are required; unsupported layouts fail before writing.
pub fn bake(path: &Path, donor: &str, paint: SwapPaint) -> Result<usize, String> {
    if paint == SwapPaint::Default { return Ok(0); }
    if !supports(donor) { return Err("Experimental UPK paint supports Fennec and Standard boost only".into()); }
    let body = donor.eq_ignore_ascii_case("body_grain_sf.upk");
    let mut package = UpkPackage::load(path)?;
    let mut patches: Vec<(usize, Vec<u8>)> = Vec::new();
    let material_name = if body { "MIC_Body_Grain" } else { "MasterBoost_Standard_MIC" };
    let parameter = if body { "TrimColor" } else { "CustomColor" };
    let colors = paint.material();
    let mut materials = 0;
    let mut particles = 0;
    for export in &package.exports {
        let class = package.class_of(export);
        if strip(&class) == "MaterialInstanceConstant" && strip(&package.name_of(export.object_name)) == material_name {
            let (props, _) = package.serialized_props(export)?;
            let at = vector_slot(&package, &props, export.serial_offset, parameter)?;
            patches.push((at, colors.iter().flat_map(|v| v.to_le_bytes()).collect()));
            materials += 1;
        }
        if !body && strip(&class) == "DistributionVectorParticleParameter" {
            // These distribution subobjects have a four-byte template index
            // following the net index. Require the observed template sentinel.
            if package.read_int(export.serial_offset + 4)? != -1 {
                return Err("Unsupported particle distribution template header".into());
            }
            let (props, _) = package.nested_props(export.serial_offset + 8, export.serial_offset + export.serial_size)?;
            let name = named(&props, "ParameterName", "NameProperty")?;
            if matches!(parameter_name(&package, name, 0)?.as_str(), "CustomColor" | "InnerColor") {
                let constant = named(&props, "Constant", "StructProperty")?;
                if constant.struct_name != "Vector" || constant.size != 12 { return Err("Unexpected particle colour layout".into()); }
                let at = constant.value_offset;
                // Preserve the original HDR peak brightness while replacing hue.
                let old = &package.image[at..at+12];
                let peak = old.chunks_exact(4).map(|v| f32::from_le_bytes(v.try_into().unwrap())).fold(0.0f32, f32::max);
                if !peak.is_finite() || peak <= 0.0 { return Err("Invalid particle brightness".into()); }
                let max = colors[..3].iter().copied().fold(0.0f32, f32::max);
                let rgb: Vec<u8> = colors[..3].iter().flat_map(|v| (v / max * peak).to_le_bytes()).collect();
                patches.push((at, rgb));
                particles += 1;
            }
        }
    }
    if materials != 1 || (!body && particles != 3) {
        return Err(format!("Unsupported donor layout: found {materials} paint materials and {particles} particle colour defaults"));
    }
    for (offset, bytes) in &patches { package.patch(*offset, bytes)?; }
    let temporary = path.with_extension("paint.tmp");
    let result = (|| {
        package.save(&temporary)?;
        let check = UpkPackage::load(&temporary)?;
        // Repacking may change physical offsets, but no export may change beyond
        // the planned float values. Includes meshes, textures and all native tails.
        for export in &package.exports {
            let start = export.serial_offset;
            let end = start + export.serial_size;
            if check.image.get(start..end) != package.image.get(start..end) {
                return Err("Paint package export round-trip mismatch".into());
            }
        }
        for (offset, bytes) in &patches {
            if check.image.get(*offset..*offset + bytes.len()) != Some(bytes.as_slice()) {
                return Err("Paint values did not survive package round-trip".into());
            }
        }
        std::fs::copy(&temporary, path).map_err(|e| e.to_string())?;
        Ok(patches.len())
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

/// Keep the existing identity patch and paint patch together in staging, so a
/// missing material or failed verification never installs a half-painted swap.
pub fn patch_for_target(
    source: &Path, target_backup: &Path, destination: &Path, base_dir: &Path,
    donor_asset_path: Option<&str>, target_asset_path: Option<&str>,
    donor_filename: &str, paint: SwapPaint,
) -> Result<(), String> {
    if paint == SwapPaint::Default {
        return crate::cosmetic_upk::patch_for_target(source, target_backup, destination, base_dir, donor_asset_path, target_asset_path);
    }
    let parent = destination.parent().ok_or("No destination directory")?;
    let staging = parent.join(format!(".hebnix-paint-{}-{}", std::process::id(), rand::random::<u64>()));
    std::fs::create_dir(&staging).map_err(|e| e.to_string())?;
    let result = (|| {
        let staged = staging.join(destination.file_name().ok_or("No destination filename")?);
        crate::cosmetic_upk::patch_for_target(source, target_backup, &staged, base_dir, donor_asset_path, target_asset_path)?;
        bake(&staged, donor_filename, paint)?;
        std::fs::copy(&staged, destination).map_err(|e| e.to_string())?;
        Ok(())
    })();
    // Only files created above, never an arbitrary recursive directory removal.
    if let Some(name) = destination.file_name() {
        let staged = staging.join(name);
        let _ = std::fs::remove_file(staged.with_extension("paint.tmp"));
        let _ = std::fs::remove_file(staged);
    }
    let _ = std::fs::remove_dir(&staging);
    result
}
