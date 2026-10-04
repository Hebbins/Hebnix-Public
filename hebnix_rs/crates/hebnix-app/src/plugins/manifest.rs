//! plugin discovery + manifests.
//!
//! a plugin is a folder in plugins/ with a plugin.toml and the lua file it
//! names. identity is the slug (the folder name), not the display name.
//! broken folders come back with an error set instead of getting dropped.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PluginManifest {
    pub name: String,
    pub author: String,
    pub version: String,
    pub entry: String,
    pub plugin_id: Option<String>,
    /// capabilities the plugin declares it needs, enforced by the host.
    pub permissions: PluginPermissions,
}

/// per-plugin capability grants, declared under [permissions] in plugin.toml.
/// Absent keys default to no access, so a plugin only gets what it asks for.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PluginPermissions {
    /// directory roots this plugin may read via hebnix.read_file. Entries may
    /// contain %VAR% environment placeholders (e.g. "%LOCALAPPDATA%/Spotify").
    /// A read is allowed only if the target canonicalizes under one of these.
    pub read_roots: Vec<String>,
}

impl Default for PluginManifest {
    fn default() -> Self {
        Self {
            name: String::new(),
            author: "Unknown".to_string(),
            version: "1.0".to_string(),
            entry: String::new(),
            plugin_id: None,
            permissions: PluginPermissions::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    pub slug: String,
    pub entry_path: PathBuf,
    pub manifest: PluginManifest,
    pub error: Option<String>,
}

impl DiscoveredPlugin {
    pub fn filename(&self) -> String {
        format!("{}/{}", self.slug, self.manifest.entry)
    }
}

const RESERVED_DIRS: [&str; 3] = ["config", "cache", "runtime"];

pub fn discover_plugins(plugin_dir: &Path) -> Vec<DiscoveredPlugin> {
    let mut found: Vec<DiscoveredPlugin> = Vec::new();
    let Ok(entries) = std::fs::read_dir(plugin_dir) else {
        return found;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if name.starts_with("__") || name.starts_with('.') {
            continue;
        }

        if !path.is_dir() {
            if name.ends_with(".lua") {
                let slug = name.trim_end_matches(".lua").to_string();
                found.push(broken(
                    &slug,
                    &path,
                    format!("loose .lua files aren't plugins, move it to {slug}/main.lua and add a plugin.toml"),
                ));
            }
            continue;
        }
        if RESERVED_DIRS.contains(&name.as_str()) {
            continue;
        }

        match read_manifest(&path.join("plugin.toml")) {
            Err(e) => found.push(broken(&name, &path, e)),
            Ok(manifest) => {
                let entry_path = path.join(&manifest.entry);
                if entry_path.is_file() {
                    found.push(DiscoveredPlugin {
                        slug: name,
                        entry_path,
                        manifest,
                        error: None,
                    });
                } else {
                    let missing = manifest.entry.clone();
                    found.push(broken(
                        &name,
                        &path,
                        format!("plugin.toml points at {missing}, which isn't there"),
                    ));
                }
            }
        }
    }

    found.sort_by(|a, b| a.slug.cmp(&b.slug));
    found
}

fn broken(slug: &str, dir: &Path, error: String) -> DiscoveredPlugin {
    DiscoveredPlugin {
        slug: slug.to_string(),
        entry_path: dir.to_path_buf(),
        manifest: PluginManifest {
            name: slug.to_string(),
            ..Default::default()
        },
        error: Some(error),
    }
}

fn read_manifest(path: &Path) -> Result<PluginManifest, String> {
    let text = std::fs::read_to_string(path).map_err(|_| "no plugin.toml".to_string())?;
    let manifest: PluginManifest =
        toml::from_str(&text).map_err(|e| format!("plugin.toml won't parse: {e}"))?;
    if manifest.name.trim().is_empty() {
        return Err("plugin.toml has no name".to_string());
    }
    if manifest.entry.trim().is_empty() {
        return Err("plugin.toml has no entry".to_string());
    }
    Ok(manifest)
}
