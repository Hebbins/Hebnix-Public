//! Recolour an existing Fennec swap in a separate output file, preserving its identity.
#![allow(dead_code)]
#[path = "../src/patcher/patch_core.rs"]
pub mod patch_core;
#[path = "../src/patcher/upk_package.rs"]
pub mod upk_package;
#[path = "../src/patcher/upk_keys.rs"]
pub mod upk_keys;
#[path = "../src/patcher/cosmetic_upk.rs"]
pub mod cosmetic_upk;
#[path = "../src/patcher/painted_swap.rs"]
pub mod painted_swap;
pub mod patcher { pub use crate::{patch_core, upk_package}; }
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 { return Err("Usage: try_red_fennec <existing-fennec-swap> <separate-output>".into()); }
    let source = std::path::Path::new(&args[0]);
    let destination = std::path::Path::new(&args[1]);
    if destination.exists() && source.canonicalize().unwrap() == destination.canonicalize().unwrap() {
        return Err("Output must differ from source".into());
    }
    let before = upk_package::UpkPackage::load(source)?;
    std::fs::copy(source, destination).map_err(|e| e.to_string())?;
    assert_eq!(painted_swap::bake(destination, "body_grain_SF.upk", painted_swap::SwapPaint::Red)?, 1);
    let after = upk_package::UpkPackage::load(destination)?;
    assert_eq!(before.exports.len(), after.exports.len());
    let mut changed = Vec::new();
    for (left, right) in before.exports.iter().zip(&after.exports) {
        assert_eq!((left.serial_offset, left.serial_size), (right.serial_offset, right.serial_size));
        let range = left.serial_offset..left.serial_offset + left.serial_size;
        if before.image[range.clone()] != after.image[range] { changed.push(after.name_of(right.object_name)); }
    }
    assert_eq!(changed, vec!["MIC_Body_Grain_-2"]);
    println!("Verified pure red [1, 0, 0, 1]; only the Fennec body material export changed: {}", destination.display());
    Ok(())
}
