//! `openproxy provider oauth *` — wrap the OAuth/device-code/import endpoints.
//!
//! Each subcommand maps to a single HTTP endpoint on the running server.
//! For `start` and `status` we keep things stateless: the server already owns
//! the OAuth state machine, so the CLI just renders whatever JSON comes
//! back.

use clap::Subcommand;
use serde_json::{json, Value};

use crate::cli::config::ResolvedConfig;
use crate::cli::output::{emit_robot, humanln, OutputCtx};
use crate::cli::runtime::{read_input, require_runtime, rt_error_to_exit, Runtime};

#[derive(Debug, Clone, Subcommand)]
pub enum ProviderOAuthCmd {
    /// Begin the OAuth flow for a provider. Returns the URL the user must
    /// open in a browser, plus state metadata.
    Start {
        /// Provider slug (e.g. `claude`, `codex`, `kiro`).
        provider: String,
        /// Optional redirect URI override.
        #[arg(long)]
        redirect_uri: Option<String>,
    },
    /// Long-poll the device-code endpoint until the user finishes auth or
    /// the server reports an error.
    Poll {
        provider: String,
        /// Optional device code returned by `start` (if not provided, the
        /// server uses whatever pending flow it tracked).
        #[arg(long)]
        device_code: Option<String>,
    },
    /// Report the current OAuth status for a provider.
    Status { provider: String },
    /// Refresh the access token for a provider.
    Refresh {
        provider: String,
        #[arg(long)]
        refresh_token: Option<String>,
    },
    /// Import the Kiro SSO cache. With `--auto` discovers cache files
    /// locally; otherwise expects an explicit payload on stdin.
    ImportKiro {
        /// Auto-discover Kiro cache files instead of reading stdin.
        #[arg(long)]
        auto: bool,
    },
    /// Submit a raw iFlow cookie (read from stdin by default).
    IflowCookie {
        #[arg(long, default_value = "-")]
        cookie: String,
    },
    /// Submit a GitLab personal access token (read from stdin by default).
    GitlabPat {
        #[arg(long, default_value = "-")]
        token: String,
    },
}

pub async fn run(
    cmd: ProviderOAuthCmd,
    cfg: &ResolvedConfig,
    ctx: OutputCtx,
) -> anyhow::Result<i32> {
    let rt = match require_runtime(cfg).await {
        Ok(rt) => rt,
        Err(e) => return rt_error_to_exit(ctx, e),
    };

    match cmd {
        ProviderOAuthCmd::Start {
            provider,
            redirect_uri,
        } => run_start(&rt, ctx, &provider, redirect_uri).await,
        ProviderOAuthCmd::Poll {
            provider,
            device_code,
        } => run_poll(&rt, ctx, &provider, device_code).await,
        ProviderOAuthCmd::Status { provider } => run_status(&rt, ctx, &provider).await,
        ProviderOAuthCmd::Refresh {
            provider,
            refresh_token,
        } => run_refresh(&rt, ctx, &provider, refresh_token).await,
        ProviderOAuthCmd::ImportKiro { auto } => run_import_kiro(&rt, ctx, auto).await,
        ProviderOAuthCmd::IflowCookie { cookie } => run_iflow_cookie(&rt, ctx, &cookie).await,
        ProviderOAuthCmd::GitlabPat { token } => run_gitlab_pat(&rt, ctx, &token).await,
    }
}

async fn run_start(
    rt: &Runtime,
    ctx: OutputCtx,
    provider: &str,
    redirect_uri: Option<String>,
) -> anyhow::Result<i32> {
    let mut path = format!("/api/oauth/{}/start", urlencoding::encode(provider));
    if let Some(uri) = redirect_uri {
        path.push_str(&format!("?redirect_uri={}", urlencoding::encode(&uri)));
    }
    match rt.get_json(&path).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.oauth.start", payload)?;
            } else {
                let url = payload
                    .get("url")
                    .or_else(|| payload.get("verification_uri_complete"))
                    .or_else(|| payload.get("verification_uri"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                humanln(ctx, format!("Open this URL to authorize:\n  {}", url));
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_poll(
    rt: &Runtime,
    ctx: OutputCtx,
    provider: &str,
    device_code: Option<String>,
) -> anyhow::Result<i32> {
    let path = format!("/api/oauth/{}/poll", urlencoding::encode(provider));
    let body = json!({"device_code": device_code });
    match rt.post_json(&path, &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.oauth.poll", payload)?;
            } else {
                let status = payload.get("status").and_then(Value::as_str).unwrap_or("?");
                humanln(ctx, format!("status: {}", status));
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_status(rt: &Runtime, ctx: OutputCtx, provider: &str) -> anyhow::Result<i32> {
    let path = format!("/api/oauth/{}/status", urlencoding::encode(provider));
    match rt.get_json(&path).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.oauth.status", payload)?;
            } else {
                humanln(
                    ctx,
                    serde_json::to_string_pretty(&payload).unwrap_or_default(),
                );
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_refresh(
    rt: &Runtime,
    ctx: OutputCtx,
    provider: &str,
    refresh_token: Option<String>,
) -> anyhow::Result<i32> {
    let path = format!("/api/oauth/{}/refresh", urlencoding::encode(provider));
    let body = json!({"refresh_token": refresh_token });
    match rt.post_json(&path, &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.oauth.refresh", payload)?;
            } else {
                let status = payload
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("ok");
                humanln(ctx, format!("refresh: {}", status));
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_import_kiro(rt: &Runtime, ctx: OutputCtx, auto: bool) -> anyhow::Result<i32> {
    if auto {
        match rt.get_json("/api/oauth/kiro/auto-import").await {
            Ok(payload) => {
                if ctx.is_robot() {
                    emit_robot("openproxy.v1.oauth.import_kiro", payload)?;
                } else {
                    humanln(
                        ctx,
                        format!(
                            "Discovered {} cache files",
                            payload.get("count").and_then(Value::as_u64).unwrap_or(0)
                        ),
                    );
                }
                Ok(0)
            }
            Err(e) => rt_error_to_exit(ctx, e),
        }
    } else {
        let raw = read_input("-")?;
        let body: Value = serde_json::from_str(raw.trim())?;
        match rt.post_json("/api/oauth/kiro/import", &body).await {
            Ok(payload) => {
                if ctx.is_robot() {
                    emit_robot("openproxy.v1.oauth.import_kiro", payload)?;
                } else {
                    humanln(ctx, "Imported Kiro identity.");
                }
                Ok(0)
            }
            Err(e) => rt_error_to_exit(ctx, e),
        }
    }
}

async fn run_iflow_cookie(rt: &Runtime, ctx: OutputCtx, source: &str) -> anyhow::Result<i32> {
    let cookie = read_input(source)?;
    let body = json!({"cookie": cookie.trim()});
    match rt.post_json("/api/oauth/iflow/cookie", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.oauth.iflow_cookie", payload)?;
            } else {
                humanln(ctx, "Imported iFlow cookie.");
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_gitlab_pat(rt: &Runtime, ctx: OutputCtx, source: &str) -> anyhow::Result<i32> {
    let token = read_input(source)?;
    let body = json!({"token": token.trim()});
    match rt.post_json("/api/oauth/gitlab/pat", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.oauth.gitlab_pat", payload)?;
            } else {
                humanln(ctx, "Imported GitLab PAT.");
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}
