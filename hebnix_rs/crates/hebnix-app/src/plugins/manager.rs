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
use crate::plugins::lua_api::{self, HostCtx, HostShared, WindowState};
use crate::plugins::manifest::{DiscoveredPlugin, PluginManifest, discover_plugins};
use crate::plugins::store::PluginStore;

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
                app_version: app_version.to_string(),
                platform: String::new(),
            })),
            last_pos_flush: std::time::Instant::now(),
        }
    }

    fn log(&self, msg: impl Into<String>) {
        let _ = self.tx.send(AppMsg::Log(msg.into()));
    }

    /// full refresh: unload all, re-discover, load the enabled ones
    pub fn refresh(&mut self, config: &mut Config, verbose: bool) {
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
            } else if verbose {
                self.log(format!(
                    "[Core] Skipped Loading Plugin: {} [Disabled]",
                    plugin.display_name()
                ));
            }

            self.plugins.push(plugin);
        }
    }

    /// enable+(re)load or disable+unload one plugin, returns success
    pub fn set_enabled(&mut self, slug: &str, enabled: bool, config: &mut Config) -> bool {
        let Some(idx) = self.plugins.iter().position(|p| p.slug == slug) else {
            return false;
        };

        if enabled {
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

    /// Recreate enabled plugin runtimes after an actual Steam/Epic transition.
    /// This intentionally emits no success messages; plugins simply receive the
    /// updated shared platform on their next `on_load` call.
    pub fn reload_enabled_silent(&mut self, config: &mut Config) {
        let enabled = self
            .plugins
            .iter()
            .filter(|plugin| plugin.enabled)
            .map(|plugin| plugin.slug.clone())
            .collect::<Vec<_>>();
        for slug in enabled {
            let _ = self.set_enabled(&slug, true, config);
        }
    }

    fn instantiate(&self, disc: &DiscoveredPlugin) -> Result<PluginRuntime, String> {
        let lua = Lua::new();

        let host = Rc::new(HostCtx {
            slug: disc.slug.clone(),
            display_name: RefCell::new(disc.manifest.name.clone()),
            tx: self.tx.clone(),
            store: RefCell::new(PluginStore::load(&self.plugin_dir, &disc.slug)),
            window: RefCell::new(WindowState::default()),
            shared: Rc::clone(&self.shared),
            text_bufs: RefCell::new(Default::default()),
            dir: self.plugin_dir.join(&disc.slug),
            assets: RefCell::new(Default::default()),
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

    /// call on_tick on every enabled plugin, once per ui frame. keep em cheap.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn example_plugins_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("examples")
            .join("plugins")
    }

    #[test]
    fn loads_example_plugin_and_dispatches_events() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut mgr = PluginManager::new(example_plugins_dir(), tx, "test");
        let mut config = Config::default();
        config.plugins.insert("goal_tracker".to_string(), true);

        mgr.refresh(&mut config, true);

        let plugin = mgr
            .plugins
            .iter()
            .find(|p| p.slug == "goal_tracker")
            .expect("goal_tracker discovered");
        assert!(plugin.enabled, "plugin should be enabled");
        assert!(plugin.runtime.is_some(), "plugin should have a Lua runtime");
        assert_eq!(plugin.display_name(), "Goal Tracker");
        assert!(plugin.has_settings());

        mgr.dispatch_simple(
            "GoalScored",
            serde_json::json!({ "Scorer": { "Name": "TestPlayer" } }),
        );

        let mut saw_goal_log = false;
        while let Ok(msg) = rx.try_recv() {
            if let AppMsg::Log(line) = msg {
                if line.contains("GOAL by TestPlayer") {
                    saw_goal_log = true;
                }
                assert!(
                    !line.contains("Critical Error"),
                    "plugin crashed during dispatch: {line}"
                );
            }
        }
        assert!(saw_goal_log, "expected the plugin to log the goal");

        // Unload cleanly.
        mgr.unload_all();
    }

    #[test]
    fn loads_overlay_demo_plugin() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut mgr = PluginManager::new(example_plugins_dir(), tx, "test");
        let mut config = Config::default();
        config.plugins.insert("overlay_demo".to_string(), true);

        mgr.refresh(&mut config, true);
        let plugin = mgr
            .plugins
            .iter()
            .find(|p| p.slug == "overlay_demo")
            .expect("overlay_demo discovered");
        assert!(plugin.enabled && plugin.runtime.is_some());
        assert_eq!(mgr.overlay_plugins(), vec!["overlay_demo".to_string()]);

        // Feed a couple of events; on_overlay is exercised by the app with a
        // real Ui, so here we just confirm events don't crash the plugin.
        mgr.dispatch_simple(
            "GoalScored",
            serde_json::json!({ "Scorer": { "Name": "P", "TeamNum": 1 } }),
        );
        mgr.dispatch_simple(
            "ClockUpdatedSeconds",
            serde_json::json!({ "TimeSeconds": 90 }),
        );
        while let Ok(msg) = rx.try_recv() {
            if let AppMsg::Log(line) = msg {
                assert!(!line.contains("Critical Error"), "plugin crashed: {line}");
            }
        }
        mgr.unload_all();
    }

    #[test]
    fn loads_rlapi_demo_plugin() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut mgr = PluginManager::new(example_plugins_dir(), tx, "test");
        let mut config = Config::default();
        config.plugins.insert("rlapi_demo".to_string(), true);

        mgr.refresh(&mut config, true);
        let plugin = mgr
            .plugins
            .iter()
            .find(|p| p.slug == "rlapi_demo")
            .expect("rlapi_demo discovered");
        assert!(plugin.enabled && plugin.runtime.is_some());

        // on_load fires eos/rlapi requests; on_tick polls them. Neither should
        // crash the plugin even though no result arrives during the test.
        mgr.dispatch_tick();
        while let Ok(msg) = rx.try_recv() {
            if let AppMsg::Log(line) = msg {
                assert!(!line.contains("Critical Error"), "plugin crashed: {line}");
            }
        }
        mgr.unload_all();
    }

    #[test]
    fn loads_test_plugin() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut mgr = PluginManager::new(example_plugins_dir(), tx, "test");
        let mut config = Config::default();
        config.plugins.insert("test_plugin".to_string(), true);

        mgr.refresh(&mut config, true);

        let plugin = mgr
            .plugins
            .iter()
            .find(|p| p.slug == "test_plugin")
            .expect("test_plugin discovered");
        assert!(plugin.enabled && plugin.runtime.is_some());

        mgr.dispatch_simple(
            "GoalScored",
            serde_json::json!({ "Scorer": { "Name": "T" }, "GoalSpeed": 88.0 }),
        );
        mgr.dispatch_simple("MatchEnded", serde_json::json!({ "WinnerTeamNum": 0 }));
        mgr.dispatch_simple("GameLeft", serde_json::json!({ "reason": "test" }));
        mgr.dispatch_tick();

        while let Ok(msg) = rx.try_recv() {
            if let AppMsg::Log(line) = msg {
                assert!(!line.contains("Critical Error"), "plugin crashed: {line}");
            }
        }
        mgr.unload_all();
    }

    #[test]
    fn http_response_only_reaches_the_requesting_plugin() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut mgr = PluginManager::new(example_plugins_dir(), tx, "test");
        let mut config = Config::default();
        config.plugins.insert("test_plugin".to_string(), true);
        config.plugins.insert("goal_tracker".to_string(), true);
        mgr.refresh(&mut config, true);
        while rx.try_recv().is_ok() {}

        let drain = |rx: &crossbeam_channel::Receiver<AppMsg>| {
            let mut lines = Vec::new();
            while let Ok(msg) = rx.try_recv() {
                if let AppMsg::Log(line) = msg {
                    lines.push(line);
                }
            }
            lines
        };

        mgr.on_http_response("test_plugin", "test_ping", 200, "hello");
        let lines = drain(&rx);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("status 200") && l.contains("5 bytes")),
            "requesting plugin should get its response: {lines:?}"
        );

        // goal_tracker has no on_http_response, must be a no-op
        mgr.on_http_response("goal_tracker", "test_ping", 500, "x");
        let lines = drain(&rx);
        assert!(
            !lines.iter().any(|l| l.contains("status 500")),
            "response reached the wrong plugin: {lines:?}"
        );

        mgr.on_http_response("does_not_exist", "test_ping", 500, "x");
        mgr.unload_all();
    }

    #[test]
    fn loads_ingame_rank_plugin() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut mgr = PluginManager::new(example_plugins_dir(), tx, "test");
        let mut config = Config::default();
        config.plugins.insert("ingame_rank".to_string(), true);

        mgr.refresh(&mut config, true);

        let plugin = mgr
            .plugins
            .iter()
            .find(|p| p.slug == "ingame_rank")
            .expect("ingame_rank discovered");
        assert!(plugin.enabled, "plugin should be enabled");
        assert!(plugin.runtime.is_some(), "plugin should have a Lua runtime");
        assert!(plugin.has_settings());
        assert!(mgr.has_tick_plugins());

        // Bots must not be queued for tracker fetches; this must not crash.
        mgr.dispatch_simple(
            "UpdateState",
            serde_json::json!({
                "Players": [ { "Name": "Bot", "PrimaryId": "unknown" } ]
            }),
        );
        mgr.dispatch_tick();
        mgr.dispatch_simple("GameLeft", serde_json::json!({}));
        mgr.dispatch_tick();

        while let Ok(msg) = rx.try_recv() {
            if let AppMsg::Log(line) = msg {
                assert!(!line.contains("Critical Error"), "plugin crashed: {line}");
            }
        }
        mgr.unload_all();
    }
}
