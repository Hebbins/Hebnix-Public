# Workshop multiplayer: what the server side needs

This is the checklist for whoever stands up the coordination server Hebnix's
Workshop multiplayer clients connect to (`TSNET_CONTROL_URL` in
`crates/hebnix-app/src/multiplayer-lan/mod.rs`, currently
`https://api.hebnix.com`). Update that constant if the server ends up
somewhere else.

None of this affects the client build -- everything below is server-side
infrastructure and a small backend endpoint.

## 1. Run headscale (self-hosted Tailscale coordination server)

Standard self-hosted [headscale](https://headscale.net/), no special
Hebnix-specific config beyond the usual `server_url`/`listen_addr`/DERP
settings in its `config.yaml` -- except one:

**Set `dns.magic_dns: false`.** Confirmed this the hard way: with MagicDNS
on, tailscaled writes a Windows NRPT (Name Resolution Policy Table) rule
pointing DNS at the tailnet, and if that rule is ever left behind (a crash,
an unclean shutdown, anything that skips `tailscale down` on the way out),
Windows keeps trying to resolve *all* DNS through a server that's no longer
there -- breaking DNS system-wide on that PC, not just for Hebnix. Workshop
multiplayer doesn't need MagicDNS at all (players connect by raw tailnet
IP), so turning it off server-side removes the risk entirely rather than
relying on every client to clean up perfectly on exit.

## 2. Reverse proxy: must support raw duplex HTTP, not just request/response

**This is the one gotcha worth calling out explicitly, confirmed by testing
against a real client tonight.** Tailscale's registration protocol
(`ts2021`/noise) needs a raw, unbuffered, bidirectional connection through
whatever sits in front of headscale -- it is not a normal
request-then-response HTTP call.

- **A free/quick tunnel (e.g. Cloudflare Quick Tunnels) does not work.**
  Client registration fails with a `400 Bad Request` that never even reaches
  headscale's own logs -- the tunnel's proxying breaks the protocol before
  the request gets there. Confirmed directly this session.
- **A normal reverse proxy works fine**, as long as it doesn't buffer the
  request/response or force it down to plain request/response semantics.
  For nginx specifically, headscale's own docs cover this
  (`proxy_http_version 1.1`, `proxy_buffering off`, forwarding the
  `Upgrade`/`Connection` headers) -- see
  [headscale's reverse-proxy guide](https://headscale.net/stable/ref/integration/reverse-proxy/).
  If `api.hebnix.com` already runs behind nginx/Caddy for other things,
  adding a location block for headscale with those settings is enough; a
  dedicated subdomain isn't required.
- Plain HTTP directly on the box, no proxy at all, also works (confirmed
  tonight) if TLS isn't wanted for the coordination endpoint specifically --
  but a reverse proxy with real TLS is the normal production setup.

## 3. Room API: mint a pre-auth key, that's it

There's no room/PIN system on the client any more -- Hebnix's own UI for
Workshop multiplayer is down to a single "Connect" button, no host/join
choice, no room to create or join. The client's only remaining call to the
room API is `?request=tsnet/authkey` (see `RoomClient::request_tsnet_authkey`
in `crates/hebnix-app/src/multiplayer-lan/room_api.rs`), just to get onto
the tailnet at all:

```json
{ "auth_key": "...", "control_url": "https://api.hebnix.com", "expires_at": "..." }
```

Server-side, that's minting a headscale pre-auth key scoped to a single
node, short-lived, and ephemeral (so it disappears from the tailnet
automatically when the player disconnects rather than accumulating stale
nodes forever):

```
headscale preauthkeys create --user <pool-user> --ephemeral --reusable=false --expiration 1h
```

(via headscale's gRPC/HTTP API from the room-api backend, not shelling out to
the CLI in production, but that's the equivalent operation). A single
headscale "user" to pool every Workshop player under is fine -- Hebnix's own
client-side ACLs/policy isn't a concern yet at this stage. The client
currently sends an empty `pin` and a hardcoded `role: "peer"` in the
request body -- neither means anything server-side any more, they're
leftover from when there was a room concept; safe to just ignore both
fields and hand back a key regardless of what's in them.

## 4. Peer discovery doesn't need the room API at all

This used to need the room API to track who's in a room and expose everyone's
`tailnet_ip` to each other. That's gone -- once a player's node is on the
tailnet, every other connected player's Hebnix client already sees them
directly from `tailscale status --json` (which is how the beacon relay finds
who to relay to). The room API genuinely doesn't need to know or store
anything about individual players or sessions any more -- minting auth keys
is the whole job.

The room/player endpoints (`multiplayer/create`, `join`, `leave`, `player`,
`update`, `close`) are all still defined client-side in `room_api.rs` but
nothing calls them any more -- dead code kept around rather than ripped out
mid-rework. None of them need a server implementation.

---

Everything past this point is just for context, not action items:

The client used to plan on using Tailscale's `tsnet` Go library embedded
directly in a custom Hebnix helper process. That's been replaced with the
real, upstream `tailscaled` daemon + `tailscale` CLI (see
`sidecar/README.md`) -- `tsnet` doesn't expose a real OS network adapter,
which turned out to be a hard requirement for Rocket League's `-multihome`
to work. This doesn't change anything about what the server needs to do;
it's the same headscale protocol either way.

Getting the actual game discovery working (Rocket League's LAN browser
showing a game across the tunnel) was entirely a client-side problem, not
a server one -- Rocket League's own LAN discovery broadcast can't cross a
WireGuard tunnel by design, so Hebnix runs a small relay on each player's
machine that captures that broadcast (via WinDivert) and re-sends it
directly to whoever else is on the tailnet. None of this touches the
server at all; it's mentioned here only so it's not a surprise that "the
tailnet connects fine but the game doesn't show up" was never a server-side
bug to chase.
