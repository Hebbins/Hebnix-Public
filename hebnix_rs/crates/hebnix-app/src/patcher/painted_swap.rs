//! Experimental baked paint for known cosmetic packages. This changes local asset
//! defaults, not inventory paint attributes. Runtime materials/decals may override
//! these defaults; each supported asset still needs an in-game visual check.
use super::upk_package::{Prop, UpkPackage, strip};
use std::path::Path;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SwapPaint {
    #[default]
    Default,
    Crimson,
    Red, // Kept for existing saved swaps.
    TitaniumWhite,
    Preset(u8),
    Custom([u8; 3]),
}

// ProductPaint_TA Colors[0] from the installed TAGame.upk. The picker displays
// sRGB HEX; the UPK stores linear colour. Metallic finishes remain asset-defined.
const PALETTE: [(&str, [f32; 3]); 18] = [
    ("Crimson", [0.6, 0.0, 0.0]),
    ("Lime", [0.5, 1.0, 0.0]),
    ("Black", [0.05, 0.05, 0.05]),
    ("Sky Blue", [0.02, 0.5, 0.8]),
    ("Cobalt", [0.08, 0.2, 1.0]),
    ("Burnt Sienna", [0.3, 0.07, 0.0]),
    ("Forest Green", [0.0, 0.5, 0.0]),
    ("Purple", [0.25, 0.0, 0.5]),
    ("Pink", [0.8, 0.2, 0.6]),
    ("Orange", [1.0, 0.3, 0.0]),
    ("Grey", [0.25, 0.25, 0.25]),
    ("Titanium White", [0.8, 0.8, 0.8]),
    ("Saffron", [0.75, 0.75, 0.0]),
    ("Gold", [0.8045591, 0.5542271, 0.1801443]),
    ("Rose Gold", [1.0, 0.66611695, 0.63204265]),
    ("White Gold", [0.74030423, 0.5621126, 0.40401086]),
    ("Onyx", [0.04943346, 0.04943346, 0.04943346]),
    ("Platinum", [0.78931373, 0.7817507, 0.7667436]),
];
impl SwapPaint {
    pub const ALL: [Self; 20] = [
        Self::Default,
        Self::Custom([255; 3]),
        Self::Crimson,
        Self::Preset(2),
        Self::Preset(3),
        Self::Preset(4),
        Self::Preset(5),
        Self::Preset(6),
        Self::Preset(7),
        Self::Preset(8),
        Self::Preset(9),
        Self::Preset(10),
        Self::Preset(11),
        Self::TitaniumWhite,
        Self::Preset(13),
        Self::Preset(14),
        Self::Preset(15),
        Self::Preset(16),
        Self::Preset(17),
        Self::Preset(18),
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Crimson => "Crimson",
            Self::Red | Self::Custom(_) => "Custom",
            Self::TitaniumWhite => "Titanium White",
            Self::Preset(id) => PALETTE
                .get(id.wrapping_sub(1) as usize)
                .map_or("Invalid paint", |p| p.0),
        }
    }
    pub fn rgb(self) -> [u8; 3] {
        if let Self::Custom(rgb) = self {
            return rgb;
        }
        if self == Self::Default {
            return [255; 3];
        }
        self.material()[..3]
            .try_into()
            .map(|rgb: [f32; 3]| {
                rgb.map(|v| {
                    let srgb = if v <= 0.0031308 {
                        12.92 * v
                    } else {
                        1.055 * v.powf(1.0 / 2.4) - 0.055
                    };
                    (srgb.clamp(0.0, 1.0) * 255.0).round() as u8
                })
            })
            .unwrap()
    }
    pub fn description(self) -> String {
        if self == Self::Default {
            return "Default (uncoloured)".into();
        }
        let [r, g, b] = self.rgb();
        format!("{} #{r:02X}{g:02X}{b:02X}", self.label())
    }
    fn material(self) -> [f32; 4] {
        let rgb = match self {
            Self::Crimson => PALETTE[0].1,
            Self::Red => [1.0, 0.0, 0.0],
            Self::TitaniumWhite => PALETTE[11].1,
            Self::Preset(id) => PALETTE
                .get(id.wrapping_sub(1) as usize)
                .map_or([0.0; 3], |p| p.1),
            Self::Custom(rgb) => rgb.map(|v| {
                let v = v as f32 / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            }),
            Self::Default => [0.0; 3],
        };
        [rgb[0], rgb[1], rgb[2], 1.0]
    }
}

