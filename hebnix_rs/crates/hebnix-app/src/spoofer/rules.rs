// crates/hebnix-app/src/spoofer/rules.rs
//! spoof rules. each one picks a host and says how to rewrite the body.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct Body<'a> {
    pub content_type: &'a str,
    /// Request path, when the response came through the HTTP proxy.  Rules must
    /// use this for endpoints which share a host with unrelated account APIs.
    pub request_path: Option<&'a str>,
    pub bytes: Vec<u8>,
    pub set_headers: Vec<(String, String)>, // these replace whatevers already there
    pub response_headers: Vec<(String, String)>,
}

impl<'a> Body<'a> {
    pub fn new(content_type: &'a str, bytes: Vec<u8>) -> Self {
        Self {
            content_type,
            request_path: None,
            bytes,
            set_headers: Vec::new(),
            response_headers: Vec::new(),
        }
    }
}

pub trait Rule: Send + Sync {
    fn matches_host(&self, host: &str) -> bool;
    /// dropped before forwarding, lowercase
    fn strip_request_headers(&self) -> &[&str] {
        &[]
    }
    /// true if it changed anything
    fn rewrite(&self, body: &mut Body) -> bool;
    /// one console line the first time it fires, None after. it repeats a lot.
    fn announce(&self) -> Option<String> {
        None
    }
}

/// Observes PsyNet inventory responses without modifying them. Rocket League wraps
/// some RPC payloads, so inspect both the whole body and each JSON-looking suffix.
pub struct OwnedProductsRule {
    owned: Arc<Mutex<HashSet<i64>>>,
    cache_path: PathBuf,
}

impl OwnedProductsRule {
    pub fn new(owned: Arc<Mutex<HashSet<i64>>>, cache_path: PathBuf) -> Self {
        Self { owned, cache_path }
    }

    fn collect(value: &serde_json::Value, ids: &mut HashSet<i64>) {
        match value {
            serde_json::Value::Object(object) => {
                for (key, value) in object {
                    let normalized = key
                        .chars()
                        .filter(|character| character.is_ascii_alphanumeric())
                        .flat_map(char::to_lowercase)
                        .collect::<String>();
                    if normalized == "productid" {
                        if let Some(id) = value
                            .as_i64()
                            .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
                        {
                            ids.insert(id);
                        }
                    }
                    Self::collect(value, ids);
                }
            }
            serde_json::Value::Array(array) => {
                for value in array {
                    Self::collect(value, ids);
                }
            }
            _ => {}
        }
    }
}

impl Rule for OwnedProductsRule {
    fn matches_host(&self, host: &str) -> bool {
        host.eq_ignore_ascii_case("psynet.gg") || host.to_ascii_lowercase().ends_with(".psynet.gg")
    }

    fn rewrite(&self, body: &mut Body) -> bool {
        let mut found = HashSet::new();
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body.bytes) {
            Self::collect(&value, &mut found);
        } else {
            for start in body
                .bytes
                .iter()
                .enumerate()
                .filter_map(|(index, byte)| (*byte == b'{' || *byte == b'[').then_some(index))
            {
                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body.bytes[start..])
                {
                    Self::collect(&value, &mut found);
                    if !found.is_empty() {
                        break;
                    }
                }
            }
        }
        if found.is_empty() {
            return false;
        }
        if let Ok(mut owned) = self.owned.lock() {
            let previous = owned.len();
            owned.extend(found);
            if owned.len() != previous {
                if let Some(parent) = self.cache_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let mut ids = owned.iter().copied().collect::<Vec<_>>();
                ids.sort_unstable();
                if let Ok(json) = serde_json::to_vec_pretty(&ids) {
                    let _ = std::fs::write(&self.cache_path, json);
                }
            }
        }
        false
    }
}

/// swaps displayName in the eos account response, a list with one object in it
pub struct NameRule {
    pub name: Arc<Mutex<String>>,
    announced: AtomicBool,
}

