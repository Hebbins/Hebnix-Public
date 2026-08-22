use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Preset {
    pub name: String,
    pub include_patches: bool,
    pub patches: serde_json::Value,
    pub swaps: Vec<serde_json::Value>,
}

pub struct PresetStore {
    pub dir: PathBuf,
    pub presets: Vec<Preset>,
    pub selected: usize,
    pub name_edit: String,
    pub include_patches: bool,
}

impl PresetStore {
    pub fn new(base_dir: &Path) -> Self {
        let dir = base_dir.join("presets");
        let _ = std::fs::create_dir_all(&dir);
        let mut this = Self { dir, presets: Vec::new(), selected: 0, name_edit: String::new(), include_patches: true };
        this.refresh();
        this
    }
    pub fn refresh(&mut self) {
        self.presets.clear();
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") { continue; }
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(preset) = serde_json::from_slice(&bytes) { self.presets.push(preset); }
            }
        }
        self.presets.sort_by(|a,b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        self.selected = self.selected.min(self.presets.len().saturating_sub(1));
    }
    pub fn save(&mut self, preset: Preset) -> Result<(), String> {
        if self.presets.len() >= 10 && !self.presets.iter().any(|p| p.name == preset.name) { return Err("A maximum of 10 presets is supported".into()); }
        let safe: String = preset.name.chars().map(|c| if c.is_ascii_alphanumeric() || c=='-' || c=='_' { c } else {'_'}).collect();
        if safe.trim().is_empty() { return Err("Enter a preset name".into()); }
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        std::fs::write(self.dir.join(format!("{safe}.json")), serde_json::to_vec_pretty(&preset).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        self.refresh(); Ok(())
    }
    pub fn delete_selected(&mut self) {
        if let Some(p) = self.presets.get(self.selected) {
            let safe: String = p.name.chars().map(|c| if c.is_ascii_alphanumeric() || c=='-' || c=='_' { c } else {'_'}).collect();
            let _ = std::fs::remove_file(self.dir.join(format!("{safe}.json")));
        }
        self.refresh();
    }
}
