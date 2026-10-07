//! plugin lifecycle: discovery, load/unload/reload, event dispatch,
//! settings/window rendering.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use crossbeam_channel::Sender;
use eframe::egui;
use mlua::{Lua, RegistryKey, Table, Value as LuaValue};

use hebnix_sdk::stats::StatsEvent;

use crate::config::Config;
use crate::messages::AppMsg;
use crate::plugins::cvar::{ConsoleCvarCommand, CvarRegistry, parse_console_command};
use crate::plugins::lua_api::{self, HostCtx, HostShared, WindowState};
use crate::plugins::manifest::{DiscoveredPlugin, PluginManifest, discover_plugins};
use crate::plugins::store::PluginStore;

/// expand windows %VAR% env placeholders in a manifest path. unknown vars are
/// left verbatim, so canonicalize just drops the root.
fn expand_env_vars(input: &str) -> String {
    let mut out = String::new();
    for (i, part) in input.split('%').enumerate() {
        if i % 2 == 0 {
            out.push_str(part);
        } else if let Ok(val) = std::env::var(part) {
            out.push_str(&val);
        } else {
            out.push('%');
            out.push_str(part);
            out.push('%');
        }
    }
    out
}

pub struct PluginRuntime {
    lua: Lua,
    plugin_table: RegistryKey,
    pub host: Rc<HostCtx>,
}

pub struct LoadedPlugin {
    pub slug: String,
    pub manifest: PluginManifest,
    pub filename: String,
    pub enabled: bool,
    pub load_error: Option<String>,
    pub runtime: Option<PluginRuntime>,
}

impl LoadedPlugin {
    pub fn display_name(&self) -> &str {
        &self.manifest.name
    }

    pub fn has_settings(&self) -> bool {
        self.runtime
            .as_ref()
            .map(|rt| {
                rt.lua
                    .registry_value::<Table>(&rt.plugin_table)
                    .and_then(|t| t.get::<LuaValue>("on_settings"))
                    .map(|v| v.is_function())
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }
}

const POS_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1000);

pub struct PluginManager {
    pub plugin_dir: PathBuf,
    pub plugins: Vec<LoadedPlugin>,
    tx: Sender<AppMsg>,
    pub shared: Rc<RefCell<HostShared>>,
    last_pos_flush: std::time::Instant,
    last_tick_dispatch: std::time::Instant,
}

impl PluginManager {
    pub fn new(plugin_dir: PathBuf, tx: Sender<AppMsg>, app_version: &str) -> Self {
        let _ = std::fs::create_dir_all(&plugin_dir);
        Self {
            plugin_dir,
            plugins: Vec::new(),
            tx,
            shared: Rc::new(RefCell::new(HostShared {
                is_gui_open: true,
                rl_connected: false,
                in_match: false,
                app_version: app_version.to_string(),
                platform: String::new(),
                suppress_plugin_logs: false,
                rl_config_dir: PathBuf::new(),
                cvars: CvarRegistry::default(),
            })),
            last_pos_flush: std::time::Instant::now(),
            last_tick_dispatch: std::time::Instant::now() - std::time::Duration::from_secs(1),
        }
    }

    fn log(&self, msg: impl Into<String>) {
        let _ = self.tx.send(AppMsg::Log(msg.into()));
    }

    /// full refresh: unload all, re-discover, load the enabled ones
    pub fn refresh(&mut self, config: &mut Config, _verbose: bool) {
        self.update_rl_config_dir(config);
        for plugin in &mut self.plugins {
            if plugin.enabled {
                Self::call_on_unload(plugin);
            }
        }
        self.plugins.clear();

        let discovered = discover_plugins(&self.plugin_dir);
        for disc in discovered {
            if let Some(err) = &disc.error {
                self.log(format!("[Core] Ignoring '{}': {err}", disc.slug));
                self.plugins.push(LoadedPlugin {
                    slug: disc.slug.clone(),
                    manifest: disc.manifest.clone(),
                    filename: disc.filename(),
                    enabled: false,
                    load_error: Some(err.clone()),
                    runtime: None,
                });
                continue; // no config entry, its not a plugin yet
            }

            let enabled = match config.plugins.get(&disc.slug) {
                Some(v) => *v,
                None => {
                    config.plugins.insert(disc.slug.clone(), false);
                    false
                }
            };

            let mut plugin = LoadedPlugin {
                slug: disc.slug.clone(),
                manifest: disc.manifest.clone(),
                filename: disc.filename(),
                enabled,
                load_error: None,
                runtime: None,
            };

            if enabled {
                match self.instantiate(&disc) {
                    Ok(runtime) => {
                        plugin.runtime = Some(runtime);
                        if let Err(e) = Self::call_callback_on(&mut plugin, "on_load", ()) {
                            self.log(format!(
                                "[Core] Plugin {} crashed on load: {e}",
                                plugin.display_name()
                            ));
                            plugin.enabled = false;
                            Self::call_on_unload(&mut plugin);
                            plugin.runtime = None;
                        }
                    }
                    Err(e) => {
                        self.log(format!("[Core] Failed to load plugin '{}': {e}", disc.slug));
                        plugin.enabled = false;
                        plugin.load_error = Some(e);
                    }
                }
            }

            self.plugins.push(plugin);
        }
    }

