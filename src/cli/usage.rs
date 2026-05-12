//! `openproxy usage *` — runtime usage statistics.
//!
//! These commands talk to the running server's `/api/usage/*` endpoints.
//! They are deliberately thin wrappers: the server already owns formatting
//! and pricing logic; the CLI just adds the agent-friendly `--robot`
//! envelope and stable exit codes.
//!
//! The streaming variant (`usage stream`) tails `/api/usage/stream` (SSE)
//! and emits one NDJSON line per event until the user hits Ctrl+C. There is
//! no client-side timeout — callers can wrap the command in `timeout(1)` if
//! they need bounded behaviour for tests.

use clap::Subcommand;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::io::Write;

use crate::cli::config::ResolvedConfig;
use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};
use crate::cli::runtime::{require_runtime, rt_error_to_exit, Runtime};

#[derive(Debug, Clone, Subcommand)]
pub enum UsageCmd {
    /// Lifetime totals (requests, prompt/completion tokens, cost).
    Summary,
    /// Per-day aggregate of every recorded request.
    Daily,
    /// Pre-bucketed token + cost series for the requested period.
    Chart {
        /// Period to bucket over. One of `24h`, `7d`, `30d`, `60d`.
        #[arg(long, default_value = "7d")]
        period: String,
    },
    /// Recent history with model / provider / cost per row.
    History {
        #[arg(long)]
        limit: Option<u64>,
    },
    /// Dashboard-style aggregate stats (totals + buckets + recent).
    Stats {
        /// Period for the bucket series.
        #[arg(long, default_value = "7d")]
        period: String,
    },
    /// Aggregated usage per provider.
    Providers,
    /// Combined activity log (request rows, paginated).
    Logs {
        /// Maximum rows to fetch (default: server-side).
        #[arg(long)]
        limit: Option<u64>,
        /// Filter by model id.
        #[arg(long)]
        model: Option<String>,
        /// Filter by provider id.
        #[arg(long)]
        provider: Option<String>,
    },
    /// One request's prompt/response + token breakdown.
    RequestLogs {
        /// Request id (matches the `id` field in `usage logs`).
        id: String,
    },
    /// Live usage stream (SSE → NDJSON). Runs until Ctrl+C.
    Stream {
        /// Backward-compatible no-op (the stream always follows).
        #[arg(long)]
        #[allow(dead_code)]
        follow: bool,
    },
    /// Active pricing table for cost calculation.
    Pricing,
}

pub async fn run(cmd: UsageCmd, cfg: &ResolvedConfig, ctx: OutputCtx) -> anyhow::Result<i32> {
    let rt = match require_runtime(cfg).await {
        Ok(rt) => rt,
        Err(e) => return rt_error_to_exit(ctx, e),
    };

    match cmd {
        UsageCmd::Summary => run_summary(&rt, ctx).await,
        UsageCmd::Daily => run_daily(&rt, ctx).await,
        UsageCmd::Chart { period } => run_chart(&rt, ctx, &period).await,
        UsageCmd::History { limit } => run_history(&rt, ctx, limit).await,
        UsageCmd::Stats { period } => run_stats(&rt, ctx, &period).await,
        UsageCmd::Providers => run_providers(&rt, ctx).await,
        UsageCmd::Logs {
            limit,
            model,
            provider,
        } => run_logs(&rt, ctx, limit, model, provider).await,
        UsageCmd::RequestLogs { id } => run_request_logs(&rt, ctx, &id).await,
        UsageCmd::Stream { .. } => run_stream(&rt, ctx).await,
        UsageCmd::Pricing => run_pricing(&rt, ctx).await,
    }
}

