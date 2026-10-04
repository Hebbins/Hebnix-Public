// Drives the bundled real `tailscaled.exe` + `tailscale.exe` (the actual
// upstream Tailscale daemon/CLI, not a custom program -- see
// sidecar/README.md for why this isn't the `tsnet` library) to join a
// headscale-coordinated tailnet for Workshop multiplayer.
//
// `tailscaled.exe` is installed and run as its own Windows service
// (`HebnixTailscale`, entirely separate from any Tailscale the user has
// installed themselves) rather than as a plain child process: run any other
// way, its Windows-specific per-session profile-switching logic misfires on
// every connecting client and tears the login down (confirmed by tracing
// its own log output during development). Commands are issued by shelling
// out to `tailscale.exe --socket=<hebnix pipe> ...`; results are delivered
// asynchronously as `AppMsg::Tsnet*` variants on the app's message channel,
// following the same background-thread -> mpsc -> egui-main-thread pattern
// used elsewhere in hebnix-app (see `app.rs`'s stats/game-event handling).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::Sender;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde::Deserialize;

use crate::messages::AppMsg;

#[cfg(windows)]
use winreg::RegKey;
#[cfg(windows)]
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const SERVICE_NAME: &str = "HebnixTailscale";
// tailscaled ignores --socket when it runs as a windows service and uses its
// built-in default pipe. the bundled tailscaled/tailscale are built with that
// default (and the adapter name/guid) changed to HebnixTailscale, so this
// matches it and doesn't collide with a normal tailscale install.
const SERVICE_PIPE: &str = r"\\.\pipe\ProtectedPrefix\Administrators\HebnixTailscale\tailscaled";
const SERVICE_START_TIMEOUT: Duration = Duration::from_secs(10);
/// tailscaled knob that turns off direct (udp) connections, so all traffic
/// goes through tailscale's DERP relays and other players never learn this
/// machine's public address. costs some ping. set in the service's
/// environment, so it only applies after a service restart.
const RELAY_ONLY_KNOB: &str = "TS_DEBUG_ALWAYS_USE_DERP";
const PEER_POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TsState {
    Stopped,
    Starting,
    Connecting,
    Connected,
    Backoff,
}

impl TsState {
    fn parse(backend_state: &str) -> Self {
        match backend_state {
            "Running" => TsState::Connected,
            "Starting" => TsState::Starting,
            "NeedsLogin" | "NeedsMachineAuth" => TsState::Connecting,
            _ => TsState::Stopped,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub tailnet_ip: String,
    pub hostname: String,
    pub online: bool,
}

// tailscale sends null for empty lists/maps (e.g. "Peer": null with no peers
// yet), and serde's default only covers a missing field, so treat null as empty
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Deserialize, Default)]
struct RawStatus {
    #[serde(rename = "BackendState", default, deserialize_with = "null_as_default")]
    backend_state: String,
    #[serde(rename = "TailscaleIPs", default, deserialize_with = "null_as_default")]
    tailscale_ips: Vec<String>,
    #[serde(rename = "Peer", default, deserialize_with = "null_as_default")]
    peer: HashMap<String, RawPeer>,
}

#[derive(Deserialize)]
struct RawPeer {
    #[serde(rename = "HostName", default, deserialize_with = "null_as_default")]
    host_name: String,
    #[serde(rename = "TailscaleIPs", default, deserialize_with = "null_as_default")]
    tailscale_ips: Vec<String>,
    // confirmed live (2026-09-28): a peer's own `TailscaleIPs` here can be
    // v6-only - `AllowedIPs` (CIDR routes, e.g. "10.242.77.2/32") is what
    // actually has the v4 address reliably for every peer. `TailscaleIPs`
    // still works fine for *self* (the node's own status always lists v4
    // first there), just not for peers.
    #[serde(rename = "AllowedIPs", default, deserialize_with = "null_as_default")]
    allowed_ips: Vec<String>,
    #[serde(rename = "Online", default)]
    online: bool,
}

/// A handle to the Hebnix-managed tailscaled service. Commands are
/// fire-and-forget (each spawns its own short-lived worker thread that
/// shells out to the CLI); results/events arrive asynchronously as
/// `AppMsg::Tsnet*` variants on `tx`.
pub struct TsnetSidecarHandle {
    tailscale_cli: PathBuf,
    tx: Sender<AppMsg>,
    poll_stop: Arc<AtomicBool>,
}

