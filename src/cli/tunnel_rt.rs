//! `openproxy tunnel *` — runtime tunnel commands (PLAN v3 mục 4.11).
//!
//! Distinct from the in-process `core::tunnel::TunnelManager` used by
//! `Command::Tunnel` (Start/Stop/Status), these commands hit the live
//! server's `/api/tunnel/*` endpoints so they can flip persisted settings
//! (cloudflare on/off, tailscale install/login/check, etc.). They are the
//! M5 extension of the M1 stub.

use clap::Subcommand;
use serde_json::{json, Value};

use crate::cli::config::ResolvedConfig;
use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};
use crate::cli::runtime::{require_runtime, rt_error_to_exit, Runtime};

#[derive(Debug, Clone, Subcommand)]
pub enum TunnelRtCmd {
    /// Turn on a tunnel provider (`cloudflare` is the default).
    Enable {
        /// Provider name. One of `cloudflare`, `tailscale`.
        provider: String,
        /// Bind port to forward (default: server port).
        #[arg(long)]
        port: Option<u16>,
    },
    /// Turn off a tunnel provider.
    Disable {
        /// Provider name. One of `cloudflare`, `tailscale`.
        provider: String,
    },
    /// Tailscale-specific helpers.
    Tailscale {
        #[command(subcommand)]
        cmd: TailscaleCmd,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum TailscaleCmd {
    /// Install the tailscale binary (prints the script the server expects
    /// the operator to run).
    Install,
    /// Open `tailscale login` on the server host.
    Login,
    /// Report whether tailscale is installed + logged in.
    Check,
    /// Enable the tailscale tunnel.
    Enable {
        /// Bind port to forward (default: server port).
        #[arg(long)]
        port: Option<u16>,
    },
    /// Disable the tailscale tunnel.
    Disable,
}

pub async fn run(cmd: TunnelRtCmd, cfg: &ResolvedConfig, ctx: OutputCtx) -> anyhow::Result<i32> {
    let rt = match require_runtime(cfg).await {
        Ok(rt) => rt,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    match cmd {
        TunnelRtCmd::Enable { provider, port } => {
            run_enable_disable(&rt, ctx, &provider, port, true).await
        }
        TunnelRtCmd::Disable { provider } => {
            run_enable_disable(&rt, ctx, &provider, None, false).await
        }
        TunnelRtCmd::Tailscale { cmd } => match cmd {
            TailscaleCmd::Install => {
                simple(&rt, ctx, "/api/tunnel/tailscale-install", "install").await
            }
            TailscaleCmd::Login => simple(&rt, ctx, "/api/tunnel/tailscale-login", "login").await,
            TailscaleCmd::Check => run_check(&rt, ctx).await,
            TailscaleCmd::Enable { port } => {
                run_enable_disable(&rt, ctx, "tailscale", port, true).await
            }
            TailscaleCmd::Disable => run_enable_disable(&rt, ctx, "tailscale", None, false).await,
        },
    }
}

async fn run_enable_disable(
    rt: &Runtime,
    ctx: OutputCtx,
    provider: &str,
    port: Option<u16>,
    enable: bool,
) -> anyhow::Result<i32> {
    let provider = provider.to_ascii_lowercase();
    let (path, body) = match (provider.as_str(), enable) {
        ("cloudflare", true) => ("/api/tunnel/enable", json!({"port": port})),
        ("cloudflare", false) => ("/api/tunnel/disable", Value::Null),
        ("tailscale", true) => ("/api/tunnel/tailscale-enable", json!({"port": port})),
        ("tailscale", false) => ("/api/tunnel/tailscale-disable", Value::Null),
        _ => {
            return Ok(emit_error(
                ctx,
                "usage",
                "provider must be `cloudflare` or `tailscale`",
            )?);
        }
    };
    let result = if body.is_null() {
        rt.post_empty(path).await
    } else {
        rt.post_json(path, &body).await
    };
    match result {
        Ok(payload) => {
            let schema = if enable {
                "openproxy.v1.tunnel.enable"
            } else {
                "openproxy.v1.tunnel.disable"
            };
            if ctx.is_robot() {
                emit_robot(schema, payload)?;
            } else {
                humanln(
                    ctx,
                    format!(
                        "Tunnel {} {} on `{provider}`.",
                        if enable { "enabled" } else { "disabled" },
                        if enable { "started" } else { "stopped" },
                    ),
                );
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn simple(rt: &Runtime, ctx: OutputCtx, path: &str, action: &str) -> anyhow::Result<i32> {
    match rt.post_empty(path).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot(
                    &format!("openproxy.v1.tunnel.tailscale.{}", action),
                    payload,
                )?;
            } else {
                let msg = payload
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("ok");
                humanln(ctx, msg);
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_check(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.get_json("/api/tunnel/tailscale-check").await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.tunnel.tailscale.check", payload)?;
            } else {
                let installed = payload
                    .get("installed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let logged_in = payload
                    .get("loggedIn")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let daemon = payload
                    .get("daemonRunning")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                humanln(
                    ctx,
                    format!(
                        "tailscale: installed={installed} daemon_running={daemon} logged_in={logged_in}"
                    ),
                );
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}