    pub fn reload_all(&mut self, config: &mut Config) {
        self.shared.borrow_mut().suppress_plugin_logs = true;
        self.refresh(config, false);
        self.shared.borrow_mut().suppress_plugin_logs = false;
        self.log("[Core] Reloaded Plugins");
    }

    /// enable+(re)load or disable+unload one plugin, returns success
    pub fn set_enabled(&mut self, slug: &str, enabled: bool, config: &mut Config) -> bool {
        self.update_rl_config_dir(config);
        let Some(idx) = self.plugins.iter().position(|p| p.slug == slug) else {
            return false;
        };

        if enabled {
            // Always reload fresh from disk, like the Python reload_plugin.
            Self::call_on_unload(&mut self.plugins[idx]);
            self.plugins[idx].runtime = None;

            let disc = discover_plugins(&self.plugin_dir)
                .into_iter()
                .find(|d| d.slug == slug && d.error.is_none());
            let Some(disc) = disc else {
                self.log(format!("[Console] Cannot find plugin file for '{slug}'"));
                self.plugins[idx].enabled = false;
                config.plugins.insert(slug.to_string(), false);
                return false;
            };

            self.plugins[idx].manifest = disc.manifest.clone();
            self.plugins[idx].filename = disc.filename();

            match self.instantiate(&disc) {
                Ok(runtime) => {
                    self.plugins[idx].runtime = Some(runtime);
                    self.plugins[idx].enabled = true;
                    self.plugins[idx].load_error = None;
                    if let Err(e) = Self::call_callback_on(&mut self.plugins[idx], "on_load", ()) {
                        self.log(format!("[Console] Error during plugin start: {e}"));
                        self.plugins[idx].enabled = false;
                        Self::call_on_unload(&mut self.plugins[idx]);
                        self.plugins[idx].runtime = None;
                        config.plugins.insert(slug.to_string(), false);
                        return false;
                    }
                    config.plugins.insert(slug.to_string(), true);
                    true
                }
                Err(e) => {
                    self.log(format!(
                        "[Console] {slug} failed to load (syntax error?): {e}"
                    ));
                    self.plugins[idx].enabled = false;
                    self.plugins[idx].load_error = Some(e);
                    config.plugins.insert(slug.to_string(), false);
                    false
                }
            }
        } else {
            self.plugins[idx].enabled = false;
            Self::call_on_unload(&mut self.plugins[idx]);
            self.plugins[idx].runtime = None;
            config.plugins.insert(slug.to_string(), false);
            true
        }
    }

    /// Discover and enable one freshly installed catalog plugin without
    /// unloading or reloading any other plugin.
    pub fn enable_installed_plugin(
        &mut self,
        plugin_id: &str,
        config: &mut Config,
    ) -> Result<(), String> {
        let discovered = discover_plugins(&self.plugin_dir)
            .into_iter()
            .find(|plugin| plugin.manifest.plugin_id.as_deref() == Some(plugin_id))
            .ok_or_else(|| format!("Installed plugin ID '{plugin_id}' was not discovered"))?;
        if let Some(error) = &discovered.error {
            return Err(format!(
                "Installed plugin '{}' is invalid: {error}",
                discovered.slug
            ));
        }

        let slug = discovered.slug.clone();
        if let Some(index) = self.plugins.iter().position(|plugin| {
            plugin.slug == slug || plugin.manifest.plugin_id.as_deref() == Some(plugin_id)
        }) {
            self.plugins[index].slug = slug.clone();
            self.plugins[index].manifest = discovered.manifest.clone();
            self.plugins[index].filename = discovered.filename();
            self.plugins[index].load_error = None;
        } else {
            self.plugins.push(LoadedPlugin {
                slug: slug.clone(),
                manifest: discovered.manifest.clone(),
                filename: discovered.filename(),
                enabled: false,
                load_error: None,
                runtime: None,
            });
        }

        self.set_enabled(&slug, true, config)
            .then_some(())
            .ok_or_else(|| format!("Installed plugin '{slug}' failed to enable"))
    }

