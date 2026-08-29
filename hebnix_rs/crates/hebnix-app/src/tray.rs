//! tray icon with open/close menu.

use std::path::Path;

use tray_icon::menu::{Menu, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

pub struct Tray {
    // keep this alive, dropping it removes the tray icon
    pub _icon: TrayIcon,
    visibility_item: MenuItem,
    pub open_id: tray_icon::menu::MenuId,
    pub quit_id: tray_icon::menu::MenuId,
}

// logo baked into the exe, hebnix.ico next to the exe overrides it
pub const EMBEDDED_ICON: &[u8] = include_bytes!("../assets/hebnix.ico");

fn decode_icon(bytes: &[u8]) -> Option<Icon> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Icon::from_rgba(rgba.into_raw(), w, h).ok()
}

fn load_icon(base_dir: &Path) -> Icon {
    if let Ok(bytes) = std::fs::read(base_dir.join("hebnix.ico")) {
        if let Some(icon) = decode_icon(&bytes) {
            return icon;
        }
    }
    if let Some(icon) = decode_icon(EMBEDDED_ICON) {
        return icon;
    }
    let size = 32u32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for _ in 0..size * size {
        rgba.extend_from_slice(&[30, 30, 30, 255]);
    }
    Icon::from_rgba(rgba, size, size).expect("fallback tray icon")
}

impl Tray {
    pub fn new(base_dir: &Path, tooltip: &str, hidden: bool) -> Option<Self> {
        let menu = Menu::new();
        let open_item = MenuItem::new(if hidden { "Show" } else { "Hide" }, true, None);
        let quit_item = MenuItem::new("Close", true, None);
        menu.append(&open_item).ok()?;
        menu.append(&quit_item).ok()?;

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(tooltip)
            .with_icon(load_icon(base_dir))
            .build()
            .ok()?;

        Some(Self {
            _icon: icon,
            visibility_item: open_item.clone(),
            open_id: open_item.id().clone(),
            quit_id: quit_item.id().clone(),
        })
    }

    pub fn set_hidden(&self, hidden: bool) {
        self.visibility_item
            .set_text(if hidden { "Show" } else { "Hide" });
    }
}