impl NameRule {
    pub fn new(name: Arc<Mutex<String>>) -> Self {
        Self {
            name,
            announced: AtomicBool::new(false),
        }
    }
}

// epicgames.dev only, mitm'ing *.epicgames.com breaks login
const NAME_HOSTS: [&str; 1] = ["api.epicgames.dev"];

// the eos accont response, nothing else looks like this
const ACCOUNT_KEYS: [&str; 5] = [
    "accountId",
    "displayName",
    "preferredLanguage",
    "linkedAccounts",
    "cabinedMode",
];

impl Rule for NameRule {
    fn matches_host(&self, host: &str) -> bool {
        NAME_HOSTS.iter().any(|&d| host.eq_ignore_ascii_case(d))
    }

    fn rewrite(&self, body: &mut Body) -> bool {
        if !body.content_type.contains("application/json") {
            return false;
        }
        // The account host also serves the friends roster.  C# limits the name
        // replacement to the SDK account endpoint; applying it to every
        // single-entry account response can make the roster appear empty.
        if !body
            .request_path
            .is_some_and(|path| path.contains("/epic/id/v2/sdk/accounts"))
        {
            return false;
        }
        let Ok(mut val) = serde_json::from_slice::<serde_json::Value>(&body.bytes) else {
            return false;
        };
        let Some(arr) = val.as_array_mut() else {
            return false;
        };
        if arr.len() != 1 {
            return false;
        }
        let Some(obj) = arr[0].as_object_mut() else {
            return false;
        };
        if !ACCOUNT_KEYS.iter().all(|k| obj.contains_key(*k)) {
            return false;
        }
        if !obj["linkedAccounts"].is_array() || !obj["cabinedMode"].is_boolean() {
            return false;
        }

        let new_name = self.name.lock().map(|n| n.clone()).unwrap_or_default();
        if new_name.is_empty() || obj["displayName"] == serde_json::Value::String(new_name.clone())
        {
            return false;
        }
        obj.insert(
            "displayName".to_string(),
            serde_json::Value::String(new_name),
        );
        match serde_json::to_vec(&val) {
            Ok(out) => {
                body.bytes = out;
                true
            }
            Err(_) => false,
        }
    }

    fn announce(&self) -> Option<String> {
        (!self.announced.swap(true, Ordering::Relaxed)).then(|| {
            let name = self
                .name
                .lock()
                .map(|name| name.clone())
                .unwrap_or_default();
            format!("Username Spoofed to {name}")
        })
    }
}

pub struct FriendsRule {
    pub spoofs: Arc<Mutex<HashMap<String, String>>>,
    pub discovered: Arc<Mutex<HashMap<String, String>>>,
    announced: AtomicBool,
}

impl FriendsRule {
    pub fn new(
        spoofs: Arc<Mutex<HashMap<String, String>>>,
        discovered: Arc<Mutex<HashMap<String, String>>>,
    ) -> Self {
        Self {
            spoofs,
            discovered,
            announced: AtomicBool::new(false),
        }
    }
}

impl Rule for FriendsRule {
    fn matches_host(&self, host: &str) -> bool {
        NAME_HOSTS.iter().any(|&d| host.eq_ignore_ascii_case(d))
    }

