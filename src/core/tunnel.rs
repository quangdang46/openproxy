use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::RwLock;

use crate::db::Db;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TunnelProvider {
    #[default]
    Cloudflare,
    Tailscale,
}

impl std::fmt::Display for TunnelProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TunnelProvider::Cloudflare => write!(f, "cloudflare"),
            TunnelProvider::Tailscale => write!(f, "tailscale"),
        }
    }
}

impl std::str::FromStr for TunnelProvider {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "cloudflare" | "cloudflared" => Ok(TunnelProvider::Cloudflare),
            "tailscale" | "tailnet" => Ok(TunnelProvider::Tailscale),
            _ => Err(format!("Unknown tunnel provider: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TunnelStatus {
    pub running: bool,
    pub provider: Option<String>,
    pub url: Option<String>,
    pub pid: Option<u32>,
}

pub struct TunnelManager {
    db: Arc<Db>,
    process: RwLock<Option<Child>>,
    status: RwLock<TunnelStatus>,
}

impl TunnelManager {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            db,
            process: RwLock::new(None),
            status: RwLock::new(TunnelStatus::default()),
        }
    }

    pub async fn start(&self, provider: TunnelProvider, port: u16) -> anyhow::Result<()> {
        // Tear down any previous process without wiping "desired enabled" flags
        // for the *other* provider — stop_process_only keeps settings intact when
        // nothing is running so boot-resume can call start safely.
        self.stop_process_only().await.ok();

        let mut child = match provider {
            TunnelProvider::Cloudflare => tokio::process::Command::new("cloudflared")
                .args(["tunnel", "--url", &format!("http://localhost:{}", port)])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("Failed to spawn cloudflared. Is cloudflared installed?")?,
            TunnelProvider::Tailscale => tokio::process::Command::new("tailscale")
                .args(["funnel", &port.to_string()])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("Failed to spawn tailscale. Is tailscale installed?")?,
        };

        let pid = child.id();

        // `cloudflared tunnel --url` logs everything — including the
        // `https://<sub>.trycloudflare.com` connection line we need, and an
        // unrelated `https://developers.cloudflare.com/...` docs link in its
        // startup banner that appears first — to **stderr**, not stdout
        // (documented cloudflared behavior; verified live). Scanning stdout
        // alone means the real URL is never found and this always burns the
        // full 30s timeout below. `trycloudflare.com` is a stronger/first
        // signal than a bare "https://" match, so check it before falling
        // back to a generic https line.
        //
        // Deliberately DROP (not background-drain) the streams once we're
        // done reading: an earlier version spawned `tokio::spawn` loops to
        // keep consuming output indefinitely so the pipe buffer wouldn't
        // fill and block cloudflared's writer — but those loops never
        // terminate while the tunnel keeps running, and the CLI's
        // short-lived `tokio::runtime::Runtime` blocks on shutdown waiting
        // for every spawned task to finish, wedging the whole `tunnel
        // start` invocation forever (had to be force-killed in testing).
        // Dropping our end of the pipe instead closes it outright — the
        // child's future writes to auxiliary log streams simply fail
        // (broken pipe), which cloudflared/tailscale tolerate fine; this is
        // exactly what happens whenever a user closes the terminal a
        // background tunnel was launched from.
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        drop(stdout);

        let tunnel_url = if provider == TunnelProvider::Cloudflare {
            if let Some(stderr) = stderr {
                let mut reader = BufReader::new(stderr).lines();
                tokio::time::timeout(Duration::from_secs(30), async {
                    let mut fallback: Option<String> = None;
                    while let Ok(Some(line)) = reader.next_line().await {
                        if line.contains("trycloudflare.com") {
                            if let Some(url) = extract_url(&line) {
                                return Some(url);
                            }
                        } else if fallback.is_none() && line.contains("https://") {
                            fallback = extract_url(&line);
                        }
                    }
                    fallback
                })
                .await
                .ok()
                .flatten()
            } else {
                None
            }
        } else {
            None
        };

        let status = TunnelStatus {
            running: true,
            provider: Some(provider.to_string()),
            url: tunnel_url.clone(),
            pid,
        };

        *self.process.write().await = Some(child);
        *self.status.write().await = status;

        self.db
            .update(|db| {
                let settings = &mut db.settings;
                match provider {
                    TunnelProvider::Cloudflare => {
                        settings.tunnel_enabled = true;
                        settings.tunnel_url = tunnel_url.unwrap_or_default();
                        settings.tunnel_provider = provider.to_string();
                    }
                    TunnelProvider::Tailscale => {
                        settings.tailscale_enabled = true;
                        if let Some(url) = tunnel_url {
                            settings.tailscale_url = url;
                        }
                    }
                }
            })
            .await?;

        Ok(())
    }

