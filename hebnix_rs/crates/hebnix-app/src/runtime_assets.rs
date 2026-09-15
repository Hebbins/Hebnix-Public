//! Extract bundled support programs into the shared Hebnix AppData folder.

use std::path::Path;

const STEAM_API: &[u8] = include_bytes!("../../../vendor/steam_api64.dll");
const RLAPI_BRIDGE: &[u8] = include_bytes!("../../../../rlapi_bridge/dist/rlapi-bridge.exe");
const CURL_IMPERSONATE: &[u8] =
    include_bytes!("../../../vendor/curl-impersonate/curl-impersonate.exe");
const CURL_CA_CERTS: &[u8] = include_bytes!("../../../vendor/curl-impersonate/cacert.pem");

pub fn ensure_present(base_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(base_dir)?;
    if let Err(error) = migrate_legacy_exe_data(base_dir) {
        tracing::warn!("could not finish legacy data migration: {error}");
    }
    write_if_changed(&base_dir.join("steam_api64.dll"), STEAM_API)?;
    write_if_changed(&base_dir.join("rlapi-bridge.exe"), RLAPI_BRIDGE)?;

    let curl_dir = base_dir.join("curl-impersonate");
    std::fs::create_dir_all(&curl_dir)?;
    write_if_changed(&curl_dir.join("curl-impersonate.exe"), CURL_IMPERSONATE)?;
    write_if_changed(&curl_dir.join("cacert.pem"), CURL_CA_CERTS)?;
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
