//! UPK patching and item swapping implementation.
//!
//! Keeping these modules together makes the patching boundary explicit while
//! the compatibility re-exports in main.rs preserve the existing call sites.
pub mod background_merger;
pub mod backup_guard;
pub mod ball;
pub mod ball_visual;
pub mod boost_patcher;
pub mod car_patcher;
pub mod catalog;
pub mod colours;
pub mod cosmetic_thumbnail;
pub mod cosmetic_upk;
pub mod decal_patcher;
pub mod heatseeker;
pub mod painted_swap;
pub mod patch_core;
pub mod rl_font;
pub mod speed_patch;
pub mod swapper;
pub mod upk_keys;
pub mod upk_package;
pub mod wheel_alignment;

use crate::config::PatchSource;
use crate::i18n::t;

pub(crate) fn patch_source_selector(ui: &mut eframe::egui::Ui, source: &mut PatchSource) -> bool {
    ui.horizontal(|ui| {
        let mut changed = ui
            .selectable_value(
                source,
                PatchSource::Catalog,
                t("patch-source-selector-catalog"),
            )
            .changed();
        ui.label("|");
        changed |= ui
            .selectable_value(
                source,
                PatchSource::Custom,
                t("patch-source-selector-local"),
            )
            .changed();
        changed
    })
    .inner
}
