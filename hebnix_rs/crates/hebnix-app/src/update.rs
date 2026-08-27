use serde_json::Value;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub version: String,
    pub setup_url: String,
}

#[derive(Debug, Clone)]
pub struct ChangelogEntry {
    pub version: String,
    pub changelog: String,
    pub release_date: String,
}

#[derive(Debug, Clone)]
pub struct ApiInfo {
    pub update: Option<UpdateInfo>,
    pub newest_changelog: Option<ChangelogEntry>,
}
fn is_newer_version(current: &str, remote: &str) -> bool {
    let curr_parts: Vec<u32> = current.split('.').filter_map(|s| s.parse().ok()).collect();
    let rem_parts: Vec<u32> = remote.split('.').filter_map(|s| s.parse().ok()).collect();

    for i in 0..std::cmp::max(curr_parts.len(), rem_parts.len()) {
        let c = curr_parts.get(i).unwrap_or(&0);
        let r = rem_parts.get(i).unwrap_or(&0);
        if r > c {
            return true;
        }
        if r < c {
            return false;
        }
    }
    false
}

pub fn fetch_info(current_version: &str) -> Result<ApiInfo, String> {
    let agent = ureq::AgentBuilder::new().try_proxy_from_env(false).build();
    let resp = agent
        .get("https://api.hebnix.com/info")
        .set(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        )
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| format!("Network error while checking for updates: {e}"))?;

    let json: Value = resp
        .into_json()
        .map_err(|e| format!("Invalid JSON response: {e}"))?;
    let latest_version = json
        .get("latest_version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let setup_url = json
        .get("setup_url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    if latest_version.is_empty() || setup_url.is_empty() {
        return Err("API response missing latest_version or setup_url".to_string());
    }

    let newest_changelog = json
        .get("changelog_history")
        .and_then(Value::as_array)
        .and_then(|entries| entries.first())
        .map(|entry| ChangelogEntry {
            version: entry
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            changelog: entry
                .get("changelog")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            release_date: entry
                .get("release_date")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        });

    let update = is_newer_version(current_version, &latest_version).then(|| UpdateInfo {
        version: latest_version,
        setup_url,
    });
    Ok(ApiInfo {
        update,
        newest_changelog,
    })
}

pub fn render_changelog(ui: &mut eframe::egui::Ui, entry: &ChangelogEntry) {
    ui.heading(format!("Hebnix v{}", entry.version));
    if !entry.release_date.is_empty() {
        ui.label(
            eframe::egui::RichText::new(&entry.release_date)
                .small()
                .weak(),
        );
    }
    ui.add_space(8.0);

    for line in entry
        .changelog
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let trimmed = line.trim();
        let (prefix, body, colour) = if let Some(body) = trimmed.strip_prefix("[Added]") {
            (
                "ADDED:",
                body.trim().trim_start_matches(':').trim(),
                eframe::egui::Color32::from_rgb(46, 204, 113),
            )
        } else if let Some(body) = trimmed.strip_prefix("[Fixed]") {
            (
                "FIXED:",
                body.trim().trim_start_matches(':').trim(),
                eframe::egui::Color32::from_rgb(52, 152, 219),
            )
        } else if let Some(body) = trimmed.strip_prefix("[Removed]") {
            (
                "REMOVED:",
                body.trim().trim_start_matches(':').trim(),
                eframe::egui::Color32::from_rgb(231, 76, 60),
            )
        } else {
            ("", trimmed, ui.visuals().text_color())
        };
        ui.horizontal_wrapped(|ui| {
            if !prefix.is_empty() {
                ui.label(eframe::egui::RichText::new(prefix).strong().color(colour));
            }
            ui.label(body);
        });
    }
}

/// Downloads the update, extracts it, runs the installer, and shuts down Hebnix.
pub fn download_and_install_update(_setup_url: &str, base_dir: &Path) -> Result<(), String> {
    let updater_dir = base_dir.join("updater");
    std::fs::create_dir_all(&updater_dir)
        .map_err(|e| format!("Failed to create updater directory: {e}"))?;

    let update_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let staging_dir = updater_dir.join(format!("update-{update_id}"));
    std::fs::create_dir_all(&staging_dir)
        .map_err(|e| format!("Failed to create update staging directory: {e}"))?;
    let zip_path = staging_dir.join("setup.zip");

    let agent = ureq::AgentBuilder::new().try_proxy_from_env(false).build();
    let resp = agent
        .get("https://api.hebnix.com/download-setup")
        .set(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        )
        .timeout(Duration::from_secs(120))
        .call()
        .map_err(|e| format!("Failed to download update file: {e}"))?;

    {
        let mut file = std::fs::File::create(&zip_path)
            .map_err(|e| format!("Failed to create setup.zip: {e}"))?;
        let mut reader = resp.into_reader();
        std::io::copy(&mut reader, &mut file)
            .map_err(|e| format!("Failed to write setup.zip: {e}"))?;
        file.sync_all()
            .map_err(|e| format!("Failed to finish setup.zip: {e}"))?;
    }

    {
        let zip_file =
            std::fs::File::open(&zip_path).map_err(|e| format!("Failed to open setup.zip: {e}"))?;
        let mut archive =
            zip::ZipArchive::new(zip_file).map_err(|e| format!("Invalid zip archive: {e}"))?;
        archive
            .extract(&staging_dir)
            .map_err(|e| format!("Failed to extract update files: {e}"))?;
    }

    let _ = std::fs::remove_file(&zip_path);
    let setup_exe = staging_dir.join("setup.exe");
    if !setup_exe.exists() {
        return Err("setup.exe was not found inside the extracted update".to_string());
    }

    let _ = std::fs::File::create(base_dir.join(".first"));

    std::process::Command::new(setup_exe)
        .spawn()
        .map_err(|e| format!("Failed to launch setup.exe: {e}"))?;

    std::process::exit(0);
}
