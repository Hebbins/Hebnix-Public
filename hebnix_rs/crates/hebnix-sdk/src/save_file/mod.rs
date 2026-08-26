//! decrypt + read RL .save files. the property tree is a serde_json::Value
//! with typed accessors on top via SaveData.

pub mod accessors;
pub mod binary_parser;
pub mod binary_serializer;
pub mod configuration;
pub mod crypto;
pub mod file_io;
pub mod models;

pub use accessors::SaveData;
pub use configuration::{
    ConfigurationEditorKind, ConfigurationValue, list_configuration, restore_configuration,
    update_configuration, validate_configuration,
};
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

/// one active Rocket League account save selected using the same rules as hebstool.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SaveAccount {
    pub account_id: String,
    pub display_name: String,
    pub path: PathBuf,
}

fn account_display_names(save_dir: &Path) -> std::collections::HashMap<String, String> {
    let mut names = std::collections::HashMap::new();
    let Some(tagame) = save_dir.parent().and_then(Path::parent) else {
        return names;
    };
    let logs = tagame.join("Logs");
    let Ok(entries) = std::fs::read_dir(logs) else {
        return names;
    };
    let id_re = regex::Regex::new(r"-epicuserid=([0-9a-fA-F]{32})").expect("valid epic id regex");
    let name_re =
        regex::Regex::new(r#"-epicusername=(?:"([^"]+)"|(\S+))"#).expect("valid epic name regex");
    let mut logs: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("log"))
        })
        .collect();
    logs.sort_by_key(|path| {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
    });
    logs.reverse();
    for path in logs {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in text.lines() {
            let Some(id) = id_re
                .captures(line)
                .and_then(|capture| capture.get(1))
                .map(|value| value.as_str().to_string())
            else {
                continue;
            };
            if names.contains_key(&id) {
                continue;
            }
            if let Some(name) = name_re
                .captures(line)
                .and_then(|capture| capture.get(1).or_else(|| capture.get(2)))
                .map(|value| value.as_str().trim().to_string())
                .filter(|value| !value.is_empty())
            {
                names.insert(id, name);
            }
        }
    }
    names
}

/// return one current save per account, preferring base saves over numbered backups.
pub fn find_save_accounts(save_data_path: Option<&Path>) -> Vec<SaveAccount> {
    let Some(dir) = save_data_path
        .map(PathBuf::from)
        .or_else(detect_save_data_path)
    else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let account_re = regex::Regex::new(r"^(?:[0-9a-fA-F]{32}|\d{10,20})(?:_\d+)?$")
        .expect("valid account regex");
    let numbered_re = regex::Regex::new(r"_\d+$").expect("valid numbered save regex");
    let mut candidates: std::collections::HashMap<
        String,
        Vec<(bool, std::time::SystemTime, PathBuf)>,
    > = std::collections::HashMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("save"))
        {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if !account_re.is_match(stem) {
            continue;
        }
        let account_id = numbered_re.replace(stem, "").to_string();
        let modified = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        candidates.entry(account_id).or_default().push((
            numbered_re.is_match(stem),
            modified,
            path,
        ));
    }
    let names = account_display_names(&dir);
    let mut accounts: Vec<_> = candidates
        .into_iter()
        .filter_map(|(account_id, mut entries)| {
            entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));
            let (_, _, path) = entries.into_iter().next()?;
            let short_id: String = account_id.chars().take(8).collect();
            let display_name = names
                .get(&account_id)
                .cloned()
                .unwrap_or_else(|| format!("Account ({short_id}...)"));
            Some(SaveAccount {
                account_id,
                display_name,
                path,
            })
        })
        .collect();
    accounts.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    accounts
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
