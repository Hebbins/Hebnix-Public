//! hotkey binds: check if held + capture new ones (kb or controller)

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::input::dinput::{
    DINPUT_BUTTONS, button_display, get_dinput_inputs, get_dinput_raw_inputs,
    is_dinput_raw_pressed, start_dinput_monitor,
};
use crate::input::keyboard::is_key_pressed;
use crate::input::xinput::{XINPUT_BUTTON_DISPLAY, get_xinput_state};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ControllerType {
    XInput,
    /// any non-xinput (dinput) pad: ps4/ps5, 8bitdo, switch-style, etc.
    DInput,
    Keyboard,
}

/// a hotkey bind, either a kb key or a controller button
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyBind {
    pub is_controller: bool,
    /// kb key name (e.g. "tab") when not a controller
    #[serde(default)]
    pub hotkey: String,
    /// "xinput" or "dinput"
    #[serde(default)]
    pub controller_type: String,
    /// xinput mask or dinput button index (index into DINPUT_BUTTONS)
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

// xinput mask <-> dinput canonical name, for cross-backend fallback. Rocket
// League always records its own control bindings as xinput-shaped names
// ("A", "LeftShoulder", ...) regardless of which physical controller you're
// actually using - so a bind captured from RL's own settings (or from a
// real xinput device) needs to also register on a dinput-only pad (generic
// or playstation) with no real xinput presence, and vice versa. Sticks-click
// and dpad line up directly; LT/RT have no xinput digital-button equivalent
// (analog-only in XINPUT_GAMEPAD) so they're left out here on purpose.
const XI_DIN_BRIDGE: [(u16, &str); 14] = [
    (0x1000, "south"),
    (0x2000, "east"),
    (0x4000, "west"),
    (0x8000, "north"),
    (0x0100, "l1"),
    (0x0200, "r1"),
    (0x0040, "l3"),
    (0x0080, "r3"),
    (0x0010, "start"),
    (0x0020, "select"),
    (0x0001, "dpad_up"),
    (0x0002, "dpad_down"),
    (0x0004, "dpad_left"),
    (0x0008, "dpad_right"),
];

/// dinput canonical button name equivalent to an xinput mask, if any
pub fn xinput_mask_to_dinput_name(mask: u16) -> Option<&'static str> {
    XI_DIN_BRIDGE.iter().find(|(m, _)| *m == mask).map(|(_, n)| *n)
}

/// xinput mask equivalent to a dinput canonical button name, if any
pub fn dinput_name_to_xinput_mask(name: &str) -> Option<u16> {
    XI_DIN_BRIDGE.iter().find(|(_, n)| *n == name).map(|(m, _)| *m)
}

// dinput canonical name -> index into DINPUT_BUTTONS, plus brand-flavoured
// aliases (playstation/nintendo labels) so binds can be typed either way
const DIN_NAMES: [(&str, u32); 33] = [
    ("south", 0),
    ("cross", 0),
    ("b", 0), // nintendo-layout alias
    ("east", 1),
    ("circle", 1),
    ("a", 1), // nintendo-layout alias
    ("north", 2),
    ("triangle", 2),
    ("x", 2), // nintendo-layout alias
    ("west", 3),
    ("square", 3),
    ("y", 3), // nintendo-layout alias
    ("l1", 4),
    ("lb", 4),
    ("r1", 5),
    ("rb", 5),
    ("l2", 6),
    ("lt", 6),
    ("zl", 6),
    ("r2", 7),
    ("rt", 7),
    ("zr", 7),
    ("l3", 8),
    ("r3", 9),
    ("select", 10),
    ("back", 10),
    ("share", 10),
    ("create", 10),
    ("minus", 10),
    ("start", 11),
    ("options", 11),
    ("plus", 11),
    ("mode", 12),
];

