#![allow(clippy::single_match)]
use clap::CommandFactory;
use clap::Parser;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use openproxy::cli::config::ResolvedConfig;
use openproxy::cli::{
    chat as cli_chat, db as cli_db, logs as cli_logs, media as cli_media, mitm as cli_mitm,
    provider_oauth, quota as cli_quota, settings as cli_settings, tool as cli_tool,
    translator as cli_translator, tunnel_rt as cli_tunnel_rt, usage as cli_usage, AuthCmd, Cli,
    Command, ProviderCmd, SchemaCmd, ServerCmd, TunnelCmd,
};
use openproxy::db::watcher::spawn_watcher;
use openproxy::db::Db;
use openproxy::server::console_logs::{shared_console_log_buffer, ConsoleLogMakeWriter};
use openproxy::server::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Backward-compat: previous releases read the bind host from `$HOST`.
    // The CLI flag now reads `$HOSTNAME` (matches the README and Docker
    // env), so promote `$HOST` to `$HOSTNAME` if the latter is unset.
    if std::env::var_os("HOSTNAME").is_none() {
        if let Some(legacy) = std::env::var_os("HOST") {
            std::env::set_var("HOSTNAME", legacy);
        }
    }

    let mut cli = Cli::parse();
    openproxy::core::tls::ensure_rustls_provider();
    let ctx = cli.output_ctx();
    let resolved = ResolvedConfig::resolve(cli.cli_overrides())?;

    // Make sure downstream code (server, Db::load, oauth helpers, ...) sees
    // the same DATA_DIR the CLI resolved. Doing this here means a single
    // resolution path: flag > env > config profile > default.
    std::env::set_var("DATA_DIR", &resolved.data_dir);

    if let Some(cmd) = &cli.cmd {
        match cmd {
            Command::Provider { cmd } => {
                if let ProviderCmd::Oauth { cmd: oauth_cmd } = cmd {
                    let exit = provider_oauth::run(oauth_cmd.clone(), &resolved, ctx).await?;
                    if exit != 0 {
                        std::process::exit(exit);
                    }
                    return Ok(());
                }
                let db = Db::load().await?;
                let db = Arc::new(db);
                openproxy::cli::run_provider(cmd.clone(), &db, ctx).await?;
                return Ok(());
            }
            Command::Key { cmd } => {
                let db = Db::load().await?;
                let db = Arc::new(db);
                openproxy::cli::run_key(cmd.clone(), &db, ctx).await?;
                return Ok(());
            }
            Command::Pool { cmd } => {
                let db = Db::load().await?;
                let db = Arc::new(db);
                openproxy::cli::run_pool(cmd.clone(), &db, ctx).await?;
                return Ok(());
            }
            Command::Combo { cmd } => {
                let db = Db::load().await?;
                let db = Arc::new(db);
                openproxy::cli::combo::run(cmd.clone(), &db, ctx).await?;
                return Ok(());
            }
            Command::Models { cmd } => {
                let db = Db::load().await?;
                let db = Arc::new(db);
                openproxy::cli::models::run(cmd.clone(), &db, ctx).await?;
                return Ok(());
            }
            Command::Tunnel { cmd } => match cmd {
                TunnelCmd::Start { .. } | TunnelCmd::Stop | TunnelCmd::Status => {
                    let db = Db::load().await?;
                    let db = Arc::new(db);
                    openproxy::cli::run_tunnel(cmd.clone(), db, ctx).await?;
                    return Ok(());
                }
                TunnelCmd::Enable { provider, port } => {
                    let exit = cli_tunnel_rt::run(
                        cli_tunnel_rt::TunnelRtCmd::Enable {
                            provider: provider.clone(),
                            port: *port,
                        },
                        &resolved,
                        ctx,
                    )
                    .await?;
                    if exit != 0 {
                        std::process::exit(exit);
                    }
                    return Ok(());
                }
                TunnelCmd::Disable { provider } => {
                    let exit = cli_tunnel_rt::run(
                        cli_tunnel_rt::TunnelRtCmd::Disable {
                            provider: provider.clone(),
                        },
                        &resolved,
                        ctx,
                    )
                    .await?;
                    if exit != 0 {
                        std::process::exit(exit);
                    }
                    return Ok(());
                }
                TunnelCmd::Tailscale { cmd: ts_cmd } => {
                    let exit = cli_tunnel_rt::run(
                        cli_tunnel_rt::TunnelRtCmd::Tailscale {
                            cmd: ts_cmd.clone(),
                        },
                        &resolved,
                        ctx,
                    )
                    .await?;
                    if exit != 0 {
                        std::process::exit(exit);
                    }
                    return Ok(());
                }
            },
            Command::Route {
                model,
                combo,
                prompt,
                stream,
                json,
            } => {
                let db = Db::load().await?;
                let db = Arc::new(db);
                // `--robot` implies JSON mode for route, but the per-event
                // shape is left to a future M3 refactor (streaming envelope).
                let json_mode = *json || ctx.is_robot();
                return run_route(
                    model.clone(),
                    combo.clone(),
                    prompt.clone(),
                    *stream,
                    json_mode,
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
                    SchemaCmd::Stability => {
                        openproxy::cli::schema::run_stability(ctx)?;
                        0
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
            Command::Server { cmd } => match cmd {
                ServerCmd::Start {
                    detach,
                    host,
                    port,
                    no_open,
                } => {
                    // Hoist subcommand-level `--no-open` onto the global flag
                    // so the foreground server-boot path (which reads
                    // `cli.no_open`) honors it. Bug #6: README and SKILL.md
                    // both show `openproxy server start --detach --no-open`,
                    // so we accept it here too.
                    if *no_open {
                        cli.no_open = true;
                    }
                    let opts = openproxy::cli::server::StartOptions {
                        host: host.clone().unwrap_or_else(|| cli.host.clone()),
                        port: port.unwrap_or(cli.port),
                        detach: *detach,
                    };
                    match openproxy::cli::server::run_start(ctx, &resolved, opts).await? {
                        Some(exit) => {
                            if exit != 0 {
                                std::process::exit(exit);
                            }
                            return Ok(());
                        }
                        // Foreground: fall through to the server boot below.
                        None => {}
                    }
                }
                ServerCmd::Stop => {
                    let exit = openproxy::cli::server::run_stop(ctx, &resolved).await?;
                    if exit != 0 {
                        std::process::exit(exit);
                    }
                    return Ok(());
                }
                ServerCmd::Status => {
                    let exit = openproxy::cli::server::run_status(ctx, &resolved).await?;
                    if exit != 0 {
                        std::process::exit(exit);
                    }
                    return Ok(());
                }
                ServerCmd::Init { force } => {
                    let exit = openproxy::cli::server::run_init(ctx, &resolved, *force).await?;
                    if exit != 0 {
                        std::process::exit(exit);
                    }
                    return Ok(());
                }
            },
            Command::Auth { cmd } => {
                let exit = match cmd {
                    AuthCmd::Login {
                        url,
                        api_key,
                        profile,
                        no_verify,
                        no_activate,
                    } => {
                        openproxy::cli::auth::run_login(
                            ctx,
                            openproxy::cli::auth::LoginOptions {
                                url: url.clone(),
                                api_key: api_key.clone(),
                                profile: profile.clone(),
                                no_verify: *no_verify,
                                no_activate: *no_activate,
                            },
                        )
                        .await?
                    }
                    AuthCmd::Logout {
                        profile,
                        keep_default,
                    } => openproxy::cli::auth::run_logout(
                        ctx,
                        openproxy::cli::auth::LogoutOptions {
                            profile: profile.clone(),
                            keep_default: *keep_default,
                        },
                    )?,
                    AuthCmd::Whoami { verify } => {
                        openproxy::cli::auth::run_whoami(ctx, &resolved, *verify).await?
                    }
                    AuthCmd::List => openproxy::cli::auth::run_list(ctx)?,
                };
                if exit != 0 {
                    std::process::exit(exit);
                }
                return Ok(());
            }
            Command::Usage { cmd } => {
                let exit = cli_usage::run(cmd.clone(), &resolved, ctx).await?;
                if exit != 0 {
                    std::process::exit(exit);
                }
                return Ok(());
            }
            Command::Logs { cmd } => {
                let exit = cli_logs::run(cmd.clone(), &resolved, ctx).await?;
                if exit != 0 {
                    std::process::exit(exit);
                }
                return Ok(());
            }
            Command::Quota { cmd } => {
                let exit = cli_quota::run(cmd.clone(), &resolved, ctx).await?;
                if exit != 0 {
                    std::process::exit(exit);
                }
                return Ok(());
            }
            Command::Chat { cmd } => {
                let exit = cli_chat::run(cmd.clone(), &resolved, ctx).await?;
                if exit != 0 {
                    std::process::exit(exit);
                }
                return Ok(());
            }
            Command::Mitm { cmd } => {
                let exit = cli_mitm::run(cmd.clone(), &resolved, ctx).await?;
                if exit != 0 {
                    std::process::exit(exit);
                }
                return Ok(());
            }
            Command::Tool { cmd } => {
                let exit = cli_tool::run(cmd.clone(), &resolved, ctx).await?;
                if exit != 0 {
                    std::process::exit(exit);
                }
                return Ok(());
            }
            Command::Translator { cmd } => {
                let exit = cli_translator::run(cmd.clone(), &resolved, ctx).await?;
                if exit != 0 {
                    std::process::exit(exit);
                }
                return Ok(());
            }
            Command::Media { cmd } => {
                let exit = cli_media::run(cmd.clone(), &resolved, ctx).await?;
                if exit != 0 {
                    std::process::exit(exit);
                }
                return Ok(());
            }
            Command::Settings { cmd } => {
                let exit = cli_settings::run(cmd.clone(), &resolved, ctx).await?;
                if exit != 0 {
                    std::process::exit(exit);
                }
                return Ok(());
            }
            Command::Db { cmd } => {
                let exit = cli_db::run(cmd.clone(), &resolved, ctx).await?;
                if exit != 0 {
                    std::process::exit(exit);
                }
                return Ok(());
            }
            Command::Sync { cmd } => {
                let db = Db::load().await?;
                let db = Arc::new(db);
                openproxy::cli::sync::run(cmd.clone(), &db, ctx).await?;
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
    seed_default_api_key_if_missing(&db).await?;
    let db = Arc::new(db);
    spawn_watcher(db.clone());
    spawn_auto_backup(db.clone());
    // Prune old usage/request details on startup (keep 30 days).
    spawn_usage_retention_cleanup(db.clone());
    let state = AppState::new(db)
        .init_oidc_from_env()
        .await
        .with_dashboard_sidecar_url(cli.dashboard_sidecar_url.clone())
        .with_web_dir(cli.web_dir.clone());
    // Periodic cleanup of stale HTTP client connections.
    state.client_pool.start_periodic_cleanup();
    // Periodic cleanup of expired OAuth pending flows (every 5 minutes).
    {
        let pending = state.pending_flows.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                pending.cleanup_expired();
            }
        });
    }
    let app = openproxy::build_app(state);
    let addr = format!("{}:{}", cli.host, cli.port);
    info!("Starting openproxy on {}", addr);
    let listener = TcpListener::bind(&addr).await?;
    let bound = listener.local_addr().ok();

    // Print startup banner to stderr so the user sees it even when
    // stdout is captured (containers, CI, …). The tracing subscriber
    // writes to the log file only — this is the only terminal feedback.
    eprintln!();
    eprintln!("  openproxy {}", env!("CARGO_PKG_VERSION"));
    eprintln!(
        "  Dashboard → http://{}:{}",
        browser_host(&cli.host),
        bound.map(|a| a.port()).unwrap_or(cli.port)
    );
    eprintln!(
        "  API       → http://{}:{}/v1",
        browser_host(&cli.host),
        bound.map(|a| a.port()).unwrap_or(cli.port)
    );
    eprintln!("  Press Ctrl+C to stop");
    eprintln!();

    // Auto-open the dashboard in the user's default browser when running
    // interactively. Skipped when:
    //   • --no-open / OPENPROXY_NO_OPEN is set
    //   • stdout is not a TTY (containers, CI, SSH redirected, systemd, …)
    //   • --robot is set (machine-readable output mode)
    if should_open_browser(&cli) {
        let port = bound.map(|a| a.port()).unwrap_or(cli.port);
        let host = browser_host(&cli.host);
        let url = format!("http://{host}:{port}/");
        tokio::spawn(async move {
            // axum needs a moment to start accepting connections. The browser
            // is forgiving — if the request lands ~200ms before the server is
            // ready, modern browsers retry. A short sleep is enough.
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            if let Err(err) = open::that(&url) {
                tracing::warn!(target: "openproxy", "could not open browser at {url}: {err}");
            } else {
                tracing::info!(target: "openproxy", "opened {url} in default browser");
            }
        });
    }

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// Decide whether to launch the user's default browser at startup.
fn should_open_browser(cli: &Cli) -> bool {
    if cli.no_open || cli.robot {
        return false;
    }
    is_stdout_tty()
}

#[cfg(unix)]
fn is_stdout_tty() -> bool {
    // SAFETY: `isatty` only inspects a file descriptor.
    unsafe { libc::isatty(libc::STDOUT_FILENO) == 1 }
}

#[cfg(not(unix))]
fn is_stdout_tty() -> bool {
    // Conservative default on non-Unix: assume interactive. Users on Windows
    // can opt out with --no-open if needed.
    true
}

/// Background task that snapshots db state once per hour. The throttle and
/// retention are enforced inside `BackupManager::create_from_json` / `cleanup`,
/// so the loop just nudges the manager on a fixed interval. Honors
/// `DISABLE_AUTO_BACKUP=1`.
fn spawn_auto_backup(db: Arc<Db>) {
    use openproxy::db::backups::{BackupManager, BackupReason};

    if BackupManager::is_auto_disabled() {
        tracing::info!(target: "openproxy::db::backups", "auto-backup disabled via DISABLE_AUTO_BACKUP");
        return;
    }

    tokio::spawn(async move {
        // Small startup delay so the very first request doesn't compete with
        // a fresh-DB snapshot.
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        let mgr = BackupManager::new(&db.data_dir);
        loop {
            // Export the in-memory snapshot as JSON bytes for the backup.
            let (json_bytes, _filename) = match db.export_db() {
                Ok(m) => m,
                Err(err) => {
                    tracing::warn!(
                        target: "openproxy::db::backups",
                        error = %err,
                        "auto backup: export failed"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(60 * 60)).await;
                    continue;
                }
            };
            match mgr.create_from_json(BackupReason::Auto, &json_bytes).await {
                Ok(Some(info)) => tracing::debug!(
                    target: "openproxy::db::backups",
                    id = %info.id,
                    "auto backup created"
                ),
                Ok(None) => {}
                Err(err) => tracing::warn!(
                    target: "openproxy::db::backups",
                    error = %err,
                    "auto backup failed"
                ),
            }
            tokio::time::sleep(std::time::Duration::from_secs(60 * 60)).await;
        }
    });
}

/// Periodic cleanup: prune usageHistory, requestDetails, and usageDaily older than 30 days.
/// Runs once at startup and then every 24 hours.
fn spawn_usage_retention_cleanup(db: Arc<Db>) {
    tokio::spawn(async move {
        loop {
            // Run retention cleanup
            let cutoff = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
            // Prune usageHistory
            match db.sqlite.with_conn(|conn| {
                let count: u64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM usageHistory WHERE timestamp < ?1",
                        [&cutoff],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);
                if count > 0 {
                    conn.execute("DELETE FROM usageHistory WHERE timestamp < ?1", [&cutoff])?;
                }
                Ok(count)
            }) {
                Ok(0) => {}
                Ok(count) => tracing::info!(
                    target: "openproxy::db::retention",
                    deleted = count,
                    "pruned old usageHistory records"
                ),
                Err(e) => tracing::warn!(
                    target: "openproxy::db::retention",
                    error = %e,
                    "usage retention cleanup failed"
                ),
            }
            // Prune requestDetails
            match db.sqlite.with_conn(|conn| {
                let count: u64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM requestDetails WHERE timestamp < ?1",
                        [&cutoff],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);
                if count > 0 {
                    conn.execute("DELETE FROM requestDetails WHERE timestamp < ?1", [&cutoff])?;
                }
                Ok(count)
            }) {
                Ok(0) => {}
                Ok(count) => tracing::info!(
                    target: "openproxy::db::retention",
                    deleted = count,
                    "pruned old requestDetails records"
                ),
                Err(e) => tracing::warn!(
                    target: "openproxy::db::retention",
                    error = %e,
                    "requestDetails retention cleanup failed"
                ),
            }
            // Prune usageDaily older than 90 days
            let daily_cutoff = (chrono::Utc::now() - chrono::Duration::days(90))
                .format("%Y-%m-%d")
                .to_string();
            match db.sqlite.with_conn(|conn| {
                let count: u64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM usageDaily WHERE dateKey < ?1",
                        [&daily_cutoff],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);
                if count > 0 {
                    conn.execute("DELETE FROM usageDaily WHERE dateKey < ?1", [&daily_cutoff])?;
                }
                Ok(count)
            }) {
                Ok(0) => {}
                Ok(count) => tracing::info!(
                    target: "openproxy::db::retention",
                    deleted = count,
                    "pruned old usageDaily records"
                ),
                Err(e) => tracing::warn!(
                    target: "openproxy::db::retention",
                    error = %e,
                    "usageDaily retention cleanup failed"
                ),
            }
            // Sleep 24 hours before next cleanup
            tokio::time::sleep(std::time::Duration::from_secs(86400)).await;
        }
    });
}

