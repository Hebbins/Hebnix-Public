use std::path::{Path, PathBuf};

pub const SECTION: &str = "[TAGame.MatchStatsExporter_TA]";

/// Read PacketSendRate, Port, and WebPort. Any can be None if missing.
pub fn read_ini(path: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (None, None, None);
    };
    let mut rate = None;
    let mut port = None;
    let mut web_port = None;

    for line in text.lines() {
        let lower = line.trim().to_lowercase();
        if lower.starts_with("packetsendrate") {
            if let Some((_, v)) = line.split_once('=') {
                rate = Some(v.trim().to_string());
            }
        } else if lower.starts_with("webport") {
            if let Some((_, v)) = line.split_once('=') {
                web_port = Some(v.trim().to_string());
            }
        } else if lower.starts_with("port") {
            if let Some((_, v)) = line.split_once('=') {
                port = Some(v.trim().to_string());
            }
        }
    }
    (rate, port, web_port)
}

/// Set target_key=target_value, adding the section + key if missing.
pub fn update_ini_setting(
    path: &Path,
    target_key: &str,
    target_value: &str,
) -> std::io::Result<()> {
    let mut lines: Vec<String> = if path.exists() {
        std::fs::read_to_string(path)?
            .lines()
            .map(str::to_string)
            .collect()
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Vec::new()
    };

    let prefix = format!("{}=", target_key.to_lowercase());
    let mut found = false;
    for line in lines.iter_mut() {
        if line.trim().to_lowercase().starts_with(&prefix) {
            *line = format!("{target_key}={target_value}");
            found = true;
            break;
        }
    }

    if !found {
        let has_section = lines.iter().any(|l| l.contains(SECTION));
        if !has_section {
            lines.push(String::new());
            lines.push(SECTION.to_string());
        }
        lines.push(format!("{target_key}={target_value}"));
    }

    std::fs::write(path, lines.join("\n") + "\n")
}

pub fn resolve_ini_path(statsapi_path: &str, rl_path: &str) -> PathBuf {
    let configured = PathBuf::from(statsapi_path);
    if configured.exists() {
        return configured;
    }
    let rl_dir = PathBuf::from(rl_path);
    if !rl_path.is_empty() && rl_dir.exists() {
        return rl_dir
            .join("TAGame")
            .join("Config")
            .join("DefaultStatsAPI.ini");
    }
    configured
}
