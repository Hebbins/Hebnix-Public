//! EOS oauth tokens for RL, steam + epic
//!
// ported from the python hebnix.eos package. two paths:
//  - steam: load steam_api64.dll, get an auth session ticket, POST it to epic
//    oauth as an external_auth grant (see steam, ported from the C++ eos_client)
//  - epic: scan EpicGamesLauncher.exe memory for the eg1~eyJ... bearer token,
//    swap it for an exchange_code via the EGS oauth api, POST that to epic oauth
//    (see memory)
// both give an EOSToken. a refresh token can be reused via load_from_refresh
// with no platform interaction.

pub mod memory;
pub mod steam;

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// RL's EOS creds (from RLAPI/egs.go, same as the python/C++ clients)
const EOS_CLIENT_ID: &str = "xyza7891p5D7s9R6Gm6moTHWGloerp7B";
const EOS_CLIENT_SECRET: &str = "Knh18du4NVlFs+3uQ+ZPpDCVto0WYf4yXP8+OcwVt1o";
const EOS_DEPLOYMENT_ID: &str = "da32ae9c12ae40e8a112c52e1f17f3ba";
const EOS_OAUTH_URL: &str = "https://api.epicgames.dev/epic/oauth/v2/token";
const EGS_EXCHANGE_URL: &str =
    "https://account-public-service-prod03.ol.epicgames.com/account/api/oauth/exchange";

const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// which account source an EOSToken came from
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Steam,
    Epic,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Steam => "steam",
            Platform::Epic => "epic",
        }
    }
}

/// an EOS oauth token plus the identity fields that come with it
// account_id is the epic account id (EOS accounts are epic accounts even via
// steam). steam_id is the steamid64, only set on the steam path. rlapi/psynet
// needs both.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EOSToken {
    #[serde(alias = "AccessToken")]
    pub access_token: String,
    #[serde(alias = "RefreshToken")]
    pub refresh_token: String,
    #[serde(alias = "AccountId", alias = "account_id")]
    pub account_id: String,
    #[serde(alias = "ExpiresAt")]
    pub expires_at: String,
    #[serde(alias = "Platform")]
    pub platform: String,
    #[serde(alias = "SteamID", alias = "steam_id")]
    pub steam_id: String,
    #[serde(alias = "DisplayName")]
    pub display_name: String,
}

impl EOSToken {
    /// true if expires_at (iso-8601, e.g. 2026-07-02T15:32:06.605Z) is in the
    /// past. missing/unparseable timestamps count as not expired.
    pub fn expired(&self) -> bool {
        if self.expires_at.is_empty() {
            return false;
        }
        match parse_iso8601_unix(&self.expires_at) {
            Some(exp) => now_unix() > exp,
            None => false,
        }
    }

    fn from_oauth(body: &Value, platform: &str) -> Option<EOSToken> {
        let access_token = body.get("access_token")?.as_str()?.to_string();
        if access_token.is_empty() {
            return None;
        }
        Some(EOSToken {
            access_token,
            refresh_token: str_field(body, "refresh_token"),
            account_id: str_field(body, "account_id"),
            expires_at: str_field(body, "expires_at"),
            platform: platform.to_string(),
            steam_id: String::new(),
            display_name: str_field(body, "display_name"),
        })
    }
}

// public api

/// fresh EOS token for platform. always does platform auth, no caching. use
/// load_from_refresh to reuse a refresh token.
pub fn get_eos_token(platform: Platform) -> Option<EOSToken> {
    let mut token = match platform {
        Platform::Steam => match steam::get_ticket() {
            Ok((ticket_hex, steam_id)) => steam_to_eos(&ticket_hex, &steam_id),
            Err(e) => {
                tracing::warn!("eos: steam ticket failed: {e}");
                None
            }
        },
        Platform::Epic => match scan_egl_token() {
            Some(egl) => egl_to_eos(&egl),
            None => {
                tracing::warn!("eos: no EpicGamesLauncher bearer token found in memory");
                None
            }
        },
    };
    if let Some(t) = token.as_mut() {
        t.platform = platform.as_str().to_string();
    }
    token
}

