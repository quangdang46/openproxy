use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
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

pub mod apply;
pub mod auth;
pub mod chat;
pub mod combo;
pub mod config;
pub mod db;
pub mod doctor;
pub mod key_ext;
pub mod logs;
pub mod media;
pub mod mitm;
pub mod models;
pub mod output;
pub mod pool_ext;
pub mod provider_ext;
pub mod provider_models;
pub mod provider_node;
pub mod provider_oauth;
pub mod quota;
pub mod runtime;
pub mod schema;
pub mod server;
pub mod settings;
pub mod tool;
pub mod translator;
pub mod tunnel_rt;
pub mod usage;

#[cfg(test)]
pub(crate) mod test_lock {
    use std::sync::Mutex;
    pub static ENV_LOCK: Mutex<()> = Mutex::new(());
}

#[derive(Debug, Clone, Parser)]
#[command(
    name = "openproxy",
    about = "Local AI routing gateway (server + agent-first CLI)"
)]
pub struct Cli {
    #[arg(long, env = "HOST", default_value = "0.0.0.0")]
    pub host: String,

    #[arg(long, env = "PORT", default_value_t = 4623)]
    pub port: u16,

    #[arg(long, env = "RUST_LOG", default_value = "info")]
    pub log_filter: String,

    /// Path to the OpenProxy data directory (db.json, usage.json).
    /// Falls back to $DATA_DIR or ~/.openproxy.
    #[arg(long, env = "DATA_DIR", global = true)]
    pub data_dir: Option<PathBuf>,

    /// Emit a stable JSON envelope (`openproxy.v1.*`) to stdout for agents.
    /// No banners, no color, NDJSON for streaming commands.
    #[arg(long, global = true)]
    pub robot: bool,

    /// Increase logging verbosity (-v info, -vv debug, -vvv trace).
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Suppress non-essential human output. Errors still go to stderr.
    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,

    /// Color preference for human output. Default = auto-detect TTY.
    #[arg(long, value_enum, default_value_t = ColorChoice::Auto, global = true)]
    pub color: ColorChoice,

    /// Optional config profile (from ~/.config/openproxy/config.toml).
    #[arg(long, env = "OPENPROXY_PROFILE", global = true)]
    pub profile: Option<String>,

    /// Remote management mode: target a server at this base URL instead of
    /// the local DB. Pairs with --api-key (or $OPENPROXY_API_KEY).
    #[arg(long, env = "OPENPROXY_URL", global = true)]
    pub url: Option<String>,

    /// API key for remote management. Read from $OPENPROXY_API_KEY by default.
    #[arg(long, env = "OPENPROXY_API_KEY", global = true, hide_env_values = true)]
    pub api_key: Option<String>,

    #[command(subcommand)]
    pub cmd: Option<Command>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

impl Cli {
    /// Build the resolved output context shared by every subcommand.
    pub fn output_ctx(&self) -> output::OutputCtx {
        let mode = if self.robot {
            output::OutputMode::Robot
        } else {
            output::OutputMode::Human
        };
        let color = match self.color {
            ColorChoice::Auto => output::ColorMode::Auto,
            ColorChoice::Always => output::ColorMode::Always,
            ColorChoice::Never => output::ColorMode::Never,
        };
        output::OutputCtx {
            mode,
            color,
            quiet: self.quiet,
        }
    }

