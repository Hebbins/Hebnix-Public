//! layout helpers so rows don't clip when a translation runs longer than english.

use eframe::egui::{self, Color32};

/// width for a settings page's label column: the widest label on that page,
/// never narrower than `min` (the old fixed width), capped so one huge label
/// can't eat the row. keeps the inputs next to the labels lined up.
pub fn label_column_width(ui: &egui::Ui, labels: &[String], min: f32) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let widest = labels
        .iter()
        .map(|label| {
            ui.painter()
                .layout_no_wrap(label.clone(), font.clone(), Color32::WHITE)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max);
    (widest + 8.0).clamp(min, min.max(320.0))
}
