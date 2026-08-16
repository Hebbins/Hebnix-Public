//! global show/hide hotkey via the global-hotkey crate.

use global_hotkey::GlobalHotKeyManager;
use global_hotkey::hotkey::{Code, HotKey};

/// map a config key name to a global-hotkey Code
pub fn name_to_code(name: &str) -> Option<Code> {
    let n = name.trim().to_lowercase();
    // single letters/digits
    let mut chars = n.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphabetic() {
            return code_from_letter(c);
        }
        if c.is_ascii_digit() {
            return code_from_digit(c);
        }
    }
    let code = match n.as_str() {
        "f1" => Code::F1,
        "f2" => Code::F2,
        "f3" => Code::F3,
        "f4" => Code::F4,
        "f5" => Code::F5,
        "f6" => Code::F6,
        "f7" => Code::F7,
        "f8" => Code::F8,
        "f9" => Code::F9,
        "f10" => Code::F10,
        "f11" => Code::F11,
        "f12" => Code::F12,
        "tab" => Code::Tab,
        "space" => Code::Space,
        "enter" | "return" => Code::Enter,
        "backspace" => Code::Backspace,
        "esc" | "escape" => Code::Escape,
        "insert" => Code::Insert,
        "delete" => Code::Delete,
        "home" => Code::Home,
        "end" => Code::End,
        "page up" => Code::PageUp,
        "page down" => Code::PageDown,
        "up" => Code::ArrowUp,
        "down" => Code::ArrowDown,
        "left" => Code::ArrowLeft,
        "right" => Code::ArrowRight,
        "`" => Code::Backquote,
        "-" => Code::Minus,
        "=" => Code::Equal,
        "[" => Code::BracketLeft,
        "]" => Code::BracketRight,
        "\\" => Code::Backslash,
        ";" => Code::Semicolon,
        "'" => Code::Quote,
        "," => Code::Comma,
        "." => Code::Period,
        "/" => Code::Slash,
        _ => return None,
    };
    Some(code)
}

fn code_from_letter(c: char) -> Option<Code> {
    Some(match c.to_ascii_lowercase() {
        'a' => Code::KeyA,
        'b' => Code::KeyB,
        'c' => Code::KeyC,
        'd' => Code::KeyD,
        'e' => Code::KeyE,
        'f' => Code::KeyF,
        'g' => Code::KeyG,
        'h' => Code::KeyH,
        'i' => Code::KeyI,
        'j' => Code::KeyJ,
        'k' => Code::KeyK,
        'l' => Code::KeyL,
        'm' => Code::KeyM,
        'n' => Code::KeyN,
        'o' => Code::KeyO,
        'p' => Code::KeyP,
        'q' => Code::KeyQ,
        'r' => Code::KeyR,
        's' => Code::KeyS,
        't' => Code::KeyT,
        'u' => Code::KeyU,
        'v' => Code::KeyV,
        'w' => Code::KeyW,
        'x' => Code::KeyX,
        'y' => Code::KeyY,
        'z' => Code::KeyZ,
        _ => return None,
    })
}

fn code_from_digit(c: char) -> Option<Code> {
    Some(match c {
        '0' => Code::Digit0,
        '1' => Code::Digit1,
        '2' => Code::Digit2,
        '3' => Code::Digit3,
        '4' => Code::Digit4,
        '5' => Code::Digit5,
        '6' => Code::Digit6,
        '7' => Code::Digit7,
        '8' => Code::Digit8,
        '9' => Code::Digit9,
        _ => return None,
    })
}

/// owns the manager + the currently registered toggle hotkey
pub struct ToggleHotkey {
    manager: GlobalHotKeyManager,
    current: Option<HotKey>,
}

impl ToggleHotkey {
    pub fn new() -> Option<Self> {
        GlobalHotKeyManager::new().ok().map(|manager| Self {
            manager,
            current: None,
        })
    }

    /// (re)register the toggle hotkey. false if the name can't map or register
    /// fails, in which case the old binding stays so the menu isn't lost.
    pub fn rebind(&mut self, key_name: &str) -> bool {
        let Some(code) = name_to_code(key_name) else {
            tracing::warn!("cannot map '{key_name}' to a global hotkey");
            return false;
        };
        let hotkey = HotKey::new(None, code);
        if let Some(old) = self.current {
            if old.id() == hotkey.id() {
                return true; // already bound to this key
            }
        }
        // register new before dropping old, so a failed rebind keeps the old
        // key. register fails if another app already owns it system-wide.
        match self.manager.register(hotkey) {
            Ok(()) => {
                if let Some(old) = self.current.take() {
                    let _ = self.manager.unregister(old);
                }
                self.current = Some(hotkey);
                tracing::info!("global hotkey bound to '{key_name}'");
                true
            }
            Err(e) => {
                tracing::warn!(
                    "failed to register hotkey '{key_name}' (already taken by another app?): {e}"
                );
                false
            }
        }
    }
}
