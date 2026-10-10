//! Closed-game save cleanup. Preflight every file before installing any change.
use hebnix_sdk::save_file::item_cleanup::{CleanupPlan, plan_cleanup};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

fn require_closed() -> Result<(), String> {
    if hebnix_sdk::process::detector::rocket_league_running_checked()? {
        Err("Rocket League restarted; close it and Check Again to finish save cleanup".into())
    } else {
        Ok(())
    }
}
fn regular(path: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err("save path is a reparse point; refusing to follow it".into());
        }
    }
    if !meta.is_file() || meta.len() > 64 * 1024 * 1024 {
        return Err("save is not a supported regular file".into());
    }
    Ok(())
}
fn account_filename(path: &Path) -> bool {
    if !path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("save"))
    {
        return false;
    }
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    let mut parts = stem.split('_');
    let base = parts.next().unwrap_or("");
    let valid = (base.len() == 32 && base.bytes().all(|b| b.is_ascii_hexdigit()))
        || ((10..=20).contains(&base.len()) && base.bytes().all(|b| b.is_ascii_digit()));
    valid
        && parts
            .next()
            .is_none_or(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
        && parts.next().is_none()
}
fn discover() -> Result<Vec<PathBuf>, String> {
    let docs = dirs::document_dir().ok_or("Cannot locate Rocket League's Documents directory")?;
    let mut paths = Vec::new();
    for relative in [
        hebnix_sdk::utils::constants::SAVE_PATH_STEAM,
        hebnix_sdk::utils::constants::SAVE_PATH_EPIC,
    ] {
        let dir = docs.join(relative);
        if !dir.exists() {
            continue;
        }
        let root = std::fs::canonicalize(&dir).map_err(|e| e.to_string())?;
        for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if !account_filename(&path) {
                continue;
            }
            regular(&path)?;
            let resolved = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
            if resolved.parent() != Some(root.as_path()) {
                return Err("save escaped its expected directory".into());
            }
            paths.push(resolved);
        }
    }
    // Match the existing save selector: the most recently saved local account.
    // Its numbered rotations must be cleaned too, or RL can recover the old item.
    let latest = paths
        .iter()
        .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .cloned();
    if let Some(latest) = latest {
        let account = latest
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap()
            .split('_')
            .next()
            .unwrap()
            .to_owned();
        paths.retain(|p| {
            p.parent() == latest.parent()
                && p.file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.split('_').next() == Some(account.as_str()))
        });
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}
struct Prepared {
    path: PathBuf,
    before: Vec<u8>,
    plan: CleanupPlan,
    backup: PathBuf,
    temp: PathBuf,
}
impl Drop for Prepared {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.temp);
    }
}

fn execute(
    paths: &[PathBuf],
    targets: &BTreeSet<String>,
    generation: u64,
    mut check: impl FnMut() -> Result<(), String>,
    mut planner: impl FnMut(&[u8], &BTreeSet<String>) -> Result<CleanupPlan, String>,
) -> Result<String, String> {
    check()?;
    let mut prepared = Vec::new();
    for path in paths {
        regular(path)?;
        let before = std::fs::read(path).map_err(|e| e.to_string())?;
        let plan = planner(&before, targets).map_err(|e| format!("{}: {e}", path.display()))?;
        if !plan.report.changed() {
            continue;
        }
        let token = format!("{generation}-{:032x}", rand::random::<u128>());
        let name = path
            .file_name()
            .ok_or("invalid save filename")?
            .to_string_lossy();
        let backup = path.with_file_name(format!("{name}.hebnix-clear-{token}.bak"));
        let temp = path.with_file_name(format!("{name}.hebnix-clear-{token}.tmp"));
        prepared.push(Prepared {
            path: path.clone(),
            before,
            plan,
            backup,
            temp,
        });
        check()?;
    }
    // Immutable backups and flushed replacement files exist before the first install.
    for p in &prepared {
        check()?;
        write_new(&p.backup, &p.before)?;
        write_new(&p.temp, &p.plan.bytes)?;
        if std::fs::read(&p.backup).map_err(|e| e.to_string())? != p.before
            || std::fs::read(&p.temp).map_err(|e| e.to_string())? != p.plan.bytes
        {
            return Err("save backup/staging verification failed".into());
        }
    }
    let mut instances = 0;
    let mut slots = 0;
    for p in &prepared {
        check()?;
        regular(&p.path)?;
        // Deny writers while validating the snapshot. Windows requires this
        // handle to close before MoveFileEx can replace the destination.
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1 | 4);
        }
        let _lock = options
            .open(&p.path)
            .map_err(|e| format!("Save is busy; retry after closing other save editors: {e}"))?;
        if std::fs::read(&p.path).map_err(|e| e.to_string())? != p.before {
            return Err("Save changed during cleanup; originals backed up, Check Again".into());
        }
        check()?;
        drop(_lock);
        super::store::replace(&p.temp, &p.path)?;
        if std::fs::read(&p.path).map_err(|e| e.to_string())? != p.plan.bytes {
            return Err("Installed save verification failed; recovery backup retained".into());
        }
        check()?;
        instances += p.plan.report.inventory_instances;
        slots += p.plan.report.equipped_slots;
    }
    check()?;
    Ok(format!(
        "Cleaned {} files for the most recently saved account ({instances} inventory entries, {slots} equipped slots, including rotating copies). Original saves backed up beside each changed file.",
        prepared.len()
    ))
}
pub fn clear_saved_instances(
    targets: &BTreeSet<String>,
    generation: u64,
) -> Result<String, String> {
    require_closed()?;
    if targets.is_empty() {
        return Ok("No tracked instances require save cleanup.".into());
    }
    let paths = discover()?;
    if paths.is_empty() {
        return Err(
            "No Rocket League account saves found; cannot verify saved-item cleanup".into(),
        );
    }
    execute(&paths, targets, generation, require_closed, plan_cleanup)
}

