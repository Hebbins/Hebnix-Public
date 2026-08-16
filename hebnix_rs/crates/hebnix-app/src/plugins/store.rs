//! per-plugin key-value settings, saved to plugins/config/<slug>/settings.toml

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub struct PluginStore {
    path: PathBuf,
    values: BTreeMap<String, toml::Value>,
}

impl PluginStore {
    pub fn load(plugin_dir: &Path, slug: &str) -> Self {
        let dir = plugin_dir.join("config").join(slug);
        let path = dir.join("settings.toml");
        let values = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default();
        Self { path, values }
    }

    fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = toml::to_string_pretty(&self.values) {
            let _ = std::fs::write(&self.path, text);
        }
    }

    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        self.values
            .get(key)
            .and_then(|v| v.as_bool())
            .unwrap_or(default)
    }

    pub fn get_string(&self, key: &str, default: &str) -> String {
        self.values
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    }

    pub fn get_number(&self, key: &str, default: f64) -> f64 {
        self.values
            .get(key)
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .unwrap_or(default)
    }

    pub fn set_bool(&mut self, key: &str, value: bool) {
        self.values
            .insert(key.to_string(), toml::Value::Boolean(value));
        self.save();
    }

    pub fn set_string(&mut self, key: &str, value: &str) {
        self.values
            .insert(key.to_string(), toml::Value::String(value.to_string()));
        self.save();
    }

    pub fn set_number(&mut self, key: &str, value: f64) {
        self.values
            .insert(key.to_string(), toml::Value::Float(value));
        self.save();
    }
}