async fn seed_default_api_key_if_missing(db: &Db) -> anyhow::Result<()> {
    if !db.snapshot().api_keys.is_empty() {
        return Ok(());
    }

    use openproxy::core::auth::generate_api_key_with_machine;
    use openproxy::server::api::consistent_machine_id;
    use openproxy::types::ApiKey;

    let machine_id = consistent_machine_id();
    let key = generate_api_key_with_machine(&machine_id);
    let api_key = ApiKey {
        id: uuid::Uuid::new_v4().to_string(),
        name: "default".to_string(),
        key: key.clone(),
        machine_id: Some(machine_id),
        is_active: Some(true),
        created_at: Some(chrono::Utc::now().to_rfc3339()),
        extra: std::collections::BTreeMap::new(),
    };

    db.update(|d| d.api_keys.push(api_key.clone())).await?;
    tracing::info!(target: "openproxy", "seeded default API key (apiKeys was empty)");
    eprintln!("  Default API key (saved):");
    eprintln!("    {key}");
    eprintln!();
    Ok(())
}

/// Browser-friendly host: bind hosts like `0.0.0.0` are not resolvable in
/// browsers; rewrite them to `127.0.0.1` for the launch URL only.
fn browser_host(bind_host: &str) -> &str {
    match bind_host {
        "0.0.0.0" | "[::]" | "::" | "" => "127.0.0.1",
        h => h,
    }
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
