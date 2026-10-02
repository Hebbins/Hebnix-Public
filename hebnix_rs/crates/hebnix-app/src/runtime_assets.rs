//! Extract bundled support programs into the shared Hebnix AppData folder.

use std::path::Path;

const STEAM_API: &[u8] = include_bytes!("../../../vendor/steam_api64.dll");
const RLAPI_BRIDGE: &[u8] = include_bytes!("../../../../rlapi_bridge/dist/rlapi-bridge.exe");

/// Multiplayer support files shipped inside Hebnix. The paths are relative to
/// `%AppData%\\Hebnix\\multiplayer-lan` and deliberately use a fixed manifest,
/// so an embedded asset can never escape that directory on extraction.
const MULTIPLAYER_ASSETS: &[(&str, &[u8])] = &[
    (
        "tailscale.exe",
        include_bytes!("../assets/multiplayer/tailscale.exe"),
    ),
    (
        "tailscaled.exe",
        include_bytes!("../assets/multiplayer/tailscaled.exe"),
    ),
    (
        "WinDivert.dll",
        include_bytes!("../assets/multiplayer/WinDivert.dll"),
    ),
    (
        "WinDivert64.sys",
        include_bytes!("../assets/multiplayer/WinDivert64.sys"),
    ),
    (
        "wintun.dll",
        include_bytes!("../assets/multiplayer/wintun.dll"),
    ),
    (
        "depotdownloader/DepotDownloaderMod.deps.json",
        include_bytes!("../assets/multiplayer/depotdownloader/DepotDownloaderMod.deps.json"),
    ),
    (
        "depotdownloader/DepotDownloaderMod.dll",
        include_bytes!("../assets/multiplayer/depotdownloader/DepotDownloaderMod.dll"),
    ),
    (
        "depotdownloader/DepotDownloaderMod.runtimeconfig.json",
        include_bytes!(
            "../assets/multiplayer/depotdownloader/DepotDownloaderMod.runtimeconfig.json"
        ),
    ),
    (
        "depotdownloader/LICENSE",
        include_bytes!("../assets/multiplayer/depotdownloader/LICENSE"),
    ),
    (
        "depotdownloader/protobuf-net.Core.dll",
        include_bytes!("../assets/multiplayer/depotdownloader/protobuf-net.Core.dll"),
    ),
    (
        "depotdownloader/protobuf-net.dll",
        include_bytes!("../assets/multiplayer/depotdownloader/protobuf-net.dll"),
    ),
    (
        "depotdownloader/QRCoder.dll",
        include_bytes!("../assets/multiplayer/depotdownloader/QRCoder.dll"),
    ),
    (
        "depotdownloader/SteamKit2.dll",
        include_bytes!("../assets/multiplayer/depotdownloader/SteamKit2.dll"),
    ),
    (
        "depotdownloader/System.IO.Hashing.dll",
        include_bytes!("../assets/multiplayer/depotdownloader/System.IO.Hashing.dll"),
    ),
    (
        "depotdownloader/ZstdSharp.dll",
        include_bytes!("../assets/multiplayer/depotdownloader/ZstdSharp.dll"),
    ),
];

pub fn ensure_present(base_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(base_dir)?;
    if let Err(error) = migrate_legacy_exe_data(base_dir) {
        tracing::warn!("could not finish legacy data migration: {error}");
    }
    write_if_changed(&base_dir.join("steam_api64.dll"), STEAM_API)?;
    write_if_changed(&base_dir.join("rlapi-bridge.exe"), RLAPI_BRIDGE)?;

    Ok(())
}

/// Extracts the multiplayer bundle when the player connects to Workshop
/// multiplayer. Existing files are updated only when their bundled contents
/// changed, avoiding unnecessary writes on later connections.
pub fn ensure_multiplayer_present(base_dir: &Path) -> std::io::Result<()> {
    let multiplayer_dir = base_dir.join("multiplayer-lan");
    for &(relative_path, bytes) in MULTIPLAYER_ASSETS {
        write_if_changed(&multiplayer_dir.join(relative_path), bytes)?;
    }
    Ok(())
}

fn migrate_legacy_exe_data(base_dir: &Path) -> std::io::Result<()> {
    let marker = base_dir.join(".appdata-migration-v1");
    if marker.exists() {
        return Ok(());
    }
    let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    else {
        return Ok(());
    };
    if exe_dir == base_dir {
        return Ok(());
    }

    for name in [
        "themes",
        "fonts",
        "plugins",
        "balls",
        "boosts",
        "decals",
        "presets",
        "assets",
        "spoofer",
        "curl-impersonate",
        "config.toml",
        "config.ini",
        "hebnix.ico",
        "hebnix.log",
        "hebnix.log.old",
        "hebnix-lite.log",
        "crash.txt",
        "theme_errors.txt",
        "spoofer_settings.json",
        "owned_products.json",
        "friends.json",
    ] {
        copy_missing(&exe_dir.join(name), &base_dir.join(name))?;
    }
    std::fs::write(marker, b"migrated from the legacy executable directory\n")
}

fn copy_missing(source: &Path, destination: &Path) -> std::io::Result<()> {
    if !source.exists() {
        return Ok(());
    }
    if source.is_dir() {
        std::fs::create_dir_all(destination)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_missing(&entry.path(), &destination.join(entry.file_name()))?;
        }
    } else if source.is_file() && !destination.exists() {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(source, destination)?;
    }
    Ok(())
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if std::fs::read(path).is_ok_and(|existing| existing == bytes) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}
