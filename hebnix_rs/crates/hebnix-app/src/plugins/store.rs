//! per-plugin key-value settings, saved to plugins/config/<slug>/settings.toml

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::plugins::cvar::CvarValue;

#[derive(Debug)]
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

    pub fn get_cvar(&self, name: &str) -> Option<CvarValue> {
        let value = self.values.get("cvars")?.as_table()?.get(name)?;
        match value {
            toml::Value::String(value) => Some(CvarValue::String(value.clone())),
            toml::Value::Integer(value) => Some(CvarValue::Integer(*value)),
            toml::Value::Float(value) if value.is_finite() => Some(CvarValue::Number(*value)),
            _ => None,
        }
    }

    pub fn set_cvar(&mut self, name: &str, value: &CvarValue) {
        let cvars = self
            .values
            .entry("cvars".to_string())
            .or_insert_with(|| toml::Value::Table(Default::default()));
        if !cvars.is_table() {
            *cvars = toml::Value::Table(Default::default());
        }
        let value = match value {
            CvarValue::String(value) => toml::Value::String(value.clone()),
            CvarValue::Integer(value) => toml::Value::Integer(*value),
            CvarValue::Number(value) => toml::Value::Float(*value),
        };
        cvars
            .as_table_mut()
            .expect("cvars was replaced with a table")
            .insert(name.to_string(), value);
        self.save();
    }

    pub fn delete_cvar(&mut self, name: &str) {
        if let Some(cvars) = self.values.get_mut("cvars").and_then(|v| v.as_table_mut()) {
            cvars.remove(name);
        }
        self.save();
    }
}
