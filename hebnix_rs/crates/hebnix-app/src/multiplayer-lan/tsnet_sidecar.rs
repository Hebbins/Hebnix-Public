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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Sender;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde::Deserialize;

use crate::messages::AppMsg;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const SERVICE_NAME: &str = "HebnixTailscale";
// tailscaled ignores --socket when it runs as a windows service and uses its
// built-in default pipe. the bundled tailscaled/tailscale are built with that
// default (and the adapter name/guid) changed to HebnixTailscale, so this
// matches it and doesn't collide with a normal tailscale install.
const SERVICE_PIPE: &str = r"\\.\pipe\ProtectedPrefix\Administrators\HebnixTailscale\tailscaled";
const SERVICE_START_TIMEOUT: Duration = Duration::from_secs(10);
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

impl TsnetSidecarHandle {
    /// Ensures the `HebnixTailscale` service is installed (pointed at the
    /// `tailscaled.exe`/`wintun.dll` next to `exe_dir`) and running, then
    /// starts a background peer-status poller. Requires administrator
    /// rights (same as the firewall-rule and, previously, TAP-driver setup
    /// this app already needed).
    pub fn spawn(exe_dir: &Path, state_dir: &Path, tx: Sender<AppMsg>) -> Result<Self, String> {
        let tailscaled_exe = exe_dir.join("tailscaled.exe");
        let tailscale_cli = exe_dir.join("tailscale.exe");
        if !tailscaled_exe.is_file() || !tailscale_cli.is_file() {
            return Err(format!(
                "the multiplayer network components are missing from {}",
                exe_dir.display()
            ));
        }

        ensure_service(&tailscaled_exe, state_dir)?;

        let poll_stop = Arc::new(AtomicBool::new(false));
        spawn_peer_poller(tailscale_cli.clone(), tx.clone(), poll_stop.clone());

        Ok(Self {
            tailscale_cli,
            tx,
            poll_stop,
        })
    }

    pub fn request_up(&self, auth_key: String, hostname: String, control_url: String) -> Result<(), String> {
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
                let _ = tx.send(AppMsg::Log(format!("[tsnet] status check failed: {error}")));
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
            address.parse::<std::net::Ipv4Addr>().is_ok().then(|| address.to_string())
        })
        .or_else(|| pick_ipv4(tailscale_ips))
}

fn bring_up(cli: &Path, auth_key: &str, hostname: &str, control_url: &str) -> Result<String, String> {
    run_tailscale(
        cli,
        &[
            "up",
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
    pick_ipv4(status.tailscale_ips)
        .ok_or_else(|| "connected, but the multiplayer network did not assign an address".to_string())
}

fn fetch_status(cli: &Path) -> Result<RawStatus, String> {
    let raw = run_tailscale(cli, &["status", "--json"])?;
    serde_json::from_str(&raw).map_err(|error| format!("could not understand the multiplayer network's status: {error}"))
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
fn ensure_service(tailscaled_exe: &Path, state_dir: &Path) -> Result<(), String> {
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
            return Err(format!("could not install the multiplayer network service: {error}"));
        }
        // Already installed from a previous run -- keep its binPath current
        // in case Hebnix was reinstalled to a different folder.
        let _ = run_sc(&["config", SERVICE_NAME, "binPath=", &bin_path]);
    }

    if !state_is_running(&query_state()?) {
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
            digits.parse::<u8>().ok().filter(|code| (1..=7).contains(code))
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

fn query_state() -> Result<String, String> {
    let mut command = Command::new("sc.exe");
    command.args(["query", SERVICE_NAME]).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let output = command
        .output()
        .map_err(|error| format!("could not query the multiplayer network service: {error}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_sc(args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("sc.exe");
    command.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
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
            message.to_string()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const RUNNING: &str = "SERVICE_NAME: HebnixTailscale\n        TYPE               : 10  WIN32_OWN_PROCESS\n        STATE              : 4  RUNNING\n                                (STOPPABLE, NOT_PAUSABLE, ACCEPTS_SHUTDOWN)\n        WIN32_EXIT_CODE    : 0  (0x0)\n        SERVICE_EXIT_CODE  : 0  (0x0)\n";

    #[test]
    fn the_service_state_is_read_from_its_number() {
        assert_eq!(service_state_code(RUNNING), Some(4));
        assert!(state_is_running(RUNNING));
        // translated labels and a stopped service
        let spanish = RUNNING.replace("TYPE  ", "TIPO  ").replace("STATE ", "ESTADO");
        assert!(state_is_running(&spanish));
        let stopped = RUNNING.replace("4  RUNNING", "1  STOPPED");
        assert_eq!(service_state_code(&stopped), Some(1));
        assert!(!state_is_running(&stopped));
        // a missing service prints an error, not a state
        let missing = "[SC] EnumQueryServicesStatus:OpenService FAILED 1060:\n\nThe specified service does not exist.";
        assert_eq!(service_state_code(missing), None);
        assert!(!state_is_running(missing));
    }

    #[test]
    fn status_json_with_null_peer_and_ips_parses() {
        let json = r#"{"BackendState":"Running","TailscaleIPs":null,"Peer":null}"#;
        let status: RawStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status.backend_state, "Running");
        assert!(status.tailscale_ips.is_empty());
        assert!(status.peer.is_empty());
    }

    // Drives the real bundled tailscaled/tailscale binaries through
    // spawn -> status -> shutdown. Ignored by default because it needs
    // administrator rights (installs a real Windows service) and the
    // binaries built per sidecar/README.md, passed via HEBNIX_TS_BIN_DIR;
    // this isn't wired into the normal cargo build/CI.
    //
    // Run (from an elevated shell) with:
    //   HEBNIX_TS_BIN_DIR=D:/hebnix/hebnix_rs/sidecar \
    //     cargo test -p hebnix-app --release tsnet_sidecar::tests -- --ignored --nocapture
    #[test]
    #[ignore = "requires admin rights and the bundled tailscaled/tailscale binaries"]
    fn spawn_status_and_shutdown_round_trip() {
        let bin_dir = std::env::var("HEBNIX_TS_BIN_DIR")
            .expect("set HEBNIX_TS_BIN_DIR to the folder containing tailscaled.exe/tailscale.exe/wintun.dll");
        let state_dir = std::env::temp_dir().join("hebnix-tsnet-sidecar-test-state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let (tx, rx) = crossbeam_channel::unbounded();
        let mut handle = TsnetSidecarHandle::spawn(Path::new(&bin_dir), &state_dir, tx)
            .expect("sidecar failed to spawn / install the service");

        handle.request_status().expect("failed to send status command");

        let message = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("no response from sidecar within 5s");
        match message {
            AppMsg::TsnetStatus { state, .. } => {
                assert_ne!(state, TsState::Connected, "fresh sidecar should not already be connected");
            }
            other => panic!("expected TsnetStatus, got {other:?}"),
        }

        handle.request_shutdown().expect("failed to send shutdown command");
        assert!(
            handle.wait_for_exit(Duration::from_secs(10)),
            "service did not stop after shutdown command"
        );
    }
}
