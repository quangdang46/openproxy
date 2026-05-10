use clap::CommandFactory;
use clap::Parser;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use openproxy::cli::config::ResolvedConfig;
use openproxy::cli::{Cli, Command, SchemaCmd};
use openproxy::db::watcher::spawn_watcher;
use openproxy::db::Db;
use openproxy::server::console_logs::{shared_console_log_buffer, ConsoleLogMakeWriter};
use openproxy::server::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let ctx = cli.output_ctx();
    let resolved = ResolvedConfig::resolve(cli.cli_overrides())?;

    // Make sure downstream code (server, Db::load, oauth helpers, ...) sees
    // the same DATA_DIR the CLI resolved. Doing this here means a single
    // resolution path: flag > env > config profile > default.
    std::env::set_var("DATA_DIR", &resolved.data_dir);

    if let Some(cmd) = &cli.cmd {
        match cmd {
            Command::Provider { cmd } => {
                let db = Db::load().await?;
                let db = Arc::new(db);
                openproxy::cli::run_provider(cmd.clone(), &db).await?;
                return Ok(());
            }
            Command::Key { cmd } => {
                let db = Db::load().await?;
                let db = Arc::new(db);
                openproxy::cli::run_key(cmd.clone(), &db).await?;
                return Ok(());
            }
            Command::Pool { cmd } => {
                let db = Db::load().await?;
                let db = Arc::new(db);
                openproxy::cli::run_pool(cmd.clone(), &db).await?;
                return Ok(());
            }
            Command::Tunnel { cmd } => {
                let db = Db::load().await?;
                let db = Arc::new(db);
                openproxy::cli::run_tunnel(cmd.clone(), db).await?;
                return Ok(());
            }
            Command::Route {
                model,
                combo,
                prompt,
                stream,
                json,
            } => {
                let db = Db::load().await?;
                let db = Arc::new(db);
                return run_route(
                    model.clone(),
                    combo.clone(),
                    prompt.clone(),
                    *stream,
                    *json,
                    &db,
                )
                .await;
            }
            Command::Completion { shell } => {
                let mut cmd = Cli::command();
                clap_complete::generate(*shell, &mut cmd, "openproxy", &mut std::io::stdout());
                return Ok(());
            }
            Command::Schema { cmd } => {
                let exit = match cmd {
                    SchemaCmd::List => {
                        openproxy::cli::schema::run_list(ctx)?;
                        0
                    }
                    SchemaCmd::Show { resource } => {
                        openproxy::cli::schema::run_show(ctx, resource)?
                    }
                    SchemaCmd::Example { resource } => {
                        openproxy::cli::schema::run_example(ctx, resource)?
                    }
                };
                if exit != 0 {
                    std::process::exit(exit);
                }
                return Ok(());
            }
            Command::Doctor => {
                let exit = openproxy::cli::doctor::run(ctx, &resolved).await?;
                if exit != 0 {
                    std::process::exit(exit);
                }
                return Ok(());
            }
        }
    }

    let console_log_writer = ConsoleLogMakeWriter::new(shared_console_log_buffer());

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(cli.log_filter.clone()))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(console_log_writer),
        )
        .init();

    let db = Db::load().await?;
    let db = Arc::new(db);
    spawn_watcher(db.clone());
    let state = AppState::new(db);
    let app = openproxy::build_app(state);
    let addr = format!("{}:{}", cli.host, cli.port);
    info!("Starting openproxy on {}", addr);
    let listener = TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn run_route(
    model: Option<String>,
    combo: Option<String>,
    prompt: String,
    stream: bool,
    json: bool,
    db: &Arc<Db>,
) -> anyhow::Result<()> {
    let model_id = model
        .or_else(|| combo.map(|c| format!("combo/{}", c)))
        .ok_or_else(|| anyhow::anyhow!("--model or --combo required"))?;

    let snapshot = db.snapshot();
    let api_key = snapshot
        .api_keys
        .iter()
        .find(|k| k.is_active())
        .map(|k| k.key.clone())
        .ok_or_else(|| {
            anyhow::anyhow!("No active API key. Add one: openproxy key add <name> <key>")
        })?;

    let port = std::env::var("PORT")
        .ok()
        .unwrap_or_else(|| "4623".to_string());
    let url = format!("http://127.0.0.1:{}/v1/chat/completions", port);

    let body = serde_json::json!({
        "model": model_id,
        "messages": [{"role": "user", "content": prompt}],
        "stream": stream,
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to server. Is it running? Error: {}", e))?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Server returned {}: {}", status, text);
    }

    let text = resp.text().await?;
    if json {
        println!("{}", text);
    } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
        if let Some(content) = v["choices"][0]["message"]["content"].as_str() {
            println!("{}", content);
        } else {
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or(text));
        }
    } else {
        println!("{}", text);
    }
    Ok(())
}