    fn rewrite(&self, body: &mut Body) -> bool {
        if !body.content_type.contains("application/json") {
            return false;
        }
        let Ok(mut val) = serde_json::from_slice::<serde_json::Value>(&body.bytes) else {
            return false;
        };
        let Some(arr) = val.as_array_mut() else {
            return false;
        };

        if arr.len() <= 1 {
            return false;
        }

        let mut modified = false;
        let spoofs = self.spoofs.lock().unwrap().clone();
        let mut discovered = self.discovered.lock().unwrap();

        for item in arr.iter_mut() {
            if let Some(obj) = item.as_object_mut() {
                if let (Some(acc_id), Some(disp)) = (
                    obj.get("accountId").and_then(|v| v.as_str()),
                    obj.get("displayName").and_then(|v| v.as_str()),
                ) {
                    discovered.insert(acc_id.to_string(), disp.to_string());

                    if let Some(spoofed_name) = spoofs.get(acc_id) {
                        obj.insert(
                            "displayName".to_string(),
                            serde_json::Value::String(spoofed_name.clone()),
                        );

                        if let Some(linked) =
                            obj.get_mut("linkedAccounts").and_then(|v| v.as_array_mut())
                        {
                            for link in linked.iter_mut() {
                                if let Some(link_obj) = link.as_object_mut() {
                                    if link_obj.contains_key("displayName") {
                                        link_obj.insert(
                                            "displayName".to_string(),
                                            serde_json::Value::String(spoofed_name.clone()),
                                        );
                                    }
                                }
                            }
                        }
                        modified = true;
                    }
                }
            }
        }

        if modified {
            if let Ok(out) = serde_json::to_vec(&val) {
                body.bytes = out;
                return true;
            }
        }
        false
    }

    fn announce(&self) -> Option<String> {
        (!self.announced.swap(true, Ordering::Relaxed)).then_some("Friends Spoofed".to_string())
    }
}

pub struct TitleRule {
    pub settings: Arc<Mutex<TitleSettings>>,
    announced: AtomicBool,
}

impl TitleRule {
    pub fn new(settings: Arc<Mutex<TitleSettings>>) -> Self {
        Self {
            settings,
            announced: AtomicBool::new(false),
        }
    }
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct TitleSpoofSettings {
    pub text: String,
    pub color: String,
    pub glow: bool,
    pub target_id: Option<String>,
}

#[derive(Clone)]
pub struct TitleSettings {
    pub enabled: bool,
    pub titles: Vec<TitleSpoofSettings>,
}

impl Default for TitleSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            titles: Vec::new(),
        }
    }
}

pub const TITLE_HOST: &str = "config.psynet.gg";

const PSY_KEY: &[u8] = b"cqhyz50f3c3j2pxhwo6b1kypxikah0wh";

