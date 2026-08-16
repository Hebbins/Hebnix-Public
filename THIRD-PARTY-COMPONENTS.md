# Third-party components

Binaries that ship with to `hebnix-app.exe`. Not ours, not covered by Hebnix's
licence.

## curl-impersonate.exe

From lexiforest/curl-impersonate v1.5.6, `bin/` inside

    https://github.com/lexiforest/curl-impersonate/releases/download/v1.5.6/libcurl-impersonate-v1.5.6.x86_64-win32.tar.gz

sha256 `0b4e5552a818190dc1fd8bc89a4e78ea45df5546c69af8e935c791621bed66f5`


## cacert.pem

CA bundle, BoringSSL can't read the Windows cert store. MPL-2.0, Mozilla's CA
store as published by curl.

    https://curl.se/ca/cacert-2026-05-14.pem

sha256 `86a1f3366afac7c6f8ae9f3c779ac221129328c43f0ab2b8817eb2f362a5025c`, CRLF via `.gitattributes`.

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
