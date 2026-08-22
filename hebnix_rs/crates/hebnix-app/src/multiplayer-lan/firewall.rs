use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

const RULE_PREFIX: &str = "Hebnix Workshop LAN";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const LAN_PORTS: &str = "7777-7778,14000-14010";
const PROFILES: &str = "private,public";

pub fn ensure_host_rule(executable: &Path, port: u16) -> Result<(), String> {
    let inbound = format!("{RULE_PREFIX} v2 tunnel host inbound UDP {port}");
    ensure_udp_rule(
        &inbound,
        executable,
        "in",
        Some(&format!("localport={port}")),
        None,
        None,
    )?;
    if outbound_is_blocked()? {
        let outbound = format!("{RULE_PREFIX} v2 tunnel host outbound UDP {port}");
        ensure_udp_rule(
            &outbound,
            executable,
            "out",
            Some(&format!("localport={port}")),
            None,
            None,
        )?;
    }
    Ok(())
}

pub fn ensure_join_rule_if_needed(
    executable: &Path,
    host_ip: &str,
    host_port: u16,
) -> Result<(), String> {
    let inbound = format!("{RULE_PREFIX} v2 tunnel guest inbound UDP {host_ip}:{host_port}");
    ensure_udp_rule(
        &inbound,
        executable,
        "in",
        None,
        Some(&format!("remoteport={host_port}")),
        Some(host_ip),
    )?;
    if outbound_is_blocked()? {
        let outbound = format!("{RULE_PREFIX} v2 tunnel guest outbound UDP {host_ip}:{host_port}");
        ensure_udp_rule(
            &outbound,
            executable,
            "out",
            None,
            Some(&format!("remoteport={host_port}")),
            Some(host_ip),
        )?;
    }
    Ok(())
}

pub fn ensure_rocket_league_lan_rule(executable: &Path, remote_ip: &str) -> Result<(), String> {
    let inbound = format!("{RULE_PREFIX} v2 Rocket League LAN inbound from {remote_ip}");
    ensure_udp_rule(
        &inbound,
        executable,
        "in",
        Some(&format!("localport={LAN_PORTS}")),
        None,
        Some(remote_ip),
    )?;
    if outbound_is_blocked()? {
        let outbound = format!("{RULE_PREFIX} v2 Rocket League LAN outbound to {remote_ip}");
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
        "protocol=UDP".to_string(),
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