fn psysignature(body: &[u8]) -> String {
    use base64::Engine;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let mut mac = <Hmac<Sha256>>::new_from_slice(PSY_KEY).expect("hmac takes any key length");
    mac.update(body);
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

impl Rule for TitleRule {
    fn matches_host(&self, host: &str) -> bool {
        host.contains(TITLE_HOST)
    }

    fn strip_request_headers(&self) -> &[&str] {
        &["if-none-match", "if-modified-since"]
    }

    fn rewrite(&self, body: &mut Body) -> bool {
        if !body.bytes.windows(18).any(|w| w == b"\"PlayerTitleConfig") {
            return false;
        }
        let Ok(mut val) = serde_json::from_slice::<serde_json::Value>(&body.bytes) else {
            return false;
        };

        let settings = self
            .settings
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();
        if !settings.enabled || settings.titles.is_empty() {
            return false;
        }

        let Some(titles) = val
            .get_mut("PlayerTitleConfig")
            .and_then(|c| c.get_mut("Titles"))
            .and_then(|t| t.as_array_mut())
        else {
            return false;
        };

        let mut applied = std::collections::HashSet::new();
        for title in titles.iter_mut() {
            if let Some(obj) = title.as_object_mut() {
                let original_id = obj.get("ID").and_then(serde_json::Value::as_str);
                let spoof = settings
                    .titles
                    .iter()
                    .enumerate()
                    .find(|(_, spoof)| {
                        spoof
                            .target_id
                            .as_deref()
                            .is_some_and(|target| Some(target) == original_id)
                    })
                    .or_else(|| {
                        settings
                            .titles
                            .iter()
                            .enumerate()
                            .find(|(_, spoof)| spoof.target_id.is_none())
                    });
                if let Some((index, spoof)) = spoof
                    .filter(|(_, spoof)| !spoof.text.trim().is_empty() && obj.contains_key("Text"))
                {
                    let category_id = format!("Hebnix_Custom_{index}");
                    obj.insert(
                        "Text".to_string(),
                        serde_json::Value::String(spoof.text.trim().to_string()),
                    );
                    obj.insert(
                        "Category".to_string(),
                        serde_json::Value::String(category_id),
                    );
                    applied.insert(index);
                }
            }
        }
        if applied.is_empty() {
            return false;
        }

        if let Some(categories) = val
            .get_mut("PlayerTitleConfig")
            .and_then(|config| config.get_mut("Categories"))
            .and_then(serde_json::Value::as_array_mut)
        {
            for index in applied {
                let spoof = &settings.titles[index];
                let category_id = format!("Hebnix_Custom_{index}");
                let mut replacement = serde_json::json!({
                    "ID": category_id,
                    "Color": spoof.color,
                });
                if spoof.glow {
                    replacement["GlowColor"] = replacement["Color"].clone();
                }
                if let Some(category) = categories.iter_mut().find(|category| {
                    category.get("ID").and_then(serde_json::Value::as_str)
                        == replacement.get("ID").and_then(serde_json::Value::as_str)
                }) {
                    *category = replacement;
                } else {
                    categories.insert(0, replacement);
                }
            }
        }

        let Ok(out) = serde_json::to_vec(&val) else {
            return false;
        };
        body.set_headers
            .push(("Psysignature".to_string(), psysignature(&out)));
        body.bytes = out;
        true
    }

    fn announce(&self) -> Option<String> {
        (!self.announced.swap(true, Ordering::Relaxed)).then(|| {
            let titles = self
                .settings
                .lock()
                .map(|settings| {
                    settings
                        .titles
                        .iter()
                        .map(|title| title.text.trim())
                        .filter(|title| !title.is_empty())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            format!("Titles Spoofed to {titles}")
        })
    }
}

pub struct RankRule {
    pub spoofs: Arc<Mutex<HashMap<i32, (i32, f64)>>>,
    route_item_spawner: Arc<AtomicBool>,
    announced: AtomicBool,
}

impl RankRule {
    pub fn new(spoofs: Arc<Mutex<HashMap<i32, (i32, f64)>>>) -> Self {
        Self::with_item_spawner(spoofs, Arc::new(AtomicBool::new(false)))
    }

    pub fn with_item_spawner(
        spoofs: Arc<Mutex<HashMap<i32, (i32, f64)>>>,
        route_item_spawner: Arc<AtomicBool>,
    ) -> Self {
        Self {
            spoofs,
            route_item_spawner,
            announced: AtomicBool::new(false),
        }
    }
}

impl Rule for RankRule {
    fn matches_host(&self, host: &str) -> bool {
        // Inspect PsyNet HTTP responses while leaving the game WebSocket alone.
        host.eq_ignore_ascii_case("api.rlpp.psynet.gg")
            || host.eq_ignore_ascii_case("config.psynet.gg")
    }

    fn strip_request_headers(&self) -> &[&str] {
        &["if-none-match", "if-modified-since"]
    }

    fn rewrite(&self, body: &mut Body) -> bool {
        let body_str = match std::str::from_utf8(&body.bytes) {
            Ok(s) => s,
            Err(_) => return false,
        };

        let spoofs = self.spoofs.lock().unwrap().clone();
        if spoofs.is_empty() && !self.route_item_spawner.load(Ordering::Relaxed) {
            return false;
        }
        // Send game RPC through the intercepted config host, then route those
        // paths to the real API backend in proxy.rs.
        if body_str.contains("api.rlpp.psynet.gg") {
            let rewritten = body_str
                .replace(
                    "https:\\/\\/api.rlpp.psynet.gg\\/rpc",
                    "https:\\/\\/config.psynet.gg\\/rpc",
                )
                .replace(
                    "https://api.rlpp.psynet.gg/rpc",
                    "https://config.psynet.gg/rpc",
                )
                .replace(
                    "https://api.rlpp.psynet.gg/Services",
                    "https://config.psynet.gg/Services",
                );
            if rewritten != body_str {
                let bytes = rewritten.into_bytes();
                body.set_headers
                    .push(("Psysignature".into(), psysignature(&bytes)));
                body.bytes = bytes;
                return true;
            }
        }
        if !body_str.contains("\"Skills\"") && !body_str.contains("\"PerConURL") {
            return false;
        }

        let envelope = if let Some(index) = body_str.find("\r\n\r\n") {
            Some((&body_str[..index], &body_str[index + 4..], "\r\n"))
        } else if let Some(index) = body_str.find("\n\n") {
            Some((&body_str[..index], &body_str[index + 2..], "\n"))
        } else {
            None
        };
        let json_body = envelope.map(|(_, json, _)| json).unwrap_or(body_str);

        let mut val: serde_json::Value = match serde_json::from_str(json_body) {
            Ok(v) => v,
            Err(_) => return false,
        };

        let mut modified = false;
        fn rewrite_connection_urls(value: &mut serde_json::Value) -> bool {
            match value {
                serde_json::Value::Object(object) => {
                    object.iter_mut().fold(false, |changed, (key, value)| {
                        let replacement = match key.as_str() {
                            "PerConURL" => {
                                Some("ws://127.0.0.1:8025/ws/gc?PsyConnectionType=Player")
                            }
                            "PerConURLv2" => Some("ws://127.0.0.1:8025/ws/gc2"),
                            _ => None,
                        };
                        if let Some(url) = replacement {
                            let was_different = value.as_str() != Some(url);
                            if was_different {
                                *value = serde_json::Value::String(url.into());
                            }
                            changed || was_different
                        } else {
                            changed | rewrite_connection_urls(value)
                        }
                    })
                }
                serde_json::Value::Array(array) => {
                    array.iter_mut().fold(false, |changed, value| {
                        changed | rewrite_connection_urls(value)
                    })
                }
                _ => false,
            }
        }
        modified |= rewrite_connection_urls(&mut val);

        if let Some(result) = val.get_mut("Result").and_then(|v| v.as_object_mut()) {
            if let Some(skills) = result.get_mut("Skills").and_then(|v| v.as_array_mut()) {
                for skill in skills.iter_mut() {
                    if let Some(skill_obj) = skill.as_object_mut() {
                        if let Some(playlist) = skill_obj.get("Playlist").and_then(|v| v.as_i64()) {
                            let playlist = playlist as i32;
                            if let Some(&(tier, mu)) = spoofs.get(&playlist) {
                                skill_obj.insert("Tier".to_string(), serde_json::json!(tier));
                                skill_obj.insert("Division".to_string(), serde_json::json!(0));
                                skill_obj.insert("MMR".to_string(), serde_json::json!(mu));
                                skill_obj.insert("Mu".to_string(), serde_json::json!(mu));
                                modified = true;
                            }
                        }
                    }
                }
            }
        }

        if !modified {
            return false;
        }

        let new_json = match serde_json::to_string(&val) {
            Ok(s) => s,
            Err(_) => return false,
        };

        // Re-sign a changed PsyNet RPC result so Rocket League accepts it.
        let psy_time = envelope
            .and_then(|(head, _, line_sep)| {
                head.split(line_sep).find_map(|line| {
                    line.split_once(':').and_then(|(name, value)| {
                        name.eq_ignore_ascii_case("PsyTime").then_some(value.trim())
                    })
                })
            })
            .or_else(|| {
                body.response_headers.iter().find_map(|(name, value)| {
                    name.eq_ignore_ascii_case("PsyTime")
                        .then_some(value.as_str())
                })
            })
            .unwrap_or("");
        let sig = psy_response_signature(psy_time, new_json.as_bytes());
        if let Some((head, _, line_sep)) = envelope {
            let mut lines = head
                .split(line_sep)
                .filter(|line| !line.to_ascii_lowercase().starts_with("psysig:"))
                .map(str::to_string)
                .collect::<Vec<_>>();
            lines.push(format!("PsySig: {sig}"));
            body.bytes = format!(
                "{}{}{}{}",
                lines.join(line_sep),
                line_sep,
                line_sep,
                new_json
            )
            .into_bytes();
        } else {
            body.bytes = new_json.into_bytes();
            body.set_headers.push(("PsySig".to_string(), sig));
        }

        true
    }

    fn announce(&self) -> Option<String> {
        (!self.announced.swap(true, Ordering::Relaxed)).then_some("Ranks Spoofed".to_string())
    }
}

pub(crate) fn psy_response_signature(psy_time: &str, body: &[u8]) -> String {
    use base64::Engine;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    const KEY: &[u8] = b"3b932153785842ac927744b292e40e52";
    let mut mac = <Hmac<Sha256>>::new_from_slice(KEY).expect("HMAC accepts this key");
    mac.update(psy_time.as_bytes());
    mac.update(b"-");
    mac.update(body);
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_rule_rewrites_direct_psynet_response_and_signs_it() {
        let spoofs = Arc::new(Mutex::new(HashMap::from([(10, (22, 95.0))])));
        let rule = RankRule::new(spoofs);
        let mut body = Body::new(
            "application/json",
            br#"{"Result":{"Skills":[{"Playlist":10,"Tier":1,"Division":2,"MMR":15.0,"Mu":15.0}]}}"#.to_vec(),
        );
        body.response_headers
            .push(("PsyTime".into(), "123456".into()));
        assert!(rule.rewrite(&mut body));
        let value: serde_json::Value = serde_json::from_slice(&body.bytes).unwrap();
        let skill = &value["Result"]["Skills"][0];
        assert_eq!(skill["Tier"], 22);
        assert_eq!(skill["Division"], 0);
        assert_eq!(skill["Mu"], 95.0);
        assert!(
            body.set_headers
                .iter()
                .any(|(name, value)| name == "PsySig" && !value.is_empty())
        );
    }

    #[test]
    fn rank_rule_routes_percon_to_local_bridge() {
        let rule = RankRule::new(Arc::new(Mutex::new(HashMap::from([(10, (22, 95.0))]))));
        let original = br#"{"Result":{"PerConURL":"wss://ws.rlpp.psynet.gg/ws/gc","PerConURLv2":"wss://ws.rlpp.psynet.gg/ws/gc2"}}"#.to_vec();
        let mut body = Body::new("application/json", original);
        assert!(rule.rewrite(&mut body));
        let value: serde_json::Value = serde_json::from_slice(&body.bytes).unwrap();
        assert_eq!(
            value["Result"]["PerConURL"],
            "ws://127.0.0.1:8025/ws/gc?PsyConnectionType=Player"
        );
        assert_eq!(value["Result"]["PerConURLv2"], "ws://127.0.0.1:8025/ws/gc2");
    }

    #[test]
    fn rank_rule_routes_psynet_api_through_config() {
        let rule = RankRule::new(Arc::new(Mutex::new(HashMap::from([(10, (22, 95.0))]))));
        let mut body = Body::new(
            "application/json",
            br#"{"PsyNetUrl":"https://api.rlpp.psynet.gg/rpc"}"#.to_vec(),
        );
        assert!(rule.rewrite(&mut body));
        let value: serde_json::Value = serde_json::from_slice(&body.bytes).unwrap();
        assert_eq!(value["PsyNetUrl"], "https://config.psynet.gg/rpc");
        assert!(
            body.set_headers
                .iter()
                .any(|(name, _)| name == "Psysignature")
        );
    }

    #[test]
    fn item_spawner_routes_websocket_without_rank_spoofs() {
        let enabled = Arc::new(AtomicBool::new(true));
        let rule =
            RankRule::with_item_spawner(Arc::new(Mutex::new(HashMap::new())), Arc::clone(&enabled));
        let mut body = Body::new(
            "application/json",
            br#"{"Result":{"PerConURLv2":"wss://ws.rlpp.psynet.gg/ws/gc2"}}"#.to_vec(),
        );
        assert!(rule.rewrite(&mut body));
        let value: serde_json::Value = serde_json::from_slice(&body.bytes).unwrap();
        assert_eq!(value["Result"]["PerConURLv2"], "ws://127.0.0.1:8025/ws/gc2");
        enabled.store(false, Ordering::Relaxed);
        let mut original = Body::new(
            "application/json",
            br#"{"Result":{"PerConURLv2":"wss://ws.rlpp.psynet.gg/ws/gc2"}}"#.to_vec(),
        );
        assert!(!rule.rewrite(&mut original));
    }

    #[test]
    fn rank_rule_keeps_psynet_api_url() {
        let rule = RankRule::new(Arc::new(Mutex::new(HashMap::new())));
        let original = br#"{"PsyNetUrl":"https://api.rlpp.psynet.gg/rpc"}"#.to_vec();
        let mut body = Body::new("application/json", original.clone());
        assert!(!rule.rewrite(&mut body));
        assert_eq!(body.bytes, original);
        assert!(body.set_headers.is_empty());
    }
    #[test]
    fn ranked_heatseeker_uses_the_live_skills_playlist() {
        let rule = RankRule::new(Arc::new(Mutex::new(HashMap::from([(63, (22, 95.0))]))));
        let mut body = Body::new(
            "application/json",
            br#"{"Result":{"Skills":[{"Playlist":63,"Tier":1,"Division":2,"MMR":15.0,"Mu":15.0}]}}"#.to_vec(),
        );
        body.response_headers
            .push(("PsyTime".into(), "123456".into()));
        assert!(rule.rewrite(&mut body));
        let value: serde_json::Value = serde_json::from_slice(&body.bytes).unwrap();
        assert_eq!(value["Result"]["Skills"][0]["Tier"], 22);
    }

    #[test]
    fn title_rule_targets_one_title_and_adds_custom_palette() {
        let settings = Arc::new(Mutex::new(TitleSettings {
            enabled: true,
            titles: vec![TitleSpoofSettings {
                text: "Hebnix".into(),
                color: "12ABEF".into(),
                glow: true,
                target_id: Some("Second".into()),
            }],
        }));
        let rule = TitleRule::new(settings);
        let mut body = Body::new(
            "application/json",
            br#"{"PsyNetUrl":"https://api.rlpp.psynet.gg/rpc","PlayerTitleConfig":{"Titles":[{"ID":"First","Text":"One"},{"ID":"Second","Text":"Two"}],"Categories":[]}}"#.to_vec(),
        );
        assert!(rule.rewrite(&mut body));
        let value: serde_json::Value = serde_json::from_slice(&body.bytes).unwrap();
        assert_eq!(value["PlayerTitleConfig"]["Titles"][0]["Text"], "One");
        assert_eq!(value["PlayerTitleConfig"]["Titles"][1]["Text"], "Hebnix");
        assert_eq!(value["PsyNetUrl"], "https://api.rlpp.psynet.gg/rpc");
        assert_eq!(
            value["PlayerTitleConfig"]["Categories"][0]["GlowColor"],
            "12ABEF"
        );
    }

    #[test]
    fn title_rule_spoofs_each_configured_title() {
        let settings = Arc::new(Mutex::new(TitleSettings {
            enabled: true,
            titles: vec![
                TitleSpoofSettings {
                    text: "First Spoof".into(),
                    color: "112233".into(),
                    glow: false,
                    target_id: Some("First".into()),
                },
                TitleSpoofSettings {
                    text: "Second Spoof".into(),
                    color: "AABBCC".into(),
                    glow: true,
                    target_id: Some("Second".into()),
                },
            ],
        }));
        let rule = TitleRule::new(settings);
        let mut body = Body::new(
            "application/json",
            br#"{"PlayerTitleConfig":{"Titles":[{"ID":"First","Text":"One"},{"ID":"Second","Text":"Two"}],"Categories":[]}}"#.to_vec(),
        );
        assert!(rule.rewrite(&mut body));
        let value: serde_json::Value = serde_json::from_slice(&body.bytes).unwrap();
        assert_eq!(
            value["PlayerTitleConfig"]["Titles"][0]["Text"],
            "First Spoof"
        );
        assert_eq!(
            value["PlayerTitleConfig"]["Titles"][1]["Text"],
            "Second Spoof"
        );
        assert_ne!(
            value["PlayerTitleConfig"]["Titles"][0]["Category"],
            value["PlayerTitleConfig"]["Titles"][1]["Category"]
        );
    }

    #[test]
    fn dedicated_title_takes_priority_and_all_spoofs_the_rest() {
        let settings = Arc::new(Mutex::new(TitleSettings {
            enabled: true,
            titles: vec![
                TitleSpoofSettings {
                    text: "Fallback".into(),
                    color: "112233".into(),
                    glow: false,
                    target_id: None,
                },
                TitleSpoofSettings {
                    text: "Dedicated".into(),
                    color: "AABBCC".into(),
                    glow: true,
                    target_id: Some("Second".into()),
                },
            ],
        }));
        let rule = TitleRule::new(settings);
        let mut body = Body::new(
            "application/json",
            br#"{"PlayerTitleConfig":{"Titles":[{"ID":"First","Text":"One"},{"ID":"Second","Text":"Two"},{"ID":"Third","Text":"Three"}],"Categories":[]}}"#.to_vec(),
        );
        assert!(rule.rewrite(&mut body));
        let value: serde_json::Value = serde_json::from_slice(&body.bytes).unwrap();
        assert_eq!(value["PlayerTitleConfig"]["Titles"][0]["Text"], "Fallback");
        assert_eq!(value["PlayerTitleConfig"]["Titles"][1]["Text"], "Dedicated");
        assert_eq!(value["PlayerTitleConfig"]["Titles"][2]["Text"], "Fallback");
    }
}

/// Keep normal spoof rules inactive when only the RLAPI workbench is enabled.
pub struct EnabledRule {
    pub inner: Box<dyn Rule>,
    pub http: Arc<AtomicBool>,
    pub socket: Arc<AtomicBool>,
}
impl Rule for EnabledRule {
    fn matches_host(&self, host: &str) -> bool {
        (self.http.load(Ordering::Relaxed) || self.socket.load(Ordering::Relaxed))
            && self.inner.matches_host(host)
    }
    fn strip_request_headers(&self) -> &[&str] {
        self.inner.strip_request_headers()
    }
    fn rewrite(&self, body: &mut Body) -> bool {
        self.inner.rewrite(body)
    }
    fn announce(&self) -> Option<String> {
        self.inner.announce()
    }
}

/// Route the original game authentication and WebSocket, with no data spoofs.
pub struct RlApiRouteRule;
impl Rule for RlApiRouteRule {
    fn matches_host(&self, host: &str) -> bool {
        hebnix_sdk::rlapi::session::shared_game_session().enabled()
            && (host.eq_ignore_ascii_case(TITLE_HOST)
                || host.eq_ignore_ascii_case("api.rlpp.psynet.gg"))
    }
    fn strip_request_headers(&self) -> &[&str] {
        &["if-none-match", "if-modified-since"]
    }
    fn rewrite(&self, body: &mut Body) -> bool {
        if !hebnix_sdk::rlapi::session::shared_game_session().enabled() {
            return false;
        }
        let route = RankRule::with_item_spawner(
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(AtomicBool::new(true)),
        );
        route.rewrite(body)
    }
}
