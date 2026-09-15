//! Shared Hebnix runtime and data paths.

use std::path::PathBuf;

/// The root for all Hebnix-owned files.
///
/// On Windows this is `%AppData%\Hebnix`. `HEBNIX_BASE_DIR` remains available
/// for isolated development and test runs.
pub fn base_dir() -> PathBuf {
    let dir = std::env::var_os("HEBNIX_BASE_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::data_dir().map(|dir| dir.join("Hebnix")))
        .unwrap_or_else(|| std::env::temp_dir().join("Hebnix"));

    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "failed to create Hebnix data directory {}: {error}",
            dir.display()
        );
    }
    dir
}
