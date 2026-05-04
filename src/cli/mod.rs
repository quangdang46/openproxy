use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use futures_util::TryStreamExt;
use http_body_util::BodyExt;
use serde_json::Value;

use crate::core::account_fallback::AccountRegistry;
use crate::core::combo::{get_combo_models_from_data, ComboStrategy};
use crate::core::executor::{ClientPool, DefaultExecutor, ExecutionRequest};
use crate::core::model::get_model_info;
use crate::core::proxy::resolve_proxy_target;
use crate::core::rtk::apply_request_preprocessing;
use crate::db::Db;
use crate::types::{ApiKey, AppDb, ProviderConnection, ProxyPool};

use crate::core::tunnel::{TunnelManager, TunnelProvider};

#[derive(Debug, Clone, Parser)]
#[command(name = "openproxy", about = "Local AI routing gateway")]
pub struct Cli {
    #[arg(long, env = "HOST", default_value = "0.0.0.0")]
    pub host: String,

    #[arg(long, env = "PORT", default_value_t = 4623)]
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
        #[arg(long, default_value_t = 4623)]
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
                Command::Route {
                    model,
                    combo,
                    prompt,
                    stream,
                    json,
                } => {
                    let rt = tokio::runtime::Runtime::new()?;
                    rt.block_on(run_route(model, combo, prompt, stream, json))?;
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
            let connections =
                db.provider_connections(crate::db::ProviderConnectionFilter::default());
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
            let provider = provider
                .parse::<TunnelProvider>()
                .map_err(|e| anyhow::anyhow!("{}", e))?;

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
                        println!(
                            "  Status: {:?}",
                            pool.test_status.as_deref().unwrap_or("unknown")
                        );
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
        PoolCmd::Create {
            name,
            proxy_url,
            json,
        } => {
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
                println!(
                    "{}",
                    serde_json::to_string_pretty(&DeleteOutput { deleted: name })?
                );
            } else {
                println!("Pool '{}' deleted successfully", name);
            }
        }
    }
    Ok(())
}

async fn run_route(
    model: Option<String>,
    combo: Option<String>,
    prompt: String,
    stream: bool,
    json: bool,
) -> anyhow::Result<()> {
    let pool = Arc::new(ClientPool::new());
    let registry = AccountRegistry::default();

    if let (Some(model_str), None) = (&model, &combo) {
        run_direct_route(pool, registry, model_str, &prompt, stream, json).await
    } else if let (None, Some(combo_name)) = (&model, &combo) {
        run_combo_route(pool, registry, combo_name, &prompt, stream, json).await
    } else if let (Some(_model_str), Some(combo_name)) = (&model, &combo) {
        eprintln!(
            "Warning: both --model and --combo specified, using --combo '{}'",
            combo_name
        );
        run_combo_route(pool, registry, combo_name, &prompt, stream, json).await
    } else {
        eprintln!("Error: must specify either --model or --combo");
        eprintln!("Usage: openproxy route --model cc/claude-opus-4-7 --prompt 'hello'");
        eprintln!("   or: openproxy route --combo default --prompt 'hello'");
        std::process::exit(1);
    }
}

