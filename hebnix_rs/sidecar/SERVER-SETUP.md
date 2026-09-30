# Workshop multiplayer: what the server side needs

This is the checklist for whoever stands up the coordination server Hebnix's
Workshop multiplayer clients connect to. The client has two constants in
`crates/hebnix-app/src/multiplayer-lan/mod.rs`:

- `ROOM_API_BASE_URL`: where the client asks for an auth key
  (`POST /?request=tsnet/authkey`).
- `TSNET_CONTROL_URL`: the headscale server.

Both currently point at a **test** server (`https://hs.xplodingeggo.space`,
marked TEST ONLY). Change them to the real server before release. In practice
the client logs in to whatever `control_url` the auth key reply returns (see
section 4), so the two only have to agree with each other.

None of this affects the client build. Everything below is server-side
infrastructure and one small endpoint.

A working reference setup (headscale 0.29, Caddy, a ~100 line Python room
API) has been tested with Windows and Linux clients connecting from different
networks. Map sync works across it, and players have joined each other's LAN
matches through it.

## 1. Run headscale (self-hosted Tailscale coordination server)

Standard self-hosted [headscale](https://headscale.net/). Most of its
`config.yaml` stays at the defaults (`server_url`, `listen_addr`, DERP).
Three settings matter for Hebnix:

**Set `prefixes.v4: 10.242.77.0/24`.** The client depends on this range:
the Windows firewall rules for map sync only allow `10.242.77.0/24`
(`TAILNET_SUBNET` in `multiplayer-lan/firewall.rs`), and map sync only talks
to peers in the same /24 as the player's own address. Headscale's default
`100.64.0.0/10` connects fine but breaks map sync.

```yaml
prefixes:
  v4: 10.242.77.0/24
  v6: fd7a:115c:a1e0::/48
  allocation: sequential
```

Headscale logs a warning at startup that the prefix is outside the usual
Tailscale range. That's expected, and it works. A /24 holds 253 players at
once; ephemeral nodes free their address when they go (see below).

**Set `dns.magic_dns: false`.** Confirmed this the hard way: with MagicDNS
on, tailscaled writes a Windows NRPT (Name Resolution Policy Table) rule
pointing DNS at the tailnet, and if that rule is ever left behind (a crash,
an unclean shutdown, anything that skips `tailscale down` on the way out),
Windows keeps trying to resolve *all* DNS through a server that's no longer
there -- breaking DNS system-wide on that PC, not just for Hebnix. Workshop
multiplayer doesn't need MagicDNS at all (players connect by raw tailnet
IP), so turning it off server-side removes the risk entirely rather than
relying on every client to clean up perfectly on exit. (The clients also run
`tailscale up --accept-dns=false`.)

```yaml
dns:
  magic_dns: false
  override_local_dns: false
```

**Clean up disconnected players.** Players join with ephemeral keys (section
4), so their node is removed after it has been offline for
`node.ephemeral.inactivity_timeout`. 5 minutes is a good value: long enough
for a game restart, short enough that the peer list doesn't fill with ghosts.

```yaml
node:
  expiry: 0
  ephemeral:
    inactivity_timeout: 5m
```

DERP: no own DERP server is needed. The default
`https://controlplane.tailscale.com/derpmap/default` works; players whose
direct connection fails fall back to Tailscale's public relays.

If headscale sits behind a reverse proxy on the same box, also set
`trusted_proxies` (for example `127.0.0.1/32`) and bind `listen_addr` to
localhost.

## 2. Reverse proxy: must support raw duplex HTTP, not just request/response

**This is the one gotcha worth calling out explicitly, confirmed by testing
against a real client.** Tailscale's registration protocol
(`ts2021`/noise) needs a raw, unbuffered, bidirectional connection through
whatever sits in front of headscale -- it is not a normal
request-then-response HTTP call.

- **A free/quick tunnel (e.g. Cloudflare Quick Tunnels) does not work.**
  Client registration fails with a `400 Bad Request` that never even reaches
  headscale's own logs -- the tunnel's proxying breaks the protocol before
  the request gets there.
