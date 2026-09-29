use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

fn game_build(cooked_pc: &Path) -> Result<String, String> {
    let root = cooked_pc
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "Invalid Rocket League CookedPCConsole path".to_string())?;
    let exe = root.join("Binaries").join("Win64").join("RocketLeague.exe");
    let bytes = fs::read(&exe)
        .map_err(|e| format!("Could not read {} to check backup compatibility: {e}", exe.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

/// Refuse legacy and cross-build backups before they can replace current game files.
pub fn check(
    cooked_pc: &Path,
    backups_dir: &Path,
    marker_name: &str,
    has_backups: impl Fn(&str) -> bool,
) -> Result<(), String> {
    fs::create_dir_all(backups_dir)
        .map_err(|e| format!("Could not create {}: {e}", backups_dir.display()))?;
    let current = game_build(cooked_pc)?;
    let marker = backups_dir.join(marker_name);
    let existing = fs::read_dir(backups_dir)
        .map_err(|e| format!("Could not inspect {}: {e}", backups_dir.display()))?
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_str().is_some_and(&has_backups));
    if !existing {
        fs::write(&marker, &current)
            .map_err(|e| format!("Could not write {}: {e}", marker.display()))?;
        return Ok(());
    }
    let recorded = fs::read_to_string(&marker).unwrap_or_default();
    if recorded.trim() != current {
        return Err(format!(
            "Backup is from an unknown or older Rocket League build. No game files were changed. Verify Rocket League in Epic, then apply a new ball or boost meter to recreate the backup. Do not restore the old backup in {}.",
            backups_dir.display()
        ));
    }
    Ok(())
}


/// Recreate outdated feature backups from installed game packages before applying a patch.
/// Old backups are retained in an Outdated directory for inspection.
pub fn prepare(
    cooked_pc: &Path,
    backups_dir: &Path,
    marker_name: &str,
    has_backups: impl Fn(&str) -> bool,
) -> Result<bool, String> {
    fs::create_dir_all(backups_dir)
        .map_err(|e| format!("Could not create {}: {e}", backups_dir.display()))?;
    let current = game_build(cooked_pc)?;
    let marker = backups_dir.join(marker_name);
    let old_files: Vec<_> = fs::read_dir(backups_dir)
        .map_err(|e| format!("Could not inspect {}: {e}", backups_dir.display()))?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_str().is_some_and(&has_backups))
        .map(|entry| entry.path())
        .collect();
    if old_files.is_empty() {
        fs::write(&marker, current)
            .map_err(|e| format!("Could not write {}: {e}", marker.display()))?;
        return Ok(false);
    }
    if fs::read_to_string(&marker).is_ok_and(|saved| saved.trim() == current) {
        return Ok(false);
    }

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let archive = backups_dir.join(format!("Outdated-{}-{nonce}", marker_name.trim_end_matches(".sha256")));
    let staged = backups_dir.join(format!(".refresh-{nonce}"));
    fs::create_dir(&staged).map_err(|e| format!("Could not stage new backups: {e}"))?;
    for old in &old_files {
        let name = old.file_name().and_then(|name| name.to_str())
            .ok_or_else(|| "Invalid backup filename".to_string())?;
        if let Some(live_name) = name.strip_suffix(".bak") {
            let live = cooked_pc.join(live_name);
            fs::copy(&live, staged.join(name))
                .map_err(|e| format!("Could not recreate {name} from {}: {e}", live.display()))?;
        }
    }
    fs::create_dir(&archive).map_err(|e| format!("Could not archive old backups: {e}"))?;
    for old in &old_files {
        fs::rename(old, archive.join(old.file_name().unwrap()))
            .map_err(|e| format!("Could not archive {}: {e}", old.display()))?;
    }
    if marker.exists() {
        fs::rename(&marker, archive.join(marker_name))
            .map_err(|e| format!("Could not archive old build marker: {e}"))?;
    }
    for staged_file in fs::read_dir(&staged).map_err(|e| e.to_string())? {
        let staged_file = staged_file.map_err(|e| e.to_string())?.path();
        fs::rename(&staged_file, backups_dir.join(staged_file.file_name().unwrap()))
            .map_err(|e| format!("Could not install fresh backup: {e}"))?;
    }
    fs::remove_dir(&staged).map_err(|e| format!("Could not remove staging directory: {e}"))?;
    fs::write(&marker, current)
        .map_err(|e| format!("Could not write {}: {e}", marker.display()))?;
    Ok(true)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn stale_backup_is_archived_and_recreated() {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("hebnix-backup-guard-{}-{nonce}", std::process::id()));
        let cooked = root.join("TAGame").join("CookedPCConsole");
        let exe = root.join("Binaries").join("Win64").join("RocketLeague.exe");
        let backups = cooked.join("Backups");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::create_dir_all(&backups).unwrap();
        fs::write(&exe, b"build one").unwrap();
        fs::write(cooked.join("Mutators_Balls_SF.upk"), b"current package").unwrap();
        let backup = backups.join("Mutators_Balls_SF.upk.bak");
        fs::write(&backup, b"old package").unwrap();
        fs::write(backups.join("Textures2.tfc_123.bin"), b"old region").unwrap();
        let predicate = |name: &str| name == "Mutators_Balls_SF.upk.bak" || name == "Textures2.tfc_123.bin";
        assert!(check(&cooked, &backups, "ball-build.sha256", predicate).is_err());
        assert!(prepare(&cooked, &backups, "ball-build.sha256", predicate).unwrap());
        assert_eq!(fs::read(&backup).unwrap(), b"current package");
        let archive = fs::read_dir(&backups).unwrap().filter_map(Result::ok)
            .find(|entry| entry.file_name().to_string_lossy().starts_with("Outdated-ball-build-"))
            .unwrap().path();
        assert_eq!(fs::read(archive.join("Mutators_Balls_SF.upk.bak")).unwrap(), b"old package");
        assert_eq!(fs::read(archive.join("Textures2.tfc_123.bin")).unwrap(), b"old region");
        assert!(!backups.join("Textures2.tfc_123.bin").exists());
        assert!(!prepare(&cooked, &backups, "ball-build.sha256", predicate).unwrap());
        fs::write(&exe, b"build two").unwrap();
        fs::write(cooked.join("Mutators_Balls_SF.upk"), b"newer package").unwrap();
        assert!(check(&cooked, &backups, "ball-build.sha256", predicate).is_err());
        assert!(prepare(&cooked, &backups, "ball-build.sha256", predicate).unwrap());
        assert_eq!(fs::read(&backup).unwrap(), b"newer package");
        fs::remove_dir_all(root).unwrap();
    }
}