    /// Unload and remove one plugin's own directory.
    pub fn delete_plugin(&mut self, slug: &str, config: &mut Config) -> Result<(), String> {
        let index = self
            .plugins
            .iter()
            .position(|plugin| plugin.slug == slug)
            .ok_or_else(|| format!("Plugin '{slug}' was not found"))?;
        let path = self.plugin_dir.join(slug);
        if !path.is_dir() {
            return Err(format!("Plugin folder {} was not found", path.display()));
        }
        let plugin = &mut self.plugins[index];
        plugin.enabled = false;
        Self::call_on_unload(plugin);
        plugin.runtime = None;
        std::fs::remove_dir_all(&path)
            .map_err(|error| format!("Could not delete {}: {error}", path.display()))?;
        self.plugins.remove(index);
        config.plugins.remove(slug);
        self.log(format!("[Core] Deleted plugin '{slug}'"));
        Ok(())
    }

    /// Refresh one plugin after its files were replaced by an auto-update.
    /// Disabled plugins remain disabled; enabled plugins are re-instantiated from disk.
    pub fn reload_updated_plugin(&mut self, slug: &str, was_enabled: bool, config: &mut Config) {
        if was_enabled {
            let _ = self.set_enabled(slug, true, config);
            return;
        }

        let Some(index) = self.plugins.iter().position(|plugin| plugin.slug == slug) else {
            return;
        };
        if let Some(discovered) = discover_plugins(&self.plugin_dir)
            .into_iter()
            .find(|plugin| plugin.slug == slug && plugin.error.is_none())
        {
            self.plugins[index].filename = discovered.filename();
            self.plugins[index].manifest = discovered.manifest;
            self.plugins[index].load_error = None;
        }
        self.plugins[index].enabled = false;
        self.plugins[index].runtime = None;
        config.plugins.insert(slug.to_string(), false);
    }
    /// Recreate enabled plugin runtimes after an actual Steam/Epic transition.
    /// This intentionally emits no success messages; plugins simply receive the
    /// updated shared platform on their next `on_load` call.
    pub fn reload_enabled_silent(&mut self, config: &mut Config) {
        self.shared.borrow_mut().suppress_plugin_logs = true;
        let enabled = self
            .plugins
            .iter()
            .filter(|plugin| plugin.enabled)
            .map(|plugin| plugin.slug.clone())
            .collect::<Vec<_>>();
        for slug in enabled {
            let _ = self.set_enabled(&slug, true, config);
        }
        self.shared.borrow_mut().suppress_plugin_logs = false;
    }

    fn update_rl_config_dir(&self, _config: &Config) {
        self.shared.borrow_mut().rl_config_dir =
            hebnix_sdk::utils::system_settings::find_system_settings()
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default();
    }

    fn instantiate(&self, disc: &DiscoveredPlugin) -> Result<PluginRuntime, String> {
        let lua = Lua::new();

        // expand + canonicalize the plugin's declared read roots; a root that
        // doesn't resolve is simply dropped (reads under it then return nil).
        let read_roots = disc
            .manifest
            .permissions
            .read_roots
            .iter()
            .map(|r| expand_env_vars(r))
            .filter_map(|p| std::path::Path::new(&p).canonicalize().ok())
            .collect::<Vec<_>>();

        let host = Rc::new(HostCtx {
            slug: disc.slug.clone(),
            display_name: RefCell::new(disc.manifest.name.clone()),
            tx: self.tx.clone(),
            store: Rc::new(RefCell::new(PluginStore::load(&self.plugin_dir, &disc.slug))),
            window: RefCell::new(WindowState::default()),
            shared: Rc::clone(&self.shared),
            text_bufs: RefCell::new(Default::default()),
            dir: self.plugin_dir.join(&disc.slug),
            assets: RefCell::new(Default::default()),
            captures: Default::default(),
            read_roots,
        });

        lua_api::install_api(&lua, Rc::clone(&host)).map_err(|e| e.to_string())?;

        // Make require resolve files inside the plugin's own directory.
        if let Some(dir) = disc.entry_path.parent() {
            let dir_str = dir.to_string_lossy().replace('\\', "/");
            let code =
                format!("package.path = \"{dir_str}/?.lua;{dir_str}/?/init.lua;\" .. package.path");
            lua.load(&code).exec().map_err(|e| e.to_string())?;
        }

        let source = std::fs::read_to_string(&disc.entry_path).map_err(|e| e.to_string())?;
        let result: LuaValue = lua
            .load(&source)
            .set_name(format!("@{}", disc.filename()))
            .eval()
            .map_err(|e| e.to_string())?;

        // The script returns the plugin table, or defines a global `plugin`.
        let table: Table = match result {
            LuaValue::Table(t) => t,
            _ => lua
                .globals()
                .get::<Table>("plugin")
                .map_err(|_| "plugin script must return a table of callbacks".to_string())?,
        };

        let plugin_table = lua
            .create_registry_value(table)
            .map_err(|e| e.to_string())?;

        Ok(PluginRuntime {
            lua,
            plugin_table,
            host,
        })
    }

