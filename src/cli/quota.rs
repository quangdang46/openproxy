//! `openproxy quota *` — per-provider quota tracking and reset.
//!
//! Built on top of the existing usage providers endpoint
//! (`/api/usage/providers`) and the local `db.json` for reset/refresh. The
//! v1 quota schema is intentionally compact: provider id, requests,
//! prompt/completion/total tokens, cost.

use clap::Subcommand;
use serde_json::{json, Value};

use crate::cli::config::ResolvedConfig;
use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};
use crate::cli::runtime::{require_runtime, rt_error_to_exit, Runtime};

#[derive(Debug, Clone, Subcommand)]
pub enum QuotaCmd {
    /// List every provider with its aggregated counters.
    List,
    /// Show one provider's counters by id (matches `provider`).
    Get { provider: String },
    /// Reset all in-memory usage counters by clearing the usage history.
    Reset {
        /// Skip the confirmation prompt in human mode.
        #[arg(long)]
        yes: bool,
    },
    /// Re-pull the per-provider rollup from the server (no state change).
    Refresh,
}

pub async fn run(cmd: QuotaCmd, cfg: &ResolvedConfig, ctx: OutputCtx) -> anyhow::Result<i32> {
    let rt = match require_runtime(cfg).await {
        Ok(rt) => rt,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    match cmd {
        QuotaCmd::List => run_list(&rt, ctx).await,
        QuotaCmd::Get { provider } => run_get(&rt, ctx, &provider).await,
        QuotaCmd::Reset { yes } => run_reset(&rt, ctx, yes).await,
        QuotaCmd::Refresh => run_refresh(&rt, ctx).await,
    }
}

async fn fetch_quotas(rt: &Runtime) -> Result<Vec<Value>, crate::cli::runtime::RuntimeError> {
    let payload = rt.get_json("/api/usage/providers").await?;
    Ok(payload
        .get("providers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

async fn run_list(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match fetch_quotas(rt).await {
        Ok(rows) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.quota.list", json!({"quotas": rows}))?;
            } else {
                for row in &rows {
                    humanln(
                        ctx,
                        format!(
                            "{:24} requests={:6} tokens={:8} cost=${:.4}",
                            row.get("provider").and_then(Value::as_str).unwrap_or("?"),
                            row.get("requests").and_then(Value::as_u64).unwrap_or(0),
                            row.get("tokens").and_then(Value::as_u64).unwrap_or(0),
                            row.get("cost").and_then(Value::as_f64).unwrap_or(0.0),
                        ),
                    );
                }
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_get(rt: &Runtime, ctx: OutputCtx, provider: &str) -> anyhow::Result<i32> {
    let rows = match fetch_quotas(rt).await {
        Ok(rows) => rows,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    let matching = rows.iter().find(|r| {
        r.get("provider").and_then(Value::as_str) == Some(provider)
            || r.get("id").and_then(Value::as_str) == Some(provider)
    });
    match matching {
        Some(row) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.quota.get", row.clone())?;
            } else {
                let pretty = serde_json::to_string_pretty(row).unwrap_or_default();
                humanln(ctx, pretty);
            }
            Ok(0)
        }
        None => Ok(emit_error(
            ctx,
            "not_found",
            &format!("no quota row for provider '{provider}'"),
        )?),
    }
}

async fn run_reset(rt: &Runtime, ctx: OutputCtx, yes: bool) -> anyhow::Result<i32> {
    if !yes && !ctx.is_robot() {
        humanln(
            ctx,
            "Reset will clear the in-memory usage history. Pass --yes to confirm.",
        );
        return Ok(0);
    }
    let body = json!({"action": "reset", "confirm": true});
    match rt.post_json("/api/observability/clear", &body).await {
        Ok(_) => {
            if ctx.is_robot() {
                emit_robot(
                    "openproxy.v1.quota.reset",
                    json!({"ok": true, "cleared": "usage history (process memory)"}),
                )?;
            } else {
                humanln(ctx, "Cleared usage history in memory.");
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_refresh(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match fetch_quotas(rt).await {
        Ok(rows) => {
            if ctx.is_robot() {
                emit_robot(
                    "openproxy.v1.quota.refresh",
                    json!({"refreshed": rows.len(), "quotas": rows}),
                )?;
            } else {
                humanln(ctx, format!("Refreshed {} provider rows.", rows.len()));
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}
