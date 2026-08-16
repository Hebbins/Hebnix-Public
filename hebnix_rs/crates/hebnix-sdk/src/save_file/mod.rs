//! decrypt + read RL .save files. the property tree is a serde_json::Value
//! with typed accessors on top via SaveData.

pub mod accessors;
pub mod binary_parser;
pub mod binary_serializer;
pub mod crypto;
pub mod file_io;
pub mod models;

pub use accessors::SaveData;
pub use file_io::{RawSave, assemble_savedata, parse_savedata};
pub use models::*;

use std::path::{Path, PathBuf};

use crate::utils::constants::{SAVE_PATH_EPIC, SAVE_PATH_STEAM};

/// decrypt + parse a .save into a typed SaveData
pub fn load(filepath: &Path, check_crc: bool) -> Result<SaveData, crypto::SaveError> {
    let raw = parse_savedata(filepath, check_crc)?;
    Ok(SaveData::from_raw(raw, filepath.to_path_buf()))
}

/// newest *.save in the DBE_Production dir
pub fn find_save_file(save_data_path: Option<&Path>) -> Option<PathBuf> {
    let dir = match save_data_path {
        Some(p) => p.to_path_buf(),
        None => detect_save_data_path()?,
    };
    if !dir.is_dir() {
        return None;
    }
    let mut saves: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext.eq_ignore_ascii_case("save"))
                .unwrap_or(false)
        })
        .filter_map(|e| {
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((mtime, e.path()))
        })
        .collect();
    saves.sort_by(|a, b| b.0.cmp(&a.0));
    saves.into_iter().next().map(|(_, p)| p)
}

/// find the right save data path (steam or epic). tries the running game
/// first, else picks whichever path has the newest .save.
pub fn detect_save_data_path() -> Option<PathBuf> {
    if let Some(rl) = crate::process::find_rocket_league() {
        return Some(rl.save_data_path);
    }

    let docs = dirs::document_dir()?;
    let mut best_path: Option<PathBuf> = None;
    let mut best_mtime: Option<std::time::SystemTime> = None;

    for rel in [SAVE_PATH_STEAM, SAVE_PATH_EPIC] {
        let candidate = docs.join(rel);
        if !candidate.is_dir() {
            continue;
        }
        if let Some(latest) = find_save_file(Some(&candidate)) {
            if let Ok(mtime) = latest.metadata().and_then(|m| m.modified()) {
                if best_mtime.map(|b| mtime > b).unwrap_or(true) {
                    best_mtime = Some(mtime);
                    best_path = Some(candidate);
                }
            }
        }
    }
    best_path
}
