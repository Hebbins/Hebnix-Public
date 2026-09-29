#![allow(dead_code)]
#[path = "../src/patcher/patch_core.rs"]
pub mod patch_core;
#[path = "../src/patcher/upk_package.rs"]
pub mod upk_package;
#[path = "../src/patcher/upk_keys.rs"]
pub mod upk_keys;
pub mod patcher {
    pub use crate::{patch_core, upk_package};
}

fn dump(p: &upk_package::UpkPackage, props: &[upk_package::Prop], base: usize, indent: usize) {
    for prop in props {
        let start = base + prop.value_offset;
        let end = start + prop.size;
        let bytes = &p.image[start..end];
        let value = match prop.tag_type.as_str() {
            "NameProperty" => p.names[p.read_int(start).unwrap() as usize].clone(),
            "ObjectProperty" => p.obj_name(p.read_int(start).unwrap()),
            "FloatProperty" => format!("{}", f32::from_le_bytes(bytes.try_into().unwrap())),
            "StructProperty" if prop.struct_name == "LinearColor" => format!("{:?}", bytes.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect::<Vec<_>>()),
            "BoolProperty" => format!("{:?}", prop.bool_value),
            _ => format!("{:?}", &bytes[..bytes.len().min(20)]),
        };
        println!("{}{} {} @{} = {}", " ".repeat(indent), prop.name, prop.struct_name, start, value);
        if prop.tag_type == "StructProperty" {
            if let Ok((nested, _)) = p.nested_props(start, end) { dump(p, &nested, 0, indent+2); }
        } else if prop.tag_type == "ArrayProperty" && bytes.len() >= 4 {
            let count = p.read_int(start).unwrap();
            let mut at = start + 4;
            for _ in 0..count.min(50) {
                if let Ok((nested, next)) = p.nested_props(at, end) { dump(p, &nested, 0, indent+2); at = next; } else { break; }
            }
        }
    }
}
fn main() -> Result<(), String> {
    for path in std::env::args().skip(1) {
        let package = upk_package::UpkPackage::load(std::path::Path::new(&path))?;
        println!("PACKAGE {path}");
        for name in &package.names {
            if name.to_lowercase().contains("paint") || name.to_lowercase().contains("color") {
                println!("NAME {name}");
            }
        }
        for export in &package.exports {
            let class = package.class_of(export); if let Ok(filter) = std::env::var("HEBNIX_INSPECT_OBJECT") { if !package.name_of(export.object_name).contains(&filter) { continue; } }
            let filter = std::env::var("HEBNIX_INSPECT_CLASSES").unwrap_or_else(|_| "MaterialInstance,Product,CarMesh,FXActor,ParticleModuleParameterDynamic".into()); if !filter.split(',').any(|s| class.contains(s)) { continue; }
            println!("EXPORT {} {} @{}", package.name_of(export.object_name), class, export.serial_offset);
            println!("HEADER {:?}", &package.image[export.serial_offset..export.serial_offset+export.serial_size.min(64)]); let props = package.serialized_props(export).map(|(p, _)| p).unwrap_or_else(|_| package.parse_props(export)); dump(&package, &props, export.serial_offset, 2);
        }
        if let Some(dir) = std::env::var_os("HEBNIX_PAINT_DUMP") {
            let name = std::path::Path::new(&path).file_name().unwrap().to_string_lossy();
            std::fs::write(std::path::Path::new(&dir).join(format!("{name}.image")), &package.image).map_err(|e| e.to_string())?;
            std::fs::write(std::path::Path::new(&dir).join(format!("{name}.names.json")), serde_json::to_vec(&package.names).unwrap()).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