/// just the access-token string
pub fn get_access_token(platform: Platform) -> Option<String> {
    get_eos_token(platform).map(|t| t.access_token)
}

/// just the refresh-token string
pub fn get_refresh_token(platform: Platform) -> Option<String> {
    get_eos_token(platform).map(|t| t.refresh_token)
}

/// swap a bare refresh-token string for a fresh EOSToken (no platform needed).
/// platform field left blank since the refresh response may omit account_id.
pub fn load_from_refresh(refresh_token: &str) -> Option<EOSToken> {
    refresh(refresh_token).map(|mut t| {
        t.platform = String::new();
        t
    })
}

/// which token(s) to write when caching. metadata (account_id, expires_at,
/// platform, steam_id) is always written, this only picks which tokens go in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveContent {
    /// access token only (+ metadata)
    AccessOnly,
    /// refresh token only (+ metadata), enough to mint access tokens later via
    /// load_from_refresh
    RefreshOnly,
    /// both tokens (+ metadata)
    Both,
}

/// on-disk cache format
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveFormat {
    /// Key=Value lines (compatible with the python/C++ EOS_token.txt)
    Text,
    /// pretty json, snake_case keys
    Json,
    /// pick from the extension: .json -> Json, else Text
    Auto,
}

impl SaveFormat {
    fn resolve(self, path: &std::path::Path) -> SaveFormat {
        match self {
            SaveFormat::Auto => {
                let is_json = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("json"))
                    .unwrap_or(false);
                if is_json {
                    SaveFormat::Json
                } else {
                    SaveFormat::Text
                }
            }
            other => other,
        }
    }
}

/// write token to whatever path/name/ext you want, choosing which tokens and
/// what format.
// only thing in here that touches disk, nothing gets written implicitly (the
// C++ client's habit of dumping to ~/Documents is deliberately not copied).
// parent dirs created as needed.
pub fn save_token(
    token: &EOSToken,
    path: impl AsRef<std::path::Path>,
    content: SaveContent,
    format: SaveFormat,
) -> std::io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let want_access = matches!(content, SaveContent::AccessOnly | SaveContent::Both);
    let want_refresh = matches!(content, SaveContent::RefreshOnly | SaveContent::Both);

    let text = match format.resolve(path) {
        SaveFormat::Json => {
            let mut map = serde_json::Map::new();
            if want_access {
                map.insert("access_token".into(), token.access_token.clone().into());
            }
            if want_refresh {
                map.insert("refresh_token".into(), token.refresh_token.clone().into());
            }
            map.insert("account_id".into(), token.account_id.clone().into());
            map.insert("expires_at".into(), token.expires_at.clone().into());
            map.insert("platform".into(), token.platform.clone().into());
            if !token.steam_id.is_empty() {
                map.insert("steam_id".into(), token.steam_id.clone().into());
            }
            if !token.display_name.is_empty() {
                map.insert("display_name".into(), token.display_name.clone().into());
            }
            serde_json::to_string_pretty(&Value::Object(map)).unwrap_or_default()
        }
        SaveFormat::Text | SaveFormat::Auto => {
            let mut lines = Vec::new();
            if want_access {
                lines.push(format!("AccessToken={}", token.access_token));
            }
            if want_refresh {
                lines.push(format!("RefreshToken={}", token.refresh_token));
            }
            lines.push(format!("AccountId={}", token.account_id));
            lines.push(format!("ExpiresAt={}", token.expires_at));
            lines.push(format!("Platform={}", token.platform));
            if !token.steam_id.is_empty() {
                lines.push(format!("SteamID={}", token.steam_id));
            }
            lines.join("\n") + "\n"
        }
    };
    std::fs::write(path, text)
}

