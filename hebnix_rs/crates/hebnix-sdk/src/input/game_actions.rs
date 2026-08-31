use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

use super::bindings::is_bind_pressed;

struct ActionBindingCache {
    checked_at: Option<Instant>,
    path: Option<PathBuf>,
    modified: Option<SystemTime>,
    bindings: HashMap<String, Vec<String>>,
    // Keyboard-only chat channel binds ("global" | "team" | "party" -> key).
    // Kept separate from `bindings` because the chat channel actions aren't
    // in the known action set normalise_action() recognises, and because
    // callers here specifically want the keyboard key (not a controller
    // button) to feed into tap_key().
    chat_binds: HashMap<&'static str, String>,
}

impl Default for ActionBindingCache {
    fn default() -> Self {
        Self {
            checked_at: None,
            path: None,
            modified: None,
            bindings: HashMap::new(),
            chat_binds: HashMap::new(),
        }
    }
}

// Classifies a raw (un-normalised) action name as a text chat channel.
// Rocket League doesn't expose these under a fixed action id we can rely on
// across versions, so match loosely on the action name instead of an exact
// string. Excludes quick chat presets and voice chat, which also contain
// "chat" but aren't the text-chat-to-channel binds.
fn classify_chat_channel(action: &str) -> Option<&'static str> {
    let lower = action.to_ascii_lowercase();
    if !lower.contains("chat") || lower.contains("preset") || lower.contains("voice") {
        return None;
    }
    if lower.contains("team") {
        Some("team")
    } else if lower.contains("party") {
        Some("party")
    } else {
        Some("global")
    }
}

fn collect_chat_binds(raw: &Value, output: &mut HashMap<&'static str, String>) {
    let Some(bindings) = raw.as_array() else {
        return;
    };
    for binding in bindings {
        let Some(action) = binding.get("Action").and_then(Value::as_str) else {
            continue;
        };
        let Some(channel) = classify_chat_channel(action) else {
            continue;
        };
        let Some(key) = binding.get("Key").and_then(Value::as_str) else {
            continue;
        };
        let Some(bind) = keyboard_key_to_bind(key) else {
            continue;
        };
        output.entry(channel).or_insert(bind);
    }
}

fn cache() -> &'static Mutex<ActionBindingCache> {
    static CACHE: OnceLock<Mutex<ActionBindingCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(ActionBindingCache::default()))
}

fn normalise_action(action: &str) -> String {
    let compact: String = action
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();

    match compact.as_str() {
        "scoreboard" | "togglescoreboard" => "togglescoreboard".into(),
        "airroll" | "roll" | "toggleroll" => "roll".into(),
        "airrollleft" | "rollleft" => "rollleft".into(),
        "airrollright" | "rollright" => "rollright".into(),
        "powerslide" | "handbrake" => "handbrake".into(),
        "ballcam" | "focusonball" | "secondarycamera" => "focusonball".into(),
        "rearview" | "rearcamera" | "lookback" => "rearview".into(),
        "useitem" | "usepickup" => "usepickup".into(),
        "pausemenu" | "togglemidgamemenu" => "togglemidgamemenu".into(),
        "accelerate" | "throttle" | "throttleforward" => "throttle".into(),
        "reverse" | "throttlereverse" => "throttlereverse".into(),
        "quickchatup" | "chatpreset1" => "chatpreset1".into(),
        "quickchatleft" | "chatpreset2" => "chatpreset2".into(),
        "quickchatright" | "chatpreset3" => "chatpreset3".into(),
        "quickchatdown" | "chatpreset4" => "chatpreset4".into(),
        _ => compact,
    }
}

fn controller_key_to_bind(key: &str) -> Option<String> {
    let key = key.strip_prefix("XboxTypeS_").unwrap_or(key);
    let compact = key.to_ascii_lowercase().replace([' ', '-', '_'], "");

    let bind = match compact.as_str() {
        "a" => "controller_a",
        "b" => "controller_b",
        "x" => "controller_x",
        "y" => "controller_y",
        "leftshoulder" | "leftbumper" => "controller_lb",
        "rightshoulder" | "rightbumper" => "controller_rb",
        "leftthumbstick" | "leftstickclick" => "controller_ls",
        "rightthumbstick" | "rightstickclick" => "controller_rs",
        "lefttrigger" | "lefttriggeraxis" => "controller_lt",
        "righttrigger" | "righttriggeraxis" => "controller_rt",
        "start" | "menu" => "controller_start",
        "back" | "select" | "view" => "controller_select",
        "dpadup" => "controller_dpad_up",
        "dpaddown" => "controller_dpad_down",
        "dpadleft" => "controller_dpad_left",
        "dpadright" => "controller_dpad_right",
        "none" | "" => return None,
        _ => return None,
    };
    Some(bind.into())
}

