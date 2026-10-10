//! Notifications dropdown with plugin filters above the message history.

use std::collections::HashSet;
use std::sync::Arc;

use eframe::egui;

use crate::i18n::{t, t_args};
use crate::toast::{HISTORY_MAX, PluginTag, Stamp, ToastCenter, preview};

#[derive(Default)]
pub struct ToastView {
    // plugin slug, empty is all
    selected: String,
    open: HashSet<u64>,
}

impl ToastView {
    pub fn render(&mut self, ui: &mut egui::Ui, center: &mut ToastCenter) {
        // ids only ever go up, so anything under the oldest is gone from history
        let oldest = center.history().next().map_or(0, |toast| toast.id);
        self.open.retain(|id| *id >= oldest);

        // plugins newest first, with how many each sent
        let mut plugins: Vec<(Arc<PluginTag>, u32)> = Vec::new();
        for toast in center.history().rev() {
            match plugins
                .iter_mut()
                .find(|(tag, _)| tag.slug == toast.plugin.slug)
            {
                Some((_, count)) => *count += 1,
                None => plugins.push((Arc::clone(&toast.plugin), 1)),
            }
        }
        if !self.selected.is_empty() && !plugins.iter().any(|(tag, _)| tag.slug == self.selected) {
            self.selected.clear();
        }

        egui::ScrollArea::vertical()
            .id_salt("toast_plugin_names")
            .max_height(110.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .selectable_label(self.selected.is_empty(), t("toast-filter-all"))
                        .clicked()
                    {
                        self.selected.clear();
                    }
                    for (tag, count) in &plugins {
                        let label = format!("{} ({count})", tag.name);
                        if ui
                            .selectable_label(tag.slug == self.selected, label)
                            .clicked()
                        {
                            self.selected = tag.slug.clone();
                        }
                    }
                });
            });

        let title = plugins
            .iter()
            .find(|(tag, _)| tag.slug == self.selected)
            .map_or_else(|| t("toast-filter-all"), |(tag, _)| tag.name.clone());
        let today = Stamp::now();

        ui.separator();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(title).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!plugins.is_empty(), egui::Button::new(t("toast-clear")))
                    .clicked()
                {
                    center.clear();
                    self.open.clear();
                    self.selected.clear();
                }
            });
        });
        ui.add_space(8.0);

        egui::ScrollArea::vertical()
            .id_salt("toast_view")
            .max_height((ui.ctx().content_rect().height() - 230.0).clamp(100.0, 460.0))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if center.history().next().is_none() {
                    ui.label(t("toast-empty"));
                    return;
                }
                let all = self.selected.is_empty();
                for toast in center.history().rev() {
                    if !all && toast.plugin.slug != self.selected {
                        continue;
                    }
                    let (line, cut) = preview(&toast.text);
                    let head = match (all, cut) {
                        (true, true) => format!("{}: {line}...", toast.plugin.name),
                        (true, false) => format!("{}: {line}", toast.plugin.name),
                        (false, true) => format!("{line}..."),
                        (false, false) => line.to_string(),
                    };
                    let open = self.open.contains(&toast.id);
                    // the header id wraps around so egui memory stays small, open state is ours
                    let shown = egui::CollapsingHeader::new(head)
                        .id_salt(toast.id % (HISTORY_MAX as u64 * 2))
                        .open(Some(open))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(t_args(
                                    "toast-sent-at",
                                    &[("time", toast.at.label(today).into())],
                                ))
                                .weak()
                                .size(11.0),
                            );
                            ui.add(egui::Label::new(&*toast.text).selectable(true));
                            ui.add_space(4.0);
                        });
                    if shown.header_response.clicked() && !self.open.remove(&toast.id) {
                        self.open.insert(toast.id);
                    }
                }
            });
    }
}

/// Bell button and a portrait popup anchored below its right edge.
pub fn render_bell(ui: &mut egui::Ui, view: &mut ToastView, center: &mut ToastCenter) {
    let unread = center.unread();
    let label = if unread == 0 {
        None
    } else {
        Some(egui::WidgetText::from(unread.to_string()))
    };
    let button = ui
        .add(
            egui::Button::opt_image_and_text(
                Some(
                    egui::Image::new(egui::include_image!("../../assets/notification-bell.svg"))
                        .fit_to_exact_size(egui::vec2(16.0, 16.0))
                        .tint(ui.visuals().text_color()),
                ),
                label,
            )
            .min_size(egui::vec2(28.0, 24.0)),
        )
        .on_hover_text(t("tab-notifications"));
    let width = (ui.ctx().content_rect().width() - 32.0).clamp(180.0, 360.0);
    egui::Popup::from_toggle_button_response(&button)
        .align(egui::RectAlign::BOTTOM_END)
        .align_alternatives(&[])
        .gap(4.0)
        .width(width)
        .layout(egui::Layout::top_down(egui::Align::Min))
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_width(width);
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            center.mark_read();
            ui.horizontal(|ui| {
                ui.heading(t("tab-notifications"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("×").clicked() {
                        ui.close();
                    }
                });
            });
            ui.separator();
            view.render(ui, center);
        });
}
