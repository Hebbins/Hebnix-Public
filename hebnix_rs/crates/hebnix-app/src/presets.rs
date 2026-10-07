use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Preset {
    pub name: String,
    #[serde(default)]
    pub include_patches: bool,
    #[serde(default)]
    pub patches: serde_json::Value,
    #[serde(default)]
    pub swaps: Vec<serde_json::Value>,
}

#[derive(Clone)]
pub enum MissingItem {
    Swap(usize),
    Patch { kind: String, key: Option<String> },
}

#[derive(Serialize, Deserialize)]
struct PresetKey {
    version: u8,
    preset: Preset,
}

pub struct PresetStore {
    pub dir: PathBuf,
    pub presets: Vec<Preset>,
    pub selected: usize,
    pub name_edit: String,
    pub include_patches: bool,
    pub editing: Option<Preset>,
    pub preset_key: String,
    pub status: String,
    pub missing: Vec<MissingItem>,
    pub pending_ball: Option<String>,
    pub pending_boost: Option<String>,
    pub pending_decals: Vec<(String, String)>,
}

impl PresetStore {
    pub fn new(base_dir: &Path) -> Self {
        let dir = base_dir.join("presets");
        let _ = std::fs::create_dir_all(&dir);
        let mut this = Self {
            dir,
            presets: Vec::new(),
            selected: 0,
            name_edit: String::new(),
            include_patches: true,
            editing: None,
            preset_key: String::new(),
            status: String::new(),
            missing: Vec::new(),
            pending_ball: None,
            pending_boost: None,
            pending_decals: Vec::new(),
        };
        this.refresh();
        this
    }
    pub fn refresh(&mut self) {
        self.presets.clear();
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(preset) = serde_json::from_slice(&bytes) {
                    self.presets.push(preset);
                }
            }
        }
        self.presets
            .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        self.selected = self.selected.min(self.presets.len().saturating_sub(1));
    }
    pub fn save(&mut self, preset: Preset) -> Result<(), String> {
        if self.presets.len() >= 10 && !self.presets.iter().any(|p| p.name == preset.name) {
            return Err("A maximum of 10 presets is supported".into());
        }
        let safe: String = preset
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if safe.trim().is_empty() {
            return Err("Enter a preset name".into());
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        std::fs::write(
            self.dir.join(format!("{safe}.json")),
            serde_json::to_vec_pretty(&preset).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        self.refresh();
        Ok(())
    }

    pub fn begin_edit_selected(&mut self) {
        self.editing = self.presets.get(self.selected).cloned();
        self.status.clear();
    }

    pub fn cancel_edit(&mut self) {
        self.editing = None;
    }

    pub fn save_edit(&mut self) -> Result<(), String> {
        let preset = self.editing.clone().ok_or("No preset is being edited")?;
        let old_name = self
            .presets
            .get(self.selected)
            .map(|preset| preset.name.clone())
            .unwrap_or_default();
        self.save(preset.clone())?;
        if old_name != preset.name {
            let old_path = self.path_for_name(&old_name);
            if old_path != self.path_for_name(&preset.name) {
                let _ = std::fs::remove_file(old_path);
            }
            self.refresh();
        }
        if let Some(index) = self.presets.iter().position(|item| item.name == preset.name) {
            self.selected = index;
        }
        self.editing = None;
        Ok(())
    }

    pub fn selected_key(&self) -> Result<String, String> {
        let preset = self
            .presets
            .get(self.selected)
            .cloned()
            .ok_or("No preset is selected")?;
        let bytes = serde_json::to_vec(&PresetKey { version: 1, preset })
            .map_err(|error| error.to_string())?;
        Ok(URL_SAFE_NO_PAD.encode(bytes))
    }

    pub fn import_key(&mut self, key: &str) -> Result<(), String> {
        let bytes = URL_SAFE_NO_PAD
            .decode(key.trim())
            .map_err(|_| "The preset key is not valid base64".to_string())?;
        let envelope: PresetKey = serde_json::from_slice(&bytes)
            .map_err(|_| "The preset key does not contain a valid preset".to_string())?;
        if envelope.version != 1 {
            return Err(format!("Preset key version {} is not supported", envelope.version));
        }
        let name = envelope.preset.name.clone();
        self.save(envelope.preset)?;
        if let Some(index) = self.presets.iter().position(|item| item.name == name) {
            self.selected = index;
        }
        Ok(())
    }

    pub fn remove_missing_selected(&mut self) -> Result<(), String> {
        let mut preset = self
            .presets
            .get(self.selected)
            .cloned()
            .ok_or("No preset is selected")?;
        let mut swap_indexes = self
            .missing
            .iter()
            .filter_map(|item| match item {
                MissingItem::Swap(index) => Some(*index),
                _ => None,
            })
            .collect::<Vec<_>>();
        swap_indexes.sort_unstable_by(|left, right| right.cmp(left));
        swap_indexes.dedup();
        for index in swap_indexes {
            if index < preset.swaps.len() {
                preset.swaps.remove(index);
            }
        }
        for missing in &self.missing {
            let MissingItem::Patch { kind, key } = missing else {
                continue;
            };
            let Some(patches) = preset.patches.as_object_mut() else {
                continue;
            };
            if let Some(key) = key {
                if let Some(entries) = patches.get_mut(kind).and_then(|value| value.as_object_mut()) {
                    entries.remove(key);
                    if entries.is_empty() {
                        patches.remove(kind);
                    }
                }
            } else {
                patches.remove(kind);
            }
        }
        self.save(preset)?;
        self.missing.clear();
        self.status = "Missing items were removed from the preset.".into();
        Ok(())
    }

    fn path_for_name(&self, name: &str) -> PathBuf {
        let safe: String = name
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect();
        self.dir.join(format!("{safe}.json"))
    }
    pub fn delete_selected(&mut self) {
        if let Some(p) = self.presets.get(self.selected) {
            let safe: String = p
                .name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let _ = std::fs::remove_file(self.dir.join(format!("{safe}.json")));
        }
        self.refresh();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_key_round_trips_paint_metadata() {
        let preset = Preset {
            name: "Painted".into(),
            include_patches: false,
            patches: serde_json::Value::Null,
            swaps: vec![serde_json::json!({
                "source_upk": "A.upk",
                "target_upk": "B.upk",
                "paint": { "Preset": 7 }
            })],
        };
        let bytes = serde_json::to_vec(&PresetKey {
            version: 1,
            preset: preset.clone(),
        })
        .unwrap();
        let decoded = URL_SAFE_NO_PAD.decode(URL_SAFE_NO_PAD.encode(bytes)).unwrap();
        let key: PresetKey = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(key.preset.swaps, preset.swaps);
    }
}
