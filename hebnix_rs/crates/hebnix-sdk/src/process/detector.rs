//! RL process detection + steam/epic platform id (by files in the game dir).

use std::path::{Path, PathBuf};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

use crate::utils::constants::{SAVE_PATH_EPIC, SAVE_PATH_STEAM};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RlPlatform {
    Steam,
    Epic,
    Unknown,
}

impl RlPlatform {
    pub fn as_str(&self) -> &'static str {
        match self {
            RlPlatform::Steam => "steam",
            RlPlatform::Epic => "epic",
            RlPlatform::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RlProcessInfo {
    pub pid: u32,
    pub exe_path: PathBuf,
    pub root_dir: PathBuf,
    pub platform: RlPlatform,
    pub save_data_path: PathBuf,
}

/// walk up from exe_path to the game root (the folder with Binaries/ and
/// Engine/ or TAGame/)
fn find_root_dir(exe_path: &Path) -> Option<PathBuf> {
    let mut current = exe_path.parent()?.to_path_buf();
    for _ in 0..10 {
        let bins = current.join("Binaries");
        let engine = current.join("Engine");
        let tagame = current.join("TAGame");
        if bins.is_dir() && (engine.is_dir() || tagame.is_dir()) {
            return Some(current);
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => break,
        }
    }
    None
}

/// steam vs epic from the root dir: appinfo.vdf = steam, .egstore = epic, else
/// sniff the path for steamapps/steamlibrary or epic games.
pub fn detect_platform(root_dir: &Path) -> RlPlatform {
    if root_dir.join("appinfo.vdf").exists() {
        return RlPlatform::Steam;
    }
    if root_dir.join(".egstore").is_dir() {
        return RlPlatform::Epic;
    }
    let root_str = root_dir.to_string_lossy().to_lowercase();
    if root_str.contains("steamapps") || root_str.contains("steamlibrary") {
        return RlPlatform::Steam;
    }
    if root_str.contains("epic games") || root_str.contains("epicgames") {
        return RlPlatform::Epic;
    }
    RlPlatform::Unknown
}

fn documents_dir() -> PathBuf {
    dirs::document_dir().unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Documents")
    })
}

/// full path to DBE_Production/ for the platform
pub fn get_save_data_path(platform: RlPlatform) -> PathBuf {
    let docs = documents_dir();
    match platform {
        RlPlatform::Steam => docs.join(SAVE_PATH_STEAM),
        RlPlatform::Epic => docs.join(SAVE_PATH_EPIC),
        RlPlatform::Unknown => {
            for rel in [SAVE_PATH_STEAM, SAVE_PATH_EPIC] {
                let candidate = docs.join(rel);
                if candidate.is_dir() {
                    return candidate;
                }
            }
            docs.join(SAVE_PATH_STEAM) // best guess
        }
    }
}

/// name-only check for a running RocketLeague.exe. works even when the exe
/// path is unreadable (eac protects it), unlike find_rocket_league.
pub fn is_rocket_league_running() -> bool {
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    sys.processes().values().any(|p| {
        p.name()
            .to_string_lossy()
            .to_lowercase()
            .contains("rocketleague")
    })
}

/// find the running RL process + its info. None if not running or the exe
/// path isn't readable (eac), use is_rocket_league_running for plain liveness.
pub fn find_rocket_league() -> Option<RlProcessInfo> {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::Always),
    );

    for (pid, proc_) in sys.processes() {
        let name = proc_.name().to_string_lossy().to_lowercase();
        if !name.contains("rocketleague") {
            continue;
        }
        let Some(exe) = proc_.exe() else {
            continue;
        };
        if !exe.exists() {
            continue;
        }
        let Some(root) = find_root_dir(exe) else {
            continue;
        };
        let platform = detect_platform(&root);
        let save_path = get_save_data_path(platform);

        return Some(RlProcessInfo {
            pid: pid.as_u32(),
            exe_path: exe.to_path_buf(),
            root_dir: root,
            platform,
            save_data_path: save_path,
        });
    }
    None
}
