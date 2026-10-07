use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use eframe::egui;
use serde_json::Value;

use crate::i18n::t;

const LINK_URL: &str = "https://req.hebnix.com/discord/link";
const RESPONSE_MAX_BYTES: u64 = 64 * 1024;
const LINKED_ACCOUNTS_FILE: &str = "discord_linked_accounts.json";

struct LinkResult {
    result: Result<Vec<String>, String>,
    message: Option<String>,
}

pub struct DiscordLinkState {
    code: String,
    status: String,
    linked_accounts: Vec<String>,
    linking: bool,
    accounts_path: PathBuf,
    tx: Sender<LinkResult>,
    rx: Receiver<LinkResult>,
}

impl DiscordLinkState {
    pub fn new(base_dir: &Path) -> Self {
        let (tx, rx) = mpsc::channel();
        let accounts_path = base_dir.join(LINKED_ACCOUNTS_FILE);
        Self {
            code: String::new(),
            status: t("discord-link-status-ready"),
            linked_accounts: load_linked_accounts(&accounts_path),
            linking: false,
            accounts_path,
            tx,
            rx,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.poll();

        ui.separator();
        ui.strong(t("discord-link-heading"));
        ui.label(
            egui::RichText::new(t("discord-link-instruction"))
                .small()
                .color(egui::Color32::GRAY),
        );
        ui.horizontal(|ui| {
            ui.label(t("discord-link-code"));
            let response = ui.add_enabled(
                !self.linking,
                egui::TextEdit::singleline(&mut self.code)
                    .desired_width(180.0)
                    .char_limit(64),
            );
            let submit = !self.linking
                && (ui
                    .add_enabled(!self.linking, egui::Button::new(t("discord-link-button")))
                    .clicked()
                    || (response.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter))));
            if submit {
                self.start_link(ctx.clone());
            }
        });
        ui.label(
            egui::RichText::new(&self.status)
                .small()
                .color(egui::Color32::GRAY),
        );

        if !self.linked_accounts.is_empty() {
            ui.add_space(8.0);
            ui.strong(t("discord-linked-accounts"));
            for display_name in &self.linked_accounts {
                ui.label(display_name);
            }
        }
    }

    fn poll(&mut self) {
        while let Ok(link_result) = self.rx.try_recv() {
            self.linking = false;
            match link_result.result {
                Ok(accounts) => {
                    for account in accounts {
                        if !self
                            .linked_accounts
                            .iter()
                            .any(|existing| existing == &account)
                        {
                            self.linked_accounts.push(account);
                        }
                    }
                    self.code.clear();
                    self.status = link_result
                        .message
                        .unwrap_or_else(|| t("discord-link-status-linked"));
                    if !self.linked_accounts.is_empty()
                        && let Err(error) =
                            save_linked_accounts(&self.accounts_path, &self.linked_accounts)
                    {
                        self.status = format!("{} {error}", t("discord-link-status-save-failed"));
                    }
                }
                Err(error) => self.status = error,
            }
        }
    }

    fn start_link(&mut self, ctx: egui::Context) {
        let code = self.code.trim().to_string();
        if code.is_empty() {
            self.status = t("discord-link-status-code-required");
            return;
        }
        if !hebnix_sdk::process::is_rocket_league_running() {
            self.status = t("discord-link-status-game-required");
            return;
        }

        self.linking = true;
        self.status = t("discord-link-status-linking");
        let tx = self.tx.clone();
        if std::thread::Builder::new()
            .name("discord-account-link".into())
            .spawn(move || {
                let result = link_account(&code);
                let _ = tx.send(result);
                ctx.request_repaint();
            })
            .is_err()
        {
            self.linking = false;
            self.status = t("discord-link-status-start-failed");
        }
    }
}

fn load_linked_accounts(path: &Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let Ok(accounts) = serde_json::from_slice::<Vec<String>>(&bytes) else {
        return Vec::new();
    };

    let mut loaded = Vec::new();
    for account in accounts {
        let account = account.trim();
        if !account.is_empty() && !loaded.iter().any(|existing| existing == account) {
            loaded.push(account.to_string());
        }
    }
    loaded
}

fn save_linked_accounts(path: &Path, accounts: &[String]) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(accounts)
        .map_err(|error| format!("Could not encode linked accounts: {error}"))?;
    std::fs::write(path, bytes).map_err(|error| format!("Could not save linked accounts: {error}"))
}

