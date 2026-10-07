//! RLAPI request workbench. Credentials stay in the session transport.
use crate::i18n::t;
use eframe::egui;
use serde_json::Value;

pub const ENDPOINTS: &[(&str, &str)] = &[
    ("Population/GetPopulation v1", "{}"),
    ("Playlists/GetActivePlaylists v1", "{}"),
    ("Players/GetProfile v1", "{\n  \"PlayerIDs\": [\"\"]\n}"),
    ("Skills/GetPlayerSkill v1", "{\n  \"PlayerID\": \"\"\n}"),
    ("Skills/GetPlayersSkills v1", "{\n  \"PlayerIDs\": []\n}"),
    ("Custom endpoint", "{}"),
];

/// the last entry is the "custom endpoint" slot, its name is translated
fn endpoint_label(index: usize) -> String {
    if index == ENDPOINTS.len() - 1 {
        t("rlapi-custom-endpoint")
    } else {
        ENDPOINTS[index].0.to_string()
    }
}

pub enum Action {
    Enable,
    Disable,
    Send { service: String, body: Value },
}

pub struct RlApiPanel {
    selected: usize,
    custom_endpoint: String,
    payload: String,
    response: String,
    pub busy: bool,
    pub starting: bool,
}

impl Default for RlApiPanel {
    fn default() -> Self {
        Self {
            selected: 0,
            custom_endpoint: String::new(),
            payload: "{}".into(),
            response: String::new(),
            busy: false,
            starting: false,
        }
    }
}

impl RlApiPanel {
    pub fn complete(&mut self, result: Result<Value, String>) {
        self.busy = false;
        self.response = match result {
            Ok(value) => {
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| "Invalid response".into())
            }
            Err(error) => format!("Error: {error}"),
        };
    }

    fn begin_request(&mut self) -> Option<Action> {
        self.response.clear();
        let service = if self.selected == ENDPOINTS.len() - 1 {
            self.custom_endpoint.trim()
        } else {
            ENDPOINTS[self.selected].0
        };
        match validate_request(service, &self.payload) {
            Ok(body) => {
                self.busy = true;
                Some(Action::Send {
                    service: service.into(),
                    body,
                })
            }
            Err(error) => {
                self.complete(Err(error));
                None
            }
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        enabled: bool,
        connected: bool,
        status: &str,
    ) -> Option<Action> {
        let mut action = None;
        ui.heading(t("app-rlapi"));
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.starting,
                    egui::Button::new(if enabled {
                        t("hebnix-install-disable")
                    } else {
                        t("hebnix-install-enable")
                    }),
                )
                .clicked()
            {
                action = Some(if enabled {
                    Action::Disable
                } else {
                    Action::Enable
                });
            }
            ui.label(status);
        });
        ui.label(t("rlapi-enable-before-launching-rocket-league-to"));
        ui.label(t("rlapi-disabling-stops-new-requests-the-relay"));
        ui.add_space(10.0);
        let previous = self.selected;
        ui.horizontal(|ui| {
            ui.label(t("rlapi-endpoint"));
            egui::ComboBox::from_id_salt("rlapi_endpoint")
                .selected_text(endpoint_label(self.selected))
                .width(360.0)
                .show_ui(ui, |ui| {
                    for (index, (endpoint, _)) in ENDPOINTS.iter().enumerate() {
                        let _ = endpoint;
                        ui.selectable_value(&mut self.selected, index, endpoint_label(index));
                    }
                });
        });
        if self.selected != previous {
            self.payload = ENDPOINTS[self.selected].1.into();
        }
        if self.selected == ENDPOINTS.len() - 1 {
            ui.add(
                egui::TextEdit::singleline(&mut self.custom_endpoint)
                    .hint_text(t("rlapi-namespace-endpoint-v1"))
                    .desired_width(f32::INFINITY),
            );
        }
        ui.add_space(6.0);
        ui.label(t("rlapi-payload-json"));
        egui::ScrollArea::vertical()
            .id_salt("rlapi_payload_scroll")
            .max_height(170.0)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.payload)
                        .code_editor()
                        .desired_rows(7)
                        .desired_width(f32::INFINITY),
                );
            });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    enabled && connected && !self.busy,
                    egui::Button::new(t("rlapi-send-request")),
                )
                .clicked()
            {
                action = self.begin_request();
            }
            if self.busy {
                ui.spinner();
                ui.label(t("rlapi-waiting-for-response"));
            }
            if ui
                .add_enabled(!self.busy, egui::Button::new(t("rlapi-format-json")))
                .clicked()
            {
                match serde_json::from_str::<Value>(&self.payload) {
                    Ok(value) => self.payload = serde_json::to_string_pretty(&value).unwrap(),
                    Err(error) => self.complete(Err(format!("Invalid JSON: {error}"))),
                }
            }
            if ui
                .add_enabled(
                    !self.response.is_empty(),
                    egui::Button::new(t("rlapi-copy-response")),
                )
                .clicked()
            {
                ui.ctx().copy_text(self.response.clone());
            }
        });
        ui.separator();
        ui.label(t("rlapi-response"));
        egui::ScrollArea::vertical()
            .id_salt("rlapi_response_scroll")
            .show(ui, |ui| {
                // &str is a read-only TextBuffer; selection and copying still work.
                let mut text = self.response.as_str();
                ui.add(
                    egui::TextEdit::multiline(&mut text)
                        .code_editor()
                        .desired_rows(12)
                        .desired_width(f32::INFINITY),
                );
            });
        action
    }
}

pub fn validate_request(service: &str, payload: &str) -> Result<Value, String> {
    if payload.len() > 1024 * 1024 {
        return Err("Payload must be smaller than 1 MiB.".into());
    }
    let value: Value =
        serde_json::from_str(payload).map_err(|error| format!("Invalid JSON: {error}"))?;
    hebnix_sdk::rlapi::session::validate_request(service, &value)?;
    Ok(value)
}
