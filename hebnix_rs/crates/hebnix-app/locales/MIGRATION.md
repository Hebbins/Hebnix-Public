# Localization migration status

Translated through `t()` / `t_args()` / `t_n()`:

- all main tabs: Console, Maps (browse, import, background changer, multiplayer),
  Spoofer, Items (swapper, spawner, ball/boost/decal/car patchers, presets),
  Colours, Settings, Plugins, Experimental (RLAPI, ball appearance, car patcher,
  wheel alignment), About
- language picker, tray menu, update/changelog dialogs, all prompts and notices
- Discord Rich Presence texts
- the lite app (tabs, settings, dialogs)
- plural strings in the migrated screens

`Tab::config_key()` keeps `default_tab` stored as English names so old configs load.

## Still english-only

- error and log text built with `format!` that carries values: `[Console]`-style
  log lines, `Err(...)` strings from the patchers, `update.rs` download errors,
  parts of `rl_launch.rs` / `epic_connection.rs`
- a few number-only or id-only formats (`{}.`, `({kind})`, `{name} v{version}`)
- dynamic text built from several pieces, e.g. the tunnel status word in the
  multiplayer panel
- game vocabulary on purpose: ranks, playlists, arena and map names
- the Lua plugins' own UI text (a `hebnix.t()` host API is a possible follow-up)

Never translate (see `README.md`): log parser regexes, StatsAPI/RL API/EOS JSON,
spoofer rules, server changelog tags (`[Added]`...), `[Console]`-style log tags,
serde enum values, JSON keys, paths, registry and process names, Discord IPC keys.

## How the bulk migration was done

String literals passed to egui widgets were moved to keys by script, keys are
`<function or file>-<first words of the text>`. They are long but stable, rename
one only together with its `.ftl` entries.
