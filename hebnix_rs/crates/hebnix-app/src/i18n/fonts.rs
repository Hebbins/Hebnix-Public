//! glyph fallbacks for languages egui's built-in fonts can't draw (CJK, Thai,
//! Arabic/Hebrew glyphs, Devanagari). uses fonts that ship with windows, if one
//! is missing the text just shows boxes like before.
//!
//! note: egui has no RTL shaping, so Arabic/Hebrew would draw unjoined and
//! left-to-right even with a font. not shipped for that reason.

use std::path::PathBuf;

use eframe::egui;

fn candidates(code: &str) -> &'static [&'static str] {
    let lower = code.to_ascii_lowercase();
    let primary = lower.split('-').next().unwrap_or("");
    match primary {
        "zh" if lower.contains("hant") || lower.ends_with("-tw") || lower.ends_with("-hk") => {
            &["msjh.ttc", "msyh.ttc", "simsun.ttc"]
        }
        "zh" => &["msyh.ttc", "msjh.ttc", "simsun.ttc"],
        "ja" => &["YuGothR.ttc", "meiryo.ttc", "msgothic.ttc"],
        "ko" => &["malgun.ttf", "batang.ttc"],
        "ar" | "fa" | "ur" | "he" => &["segoeui.ttf", "arial.ttf"],
        "th" => &["leelawui.ttf", "tahoma.ttf"],
        "hi" | "mr" | "ne" => &["Nirmala.ttc", "mangal.ttf"],
        _ => &[],
    }
}

fn fonts_dir() -> PathBuf {
    let windir = std::env::var_os("WINDIR")
        .or_else(|| std::env::var_os("SystemRoot"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    windir.join("Fonts")
}

/// appends a fallback font for the active language behind whatever fonts are
/// already set, so a theme font stays first.
pub fn add_fallbacks(fonts: &mut egui::FontDefinitions) {
    let code = super::current();
    let list = candidates(&code);
    if list.is_empty() {
        return;
    }

    let dir = fonts_dir();
    for file in list {
        let path = dir.join(file);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let name = format!("i18n-fallback-{code}");
        fonts
            .font_data
            .insert(name.clone(), egui::FontData::from_owned(bytes).into());
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(name.clone());
        }
        return;
    }
    tracing::warn!("i18n: no system font found for `{code}`, glyphs may be missing");
}