/// load an EOSToken written by save_token. sniffs json (starts with {) vs the
/// Key=Value text format, takes both snake_case and TitleCase keys. None if the
/// file is missing or has no access/refresh token.
pub fn load_token(path: impl AsRef<std::path::Path>) -> Option<EOSToken> {
    let raw = std::fs::read_to_string(path).ok()?;
    let token = if raw.trim_start().starts_with('{') {
        serde_json::from_str::<EOSToken>(&raw).ok()?
    } else {
        parse_text_token(&raw)
    };
    if token.access_token.is_empty() && token.refresh_token.is_empty() {
        None
    } else {
        Some(token)
    }
}

fn parse_text_token(text: &str) -> EOSToken {
    let mut token = EOSToken::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().to_string();
        match k.trim() {
            "AccessToken" | "access_token" => token.access_token = v,
            "RefreshToken" | "refresh_token" => token.refresh_token = v,
            "AccountId" | "account_id" => token.account_id = v,
            "ExpiresAt" | "expires_at" => token.expires_at = v,
            "Platform" | "platform" => token.platform = v,
            "SteamID" | "steam_id" => token.steam_id = v,
            "DisplayName" | "display_name" => token.display_name = v,
            _ => {}
        }
    }
    token
}

/// save both tokens, format from the extension
pub fn save_to(token: &EOSToken, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
    save_token(token, path, SaveContent::Both, SaveFormat::Auto)
}

/// alias for load_token
pub fn load_from(path: impl AsRef<std::path::Path>) -> Option<EOSToken> {
    load_token(path)
}

// oauth exchanges

/// steam session ticket -> EOS token (external_auth grant)
fn steam_to_eos(ticket_hex: &str, steam_id: &str) -> Option<EOSToken> {
    let body = format!(
        "grant_type=external_auth\
         &external_auth_type=steam_session_ticket\
         &external_auth_token={ticket_hex}\
         &deployment_id={EOS_DEPLOYMENT_ID}\
         &scope=basic_profile"
    );
    let resp = oauth_post(&body)?;
    let mut token = EOSToken::from_oauth(&resp, "steam")?;
    // oauth response has the epic account_id but not the steamid, so keep the
    // one we authed with (rlapi steam auth needs both)
    token.steam_id = steam_id.to_string();
    Some(token)
}

/// EGL bearer token -> EOS token (exchange_code grant)
fn egl_to_eos(egl_token: &str) -> Option<EOSToken> {
    let exch = http_get_json(EGS_EXCHANGE_URL, &format!("bearer {egl_token}"))?;
    let code = exch.get("code").and_then(Value::as_str).unwrap_or("");
    if code.is_empty() {
        tracing::warn!("eos: EGS exchange returned no code");
        return None;
    }
    let body = format!(
        "grant_type=exchange_code\
         &exchange_code={code}\
         &deployment_id={EOS_DEPLOYMENT_ID}\
         &scope=basic_profile"
    );
    let resp = oauth_post(&body)?;
    EOSToken::from_oauth(&resp, "epic")
}

/// refresh token -> fresh access token (refresh_token grant)
fn refresh(refresh_token: &str) -> Option<EOSToken> {
    let body = format!(
        "grant_type=refresh_token\
         &refresh_token={refresh_token}\
         &deployment_id={EOS_DEPLOYMENT_ID}\
         &scope=basic_profile"
    );
    let resp = oauth_post(&body)?;
    // Keep the caller's refresh token if the response omits a new one.
    let mut token = EOSToken::from_oauth(&resp, "")?;
    if token.refresh_token.is_empty() {
        token.refresh_token = refresh_token.to_string();
    }
    Some(token)
}