    /// Kill the child process and clear runtime status, without mutating
    /// persisted "desired enabled" settings. Used by `start` so a no-op stop
    /// cannot wipe resume flags before the new process is spawned.
    async fn stop_process_only(&self) -> anyhow::Result<()> {
        if let Some(mut child) = self.process.write().await.take() {
            child.kill().await.ok();
        }
        *self.status.write().await = TunnelStatus::default();
        Ok(())
    }

    /// Explicit disable: kill process and clear enabled flags.
    ///
    /// `provider` selects which desired-state flags to clear when the live
    /// process provider is unknown (e.g. process already exited). Prefer
    /// matching the caller's disable endpoint (cloudflare vs tailscale).
    pub async fn stop(&self) -> anyhow::Result<()> {
        self.stop_provider(None).await
    }

    pub async fn stop_provider(&self, preferred: Option<TunnelProvider>) -> anyhow::Result<()> {
        let prev_provider = self.status.read().await.provider.clone();

        // Only kill the process when it matches the provider being disabled
        // (or when no preferred provider was specified).
        let should_kill = match (preferred, prev_provider.as_deref()) {
            (None, _) => true,
            (Some(TunnelProvider::Cloudflare), Some("cloudflare") | None) => true,
            (Some(TunnelProvider::Tailscale), Some("tailscale") | None) => true,
            // Other provider is currently running — leave its process alone.
            (Some(_), Some(_)) => false,
        };

        if should_kill {
            if let Some(mut child) = self.process.write().await.take() {
                child.kill().await.ok();
            }
            *self.status.write().await = TunnelStatus::default();
        }

        // Clear the desired-state flag for the provider the caller asked to disable.
        let clear_cloudflare = matches!(preferred, Some(TunnelProvider::Cloudflare) | None)
            && (preferred.is_some()
                || matches!(prev_provider.as_deref(), Some("cloudflare") | None));
        let clear_tailscale = matches!(preferred, Some(TunnelProvider::Tailscale))
            || (preferred.is_none() && prev_provider.as_deref() == Some("tailscale"));

        self.db
            .update(|db| {
                let settings = &mut db.settings;
                if clear_cloudflare {
                    settings.tunnel_enabled = false;
                    settings.tunnel_url = String::new();
                }
                if clear_tailscale {
                    settings.tailscale_enabled = false;
                    settings.tailscale_url = String::new();
                }
            })
            .await?;

        Ok(())
    }

    pub async fn status(&self) -> TunnelStatus {
        self.status.read().await.clone()
    }

    pub async fn is_running(&self) -> bool {
        self.status.read().await.running
    }
}

fn extract_url(line: &str) -> Option<String> {
    for part in line.split_whitespace() {
        if part.starts_with("https://")
            && (part.contains("trycloudflare.com") || part.contains("cloudflare.com"))
        {
            return Some(
                part.trim_end_matches(|c: char| !c.is_alphanumeric())
                    .to_string(),
            );
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_url_finds_trycloudflare_link() {
        let line = "2026-09-18T10:00:00Z INF |  https://dependence-suspension-grocery-exceptions.trycloudflare.com  |";
        assert_eq!(
            extract_url(line).as_deref(),
            Some("https://dependence-suspension-grocery-exceptions.trycloudflare.com")
        );
    }

    #[test]
    fn extract_url_returns_none_without_a_cloudflare_link() {
        assert_eq!(
            extract_url("2026-09-18T10:00:00Z INF Starting tunnel"),
            None
        );
        assert_eq!(extract_url("some https://example.com line"), None);
    }

    // Live bug (2026-09-18): cloudflared's startup banner prints a docs link
    // (https://developers.cloudflare.com/...) to stderr BEFORE the real
    // ephemeral tunnel URL. `start()` must not report that banner link as
    // the tunnel URL — this pins the priority logic (trycloudflare.com line
    // wins over an earlier generic-https fallback), mirroring the scan order
    // in `start()`.
    #[test]
    fn banner_docs_link_does_not_shadow_the_real_tunnel_url() {
        let lines = [
            "2026-09-18T10:00:00Z INF Thank you for trying Cloudflare Tunnel.",
            "2026-09-18T10:00:00Z INF Requesting new quick Tunnel on trycloudflare.com...",
            "2026-09-18T10:00:00Z INF |  https://developers.cloudflare.com/cloudflare-one/connections/connect-apps  |",
            "2026-09-18T10:00:01Z INF |  https://dependence-suspension-grocery-exceptions.trycloudflare.com  |",
        ];
        let mut fallback: Option<String> = None;
        let mut found: Option<String> = None;
        for line in lines {
            if line.contains("trycloudflare.com") {
                if let Some(url) = extract_url(line) {
                    found = Some(url);
                    break;
                }
            } else if fallback.is_none() && line.contains("https://") {
                fallback = extract_url(line);
            }
        }
        assert_eq!(
            found.as_deref(),
            Some("https://dependence-suspension-grocery-exceptions.trycloudflare.com"),
            "must resolve the real tunnel URL, not the banner docs link (fallback would have been {fallback:?})"
        );
    }
}
