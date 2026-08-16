//! rlapi: rocket league's internal psynet api.
//!
//! psynet's protocol (hmac-signed websocket rpc, reverse-engineered keys + per
//! build headers) lives in the go rlapi-bridge.exe. it runs as a persistent
//! stdin/stdout json-rpc subprocess, we spawn it with an eos token (that's why
//! eos auth exists) and talk to it a line at a time:
//!   -> {"id":"1","service":"Skills/GetPlayerSkill v1","body":{"PlayerID":"Steam|76561..|0"}}
//!   <- {"id":"1","ok":true,"result":{..}}
//!
//! request() sends any service by name, returns raw json. typed wrappers cover
//! the common ones. everything in REQUESTS.md is reachable via request().

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

use crate::eos::{self, EOSToken, Platform};

/// Locate `rlapi-bridge.exe`. Lookup order:
/// 1. `HEBNIX_RLAPI_BRIDGE` (explicit full path).
/// 2. `rlapi-bridge.exe` next to the running executable.
/// 3. `dist/rlapi-bridge.exe` next to the running executable.
/// 4. `rlapi-bridge`/`rlapi-bridge.exe` on `PATH`.
pub fn bridge_binary() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("HEBNIX_RLAPI_BRIDGE") {
        let path = std::path::PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    let exe_name = if cfg!(windows) {
        "rlapi-bridge.exe"
    } else {
        "rlapi-bridge"
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in [dir.join(exe_name), dir.join("dist").join(exe_name)] {
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    // Fall back to PATH resolution by the OS.
    Some(std::path::PathBuf::from(exe_name))
}

/// Credentials + platform for a PsyNet authentication.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// EOS access token (the `AuthTicket`).
    pub token: String,
    /// Epic account ID (EOS accounts are Epic accounts).
    pub account_id: String,
    /// steamid64, needed for steam, ignored for epic
    pub steam_id: Option<String>,
    pub platform: Platform,
}

impl AuthConfig {
    /// Build a config from an [`EOSToken`] obtained for `platform`.
    pub fn from_token(token: &EOSToken, platform: Platform) -> Self {
        let steam_id = match platform {
            Platform::Steam if !token.steam_id.is_empty() => Some(token.steam_id.clone()),
            _ => None,
        };
        AuthConfig {
            token: token.access_token.clone(),
            account_id: token.account_id.clone(),
            steam_id,
            platform,
        }
    }
}

/// An authenticated PsyNet session, backed by the bridge subprocess.
///
/// Dropping the session sends a graceful `__close__` and reaps the child.
pub struct RlApi {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: u64,
}

impl RlApi {
    /// Acquire an EOS token for `platform` and open a PsyNet session in one
    /// step. This is the common entry point.
    pub fn connect_platform(platform: Platform) -> Result<Self, String> {
        let token = eos::get_eos_token(platform)
            .ok_or_else(|| format!("could not obtain an EOS token for {}", platform.as_str()))?;
        Self::connect_with_token(&token, platform)
    }

    /// Open a PsyNet session using an already-acquired [`EOSToken`].
    pub fn connect_with_token(token: &EOSToken, platform: Platform) -> Result<Self, String> {
        Self::connect(&AuthConfig::from_token(token, platform))
    }

    /// Spawn the bridge with explicit credentials and complete authentication.
    pub fn connect(cfg: &AuthConfig) -> Result<Self, String> {
        if cfg.token.is_empty() || cfg.account_id.is_empty() {
            return Err("rlapi: token and account_id are required".to_string());
        }
        if cfg.platform == Platform::Steam && cfg.steam_id.as_deref().unwrap_or("").is_empty() {
            return Err("rlapi: steam_id is required for the Steam platform".to_string());
        }

        let bin = bridge_binary().ok_or("rlapi-bridge.exe not found")?;
        let mut cmd = Command::new(&bin);
        cmd.arg("--token")
            .arg(&cfg.token)
            .arg("--account")
            .arg(&cfg.account_id)
            .arg("--platform")
            .arg(cfg.platform.as_str())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(sid) = &cfg.steam_id {
            cmd.arg("--steam-id").arg(sid);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("rlapi: failed to launch {}: {e}", bin.display()))?;
        let stdin = child.stdin.take().ok_or("rlapi: no stdin pipe")?;
        let stdout = child.stdout.take().ok_or("rlapi: no stdout pipe")?;
        let mut reader = BufReader::new(stdout);

        // The bridge emits one `__init__` line reporting auth success/failure.
        let init = read_response(&mut reader)?;
        if !init.ok {
            let _ = child.kill();
            return Err(format!(
                "rlapi: authentication failed: {}",
                if init.error.is_empty() {
                    "unknown".into()
                } else {
                    init.error
                }
            ));
        }
        tracing::info!(
            platform = cfg.platform.as_str(),
            "rlapi: PsyNet session established"
        );

        Ok(RlApi {
            child,
            stdin,
            reader,
            next_id: 0,
        })
    }

    /// Send any PsyNet service by name (e.g. `"Skills/GetPlayerSkill v1"`) with
    /// a JSON body, returning the raw JSON result.
    pub fn request(&mut self, service: &str, body: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id.to_string();
        let line = json!({ "id": id, "service": service, "body": body });
        self.write_line(&line.to_string())?;

        // The bridge processes requests sequentially; still match on id and
        // skip any stray line defensively.
        loop {
            let resp = read_response(&mut self.reader)?;
            if resp.id != id {
                tracing::debug!(got = %resp.id, want = %id, "rlapi: skipping unmatched response");
                continue;
            }
            if resp.ok {
                return Ok(resp.result);
            }
            return Err(resp.error);
        }
    }

    /// Health-check the bridge/connection.
    pub fn ping(&mut self) -> bool {
        if self.write_line(r#"{"id":"__ping__"}"#).is_err() {
            return false;
        }
        loop {
            match read_response(&mut self.reader) {
                Ok(resp) if resp.id == "__ping__" => return resp.ok,
                Ok(_) => continue,
                Err(_) => return false,
            }
        }
    }

    /// Gracefully close the session and wait for the bridge to exit.
    pub fn close(mut self) {
        let _ = self.write_line(r#"{"id":"__close__"}"#);
        let _ = self.child.wait();
    }

    fn write_line(&mut self, line: &str) -> Result<(), String> {
        writeln!(self.stdin, "{line}").map_err(|e| format!("rlapi: write failed: {e}"))?;
        self.stdin
            .flush()
            .map_err(|e| format!("rlapi: flush failed: {e}"))
    }

    // -- Typed convenience wrappers (a sampling; use `request` for the rest) --

    /// Skills/GetPlayerSkill v1: ranked skill/mmr for one player
    pub fn get_player_skill(&mut self, player_id: &str) -> Result<Value, String> {
        self.request("Skills/GetPlayerSkill v1", json!({ "PlayerID": player_id }))
    }

    /// Players/GetProfile v1: public profile for one player
    pub fn get_profile(&mut self, player_id: &str) -> Result<Value, String> {
        self.request("Players/GetProfile v1", json!({ "PlayerID": player_id }))
    }

    /// Population/GetPopulation v1: online pop per playlist
    pub fn get_population(&mut self) -> Result<Value, String> {
        self.request("Population/GetPopulation v1", json!({}))
    }

    /// Playlists/GetActivePlaylists v1: currently enabled playlists
    pub fn get_active_playlists(&mut self) -> Result<Value, String> {
        self.request("Playlists/GetActivePlaylists v1", json!({}))
    }
}

impl Drop for RlApi {
    fn drop(&mut self) {
        // Best-effort graceful shutdown; then make sure the child is reaped.
        let _ = writeln!(self.stdin, r#"{{"id":"__close__"}}"#);
        let _ = self.stdin.flush();
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            _ => {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}

/// Build a PsyNet PlayerID string: `"<Platform>|<id>|0"` (e.g.
/// `"Steam|76561197960287930|0"`). PsyNet uses capitalised platform names.
pub fn player_id(platform: Platform, id: &str) -> String {
    let name = match platform {
        Platform::Steam => "Steam",
        Platform::Epic => "Epic",
    };
    format!("{name}|{id}|0")
}

/// Parsed bridge response line.
struct BridgeResponse {
    id: String,
    ok: bool,
    result: Value,
    error: String,
}

/// Read the next non-empty response line from the bridge.
fn read_response(reader: &mut BufReader<ChildStdout>) -> Result<BridgeResponse, String> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| format!("rlapi: read failed: {e}"))?;
        if n == 0 {
            return Err("rlapi: bridge closed the connection".to_string());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let v: Value =
            serde_json::from_str(trimmed).map_err(|e| format!("rlapi: bad response line: {e}"))?;
        return Ok(BridgeResponse {
            id: v
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            ok: v.get("ok").and_then(Value::as_bool).unwrap_or(false),
            result: v.get("result").cloned().unwrap_or(Value::Null),
            error: v
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_id_format() {
        assert_eq!(
            player_id(Platform::Steam, "76561197960287930"),
            "Steam|76561197960287930|0"
        );
        assert_eq!(player_id(Platform::Epic, "abc123"), "Epic|abc123|0");
    }

    #[test]
    fn auth_config_from_token_scopes_steam_id() {
        let mut token = EOSToken::default();
        token.access_token = "t".into();
        token.account_id = "acc".into();
        token.steam_id = "76561198000000000".into();

        let steam = AuthConfig::from_token(&token, Platform::Steam);
        assert_eq!(steam.steam_id.as_deref(), Some("76561198000000000"));

        let epic = AuthConfig::from_token(&token, Platform::Epic);
        assert_eq!(epic.steam_id, None, "steam_id must not leak into Epic auth");
    }
}
