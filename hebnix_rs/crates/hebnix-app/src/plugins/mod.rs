//! lua plugin system.
//!
//! plugins live in plugins/ as a dir <slug>/ with a plugin.toml and the lua
//! file it names. a plugin returns a table of callbacks (on_load, on_unload,
//! on_game_event, on_settings(ui), on_window(ui), on_overlay(draw,w,h)).
//! see examples/plugins/.

pub mod gamepad_icons;
pub mod lua_api;
pub mod manager;
pub mod manifest;
pub mod store;

pub use manager::PluginManager;
