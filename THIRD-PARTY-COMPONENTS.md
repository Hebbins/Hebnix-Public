# Third-party components

Binaries that ship with to `hebnix.exe`. Not ours, not covered by Hebnix's
licence.

## steam_api64.dll

Valve's Steamworks SDK, <https://partner.steamgames.com/downloads/list>

Proprietary, Steamworks SDK Access Agreement, not Hebnix's licence.

## egui-winit

`hebnix_rs/patches/egui-winit` copy of egui-winit 0.35.0 from
crates.io with one addition, marked "hebnix patch" this allows for transparent overlay to work properley.
The published crate carries no licence files, but they can be found here <https://github.com/emilk/egui>.


## notice for rlapi-bridge

Go deps are fetched at build time, nothing third-party is committed. The built
exe contains dank/rlapi (MIT) and gorilla/websocket (BSD-2-Clause).
