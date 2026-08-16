use serde_json::Value;
use std::path::Path;
use std::time::Duration;

#[derive(Debug)]
pub struct UpdateInfo {
    pub version: String,
    pub setup_url: String,
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

/// Pings the API and checks if an update is available.
pub fn check_for_updates(current_version: &str) -> Result<Option<UpdateInfo>, String> {
    let resp = ureq::get("https://api.hebnix.com/info")
        .set("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| format!("Network error while checking for updates: {e}"))?;

    let json: Value = resp
        .into_json()
        .map_err(|e| format!("Invalid JSON response: {e}"))?;

    let latest_version = json
        .get("latest_version")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let setup_url = json
        .get("setup_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if latest_version.is_empty() || setup_url.is_empty() {
        return Err("API response missing 'latest_version' or 'setup_url'".to_string());
    }

    if is_newer_version(current_version, &latest_version) {
        Ok(Some(UpdateInfo {
            version: latest_version,
            setup_url,
        }))
    } else {
        Ok(None)
    }
}

/// Downloads the update, extracts it, runs the installer, and shuts down Hebnix.
pub fn download_and_install_update(setup_url: &str, base_dir: &Path) -> Result<(), String> {
    let updater_dir = base_dir.join("updater");

    if !updater_dir.exists() {
        std::fs::create_dir_all(&updater_dir)
            .map_err(|e| format!("Failed to create updater directory: {e}"))?;
    }

    let full_url = format!("https://hebnix.com{}", setup_url);
    let zip_path = updater_dir.join("setup.zip");

    // 1. Download the Zip
    let resp = ureq::get(&full_url)
        .set("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .timeout(Duration::from_secs(120)) // Give it a bit more time for file downloads
        .call()
        .map_err(|e| format!("Failed to download update file: {e}"))?;

    let mut file =
        std::fs::File::create(&zip_path).map_err(|e| format!("Failed to create setup.zip: {e}"))?;
    let mut reader = resp.into_reader();
    std::io::copy(&mut reader, &mut file).map_err(|e| format!("Failed to write setup.zip: {e}"))?;

    // 2. Extract the Zip
    let zip_file =
        std::fs::File::open(&zip_path).map_err(|e| format!("Failed to open setup.zip: {e}"))?;
    let mut archive =
        zip::ZipArchive::new(zip_file).map_err(|e| format!("Invalid zip archive: {e}"))?;
    archive
        .extract(&updater_dir)
        .map_err(|e| format!("Failed to extract update files: {e}"))?;

    // Clean up the zip file so it doesn't waste space (ignore if it fails)
    let _ = std::fs::remove_file(&zip_path);

    // 3. Run setup.exe
    let setup_exe = updater_dir.join("setup.exe");
    if !setup_exe.exists() {
        return Err("setup.exe was not found inside the extracted update".to_string());
    }

    std::process::Command::new(setup_exe)
        .spawn()
        .map_err(|e| format!("Failed to launch setup.exe: {e}"))?;

    // 4. Force Close Hebnix so the installer can overwrite the files
    std::process::exit(0);
}