impl std::fmt::Debug for TsnetSidecarHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TsnetSidecarHandle")
            .finish_non_exhaustive()
    }
}

/// the files that make up the multiplayer network helper
const SIDECAR_EXES: [&str; 2] = ["tailscaled.exe", "tailscale.exe"];
const SIDECAR_DLL: &str = "wintun.dll";

/// picks which folder to run the network helper from. the first folder that
/// has the whole set wins, else the first with both programs (wintun.dll can
/// then come from wherever windows finds it). `dirs` is in priority order.
pub fn find_sidecar_dir(dirs: &[PathBuf]) -> Result<PathBuf, String> {
    let has = |dir: &PathBuf, files: &[&str]| files.iter().all(|file| dir.join(file).is_file());
    let full = [SIDECAR_EXES[0], SIDECAR_EXES[1], SIDECAR_DLL];
    dirs.iter()
        .find(|dir| has(dir, &full))
        .or_else(|| dirs.iter().find(|dir| has(dir, &SIDECAR_EXES)))
        .cloned()
        .ok_or_else(|| {
            let places: Vec<String> = dirs.iter().map(|dir| dir.display().to_string()).collect();
            format!(
                "the multiplayer network components are missing, looked in: {}",
                places.join(", ")
            )
        })
}

impl TsnetSidecarHandle {
    /// Ensures the `HebnixTailscale` service is installed (pointed at the
    /// `tailscaled.exe`/`wintun.dll` next to `exe_dir`) and running, then
    /// starts a background peer-status poller. Requires administrator
    /// rights (same as the firewall-rule and, previously, TAP-driver setup
    /// this app already needed).
    pub fn spawn(
        exe_dir: &Path,
        state_dir: &Path,
        relay_only: bool,
        tx: Sender<AppMsg>,
    ) -> Result<Self, String> {
        let tailscaled_exe = exe_dir.join("tailscaled.exe");
        let tailscale_cli = exe_dir.join("tailscale.exe");
        if !tailscaled_exe.is_file() || !tailscale_cli.is_file() {
            return Err(format!(
                "the multiplayer network components are missing from {}",
                exe_dir.display()
            ));
        }

        ensure_service(&tailscaled_exe, state_dir, relay_only)?;

        let poll_stop = Arc::new(AtomicBool::new(false));
        spawn_peer_poller(tailscale_cli.clone(), tx.clone(), poll_stop.clone());

        Ok(Self {
            tailscale_cli,
            tx,
            poll_stop,
        })
    }

    pub fn request_up(
        &self,
        auth_key: String,
        hostname: String,
        control_url: String,
    ) -> Result<(), String> {
        let cli = self.tailscale_cli.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = bring_up(&cli, &auth_key, &hostname, &control_url);
            let _ = tx.send(AppMsg::TsnetUpResult { result });
        });
        Ok(())
    }

    /// Blocking fetch of the current tailnet peer list, for a background
    /// worker thread (e.g. the beacon relay) that needs a fresh list on its
    /// own cadence rather than an async AppMsg round trip.
    pub fn peers_now(&self) -> Result<Vec<PeerInfo>, String> {
        let status = fetch_status(&self.tailscale_cli)?;
        Ok(status.peer.into_values().map(peer_info).collect())
    }

    pub fn request_status(&self) -> Result<(), String> {
        let cli = self.tailscale_cli.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || match fetch_status(&cli) {
            Ok(status) => {
                let _ = tx.send(AppMsg::TsnetStatus {
                    state: TsState::parse(&status.backend_state),
                    tailnet_ip: pick_ipv4(status.tailscale_ips),
                    peers: status.peer.into_values().map(peer_info).collect(),
                });
            }
            Err(error) => {
                let _ = tx.send(AppMsg::Log(format!(
                    "[tsnet] status check failed: {}",
                    redact(&error)
                )));
            }
        });
        Ok(())
    }

    /// Releases the tailnet connection but leaves the service running, so a
    /// quick rejoin doesn't have to wait on the service starting again.
    pub fn request_down(&self) -> Result<(), String> {
        let cli = self.tailscale_cli.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let ok = run_tailscale(&cli, &["down"]).is_ok();
            let _ = tx.send(AppMsg::TsnetDownResult { ok });
        });
        Ok(())
    }

    /// Releases the tailnet connection and stops the `HebnixTailscale`
    /// service so nothing lingers once Workshop multiplayer ends.
    pub fn request_shutdown(&self) -> Result<(), String> {
        self.poll_stop.store(true, Ordering::Relaxed);
        let cli = self.tailscale_cli.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let ok = run_tailscale(&cli, &["down"]).is_ok();
            let _ = run_sc(&["stop", SERVICE_NAME]);
            let _ = tx.send(AppMsg::TsnetDownResult { ok });
        });
        Ok(())
    }

    /// Waits for the `HebnixTailscale` service to actually stop.
    pub fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            match query_state() {
                Ok(state) if state_is_running(&state) => {}
                _ => return true,
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        false
    }

    pub fn kill(&mut self) {
        self.poll_stop.store(true, Ordering::Relaxed);
        let _ = run_sc(&["stop", SERVICE_NAME]);
    }
}

