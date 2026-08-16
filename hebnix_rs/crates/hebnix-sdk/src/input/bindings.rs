//! hotkey binds: check if held + capture new ones (kb or controller)

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::input::dualsense::{
    DS4_BUTTON_DISPLAY, DS4_BUTTONS, get_dualsense_inputs, start_dualsense_monitor,
};
use crate::input::keyboard::is_key_pressed;
use crate::input::xinput::{XINPUT_BUTTON_DISPLAY, get_xinput_state};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ControllerType {
    XInput,
    DualSense,
    Keyboard,
}

/// a hotkey bind, either a kb key or a controller button
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyBind {
    pub is_controller: bool,
    /// kb key name (e.g. "tab") when not a controller
    #[serde(default)]
    pub hotkey: String,
    /// "xinput" or "dualsense"
    #[serde(default)]
    pub controller_type: String,
    /// xinput mask or dualsense button index
    #[serde(default)]
    pub controller_button: u32,
}

// xinput name -> mask (canonical names + "controller_" aliases)

const XI_NAMES: [(&str, u16); 16] = [
    ("a", 0x1000),
    ("b", 0x2000),
    ("x", 0x4000),
    ("y", 0x8000),
    ("lb", 0x0100),
    ("rb", 0x0200),
    ("ls", 0x0040),
    ("rs", 0x0080),
    ("start", 0x0010),
    ("select", 0x0020),
    ("back", 0x0020),
    ("dpad_up", 0x0001),
    ("dpad_down", 0x0002),
    ("dpad_left", 0x0004),
    ("dpad_right", 0x0008),
    ("lstick", 0x0040),
];

const DS_NAMES: [(&str, u32); 19] = [
    ("cross", 0),
    ("circle", 1),
    ("square", 2),
    ("triangle", 3),
    ("l1", 4),
    ("r1", 5),
    ("l2", 6),
    ("r2", 7),
    ("l3", 8),
    ("r3", 9),
    ("options", 10),
    ("create", 11),
    ("ps", 12),
    ("touchpad", 13),
    ("mute", 14),
    ("d_up", 15),
    ("d_down", 16),
    ("d_left", 17),
    ("d_right", 18),
];

fn normalise(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .replace([' ', '-'], "_")
        .replace("__", "_")
}

fn strip_controller_prefix(s: &str) -> &str {
    s.strip_prefix("controller_").unwrap_or(s)
}

fn xi_lookup(name: &str) -> Option<u16> {
    let n = strip_controller_prefix(name);
    // also accept compact spellings from display names ("d_pad_up" / "dpadup")
    let compact = n.replace('_', "");
    XI_NAMES
        .iter()
        .find(|(k, _)| *k == n || k.replace('_', "") == compact)
        .map(|(_, m)| *m)
        .or_else(|| {
            XINPUT_BUTTON_DISPLAY
                .iter()
                .find(|(_, label)| normalise(label).replace('_', "") == compact)
                .map(|(m, _)| *m)
        })
}

fn ds_lookup(name: &str) -> Option<u32> {
    let n = strip_controller_prefix(name);
    let with_dpad_alias = n.replace("dpad_", "d_");
    DS_NAMES
        .iter()
        .find(|(k, _)| *k == n || *k == with_dpad_alias)
        .map(|(_, idx)| *idx)
}

/// turn a raw button id into something readable
pub fn get_button_display(controller_type: &str, raw_button: u32) -> String {
    if controller_type == "dualsense" {
        let name = DS4_BUTTONS.get(raw_button as usize).copied().unwrap_or("?");
        DS4_BUTTON_DISPLAY
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.to_string())
            .unwrap_or_else(|| name.to_string())
    } else {
        XINPUT_BUTTON_DISPLAY
            .iter()
            .find(|(m, _)| *m as u32 == raw_button)
            .map(|(_, v)| v.to_string())
            .unwrap_or_else(|| format!("Btn 0x{raw_button:04X}"))
    }
}

/// is this bind currently held (kb/xinput/dualsense)
pub fn is_hotkey_pressed(bind: &HotkeyBind) -> bool {
    if bind.is_controller {
        if bind.controller_type == "dualsense" {
            let held = get_dualsense_inputs();
            DS4_BUTTONS
                .get(bind.controller_button as usize)
                .map(|name| held.iter().any(|h| h == name))
                .unwrap_or(false)
        } else {
            (0..4).any(|i| {
                get_xinput_state(i)
                    .map(|state| state.is_pressed(bind.controller_button as u16))
                    .unwrap_or(false)
            })
        }
    } else {
        is_key_pressed(&bind.hotkey)
    }
}