async fn run_direct_route(
    pool: Arc<ClientPool>,
    registry: AccountRegistry,
    model_str: &str,
    prompt: &str,
    stream: bool,
    json: bool,
) -> anyhow::Result<()> {
    let snapshot = db_snapshot();
    let resolved = get_model_info(model_str, &snapshot);

    let Some(provider) = resolved.provider.clone() else {
        eprintln!(
            "Error: could not resolve provider from model '{}'",
            model_str
        );
        std::process::exit(1);
    };

    let mut request_body = serde_json::json!({
        "model": resolved.model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": stream,
    });

    let _ = apply_request_preprocessing(&mut request_body, &snapshot.settings, &resolved.model);

    let mut excluded = HashSet::new();
    let mut last_error = None;

    loop {
        let snapshot = db_snapshot();
        let connection = select_connection_cli(&snapshot, &provider, &resolved.model, &excluded);

        let Some(connection) = connection else {
            if let Some(error) = last_error {
                eprintln!("Error: {}", error);
                std::process::exit(1);
            }
            eprintln!(
                "Error: no available credentials for provider '{}'",
                provider
            );
            std::process::exit(1);
        };

        let provider_node = snapshot
            .provider_nodes
            .iter()
            .find(|node| node.id == provider)
            .cloned();

        let proxy = resolve_proxy_target(&snapshot, &connection, &snapshot.settings);

        let (rate_limit_remaining, rate_limit_reset) = registry.rate_limit_info(&connection.id);
        let slot =
            registry.acquire_slot(&connection.id, 10, rate_limit_remaining, rate_limit_reset);

        let Some(_slot) = slot else {
            excluded.insert(connection.id.clone());
            continue;
        };

        let executor = match DefaultExecutor::new(provider.clone(), pool.clone(), provider_node) {
            Ok(ex) => ex,
            Err(e) => {
                eprintln!("Error creating executor: {:?}", e);
                std::process::exit(1);
            }
        };

        let stream_flag = request_body
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let result = executor
            .execute(ExecutionRequest {
                model: resolved.model.clone(),
                body: request_body.clone(),
                stream: stream_flag,
                credentials: connection.clone(),
                proxy,
            })
            .await;

        match result {
            Ok(response) => {
                if json {
                    let body = response.transformed_body;
                    println!("{}", serde_json::to_string_pretty(&body)?);
                    return Ok(());
                }

                match response.response {
                    crate::core::executor::UpstreamResponse::Reqwest(reqwest_resp) => {
                        if stream_flag {
                            print_stream_response(reqwest_resp).await?;
                        } else {
                            let text = reqwest_resp.text().await?;
                            let parsed: Value = serde_json::from_str(&text)?;
                            println!("{}", serde_json::to_string_pretty(&parsed)?);
                        }
                    }
                    crate::core::executor::UpstreamResponse::Hyper(_) => {
                        eprintln!("Hyper response not supported in CLI mode");
                    }
                }
                return Ok(());
            }
            Err(e) => {
                let error_msg = format!("{:?}", e);
                last_error = Some(error_msg.clone());
                excluded.insert(connection.id.clone());
                continue;
            }
        }
    }
}

async fn run_combo_route(
    pool: Arc<ClientPool>,
    registry: AccountRegistry,
    combo_name: &str,
    prompt: &str,
    stream: bool,
    json: bool,
) -> anyhow::Result<()> {
    let snapshot = db_snapshot();
    let Some(combo_models) = get_combo_models_from_data(combo_name, &snapshot.combos) else {
        eprintln!("Error: combo '{}' not found", combo_name);
        std::process::exit(1);
    };

    let strategy = snapshot
        .settings
        .combo_strategies
        .get(combo_name)
        .map(String::as_str)
        .unwrap_or(snapshot.settings.combo_strategy.as_str());

    let _combo_strategy = if strategy.eq_ignore_ascii_case("round-robin") {
        ComboStrategy::RoundRobin
    } else {
        ComboStrategy::Fallback
    };

    let model_str = combo_models
        .first()
        .map(|m| m.as_str())
        .unwrap_or("gpt-4o-mini");
    let resolved = get_model_info(model_str, &snapshot);

    let Some(provider) = resolved.provider.clone() else {
        eprintln!(
            "Error: could not resolve provider from combo model '{}'",
            model_str
        );
        std::process::exit(1);
    };

    let mut request_body = serde_json::json!({
        "model": resolved.model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": stream,
    });

    let _ = apply_request_preprocessing(&mut request_body, &snapshot.settings, &resolved.model);

    let mut excluded = HashSet::new();

    loop {
        let snapshot = db_snapshot();
        let connection = select_connection_cli(&snapshot, &provider, &resolved.model, &excluded);

        let Some(connection) = connection else {
            eprintln!(
                "Error: no available credentials for provider '{}'",
                provider
            );
            std::process::exit(1);
        };

        let provider_node = snapshot
            .provider_nodes
            .iter()
            .find(|node| node.id == provider)
            .cloned();

        let proxy = resolve_proxy_target(&snapshot, &connection, &snapshot.settings);

        let (rate_limit_remaining, rate_limit_reset) = registry.rate_limit_info(&connection.id);
        let slot =
            registry.acquire_slot(&connection.id, 10, rate_limit_remaining, rate_limit_reset);

        let Some(_slot) = slot else {
            excluded.insert(connection.id.clone());
            continue;
        };

        let executor = match DefaultExecutor::new(provider.clone(), pool.clone(), provider_node) {
            Ok(ex) => ex,
            Err(e) => {
                eprintln!("Error creating executor: {:?}", e);
                std::process::exit(1);
            }
        };

        let stream_flag = request_body
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let result = executor
            .execute(ExecutionRequest {
                model: resolved.model.clone(),
                body: request_body.clone(),
                stream: stream_flag,
                credentials: connection.clone(),
                proxy,
            })
            .await;

        match result {
            Ok(response) => {
                if json {
                    let body = response.transformed_body;
                    println!("{}", serde_json::to_string_pretty(&body)?);
                    return Ok(());
                }

                match response.response {
                    crate::core::executor::UpstreamResponse::Reqwest(reqwest_resp) => {
                        if stream_flag {
                            print_stream_response(reqwest_resp).await?;
                        } else {
                            let text = reqwest_resp.text().await?;
                            let parsed: Value = serde_json::from_str(&text)?;
                            println!("{}", serde_json::to_string_pretty(&parsed)?);
                        }
                    }
                    crate::core::executor::UpstreamResponse::Hyper(_) => {
                        eprintln!("Hyper response not supported in CLI mode");
                    }
                }
                return Ok(());
            }
            Err(e) => {
                let _error_msg = format!("{:?}", e);
                excluded.insert(connection.id.clone());
                continue;
            }
        }
    }
}

