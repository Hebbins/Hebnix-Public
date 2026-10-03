use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

const RULE_PREFIX: &str = "Hebnix Workshop LAN";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const LAN_PORTS: &str = "7777-7778,14000-14010,14777";
const DISCOVERY_PORTS: &str = "14000-14010,14777";
const PROFILES: &str = "private,public";
/// the whole tailnet, same range the Rocket League rule below is scoped to
pub const TAILNET_SUBNET: &str = "10.242.77.0/24";

/// Tailscale doesn't guarantee a fixed outbound port the way the old
/// UPnP/STUN tunnel did, so this is scoped by executable (tailscaled.exe)
/// only rather than a port number. tailscaled also manages its own WFP
/// firewall rules internally; this is defense in depth on top of that.
pub fn ensure_sidecar_rule(executable: &Path) -> Result<(), String> {
    let outbound = format!("{RULE_PREFIX} v3 tailscaled outbound UDP");
    ensure_udp_rule(&outbound, executable, "out", None, None, None)
}

/// lets Hebnix's own beacon-relay socket (see beacon.rs) send/receive on
/// Rocket League's LAN discovery ports -- separate from the rule below,
/// which is scoped to RocketLeague.exe rather than Hebnix's own binary.
pub fn ensure_beacon_relay_rule(executable: &Path) -> Result<(), String> {
    let ports = DISCOVERY_PORTS;
    let inbound = format!("{RULE_PREFIX} v3 beacon relay inbound UDP {ports}");
    ensure_udp_rule(
        &inbound,
        executable,
        "in",
        Some(&format!("localport={ports}")),
        None,
        None,
    )?;
    let outbound = format!("{RULE_PREFIX} v3 beacon relay outbound UDP {ports}");
    ensure_udp_rule(
        &outbound,
        executable,
        "out",
        Some(&format!("localport={ports}")),
        None,
        None,
    )
}

/// lets peers reach Hebnix's map sync listener (see map_sync.rs) -- TCP,
/// tailnet only
pub fn ensure_map_sync_rule(executable: &Path) -> Result<(), String> {
    let port = super::map_sync::MAP_SYNC_PORT;
    let inbound = format!("{RULE_PREFIX} v3 map sync inbound TCP {port}");
    ensure_rule(
        &inbound,
        "TCP",
        executable,
        "in",
        Some(&format!("localport={port}")),
        None,
        Some(TAILNET_SUBNET),
    )?;
    if outbound_is_blocked()? {
        let outbound = format!("{RULE_PREFIX} v3 map sync outbound TCP {port}");
        ensure_rule(
            &outbound,
            "TCP",
            executable,
            "out",
            Some(&format!("remoteport={port}")),
            None,
            Some(TAILNET_SUBNET),
        )?;
    }
    Ok(())
}

pub fn ensure_rocket_league_lan_rule(executable: &Path, remote_ip: &str) -> Result<(), String> {
    let inbound = format!("{RULE_PREFIX} v3 Rocket League LAN inbound from {remote_ip}");
    ensure_udp_rule(
        &inbound,
        executable,
        "in",
        Some(&format!("localport={LAN_PORTS}")),
        None,
        Some(remote_ip),
    )?;
    if outbound_is_blocked()? {
        let outbound = format!("{RULE_PREFIX} v3 Rocket League LAN outbound to {remote_ip}");
        ensure_udp_rule(
            &outbound,
            executable,
            "out",
            Some(&format!("localport={LAN_PORTS}")),
            None,
            Some(remote_ip),
        )?;
    }
    Ok(())
}

pub fn remove_rules() -> Result<(), String> {
    let script = format!(
        "Get-NetFirewallRule -ErrorAction SilentlyContinue | Where-Object {{ $_.DisplayName -like '{}*' }} | Remove-NetFirewallRule -ErrorAction SilentlyContinue",
        RULE_PREFIX
    );
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("could not remove Workshop firewall rules: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn ensure_udp_rule(
    name: &str,
    executable: &Path,
    direction: &str,
    local_port: Option<&str>,
    remote_port: Option<&str>,
    remote_ip: Option<&str>,
) -> Result<(), String> {
    ensure_rule(
        name,
        "UDP",
        executable,
        direction,
        local_port,
        remote_port,
        remote_ip,
    )
}

fn ensure_rule(
    name: &str,
    protocol: &str,
    executable: &Path,
    direction: &str,
    local_port: Option<&str>,
    remote_port: Option<&str>,
    remote_ip: Option<&str>,
) -> Result<(), String> {
    if rule_exists(name)? {
        return Ok(());
    }
    let program = executable
        .to_str()
        .ok_or_else(|| "executable path is not valid Unicode".to_string())?;
    let mut args = vec![
        "advfirewall".to_string(),
        "firewall".to_string(),
        "add".to_string(),
        "rule".to_string(),
        format!("name={name}"),
        format!("dir={direction}"),
        "action=allow".to_string(),
        format!("protocol={protocol}"),
        format!("program={program}"),
        format!("profile={PROFILES}"),
        "enable=yes".to_string(),
    ];
    if let Some(value) = local_port {
        args.push(value.to_string());
    }
    if let Some(value) = remote_port {
        args.push(value.to_string());
    }
    if let Some(value) = remote_ip {
        args.push(format!("remoteip={value}"));
    }
    run_netsh(&args)
}

fn rule_exists(name: &str) -> Result<bool, String> {
    let output = Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "show",
            "rule",
            &format!("name={name}"),
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("could not query Windows Firewall: {error}"))?;
    Ok(output.status.success()
        && !String::from_utf8_lossy(&output.stdout).contains("No rules match"))
}

fn outbound_is_blocked() -> Result<bool, String> {
    Ok(profile_outbound_is_blocked("private")? || profile_outbound_is_blocked("public")?)
}

fn profile_outbound_is_blocked(profile: &str) -> Result<bool, String> {
    let output = Command::new("netsh")
        .args(["advfirewall", "show", &format!("{profile}profile")])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("could not query the {profile} firewall profile: {error}"))?;
    if !output.status.success() {
        return Err(format!("could not query the {profile} firewall profile"));
    }
    let text = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    Ok(
        text.contains("outboundconnections") && text.contains("block")
            || text.contains("outbound connections") && text.contains("block"),
    )
}

fn run_netsh(args: &[String]) -> Result<(), String> {
    let output = Command::new("netsh")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("could not update Windows Firewall: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}