// legacy ds4 button order (cross, circle, square, triangle, l1, r1, l2, r2,
// l3, r3, options, create, ps, touchpad, mute, up, down, left, right) used by
// the old dualsense-only binder, mapped to the new DINPUT_BUTTONS index so
// hotkeys.json binds saved before dinput support don't silently break.
// touchpad/mute have no direct gilrs equivalent and fall back to "mode".
const LEGACY_DS4_ORDER: [u32; 19] = [
    0, 1, 3, 2, 4, 5, 6, 7, 8, 9, 11, 10, 12, 12, 12, 13, 14, 15, 16,
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

fn din_lookup(name: &str) -> Option<u32> {
    let n = strip_controller_prefix(name);
    let with_dpad_alias = n.replace("dpad_", "d_");
    DIN_NAMES
        .iter()
        .find(|(k, _)| *k == n || *k == with_dpad_alias)
        .map(|(_, idx)| *idx)
}

// old dualsense-only binds are stored with controller_type "dualsense" and
// an index into the old DS4_BUTTONS order; translate that to the new
// DINPUT_BUTTONS index so saved hotkeys keep working.
fn legacy_ds4_to_dinput(raw_button: u32) -> u32 {
    LEGACY_DS4_ORDER
        .get(raw_button as usize)
        .copied()
        .unwrap_or(0)
}

/// turn a raw button id into something readable
pub fn get_button_display(controller_type: &str, raw_button: u32) -> String {
    if controller_type == "dinput_raw" {
        // paddles/misc buttons gilrs can't name semantically - see dinput.rs
        format!("Extra 0x{raw_button:X}")
    } else if controller_type == "dinput" {
        DINPUT_BUTTONS
            .get(raw_button as usize)
            .map(|name| button_display(name))
            .unwrap_or_else(|| format!("Btn {raw_button}"))
    } else if controller_type == "dualsense" {
        DINPUT_BUTTONS
            .get(legacy_ds4_to_dinput(raw_button) as usize)
            .map(|name| button_display(name))
            .unwrap_or_else(|| format!("Btn {raw_button}"))
    } else {
        XINPUT_BUTTON_DISPLAY
            .iter()
            .find(|(m, _)| *m as u32 == raw_button)
            .map(|(_, v)| v.to_string())
            .unwrap_or_else(|| format!("Btn 0x{raw_button:04X}"))
    }
}

/// is this bind currently held (kb/xinput/dinput)
pub fn is_hotkey_pressed(bind: &HotkeyBind) -> bool {
    if bind.is_controller {
        if bind.controller_type == "dinput_raw" {
            is_dinput_raw_pressed(bind.controller_button)
        } else if bind.controller_type == "dinput" || bind.controller_type == "dualsense" {
            let idx = if bind.controller_type == "dualsense" {
                legacy_ds4_to_dinput(bind.controller_button)
            } else {
                bind.controller_button
            };
            let name = DINPUT_BUTTONS.get(idx as usize);
            let held = get_dinput_inputs();
            let dinput_pressed = name.map(|n| held.iter().any(|h| h == n)).unwrap_or(false);
            // RL's own bindings (and manually captured xinput binds) can name a
            // button that's only reachable right now through a real xinput pad -
            // fall back to that so the bind still works regardless of which
            // controller happens to be plugged in.
            dinput_pressed
                || name
                    .and_then(|n| dinput_name_to_xinput_mask(n))
                    .is_some_and(|mask| {
                        (0..4).any(|i| {
                            get_xinput_state(i)
                                .map(|state| state.is_pressed(mask))
                                .unwrap_or(false)
                        })
                    })
        } else {
            let mask = bind.controller_button as u16;
            let xinput_pressed = (0..4).any(|i| {
                get_xinput_state(i)
                    .map(|state| state.is_pressed(mask))
                    .unwrap_or(false)
            });
            // same fallback in reverse: an xinput-shaped bind (RL always
            // records its own binds this way) still works on a dinput-only
            // pad (generic or playstation).
            xinput_pressed
                || xinput_mask_to_dinput_name(mask)
                    .is_some_and(|n| get_dinput_inputs().iter().any(|h| h == n))
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

    if let Some(code) = n.strip_prefix("dinput_raw_").and_then(|s| s.parse::<u32>().ok()) {
        return Some(HotkeyBind {
            is_controller: true,
            hotkey: String::new(),
            controller_type: "dinput_raw".to_string(),
            controller_button: code,
        });
    }
    if let Some(mask) = xi_lookup(&n) {
        return Some(HotkeyBind {
            is_controller: true,
            hotkey: String::new(),
            controller_type: "xinput".to_string(),
            controller_button: mask as u32,
        });
    }
    if let Some(idx) = din_lookup(&n) {
        return Some(HotkeyBind {
            is_controller: true,
            hotkey: String::new(),
            controller_type: "dinput".to_string(),
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
    if bind.controller_type == "dinput_raw" {
        format!("dinput_raw_{}", bind.controller_button)
    } else if bind.controller_type == "dinput" {
        DINPUT_BUTTONS
            .get(bind.controller_button as usize)
            .map(|name| name.to_string())
            .unwrap_or_else(|| format!("din_{}", bind.controller_button))
    } else if bind.controller_type == "dualsense" {
        DINPUT_BUTTONS
            .get(legacy_ds4_to_dinput(bind.controller_button) as usize)
            .map(|name| name.to_string())
            .unwrap_or_else(|| format!("din_{}", bind.controller_button))
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

/// block until any kb key / xinput / dinput button press. None on timeout.
///
/// short settle phase ignores inputs already held when capture started (the
/// click/enter that triggered it), then the first new press wins, including
/// keys still held after the settle phase.
pub fn detect_any_hotkey(timeout: Option<Duration>) -> Option<HotkeyBind> {
    let start = Instant::now();
    let mut initial_din: std::collections::HashSet<String> =
        get_dinput_inputs().into_iter().collect();
    let mut initial_din_raw: std::collections::HashSet<u32> =
        get_dinput_raw_inputs().into_iter().collect();

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

        // DInput (ps4/ps5/8bitdo/etc, via gilrs)
        let current_din: std::collections::HashSet<String> =
            get_dinput_inputs().into_iter().collect();
        let new_din: Vec<&String> = current_din.difference(&initial_din).collect();
        if let Some(btn_name) = new_din.first() {
            let idx = DINPUT_BUTTONS.iter().position(|b| b == btn_name).unwrap_or(0) as u32;
            return Some(HotkeyBind {
                is_controller: true,
                hotkey: String::new(),
                controller_type: "dinput".to_string(),
                controller_button: idx,
            });
        }
        initial_din = current_din;

        // DInput paddles/misc buttons gilrs can't name (see dinput.rs)
        let current_din_raw: std::collections::HashSet<u32> =
            get_dinput_raw_inputs().into_iter().collect();
        if let Some(code) = current_din_raw.difference(&initial_din_raw).next() {
            return Some(HotkeyBind {
                is_controller: true,
                hotkey: String::new(),
                controller_type: "dinput_raw".to_string(),
                controller_button: *code,
            });
        }
        initial_din_raw = current_din_raw;

        std::thread::sleep(Duration::from_millis(20));
    }
}

/// init controller backends, call once at startup. starts the dinput
/// monitor thread, xinput needs no init.
pub fn init_controllers() {
    start_dinput_monitor();
}
