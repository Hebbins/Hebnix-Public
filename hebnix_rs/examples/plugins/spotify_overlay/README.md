# Spotify Overlay

An overlay card that shows the song you're currently playing on Spotify — cover
art, title, album, play/pause state and a live progress bar — drawn over Rocket
League while the game is focused.

**Self-contained: pure Lua, no external process.** It speaks Spotify's own
protocol directly using host primitives (see below).

## How it works

The Spotify desktop app stores a DPoP-bound refresh token + P-256 key locally
(`%LOCALAPPDATA%\Spotify\dbrts`). The plugin:

1. reads that login (`hebnix.read_file`, scoped by this plugin's manifest),
2. refreshes an access token — DPoP proof signed with `hebnix.p256_sign`
   (ES256, RFC 9449), incl. the `DPoP-Nonce` retry,
3. mints a `client-token` (protobuf) — `hebnix.http_request_async`,
4. opens Spotify's dealer WebSocket (`hebnix.ws_connect_async`) for a
   connection id,
5. reads the Spotify Connect cluster (`connect-state`) every ~2s for the current
   track / position / cover,
6. downloads cover art into `assets/temp/` (`hebnix.http_download_async` +
   `hebnix.write_asset`).

`assets/temp/` is owned by the plugin and cleared on load, on unload, and on
every song change (`hebnix.clear_asset_dir`).

Nothing is sent anywhere except Spotify's own servers; the token lives only in
memory.

## Permissions

Declared in `plugin.toml` and enforced by the host:

```toml
[permissions]
read_roots = ["%LOCALAPPDATA%/Spotify"]
```

`hebnix.read_file` will only read files under a declared root — this plugin can
read Spotify's local data and nothing else.

## Setup

1. Have the **Spotify desktop app installed and logged in** on this PC (the
   plugin reads its local refresh token — never your password).
2. Enable **Spotify Overlay** in hebnix, focus Rocket League, and play
   something. The card appears within a few seconds.

## Settings

- **Show overlay card** — master toggle
- **Show cover art** — hide the art for a text-only card
- **Position** — which screen corner
- **Scale** — 60–160 %

The settings panel shows the connection state (or an error, e.g. Spotify not
logged in).

## Notes

- **Metadata:** title, album, cover, progress and play state come from the
  Connect cluster; the **artist name** is resolved once per song change via
  Spotify's internal `extended-metadata` endpoint (TRACK_V4) — the same path the
  desktop app uses, so it's unthrottled and reliable (the public Web API, by
  contrast, rate-limits the desktop client id).
- Uses these host capabilities (added for this plugin, generic + reusable):
  `read_file` (manifest-scoped), `write_asset` / `clear_asset_dir`
  (plugin-confined), `p256_sign` / `p256_public`, `ws_connect_async` /
  `ws_send` / `ws_close`, `http_request_async` (returns response headers +
  byte-safe body).