    fn call_on_unload(plugin: &mut LoadedPlugin) {
        if let Some(rt) = &plugin.runtime {
            let pos = {
                let mut win = rt.host.window.borrow_mut();
                win.open = false;
                win.pos_dirty.then(|| win.last_pos).flatten()
            };
            if let Some((x, y)) = pos {
                lua_api::persist_window_pos(&rt.host, x, y);
                rt.host.window.borrow_mut().pos_dirty = false;
            }
        }
        let _ = Self::call_callback_on(plugin, "on_unload", ());
        if let Some(runtime) = &plugin.runtime {
            runtime
                .host
                .shared
                .borrow_mut()
                .cvars
                .unregister_owner(&plugin.slug);
        }
    }

    pub fn execute_cvar_command(&self, raw: &str) -> String {
        match parse_console_command(raw) {
            Ok(ConsoleCvarCommand::Get { name }) => match self.shared.borrow().cvars.get(&name) {
                None => format!("[Console] Cvar '{name}' has not been registered by a plugin."),
                Some(None) => format!("[Console] {name} is unset."),
                Some(Some(value)) => format!("[Console] {name} = {value}"),
            },
            Ok(ConsoleCvarCommand::Set { name, value }) => {
                let display = value.to_string();
                match self.shared.borrow_mut().cvars.set(&name, value) {
                    Ok(()) => format!("[Console] {name} = {display}"),
                    Err(error) => format!("[Console] {error}."),
                }
            }
            Err(error) => format!("[Console] Invalid cvar command: {error}."),
        }
    }

