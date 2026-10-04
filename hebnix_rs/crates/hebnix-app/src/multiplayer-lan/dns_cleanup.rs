//! removes a stale Tailscale NRPT (DNS policy) rule, if one is left behind.
//!
//! tailscaled's Windows DNS manager writes a fixed, hardcoded registry key
//! (the same one for any tailscaled build, ours or a real install) whenever
//! it's told to manage MagicDNS resolvers:
//!   HKLM\SOFTWARE\Policies\Microsoft\Windows NT\DNSClient\DnsPolicyConfig\{5abe529b-675b-4486-8459-25a634dacc23}
//! pointing every DNS query at 100.100.100.100. It normally removes this on
//! a clean shutdown, but a crash, a forced service stop, or Windows losing
//! power mid-session can leave it behind with nothing actually listening at
//! that address -- which silently breaks DNS for every app on the machine
//! (nslookup still works since it bypasses the Windows resolver, which is
//! why this is easy to miss).
//!
//! we ship with headscale's magic_dns turned off for Workshop multiplayer
//! (it never needed MagicDNS names, only raw tailnet IPs), so this rule
//! should never get written by our own sidecar going forward. This check is
//! a safety net for whatever's already stuck from before that, or from a
//! real Tailscale install that crashed the same way -- it only removes the
//! rule if nothing answers at 100.100.100.100, so a real, working MagicDNS
//! setup (ours or the user's own Tailscale) is left alone.

use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use winreg::RegKey;
use winreg::enums::HKEY_LOCAL_MACHINE;

const NRPT_RULE_ID: &str = "{5abe529b-675b-4486-8459-25a634dacc23}";
const MAGIC_DNS_IP: &str = "100.100.100.100";
const PROBE_TIMEOUT: Duration = Duration::from_millis(300);

fn dns_policy_config_path() -> String {
    format!(r"SOFTWARE\Policies\Microsoft\Windows NT\DNSClient\DnsPolicyConfig\{NRPT_RULE_ID}")
}

/// true if something is actually listening at MagicDNS's address (port 53,
/// where the real DNS manager would be) -- a quick, best-effort check, not
/// a real DNS query, just "is anyone home".
fn magic_dns_responding() -> bool {
    let addr: SocketAddr = format!("{MAGIC_DNS_IP}:53").parse().expect("valid address");
    TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).is_ok()
}

/// checks for the stale rule and removes it if nothing answers at
/// 100.100.100.100. call this once at Hebnix startup, before anything else
/// touches the network -- logs what it finds either way via `log`.
pub fn clean_stale_nrpt_rule(log: impl Fn(&str)) {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let path = dns_policy_config_path();

    let rule_exists = hklm.open_subkey(&path).is_ok();
    if !rule_exists {
        return;
    }

    if magic_dns_responding() {
        // a real MagicDNS resolver (ours or the user's own Tailscale) is
        // actually up and serving this -- leave it alone.
        return;
    }

    log(&format!(
        "[Core] Found a stale Tailscale DNS policy rule ({NRPT_RULE_ID}) with nothing \
         listening at {MAGIC_DNS_IP} -- this breaks DNS system-wide if left in place. Removing it."
    ));

    match hklm.open_subkey_with_flags(
        r"SOFTWARE\Policies\Microsoft\Windows NT\DNSClient\DnsPolicyConfig",
        winreg::enums::KEY_SET_VALUE | winreg::enums::KEY_ENUMERATE_SUB_KEYS,
    ) {
        Ok(parent) => {
            if let Err(error) = parent.delete_subkey_all(NRPT_RULE_ID) {
                log(&format!(
                    "[Core] Could not remove the stale DNS policy rule: {error}. \
                     You can remove it yourself with: Remove-DnsClientNrptRule -DisplayName \"{NRPT_RULE_ID}\""
                ));
            } else {
                log(
                    "[Core] Removed the stale DNS policy rule. DNS resolution should be back to normal.",
                );
            }
        }
        Err(error) => {
            log(&format!(
                "[Core] Could not open the DNS policy registry key to remove the stale rule: {error}"
            ));
        }
    }
}
