# Workshop multiplayer network binaries

`tailscaled.exe`, `tailscale.exe`, and `wintun.dll` here are the real,
upstream Tailscale daemon/CLI/driver, built from the open-source
`tailscale.com` Go module -- not a custom program. Hebnix bundles and drives
them itself (installed as a Windows service named `HebnixTailscale`, kept
completely separate from any Tailscale the user has installed themselves)
instead of requiring the user to install the official Tailscale client.

## Why not `tsnet`

An earlier version of this used Tailscale's `tsnet` Go library, embedded in
a small custom program, instead of the real daemon. `tsnet` turned out to be
the wrong tool: it only exposes a userspace network stack reachable from
inside its own process's `Dial`/`Listen` calls -- there is no real Windows
network adapter for a separate process (Rocket League) to bind to. The real
`tailscaled` daemon creates an actual Wintun-backed adapter any process can
see and use, which is what Rocket League's `-multihome` flag needs.

## Rebuilding

These are prebuilt and checked in (same convention as `steam_api64.dll` and
`rlapi-bridge.exe`), so a normal `cargo build` doesn't need Go installed.
To rebuild after bumping the pinned `tailscale.com` version:

```
go get tailscale.com/cmd/tailscaled@vX.Y.Z
go get tailscale.com/cmd/tailscale@vX.Y.Z
go build -ldflags="-s -w" -o tailscaled.exe tailscale.com/cmd/tailscaled
go build -ldflags="-s -w" -o tailscale.exe tailscale.com/cmd/tailscale
```

`wintun.dll` comes from the same `golang.zx2c4.com/wintun` version pinned in
`go.sum`; it only needs re-copying if that version changes, from:
`%GOPATH%\pkg\mod\golang.zx2c4.com\wintun@<version>\bin\<arch>\wintun.dll`
(or wherever `go build` resolved it from -- check `go.sum` for the exact
version) -- or pulled straight from an official Tailscale Windows install if
that's easier.

## Runtime notes (see `tsnet_sidecar.rs` for the code)

- `tailscaled.exe` **must** run as an installed Windows service (`sc.exe
  create` + `start`), not a bare foreground process. Run any other way, its
  Windows-specific per-session profile-switching logic misfires on every new
  connecting client (including plain CLI status checks) and tears the whole
  login down. This is why `TsnetSidecarHandle::spawn` installs/starts the
  `HebnixTailscale` service rather than just launching the exe.
- `tailscale up` **must** pass `--unattended`. Without it, Windows Tailscale
  disconnects the tailnet the moment the connecting client (our short-lived
  CLI call) disconnects -- by design, since normally the persistent GUI tray
  app is what stays connected. `--unattended` is the documented flag for
  running headless/server-style instead.
- Both were confirmed by directly tracing `tailscaled`'s own log output
  against a real self-hosted headscale test server before this rewrite.

## Patched build (runs next to a normal Tailscale install)

`tailscaled.exe` ignores `--socket` when it runs as a Windows service, and a
stock build always uses the pipe `...\Administrators\Tailscale\tailscaled`,
the adapter name `Tailscale` and one fixed adapter GUID. That collides with a
normal Tailscale install. The binaries here are built from
`tailscale.com` v1.102.4 with four small changes so they use their own names:

- `paths/paths.go`: windows `DefaultTailscaledSocket()` pipe -> `...\Administrators\HebnixTailscale\tailscaled`
- `net/tstun/tun_windows.go`: `WintunTunnelType` -> `HebnixTailscale`, and a different `WintunStaticRequestedGUID`
- `cmd/tailscaled/tailscaled.go`: windows `defaultTunName()` -> `HebnixTailscale`
- `cmd/tailscaled/tailscaled_windows.go`: `serviceName` -> `HebnixTailscale`

To rebuild, copy the module out of the go mod cache, apply those four edits,
add `replace tailscale.com => ./that-copy` to a copy of `go.mod`, and run the
`go build` commands above. `tsnet_sidecar.rs` `SERVICE_PIPE` must match the pipe.
