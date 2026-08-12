//! MITM hosts-file DNS steering (9router `src/mitm/dns/dnsConfig.js`).
//!
//! Writes `127.0.0.1 <toolhost>` entries into the OS hosts file so tool
//! traffic is steered to the local MITM proxy, and removes them on shutdown.
//! Line endings: CRLF on Windows, LF elsewhere (9router dnsConfig.js:161-168).

use std::path::PathBuf;

/// Tool → upstream hosts that must be steered to the MITM proxy
/// (9router `src/shared/constants/mitmToolHosts.js`).
pub const TOOL_HOSTS: &[(&str, &[&str])] = &[
    (
        "antigravity",
        &[
            "daily-cloudcode-pa.googleapis.com",
            "cloudcode-pa.googleapis.com",
        ],
    ),
    ("copilot", &["api.individual.githubcopilot.com"]),
    (
        "kiro",
        &[
            "runtime.us-east-1.kiro.dev",
            "q.us-east-1.amazonaws.com",
            "codewhisperer.us-east-1.amazonaws.com",
        ],
    ),
    ("cursor", &["api2.cursor.sh"]),
];

/// The loopback host all steered tool hosts resolve to.
const LOOPBACK: &str = "127.0.0.1";

fn hosts_path() -> PathBuf {
    // Windows: %SystemRoot%\System32\drivers\etc\hosts; elsewhere /etc/hosts.
    #[cfg(windows)]
    {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        PathBuf::from(system_root)
            .join("System32")
            .join("drivers")
            .join("etc")
            .join("hosts")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/etc/hosts")
    }
}

fn line_ending() -> &'static str {
    #[cfg(windows)]
    {
        "\r\n"
    }
    #[cfg(not(windows))]
    {
        "\n"
    }
}

/// All tool hosts (flat list) for filtering.
fn all_tool_hosts() -> Vec<&'static str> {
    TOOL_HOSTS
        .iter()
        .flat_map(|(_, hosts)| hosts.iter().copied())
        .collect()
}

/// Add `127.0.0.1 <host>` entries for the given tool's hosts to `path`.
/// Unrelated existing lines are preserved. Returns the entries added.
pub fn add_dns_entry(tool: &str, path: &PathBuf) -> Result<Vec<String>, String> {
    let hosts = TOOL_HOSTS
        .iter()
        .find(|(t, _)| *t == tool)
        .map(|(_, h)| *h)
        .ok_or_else(|| format!("unknown tool: {tool}"))?;
    let eol = line_ending();
    let mut content = std::fs::read_to_string(path).unwrap_or_default();
    let mut added = Vec::new();
    for host in hosts {
        let entry = format!("{LOOPBACK} {host}");
        if content.lines().any(|l| l.trim() == entry.as_str()) {
            continue;
        }
        content.push_str(&format!("{entry}{eol}"));
        added.push(entry);
    }
    std::fs::write(path, content).map_err(|e| format!("write hosts: {e}"))?;
    Ok(added)
}

/// Remove all `127.0.0.1 <toolhost>` entries (any tool) from `path`, leaving
/// unrelated lines intact. Returns the number of entries removed.
pub fn remove_all_dns_entries(path: &PathBuf) -> Result<usize, String> {
    let tool_hosts = all_tool_hosts();
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let kept: Vec<String> = content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !(trimmed.starts_with(LOOPBACK)
                && tool_hosts
                    .iter()
                    .any(|h| trimmed == format!("{LOOPBACK} {h}")))
        })
        .map(|l| l.to_string())
        .collect();
    let removed = content.lines().count() - kept.len();
    let eol = line_ending();
    std::fs::write(path, kept.join(eol) + eol).map_err(|e| format!("write hosts: {e}"))?;
    Ok(removed)
}

/// Best-effort DNS cache flush after editing the hosts file.
pub fn flush_dns() {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("ipconfig").arg("/flushdns").status();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("dscacheutil")
            .arg("-flushcache")
            .status();
        let _ = std::process::Command::new("killall")
            .args(["-HUP", "mDNSResponder"])
            .status();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("resolvectl")
            .arg("flush-caches")
            .status();
    }
}

/// Add DNS entries for a tool to the default hosts file and flush DNS.
pub fn add_dns_entry_default(tool: &str) -> Result<Vec<String>, String> {
    let path = hosts_path();
    let added = add_dns_entry(tool, &path)?;
    flush_dns();
    Ok(added)
}

/// Remove all MITM DNS entries from the default hosts file and flush DNS.
pub fn remove_all_dns_entries_default() -> Result<usize, String> {
    let path = hosts_path();
    let removed = remove_all_dns_entries(&path)?;
    flush_dns();
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_hosts() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        std::fs::write(&path, "127.0.0.1 localhost\n::1 localhost\n").unwrap();
        (dir, path)
    }

    #[test]
    fn mitm_hosts_entries_write_loopback() {
        let (_dir, path) = temp_hosts();
        add_dns_entry("antigravity", &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("127.0.0.1 daily-cloudcode-pa.googleapis.com"));
        assert!(content.contains("127.0.0.1 cloudcode-pa.googleapis.com"));
        // Unrelated lines preserved.
        assert!(content.contains("127.0.0.1 localhost"));
    }

    #[test]
    fn mitm_hosts_remove_all_dns_entries() {
        let (_dir, path) = temp_hosts();
        add_dns_entry("antigravity", &path).unwrap();
        add_dns_entry("copilot", &path).unwrap();
        let removed = remove_all_dns_entries(&path).unwrap();
        assert_eq!(removed, 3); // 2 antigravity + 1 copilot
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("daily-cloudcode-pa.googleapis.com"));
        assert!(!content.contains("api.individual.githubcopilot.com"));
        assert!(content.contains("127.0.0.1 localhost"));
    }

    #[test]
    fn mitm_hosts_add_is_idempotent() {
        let (_dir, path) = temp_hosts();
        add_dns_entry("kiro", &path).unwrap();
        let second = add_dns_entry("kiro", &path).unwrap();
        assert!(second.is_empty(), "no duplicate entries on re-add");
    }
}
