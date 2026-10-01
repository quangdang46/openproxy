//! `openproxy pxpipe *` — PXPIPE token-saver status, health, stats, and logs.
//!
//! Backed by `/api/pxpipe/*` on the running server.

use clap::Subcommand;
use serde_json::{json, Value};

use crate::cli::config::ResolvedConfig;
use crate::cli::output::{emit_robot, humanln, OutputCtx};
use crate::cli::runtime::{require_runtime, rt_error_to_exit, Runtime};

#[derive(Debug, Clone, Subcommand)]
pub enum PxpipeCmd {
    /// Report PXPIPE install/version/config status.
    Status,
    /// Run PXPIPE health checks.
    Health,
    /// Show compression windows + timeline + recent events.
    Stats,
    /// Show install log + transform events.
    Logs {
        /// Max log lines to show.
        #[arg(long)]
        limit: Option<usize>,
    },
}

pub async fn run(cmd: PxpipeCmd, cfg: &ResolvedConfig, ctx: OutputCtx) -> anyhow::Result<i32> {
    let rt = match require_runtime(cfg).await {
        Ok(rt) => rt,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    match cmd {
        PxpipeCmd::Status => run_status(&rt, ctx).await,
        PxpipeCmd::Health => run_health(&rt, ctx).await,
        PxpipeCmd::Stats => run_stats(&rt, ctx).await,
        PxpipeCmd::Logs { limit } => run_logs(&rt, ctx, limit).await,
    }
}

/// One row per subcommand: the `<action>` segment of its `openproxy.v1.pxpipe.*`
/// schema. Every other robot-emitting CLI command carries `<area>.<action>`
/// (quota.list, settings.get, db.export) — pxpipe used to emit the bare
/// `openproxy.v1.pxpipe` for all four, so an agent could not tell a status
/// report from a health report without sniffing the payload.
const ACTION_STATUS: &str = "openproxy.v1.pxpipe.status";
const ACTION_HEALTH: &str = "openproxy.v1.pxpipe.health";
const ACTION_STATS: &str = "openproxy.v1.pxpipe.stats";
const ACTION_LOGS: &str = "openproxy.v1.pxpipe.logs";

async fn run_status(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    let value = match rt.get_json("/api/pxpipe/status").await {
        Ok(v) => v,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    print_json_value(ctx, ACTION_STATUS, &value);
    Ok(0)
}

async fn run_health(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    let value = match rt.get_json("/api/pxpipe/health").await {
        Ok(v) => v,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    print_json_value(ctx, ACTION_HEALTH, &value);
    Ok(0)
}

async fn run_stats(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    let value = match rt.get_json("/api/pxpipe/stats").await {
        Ok(v) => v,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    print_json_value(ctx, ACTION_STATS, &value);
    Ok(0)
}

async fn run_logs(rt: &Runtime, ctx: OutputCtx, limit: Option<usize>) -> anyhow::Result<i32> {
    let query = if let Some(n) = limit {
        json!({ "limit": n })
    } else {
        json!({})
    };
    let value = match rt.get_json_query("/api/pxpipe/logs", &query).await {
        Ok(v) => v,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    print_json_value(ctx, ACTION_LOGS, &value);
    Ok(0)
}

/// `/api/pxpipe/*` answers with the payload *flat* (`{installed, running, …}`),
/// the same shape the dashboard consumes directly — see
/// `PxpipePageClient.tsx`, which reads `status.installed` and `stats.windows`.
///
/// This used to wrap that payload in a second `{ok, data, error}` envelope, so
/// `--robot` produced `.data.data.installed` and no agent following the
/// documented `openproxy.v1.*` contract could read a single field. `.data` is
/// now the payload itself, matching quota.list / settings.get.
fn print_json_value(ctx: OutputCtx, schema: &str, value: &Value) {
    if ctx.is_robot() {
        let _ = emit_robot(schema, value.clone());
    } else {
        humanln(
            ctx,
            &serde_json::to_string_pretty(value).unwrap_or_default(),
        );
    }
}
