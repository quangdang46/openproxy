//! `openproxy logs *` — tail/export/clear the in-memory log buffer.
//!
//! Backed by `/api/observability/*` on the running server.

use clap::Subcommand;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::io::Write;

use crate::cli::config::ResolvedConfig;
use crate::cli::output::{emit_robot, humanln, OutputCtx};
use crate::cli::runtime::{require_runtime, rt_error_to_exit, Runtime};

#[derive(Debug, Clone, Subcommand)]
pub enum LogsCmd {
    /// Print recent log lines. With `--follow` keeps the connection open
    /// (NDJSON, one line per event) until Ctrl+C.
    Tail {
        /// Stream new log lines as they arrive.
        #[arg(long)]
        follow: bool,
        /// When not following, max lines to emit from the snapshot.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Dump the entire current buffer to stdout (one event per line in robot mode).
    Export {
        /// File to write to. `-` (default) means stdout.
        #[arg(long, default_value = "-")]
        out: String,
    },
    /// Clear the in-memory buffer on the server.
    Clear,
    /// Show rough log + usage stats.
    Stats,
}

pub async fn run(cmd: LogsCmd, cfg: &ResolvedConfig, ctx: OutputCtx) -> anyhow::Result<i32> {
    let rt = match require_runtime(cfg).await {
        Ok(rt) => rt,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    match cmd {
        LogsCmd::Tail { follow, limit } => run_tail(&rt, ctx, follow, limit).await,
        LogsCmd::Export { out } => run_export(&rt, ctx, &out).await,
        LogsCmd::Clear => run_clear(&rt, ctx).await,
        LogsCmd::Stats => run_stats(&rt, ctx).await,
    }
}

async fn run_tail(
    rt: &Runtime,
    ctx: OutputCtx,
    follow: bool,
    limit: Option<usize>,
) -> anyhow::Result<i32> {
    let snapshot = match rt.get_json("/api/observability/logs").await {
        Ok(v) => v,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    let lines = snapshot
        .get("logs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let start = limit.map(|n| lines.len().saturating_sub(n)).unwrap_or(0);
    let mut stdout = std::io::stdout().lock();
    for line in &lines[start..] {
        let line_text = line.as_str().unwrap_or("");
        if ctx.is_robot() {
            let envelope = json!({
                "schema": "openproxy.v1.log.event",
                "ok": true,
                "data": {"kind": "line", "line": line_text},
                "error": null,
                "meta": {"buffered": true},
            });
            writeln!(stdout, "{}", envelope)?;
        } else {
            writeln!(stdout, "{}", line_text)?;
        }
    }
    stdout.flush()?;

    if !follow {
        return Ok(0);
    }

    let mut stream = match rt.stream_sse("/api/observability/stream").await {
        Ok(s) => s,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    while let Some(chunk) = stream.next().await {
        let bytes = match chunk {
            Ok(b) => b,
            Err(e) => return rt_error_to_exit(ctx, e),
        };
        let body: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"kind": "raw", "raw": String::from_utf8_lossy(&bytes)}));
        if ctx.is_robot() {
            let envelope = json!({
                "schema": "openproxy.v1.log.event",
                "ok": true,
                "data": body,
                "error": null,
                "meta": {},
            });
            writeln!(stdout, "{}", envelope)?;
        } else if let Some(line) = body.get("line").and_then(Value::as_str) {
            writeln!(stdout, "{}", line)?;
        }
        stdout.flush()?;
    }
    Ok(0)
}

async fn run_export(rt: &Runtime, ctx: OutputCtx, out: &str) -> anyhow::Result<i32> {
    let snapshot = match rt.get_json("/api/observability/logs").await {
        Ok(v) => v,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    if ctx.is_robot() {
        emit_robot("openproxy.v1.log.export", snapshot)?;
        return Ok(0);
    }
    let logs = snapshot
        .get("logs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if out == "-" {
        let mut stdout = std::io::stdout().lock();
        for line in &logs {
            writeln!(stdout, "{}", line.as_str().unwrap_or(""))?;
        }
        stdout.flush()?;
    } else {
        let mut buf = String::new();
        for line in &logs {
            buf.push_str(line.as_str().unwrap_or(""));
            buf.push('\n');
        }
        std::fs::write(out, buf)?;
        humanln(ctx, format!("Wrote {} lines to {}", logs.len(), out));
    }
    Ok(0)
}

async fn run_clear(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.post_empty("/api/observability/clear").await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.log.clear", payload)?;
            } else {
                humanln(ctx, "Cleared log buffer.");
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_stats(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.get_json("/api/observability/stats").await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.log.stats", payload)?;
            } else {
                humanln(
                    ctx,
                    format!(
                        "Buffered lines: {}",
                        payload
                            .get("logBufferLines")
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                    ),
                );
                humanln(
                    ctx,
                    format!(
                        "Total requests (lifetime): {}",
                        payload
                            .get("totalRequestsLifetime")
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                    ),
                );
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}
