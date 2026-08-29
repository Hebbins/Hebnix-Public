//! hotkey detection via GetAsyncKeyState, windows only
//
// key names match the python keyboard lib ("tab", "f2", "ctrl", etc) so old configs still work

use std::time::{Duration, Instant};

use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

// (name, vk code) for named keys
const NAMED_KEYS: [(&str, i32); 58] = [
    ("backspace", 0x08),
    ("tab", 0x09),
    ("enter", 0x0D),
    ("return", 0x0D),
    ("shift", 0x10),
    ("ctrl", 0x11),
    ("control", 0x11),
    ("alt", 0x12),
    ("pause", 0x13),
    ("caps lock", 0x14),
    ("esc", 0x1B),
    ("escape", 0x1B),
    ("space", 0x20),
    ("page up", 0x21),
    ("page down", 0x22),
    ("end", 0x23),
    ("home", 0x24),
    ("left", 0x25),
    ("up", 0x26),
    ("right", 0x27),
    ("down", 0x28),
    ("print screen", 0x2C),
    ("insert", 0x2D),
    ("delete", 0x2E),
    ("f1", 0x70),
    ("f2", 0x71),
    ("f3", 0x72),
    ("f4", 0x73),
    ("f5", 0x74),
    ("f6", 0x75),
    ("f7", 0x76),
    ("f8", 0x77),
    ("f9", 0x78),
    ("f10", 0x79),
    ("f11", 0x7A),
    ("f12", 0x7B),
    ("num lock", 0x90),
    ("scroll lock", 0x91),
    ("left shift", 0xA0),
    ("right shift", 0xA1),
    ("left ctrl", 0xA2),
    ("right ctrl", 0xA3),
    ("left alt", 0xA4),
    ("right alt", 0xA5),
    ("left windows", 0x5B),
    ("right windows", 0x5C),
    (";", 0xBA),
    ("=", 0xBB),
    (",", 0xBC),
    ("-", 0xBD),
    (".", 0xBE),
    ("/", 0xBF),
    ("`", 0xC0),
    ("[", 0xDB),
    ("\\", 0xDC),
    ("]", 0xDD),
    ("'", 0xDE),
    ("+", 0xBB),
];

/// key name to windows vk code
pub fn name_to_vk(key: &str) -> Option<i32> {
    let lower = key.trim().to_lowercase();
    if let Some((_, vk)) = NAMED_KEYS.iter().find(|(n, _)| *n == lower) {
        return Some(*vk);
    }
    let mut chars = lower.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphanumeric() {
            return Some(c.to_ascii_uppercase() as i32);
        }
    }
    None
}

/// vk code back to key name, used by hotkey capture
pub fn vk_to_name(vk: i32) -> Option<String> {
    // prefer plain letters/digits
    if (0x30..=0x39).contains(&vk) || (0x41..=0x5A).contains(&vk) {
        return Some((vk as u8 as char).to_ascii_lowercase().to_string());
    }
    NAMED_KEYS
        .iter()
        .find(|(_, code)| *code == vk)
        .map(|(n, _)| n.to_string())
}

/// true if key is held down right now (system-wide)
pub fn is_key_pressed(key: &str) -> bool {
    let Some(vk) = name_to_vk(key) else {
        return false;
    };
    unsafe { (GetAsyncKeyState(vk) as u16) & 0x8000 != 0 }
}

// vk codes worth scanning during capture
fn capture_vks() -> Vec<i32> {
    let mut vks: Vec<i32> = Vec::new();
    vks.extend(0x30..=0x39); // digits
    vks.extend(0x41..=0x5A); // letters
    vks.extend(NAMED_KEYS.iter().map(|(_, vk)| *vk));
    vks.sort_unstable();
    vks.dedup();
    vks
}

/// single non-blocking scan, name of whatever key is held or None.
/// no wait-for-release like detect_hotkey, so a key already held gets reported.
/// detect_any_hotkey needs that.
pub fn scan_pressed_key() -> Option<String> {
    let vks = capture_vks();
    for vk in vks {
        if unsafe { (GetAsyncKeyState(vk) as u16) & 0x8000 != 0 } {
            if let Some(name) = vk_to_name(vk) {
                return Some(name);
            }
        }
    }
    None
}

/// blocks until a key is pressed, returns its name.
/// None if timeout hits first.
pub fn detect_hotkey(timeout: Option<Duration>) -> Option<String> {
    let vks = capture_vks();
    let start = Instant::now();

    // wait for everything to release first so the click that opened capture doesn't count
    loop {
        let any_down = vks
            .iter()
            .any(|vk| unsafe { (GetAsyncKeyState(*vk) as u16) & 0x8000 != 0 });
        if !any_down {
            break;
        }
        if let Some(t) = timeout {
            if start.elapsed() > t {
                return None;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    loop {
        for vk in &vks {
            if unsafe { (GetAsyncKeyState(*vk) as u16) & 0x8000 != 0 } {
                if let Some(name) = vk_to_name(*vk) {
                    return Some(name);
                }
            }
        }
        if let Some(t) = timeout {
            if start.elapsed() > t {
                return None;
            }
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}
