use clap::Parser;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use openproxy::cli::Cli;
use openproxy::db::Db;
use openproxy::server::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

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