impl Drop for TsnetSidecarHandle {
    fn drop(&mut self) {
        // best-effort, synchronous (Drop can't await the usual async
        // thread-plus-channel result) -- release the tailnet, then stop the
        // service so nothing lingers once Workshop multiplayer ends
        let _ = run_tailscale(&self.tailscale_cli, &["down"]);
        self.kill();
        let _ = self.wait_for_exit(Duration::from_secs(3));
    }
}

fn peer_info(peer: RawPeer) -> PeerInfo {
    PeerInfo {
        tailnet_ip: peer_ipv4(&peer.allowed_ips, peer.tailscale_ips).unwrap_or_default(),
        hostname: peer.host_name,
        online: peer.online,
    }
}

/// picks the v4 address out of a *node's own* `TailscaleIPs` - reliably v4
/// first there (confirmed live). Not used for peers any more, see
/// `peer_ipv4` below for why.
fn pick_ipv4(ips: Vec<String>) -> Option<String> {
    ips.iter()
        .find(|ip| ip.parse::<std::net::Ipv4Addr>().is_ok())
        .cloned()
        .or_else(|| ips.into_iter().next())
}

/// A *peer's* `TailscaleIPs` field isn't reliable for this at all -
/// confirmed live that it can be v6-only (`["fd7a:115c:a1e0::2"]`, no v4
/// entry present whatsoever), even though the same peer's `AllowedIPs`
/// always has the v4 route (`"10.242.77.2/32"`). So for peers, read the v4
/// address out of `AllowedIPs` instead, and only fall back to
/// `TailscaleIPs` if that's somehow also missing it.
fn peer_ipv4(allowed_ips: &[String], tailscale_ips: Vec<String>) -> Option<String> {
    allowed_ips
        .iter()
        .find_map(|route| {
            let address = route.split('/').next().unwrap_or(route);
            address
                .parse::<std::net::Ipv4Addr>()
                .is_ok()
                .then(|| address.to_string())
        })
        .or_else(|| pick_ipv4(tailscale_ips))
}

fn bring_up(
    cli: &Path,
    auth_key: &str,
    hostname: &str,
    control_url: &str,
) -> Result<String, String> {
    run_tailscale(
        cli,
        &[
            "up",
            // the service keeps its saved prefs between runs, and `up` refuses
            // to change them unless every non-default flag is repeated (it
            // failed on a leftover --exit-node-allow-lan-access). this is our
            // own service, so just start from defaults every time.
            "--reset",
            &format!("--login-server={control_url}"),
            &format!("--authkey={auth_key}"),
            &format!("--hostname={hostname}"),
            // Windows Tailscale disconnects the tailnet when the connecting
            // client disconnects (by design -- normally that client is the
            // persistent GUI tray app). Ours is a short-lived CLI call, so
            // without --unattended the tailnet drops the instant this
            // process exits. See sidecar/README.md.
            "--unattended",
            // this multiplayer network doesn't need MagicDNS (players use
            // raw tailnet IPs), and letting tailscaled manage Windows DNS
            // at all - even with magic_dns off server-side - is what wrote
            // the stale DNS policy rule that broke DNS system-wide earlier
            // (see dns_cleanup.rs). never let it touch DNS in the first place.
            "--accept-dns=false",
            "--timeout=30s",
        ],
    )?;
    let status = fetch_status(cli)?;
    pick_ipv4(status.tailscale_ips).ok_or_else(|| {
        "connected, but the multiplayer network did not assign an address".to_string()
    })
}

