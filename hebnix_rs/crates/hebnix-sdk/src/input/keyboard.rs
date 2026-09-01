//! hotkey detection via GetAsyncKeyState, windows only
//
// key names match the python keyboard lib ("tab", "f2", "ctrl", etc) so old configs still work

use std::time::{Duration, Instant};

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_SHIFT, VkKeyScanW,
};

// (name, vk code) for named keys
const NAMED_KEYS: [(&str, i32); 65] = [
    ("mouse_left", 0x01),
    ("mouse_right", 0x02),
    ("mouse_middle", 0x04),
    ("mouse_x1", 0x05),
    ("mouse_x2", 0x06),
    ("left mouse button", 0x01),
    ("right mouse button", 0x02),
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

// --- synthetic input (SendInput) ---
//
// used for things like quick-chat automation: tapping the chat-open key and
// typing the message. gated by callers (not here) to whatever policy applies,
// e.g. hebnix's "not while a match is in progress" rule for raw input.

fn send_one(input: INPUT) {
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

fn vk_input(vk: i32, key_up: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk as u16),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

const TAP_GAP: Duration = Duration::from_millis(1);

/// press+release a vk code
pub fn tap_vk(vk: i32) {
    send_one(vk_input(vk, false));
    std::thread::sleep(TAP_GAP);
    send_one(vk_input(vk, true));
    std::thread::sleep(TAP_GAP);
}

/// press+release a named key ("enter", "t", "f1", ...). false if the name
/// isn't recognized.
pub fn tap_key(name: &str) -> bool {
    match name_to_vk(name) {
        Some(vk) => {
            tap_vk(vk);
            true
        }
        None => false,
    }
}

/// type text by tapping real vk codes (shift held for caps/punctuation as
/// needed), mapped per the active keyboard layout via VkKeyScanW.
///
/// deliberately not KEYEVENTF_UNICODE: that path has no real vk/scan code
/// attached, so games that read keyboard via raw input (most UE titles,
/// including RL) never see it — the chat box opens (a real vk tap) and
/// enter sends (also a real vk tap), but nothing typed in between shows up.
/// tapping real keys goes through the same path a physical keypress would.
pub fn type_text(text: &str) {
    let _ = type_text_while(text, || true);
}

/// type text while the supplied guard remains true.
pub fn type_text_while<F>(text: &str, should_continue: F) -> bool
where
    F: Fn() -> bool,
{
    for unit in text.encode_utf16() {
        if !should_continue() {
            return false;
        }
        let scan = unsafe { VkKeyScanW(unit) };
        if scan == -1 {
            continue; // unmappable on the current layout, skip
        }
        let vk = (scan & 0xFF) as i32;
        let need_shift = (scan >> 8) & 0x1 != 0;

        if need_shift {
            send_one(vk_input(VK_SHIFT.0 as i32, false));
            std::thread::sleep(TAP_GAP);
        }
        tap_vk(vk);
        if need_shift {
            send_one(vk_input(VK_SHIFT.0 as i32, true));
            std::thread::sleep(TAP_GAP);
        }
    }
    true
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