/// Opening the picker does not change paint. Only a changed RGB/valid HEX does.
pub fn controls(ui: &mut eframe::egui::Ui, paint: &mut SwapPaint) {
    use eframe::egui;
    ui.horizontal_wrapped(|ui| {
        let before = *paint;
        let rgb = paint.rgb();
        egui::ComboBox::from_id_salt("paint_preset")
            .width(110.0)
            .selected_text(paint.label())
            .show_ui(ui, |ui| {
                for option in SwapPaint::ALL {
                    let selected = if matches!(option, SwapPaint::Custom(_)) {
                        matches!(*paint, SwapPaint::Custom(_) | SwapPaint::Red)
                    } else {
                        *paint == option
                    };
                    if ui.selectable_label(selected, option.label()).clicked() {
                        *paint = if matches!(option, SwapPaint::Custom(_)) {
                            SwapPaint::Custom(rgb)
                        } else {
                            option
                        };
                    }
                }
            });
        let id = ui.id().with("paint_hex");
        let rgb = paint.rgb();
        let formatted = format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]);
        let mut hex = ui
            .data_mut(|d| d.get_temp::<String>(id))
            .unwrap_or_else(|| formatted.clone());
        if *paint != before {
            hex = formatted;
        }
        ui.horizontal(|ui| {
            let mut color = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
            let response = egui::color_picker::color_edit_button_srgba(
                ui,
                &mut color,
                egui::color_picker::Alpha::Opaque,
            );
            // Outline default with a slash so its uncoloured state remains clear,
            // while retaining the same fully clickable picker hitbox.
            if *paint == SwapPaint::Default {
                let rect = response.rect.shrink(2.0);
                let centre = rect.center();
                for (min, max, shade) in [
                    (rect.min, centre, 80),
                    (
                        egui::pos2(centre.x, rect.top()),
                        egui::pos2(rect.right(), centre.y),
                        140,
                    ),
                    (
                        egui::pos2(rect.left(), centre.y),
                        egui::pos2(centre.x, rect.bottom()),
                        140,
                    ),
                    (centre, rect.max, 80),
                ] {
                    ui.painter().rect_filled(
                        egui::Rect::from_min_max(min, max),
                        0.0,
                        egui::Color32::from_gray(shade),
                    );
                }
                ui.painter().line_segment(
                    [response.rect.left_bottom(), response.rect.right_top()],
                    egui::Stroke::new(1.0, ui.visuals().text_color()),
                );
            }
            #[cfg(test)]
            ui.data_mut(|d| d.insert_temp(egui::Id::new("paint_test_swatch"), response.rect));
            if response.changed() {
                let changed = [color.r(), color.g(), color.b()];
                if changed != rgb || *paint == SwapPaint::Default {
                    *paint = SwapPaint::Custom(changed);
                    hex = format!("#{:02X}{:02X}{:02X}", changed[0], changed[1], changed[2]);
                }
            }
            let edit = ui.add(
                egui::TextEdit::singleline(&mut hex)
                    .desired_width(72.0)
                    .char_limit(7),
            );
            #[cfg(test)]
            ui.data_mut(|d| d.insert_temp(egui::Id::new("paint_test_hex"), edit.id));
            if edit.changed() {
                if let Some(changed) = parse_hex(&hex) {
                    if changed != paint.rgb() || *paint == SwapPaint::Default {
                        *paint = SwapPaint::Custom(changed);
                    }
                }
            }
            if edit.lost_focus() {
                let rgb = paint.rgb();
                hex = format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]);
            }
        });
        ui.data_mut(|d| d.insert_temp(id, hex));
    });
    ui.weak(paint.description());
}
fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let text = text.strip_prefix('#').unwrap_or(text);
    if text.len() != 6 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some([
        u8::from_str_radix(&text[..2], 16).ok()?,
        u8::from_str_radix(&text[2..4], 16).ok()?,
        u8::from_str_radix(&text[4..], 16).ok()?,
    ])
}
pub fn supports(donor: &str) -> bool {
    matches!(
        donor.to_ascii_lowercase().as_str(),
        "body_grain_sf.upk" | "boost_standard_sf.upk"
    )
}