async fn run_summary(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.get_json("/api/usage/summary").await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.usage.summary", payload)?;
            } else {
                humanln(
                    ctx,
                    format!(
                        "Total requests: {}",
                        payload
                            .get("total_requests")
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                    ),
                );
                humanln(
                    ctx,
                    format!(
                        "Prompt tokens:  {}",
                        payload
                            .get("total_prompt_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                    ),
                );
                humanln(
                    ctx,
                    format!(
                        "Completion:     {}",
                        payload
                            .get("total_completion_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                    ),
                );
                humanln(
                    ctx,
                    format!(
                        "Total cost:     ${:.4}",
                        payload
                            .get("total_cost")
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0)
                    ),
                );
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_daily(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.get_json("/api/usage/daily").await {
        Ok(payload) => emit_listy(ctx, "openproxy.v1.usage.daily", payload, |entries| {
            humanln(ctx, format!("{} days", entries.len()));
        }),
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_chart(rt: &Runtime, ctx: OutputCtx, period: &str) -> anyhow::Result<i32> {
    let path = format!("/api/usage/chart?period={}", urlencoding::encode(period));
    match rt.get_json(&path).await {
        Ok(payload) => emit_listy(ctx, "openproxy.v1.usage.chart", payload, |entries| {
            humanln(ctx, format!("{} buckets", entries.len()));
        }),
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_history(rt: &Runtime, ctx: OutputCtx, limit: Option<u64>) -> anyhow::Result<i32> {
    let mut path = String::from("/api/usage/history");
    if let Some(l) = limit {
        path.push_str(&format!("?limit={}", l));
    }
    match rt.get_json(&path).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.usage.history", payload)?;
            } else {
                let entries = payload
                    .get("history")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                humanln(ctx, format!("{} history rows", entries.len()));
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_stats(rt: &Runtime, ctx: OutputCtx, period: &str) -> anyhow::Result<i32> {
    let path = format!("/api/usage/stats?period={}", urlencoding::encode(period));
    match rt.get_json(&path).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.usage.stats", payload)?;
            } else {
                let totals = payload.get("totals").cloned().unwrap_or(json!({}));
                humanln(
                    ctx,
                    format!(
                        "Period:  {}",
                        payload
                            .get("period")
                            .and_then(Value::as_str)
                            .unwrap_or(period)
                    ),
                );
                humanln(
                    ctx,
                    format!(
                        "Requests: {}",
                        totals.get("requests").and_then(Value::as_u64).unwrap_or(0)
                    ),
                );
                humanln(
                    ctx,
                    format!(
                        "Tokens:   {}",
                        totals.get("tokens").and_then(Value::as_u64).unwrap_or(0)
                    ),
                );
                humanln(
                    ctx,
                    format!(
                        "Cost:     ${:.4}",
                        totals.get("cost").and_then(Value::as_f64).unwrap_or(0.0)
                    ),
                );
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_providers(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.get_json("/api/usage/providers").await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.usage.providers", payload)?;
            } else {
                let providers = payload
                    .get("providers")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for p in &providers {
                    humanln(
                        ctx,
                        format!(
                            "{}\t{}",
                            p.get("provider").and_then(Value::as_str).unwrap_or("?"),
                            p.get("requests").and_then(Value::as_u64).unwrap_or(0)
                        ),
                    );
                }
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_logs(
    rt: &Runtime,
    ctx: OutputCtx,
    limit: Option<u64>,
    model: Option<String>,
    provider: Option<String>,
) -> anyhow::Result<i32> {
    let mut query = Vec::<(String, String)>::new();
    if let Some(l) = limit {
        query.push(("limit".to_string(), l.to_string()));
    }
    if let Some(m) = model {
        query.push(("model".to_string(), m));
    }
    if let Some(p) = provider {
        query.push(("provider".to_string(), p));
    }
    match rt.get_json_query("/api/usage/logs", &query).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.usage.logs", payload)?;
            } else {
                let rows = payload
                    .get("logs")
                    .or_else(|| payload.get("entries"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                humanln(ctx, format!("{} entries", rows.len()));
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_request_logs(rt: &Runtime, ctx: OutputCtx, id: &str) -> anyhow::Result<i32> {
    let path = format!("/api/usage/request-details?id={}", urlencoding::encode(id));
    match rt.get_json(&path).await {
        Ok(payload) => {
            let rows = payload
                .get("rows")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if rows.is_empty() {
                return Ok(emit_error(
                    ctx,
                    "not_found",
                    &format!("no request log for id '{id}'"),
                )?);
            }
            if ctx.is_robot() {
                emit_robot("openproxy.v1.usage.request_log", payload)?;
            } else {
                let first = &rows[0];
                humanln(
                    ctx,
                    format!(
                        "id={} model={} provider={}",
                        first.get("id").and_then(Value::as_str).unwrap_or(id),
                        first.get("model").and_then(Value::as_str).unwrap_or("?"),
                        first.get("provider").and_then(Value::as_str).unwrap_or("?"),
                    ),
                );
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_stream(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    let mut stream = match rt.stream_sse("/api/usage/stream").await {
        Ok(s) => s,
        Err(e) => return rt_error_to_exit(ctx, e),
    };

    let mut stdout = std::io::stdout().lock();
    while let Some(chunk) = stream.next().await {
        let bytes = match chunk {
            Ok(b) => b,
            Err(e) => return rt_error_to_exit(ctx, e),
        };
        let body: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        let envelope = json!({
            "schema": "openproxy.v1.usage.event",
            "ok": true,
            "data": body,
            "error": null,
            "meta": {},
        });
        writeln!(stdout, "{}", envelope)?;
        stdout.flush()?;
    }
    Ok(0)
}

async fn run_pricing(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.get_json("/api/usage/pricing").await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.usage.pricing", payload)?;
            } else {
                let pricing = payload
                    .get("pricing")
                    .cloned()
                    .unwrap_or_else(|| payload.clone());
                humanln(
                    ctx,
                    serde_json::to_string_pretty(&pricing).unwrap_or_default(),
                );
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

/// Emit a top-level JSON array (or object containing an array) under the
/// given envelope, with a short human one-liner derived by `human`.
fn emit_listy<F>(ctx: OutputCtx, schema: &str, payload: Value, human: F) -> anyhow::Result<i32>
where
    F: FnOnce(&Vec<Value>),
{
    if ctx.is_robot() {
        emit_robot(schema, payload)?;
    } else {
        let arr = match &payload {
            Value::Array(a) => a.clone(),
            Value::Object(o) => o
                .values()
                .find_map(|v| v.as_array().cloned())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        human(&arr);
    }
    Ok(0)
}
