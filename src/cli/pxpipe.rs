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

async fn run_status(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    let value = match rt.get_json("/api/pxpipe/status").await {
        Ok(v) => v,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    print_json_value(ctx, &value);
    Ok(0)
}

async fn run_health(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    let value = match rt.get_json("/api/pxpipe/health").await {
        Ok(v) => v,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    print_json_value(ctx, &value);
    Ok(0)
}

async fn run_stats(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    let value = match rt.get_json("/api/pxpipe/stats").await {
        Ok(v) => v,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    print_json_value(ctx, &value);
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
    print_json_value(ctx, &value);
    Ok(0)
}

fn print_json_value(ctx: OutputCtx, value: &Value) {
    if ctx.is_robot() {
        let _ = emit_robot("openproxy.v1.pxpipe", json!({
            "ok": true,
            "data": value,
            "error": null,
        }));
    } else {
        humanln(ctx, &serde_json::to_string_pretty(value).unwrap_or_default());
    }
}
