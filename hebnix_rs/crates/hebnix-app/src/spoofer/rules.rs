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
    /// Optionally forward a matched request to a different upstream host.
    fn upstream_host(&self, _host: &str, _path: &str) -> Option<&'static str> {
        None
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
            let name = self.name.lock().map(|name| name.clone()).unwrap_or_default();
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

#[derive(Clone)]
pub struct TitleSettings {
    pub enabled: bool,
    pub text: String,
    pub color: String,
    pub glow: bool,
    pub target_id: Option<String>,
}

impl Default for TitleSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            text: String::new(),
            color: "E8E8E8".to_string(),
            glow: false,
            target_id: None,
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
        if !settings.enabled || settings.text.trim().is_empty() {
            return false;
        }

        let Some(titles) = val
            .get_mut("PlayerTitleConfig")
            .and_then(|c| c.get_mut("Titles"))
            .and_then(|t| t.as_array_mut())
        else {
            return false;
        };

        let mut count = 0;
        for title in titles.iter_mut() {
            if let Some(obj) = title.as_object_mut() {
                let matches_target = settings.target_id.as_ref().is_none_or(|target| {
                    obj.get("ID").and_then(serde_json::Value::as_str) == Some(target)
                });
                if matches_target && obj.contains_key("Text") {
                    obj.insert(
                        "Text".to_string(),
                        serde_json::Value::String(settings.text.trim().to_string()),
                    );
                    obj.insert(
                        "Category".to_string(),
                        serde_json::Value::String("Hebnix_Custom".to_string()),
                    );
                    count += 1;
                }
            }
        }
        if count == 0 {
            return false;
        }

        if let Some(categories) = val
            .get_mut("PlayerTitleConfig")
            .and_then(|config| config.get_mut("Categories"))
            .and_then(serde_json::Value::as_array_mut)
        {
            let category = categories.iter_mut().find(|category| {
                category.get("ID").and_then(serde_json::Value::as_str) == Some("Hebnix_Custom")
            });
            let mut replacement = serde_json::json!({
                "ID": "Hebnix_Custom",
                "Color": settings.color,
            });
            if settings.glow {
                replacement["GlowColor"] = replacement["Color"].clone();
            }
            if let Some(category) = category {
                *category = replacement;
            } else {
                categories.insert(0, replacement);
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
            let title = self.settings.lock().map(|settings| settings.text.clone()).unwrap_or_default();
            format!("Title Spoofed to {title}")
        })
    }
}

pub struct RankRule {
    pub spoofs: Arc<Mutex<HashMap<i32, (i32, f64)>>>,
    announced: AtomicBool,
}

impl RankRule {
    pub fn new(spoofs: Arc<Mutex<HashMap<i32, (i32, f64)>>>) -> Self {
        Self {
            spoofs,
            announced: AtomicBool::new(false),
        }
    }
}

impl Rule for RankRule {
    fn matches_host(&self, host: &str) -> bool {
        // Only the HTTP PsyNet RPC response contains PerConURL.  Never MITM
        // ws.rlpp.psynet.gg: that is a long-lived websocket and must be
        // tunnelled until the PerCon URL points it at the local bridge.
        self.spoofs.lock().map(|spoofs| !spoofs.is_empty()).unwrap_or(false)
            && (host.eq_ignore_ascii_case("api.rlpp.psynet.gg")
                || host.eq_ignore_ascii_case("config.psynet.gg"))
    }

    fn strip_request_headers(&self) -> &[&str] {
        &["if-none-match", "if-modified-since"]
    }