fn named<'a>(props: &'a [Prop], name: &str, ty: &str) -> Result<&'a Prop, String> {
    props
        .iter()
        .find(|p| p.name == name && p.tag_type == ty)
        .ok_or_else(|| format!("Missing {name} ({ty}) in paint template"))
}

fn parameter_name(package: &UpkPackage, prop: &Prop, base: usize) -> Result<String, String> {
    if prop.size != 8 {
        return Err("Invalid paint parameter FName".into());
    }
    let index = usize::try_from(package.read_int(base + prop.value_offset)?)
        .map_err(|_| "Negative paint parameter name")?;
    package
        .names
        .get(index)
        .cloned()
        .ok_or_else(|| "Invalid paint parameter name".into())
}

fn vector_slot(
    package: &UpkPackage,
    props: &[Prop],
    base: usize,
    parameter: &str,
) -> Result<usize, String> {
    let array = named(props, "VectorParameterValues", "ArrayProperty")?;
    let start = base + array.value_offset;
    let end = start
        .checked_add(array.size)
        .ok_or("Paint array overflow")?;
    if array.size < 4 {
        return Err("Truncated paint vector array".into());
    }
    let count = package.read_int(start)?;
    if !(0..=1024).contains(&count) {
        return Err("Invalid paint vector count".into());
    }
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
    if at != end {
        return Err("Unexpected bytes in paint vector array".into());
    }
    found.ok_or_else(|| format!("Material has no {parameter} vector"))
}

