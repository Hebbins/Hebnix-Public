use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PLUGIN_PREFIX: &str = "hebnix://install/plugin/";
const THEME_PREFIX: &str = "hebnix://install/theme/";
const MAX_THEME_DOWNLOAD: u64 = 50 * 1024 * 1024;

pub fn register_and_queue_from_args(base_dir: &Path) {
    if let Err(error) = register_protocol() {
        tracing::warn!("failed to register hebnix:// protocol: {error}");
    }
    for argument in std::env::args().skip(1) {
        let queued = if let Some(id) = id_from_url(&argument, PLUGIN_PREFIX) {
            queue_install(base_dir, "plugin", id)
        } else if let Some(id) = id_from_url(&argument, THEME_PREFIX) {
            queue_install(base_dir, "theme", id)
        } else {
            continue;
        };
        if let Err(error) = queued {
            tracing::error!("failed to queue install link '{argument}': {error}");
        }
    }
}

fn register_protocol() -> Result<(), String> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let executable = executable
        .to_str()
        .ok_or_else(|| "application path is not valid Unicode".to_string())?;
    let classes = RegKey::predef(HKEY_CURRENT_USER);
    let (protocol, _) = classes
        .create_subkey("Software\\Classes\\hebnix")
        .map_err(|error| error.to_string())?;
    protocol
        .set_value("", &"URL:Hebnix Installer")
        .map_err(|error| error.to_string())?;
    protocol
        .set_value("URL Protocol", &"")
        .map_err(|error| error.to_string())?;
    let (command, _) = protocol
        .create_subkey("shell\\open\\command")
        .map_err(|error| error.to_string())?;
    command
        .set_value("", &format!("\"{executable}\" \"%1\""))
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn id_from_url<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let id = value.strip_prefix(prefix)?.trim_end_matches('/');
    (!id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
    .then_some(id)
}

fn pending_dir(base_dir: &Path, kind: &str) -> PathBuf {
    base_dir.join(format!("pending-{kind}-installs"))
}

fn queue_install(base_dir: &Path, kind: &str, id: &str) -> Result<(), String> {
    let directory = pending_dir(base_dir, kind);
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_nanos();
    std::fs::write(
        directory.join(format!("{}-{stamp}.pending", std::process::id())),
        id,
    )
    .map_err(|error| error.to_string())
}

fn take_pending_ids(base_dir: &Path, kind: &str, prefix: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(pending_dir(base_dir, kind)) else {
        return Vec::new();
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("pending"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let id = std::fs::read_to_string(&path).ok();
            let _ = std::fs::remove_file(path);
            id.filter(|value| id_from_url(&format!("{prefix}{value}"), prefix).is_some())
        })
        .collect()
}

pub fn take_pending_plugin_ids(base_dir: &Path) -> Vec<String> {
    take_pending_ids(base_dir, "plugin", PLUGIN_PREFIX)
}

pub fn take_pending_theme_ids(base_dir: &Path) -> Vec<String> {
    take_pending_ids(base_dir, "theme", THEME_PREFIX)
}

fn catalog_item(endpoint: &str, id: &str) -> Result<serde_json::Value, String> {
    let catalog: serde_json::Value = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .build()
        .get(endpoint)
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|error| error.to_string())?
        .into_json()
        .map_err(|error| error.to_string())?;
    catalog
        .as_array()
        .and_then(|entries| {
            entries.iter().find(|entry| {
                entry
                    .get("plugin_id")
                    .or_else(|| entry.get("id"))
                    .and_then(serde_json::Value::as_str)
                    == Some(id)
            })
        })
        .cloned()
        .ok_or_else(|| format!("Item ID {id} was not found in the Hebnix catalog."))
}

pub fn plugin_identity(id: &str) -> Result<(String, String), String> {
    let plugin = catalog_item("https://api.hebnix.com/plugins", id)?;
    let name = plugin
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(id);
    let author = plugin
        .get("author")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Unknown");
    Ok((name.to_string(), author.to_string()))
}

pub fn install_theme(
    id: &str,
    themes_dir: &Path,
    fonts_dir: &Path,
) -> Result<(String, String), String> {
    let item = catalog_item("https://api.hebnix.com/themes", id)?;
    let item_type = item
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if !item_type.eq_ignore_ascii_case("theme") {
        return Err(format!("Item ID {id} is not a theme"));
    }
    let name = item
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(id)
        .to_string();
    let author = item
        .get("author")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Unknown")
        .to_string();
    let source_name = item
        .get("file_path")
        .and_then(serde_json::Value::as_str)
        .and_then(|path| Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("theme.toml");

    let response = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .build()
        .get(&format!("https://api.hebnix.com/download/item/{id}"))
        .timeout(Duration::from_secs(30))
        .call()
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_THEME_DOWNLOAD + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_THEME_DOWNLOAD {
        return Err("Theme download exceeds the 50 MB limit".to_string());
    }

    std::fs::create_dir_all(themes_dir).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(fonts_dir).map_err(|error| error.to_string())?;
    if bytes.starts_with(b"PK\x03\x04") {
        install_theme_zip(&bytes, themes_dir, fonts_dir)?;
    } else {
        validate_theme(&bytes)?;
        let filename = safe_filename(source_name, "toml")?;
        std::fs::write(themes_dir.join(filename), bytes).map_err(|error| error.to_string())?;
    }
    Ok((name, author))
}

fn install_theme_zip(bytes: &[u8], themes_dir: &Path, fonts_dir: &Path) -> Result<(), String> {
    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).map_err(|error| error.to_string())?;
    let mut files = Vec::new();
    let mut theme_count = 0;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        if entry.is_dir() || entry.size() > MAX_THEME_DOWNLOAD {
            continue;
        }
        let Some(path) = entry.enclosed_name() else {
            continue;
        };
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        let destination = if extension.eq_ignore_ascii_case("toml") {
            theme_count += 1;
            themes_dir
        } else if extension.eq_ignore_ascii_case("ttf") || extension.eq_ignore_ascii_case("otf") {
            fonts_dir
        } else {
            continue;
        };
        let mut contents = Vec::new();
        entry
            .by_ref()
            .take(MAX_THEME_DOWNLOAD + 1)
            .read_to_end(&mut contents)
            .map_err(|error| error.to_string())?;
        if contents.len() as u64 > MAX_THEME_DOWNLOAD {
            return Err(format!("Theme archive entry '{filename}' is too large"));
        }
        if extension.eq_ignore_ascii_case("toml") {
            validate_theme(&contents)?;
        }
        files.push((
            destination.join(safe_filename(filename, extension)?),
            contents,
        ));
    }
    if theme_count == 0 {
        return Err("Theme archive does not contain a TOML theme file".to_string());
    }
    for (path, contents) in files {
        std::fs::write(path, contents).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn validate_theme(bytes: &[u8]) -> Result<(), String> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| "Theme TOML is not valid UTF-8".to_string())?;
    toml::from_str::<crate::theme::ThemeFile>(text)
        .map(|_| ())
        .map_err(|error| format!("Invalid theme TOML: {error}"))
}

fn safe_filename<'a>(value: &'a str, expected_extension: &str) -> Result<&'a str, String> {
    let path = Path::new(value);
    let is_single_component = path.components().count() == 1 && path.file_name().is_some();
    let extension_matches = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected_extension));
    (is_single_component && extension_matches)
        .then_some(value)
        .ok_or_else(|| format!("Unsafe theme filename '{value}'"))
}
