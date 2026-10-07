//! notifications tab. plugin list on the left, its messages on the right.

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

        egui::Panel::left("toast_plugin_list")
            .resizable(false)
            .default_size(200.0)
            .size_range(200.0..=320.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("toast_plugin_names")
                    .show(ui, |ui| {
                        if ui
                            .selectable_label(self.selected.is_empty(), t("toast-filter-all"))
                            .clicked()
                        {
                            self.selected.clear();
                        }
                        for (tag, count) in &plugins {
                            let label = format!("{} ({count})", tag.name);
                            if ui.selectable_label(tag.slug == self.selected, label).clicked() {
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

        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("toast_view")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.heading(title);
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui
                                    .add_enabled(!plugins.is_empty(), egui::Button::new(t("toast-clear")))
                                    .clicked()
                                {
                                    center.clear();
                                    self.open.clear();
                                }
                            });
                        });
                        ui.add_space(8.0);

                        if plugins.is_empty() {
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
            });
    }
}

/// tab label, with the unread count once there is one
pub fn tab_label(center: &ToastCenter) -> String {
    match center.unread() {
        0 => t("tab-notifications"),
        n => t_args("tab-notifications-unread", &[("count", n.into())]),
    }
}