/// parse a bind string ("tab", "controller_a", "cross") into a HotkeyBind.
/// None if empty.
pub fn resolve_bind_string(bind_str: &str) -> Option<HotkeyBind> {
    if bind_str.trim().is_empty() {
        return None;
    }
    let n = normalise(bind_str);

    if let Some(mask) = xi_lookup(&n) {
        return Some(HotkeyBind {
            is_controller: true,
            hotkey: String::new(),
            controller_type: "xinput".to_string(),
            controller_button: mask as u32,
        });
    }
    if let Some(idx) = ds_lookup(&n) {
        return Some(HotkeyBind {
            is_controller: true,
            hotkey: String::new(),
            controller_type: "dualsense".to_string(),
            controller_button: idx,
        });
    }
    Some(HotkeyBind {
        is_controller: false,
        hotkey: bind_str.trim().to_string(),
        controller_type: String::new(),
        controller_button: 0,
    })
}

/// inverse of resolve_bind_string: bind back to its canonical string
pub fn bind_to_string(bind: &HotkeyBind) -> String {
    if !bind.is_controller {
        return bind.hotkey.clone();
    }
    if bind.controller_type == "dualsense" {
        DS_NAMES
            .iter()
            .find(|(_, idx)| *idx == bind.controller_button)
            .map(|(name, _)| name.to_string())
            .unwrap_or_else(|| format!("ds_{}", bind.controller_button))
    } else {
        XI_NAMES
            .iter()
            .find(|(_, mask)| *mask as u32 == bind.controller_button)
            .map(|(name, _)| format!("controller_{name}"))
            .unwrap_or_else(|| format!("controller_0x{:04x}", bind.controller_button))
    }
}

/// is this named bind currently held
pub fn is_bind_pressed(bind_str: &str) -> bool {
    resolve_bind_string(bind_str)
        .map(|b| is_hotkey_pressed(&b))
        .unwrap_or(false)
}

/// block until any kb key / xinput / dualsense button press. None on timeout.
///
/// short settle phase ignores inputs already held when capture started (the
/// click/enter that triggered it), then the first new press wins, including
/// keys still held after the settle phase.
pub fn detect_any_hotkey(timeout: Option<Duration>) -> Option<HotkeyBind> {
    let start = Instant::now();
    let mut initial_ds: std::collections::HashSet<String> =
        get_dualsense_inputs().into_iter().collect();

    // give em a beat to release whatever triggered capture; still held after
    // this counts as the chosen bind
    let settle_until = Instant::now() + Duration::from_millis(250);
    let initial_kb = crate::input::keyboard::scan_pressed_key();

    loop {
        if let Some(t) = timeout {
            if start.elapsed() > t {
                return None;
            }
        }
        let settling = Instant::now() < settle_until;

        // keyboard (non-blocking scan, ignores the initially-held key during settle)
        if let Some(name) = crate::input::keyboard::scan_pressed_key() {
            if !settling || initial_kb.as_deref() != Some(name.as_str()) {
                return Some(HotkeyBind {
                    is_controller: false,
                    hotkey: name,
                    controller_type: String::new(),
                    controller_button: 0,
                });
            }
        }

        // XInput
        for i in 0..4 {
            if let Some(state) = get_xinput_state(i) {
                if state.buttons != 0 {
                    for (mask, _label) in XINPUT_BUTTON_DISPLAY {
                        if state.is_pressed(mask) {
                            return Some(HotkeyBind {
                                is_controller: true,
                                hotkey: String::new(),
                                controller_type: "xinput".to_string(),
                                controller_button: mask as u32,
                            });
                        }
                    }
                }
            }
        }

        // DualSense
        let current_ds: std::collections::HashSet<String> =
            get_dualsense_inputs().into_iter().collect();
        let new_ds: Vec<&String> = current_ds.difference(&initial_ds).collect();
        if let Some(btn_name) = new_ds.first() {
            let idx = DS4_BUTTONS.iter().position(|b| b == btn_name).unwrap_or(0) as u32;
            return Some(HotkeyBind {
                is_controller: true,
                hotkey: String::new(),
                controller_type: "dualsense".to_string(),
                controller_button: idx,
            });
        }
        initial_ds = current_ds;

        std::thread::sleep(Duration::from_millis(20));
    }
}

/// init controller backends, call once at startup. starts the dualsense
/// monitor thread, xinput needs no init.
pub fn init_controllers() {
    start_dualsense_monitor();
}
