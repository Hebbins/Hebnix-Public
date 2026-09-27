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

/// Ensure exactly one Hebnix redirect per requested host. Leave the file
/// untouched when it already has the desired entries.
pub fn set_redirects(hosts: &[&str]) -> Result<(), String> {
    let path = hosts_path();
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("cant read the hosts file: {e}"))?;
    if redirects_match(&content, hosts) {
        return Ok(());
    }

    let out = with_redirects(&content, hosts);
    write(&path, &out)?;
    let verified = std::fs::read_to_string(&path)
        .map_err(|e| format!("cant verify hosts redirects: {e}"))?;
    if !redirects_match(&verified, hosts) {
        return Err("Hebnix hosts redirects did not match the requested hosts".into());
    }
    flush_dns();
    Ok(())
}

fn redirects_match(content: &str, hosts: &[&str]) -> bool {
    let ours: Vec<&str> = content.lines().filter(|line| is_ours(line)).collect();
    ours.len() == hosts.len()
        && hosts.iter().all(|host| {
            ours.iter()
                .filter(|line| line.trim().eq_ignore_ascii_case(&line_for(host)))
                .count() == 1
        })
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

    let out = without_redirects(&content);
    write(&path, &out)?;
    let verified = std::fs::read_to_string(&path)
        .map_err(|e| format!("cant verify hosts cleanup: {e}"))?;
    if verified.lines().any(is_ours) {
        return Err("Hebnix redirects remain in the hosts file after cleanup".into());
    }
    flush_dns();
    Ok(())
}

fn is_ours(line: &str) -> bool {
    let Some((_, comment)) = line.rsplit_once('#') else { return false };
    matches!(comment.trim().to_ascii_lowercase().as_str(), "hebnix spoofer" | "hebnix")
}

fn without_redirects(content: &str) -> String {
    content
        .split_inclusive('\n')
        .filter(|line| !is_ours(line.trim_end_matches(['\r', '\n'])))
        .collect()
}

fn with_redirects(content: &str, hosts: &[&str]) -> String {
    let mut out = without_redirects(content);
    let newline = if content.contains("\r\n") { "\r\n" } else { "\n" };
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(newline);
    }
    for host in hosts {
        out.push_str(&line_for(host));
        out.push_str(newline);
    }
    out
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
        assert!(is_ours("127.0.0.1 api.epicgames.dev #hebnix"));
    }

    #[test]
    fn redirects_match_only_exact_unique_entries() {
        let hosts = ["api.epicgames.dev", "config.psynet.gg"];
        let content = format!("# user line\r\n{}\r\n{}\r\n",
            line_for(hosts[0]), line_for(hosts[1]));
        assert!(redirects_match(&content, &hosts));
        assert!(!redirects_match(&(content.clone() + &line_for(hosts[0])), &hosts));
        assert!(!redirects_match(&content, &hosts[..1]));
        assert!(!redirects_match("127.0.0.1 api.epicgames.dev #hebnix", &hosts[..1]));
    }

    #[test]
    fn cleanup_preserves_other_hosts_and_line_endings() {
        let original = "# personal\n127.0.0.1 localhost\n127.0.0.1 config.psynet.gg # hebnix spoofer\n# trailing note\n";
        assert_eq!(without_redirects(original), "# personal\n127.0.0.1 localhost\n# trailing note\n");
        assert_eq!(with_redirects(original, &["config.psynet.gg"]), original);

        let windows = "127.0.0.1 other.example # user\r\n127.0.0.1 config.psynet.gg # hebnix spoofer\r\n";
        assert_eq!(without_redirects(windows), "127.0.0.1 other.example # user\r\n");
        assert_eq!(with_redirects(windows, &["config.psynet.gg"]), windows);
        assert!(!is_ours("127.0.0.1 other.example # hebnix notes"));
    }

    #[test]
    fn line_carries_host_and_mark() {
        let l = line_for("config.psynet.gg");
        assert!(l.starts_with("127.0.0.1 config.psynet.gg"));
        assert!(l.contains(MARK));
    }
}
