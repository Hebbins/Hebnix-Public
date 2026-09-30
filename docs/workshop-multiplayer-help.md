# Workshop multiplayer: VPNs, proxies and common problems

Workshop multiplayer puts every player on a small private network. It is a
Tailscale network that Hebnix runs by itself: its own Windows service
(`HebnixTailscale`), its own network adapter, and addresses in `10.242.77.x`.
It is separate from any Tailscale you installed yourself. Rocket League then
sees the other players as if they were on your LAN.

Most problems come from something on the PC getting in the way of that private
network: a VPN or proxy app, or a firewall. Find your symptom below.

- [I use a VPN or proxy](#i-use-a-vpn-or-proxy)
- [It says "timeout waiting for Tailscale service to enter a Running state"](#timeout-waiting-for-running-state)
- [Only one of us sees the other](#only-one-of-us-sees-the-other)
- [We both see each other but no game shows up in Local Matches](#no-game-in-local-matches)
- [Checking the connection by hand](#checking-the-connection-by-hand)
- [Administrator / service errors](#administrator--service-errors)
- [Still stuck?](#still-stuck)

## I use a VPN or proxy

Hebnix's network service deliberately goes around other VPNs, so it doesn't
loop through them. Most VPN and proxy apps are fine with that. Some of them
block it, or answer name lookups with fake addresses, and then the connection
fails.

### Clash, Mihomo, Clash Verge, FlClash, sing-box, v2rayN, Hiddify, Throne

Your app is in **TUN mode** (sometimes called "virtual adapter" or "service
mode"), usually with **fake-IP DNS** (addresses like `198.18.x.x`). Hebnix's
network service gets a fake address for the multiplayer server and can't
reach it. The fix is to let the multiplayer traffic go direct.

In the config file (or the app's DNS override / rules settings), add:

```yaml
dns:
  fake-ip-filter:
    - "hs.xplodingeggo.space"
    - "+.tailscale.com"
    - "+.tailscale.io"

tun:
  route-exclude-address:
    - 10.242.77.0/24

rules:
  # put these at the very top of your rules
  - DOMAIN,hs.xplodingeggo.space,DIRECT
  - PROCESS-NAME,tailscaled.exe,DIRECT
  - IP-CIDR,10.242.77.0/24,DIRECT,no-resolve
```

If your rules use a named "no VPN" group instead of `DIRECT`, use that name.

Restart the app, then in Hebnix click **Disconnect** and connect again.

Still failing? Try `strict-route: false` under `tun:`. Strict routing can block
traffic that goes around the tunnel.

### Mullvad, Proton VPN, NordVPN, Windscribe, and other VPN apps

- Turn on **split tunneling** (or "exclude apps") and exclude **Hebnix**
  (`hebnix.exe`) and its network service (`tailscaled.exe`, in the same folder
  as `hebnix.exe`).
- Turn off the **kill switch** / "block connections without VPN" / "lockdown
  mode" while playing, or allow LAN / local network sharing. A kill switch
  drops everything that doesn't go through the VPN.
- Quickest test: disconnect the VPN and try again. If it works then, it's
  the VPN's settings.

### Your country blocks direct connections

If you need a proxy to reach most sites, direct connections to the multiplayer
server or to other players may be blocked by your ISP. The fixes above can't
help then. Try a different network (a mobile hotspot, for example) to confirm.

## Timeout waiting for Running state

`timeout waiting for Tailscale service to enter a Running state` means the
network service started but couldn't reach the multiplayer server in time.

1. VPN or proxy running? See [the section above](#i-use-a-vpn-or-proxy). This
   is the most common cause.
2. A firewall or antivirus suite (not Windows Defender Firewall) may block
   `tailscaled.exe` from going online. Allow it.
3. Is the whole internet reachable from that PC right now?

## Only one of us sees the other

Player A sees player B in "Maps in use", but B doesn't see A. A's PC is
blocking connections that come **in** over the private network. That's almost
always a firewall.

- Hebnix adds its own Windows Defender Firewall rules when it connects (it
  runs as administrator for this). If you use a **third-party firewall or
  antivirus suite** (Norton, Kaspersky, Bitdefender, ESET, ...), it ignores
  those rules. Allow `hebnix.exe` and `tailscaled.exe`, or allow incoming
  connections from `10.242.77.0/24`.
- If Windows asked whether to allow Hebnix on public or private networks and
  you clicked Cancel, allow it in *Windows Security > Firewall & network
  protection > Allow an app through firewall*.

A VPN or proxy on the player who **can't see** the other can cause this too. See
[I use a VPN or proxy](#i-use-a-vpn-or-proxy).

## No game in Local Matches

Both players see each other in Hebnix, but **Local Matches** is empty.

- **Start Rocket League from Hebnix** (the Multiplayer tab's
  "Start Rocket League" button). Hebnix adds a launch option that puts Rocket
  League on the private network. Started from Epic or Steam directly, it
  won't be.
- **Both players need the same Workshop map** in the same slot. "Maps in use"
  shows who has what, and you can download a missing map from a player there.
- While the host is in a LAN match, the host's **"sent"** counter should go up.
  If it stays at 0, the host's Hebnix isn't picking up Rocket League's LAN
  announcement. Update to the newest Hebnix, restart Rocket League from Hebnix,
  and check the console for `Beacon relay` lines. The "received" counter
  always shows 0; that's normal.
- The player who can't see the game: check
  [Only one of us sees the other](#only-one-of-us-sees-the-other). The same
  firewall block stops the game announcements.
- Press **Refresh List** in Local Matches after the host has started the match.

## Checking the connection by hand

Hebnix's network service has its own control pipe. A plain `tailscale status`
(or the official Tailscale app) looks at *your* Tailscale, not Hebnix's. In an
**administrator** PowerShell, in the folder that has `hebnix.exe`:

```powershell
.\tailscale.exe --socket=\\.\pipe\ProtectedPrefix\Administrators\HebnixTailscale\tailscaled status
```

A working connection lists your own `10.242.77.x` address and the other
players. "Logged out" means the service never reached the server. See
[Timeout waiting for Running state](#timeout-waiting-for-running-state).

The official Tailscale app can stay installed. If you have trouble, quit it
while playing.

## Administrator / service errors

Workshop multiplayer needs Hebnix to **run as administrator**. It installs and
starts the `HebnixTailscale` service and adds firewall rules. Restart Hebnix
as administrator if the Multiplayer tab says so.

If the service is stuck, stop it in an administrator PowerShell and connect
again in Hebnix:

```powershell
sc.exe stop HebnixTailscale
```

## Still stuck?

Open an issue on GitHub (or ask in Discord) with:

- your Windows version, and whether you use a VPN, proxy, firewall or
  antivirus app
- the error text from Hebnix's Multiplayer tab
- the Workshop multiplayer lines from Hebnix's console (`[tsnet]`,
  `Beacon relay`, `Map sync`)
- the output of the `tailscale.exe ... status` command above