fn link_account(code: &str) -> LinkResult {
    if !hebnix_sdk::process::is_rocket_league_running() {
        return LinkResult {
            result: Err(t("discord-link-status-game-required")),
            message: None,
        };
    }
    let log = hebnix_sdk::log::parse_launch_log(None, false, "INT");
    let Some(platform_id) = log
        .session
        .primary_id
        .filter(|id| !id.trim().is_empty() && hebnix_sdk::utils::parse_primary_id(id).is_some())
    else {
        return LinkResult {
            result: Err(t("discord-link-status-player-required")),
            message: None,
        };
    };

    let app_token = match hebnix_sdk::req_auth::app_token() {
        Ok(token) => token,
        Err(error) => {
            return LinkResult {
                result: Err(error),
                message: None,
            };
        }
    };
    let payload = serde_json::json!({
        "token": code,
        "platform_id": platform_id,
    });
    let agent = ureq::AgentBuilder::new().try_proxy_from_env(false).build();
    let response = match agent
        .post(LINK_URL)
        .set("X-App-Token", &app_token)
        .set("Accept", "application/json")
        .set("User-Agent", concat!("Hebnix/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(15))
        .send_json(payload)
    {
        Ok(response) => response,
        Err(ureq::Error::Status(status, response)) => {
            let detail = read_response(response)
                .ok()
                .and_then(|body| response_message(&body))
                .unwrap_or_else(|| format!("Discord link request returned HTTP {status}."));
            return LinkResult {
                result: Err(detail),
                message: None,
            };
        }
        Err(ureq::Error::Transport(error)) => {
            return LinkResult {
                result: Err(format!("Discord link request failed: {error}")),
                message: None,
            };
        }
    };

    let body = match read_response(response) {
        Ok(body) => body,
        Err(error) => {
            return LinkResult {
                result: Err(error),
                message: None,
            };
        }
    };
    if let Some(error) = body.get("error").and_then(Value::as_str) {
        return LinkResult {
            result: Err(error.chars().take(512).collect()),
            message: None,
        };
    }
    let message = response_message(&body);
    let accounts = linked_display_names(&body);
    LinkResult {
        result: Ok(accounts),
        message,
    }
}

fn read_response(response: ureq::Response) -> Result<Value, String> {
    let mut body = String::new();
    response
        .into_reader()
        .take(RESPONSE_MAX_BYTES)
        .read_to_string(&mut body)
        .map_err(|error| format!("Could not read Discord link response: {error}"))?;
    if body.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&body).map_err(|error| format!("Invalid Discord link response: {error}"))
}

fn response_message(value: &Value) -> Option<String> {
    ["error", "message", "status"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .map(|message| message.chars().take(512).collect())
}

fn linked_display_names(value: &Value) -> Vec<String> {
    fn visit(value: &Value, names: &mut Vec<String>) {
        match value {
            Value::Object(object) => {
                if let Some(name) = object.get("display_name").and_then(Value::as_str) {
                    let name = name.trim();
                    if !name.is_empty() && !names.iter().any(|existing| existing == name) {
                        names.push(name.to_string());
                    }
                }
                for child in object.values() {
                    visit(child, names);
                }
            }
            Value::Array(values) => {
                for child in values {
                    visit(child, names);
                }
            }
            _ => {}
        }
    }

    let mut names = Vec::new();
    visit(value, &mut names);
    names
}

#[cfg(test)]
mod tests {
    use super::{linked_display_names, load_linked_accounts, save_linked_accounts};

    #[test]
    fn finds_linked_account_names_in_common_response_shapes() {
        let response = serde_json::json!({
            "linked_accounts": [
                {"display_name": "first"},
                {"display_name": "second"},
            ]
        });
        assert_eq!(linked_display_names(&response), ["first", "second"]);
    }

    #[test]
    fn linked_accounts_round_trip() {
        let path = std::env::temp_dir().join(format!(
            "hebnix-discord-accounts-{}.json",
            std::process::id()
        ));
        let accounts = vec!["first".to_string(), "second".to_string()];
        save_linked_accounts(&path, &accounts).unwrap();
        assert_eq!(load_linked_accounts(&path), accounts);
        let _ = std::fs::remove_file(path);
    }
}
