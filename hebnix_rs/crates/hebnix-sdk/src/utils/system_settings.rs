//! TASystemSettings.ini. beats the .save for window mode, the game rewrites it
//! on apply while the save waits for exit.

use std::path::{Path, PathBuf};

use crate::save_file::models::WindowMode;

#[derive(Debug, Clone)]
pub struct SystemSettings {
    pub window_mode: WindowMode,
    pub res_width: i64,
    pub res_height: i64,
    pub path: PathBuf,
}

pub fn find_system_settings() -> PathBuf {
    dirs::document_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("My Games")
        .join("Rocket League")
        .join("TAGame")
        .join("Config")
        .join("TASystemSettings.ini")
}

/// [SystemSettings]: Fullscreen and Borderless are separate bools, both false
/// means windowed.
pub fn read(path: Option<&Path>) -> Option<SystemSettings> {
    let path = path
        .map(Path::to_path_buf)
        .unwrap_or_else(find_system_settings);
    let text = std::fs::read_to_string(&path).ok()?;

    let mut fullscreen = false;
    let mut borderless = false;
    let mut res_width = 0i64;
    let mut res_height = 0i64;
    let mut in_section = false;

    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line.eq_ignore_ascii_case("[SystemSettings]");
            continue;
        }
        if !in_section {
            continue; // ignore values that are not [SystemSettings]
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_lowercase().as_str() {
            "fullscreen" => fullscreen = parse_bool(value),
            "borderless" => borderless = parse_bool(value),
            "resx" => res_width = value.parse().unwrap_or(0),
            "resy" => res_height = value.parse().unwrap_or(0),
            _ => {}
        }
    }

    let window_mode = if fullscreen {
        WindowMode::Fullscreen
    } else if borderless {
        WindowMode::Borderless
    } else {
        WindowMode::Windowed
    };

    Some(SystemSettings {
        window_mode,
        res_width,
        res_height,
        path,
    })
}

pub fn window_mode() -> Option<WindowMode> {
    read(None).map(|s| s.window_mode)
}

fn parse_bool(v: &str) -> bool {
    matches!(v.trim().to_lowercase().as_str(), "true" | "1" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(name: &str, body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn both_false_is_windowed() {
        let p = write_tmp(
            "hebnix_ts_windowed.ini",
            "[SystemSettings]\nFullscreen=False\nBorderless=False\nResX=2560\nResY=1440\n",
        );
        let s = read(Some(&p)).unwrap();
        assert_eq!(s.window_mode, WindowMode::Windowed);
        assert_eq!((s.res_width, s.res_height), (2560, 1440));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn borderless_and_fullscreen_map_over() {
        let p = write_tmp(
            "hebnix_ts_borderless.ini",
            "[SystemSettings]\nFullscreen=False\nBorderless=True\n",
        );
        assert_eq!(read(Some(&p)).unwrap().window_mode, WindowMode::Borderless);
        let _ = std::fs::remove_file(&p);

        let p = write_tmp(
            "hebnix_ts_fullscreen.ini",
            "[SystemSettings]\nFullscreen=True\nBorderless=False\n",
        );
        assert_eq!(read(Some(&p)).unwrap().window_mode, WindowMode::Fullscreen);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn other_sections_are_ignored() {
        let p = write_tmp(
            "hebnix_ts_sections.ini",
            "[SystemSettings]\nFullscreen=False\nBorderless=True\nResX=2560\nResY=1440\n\
             \n[SystemSettingsMobile]\nFullscreen=True\nResX=1280\nResY=720\n",
        );
        let s = read(Some(&p)).unwrap();
        assert_eq!(s.window_mode, WindowMode::Borderless);
        assert_eq!((s.res_width, s.res_height), (2560, 1440));
        let _ = std::fs::remove_file(p);
    }
}
