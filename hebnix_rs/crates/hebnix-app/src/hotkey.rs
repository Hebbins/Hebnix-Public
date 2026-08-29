//! non-consuming show/hide hotkey polling.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

fn normalize_key(name: &str) -> Option<String> {
    let key = name.trim().to_lowercase();
    let mut chars = key.chars();
    if let (Some(character), None) = (chars.next(), chars.next()) {
        if character.is_ascii_alphanumeric() {
            return Some(key);
        }
    }

    matches!(
        key.as_str(),
        "f1" | "f2"
            | "f3"
            | "f4"
            | "f5"
            | "f6"
            | "f7"
            | "f8"
            | "f9"
            | "f10"
            | "f11"
            | "f12"
            | "tab"
            | "space"
            | "enter"
            | "return"
            | "backspace"
            | "esc"
            | "escape"
            | "insert"
            | "delete"
            | "home"
            | "end"
            | "page up"
            | "page down"
            | "up"
            | "down"
            | "left"
            | "right"
            | "`"
            | "-"
            | "="
            | "["
            | "]"
            | "\\"
            | ";"
            | "'"
            | ","
            | "."
            | "/"
    )
    .then_some(key)
}

pub struct ToggleHotkey {
    key: Arc<RwLock<Option<String>>>,
    stop: Arc<AtomicBool>,
}

impl ToggleHotkey {
    pub fn new() -> Option<Self> {
        Some(Self {
            key: Arc::new(RwLock::new(None)),
            stop: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn rebind(&mut self, key_name: &str) -> bool {
        let Some(key) = normalize_key(key_name) else {
            tracing::warn!("cannot map '{key_name}' to a hotkey");
            return false;
        };
        let Ok(mut current) = self.key.write() else {
            return false;
        };
        *current = Some(key);
        true
    }

    pub fn listen<F>(&self, mut on_press: F)
    where
        F: FnMut() + Send + 'static,
    {
        let key = Arc::clone(&self.key);
        let stop = Arc::clone(&self.stop);
        std::thread::Builder::new()
            .name("hotkey-listener".into())
            .spawn(move || {
                let mut observed_key = None;
                let mut was_down = false;
                while !stop.load(Ordering::Relaxed) {
                    let current = key.read().ok().and_then(|value| value.clone());
                    if current != observed_key {
                        observed_key = current.clone();
                        was_down = current
                            .as_deref()
                            .is_some_and(hebnix_sdk::input::is_key_pressed);
                    } else if let Some(current) = current.as_deref() {
                        let is_down = hebnix_sdk::input::is_key_pressed(current);
                        if is_down && !was_down {
                            on_press();
                        }
                        was_down = is_down;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            })
            .ok();
    }
}

impl Drop for ToggleHotkey {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