    fn upstream_host(&self, host: &str, path: &str) -> Option<&'static str> {
        (host.eq_ignore_ascii_case("config.psynet.gg")
            && (path.contains("/rpc/") || path.contains("/Services")))
            .then_some("api.rlpp.psynet.gg")
    }

    fn rewrite(&self, body: &mut Body) -> bool {
        let body_str = match std::str::from_utf8(&body.bytes) {
            Ok(s) => s,
            Err(_) => return false,
        };

        // C# first rewrites the auth/config payload so its PsyNet RPC points
        // at config.psynet.gg. The next config request is then funnelled to
        // api.rlpp.psynet.gg by `upstream_host` above.
        if body_str.contains("api.rlpp.psynet.gg") {
            let rewritten = body_str
                .replace("https:\\/\\/api.rlpp.psynet.gg\\/rpc", "https:\\/\\/config.psynet.gg\\/rpc")
                .replace("https:\\/\\/api.rlpp.psynet.gg\\/Services", "https:\\/\\/config.psynet.gg\\/Services")
                .replace("https://api.rlpp.psynet.gg/rpc", "https://config.psynet.gg/rpc")
                .replace("https://api.rlpp.psynet.gg/Services", "https://config.psynet.gg/Services");
            if rewritten != body_str {
                let bytes = rewritten.into_bytes();
                body.set_headers.push(("Psysignature".into(), config_psysignature(&bytes)));
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
        let spoofs = self.spoofs.lock().unwrap().clone();

        fn rewrite_connection_urls(value: &mut serde_json::Value) -> bool {
            let mut modified = false;
            match value {
                serde_json::Value::Object(object) => {
                    for (key, value) in object {
                        let replacement = match key.as_str() {
                            "PerConURL" => {
                                Some("ws://127.0.0.1:8025/ws/gc?PsyConnectionType=Player")
                            }
                            "PerConURLv2" => Some("ws://127.0.0.1:8025/ws/gc2"),
                            _ => None,
                        };
                        if let Some(replacement) = replacement {
                            if value.as_str() != Some(replacement) {
                                *value = serde_json::Value::String(replacement.to_string());
                                modified = true;
                            }
                        } else {
                            modified |= rewrite_connection_urls(value);
                        }
                    }
                }
                serde_json::Value::Array(array) => {
                    for value in array {
                        modified |= rewrite_connection_urls(value);
                    }
                }
                _ => {}
            }
            modified
        }

        let connection_urls_modified = rewrite_connection_urls(&mut val);
        modified |= connection_urls_modified;

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

        // The C# relay signs every forwarded PsyNet RPC result, including a
        // PerConURL-only rewrite. Otherwise Rocket League rejects the changed
        // response because the original PsySig no longer matches its body.
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

fn config_psysignature(body: &[u8]) -> String {
    use base64::Engine;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = <Hmac<Sha256>>::new_from_slice(b"cqhyz50f3c3j2pxhwo6b1kypxikah0wh")
        .expect("HMAC accepts this key");
    mac.update(body);
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

fn psy_response_signature(psy_time: &str, body: &[u8]) -> String {
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
    fn rank_rule_routes_live_skill_connection_through_bridge() {
        let rule = RankRule::new(Arc::new(Mutex::new(HashMap::from([(10, (22, 95.0))]))));
        let mut body = Body::new(
            "application/json",
            br#"{"Result":{"PerConURL":"wss://ws.rlpp.psynet.gg/ws/gc","PerConURLv2":"wss://ws.rlpp.psynet.gg/ws/gc2"}}"#.to_vec(),
        );
        assert!(rule.rewrite(&mut body));
        let value: serde_json::Value = serde_json::from_slice(&body.bytes).unwrap();
        assert_eq!(
            value["Result"]["PerConURL"],
            "ws://127.0.0.1:8025/ws/gc?PsyConnectionType=Player"
        );
        assert_eq!(value["Result"]["PerConURLv2"], "ws://127.0.0.1:8025/ws/gc2");
        assert!(body
            .set_headers
            .iter()
            .any(|(name, value)| name == "PsySig" && !value.is_empty()));
    }

    #[test]
    fn rank_rule_routes_config_funnel_and_signs_config_payload() {
        let rule = RankRule::new(Arc::new(Mutex::new(HashMap::from([(10, (22, 95.0))]))));
        assert_eq!(
            rule.upstream_host("config.psynet.gg", "/rpc/Player/GetPlayerSkills"),
            Some("api.rlpp.psynet.gg")
        );
        let mut body = Body::new(
            "application/json",
            br#"{"PsyNetUrl":"https://api.rlpp.psynet.gg/rpc"}"#.to_vec(),
        );
        assert!(rule.rewrite(&mut body));
        assert!(String::from_utf8_lossy(&body.bytes).contains("config.psynet.gg/rpc"));
        assert!(body.set_headers.iter().any(|(name, _)| name == "Psysignature"));
    }

    #[test]
    fn ranked_heatseeker_uses_the_live_skills_playlist() {
        let rule = RankRule::new(Arc::new(Mutex::new(HashMap::from([(63, (22, 95.0))]))));
        let mut body = Body::new(
            "application/json",
            br#"{"Result":{"Skills":[{"Playlist":63,"Tier":1,"Division":2,"MMR":15.0,"Mu":15.0}]}}"#.to_vec(),
        );
        body.response_headers.push(("PsyTime".into(), "123456".into()));
        assert!(rule.rewrite(&mut body));
        let value: serde_json::Value = serde_json::from_slice(&body.bytes).unwrap();
        assert_eq!(value["Result"]["Skills"][0]["Tier"], 22);
    }

    #[test]
    fn title_rule_targets_one_title_and_adds_custom_palette() {
        let settings = Arc::new(Mutex::new(TitleSettings {
            enabled: true,
            text: "Hebnix".into(),
            color: "12ABEF".into(),
            glow: true,
            target_id: Some("Second".into()),
        }));
        let rule = TitleRule::new(settings);
        let mut body = Body::new(
            "application/json",
            br#"{"PlayerTitleConfig":{"Titles":[{"ID":"First","Text":"One"},{"ID":"Second","Text":"Two"}],"Categories":[]}}"#.to_vec(),
        );
        assert!(rule.rewrite(&mut body));
        let value: serde_json::Value = serde_json::from_slice(&body.bytes).unwrap();
        assert_eq!(value["PlayerTitleConfig"]["Titles"][0]["Text"], "One");
        assert_eq!(value["PlayerTitleConfig"]["Titles"][1]["Text"], "Hebnix");
        assert_eq!(
            value["PlayerTitleConfig"]["Categories"][0]["GlowColor"],
            "12ABEF"
        );
    }
}