fn fetch_status(cli: &Path) -> Result<RawStatus, String> {
    let raw = run_tailscale(cli, &["status", "--json"])?;
    serde_json::from_str(&raw)
        .map_err(|error| format!("could not understand the multiplayer network's status: {error}"))
}

fn spawn_peer_poller(cli: PathBuf, tx: Sender<AppMsg>, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut known: HashMap<String, bool> = HashMap::new();
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(PEER_POLL_INTERVAL);
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let Ok(status) = fetch_status(&cli) else {
                continue;
            };
            let mut seen = std::collections::HashSet::new();
            for peer in status.peer.into_values() {
                let Some(ip) = peer_ipv4(&peer.allowed_ips, peer.tailscale_ips) else {
                    continue;
                };
                seen.insert(ip.clone());
                let changed = known.get(&ip).is_none_or(|&prev| prev != peer.online);
                if changed {
                    known.insert(ip.clone(), peer.online);
                    let _ = tx.send(AppMsg::TsnetPeerEvent {
                        online: peer.online,
                        tailnet_ip: ip,
                    });
                }
            }
            known.retain(|ip, _| seen.contains(ip));
        }
    });
}

/// Installs (if needed) and starts the `HebnixTailscale` Windows service,
/// pointed at `tailscaled_exe` with its own state directory and named pipe
/// -- kept entirely separate from any real Tailscale install so the two
/// can't collide.
fn ensure_service(tailscaled_exe: &Path, state_dir: &Path, relay_only: bool) -> Result<(), String> {
    std::fs::create_dir_all(state_dir)
        .map_err(|error| format!("could not create the multiplayer network's state directory: {error}"))?;

    let bin_path = format!(
        "\"{}\" --statedir=\"{}\" --socket={SERVICE_PIPE} --port=0",
        tailscaled_exe.display(),
        state_dir.display(),
    );

    if let Err(error) = run_sc(&[
        "create",
        SERVICE_NAME,
        "binPath=",
        &bin_path,
        "start=",
        "demand",
        "DisplayName=",
        "Hebnix Tailscale",
    ]) {
        // sc.exe's message is translated on non-English Windows, but the
        // error number (1073, service already exists) is always there
        if !error.contains("1073") && !error.contains("already exists") {
            return Err(format!(
                "could not install the multiplayer network service: {error}"
            ));
        }
        // Already installed from a previous run -- keep its binPath current
        // in case Hebnix was reinstalled to a different folder.
        let _ = run_sc(&["config", SERVICE_NAME, "binPath=", &bin_path]);
    }

    let changed = set_relay_only(relay_only)
        .map_err(|error| format!("could not set the hide-my-ip option: {error}"))?;
    let mut running = state_is_running(&query_state()?);
    if changed && running {
        // the knob is only read when tailscaled starts
        let _ = run_sc(&["stop", SERVICE_NAME]);
        wait_for_stopped(SERVICE_START_TIMEOUT)
            .map_err(|_| "the multiplayer network service did not stop in time".to_string())?;
        running = false;
    }
    if !running {
        run_sc(&["start", SERVICE_NAME])
            .map_err(|error| format!("could not start the multiplayer network service: {error}"))?;
        wait_for_running(SERVICE_START_TIMEOUT)
            .map_err(|_| "the multiplayer network service did not start in time".to_string())?;
    }

    Ok(())
}

/// the state number from `sc query` output (4 = running). the number is used
/// rather than the word RUNNING, and the label in front of it is never
/// looked at, so this works on any Windows display language.
fn service_state_code(text: &str) -> Option<u8> {
    text.lines().find_map(|line| {
        let value = line.split_once(':')?.1.trim_start();
        let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
        // the state is a single digit; the TYPE line above it is 10/20/...
        // and the exit codes below it are 0 in a healthy service
        if digits.len() == 1 {
            digits
                .parse::<u8>()
                .ok()
                .filter(|code| (1..=7).contains(code))
        } else {
            None
        }
    })
}