/// Scan `EpicGamesLauncher.exe` for a valid `eg1~eyJ...` bearer token: prefer
/// the `eg1~`-prefixed form, fall back to a bare JWT, and validate that the
/// payload has the `app`+`sub` claims (matching the Python heuristic).
fn scan_egl_token() -> Option<String> {
    let pid = memory::find_process("EpicGamesLauncher.exe")?;
    let mut candidates = memory::scan_memory(pid, b"eg1~eyJ");
    if candidates.is_empty() {
        candidates = memory::scan_memory(pid, b"eyJ");
    }
    for tok in candidates {
        let raw = tok.strip_prefix("eg1~").unwrap_or(&tok);
        let mut parts = raw.split('.');
        let (_header, payload) = (parts.next()?, parts.next());
        let Some(payload) = payload else { continue };
        if let Some(claims) = decode_jwt_payload(payload) {
            if claims.get("app").is_some() && claims.get("sub").is_some() {
                return Some(tok);
            }
        }
    }
    None
}

// http

/// post a form body to the eos oauth endpoint with http basic auth. parsed
/// json on 200, else logs + None.
fn oauth_post(body: &str) -> Option<Value> {
    let auth = format!(
        "Basic {}",
        base64_encode(format!("{EOS_CLIENT_ID}:{EOS_CLIENT_SECRET}").as_bytes())
    );
    let resp = ureq::post(EOS_OAUTH_URL)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .set("Authorization", &auth)
        .timeout(HTTP_TIMEOUT)
        .send_string(body);
    match resp {
        Ok(r) => r.into_json::<Value>().ok(),
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            tracing::warn!("eos: OAuth HTTP {code}: {}", truncate(&text, 300));
            None
        }
        Err(e) => {
            tracing::warn!("eos: OAuth request failed: {e}");
            None
        }
    }
}

/// GET a URL with an `Authorization` header, returning parsed JSON on 200.
fn http_get_json(url: &str, auth: &str) -> Option<Value> {
    let resp = ureq::get(url)
        .set("Authorization", auth)
        .timeout(HTTP_TIMEOUT)
        .call();
    match resp {
        Ok(r) => r.into_json::<Value>().ok(),
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            tracing::warn!("eos: EGS exchange HTTP {code}: {}", truncate(&text, 300));
            None
        }
        Err(e) => {
            tracing::warn!("eos: EGS exchange request failed: {e}");
            None
        }
    }
}

// helpers

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

/// Standard base64 encoding (for the HTTP Basic auth header).
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Decode a base64url (no-padding) JWT payload segment into JSON.
fn decode_jwt_payload(segment: &str) -> Option<Value> {
    let bytes = base64url_decode(segment)?;
    serde_json::from_slice(&bytes).ok()
}

/// base64url decoder tolerant of missing padding (JWT segments omit `=`).
fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    let cleaned: Vec<u8> = input.bytes().filter(|&c| c != b'=').collect();
    let mut out = Vec::with_capacity(cleaned.len() * 3 / 4);
    for chunk in cleaned.chunks(4) {
        let mut n = 0u32;
        let mut bits = 0;
        for &c in chunk {
            n = (n << 6) | val(c)?;
            bits += 6;
        }
        // Emit the whole bytes represented by the accumulated bits.
        let bytes = bits / 8;
        n <<= (4 - chunk.len()) * 6; // left-align partial group
        for i in 0..bytes {
            out.push(((n >> (16 - i * 8)) & 0xFF) as u8);
        }
    }
    Some(out)
}

fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Parse an ISO-8601 timestamp (`YYYY-MM-DDTHH:MM:SS[.fff]Z`) to a Unix
/// timestamp. Handles the fixed EOS format; returns `None` otherwise.
fn parse_iso8601_unix(s: &str) -> Option<f64> {
    let s = s.trim();
    let s = s.strip_suffix('Z').unwrap_or(s);
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let mut t = time.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let minute: i64 = t.next()?.parse().ok()?;
    let sec_part = t.next()?;
    let second: f64 = sec_part.parse().ok()?;

    // Days since Unix epoch via a civil-date algorithm (Howard Hinnant's).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;

    Some((days * 86400 + hour * 3600 + minute * 60) as f64 + second)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_reference() {
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(base64_encode(b"a:b"), "YTpi");
        assert_eq!(base64_encode(b"client:secret"), "Y2xpZW50OnNlY3JldA==");
    }

    #[test]
    fn base64url_decodes_jwt_payload() {
        // {"app":"rl","sub":"abc"} base64url without padding
        let payload = "eyJhcHAiOiJybCIsInN1YiI6ImFiYyJ9";
        let json = decode_jwt_payload(payload).expect("decodes");
        assert_eq!(json.get("app").and_then(Value::as_str), Some("rl"));
        assert_eq!(json.get("sub").and_then(Value::as_str), Some("abc"));
    }

    #[test]
    fn iso8601_parses_and_detects_expiry() {
        // 2000-01-01T00:00:00Z = 946684800 unix
        let ts = parse_iso8601_unix("2000-01-01T00:00:00Z").unwrap();
        assert!((ts - 946684800.0).abs() < 1.0);

        let past = EOSToken {
            expires_at: "2000-01-01T00:00:00Z".into(),
            ..Default::default()
        };
        assert!(past.expired());

        let future = EOSToken {
            expires_at: "2999-01-01T00:00:00Z".into(),
            ..Default::default()
        };
        assert!(!future.expired());

        let none = EOSToken::default();
        assert!(!none.expired());
    }

    fn sample_token() -> EOSToken {
        EOSToken {
            access_token: "acc".into(),
            refresh_token: "ref".into(),
            account_id: "id".into(),
            expires_at: "2026-01-01T00:00:00Z".into(),
            platform: "steam".into(),
            steam_id: "76561198000000000".into(),
            display_name: String::new(),
        }
    }

    #[test]
    fn save_and_load_roundtrip_text() {
        let dir = std::env::temp_dir().join("hebnix_eos_test_txt");
        let path = dir.join("EOS_token.txt");
        save_to(&sample_token(), &path).unwrap();
        let loaded = load_from(&path).expect("loads");
        assert_eq!(loaded.access_token, "acc");
        assert_eq!(loaded.refresh_token, "ref");
        assert_eq!(loaded.platform, "steam");
        assert_eq!(loaded.steam_id, "76561198000000000");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_and_load_roundtrip_json() {
        let dir = std::env::temp_dir().join("hebnix_eos_test_json");
        let path = dir.join("cache.json");
        save_token(&sample_token(), &path, SaveContent::Both, SaveFormat::Auto).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.trim_start().starts_with('{'),
            "auto -> json by extension"
        );
        let loaded = load_token(&path).expect("loads");
        assert_eq!(loaded.access_token, "acc");
        assert_eq!(loaded.refresh_token, "ref");
        assert_eq!(loaded.steam_id, "76561198000000000");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_content_selects_tokens() {
        let dir = std::env::temp_dir().join("hebnix_eos_test_sel");
        let refresh_path = dir.join("refresh.json");
        save_token(
            &sample_token(),
            &refresh_path,
            SaveContent::RefreshOnly,
            SaveFormat::Json,
        )
        .unwrap();
        let raw = std::fs::read_to_string(&refresh_path).unwrap();
        assert!(raw.contains("refresh_token"));
        assert!(
            !raw.contains("access_token"),
            "refresh-only must omit access"
        );

        let access_path = dir.join("access.txt");
        save_token(
            &sample_token(),
            &access_path,
            SaveContent::AccessOnly,
            SaveFormat::Text,
        )
        .unwrap();
        let raw = std::fs::read_to_string(&access_path).unwrap();
        assert!(raw.contains("AccessToken="));
        assert!(
            !raw.contains("RefreshToken="),
            "access-only must omit refresh"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
