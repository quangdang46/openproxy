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
        self.stop().await.ok();

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

        let tunnel_url = if provider == TunnelProvider::Cloudflare {
            if let Some(stdout) = child.stdout.take() {
                let mut reader = BufReader::new(stdout).lines();
                tokio::time::timeout(Duration::from_secs(30), async {
                    while let Ok(Some(line)) = reader.next_line().await {
                        if line.contains("trycloudflare.com") || line.contains("https://") {
                            let url = extract_url(&line);
                            if url.is_some() {
                                return url;
                            }
                        }
                    }
                    None
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
                settings.tunnel_enabled = true;
                settings.tunnel_url = tunnel_url.unwrap_or_default();
                settings.tunnel_provider = provider.to_string();
            })
            .await?;

        Ok(())
    }

    pub async fn stop(&self) -> anyhow::Result<()> {
        if let Some(mut child) = self.process.write().await.take() {
            child.kill().await.ok();
        }

        *self.status.write().await = TunnelStatus::default();

        self.db
            .update(|db| {
                let settings = &mut db.settings;
                settings.tunnel_enabled = false;
                settings.tunnel_url = String::new();
                settings.tunnel_provider = String::new();
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