fn state_is_running(text: &str) -> bool {
    match service_state_code(text) {
        Some(code) => code == 4,
        None => text.contains("RUNNING"),
    }
}

fn wait_for_running(timeout: Duration) -> Result<(), String> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if state_is_running(&query_state()?) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err("timed out".to_string())
}

fn wait_for_stopped(timeout: Duration) -> Result<(), String> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        let text = query_state()?;
        let stopped = match service_state_code(&text) {
            Some(code) => code == 1,
            None => text.contains("STOPPED"),
        };
        if stopped {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err("timed out".to_string())
}

/// the service's environment with the relay-only knob added or removed
fn environment_with_relay_only(current: Vec<String>, relay_only: bool) -> Vec<String> {
    let prefix = format!("{RELAY_ONLY_KNOB}=");
    let mut env: Vec<String> = current
        .into_iter()
        .filter(|entry| !entry.starts_with(&prefix))
        .collect();
    if relay_only {
        env.push(format!("{prefix}true"));
    }
    env
}

/// writes the relay-only knob into the service's environment, true if that
/// changed anything
#[cfg(windows)]
fn set_relay_only(relay_only: bool) -> Result<bool, String> {
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(
            format!(r"SYSTEM\CurrentControlSet\Services\{SERVICE_NAME}"),
            KEY_READ | KEY_WRITE,
        )
        .map_err(|error| error.to_string())?;
    let current: Vec<String> = key.get_value("Environment").unwrap_or_default();
    let wanted = environment_with_relay_only(current.clone(), relay_only);
    if wanted == current {
        return Ok(false);
    }
    if wanted.is_empty() {
        key.delete_value("Environment").map_err(|error| error.to_string())?;
    } else {
        key.set_value("Environment", &wanted)
            .map_err(|error| error.to_string())?;
    }
    Ok(true)
}

#[cfg(not(windows))]
fn set_relay_only(_relay_only: bool) -> Result<bool, String> {
    Ok(false)
}

/// hides auth keys and ip addresses in text that ends up on screen, like
/// tailscale's own error messages (which echo the whole `up` command back)
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    for c in text.chars() {
        if c.is_whitespace() {
            out.push_str(&redact_word(&word));
            word.clear();
            out.push(c);
        } else {
            word.push(c);
        }
    }
    out.push_str(&redact_word(&word));
    out
}

fn redact_word(word: &str) -> String {
    for flag in ["--auth-key=", "--authkey="] {
        if word.starts_with(flag) {
            return format!("{flag}<hidden>");
        }
    }
    for key in ["hskey-", "tskey-"] {
        if let Some(at) = word.find(key) {
            return format!("{}<hidden>", &word[..at]);
        }
    }
    mask_ipv4(word)
}

/// swaps anything shaped like an ipv4 address for x.x.x.x
fn mask_ipv4(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.trim_matches('.').parse::<std::net::Ipv4Addr>().is_ok() {
            let lead = run.len() - run.trim_start_matches('.').len();
            let tail = run.len() - run.trim_end_matches('.').len();
            out.push_str(&".".repeat(lead));
            out.push_str("x.x.x.x");
            out.push_str(&".".repeat(tail));
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in word.chars() {
        if c.is_ascii_digit() || c == '.' {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

fn query_state() -> Result<String, String> {
    let mut command = Command::new("sc.exe");
    command
        .args(["query", SERVICE_NAME])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let output = command
        .output()
        .map_err(|error| format!("could not query the multiplayer network service: {error}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_sc(args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("sc.exe");
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let output = command
        .output()
        .map_err(|error| format!("failed to run sc.exe: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{} {}", stdout.trim(), stderr.trim());
        Err(combined.trim().to_string())
    }
}

fn run_tailscale(cli: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new(cli);
    command
        .arg(format!("--socket={SERVICE_PIPE}"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let output = command
        .output()
        .map_err(|error| format!("failed to run the multiplayer network helper: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.trim();
        Err(if message.is_empty() {
            format!("tailscale exited with an error ({:?})", output.status)
        } else {
            redact(message)
        })
    }
}
