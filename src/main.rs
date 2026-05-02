use std::sync::Arc;
use clap::CommandFactory;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use clap::Parser;
use clap_complete::Shell;

use openproxy::cli::{Cli, Command};
use openproxy::db::Db;
use openproxy::server::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Handle CLI-only commands before starting server
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
            Command::Route { model, combo, prompt, stream, json } => {
                eprintln!("Route command: model={:?}, combo={:?}, prompt={}, stream={}, json={}", 
                          model, combo, prompt, stream, json);
                // TODO: Implement full route logic
                return Ok(());
            }
            Command::Completion { shell } => {
                let mut cmd = Cli::command();
                clap_complete::generate(*shell, &mut cmd, "openproxy", &mut std::io::stdout());
                return Ok(());
            }
        }
    }

    // No command - start server
    if let Some(data_dir) = &cli.data_dir {
        std::env::set_var("DATA_DIR", data_dir);
    }

    // Init tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(cli.log_filter.clone()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load database
    let db = Db::load().await?;
    let db = Arc::new(db);

    let state = AppState::new(db);
    let app = openproxy::build_app(state);
    let addr = format!("{}:{}", cli.host, cli.port);

    info!("Starting openproxy on {}", addr);

    let listener = TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
