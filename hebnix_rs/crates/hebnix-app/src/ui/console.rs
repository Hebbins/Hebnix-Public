//! Developer console: scrollback, command input with history and
//! autocomplete suggestions.

use eframe::egui::{self, Key, Modifiers};

const MAX_LINES: usize = 2000;

pub struct ConsoleState {
    pub lines: Vec<String>,
    pub input: String,
    pub history: Vec<String>,
    pub history_index: usize,
    pub suggestions: Vec<String>,
    pub suggestion_index: Option<usize>,
    last_input: String,
    stick_to_bottom: bool,
}

const BASE_COMMANDS: [&str; 10] = [
    "help",
    "info",
    "server",
    "plugin load ",
    "plugin reload ",
    "plugin unload ",
    "plugins list",
    "clear",
    "quit",
    "restart",
];

impl Default for ConsoleState {
    fn default() -> Self {
        Self {
            lines: vec![
                "[Console] Hebnix Developer Console Initialized.".to_string(),
                "[Console] Type 'info' for diagnostic details.".to_string(),
                "[Console] Type 'help' for a list of commands.".to_string(),
            ],
            input: String::new(),
            history: Vec::new(),
            history_index: 0,
            suggestions: Vec::new(),
            suggestion_index: None,
            last_input: String::new(),
            stick_to_bottom: true,
        }
    }
}

impl ConsoleState {
    pub fn write(&mut self, message: impl Into<String>) {
        for line in message.into().lines() {
            // Mirror every console line into hebnix.log for post-mortems.
            tracing::info!(target: "console", "{line}");
            self.lines.push(line.to_string());
        }
        if self.lines.len() > MAX_LINES {
            let excess = self.lines.len() - MAX_LINES;
            self.lines.drain(0..excess);
        }
        self.stick_to_bottom = true;
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }

    fn update_suggestions(&mut self, plugin_names: &[String]) {
        self.suggestions.clear();
        self.suggestion_index = None;
        if self.input.is_empty() {
            return;
        }
        for cmd in BASE_COMMANDS {
            if cmd.starts_with(&self.input) {
                self.suggestions.push(cmd.to_string());
            }
        }
        if self.input.starts_with("plugin ") {
            let parts: Vec<&str> = self.input.split(' ').collect();
            if parts.len() >= 2 {
                let action = parts[1];
                for name in plugin_names {
                    let full = format!("plugin {action} {name}");
                    if full.starts_with(&self.input) && !self.suggestions.contains(&full) {
                        self.suggestions.push(full);
                    }
                }
            }
        }
        self.suggestions.truncate(5);
    }

    /// render the console. returns a command line when the user submits one.
    pub fn render(&mut self, ui: &mut egui::Ui, plugin_names: &[String]) -> Option<String> {
        let mut submitted: Option<String> = None;

        // Reserve space for the input row at the bottom.
        let input_height = 32.0;
        let log_height = (ui.available_height() - input_height - 8.0).max(60.0);

        egui::Frame::group(ui.style())
            .fill(ui.visuals().extreme_bg_color)
            .show(ui, |ui| {
                ui.set_min_height(log_height);
                ui.set_max_height(log_height);
                egui::ScrollArea::vertical()
                    .id_salt("console_scroll")
                    .auto_shrink([false, false])
                    .stick_to_bottom(self.stick_to_bottom)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        for line in &self.lines {
                            ui.add(
                                egui::Label::new(egui::RichText::new(line).monospace().size(12.0))
                                    .wrap(),
                            );
                        }
                    });
            });
        self.stick_to_bottom = false;

        ui.add_space(4.0);

        // Keyboard handling before the TextEdit consumes events.
        let input_id = egui::Id::new("console_input");
        let has_focus = ui.ctx().memory(|m| m.has_focus(input_id));

        let mut apply_suggestion: Option<String> = None;
        if has_focus {
            let up = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp));
            let down = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown));

            if !self.suggestions.is_empty() {
                if up {
                    self.suggestion_index = Some(match self.suggestion_index {
                        Some(0) | None => self.suggestions.len() - 1,
                        Some(i) => i - 1,
                    });
                }
                if down {
                    self.suggestion_index = Some(match self.suggestion_index {
                        None => 0,
                        Some(i) if i + 1 >= self.suggestions.len() => 0,
                        Some(i) => i + 1,
                    });
                }
            } else if !self.history.is_empty() {
                if up && self.history_index > 0 {
                    self.history_index -= 1;
                    self.input = self.history[self.history_index].clone();
                    self.last_input = self.input.clone();
                }
                if down {
                    if self.history_index + 1 < self.history.len() {
                        self.history_index += 1;
                        self.input = self.history[self.history_index].clone();
                    } else {
                        self.history_index = self.history.len();
                        self.input.clear();
                    }
                    self.last_input = self.input.clone();
                }
            }
        }

        let response = ui.add(
            egui::TextEdit::singleline(&mut self.input)
                .id(input_id)
                .hint_text("Enter system command...")
                .font(egui::TextStyle::Monospace)
                .desired_width(f32::INFINITY),
        );

        // Suggestions popup above the input.
        if has_focus && !self.suggestions.is_empty() {
            let popup_height = self.suggestions.len() as f32 * 20.0 + 8.0;
            let popup_pos = response.rect.left_top() - egui::vec2(0.0, popup_height + 4.0);
            egui::Area::new(egui::Id::new("console_suggestions"))
                .fixed_pos(popup_pos)
                .order(egui::Order::Foreground)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_min_width(response.rect.width());
                        for (i, s) in self.suggestions.clone().iter().enumerate() {
                            let selected = self.suggestion_index == Some(i);
                            let label = ui.selectable_label(
                                selected,
                                egui::RichText::new(s).monospace().size(12.0),
                            );
                            if label.clicked() {
                                apply_suggestion = Some(s.clone());
                            }
                        }
                    });
                });
        }

        if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            if let Some(i) = self.suggestion_index {
                // Enter with an active suggestion applies it.
                apply_suggestion = self.suggestions.get(i).cloned();
            } else {
                let raw = self.input.trim().to_string();
                if !raw.is_empty() {
                    self.write(format!("> {raw}"));
                    self.history.push(raw.clone());
                    self.history_index = self.history.len();
                    self.input.clear();
                    self.last_input.clear();
                    self.suggestions.clear();
                    self.suggestion_index = None;
                    submitted = Some(raw);
                }
            }
            response.request_focus();
        }

        if let Some(s) = apply_suggestion {
            self.input = s;
            self.last_input = self.input.clone();
            self.suggestions.clear();
            self.suggestion_index = None;
            response.request_focus();
            // Move the cursor to the end of the applied suggestion.
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), input_id) {
                let ccursor = egui::text::CCursor::new(self.input.chars().count());
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::one(ccursor)));
                state.store(ui.ctx(), input_id);
            }
        }

        if self.input != self.last_input {
            self.last_input = self.input.clone();
            self.update_suggestions(plugin_names);
        }

        submitted
    }
}
