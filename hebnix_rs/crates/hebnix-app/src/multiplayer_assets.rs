//! Multiplayer support programs embedded only in the full Hebnix binary.

use std::path::Path;

// Generated recursively by build.rs from assets/multiplayer.
include!(concat!(env!("OUT_DIR"), "/multiplayer_assets.rs"));

/// Extracts the embedded bundle to `%AppData%\\Hebnix\\multiplayer-lan`.
/// Existing files are updated only when their bundled contents changed.
pub fn ensure_present(base_dir: &Path) -> std::io::Result<()> {
    let multiplayer_dir = base_dir.join("multiplayer-lan");
    for &(relative_path, bytes) in MULTIPLAYER_ASSETS {
        crate::runtime_assets::write_if_changed(&multiplayer_dir.join(relative_path), bytes)?;
    }
    Ok(())
}
