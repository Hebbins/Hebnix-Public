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
}

impl Default for PluginManifest {
    fn default() -> Self {
        Self {
            name: String::new(),
            author: "Unknown".to_string(),
            version: "1.0".to_string(),
            entry: String::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hebnix_discover_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn plugin(root: &Path, slug: &str, toml: Option<&str>, lua: Option<&str>) {
        let dir = root.join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(t) = toml {
            std::fs::write(dir.join("plugin.toml"), t).unwrap();
        }
        if let Some(name) = lua {
            std::fs::write(dir.join(name), "return {}").unwrap();
        }
    }

    #[test]
    fn accepts_a_complete_plugin() {
        let root = tmp("ok");
        plugin(
            &root,
            "good",
            Some(
                "name = \"Good\"\nauthor = \"nixvio64\"\nversion = \"2.0\"\nentry = \"main.lua\"\n",
            ),
            Some("main.lua"),
        );
        let found = discover_plugins(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].slug, "good");
        assert_eq!(found[0].manifest.name, "Good");
        assert_eq!(found[0].manifest.entry, "main.lua");
        assert!(found[0].error.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn author_and_version_still_default() {
        let root = tmp("defaults");
        plugin(
            &root,
            "bare",
            Some("name = \"B\"\nentry = \"main.lua\"\n"),
            Some("main.lua"),
        );
        let found = discover_plugins(&root);
        assert!(found[0].error.is_none());
        assert_eq!(found[0].manifest.author, "Unknown");
        assert_eq!(found[0].manifest.version, "1.0");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_a_missing_entry_key() {
        let root = tmp("no_entry");
        plugin(&root, "lazy", Some("name = \"L\"\n"), Some("main.lua"));
        let found = discover_plugins(&root);
        assert_eq!(found.len(), 1);
        assert!(
            found[0].error.as_deref().unwrap().contains("no entry"),
            "a main.lua sitting right there must not be picked up implicitly"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_missing_malformed_and_nameless_toml() {
        let root = tmp("bad");
        plugin(&root, "no_toml", None, Some("main.lua"));
        plugin(&root, "garbage", Some("name = = ["), Some("main.lua"));
        plugin(
            &root,
            "nameless",
            Some("author = \"x\"\n"),
            Some("main.lua"),
        );

        let found = discover_plugins(&root);
        assert_eq!(found.len(), 3, "broken plugins must still be reported");
        for p in &found {
            let err = p.error.as_deref().unwrap_or("");
            assert!(!err.is_empty(), "{} should carry a reason", p.slug);
        }
        let by = |s: &str| {
            found
                .iter()
                .find(|p| p.slug == s)
                .unwrap()
                .error
                .clone()
                .unwrap()
        };
        assert!(by("no_toml").contains("no plugin.toml"));
        assert!(by("garbage").contains("won't parse"));
        assert!(by("nameless").contains("no name"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_a_missing_entry_file() {
        let root = tmp("entry");
        plugin(
            &root,
            "wrong",
            Some("name = \"W\"\nentry = \"init.lua\"\n"),
            Some("main.lua"),
        );
        let found = discover_plugins(&root);
        assert_eq!(found.len(), 1);
        assert!(found[0].error.as_deref().unwrap().contains("init.lua"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn loose_lua_is_reported_not_loaded() {
        let root = tmp("loose");
        std::fs::write(root.join("old_plugin.lua"), "return {}").unwrap();
        let found = discover_plugins(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].slug, "old_plugin");
        assert!(
            found[0]
                .error
                .as_deref()
                .unwrap()
                .contains("old_plugin/main.lua")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skips_reserved_and_hidden_dirs() {
        let root = tmp("reserved");
        for d in ["config", "cache", "runtime", "__pycache__", ".git"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        assert!(discover_plugins(&root).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