- **A normal reverse proxy works fine**, as long as it doesn't buffer the
  request/response or force it down to plain request/response semantics.
  - **Caddy** works with a plain `reverse_proxy` and gets a Let's Encrypt
    certificate by itself. Confirmed with the reference setup.
  - **nginx**: headscale's own docs cover it (`proxy_http_version 1.1`,
    `proxy_buffering off`, forwarding the `Upgrade`/`Connection` headers).
    See [headscale's reverse-proxy guide](https://headscale.net/stable/ref/integration/reverse-proxy/).
- Plain HTTP directly on the box, no proxy at all, also works if TLS isn't
  wanted for the coordination endpoint specifically -- but a reverse proxy
  with real TLS is the normal production setup.

The room API (section 4) and headscale can share one hostname. The client
posts to `/` with the query `?request=tsnet/authkey`, so route by query
string and send everything else to headscale. Caddy example from the
reference setup:

```
hs.example.com {
	@authkey {
		method POST
		path /
		query request=tsnet/authkey
	}
	handle @authkey {
		reverse_proxy 127.0.0.1:8086   # room API
	}
	handle {
		reverse_proxy 127.0.0.1:8085   # headscale
	}
}
```

If `api.hebnix.com` already runs behind nginx/Caddy for other things, adding
these routes there is enough; a dedicated subdomain isn't required.

## 3. Ports and hosting from home

The server needs **TCP 443** open (and TCP 80 if the proxy should fetch
Let's Encrypt certificates over HTTP). Nothing else: player traffic goes
directly between players, or through Tailscale's DERP relays.

Hosting from a home connection works (the test server does), with two
catches:

- Forward 80/443 on the router (UPnP or by hand) to the server box.
- Many home routers can't reach their own public IP from inside the house
  (no "hairpin NAT"). A Hebnix client on the **same network** as the server
  then can't connect by the public hostname, while everyone outside can. Fix
  on those machines with a hosts entry pointing the hostname at the server's
  LAN address, or with split DNS on the router.

## 4. Room API: mint a pre-auth key, that's it

There's no room/PIN system on the client any more -- Hebnix's own UI for
Workshop multiplayer is down to a single "Connect" button, no host/join
choice, no room to create or join. The client's only remaining call to the
room API is `?request=tsnet/authkey` (see `RoomClient::request_tsnet_authkey`
in `crates/hebnix-app/src/multiplayer-lan/room_api.rs`), just to get onto
the tailnet at all. Reply with:

```json
{ "auth_key": "hskey-...", "control_url": "https://hs.example.com", "expires_at": "2026-01-01T12:00:00Z" }
```

`control_url` is what the client passes to `tailscale up --login-server`, so
it must be the public headscale URL. `expires_at` is an RFC 3339 time.

Server-side, that's minting a headscale pre-auth key: single use,
short-lived, and ephemeral (so the node disappears from the tailnet
automatically when the player disconnects rather than accumulating stale
nodes forever). With headscale 0.26 and newer, `--user` takes the user's
numeric **id**, not its name (`headscale users list` shows it):

```
headscale preauthkeys create --user <pool-user-id> --ephemeral --expiration 1h --output json
```

Keys are single use unless `--reusable` is given. A backend can do the same
through headscale's gRPC/HTTP API; the reference setup simply runs the CLI
as the `headscale` system user, which can reach headscale's unix socket. A
single headscale "user" to pool every Workshop player under is fine.

The client sends an empty `pin` and a hardcoded `role: "peer"` in the
request body -- neither means anything server-side any more, they're
leftover from when there was a room concept; safe to just ignore both
fields and hand back a key regardless of what's in them.

**Abuse:** the endpoint has no authentication, so anyone who finds the URL
can mint keys and join the tailnet, where they can see every connected
player's tailnet address and reach their open ports (section 6). At minimum,
rate-limit it per client IP (the reference setup allows 10 keys per minute
per IP, which a real player never gets near: one key per Connect). Also
consider requiring the same `X-App-Token` header Hebnix already sends to
`req.hebnix.com`, which would need the client to send it here too.

## 5. Peer discovery doesn't need the room API at all

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

## 6. Optional: limit what players can reach (ACL)

With no policy, headscale lets every node reach every other node on every
port. Workshop multiplayer only needs these between players:

| Port | Protocol | What |
| --- | --- | --- |
| 7777 | UDP | Rocket League game traffic |
| 14000-14010, 14777 | UDP | LAN discovery beacons (relayed by Hebnix) |
| 14790 | TCP | Hebnix map sync |

A headscale policy that allows only those. **Not tested yet**, so try it on
the test server with a real match before relying on it:

```json
{
  "acls": [
    {
      "action": "accept",
      "src": ["*"],
      "dst": ["*:7777", "*:14000-14010", "*:14777", "*:14790"]
    }
  ]
}
```

Set it with `policy.mode: file` and `policy.path` pointing at that file.

---

Everything past this point is just for context, not action items:

The client used to plan on using Tailscale's `tsnet` Go library embedded
directly in a custom Hebnix helper process. That's been replaced with the
real, upstream `tailscaled` daemon + `tailscale` CLI (see
`sidecar/README.md`) -- `tsnet` doesn't expose a real OS network adapter,
which turned out to be a hard requirement for Rocket League's `-multihome`
to work. This doesn't change anything about what the server needs to do;
it's the same headscale protocol either way. The Linux port does the same
with the distro's own `tailscaled`, started by Hebnix with its own socket
and interface.

Getting the actual game discovery working (Rocket League's LAN browser
showing a game across the tunnel) was entirely a client-side problem, not
a server one -- Rocket League's own LAN discovery broadcast can't cross a
WireGuard tunnel by design, so Hebnix runs a small relay on each player's
machine that captures that broadcast (via WinDivert on Windows, a raw packet
socket on Linux) and re-sends it directly to whoever else is on the tailnet.
None of this touches the server at all; it's mentioned here only so it's not
a surprise that "the tailnet connects fine but the game doesn't show up" was
never a server-side bug to chase.

The same goes for players who connect fine to the room API but never finish
joining the tailnet ("Logged out", or "timeout waiting for Tailscale service
to enter a Running state"). The room API logs a key for them while headscale
never sees them. That is a local problem on their PC, usually a VPN or proxy
app in TUN / fake-IP mode or a firewall, not the server.