    pub fn cli_overrides(&self) -> config::CliOverrides {
        config::CliOverrides {
            data_dir: self.data_dir.clone(),
            url: self.url.clone(),
            api_key: self.api_key.clone(),
            profile: self.profile.clone(),
            port: Some(self.port),
        }
    }
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
    /// Combo (fallback / round-robin chain) management.
    Combo {
        #[command(subcommand)]
        cmd: combo::ComboCmd,
    },
    /// Top-level model registry (built-in + custom).
    Models {
        #[command(subcommand)]
        cmd: models::ModelsCmd,
    },
    Tunnel {
        #[command(subcommand)]
        cmd: TunnelCmd,
    },
    /// Manage the in-process MITM router (PLAN v3 §4.10).
    Mitm {
        #[command(subcommand)]
        cmd: mitm::MitmCmd,
    },
    /// Manage CLI-tool integrations (claude, codex, copilot, ...).
    Tool {
        #[command(subcommand)]
        cmd: tool::ToolCmd,
    },
    /// Translate or pass requests through the format translator.
    Translator {
        #[command(subcommand)]
        cmd: translator::TranslatorCmd,
    },
    /// Media providers + TTS / STT / embed / image / web helpers.
    Media {
        #[command(subcommand)]
        cmd: media::MediaCmd,
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
    /// Print JSON Schema / examples for resources the CLI accepts.
    /// Useful for agents to discover the right shape before generating payloads.
    Schema {
        #[command(subcommand)]
        cmd: SchemaCmd,
    },
    /// Run a self-test of the local install (data dir, db, server health).
    Doctor,
    /// Manage the local server daemon (start/stop/status/init).
    Server {
        #[command(subcommand)]
        cmd: ServerCmd,
    },
    /// Manage credentials for remote-management mode.
    Auth {
        #[command(subcommand)]
        cmd: AuthCmd,
    },
    /// Runtime usage statistics (talks to /api/usage/*).
    Usage {
        #[command(subcommand)]
        cmd: usage::UsageCmd,
    },
    /// Observability log buffer (talks to /api/observability/*).
    Logs {
        #[command(subcommand)]
        cmd: logs::LogsCmd,
    },
    /// Per-provider quota counters and reset.
    Quota {
        #[command(subcommand)]
        cmd: quota::QuotaCmd,
    },
    /// Lightweight chat client against the running proxy.
    Chat {
        #[command(subcommand)]
        cmd: chat::ChatCmd,
    },
    /// Manage the running server's settings document, locale, and version.
    Settings {
        #[command(subcommand)]
        cmd: settings::SettingsCmd,
    },
    /// Database snapshot, dump, import, and cloud-sync helpers (PLAN v3 §4.17-4.18).
    Db {
        #[command(subcommand)]
        cmd: db::DbCmd,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum ServerCmd {
    /// Start the API server. Without `--detach` runs in the foreground.
    Start {
        /// Run the server in the background and return immediately.
        #[arg(long)]
        detach: bool,
        /// Override the host the server binds to.
        #[arg(long)]
        host: Option<String>,
        /// Override the port the server binds to.
        #[arg(long)]
        port: Option<u16>,
    },
    /// Send SIGTERM to the running server and wait for it to exit.
    Stop,
    /// Report whether a server is running for this data dir.
    Status,
    /// Initialize an empty db.json and emit the first admin API key.
    Init {
        /// Overwrite an existing db.json if present.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum AuthCmd {
    /// Save credentials for a remote openproxy server as a profile.
    Login {
        /// Base URL of the remote server, e.g. https://op.example.com:4623
        #[arg(long)]
        url: String,
        /// API key generated by `openproxy server init` on the remote server.
        #[arg(long)]
        api_key: String,
        /// Profile name. Defaults to a slug derived from the URL host.
        #[arg(long)]
        profile: Option<String>,
        /// Skip the live health probe (use if the server is offline).
        #[arg(long)]
        no_verify: bool,
        /// Do not promote this profile to `default_profile`.
        #[arg(long)]
        no_activate: bool,
    },
    /// Remove a saved profile.
    Logout {
        /// Profile name to remove. Defaults to the active default profile.
        #[arg(long)]
        profile: Option<String>,
        /// Keep `default_profile` set even if we just removed it.
        #[arg(long)]
        keep_default: bool,
    },
    /// Show the active identity and optionally verify connectivity.
    Whoami {
        /// Probe the server to confirm the saved key still works.
        #[arg(long)]
        verify: bool,
    },
    /// List all configured profiles.
    List,
}

#[derive(Debug, Clone, Subcommand)]
pub enum SchemaCmd {
    /// List all resources for which the CLI exposes a schema/example.
    List,
    /// Print the JSON Schema for a single resource.
    Show {
        /// Resource name (e.g. provider, combo, key, pool, settings).
        resource: String,
    },
    /// Print an example payload for a single resource.
    Example {
        /// Resource name (e.g. provider, combo, key, pool, settings).
        resource: String,
    },
    /// Print the schema namespace + stability contract. As of M6 the
    /// `openproxy.v1.*` envelopes are declared **stable** (additive-only).
    Stability,
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
    /// Custom provider node (instance) management.
    Node {
        #[command(subcommand)]
        cmd: provider_node::NodeCmd,
    },
    /// Models, aliases, and disabled-model list for a provider.
    Models {
        #[command(subcommand)]
        cmd: provider_models::ModelsCmd,
    },
    /// Show one provider connection by id or name.
    Get { id_or_name: String },
    /// Edit a provider connection's fields.
    Edit {
        id_or_name: String,
        #[arg(long)]
        api_key: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        priority: Option<u32>,
        #[arg(long)]
        default_model: Option<String>,
    },
    /// Delete a provider connection.
    Delete {
        id_or_name: String,
        #[arg(long)]
        strict: bool,
    },
    /// Mark provider active.
    Enable { id_or_name: String },
    /// Mark provider inactive.
    Disable { id_or_name: String },
    /// Run a real connectivity probe.
    Test { id_or_name: String },
    /// Validate raw credentials (no DB write).
    Validate {
        #[arg(long)]
        provider: String,
        #[arg(long)]
        api_key: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
    },
    /// Report this CLI's client identity.
    ClientInfo,
    /// OAuth / device-code / cookie import flows.
    Oauth {
        #[command(subcommand)]
        cmd: provider_oauth::ProviderOAuthCmd,
    },
    /// Idempotent upsert from a YAML/JSON document.
    Apply {
        #[arg(long = "from-file", default_value = "-")]
        from_file: String,
        #[arg(long)]
        prune: bool,
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
    /// Show one key by id or name (secret is masked).
    Get { id_or_name: String },
    /// Generate a fresh secret for an existing key.
    Rotate { id_or_name: String },
    /// Delete a key.
    Delete {
        id_or_name: String,
        #[arg(long)]
        strict: bool,
    },
    /// Mark key active.
    Enable { id_or_name: String },
    /// Mark key inactive.
    Disable { id_or_name: String },
    /// Idempotent upsert from YAML/JSON.
    Apply {
        #[arg(long = "from-file", default_value = "-")]
        from_file: String,
        #[arg(long)]
        prune: bool,
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
    /// Show a single pool by name or id.
    Get { name: String },
    /// Edit a pool's URL/type/strict flag.
    Edit {
        name: String,
        #[arg(long)]
        proxy_url: Option<String>,
        #[arg(long)]
        r#type: Option<String>,
        #[arg(long)]
        strict: Option<bool>,
    },
    /// Mark pool active.
    Enable { name: String },
    /// Mark pool inactive.
    Disable { name: String },
    /// Probe an HTTP target through the pool.
    Test {
        name: String,
        #[arg(long, default_value = "https://httpbin.org/get")]
        target: String,
    },
    /// Show recorded success/rtt stats.
    Stats { name: String },
    /// Idempotent upsert from YAML/JSON.
    Apply {
        #[arg(long = "from-file", default_value = "-")]
        from_file: String,
        #[arg(long)]
        prune: bool,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum TunnelCmd {
    /// Local-in-process tunnel start (M1 stub).
    Start {
        #[arg(long, default_value = "cloudflare")]
        provider: String,
        #[arg(long, default_value_t = 4623)]
        port: u16,
    },
    /// Local-in-process tunnel stop (M1 stub).
    Stop,
    /// Local-in-process tunnel status (M1 stub).
    Status,
    /// Enable a tunnel provider via the running server's `/api/tunnel/*`.
    Enable {
        provider: String,
        #[arg(long)]
        port: Option<u16>,
    },
    /// Disable a tunnel provider via the running server's `/api/tunnel/*`.
    Disable { provider: String },
    /// Tailscale-specific helpers (install / login / check / enable / disable).
    Tailscale {
        #[command(subcommand)]
        cmd: tunnel_rt::TailscaleCmd,
    },
}

impl Cli {
    pub fn run(self) -> anyhow::Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        let ctx = self.output_ctx();
        let overrides = self.cli_overrides();
        if let Some(cmd) = self.cmd {
            match cmd {
                Command::Provider { cmd } => {
                    if let ProviderCmd::Oauth { cmd: oauth_cmd } = cmd {
                        let resolved = config::ResolvedConfig::resolve(overrides)?;
                        let exit = rt.block_on(provider_oauth::run(oauth_cmd, &resolved, ctx))?;
                        if exit != 0 {
                            std::process::exit(exit);
                        }
                        Ok(())
                    } else {
                        let db = rt.block_on(Db::load())?;
                        let db = std::sync::Arc::new(db);
                        let rt = tokio::runtime::Runtime::new()?;
                        rt.block_on(run_provider(cmd, &db, ctx))
                    }
                }
                Command::Key { cmd } => {
                    let db = rt.block_on(Db::load())?;
                    let db = std::sync::Arc::new(db);
                    let rt = tokio::runtime::Runtime::new()?;
                    rt.block_on(run_key(cmd, &db, ctx))
                }
                Command::Pool { cmd } => {
                    let db = rt.block_on(Db::load())?;
                    let db = std::sync::Arc::new(db);
                    let rt = tokio::runtime::Runtime::new()?;
                    rt.block_on(run_pool(cmd, &db, ctx))
                }
                Command::Combo { cmd } => {
                    let db = rt.block_on(Db::load())?;
                    let db = std::sync::Arc::new(db);
                    let rt = tokio::runtime::Runtime::new()?;
                    rt.block_on(combo::run(cmd, &db, ctx))
                }
                Command::Models { cmd } => {
                    let db = rt.block_on(Db::load())?;
                    let db = std::sync::Arc::new(db);
                    let rt = tokio::runtime::Runtime::new()?;
                    rt.block_on(models::run(cmd, &db, ctx))
                }
                Command::Tunnel { cmd } => match cmd {
                    TunnelCmd::Start { .. } | TunnelCmd::Stop | TunnelCmd::Status => {
                        let db = rt.block_on(Db::load())?;
                        let db = std::sync::Arc::new(db);
                        let rt = tokio::runtime::Runtime::new()?;
                        rt.block_on(run_tunnel(cmd, db.clone(), ctx))
                    }
                    TunnelCmd::Enable { provider, port } => {
                        let resolved = config::ResolvedConfig::resolve(overrides)?;
                        let rt = tokio::runtime::Runtime::new()?;
                        let exit = rt.block_on(tunnel_rt::run(
                            tunnel_rt::TunnelRtCmd::Enable { provider, port },
                            &resolved,
                            ctx,
                        ))?;
                        if exit != 0 {
                            std::process::exit(exit);
                        }
                        Ok(())
                    }
                    TunnelCmd::Disable { provider } => {
                        let resolved = config::ResolvedConfig::resolve(overrides)?;
                        let rt = tokio::runtime::Runtime::new()?;
                        let exit = rt.block_on(tunnel_rt::run(
                            tunnel_rt::TunnelRtCmd::Disable { provider },
                            &resolved,
                            ctx,
                        ))?;
                        if exit != 0 {
                            std::process::exit(exit);
                        }
                        Ok(())
                    }
                    TunnelCmd::Tailscale { cmd } => {
                        let resolved = config::ResolvedConfig::resolve(overrides)?;
                        let rt = tokio::runtime::Runtime::new()?;
                        let exit = rt.block_on(tunnel_rt::run(
                            tunnel_rt::TunnelRtCmd::Tailscale { cmd },
                            &resolved,
                            ctx,
                        ))?;
                        if exit != 0 {
                            std::process::exit(exit);
                        }
                        Ok(())
                    }
                },
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
                Command::Schema { cmd } => {
                    // Schema commands have no async I/O; we still go through
                    // the same dispatcher so the global flags (--robot, etc.)
                    // are honored uniformly.
                    match cmd {
                        SchemaCmd::List => schema::run_list(ctx),
                        SchemaCmd::Show { resource } => {
                            schema::run_show(ctx, &resource).map(|_| ())
                        }
                        SchemaCmd::Example { resource } => {
                            schema::run_example(ctx, &resource).map(|_| ())
                        }
                        SchemaCmd::Stability => schema::run_stability(ctx),
                    }
                }
                Command::Doctor => {
                    let resolved = config::ResolvedConfig::resolve(overrides)?;
                    rt.block_on(doctor::run(ctx, &resolved)).map(|_| ())
                }
                Command::Server { cmd } => {
                    let resolved = config::ResolvedConfig::resolve(overrides)?;
                    match cmd {
                        ServerCmd::Start { detach, host, port } => {
                            let opts = server::StartOptions {
                                host: host.unwrap_or_else(|| "0.0.0.0".to_string()),
                                port: port.unwrap_or(4623),
                                detach,
                            };
                            rt.block_on(server::run_start(ctx, &resolved, opts))
                                .map(|_| ())
                        }
                        ServerCmd::Stop => {
                            rt.block_on(server::run_stop(ctx, &resolved)).map(|_| ())
                        }
                        ServerCmd::Status => rt
                            .block_on(server::run_status(ctx, &resolved))
                            .map(|_| ()),
                        ServerCmd::Init { force } => rt
                            .block_on(server::run_init(ctx, &resolved, force))
                            .map(|_| ()),
                    }
                }
                Command::Auth { cmd } => {
                    let resolved = config::ResolvedConfig::resolve(overrides)?;
                    match cmd {
                        AuthCmd::Login {
                            url,
                            api_key,
                            profile,
                            no_verify,
                            no_activate,
                        } => rt
                            .block_on(auth::run_login(
                                ctx,
                                auth::LoginOptions {
                                    url,
                                    api_key,
                                    profile,
                                    no_verify,
                                    no_activate,
                                },
                            ))
                            .map(|_| ()),
                        AuthCmd::Logout {
                            profile,
                            keep_default,
                        } => auth::run_logout(
                            ctx,
                            auth::LogoutOptions {
                                profile,
                                keep_default,
                            },
                        )
                        .map(|_| ()),
                        AuthCmd::Whoami { verify } => rt
                            .block_on(auth::run_whoami(ctx, &resolved, verify))
                            .map(|_| ()),
                        AuthCmd::List => auth::run_list(ctx).map(|_| ()),
                    }
                }
                Command::Usage { cmd } => {
                    let resolved = config::ResolvedConfig::resolve(overrides)?;
                    let exit = rt.block_on(usage::run(cmd, &resolved, ctx))?;
                    if exit != 0 {
                        std::process::exit(exit);
                    }
                    Ok(())
                }
                Command::Logs { cmd } => {
                    let resolved = config::ResolvedConfig::resolve(overrides)?;
                    let exit = rt.block_on(logs::run(cmd, &resolved, ctx))?;
                    if exit != 0 {
                        std::process::exit(exit);
                    }
                    Ok(())
                }
                Command::Quota { cmd } => {
                    let resolved = config::ResolvedConfig::resolve(overrides)?;
                    let exit = rt.block_on(quota::run(cmd, &resolved, ctx))?;
                    if exit != 0 {
                        std::process::exit(exit);
                    }
                    Ok(())
                }
                Command::Chat { cmd } => {
                    let resolved = config::ResolvedConfig::resolve(overrides)?;
                    let exit = rt.block_on(chat::run(cmd, &resolved, ctx))?;
                    if exit != 0 {
                        std::process::exit(exit);
                    }
                    Ok(())
                }
                Command::Mitm { cmd } => {
                    let resolved = config::ResolvedConfig::resolve(overrides)?;
                    let exit = rt.block_on(mitm::run(cmd, &resolved, ctx))?;
                    if exit != 0 {
                        std::process::exit(exit);
                    }
                    Ok(())
                }
                Command::Tool { cmd } => {
                    let resolved = config::ResolvedConfig::resolve(overrides)?;
                    let exit = rt.block_on(tool::run(cmd, &resolved, ctx))?;
                    if exit != 0 {
                        std::process::exit(exit);
                    }
                    Ok(())
                }
                Command::Translator { cmd } => {
                    let resolved = config::ResolvedConfig::resolve(overrides)?;
                    let exit = rt.block_on(translator::run(cmd, &resolved, ctx))?;
                    if exit != 0 {
                        std::process::exit(exit);
                    }
                    Ok(())
                }
                Command::Media { cmd } => {
                    let resolved = config::ResolvedConfig::resolve(overrides)?;
                    let exit = rt.block_on(media::run(cmd, &resolved, ctx))?;
                    if exit != 0 {
                        std::process::exit(exit);
                    }
                    Ok(())
                }
                Command::Settings { cmd } => {
                    let resolved = config::ResolvedConfig::resolve(overrides)?;
                    let exit = rt.block_on(settings::run(cmd, &resolved, ctx))?;
                    if exit != 0 {
                        std::process::exit(exit);
                    }
                    Ok(())
                }
                Command::Db { cmd } => {
                    let resolved = config::ResolvedConfig::resolve(overrides)?;
                    let exit = rt.block_on(db::run(cmd, &resolved, ctx))?;
                    if exit != 0 {
                        std::process::exit(exit);
                    }
                    Ok(())
                }
            }
        } else {
            Ok(())
        }
    }
}

pub async fn run_provider(cmd: ProviderCmd, db: &Db, ctx: output::OutputCtx) -> anyhow::Result<()> {
    match cmd {
        ProviderCmd::List { json } => {
            let connections =
                db.provider_connections(crate::db::ProviderConnectionFilter::default());
            let nodes = db.provider_nodes(None);

            if ctx.is_robot() {
                output::emit_robot(
                    "openproxy.v1.provider.list",
                    serde_json::json!({
                        "provider_connections": connections,
                        "provider_nodes": nodes,
                    }),
                )?;
            } else if json {
                // Legacy --json: kept for backward compat. Schema is the
                // same shape as the robot envelope's `data`, just without
                // the wrapper.
                #[derive(serde::Serialize)]
                struct ListOutput {
                    provider_connections: Vec<ProviderConnection>,
                    provider_nodes: Vec<crate::types::ProviderNode>,
                }
                let out = ListOutput {
                    provider_connections: connections,
                    provider_nodes: nodes,
                };
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                output::humanln(ctx, "Provider Connections:");
                for conn in &connections {
                    output::humanln(
                        ctx,
                        format!(
                            "  {} ({}) - {}",
                            conn.provider,
                            conn.auth_type,
                            conn.name.as_deref().unwrap_or("unnamed")
                        ),
                    );
                }
                output::humanln(ctx, "");
                output::humanln(ctx, "Provider Nodes:");
                for node in &nodes {
                    output::humanln(
                        ctx,
                        format!("  {} - {} ({})", node.name, node.r#type, node.id),
                    );
                }
            }
        }
        ProviderCmd::Add { name, config, json } => {
            let config: ProviderConnection = match serde_json::from_str(&config) {
                Ok(c) => c,
                Err(e) => {
                    let exit = output::emit_error(
                        ctx,
                        "validation",
                        &format!("failed to parse --config JSON: {e}"),
                    )?;
                    std::process::exit(exit);
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

            if ctx.is_robot() {
                output::emit_robot(
                    "openproxy.v1.provider.add",
                    serde_json::to_value(&new_conn)?,
                )?;
            } else if json {
                println!("{}", serde_json::to_string_pretty(&new_conn)?);
            } else {
                output::humanln(
                    ctx,
                    format!("Provider '{}' added successfully", new_conn.provider),
                );
            }
        }
        ProviderCmd::Node { cmd } => provider_node::run(cmd, db, ctx).await?,
        ProviderCmd::Models { cmd } => provider_models::run(cmd, db, ctx).await?,
        ProviderCmd::Get { id_or_name } => {
            provider_ext::run(provider_ext::ProviderExtCmd::Get { id_or_name }, db, ctx).await?
        }
        ProviderCmd::Edit {
            id_or_name,
            api_key,
            base_url,
            priority,
            default_model,
        } => {
            provider_ext::run(
                provider_ext::ProviderExtCmd::Edit {
                    id_or_name,
                    api_key,
                    base_url,
                    priority,
                    default_model,
                },
                db,
                ctx,
            )
            .await?
        }
        ProviderCmd::Delete { id_or_name, strict } => {
            provider_ext::run(
                provider_ext::ProviderExtCmd::Delete { id_or_name, strict },
                db,
                ctx,
            )
            .await?
        }
        ProviderCmd::Enable { id_or_name } => {
            provider_ext::run(provider_ext::ProviderExtCmd::Enable { id_or_name }, db, ctx).await?
        }
        ProviderCmd::Disable { id_or_name } => {
            provider_ext::run(
                provider_ext::ProviderExtCmd::Disable { id_or_name },
                db,
                ctx,
            )
            .await?
        }
        ProviderCmd::Test { id_or_name } => {
            provider_ext::run(provider_ext::ProviderExtCmd::Test { id_or_name }, db, ctx).await?
        }
        ProviderCmd::Validate {
            provider,
            api_key,
            base_url,
        } => {
            provider_ext::run(
                provider_ext::ProviderExtCmd::Validate {
                    provider,
                    api_key,
                    base_url,
                },
                db,
                ctx,
            )
            .await?
        }
        ProviderCmd::ClientInfo => {
            provider_ext::run(provider_ext::ProviderExtCmd::ClientInfo, db, ctx).await?
        }
        ProviderCmd::Oauth { .. } => {
            // OAuth subcommands need access to the resolved config (for the
            // runtime base URL), which the `run_provider` helper does not
            // carry. The CLI dispatcher in `main` routes them via
            // `dispatch_provider_oauth` directly.
            unreachable!(
                "provider oauth must be dispatched via the main CLI entrypoint, \
                 not run_provider"
            );
        }
        ProviderCmd::Apply { from_file, prune } => {
            provider_ext::run(
                provider_ext::ProviderExtCmd::Apply { from_file, prune },
                db,
                ctx,
            )
            .await?
        }
    }
    Ok(())
}
pub async fn run_tunnel(
    cmd: TunnelCmd,
    db: std::sync::Arc<Db>,
    ctx: output::OutputCtx,
) -> anyhow::Result<()> {
    let tunnel_manager = TunnelManager::new((db).clone());

    match cmd {
        TunnelCmd::Start { provider, port } => {
            let provider = provider
                .parse::<TunnelProvider>()
                .map_err(|e| anyhow::anyhow!("{}", e))?;

            output::humanln(
                ctx,
                format!("Starting {} tunnel on port {}...", provider, port),
            );
            tunnel_manager.start(provider, port).await?;

            // Wait a bit for URL to appear
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

            let status = tunnel_manager.status().await;
            if ctx.is_robot() {
                output::emit_robot(
                    "openproxy.v1.tunnel.start",
                    serde_json::json!({
                        "running": status.running,
                        "provider": status.provider,
                        "url": status.url,
                        "pid": status.pid,
                    }),
                )?;
                if !status.running {
                    std::process::exit(1);
                }
            } else if status.running {
                output::humanln(ctx, "Tunnel started successfully");
                if let Some(url) = status.url {
                    output::humanln(ctx, format!("  URL: {}", url));
                }
                if let Some(pid) = status.pid {
                    output::humanln(ctx, format!("  PID: {}", pid));
                }
            } else {
                let exit = output::emit_error(ctx, "other", "tunnel failed to start")?;
                std::process::exit(exit);
            }
        }
        TunnelCmd::Stop => {
            output::humanln(ctx, "Stopping tunnel...");
            tunnel_manager.stop().await?;
            if ctx.is_robot() {
                output::emit_robot(
                    "openproxy.v1.tunnel.stop",
                    serde_json::json!({"stopped": true}),
                )?;
            } else {
                output::humanln(ctx, "Tunnel stopped");
            }
        }
        TunnelCmd::Status => {
            let status = tunnel_manager.status().await;
            if ctx.is_robot() {
                output::emit_robot(
                    "openproxy.v1.tunnel.status",
                    serde_json::json!({
                        "running": status.running,
                        "provider": status.provider,
                        "url": status.url,
                        "pid": status.pid,
                    }),
                )?;
            } else if status.running {
                output::humanln(ctx, "Tunnel is running");
                if let Some(p) = status.provider {
                    output::humanln(ctx, format!("  Provider: {}", p));
                }
                if let Some(url) = status.url {
                    output::humanln(ctx, format!("  URL: {}", url));
                }
                if let Some(pid) = status.pid {
                    output::humanln(ctx, format!("  PID: {}", pid));
                }
            } else {
                output::humanln(ctx, "Tunnel is stopped");
            }
        }
        TunnelCmd::Enable { .. } | TunnelCmd::Disable { .. } | TunnelCmd::Tailscale { .. } => {
            // Routed via `tunnel_rt` in `Cli::run`; unreachable here.
            unreachable!("runtime tunnel commands dispatched separately");
        }
    }
    Ok(())
}

pub async fn run_key(cmd: KeyCmd, db: &Db, ctx: output::OutputCtx) -> anyhow::Result<()> {
    match cmd {
        KeyCmd::List { json } => {
            let snapshot = db.snapshot();
            let api_keys = &snapshot.api_keys;

            if ctx.is_robot() {
                output::emit_robot(
                    "openproxy.v1.key.list",
                    serde_json::json!({"keys": api_keys}),
                )?;
            } else if json {
                println!("{}", serde_json::to_string_pretty(api_keys)?);
            } else {
                output::humanln(ctx, "API Keys:");
                for k in api_keys {
                    let key_preview = k.key.chars().take(8).collect::<String>();
                    output::humanln(
                        ctx,
                        format!(
                            "  {} [{}...] ({})",
                            k.name,
                            key_preview,
                            if k.is_active() { "active" } else { "inactive" }
                        ),
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

            if ctx.is_robot() {
                output::emit_robot("openproxy.v1.key.add", serde_json::to_value(&new_key)?)?;
            } else if json {
                println!("{}", serde_json::to_string_pretty(&new_key)?);
            } else {
                output::humanln(ctx, "API key added successfully");
            }
        }
        KeyCmd::Get { id_or_name } => {
            key_ext::run(key_ext::KeyExtCmd::Get { id_or_name }, db, ctx).await?
        }
        KeyCmd::Rotate { id_or_name } => {
            key_ext::run(key_ext::KeyExtCmd::Rotate { id_or_name }, db, ctx).await?
        }
        KeyCmd::Delete { id_or_name, strict } => {
            key_ext::run(key_ext::KeyExtCmd::Delete { id_or_name, strict }, db, ctx).await?
        }
        KeyCmd::Enable { id_or_name } => {
            key_ext::run(key_ext::KeyExtCmd::Enable { id_or_name }, db, ctx).await?
        }
        KeyCmd::Disable { id_or_name } => {
            key_ext::run(key_ext::KeyExtCmd::Disable { id_or_name }, db, ctx).await?
        }
        KeyCmd::Apply { from_file, prune } => {
            key_ext::run(key_ext::KeyExtCmd::Apply { from_file, prune }, db, ctx).await?
        }
    }
    Ok(())
}

pub async fn run_pool(cmd: PoolCmd, db: &Db, ctx: output::OutputCtx) -> anyhow::Result<()> {
    match cmd {
        PoolCmd::List { json } => {
            let snapshot = db.snapshot();
            let pools = &snapshot.proxy_pools;

            if ctx.is_robot() {
                output::emit_robot(
                    "openproxy.v1.pool.list",
                    serde_json::json!({"pools": pools}),
                )?;
            } else if json {
                println!("{}", serde_json::to_string_pretty(pools)?);
            } else {
                output::humanln(ctx, "Connection Pools:");
                for pool in pools {
                    let status = pool.test_status.as_deref().unwrap_or("unknown");
                    output::humanln(
                        ctx,
                        format!("  {} - {} ({})", pool.name, pool.r#type, status),
                    );
                }
            }
        }
        PoolCmd::Status { name, json } => {
            let snapshot = db.snapshot();
            let pool = snapshot.proxy_pools.iter().find(|p| p.name == name);

            match pool {
                Some(pool) => {
                    if ctx.is_robot() {
                        output::emit_robot(
                            "openproxy.v1.pool.status",
                            serde_json::to_value(pool)?,
                        )?;
                    } else if json {
                        println!("{}", serde_json::to_string_pretty(pool)?);
                    } else {
                        output::humanln(ctx, format!("Pool: {}", pool.name));
                        output::humanln(ctx, format!("  Type: {}", pool.r#type));
                        output::humanln(ctx, format!("  URL: {}", pool.proxy_url));
                        output::humanln(
                            ctx,
                            format!(
                                "  Status: {:?}",
                                pool.test_status.as_deref().unwrap_or("unknown")
                            ),
                        );
                        output::humanln(ctx, format!("  Success Rate: {:?}", pool.success_rate));
                        output::humanln(ctx, format!("  RTT (ms): {:?}", pool.rtt_ms));
                    }
                }
                None => {
                    let exit =
                        output::emit_error(ctx, "not_found", &format!("pool '{name}' not found"))?;
                    std::process::exit(exit);
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

            if ctx.is_robot() {
                output::emit_robot("openproxy.v1.pool.create", serde_json::to_value(&new_pool)?)?;
            } else if json {
                println!("{}", serde_json::to_string_pretty(&new_pool)?);
            } else {
                output::humanln(ctx, format!("Pool '{}' created successfully", name));
            }
        }
        PoolCmd::Delete { name, json } => {
            let snapshot = db.snapshot();
            let pool_exists = snapshot.proxy_pools.iter().any(|p| p.name == name);

            if !pool_exists {
                let exit =
                    output::emit_error(ctx, "not_found", &format!("pool '{name}' not found"))?;
                std::process::exit(exit);
            }

            db.update(|db| {
                db.proxy_pools.retain(|p| p.name != name);
            })
            .await?;

            if ctx.is_robot() {
                output::emit_robot(
                    "openproxy.v1.pool.delete",
                    serde_json::json!({"deleted": name}),
                )?;
            } else if json {
                #[derive(serde::Serialize)]
                struct DeleteOutput {
                    deleted: String,
                }
                println!(
                    "{}",
                    serde_json::to_string_pretty(&DeleteOutput { deleted: name })?
                );
            } else {
                output::humanln(ctx, format!("Pool '{}' deleted successfully", name));
            }
        }
        PoolCmd::Get { name } => pool_ext::run(pool_ext::PoolExtCmd::Get { name }, db, ctx).await?,
        PoolCmd::Edit {
            name,
            proxy_url,
            r#type,
            strict,
        } => {
            pool_ext::run(
                pool_ext::PoolExtCmd::Edit {
                    name,
                    proxy_url,
                    r#type,
                    strict,
                },
                db,
                ctx,
            )
            .await?
        }
        PoolCmd::Enable { name } => {
            pool_ext::run(pool_ext::PoolExtCmd::Enable { name }, db, ctx).await?
        }
        PoolCmd::Disable { name } => {
            pool_ext::run(pool_ext::PoolExtCmd::Disable { name }, db, ctx).await?
        }
        PoolCmd::Test { name, target } => {
            pool_ext::run(pool_ext::PoolExtCmd::Test { name, target }, db, ctx).await?
        }
        PoolCmd::Stats { name } => {
            pool_ext::run(pool_ext::PoolExtCmd::Stats { name }, db, ctx).await?
        }
        PoolCmd::Apply { from_file, prune } => {
            pool_ext::run(pool_ext::PoolExtCmd::Apply { from_file, prune }, db, ctx).await?
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