/// Paint an already identity-swapped package in staging. The donor name is the
/// original asset, even though the staged filename now identifies the owned item.
/// Known structures are required; unsupported layouts fail before writing.
pub fn bake(path: &Path, donor: &str, paint: SwapPaint) -> Result<usize, String> {
    if matches!(paint, SwapPaint::Preset(id) if id == 0 || id as usize > PALETTE.len()) {
        return Err("Invalid paint preset".into());
    }
    if paint == SwapPaint::Default {
        return Ok(0);
    }
    if !supports(donor) {
        return bake_declared_paint(path, paint);
    }
    let body = donor.eq_ignore_ascii_case("body_grain_sf.upk");
    let mut package = UpkPackage::load(path)?;
    let mut patches: Vec<(usize, Vec<u8>)> = Vec::new();
    let material_name = if body {
        "MIC_Body_Grain"
    } else {
        "MasterBoost_Standard_MIC"
    };
    let parameter = if body { "TrimColor" } else { "CustomColor" };
    let colors = paint.material();
    let mut materials = 0;
    let mut particles = 0;
    for export in &package.exports {
        let class = package.class_of(export);
        if strip(&class) == "MaterialInstanceConstant"
            && strip(&package.name_of(export.object_name)) == material_name
        {
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
            let (props, _) = package.nested_props(
                export.serial_offset + 8,
                export.serial_offset + export.serial_size,
            )?;
            let name = named(&props, "ParameterName", "NameProperty")?;
            if matches!(
                parameter_name(&package, name, 0)?.as_str(),
                "CustomColor" | "InnerColor"
            ) {
                let constant = named(&props, "Constant", "StructProperty")?;
                if constant.struct_name != "Vector" || constant.size != 12 {
                    return Err("Unexpected particle colour layout".into());
                }
                let at = constant.value_offset;
                // Preserve the original HDR peak brightness while replacing hue.
                let old = &package.image[at..at + 12];
                let peak = old
                    .chunks_exact(4)
                    .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
                    .fold(0.0f32, f32::max);
                if !peak.is_finite() || peak <= 0.0 {
                    return Err("Invalid particle brightness".into());
                }
                let max = colors[..3].iter().copied().fold(0.0f32, f32::max);
                let rgb: Vec<u8> = colors[..3]
                    .iter()
                    .flat_map(|v| (if max > 0.0 { v / max * peak } else { 0.0 }).to_le_bytes())
                    .collect();
                patches.push((at, rgb));
                particles += 1;
            }
        }
    }
    if materials != 1 || (!body && particles != 3) {
        return Err(format!(
            "Unsupported donor layout: found {materials} paint materials and {particles} particle colour defaults"
        ));
    }
    for (offset, bytes) in &patches {
        package.patch(*offset, bytes)?;
    }
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
    source: &Path,
    target_backup: &Path,
    destination: &Path,
    base_dir: &Path,
    donor_asset_path: Option<&str>,
    target_asset_path: Option<&str>,
    donor_filename: &str,
    paint: SwapPaint,
) -> Result<(), String> {
    if paint == SwapPaint::Default {
        return crate::cosmetic_upk::patch_for_target(
            source,
            target_backup,
            destination,
            base_dir,
            donor_asset_path,
            target_asset_path,
        );
    }
    let parent = destination.parent().ok_or("No destination directory")?;
    let staging = parent.join(format!(
        ".hebnix-paint-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&staging).map_err(|e| e.to_string())?;
    let result = (|| {
        let staged = staging.join(destination.file_name().ok_or("No destination filename")?);
        crate::cosmetic_upk::patch_for_target(
            source,
            target_backup,
            &staged,
            base_dir,
            donor_asset_path,
            target_asset_path,
        )?;
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

// Follow the asset's own paint settings rather than recolouring every material
// that happens to have a colour-looking property. Shared team colours stay intact.
fn bake_declared_paint(path: &Path, paint: SwapPaint) -> Result<usize, String> {
    use std::collections::{HashMap, HashSet};
    let mut package = UpkPackage::load(path)?;
    let mut materials: HashMap<usize, HashSet<String>> = HashMap::new();
    let mut parameter_names = HashSet::new();
    for e in &package.exports {
        if strip(&package.class_of(e)) != "ProductAttribute_PaintSettings_TA" {
            continue;
        }
        let (props, _) = package.serialized_props(e)?;
        let parameter = if let Some(p) = props
            .iter()
            .find(|p| p.name == "PaintParameterName" && p.tag_type == "NameProperty")
        {
            parameter_name(&package, p, e.serial_offset)?
        } else {
            "CustomColor".into()
        };
        parameter_names.insert(parameter.clone());
        for group in props
            .iter()
            .filter(|p| p.name == "MaterialGroups" && p.struct_name == "PaintMaterialGroup")
        {
            let start = e.serial_offset + group.value_offset;
            let (fields, _) = package.nested_props(start, start + group.size)?;
            for array in fields
                .iter()
                .filter(|p| p.name == "Materials" && p.tag_type == "ArrayProperty")
            {
                let count = package.read_int(array.value_offset)?;
                if count < 0 || array.size != 4 + count as usize * 4 {
                    return Err("Invalid paint material group".into());
                }
                for i in 0..count as usize {
                    let mut reference = package.read_int(array.value_offset + 4 + i * 4)?;
                    let mut visited = HashSet::new();
                    while reference > 0 && visited.insert(reference) {
                        let index = reference as usize - 1;
                        let material = package
                            .exports
                            .get(index)
                            .ok_or("Invalid paint material reference")?;
                        let class = package.class_of(material);
                        if !matches!(strip(&class), "Material" | "MaterialInstanceConstant") {
                            return Err("Paint group references a non-material".into());
                        }
                        materials
                            .entry(index)
                            .or_default()
                            .insert(parameter.clone());
                        let (fields, _) = package.serialized_props(material)?;
                        reference = match fields
                            .iter()
                            .find(|p| p.name == "Parent" && p.tag_type == "ObjectProperty")
                        {
                            Some(p) => package.read_int(material.serial_offset + p.value_offset)?,
                            None => 0,
                        };
                    }
                }
            }
        }
    }
    let colour = paint.material();
    let bytes: Vec<u8> = colour.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut changes = Vec::new();
    for e in &package.exports {
        let class = package.class_of(e);
        if strip(&class) == "MaterialInstanceConstant" {
            if let Some(parameters) = materials.get(&e.table_index) {
                let (props, _) = package.serialized_props(e)?;
                for parameter in parameters {
                    if let Ok(at) = vector_slot(&package, &props, e.serial_offset, parameter) {
                        changes.push((at, bytes.clone()));
                    }
                }
            }
        } else if strip(&class) == "MaterialExpressionVectorParameter" && e.outer_index > 0 {
            if let Some(parameters) = materials.get(&(e.outer_index as usize - 1)) {
                let (props, _) = package.serialized_props(e)?;
                let name = named(&props, "ParameterName", "NameProperty")?;
                if parameters.contains(&parameter_name(&package, name, e.serial_offset)?) {
                    let value = named(&props, "DefaultValue", "StructProperty")?;
                    if value.struct_name != "LinearColor" || value.size != 16 {
                        return Err("Invalid paint default colour".into());
                    }
                    changes.push((e.serial_offset + value.value_offset, bytes.clone()));
                }
            }
        } else if strip(&class) == "DistributionVectorParticleParameter" {
            let start = e.serial_offset
                + if package.read_int(e.serial_offset + 4)? == -1 {
                    8
                } else {
                    4
                };
            let (props, _) = package.nested_props(start, e.serial_offset + e.serial_size)?;
            let Some(name) = props
                .iter()
                .find(|p| p.name == "ParameterName" && p.tag_type == "NameProperty")
            else {
                continue;
            };
            if parameter_names.contains(&parameter_name(&package, name, 0)?) {
                let value = named(&props, "Constant", "StructProperty")?;
                if value.struct_name != "Vector" || value.size != 12 {
                    return Err("Invalid paint particle colour".into());
                }
                let old = &package.image[value.value_offset..value.value_offset + 12];
                let peak = old
                    .chunks_exact(4)
                    .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
                    .fold(0.0f32, f32::max);
                if !peak.is_finite() {
                    return Err("Invalid paint particle brightness".into());
                }
                let max = colour[..3].iter().copied().fold(0.0f32, f32::max);
                let rgb = colour[..3]
                    .iter()
                    .flat_map(|v| {
                        if max > 0.0 {
                            (v / max * peak).to_le_bytes()
                        } else {
                            0.0f32.to_le_bytes()
                        }
                    })
                    .collect();
                changes.push((value.value_offset, rgb));
            }
        }
    }
    changes.sort_by_key(|(at, _)| *at);
    changes.dedup_by_key(|(at, _)| *at);
    if changes.is_empty() {
        return Err("This item's paint is runtime-controlled or external; no local paint parameter could be patched. No swap was installed.".into());
    }
    for (at, bytes) in &changes {
        package.patch(*at, bytes)?;
    }
    let temporary = path.with_extension("paint.tmp");
    let result = (|| {
        package.save(&temporary)?;
        let check = UpkPackage::load(&temporary)?;
        for e in &package.exports {
            if check
                .image
                .get(e.serial_offset..e.serial_offset + e.serial_size)
                != package
                    .image
                    .get(e.serial_offset..e.serial_offset + e.serial_size)
            {
                return Err("Paint export verification failed".into());
            }
        }
        std::fs::copy(&temporary, path).map_err(|e| e.to_string())?;
        Ok(changes.len())
    })();
    let _ = std::fs::remove_file(temporary);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_hex_and_linear_colours() {
        assert_eq!(parse_hex("#12abEF"), Some([18, 171, 239]));
        assert!(parse_hex("#12zz00").is_none());
        assert!(parse_hex("#12345").is_none());
        assert_eq!(SwapPaint::Custom([0; 3]).material(), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(
            SwapPaint::Custom([255, 0, 0]).material(),
            SwapPaint::Red.material()
        );
        assert!((SwapPaint::Custom([128; 3]).material()[0] - 0.21586).abs() < 0.00001);
        assert_eq!(SwapPaint::Crimson.rgb(), [203, 0, 0]);
        assert_eq!(SwapPaint::TitaniumWhite.rgb(), [231; 3]);
        for paint in SwapPaint::ALL {
            assert!(paint.material().iter().all(|v| v.is_finite()));
        }
    }
    #[test]
    fn opening_default_picker_does_not_select_custom() {
        use eframe::egui;
        let ctx = egui::Context::default();
        let mut paint = SwapPaint::Default;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| controls(ui, &mut paint));
        assert_eq!(paint, SwapPaint::Default);
        let rect = ctx
            .data_mut(|d| d.get_temp::<egui::Rect>(egui::Id::new("paint_test_swatch")))
            .unwrap();
        let pos = rect.center();
        let input = egui::RawInput {
            events: vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                },
            ],
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| controls(ui, &mut paint));
        assert_eq!(paint, SwapPaint::Default);
    }
    #[test]
    fn editing_hex_switches_preset_to_custom() {
        use eframe::egui;
        let ctx = egui::Context::default();
        let mut paint = SwapPaint::Crimson;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| controls(ui, &mut paint));
        let id = ctx
            .data_mut(|d| d.get_temp::<egui::Id>(egui::Id::new("paint_test_hex")))
            .unwrap();
        ctx.memory_mut(|m| m.request_focus(id));
        let modifiers = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let input = egui::RawInput {
            modifiers,
            events: vec![
                egui::Event::Key {
                    key: egui::Key::A,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
                egui::Event::Text("#123456".into()),
            ],
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| controls(ui, &mut paint));
        assert_eq!(paint, SwapPaint::Custom([0x12, 0x34, 0x56]));
    }
    #[test]
    fn saved_paints_remain_compatible() {
        for paint in [
            SwapPaint::Default,
            SwapPaint::Crimson,
            SwapPaint::Red,
            SwapPaint::TitaniumWhite,
            SwapPaint::Custom([1, 2, 3]),
            SwapPaint::Preset(18),
        ] {
            assert_eq!(
                serde_json::from_str::<SwapPaint>(&serde_json::to_string(&paint).unwrap()).unwrap(),
                paint
            );
        }
        assert_eq!(
            serde_json::from_str::<SwapPaint>("\"Crimson\"").unwrap(),
            SwapPaint::Crimson
        );
    }
}

#[cfg(test)]
mod local_paint_tests {
    use super::*;
    #[test]
    #[ignore = "Requires installed packages; set HEBNIX_UPK_DIR"]
    fn verifies_declared_paint_on_copies() {
        let cooked =
            std::path::PathBuf::from(std::env::var_os("HEBNIX_UPK_DIR").expect("HEBNIX_UPK_DIR"));
        let work =
            std::env::temp_dir().join(format!("hebnix-paint-test-{}", rand::random::<u64>()));
        std::fs::create_dir(&work).unwrap();
        for name in [
            "Hat_Halo_SF.upk",
            "skin_zomba_SF.upk",
            "body_grain_SF.upk",
            "boost_standard_SF.upk",
        ] {
            let output = work.join(name);
            std::fs::copy(cooked.join(name), &output).unwrap();
            let original = std::fs::read(&output).unwrap();
            assert_eq!(bake(&output, name, SwapPaint::Default).unwrap(), 0);
            assert_eq!(std::fs::read(&output).unwrap(), original);
            let before = UpkPackage::load(&output).unwrap();
            for paint in [
                SwapPaint::Custom([24, 102, 235]),
                SwapPaint::Custom([0; 3]),
                SwapPaint::Crimson,
            ] {
                std::fs::write(&output, &original).unwrap();
                let count = bake(&output, name, paint).unwrap();
                assert!(count > 0);
                let after = UpkPackage::load(&output).unwrap();
                for (a, b) in before.exports.iter().zip(&after.exports) {
                    let class = before.class_of(a);
                    if !class.starts_with("Material")
                        && !class.starts_with("DistributionVectorParticleParameter")
                    {
                        assert_eq!(
                            &before.image[a.serial_offset..a.serial_offset + a.serial_size],
                            &after.image[b.serial_offset..b.serial_offset + b.serial_size],
                            "{name}: unrelated export changed"
                        );
                    }
                }
                println!(
                    "{name}: {} patched {count} colour values",
                    paint.description()
                );
            }
            std::fs::remove_file(output).unwrap();
        }
        std::fs::remove_dir(work).unwrap();
    }
}
