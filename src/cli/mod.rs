use std::path::PathBuf;

use clap::{Parser, Subcommand, CommandFactory};
use clap_complete::{Generator, Shell};
use crate::core::tunnel::{TunnelManager, TunnelProvider};

use crate::db::Db;
use crate::types::{ProviderConnection, ApiKey, ProxyPool};

#[derive(Debug, Clone, Parser)]
#[command(name = "openproxy", about = "Local AI routing gateway")]
pub struct Cli {
    #[arg(long, env = "HOST", default_value = "0.0.0.0")]
    pub host: String,

    #[arg(long, env = "PORT", default_value_t = 20128)]
    pub port: u16,

    #[arg(long, env = "RUST_LOG", default_value = "info")]
    pub log_filter: String,

    #[arg(long, env = "DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub cmd: Option<Command>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    Provider {
        #[command(subcommand)]
        cmd: ProviderCmd,
    },
    Key {
        #[command(subcommand)]
        cmd: KeyCmd,
    },
    Pool {
        #[command(subcommand)]
        cmd: PoolCmd,
    },
    Tunnel {
        #[command(subcommand)]
        cmd: TunnelCmd,
    },
    Route {
        /// Model ID (e.g. openai/gpt-4o-mini)
        #[arg(long)]
        model: Option<String>,
        /// Combo name
        #[arg(long)]
        combo: Option<String>,
        /// Prompt text
        #[arg(long)]
        prompt: String,
        /// Stream output
        #[arg(long, default_value_t = true)]
        stream: bool,
        /// JSON output
        #[arg(long)]
        json: bool,
    },
    Completion {
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum ProviderCmd {
    List {
        #[arg(long)]
        json: bool,
    },
    Add {
        name: String,
        config: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum KeyCmd {
    List {
        #[arg(long)]
        json: bool,
    },
    Add {
        name: String,
        key: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum PoolCmd {
    List {
        #[arg(long)]
        json: bool,
    },
    Status {
        name: String,
        #[arg(long)]
        json: bool,
    },
    Create {
        name: String,
        proxy_url: String,
        #[arg(long)]
        json: bool,
    },
    Delete {
        name: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum TunnelCmd {
    Start {
        #[arg(long, default_value = "cloudflare")]
        provider: String,
        #[arg(long, default_value_t = 20128)]
        port: u16,
    },
    Stop,
    Status,
}

impl Cli {
    pub fn run(self) -> anyhow::Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        if let Some(cmd) = self.cmd {
            match cmd {
                Command::Provider { cmd } => {
                    let db = rt.block_on(Db::load())?;
                    let db = std::sync::Arc::new(db);
                    let rt = tokio::runtime::Runtime::new()?;
                    rt.block_on(run_provider(cmd, &db))
                }
                Command::Key { cmd } => {
                    let db = rt.block_on(Db::load())?;
                    let db = std::sync::Arc::new(db);
                    let rt = tokio::runtime::Runtime::new()?;
                    rt.block_on(run_key(cmd, &db))
                }
                Command::Pool { cmd } => {
                    let db = rt.block_on(Db::load())?;
                    let db = std::sync::Arc::new(db);
                    let rt = tokio::runtime::Runtime::new()?;
                    rt.block_on(run_pool(cmd, &db))
                }
                Command::Tunnel { cmd } => {
                    let db = rt.block_on(Db::load())?;
                    let db = std::sync::Arc::new(db);
                    let rt = tokio::runtime::Runtime::new()?;
                    rt.block_on(run_tunnel(cmd, db.clone()))
                }
                Command::Route { model, combo, prompt, stream, json } => {
                    eprintln!("Route command placeholder");
                    eprintln!("  model: {:?}, combo: {:?}", model, combo);
                    eprintln!("  prompt: {}, stream: {}, json: {}", prompt, stream, json);
                    Ok(())
                }
                Command::Completion { shell } => {
                    let mut cmd = Cli::command();
                    clap_complete::generate(shell, &mut cmd, "openproxy", &mut std::io::stdout());
                    Ok(())
                }
            }
        } else {
            Ok(())
        }
    }
}

 pub async fn run_provider(cmd: ProviderCmd, db: &Db) -> anyhow::Result<()> {
    match cmd {
        ProviderCmd::List { json } => {
            let connections = db.provider_connections(crate::db::ProviderConnectionFilter::default());
            let nodes = db.provider_nodes(None);

            if json {
                #[derive(serde::Serialize)]
                struct ListOutput {
                    provider_connections: Vec<ProviderConnection>,
                    provider_nodes: Vec<crate::types::ProviderNode>,
                }
                let output = ListOutput {
                    provider_connections: connections,
                    provider_nodes: nodes,
                };
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!("Provider Connections:");
                for conn in &connections {
                    println!(
                        "  {} ({}) - {}",
                        conn.provider,
                        conn.auth_type,
                        conn.name.as_deref().unwrap_or("unnamed")
                    );
                }
                println!("\nProvider Nodes:");
                for node in &nodes {
                    println!("  {} - {} ({})", node.name, node.r#type, node.id);
                }
            }
        }
        ProviderCmd::Add { name, config, json } => {
            let config: ProviderConnection = match serde_json::from_str(&config) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Failed to parse config: {}", e);
                    std::process::exit(1);
                }
            };

            let mut new_conn = config;
            new_conn.provider = name;
            if new_conn.id.is_empty() {
                new_conn.id = uuid::Uuid::new_v4().to_string();
            }

            db.update(|db| {
                db.provider_connections.push(new_conn.clone());
            })
            .await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&new_conn)?);
            } else {
                println!("Provider '{}' added successfully", new_conn.provider);
            }
        }
    }
    Ok(())
}
 pub async fn run_tunnel(cmd: TunnelCmd, db: std::sync::Arc<Db>) -> anyhow::Result<()> {
    let tunnel_manager = TunnelManager::new((db).clone());

    match cmd {
        TunnelCmd::Start { provider, port } => {
            let provider = provider.parse::<TunnelProvider>().map_err(|e| {
                anyhow::anyhow!("{}", e)
            })?;

            println!("Starting {} tunnel on port {}...", provider, port);
            tunnel_manager.start(provider, port).await?;

            // Wait a bit for URL to appear
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

            let status = tunnel_manager.status().await;
            if status.running {
                println!("Tunnel started successfully");
                if let Some(url) = status.url {
                    println!("  URL: {}", url);
                }
                if let Some(pid) = status.pid {
                    println!("  PID: {}", pid);
                }
            } else {
                eprintln!("Tunnel failed to start");
                std::process::exit(1);
            }
        }
        TunnelCmd::Stop => {
            println!("Stopping tunnel...");
            tunnel_manager.stop().await?;
            println!("Tunnel stopped");
        }
        TunnelCmd::Status => {
            let status = tunnel_manager.status().await;
            if status.running {
                println!("Tunnel is running");
                if let Some(p) = status.provider {
                    println!("  Provider: {}", p);
                }
                if let Some(url) = status.url {
                    println!("  URL: {}", url);
                }
                if let Some(pid) = status.pid {
                    println!("  PID: {}", pid);
                }
            } else {
                println!("Tunnel is stopped");
            }
        }
    }
    Ok(())
}

 pub async fn run_key(cmd: KeyCmd, db: &Db) -> anyhow::Result<()> {
    match cmd {
        KeyCmd::List { json } => {
            let snapshot = db.snapshot();
            let api_keys = &snapshot.api_keys;

            if json {
                println!("{}", serde_json::to_string_pretty(api_keys)?);
            } else {
                println!("API Keys:");
                for k in api_keys {
                    let key_preview = k.key.chars().take(8).collect::<String>();
                    println!(
                        "  {} [{}...] ({})",
                        k.name,
                        key_preview,
                        if k.is_active() { "active" } else { "inactive" }
                    );
                }
            }
        }
        KeyCmd::Add { name, key, json } => {
            let new_key = ApiKey {
                id: uuid::Uuid::new_v4().to_string(),
                name,
                key,
                machine_id: None,
                is_active: Some(true),
                created_at: Some(chrono::Utc::now().to_rfc3339()),
                extra: std::collections::BTreeMap::new(),
            };

            db.update(|db| {
                db.api_keys.push(new_key.clone());
            })
            .await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&new_key)?);
            } else {
                println!("API key added successfully");
            }
        }
    }
    Ok(())
}

 pub async fn run_pool(cmd: PoolCmd, db: &Db) -> anyhow::Result<()> {
    match cmd {
        PoolCmd::List { json } => {
            let snapshot = db.snapshot();
            let pools = &snapshot.proxy_pools;

            if json {
                println!("{}", serde_json::to_string_pretty(pools)?);
            } else {
                println!("Connection Pools:");
                for pool in pools {
                    let status = pool.test_status.as_deref().unwrap_or("unknown");
                    println!("  {} - {} ({})", pool.name, pool.r#type, status);
                }
            }
        }
        PoolCmd::Status { name, json } => {
            let snapshot = db.snapshot();
            let pool = snapshot.proxy_pools.iter().find(|p| p.name == name);

            match pool {
                Some(pool) => {
                    if json {
                        println!("{}", serde_json::to_string_pretty(pool)?);
                    } else {
                        println!("Pool: {}", pool.name);
                        println!("  Type: {}", pool.r#type);
                        println!("  URL: {}", pool.proxy_url);
                        println!("  Status: {:?}", pool.test_status.as_deref().unwrap_or("unknown"));
                        println!("  Success Rate: {:?}", pool.success_rate);
                        println!("  RTT (ms): {:?}", pool.rtt_ms);
                    }
                }
                None => {
                    eprintln!("Pool '{}' not found", name);
                    std::process::exit(1);
                }
            }
        }
        PoolCmd::Create { name, proxy_url, json } => {
            let new_pool = ProxyPool {
                id: uuid::Uuid::new_v4().to_string(),
                name: name.clone(),
                proxy_url,
                no_proxy: String::new(),
                r#type: "http".to_string(),
                is_active: Some(true),
                strict_proxy: Some(false),
                test_status: None,
                last_tested_at: None,
                last_error: None,
                success_rate: None,
                rtt_ms: None,
                total_requests: None,
                failed_requests: None,
                created_at: Some(chrono::Utc::now().to_rfc3339()),
                updated_at: None,
                extra: std::collections::BTreeMap::new(),
            };

            db.update(|db| {
                db.proxy_pools.push(new_pool.clone());
            })
            .await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&new_pool)?);
            } else {
                println!("Pool '{}' created successfully", name);
            }
        }
        PoolCmd::Delete { name, json } => {
            let snapshot = db.snapshot();
            let pool_exists = snapshot.proxy_pools.iter().any(|p| p.name == name);

            if !pool_exists {
                eprintln!("Pool '{}' not found", name);
                std::process::exit(1);
            }

            db.update(|db| {
                db.proxy_pools.retain(|p| p.name != name);
            })
            .await?;

            if json {
                #[derive(serde::Serialize)]
                struct DeleteOutput {
                    deleted: String,
                }
                println!("{}", serde_json::to_string_pretty(&DeleteOutput { deleted: name })?);
            } else {
                println!("Pool '{}' deleted successfully", name);
            }
        }
    }
    Ok(())
}
