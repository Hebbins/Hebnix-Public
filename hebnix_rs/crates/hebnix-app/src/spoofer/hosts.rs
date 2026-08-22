//! hosts file redirect, the game ignores the wininet proxy. needs admin, the
//! file is in System32.

use std::path::{Path, PathBuf};

// our lines end with this so we can find them later
pub const MARK: &str = "# hebnix spoofer";

pub fn hosts_path() -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    Path::new(&root).join(r"System32\drivers\etc\hosts")
}

pub fn is_writable() -> bool {
    std::fs::OpenOptions::new()
        .append(true)
        .open(hosts_path())
        .is_ok()
}

fn line_for(host: &str) -> String {
    format!("127.0.0.1 {host} {MARK}")
}

/// point host at us. does nothing if its already there.
pub fn set_redirects(hosts: &[&str]) -> Result<(), String> {
    let path = hosts_path();
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("cant read the hosts file: {e}"))?;
    let mut kept = content
        .lines()
        .filter(|line| !is_ours(line))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for host in hosts {
        kept.push(line_for(host));
    }
    let mut out = kept.join("\r\n");
    if !out.is_empty() {
        out.push_str("\r\n");
    }
    write(&path, &out)?;
    flush_dns();
    Ok(())
}

pub fn has_redirects() -> bool {
    std::fs::read_to_string(hosts_path())
        .map(|content| content.lines().any(is_ours))
        .unwrap_or(false)
}

/// drops our lines, whatever host they were for
pub fn clear() -> Result<(), String> {
    let path = hosts_path();
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("cant read the hosts file: {e}"))?;

    if !content.lines().any(is_ours) {
        return Ok(());
    }

    let kept: Vec<&str> = content.lines().filter(|l| !is_ours(l)).collect();
    let mut out = kept.join("\r\n");
    if !out.is_empty() {
        out.push_str("\r\n");
    }

    write(&path, &out)?;
    flush_dns();
    Ok(())
}

fn is_ours(line: &str) -> bool {
    line.contains(MARK)
}

fn write(path: &Path, content: &str) -> Result<(), String> {
    std::fs::write(path, content).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            "the hosts file needs administrator, restart hebnix as admin".to_string()
        } else {
            format!("cant write the hosts file: {e}")
        }
    })
}

pub fn flush_dns() {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("ipconfig")
        .arg("/flushdns")
        .creation_flags(0x08000000)
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_our_lines_are_ours() {
        assert!(is_ours(&line_for("config.psynet.gg")));
        assert!(!is_ours("127.0.0.1 localhost"));
        assert!(!is_ours("# some user comment"));
    }

    #[test]
    fn line_carries_host_and_mark() {
        let l = line_for("config.psynet.gg");
        assert!(l.starts_with("127.0.0.1 config.psynet.gg"));
        assert!(l.contains(MARK));
    }
}
