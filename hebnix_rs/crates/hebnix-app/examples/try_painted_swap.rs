//! Generate experimental painted packages without modifying the game install.
//! cargo run -p hebnix-app --example try_painted_swap -- <CookedPCConsole> <output-dir>
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
pub mod patcher {
    pub use crate::{patch_core, upk_package};
}
use std::path::PathBuf;
use painted_swap::SwapPaint;
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 { return Err("Usage: try_painted_swap <CookedPCConsole> <output-dir>".into()); }
    let cooked = PathBuf::from(&args[0]);
    let output = PathBuf::from(&args[1]);
    std::fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    if output.canonicalize().map_err(|e| e.to_string())? == cooked.canonicalize().map_err(|e| e.to_string())? {
        return Err("Use a separate output directory; this example never installs live packages".into());
    }
    for (donor, target, paint) in [
        ("body_grain_SF.upk", "Body_Octane_SF.upk", SwapPaint::TitaniumWhite),
        ("Boost_Standard_SF.upk", "Boost_LightTrail_SF.upk", SwapPaint::Crimson),
    ] {
        let pristine = |name: &str| { let backup = cooked.join("Backups").join(format!("{name}.bak")); if backup.is_file() { backup } else { cooked.join(name) } };
        let source = pristine(donor);
        let original = pristine(target);
        let before_source = std::fs::read(&source).map_err(|e| e.to_string())?;
        let before_target = std::fs::read(&original).map_err(|e| e.to_string())?;
        let destination = output.join(target);
        painted_swap::patch_for_target(&source, &original, &destination, &cooked, None, None, donor, paint)?;
        let painted = upk_package::UpkPackage::load(&destination)?;
        let baseline_path = output.join("baseline");
        std::fs::create_dir_all(&baseline_path).map_err(|e| e.to_string())?;
        let baseline_file = baseline_path.join(target);
        cosmetic_upk::patch_for_target(&source, &original, &baseline_file, &cooked, None, None)?;
        let baseline = upk_package::UpkPackage::load(&baseline_file)?;
        assert_eq!(baseline.exports.len(), painted.exports.len());
        let mut changed = Vec::new();
        for (left, right) in baseline.exports.iter().zip(&painted.exports) {
            assert_eq!((left.serial_offset, left.serial_size), (right.serial_offset, right.serial_size));
            let range = left.serial_offset..left.serial_offset + left.serial_size;
            if baseline.image[range.clone()] != painted.image[range] { changed.push(painted.name_of(right.object_name)); }
        }
        assert_eq!(changed.len(), if paint == SwapPaint::TitaniumWhite { 1 } else { 4 });
        // Default is exactly a no-op; unsupported donors fail without changing output.
        let saved = std::fs::read(&destination).map_err(|e| e.to_string())?;
        assert_eq!(painted_swap::bake(&destination, donor, SwapPaint::Default)?, 0);
        assert!(painted_swap::bake(&destination, "unsupported.upk", paint).is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), saved);
        assert_eq!(std::fs::read(&source).unwrap(), before_source);
        assert_eq!(std::fs::read(&original).unwrap(), before_target);
        println!("{}: {} -> {}. Changed exports: {:?}", paint.label(), donor, destination.display(), changed);
        let donor_thumb = donor.replace("_SF.upk", "_T_SF.upk");
        let target_thumb = target.replace("_SF.upk", "_T_SF.upk");
        cosmetic_upk::patch_for_target(&pristine(&donor_thumb), &pristine(&target_thumb), &output.join(&target_thumb), &cooked, None, None)?;
        // Thumbnail is the donor's original preview; it is not recoloured.
        upk_package::UpkPackage::load(&output.join(target_thumb))?;
    }
    Ok(())
}
