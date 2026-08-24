//! themes: built-in Dark/Light plus user themes from themes/ (toml palettes
//! over a dark or light base).

use std::path::Path;

use eframe::egui::{self, Color32};
use serde::Deserialize;

/// a user theme file, see examples/themes/ for the format
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ThemeFile {
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub colors: ThemeColors,
    /// optional font name. matches a filename in fonts/, not case sensitive.
    /// leave blank to use the default font.
    #[serde(default)]
    pub font: Option<String>,
}

/// look for a .ttf or .otf in fonts_dir with this name
fn find_font_file(fonts_dir: &Path, name: &str) -> Option<std::path::PathBuf> {
    for ext in ["ttf", "otf"] {
        let candidate = fonts_dir.join(format!("{name}.{ext}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let entries = std::fs::read_dir(fonts_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_font = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf"))
            .unwrap_or(false);
        if !is_font {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            if stem.eq_ignore_ascii_case(name) {
                return Some(path);
            }
        }
    }
    None
}

/// switches the app's font to whatever's in fonts_dir. falls back to the
/// default font if nothing's set or the file isn't there.
pub fn apply_font(ctx: &egui::Context, fonts_dir: &Path, font_name: Option<&str>) {
    let mut fonts = egui::FontDefinitions::default();

    if let Some(name) = font_name {
        if let Some(path) = find_font_file(fonts_dir, name) {
            if let Ok(bytes) = std::fs::read(&path) {
                fonts
                    .font_data
                    .insert(name.to_owned(), egui::FontData::from_owned(bytes).into());
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, name.to_owned());
            }
        }
    }

    ctx.set_fonts(fonts);
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ThemeColors {
    pub accent: Option<String>,
    pub window_bg: Option<String>,
    pub panel_bg: Option<String>,
    pub widget_bg: Option<String>,
    pub text: Option<String>,
    pub hyperlink: Option<String>,
    pub warn: Option<String>,
    pub error: Option<String>,
}

fn parse_color(s: &str) -> Option<Color32> {
    let s = s.trim().trim_start_matches('#');
    let bytes = match s.len() {
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            (r, g, b, 255)
        }
        8 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            let a = u8::from_str_radix(&s[6..8], 16).ok()?;
            (r, g, b, a)
        }
        _ => return None,
    };
    Some(Color32::from_rgba_unmultiplied(
        bytes.0, bytes.1, bytes.2, bytes.3,
    ))
}

/// scale alpha on every bg fill to make the window translucent. call after
/// apply_theme (its reset stops this compounding). needs the viewport made
/// with_transparent(true).
pub fn apply_window_opacity(ctx: &egui::Context, opacity: f32) {
    let o = opacity.clamp(0.5, 1.0);
    if o >= 0.995 {
        return;
    }
    let scale = |c: Color32| -> Color32 {
        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (c.a() as f32 * o) as u8)
    };
    let theme = ctx.theme();
    ctx.style_mut_of(theme, |style| {
        let v = &mut style.visuals;
        v.window_fill = scale(v.window_fill);
        v.panel_fill = scale(v.panel_fill);
        v.extreme_bg_color = scale(v.extreme_bg_color);
        v.faint_bg_color = scale(v.faint_bg_color);
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.open,
        ] {
            w.bg_fill = scale(w.bg_fill);
            w.weak_bg_fill = scale(w.weak_bg_fill);
        }
    });
}

/// theme names: built-ins + themes/*.toml stems
pub fn list_themes(themes_dir: &Path) -> Vec<String> {
    let mut names = vec!["Dark".to_string(), "Light".to_string()];
    if let Ok(entries) = std::fs::read_dir(themes_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "toml").unwrap_or(false) {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
            }
        }
    }
    names
}

/// apply a theme by name. Errs (for the caller to log) if the file is
/// missing/invalid, caller falls back to Dark.
pub fn apply_theme(
    ctx: &egui::Context,
    themes_dir: &Path,
    fonts_dir: &Path,
    name: &str,
) -> Result<(), String> {
    match name {
        "Dark" => {
            ctx.set_theme(egui::Theme::Dark);
            ctx.set_visuals_of(egui::Theme::Dark, egui::Visuals::dark());
            apply_font(ctx, fonts_dir, None);
            Ok(())
        }
        "Light" => {
            ctx.set_theme(egui::Theme::Light);
            ctx.set_visuals_of(egui::Theme::Light, egui::Visuals::light());
            apply_font(ctx, fonts_dir, None);
            Ok(())
        }
        _ => {
            let path = themes_dir.join(format!("{name}.toml"));
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("Theme file '{name}.toml' is missing: {e}"))?;
            let theme: ThemeFile =
                toml::from_str(&text).map_err(|e| format!("Theme '{name}' is invalid: {e}"))?;

            let (base_theme, mut visuals) = if theme.base.eq_ignore_ascii_case("light") {
                (egui::Theme::Light, egui::Visuals::light())
            } else {
                (egui::Theme::Dark, egui::Visuals::dark())
            };

            let c = &theme.colors;
            if let Some(col) = c.window_bg.as_deref().and_then(parse_color) {
                visuals.window_fill = col;
                visuals.extreme_bg_color = col.gamma_multiply(0.7);
            }
            if let Some(col) = c.panel_bg.as_deref().and_then(parse_color) {
                visuals.panel_fill = col;
                visuals.faint_bg_color = col.gamma_multiply(1.2);
            }
            if let Some(col) = c.widget_bg.as_deref().and_then(parse_color) {
                visuals.widgets.inactive.bg_fill = col;
                visuals.widgets.inactive.weak_bg_fill = col;
                visuals.widgets.noninteractive.bg_fill = col;
                visuals.widgets.noninteractive.weak_bg_fill = col;
                visuals.widgets.open.bg_fill = col;
                visuals.widgets.open.weak_bg_fill = col;
            }
            if let Some(col) = c.accent.as_deref().and_then(parse_color) {
                visuals.selection.bg_fill = col;
                visuals.hyperlink_color = col;
                visuals.widgets.hovered.bg_fill = col.gamma_multiply(0.85);
                visuals.widgets.hovered.weak_bg_fill = col.gamma_multiply(0.85);
                visuals.widgets.active.bg_fill = col;
                visuals.widgets.active.weak_bg_fill = col;
            }
            if let Some(col) = c.text.as_deref().and_then(parse_color) {
                visuals.override_text_color = Some(col);
            }
            if let Some(col) = c.hyperlink.as_deref().and_then(parse_color) {
                visuals.hyperlink_color = col;
            }
            if let Some(col) = c.warn.as_deref().and_then(parse_color) {
                visuals.warn_fg_color = col;
            }
            if let Some(col) = c.error.as_deref().and_then(parse_color) {
                visuals.error_fg_color = col;
            }

            ctx.set_theme(base_theme);
            ctx.set_visuals_of(base_theme, visuals);
            apply_font(ctx, fonts_dir, theme.font.as_deref());
            Ok(())
        }
    }
}