fn db_snapshot() -> Arc<AppDb> {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let db = Db::load().await.expect("Failed to load database");
        db.snapshot()
    })
}

fn select_connection_cli(
    snapshot: &Arc<AppDb>,
    provider: &str,
    model: &str,
    excluded: &HashSet<String>,
) -> Option<ProviderConnection> {
    use chrono::Utc;

    let now = Utc::now();
    let mut candidates: Vec<_> = snapshot
        .provider_connections
        .iter()
        .filter(|connection| {
            connection.provider == provider
                && connection.is_active()
                && connection_has_credentials(connection)
                && !excluded.contains(&connection.id)
                && connection_supports_model(connection, model)
                && !is_connection_rate_limited(connection, now)
                && !is_model_locked(connection, model, now)
        })
        .cloned()
        .collect();

    candidates.sort_by_key(|connection| connection.priority.unwrap_or(999));
    candidates.into_iter().next()
}

fn connection_has_credentials(connection: &ProviderConnection) -> bool {
    connection
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
        || connection
            .access_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
}

fn is_connection_rate_limited(
    connection: &ProviderConnection,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    connection
        .rate_limited_until
        .as_deref()
        .and_then(parse_timestamp)
        .is_some_and(|until| until > now)
}

fn is_model_locked(
    connection: &ProviderConnection,
    model: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    [format!("modelLock_{model}"), "modelLock___all".to_string()]
        .into_iter()
        .filter_map(|key| connection.extra.get(&key))
        .filter_map(Value::as_str)
        .filter_map(parse_timestamp)
        .any(|until| until > now)
}

fn parse_timestamp(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

fn connection_supports_model(connection: &ProviderConnection, model: &str) -> bool {
    let enabled_models: Vec<_> = connection
        .provider_specific_data
        .get("enabledModels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect();

    if !enabled_models.is_empty() {
        return enabled_models
            .iter()
            .any(|value| model_ids_match(value, model));
    }

    connection
        .default_model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none_or(|value| model_ids_match(value, model))
}

fn model_ids_match(advertised: &str, requested: &str) -> bool {
    let advertised = advertised.trim();
    let requested = requested.trim();

    advertised == requested || advertised.ends_with(&format!("/{}", requested))
}

async fn print_stream_response(response: reqwest::Response) -> anyhow::Result<()> {
    use futures_util::StreamExt;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                print!("{}", String::from_utf8_lossy(&bytes));
                std::io::Write::flush(&mut std::io::stdout())?;
            }
            Err(e) => {
                eprintln!("Stream error: {:?}", e);
                break;
            }
        }
    }
    Ok(())
}
