//! UPK patching and item swapping implementation.
//!
//! Keeping these modules together makes the patching boundary explicit while
//! the compatibility re-exports in main.rs preserve the existing call sites.
pub mod background_merger;
pub mod ball;
pub mod boost_patcher;
pub mod catalog;
pub mod colours;
pub mod cosmetic_thumbnail;
pub mod cosmetic_upk;
pub mod decal_patcher;
pub mod heatseeker;
pub mod patch_core;
pub mod rl_font;
pub mod swapper;
pub mod upk_keys;
pub mod upk_package;

use crate::config::PatchSource;

pub(crate) fn patch_source_selector(ui: &mut eframe::egui::Ui, source: &mut PatchSource) -> bool {
    ui.horizontal(|ui| {
        let mut changed = ui
            .selectable_value(source, PatchSource::Catalog, "Catalog")
            .changed();
        ui.label("|");
        changed |= ui
            .selectable_value(source, PatchSource::Custom, "Local")
            .changed();
        changed
    })
    .inner
}