    fn call_callback_on(
        plugin: &mut LoadedPlugin,
        name: &str,
        args: impl mlua::IntoLuaMulti,
    ) -> Result<(), String> {
        let Some(rt) = &plugin.runtime else {
            return Ok(());
        };
        let table: Table = rt
            .lua
            .registry_value(&rt.plugin_table)
            .map_err(|e| e.to_string())?;
        let func: LuaValue = table.get(name).map_err(|e| e.to_string())?;
        if let LuaValue::Function(f) = func {
            f.call::<()>(args).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// dispatch a game event to every enabled plugin. one that errors gets
    /// force-disabled.
    pub fn dispatch_game_event(&mut self, event: &StatsEvent) {
        self.dispatch_named(&event.event_type.clone(), |lua| {
            let t = lua.create_table()?;
            t.set("event_type", event.event_type.clone())?;
            if let Some(guid) = &event.match_guid {
                t.set("match_guid", guid.clone())?;
            }
            t.set("data", lua_api::to_lua(lua, &event.raw_data)?)?;
            Ok(LuaValue::Table(t))
        });
    }

    /// dispatch a synthetic event (e.g. "GameLeft", "GuiVisibility")
    pub fn dispatch_simple(&mut self, event_type: &str, payload: serde_json::Value) {
        self.dispatch_named(event_type, |lua| {
            let t = lua.create_table()?;
            t.set("event_type", event_type)?;
            t.set("data", lua_api::to_lua(lua, &payload)?)?;
            Ok(LuaValue::Table(t))
        });
    }

    fn dispatch_named(
        &mut self,
        event_type: &str,
        make_payload: impl Fn(&Lua) -> mlua::Result<LuaValue>,
    ) {
        let mut crashed: Vec<(usize, String, String)> = Vec::new();

        for (idx, plugin) in self.plugins.iter_mut().enumerate() {
            if !plugin.enabled {
                continue;
            }
            let Some(rt) = &plugin.runtime else {
                continue;
            };
            let call = (|| -> mlua::Result<()> {
                let table: Table = rt.lua.registry_value(&rt.plugin_table)?;
                let func: LuaValue = table.get("on_game_event")?;
                if let LuaValue::Function(f) = func {
                    let payload = make_payload(&rt.lua)?;
                    f.call::<()>((event_type, payload))?;
                }
                Ok(())
            })();
            if let Err(e) = call {
                crashed.push((idx, plugin.display_name().to_string(), e.to_string()));
            }
        }

        for (idx, name, err) in crashed {
            self.log(format!(
                "[Core] Critical Error in '{name}': {err}. Force disabling."
            ));
            self.plugins[idx].enabled = false;
            Self::call_on_unload(&mut self.plugins[idx]);
            self.plugins[idx].runtime = None;
        }
    }

    /// Dispatch plugin ticks no faster than `minimum_interval`. UI input can
    /// generate frames much faster than the requested repaint rate, so this
    /// keeps mouse movement from multiplying plugin work.
    pub fn dispatch_tick_if_due(&mut self, minimum_interval: std::time::Duration) {
        let now = std::time::Instant::now();
        if now.duration_since(self.last_tick_dispatch) < minimum_interval {
            return;
        }
        self.last_tick_dispatch = now;
        self.dispatch_tick();
    }

    /// Call on_tick on every enabled plugin.
    pub fn dispatch_tick(&mut self) {
        let mut crashed: Vec<(usize, String, String)> = Vec::new();

        for (idx, plugin) in self.plugins.iter_mut().enumerate() {
            if !plugin.enabled {
                continue;
            }
            let Some(rt) = &plugin.runtime else {
                continue;
            };
            let call = (|| -> mlua::Result<()> {
                let table: Table = rt.lua.registry_value(&rt.plugin_table)?;
                let func: LuaValue = table.get("on_tick")?;
                if let LuaValue::Function(f) = func {
                    f.call::<()>(())?;
                }
                Ok(())
            })();
            if let Err(e) = call {
                crashed.push((idx, plugin.display_name().to_string(), e.to_string()));
            }
        }

        for (idx, name, err) in crashed {
            self.log(format!(
                "[Core] Critical Error in '{name}' on_tick: {err}. Force disabling."
            ));
            self.plugins[idx].enabled = false;
            Self::call_on_unload(&mut self.plugins[idx]);
            self.plugins[idx].runtime = None;
        }
    }

    /// does any enabled plugin define on_tick (app repaints faster when true so
    /// binds feel responsive)
    pub fn has_tick_plugins(&self) -> bool {
        self.plugins.iter().any(|p| {
            p.enabled
                && p.runtime
                    .as_ref()
                    .map(|rt| {
                        rt.lua
                            .registry_value::<Table>(&rt.plugin_table)
                            .and_then(|t| t.get::<LuaValue>("on_tick"))
                            .map(|v| v.is_function())
                            .unwrap_or(false)
                    })
                    .unwrap_or(false)
        })
    }

    /// tell plugins the main gui was shown/hidden
    pub fn dispatch_gui_visibility(&mut self, is_open: bool) {
        self.shared.borrow_mut().is_gui_open = is_open;
        self.dispatch_simple("GuiVisibility", serde_json::json!({ "is_open": is_open }));
    }

    /// unload every enabled plugin (connection lost / app exit)
    pub fn unload_all(&mut self) {
        for plugin in &mut self.plugins {
            if plugin.enabled {
                Self::call_on_unload(plugin);
            }
        }
    }

    /// goes to the requesting plugin only
    pub fn on_http_response(&mut self, slug: &str, req_id: &str, status: u16, body: &str) {
        let Some(idx) = self.plugins.iter().position(|p| p.slug == slug) else {
            return;
        };
        let plugin = &self.plugins[idx];
        if !plugin.enabled {
            return;
        }
        let Some(rt) = &plugin.runtime else {
            return;
        };
        let name = plugin.display_name().to_string();

        let call = (|| -> mlua::Result<()> {
            let table: Table = rt.lua.registry_value(&rt.plugin_table)?;
            let func: LuaValue = table.get("on_http_response")?;
            if let LuaValue::Function(f) = func {
                f.call::<()>((req_id, status, body))?;
            }
            Ok(())
        })();

        if let Err(e) = call {
            self.log(format!(
                "[Core] Critical Error in '{name}' on_http_response: {e}. Force disabling."
            ));
            self.plugins[idx].enabled = false;
            Self::call_on_unload(&mut self.plugins[idx]);
            self.plugins[idx].runtime = None;
        }
    }

    /// goes to the requesting plugin only. Byte-safe counterpart of
    /// on_http_response for http_download_async — body is passed as a raw
    /// Lua string (mlua strings are 8-bit clean) instead of a Rust `&str`,
    /// so binary responses like avatar images survive intact.
    pub fn on_http_download_response(
        &mut self,
        slug: &str,
        req_id: &str,
        status: u16,
        body: &[u8],
    ) {
        let Some(idx) = self.plugins.iter().position(|p| p.slug == slug) else {
            return;
        };
        let plugin = &self.plugins[idx];
        if !plugin.enabled {
            return;
        }
        let Some(rt) = &plugin.runtime else {
            return;
        };
        let name = plugin.display_name().to_string();

        let call = (|| -> mlua::Result<()> {
            let table: Table = rt.lua.registry_value(&rt.plugin_table)?;
            let func: LuaValue = table.get("on_http_download_response")?;
            if let LuaValue::Function(f) = func {
                let lua_body = rt.lua.create_string(body)?;
                f.call::<()>((req_id, status, lua_body))?;
            }
            Ok(())
        })();

        if let Err(e) = call {
            self.log(format!(
                "[Core] Critical Error in '{name}' on_http_download_response: {e}. Force disabling."
            ));
            self.plugins[idx].enabled = false;
            Self::call_on_unload(&mut self.plugins[idx]);
            self.plugins[idx].runtime = None;
        }
    }

    /// goes to the requesting plugin only. Result of http_get_no_redirect_async
    /// — location is the response's Location header (empty if none).
    pub fn on_http_redirect_response(
        &mut self,
        slug: &str,
        req_id: &str,
        status: u16,
        location: &str,
    ) {
        let Some(idx) = self.plugins.iter().position(|p| p.slug == slug) else {
            return;
        };
        let plugin = &self.plugins[idx];
        if !plugin.enabled {
            return;
        }
        let Some(rt) = &plugin.runtime else {
            return;
        };
        let name = plugin.display_name().to_string();

        let call = (|| -> mlua::Result<()> {
            let table: Table = rt.lua.registry_value(&rt.plugin_table)?;
            let func: LuaValue = table.get("on_http_redirect_response")?;
            if let LuaValue::Function(f) = func {
                f.call::<()>((req_id, status, location))?;
            }
            Ok(())
        })();

        if let Err(e) = call {
            self.log(format!(
                "[Core] Critical Error in '{name}' on_http_redirect_response: {e}. Force disabling."
            ));
            self.plugins[idx].enabled = false;
            Self::call_on_unload(&mut self.plugins[idx]);
            self.plugins[idx].runtime = None;
        }
    }

    /// goes to the requesting plugin only. Result of http_multipart_post_async.
    pub fn on_http_upload_response(&mut self, slug: &str, req_id: &str, status: u16, body: &str) {
        let Some(idx) = self.plugins.iter().position(|p| p.slug == slug) else {
            return;
        };
        let plugin = &self.plugins[idx];
        if !plugin.enabled {
            return;
        }
        let Some(rt) = &plugin.runtime else {
            return;
        };
        let name = plugin.display_name().to_string();

        let call = (|| -> mlua::Result<()> {
            let table: Table = rt.lua.registry_value(&rt.plugin_table)?;
            let func: LuaValue = table.get("on_http_upload_response")?;
            if let LuaValue::Function(f) = func {
                f.call::<()>((req_id, status, body))?;
            }
            Ok(())
        })();

        if let Err(e) = call {
            self.log(format!(
                "[Core] Critical Error in '{name}' on_http_upload_response: {e}. Force disabling."
            ));
            self.plugins[idx].enabled = false;
            Self::call_on_unload(&mut self.plugins[idx]);
            self.plugins[idx].runtime = None;
        }
    }

    /// dispatch a websocket callback (on_ws_open/on_ws_message/on_ws_close) to
    /// the plugin that opened the connection.
    fn on_ws_event(&mut self, slug: &str, cb: &str, id: &str, extra: Option<&str>) {
        let Some(idx) = self.plugins.iter().position(|p| p.slug == slug) else {
            return;
        };
        let plugin = &self.plugins[idx];
        if !plugin.enabled {
            return;
        }
        let Some(rt) = &plugin.runtime else {
            return;
        };
        let name = plugin.display_name().to_string();

        let call = (|| -> mlua::Result<()> {
            let table: Table = rt.lua.registry_value(&rt.plugin_table)?;
            let func: LuaValue = table.get(cb)?;
            if let LuaValue::Function(f) = func {
                match extra {
                    Some(e) => f.call::<()>((id, e))?,
                    None => f.call::<()>(id)?,
                }
            }
            Ok(())
        })();

        if let Err(e) = call {
            self.log(format!(
                "[Core] Critical Error in '{name}' {cb}: {e}. Force disabling."
            ));
            self.plugins[idx].enabled = false;
            Self::call_on_unload(&mut self.plugins[idx]);
            self.plugins[idx].runtime = None;
        }
    }

    /// generic http result including response headers (JSON object string).
    /// body is raw bytes, handed to Lua as a byte-safe string.
    pub fn on_http_result(
        &mut self,
        slug: &str,
        req_id: &str,
        status: u16,
        body: &[u8],
        headers: &str,
    ) {
        let Some(idx) = self.plugins.iter().position(|p| p.slug == slug) else {
            return;
        };
        let plugin = &self.plugins[idx];
        if !plugin.enabled {
            return;
        }
        let Some(rt) = &plugin.runtime else {
            return;
        };
        let name = plugin.display_name().to_string();

        let call = (|| -> mlua::Result<()> {
            let table: Table = rt.lua.registry_value(&rt.plugin_table)?;
            let func: LuaValue = table.get("on_http_result")?;
            if let LuaValue::Function(f) = func {
                let lua_body = rt.lua.create_string(body)?;
                f.call::<()>((req_id, status, lua_body, headers))?;
            }
            Ok(())
        })();

        if let Err(e) = call {
            self.log(format!(
                "[Core] Critical Error in '{name}' on_http_result: {e}. Force disabling."
            ));
            self.plugins[idx].enabled = false;
            Self::call_on_unload(&mut self.plugins[idx]);
            self.plugins[idx].runtime = None;
        }
    }

    pub fn on_ws_open(&mut self, slug: &str, id: &str) {
        self.on_ws_event(slug, "on_ws_open", id, None);
    }

    pub fn on_ws_message(&mut self, slug: &str, id: &str, data: &str) {
        self.on_ws_event(slug, "on_ws_message", id, Some(data));
    }

    pub fn on_ws_close(&mut self, slug: &str, id: &str, reason: &str) {
        self.on_ws_event(slug, "on_ws_close", id, Some(reason));
    }

    /// render a plugin's settings ui. Err(msg) if the callback raised, so the
    /// caller can log + disable.
    pub fn render_settings(&mut self, slug: &str, ui: &mut egui::Ui) -> Result<(), String> {
        let Some(plugin) = self.plugins.iter().find(|p| p.slug == slug) else {
            return Ok(());
        };
        let Some(rt) = &plugin.runtime else {
            return Ok(());
        };
        let table: Table = rt
            .lua
            .registry_value(&rt.plugin_table)
            .map_err(|e| e.to_string())?;
        let func: LuaValue = table.get("on_settings").map_err(|e| e.to_string())?;
        let LuaValue::Function(f) = func else {
            return Ok(());
        };
        let ui_tbl = lua_api::ui_table(&rt.lua).map_err(|e| e.to_string())?;
        lua_api::with_ui_scope(ui, || f.call::<()>(ui_tbl).map_err(|e| e.to_string()))
    }

    /// render a plugin's floating-window contents
    pub fn render_window(&mut self, slug: &str, ui: &mut egui::Ui) -> Result<(), String> {
        let Some(plugin) = self.plugins.iter().find(|p| p.slug == slug) else {
            return Ok(());
        };
        let Some(rt) = &plugin.runtime else {
            return Ok(());
        };
        let table: Table = rt
            .lua
            .registry_value(&rt.plugin_table)
            .map_err(|e| e.to_string())?;
        let func: LuaValue = table.get("on_window").map_err(|e| e.to_string())?;
        let LuaValue::Function(f) = func else {
            return Ok(());
        };
        let ui_tbl = lua_api::ui_table(&rt.lua).map_err(|e| e.to_string())?;
        lua_api::with_ui_scope(ui, || f.call::<()>(ui_tbl).map_err(|e| e.to_string()))
    }

    /// slugs of enabled plugins that define on_overlay
    pub fn overlay_plugins(&self) -> Vec<String> {
        self.plugins
            .iter()
            .filter(|p| {
                p.enabled
                    && p.runtime
                        .as_ref()
                        .map(|rt| {
                            rt.lua
                                .registry_value::<Table>(&rt.plugin_table)
                                .and_then(|t| t.get::<LuaValue>("on_overlay"))
                                .map(|v| v.is_function())
                                .unwrap_or(false)
                        })
                        .unwrap_or(false)
            })
            .map(|p| p.slug.clone())
            .collect()
    }

    /// enabled plugins that touch the overlay, with the dir they serve from.
    /// page is None for draw-only, which still needs it for draw.image.
    pub fn overlay_page_plugins(&self) -> Vec<(String, Option<String>, PathBuf, bool)> {
        self.plugins
            .iter()
            .filter(|p| p.enabled)
            .filter_map(|p| {
                let rt = p.runtime.as_ref()?;
                let table = rt.lua.registry_value::<Table>(&rt.plugin_table).ok()?;
                let draws = table
                    .get::<LuaValue>("on_overlay")
                    .map(|v| v.is_function())
                    .unwrap_or(false);
                let page = table
                    .get::<String>("overlay_page")
                    .ok()
                    .map(|page| page.trim().trim_start_matches('/').to_string())
                    .filter(|page| !page.is_empty() && !page.contains(".."));
                if !draws && page.is_none() {
                    return None;
                }
                let clickable = page.is_some() && table.get::<bool>("clickable").unwrap_or(false);
                let assets = self.plugin_dir.join(&p.slug).join("assets");
                Some((p.slug.clone(), page, assets, clickable))
            })
            .collect()
    }

    /// stacking order, bottom first. unlisted slugs go on top.
    pub fn overlay_layers(&self, order: &[String]) -> Vec<(String, Option<String>, PathBuf, bool)> {
        let mut layers = self.overlay_page_plugins();
        layers.sort_by_key(|(slug, _, _, _)| {
            order
                .iter()
                .position(|ordered| ordered == slug)
                .unwrap_or(usize::MAX)
        });
        layers
    }

    /// run a plugin's on_overlay(draw, w, h). the canvas must already be the
    /// current draw target (set by overlay::frame), the draw table paints on it.
    pub fn render_overlay_gdi(&mut self, slug: &str, w: f32, h: f32) -> Result<(), String> {
        let Some(plugin) = self.plugins.iter().find(|p| p.slug == slug) else {
            return Ok(());
        };
        let Some(rt) = &plugin.runtime else {
            return Ok(());
        };
        let table: Table = rt
            .lua
            .registry_value(&rt.plugin_table)
            .map_err(|e| e.to_string())?;
        let func: LuaValue = table.get("on_overlay").map_err(|e| e.to_string())?;
        let LuaValue::Function(f) = func else {
            return Ok(());
        };
        let draw_tbl = lua_api::draw_table(&rt.lua).map_err(|e| e.to_string())?;
        f.call::<()>((draw_tbl, w, h)).map_err(|e| e.to_string())
    }

    /// note where a plugin window ended up. not written back into the viewport
    /// builder, see WindowState::pos.
    pub fn close_window(&self, slug: &str) {
        if let Some(runtime) = self
            .plugins
            .iter()
            .find(|plugin| plugin.slug == slug)
            .and_then(|plugin| plugin.runtime.as_ref())
        {
            runtime.host.window.borrow_mut().open = false;
        }
    }
    pub fn set_window_pos(&self, slug: &str, x: f32, y: f32) {
        if let Some(rt) = self
            .plugins
            .iter()
            .find(|p| p.slug == slug)
            .and_then(|p| p.runtime.as_ref())
        {
            let mut win = rt.host.window.borrow_mut();
            win.last_pos = Some((x, y));
            win.pos_dirty = true;
        }
    }

    /// write moved window positions to disk. throttled, a drag moves the window
    /// every frame and each save is two file writes.
    pub fn flush_window_positions(&mut self) {
        if self.last_pos_flush.elapsed() < POS_FLUSH_INTERVAL {
            return;
        }
        self.last_pos_flush = std::time::Instant::now();
        self.flush_window_positions_now();
    }

    fn flush_window_positions_now(&self) {
        for plugin in &self.plugins {
            let Some(rt) = &plugin.runtime else { continue };
            let (dirty, pos) = {
                let win = rt.host.window.borrow();
                (win.pos_dirty, win.last_pos)
            };
            if !dirty {
                continue;
            }
            if let Some((x, y)) = pos {
                lua_api::persist_window_pos(&rt.host, x, y);
            }
            rt.host.window.borrow_mut().pos_dirty = false;
        }
    }
}