fn keyboard_key_to_bind(key: &str) -> Option<String> {
    let compact = key.trim().to_ascii_lowercase().replace([' ', '-', '_'], "");

    let bind = match compact.as_str() {
        "" | "none" => return None,
        "spacebar" => "space",
        "leftshift" => "left shift",
        "rightshift" => "right shift",
        "leftcontrol" | "leftctrl" => "left ctrl",
        "rightcontrol" | "rightctrl" => "right ctrl",
        "leftalt" => "left alt",
        "rightalt" => "right alt",
        "leftmousebutton" => "mouse_left",
        "rightmousebutton" => "mouse_right",
        "middlemousebutton" => "mouse_middle",
        "thumbmousebutton" => "mouse_x1",
        "thumbmousebutton2" => "mouse_x2",
        "return" => "enter",
        "escape" => "escape",
        _ => key.trim(),
    };
    Some(bind.into())
}

fn collect_bindings(raw: &Value, controller: bool, output: &mut HashMap<String, Vec<String>>) {
    let Some(bindings) = raw.as_array() else {
        return;
    };

    for binding in bindings {
        let Some(action) = binding.get("Action").and_then(Value::as_str) else {
            continue;
        };
        let Some(key) = binding.get("Key").and_then(Value::as_str) else {
            continue;
        };
        let bind = if controller {
            controller_key_to_bind(key)
        } else {
            keyboard_key_to_bind(key)
        };
        let Some(bind) = bind else {
            continue;
        };
        let entry = output.entry(normalise_action(action)).or_default();
        if !entry.contains(&bind) {
            entry.push(bind);
        }
    }
}

fn current_save_path() -> Option<PathBuf> {
    let accounts = crate::save_file::find_save_accounts(None);
    accounts
        .into_iter()
        .filter_map(|account| {
            let modified = std::fs::metadata(&account.path)
                .and_then(|metadata| metadata.modified())
                .ok()?;
            Some((modified, account.path))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
        .or_else(|| crate::save_file::find_save_file(None))
}

fn refresh_cache(cache: &mut ActionBindingCache) {
    if cache
        .checked_at
        .is_some_and(|checked| checked.elapsed() < Duration::from_secs(2))
    {
        return;
    }
    cache.checked_at = Some(Instant::now());

    let path = current_save_path();
    let modified = path
        .as_ref()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|metadata| metadata.modified().ok());

    if path == cache.path && modified == cache.modified {
        return;
    }

    let mut bindings = HashMap::new();
    let mut chat_binds = HashMap::new();
    if let Some(path) = path.as_ref()
        && let Ok(save) = crate::save_file::load(path, false)
    {
        if let Some(controls) = save.controls() {
            collect_bindings(&controls.raw_bindings, false, &mut bindings);
            collect_chat_binds(&controls.raw_bindings, &mut chat_binds);
        }
        if let Some(gamepad) = save.gamepad_bindings() {
            collect_bindings(&gamepad.raw_bindings, true, &mut bindings);
        }
    }

    cache.path = path;
    cache.modified = modified;
    cache.bindings = bindings;
    cache.chat_binds = chat_binds;
}

/// Keyboard key bound to a text chat channel ("global", "team" or "party"),
/// read from the user's actual save file. Falls back to Rocket League's
/// stock defaults (T / Y / U) if the save couldn't be read or doesn't have
/// that channel bound. Controller bindings are never returned here — chat.send
/// only ever taps a keyboard key.
pub fn chat_channel_bind(channel: &str) -> String {
    let default = match channel {
        "team" => "y",
        "party" => "u",
        _ => "t",
    };
    let Ok(mut cache) = cache().lock() else {
        return default.into();
    };
    refresh_cache(&mut cache);
    cache
        .chat_binds
        .get(channel)
        .cloned()
        .unwrap_or_else(|| default.into())
}

pub fn action_binds(action: &str) -> Vec<String> {
    let Ok(mut cache) = cache().lock() else {
        return Vec::new();
    };
    refresh_cache(&mut cache);
    cache
        .bindings
        .get(&normalise_action(action))
        .cloned()
        .unwrap_or_default()
}

pub fn is_action_pressed(action: &str) -> bool {
    action_binds(action)
        .iter()
        .any(|binding| is_bind_pressed(binding))
}

pub fn clear_action_bind_cache() {
    if let Ok(mut cache) = cache().lock() {
        *cache = ActionBindingCache::default();
    }
}
