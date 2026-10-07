use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

const INSTALL_MARKER: &str = "hebnix-install-build.sha256";
const STALE_RESTORE_MESSAGE: &str = "Rocket League was updated. Hebnix discarded all old patch backups and active entries instead of restoring files from the previous build. Apply the item again to create a fresh backup.";

pub(crate) fn build_id(cooked_pc: &Path) -> Result<String, String> {
    let root = cooked_pc
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "Invalid Rocket League CookedPCConsole path".to_string())?;
    let exe = [
        root.join("TAGame")
            .join("Binaries")
            .join("Win64")
            .join("RocketLeague.exe"),
        root.join("Binaries").join("Win64").join("RocketLeague.exe"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .ok_or_else(|| format!("Could not find RocketLeague.exe below {}", root.display()))?;
    let bytes = fs::read(&exe).map_err(|e| {
        format!(
            "Could not read {} to check backup compatibility: {e}",
            exe.display()
        )
    })?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn backup_root<'a>(backups_dir: &'a Path) -> &'a Path {
    backups_dir
        .ancestors()
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("Backups"))
        })
        .unwrap_or(backups_dir)
}

fn remove_entry(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .map_err(|e| format!("Could not discard stale backup {}: {e}", path.display()))
}

fn discard_root(backups_dir: &Path) -> Result<(), String> {
    let root = backup_root(backups_dir);
    let install_marker = root.join(INSTALL_MARKER);
    for entry in
        fs::read_dir(root).map_err(|e| format!("Could not inspect {}: {e}", root.display()))?
    {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path != install_marker {
            remove_entry(&path)?;
        }
    }
    Ok(())
}

/// Synchronize every Hebnix backup belonging to this Rocket League install.
///
/// A missing marker is deliberately treated as untrusted when backup entries already exist.
/// This makes upgrades from older Hebnix releases fail safe: an unknown backup can never be
/// copied over files from a newer Rocket League build.
pub fn synchronize_install(cooked_pc: &Path, backups_dir: &Path) -> Result<bool, String> {
    let root = backup_root(backups_dir);
    fs::create_dir_all(root).map_err(|e| format!("Could not create {}: {e}", root.display()))?;
    let current = build_id(cooked_pc)?;
    let marker = root.join(INSTALL_MARKER);
    let recorded = fs::read_to_string(&marker).unwrap_or_default();
    let entries: Vec<_> = fs::read_dir(root)
        .map_err(|e| format!("Could not inspect {}: {e}", root.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path != &marker)
        .collect();
    let stale = !entries.is_empty() && recorded.trim() != current;
    if stale {
        discard_root(backups_dir)?;
    }
    if recorded.trim() != current {
        fs::write(&marker, &current)
            .map_err(|e| format!("Could not write {}: {e}", marker.display()))?;
    }
    Ok(stale)
}

/// Guard a restore that only needs install-wide protection and has no exclusive backup names.
pub fn check_install(cooked_pc: &Path, backups_dir: &Path) -> Result<(), String> {
    if synchronize_install(cooked_pc, backups_dir)? {
        Err(STALE_RESTORE_MESSAGE.into())
    } else {
        Ok(())
    }
}

/// Refuse legacy and cross-build backups before they can replace current game files.
pub fn check(
    cooked_pc: &Path,
    backups_dir: &Path,
    marker_name: &str,
    has_backups: impl Fn(&str) -> bool,
) -> Result<(), String> {
    check_install(cooked_pc, backups_dir)?;
    fs::create_dir_all(backups_dir)
        .map_err(|e| format!("Could not create {}: {e}", backups_dir.display()))?;
    let current = build_id(cooked_pc)?;
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
        discard_root(backups_dir)?;
        return Err(STALE_RESTORE_MESSAGE.into());
    }
    Ok(())
}

/// Recreate outdated feature backups from installed game packages before applying a patch.
/// Old backups and their active-state entries are discarded rather than retained or restored.
pub fn prepare(
    cooked_pc: &Path,
    backups_dir: &Path,
    marker_name: &str,
    has_backups: impl Fn(&str) -> bool,
) -> Result<bool, String> {
    let discarded = synchronize_install(cooked_pc, backups_dir)?;
    fs::create_dir_all(backups_dir)
        .map_err(|e| format!("Could not create {}: {e}", backups_dir.display()))?;
    let current = build_id(cooked_pc)?;
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
        return Ok(discarded);
    }
    if fs::read_to_string(&marker).is_ok_and(|saved| saved.trim() == current) {
        return Ok(discarded);
    }

    discard_root(backups_dir)?;
    fs::create_dir_all(backups_dir)
        .map_err(|e| format!("Could not recreate {}: {e}", backups_dir.display()))?;
    fs::write(backups_dir.join(marker_name), current)
        .map_err(|e| format!("Could not write {}: {e}", marker.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TestInstall(PathBuf);

    impl TestInstall {
        fn new() -> (Self, PathBuf, PathBuf) {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "hebnix-backup-guard-{}-{nonce}",
                std::process::id()
            ));
            let cooked = root.join("TAGame").join("CookedPCConsole");
            let backups = cooked.join("Backups");
            fs::create_dir_all(root.join("Binaries").join("Win64")).unwrap();
            fs::create_dir_all(&backups).unwrap();
            fs::write(
                root.join("Binaries").join("Win64").join("RocketLeague.exe"),
                b"build-one",
            )
            .unwrap();
            (Self(root), cooked, backups)
        }
    }

    impl Drop for TestInstall {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn update_discards_every_backup_and_active_entry() {
        let (install, cooked, backups) = TestInstall::new();
        assert!(!synchronize_install(&cooked, &backups).unwrap());
        fs::write(backups.join("item.upk.bak"), b"old item").unwrap();
        fs::write(backups.join("swapper_swaps.json"), b"[]").unwrap();
        fs::create_dir_all(backups.join("CarPatcher")).unwrap();
        fs::write(backups.join("CarPatcher").join("body.upk.bak"), b"old car").unwrap();
        fs::write(
            install
                .0
                .join("Binaries")
                .join("Win64")
                .join("RocketLeague.exe"),
            b"build-two",
        )
        .unwrap();

        assert!(synchronize_install(&cooked, &backups).unwrap());
        let names = fs::read_dir(&backups)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(names, vec![INSTALL_MARKER]);
    }

    #[test]
    fn same_build_preserves_backups() {
        let (_install, cooked, backups) = TestInstall::new();
        assert!(!synchronize_install(&cooked, &backups).unwrap());
        fs::write(backups.join("item.upk.bak"), b"current item").unwrap();

        assert!(!synchronize_install(&cooked, &backups).unwrap());
        assert!(backups.join("item.upk.bak").is_file());
    }

    #[test]
    fn unmarked_legacy_backups_are_not_trusted() {
        let (_install, cooked, backups) = TestInstall::new();
        fs::write(backups.join("legacy.upk.bak"), b"unknown build").unwrap();

        assert!(synchronize_install(&cooked, &backups).unwrap());
        assert!(!backups.join("legacy.upk.bak").exists());
    }
}
