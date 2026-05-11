//! `openproxy pool *` — extended proxy pool commands (M3).
//!
//! Complements the existing `pool list/status/create/delete` with
//! get/edit/enable/disable/test/stats/apply.

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use clap::Subcommand;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::cli::apply::{into_items, load_document, ApplyDiff};
use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};
use crate::db::Db;
use crate::types::ProxyPool;

#[derive(Debug, Clone, Subcommand)]
pub enum PoolExtCmd {
    /// Show one pool by name.
    Get { name: String },
    /// Edit a pool's properties (any flag omitted = unchanged).
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
    /// Probe an HTTP endpoint through the pool to confirm it works.
    Test {
        name: String,
        /// URL to probe. Defaults to https://httpbin.org/get.
        #[arg(long, default_value = "https://httpbin.org/get")]
        target: String,
    },
    /// Show recorded counters (success_rate, rtt_ms, totals).
    Stats { name: String },
    /// Idempotent upsert from a YAML/JSON file or stdin.
    Apply {
        #[arg(long = "from-file", default_value = "-")]
        from_file: String,
        #[arg(long)]
        prune: bool,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PoolInput {
    name: String,
    proxy_url: String,
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    is_active: Option<bool>,
    #[serde(default)]
    strict_proxy: Option<bool>,
    #[serde(default)]
    no_proxy: Option<String>,
}

pub async fn run(cmd: PoolExtCmd, db: &Db, ctx: OutputCtx) -> anyhow::Result<()> {
    match cmd {
        PoolExtCmd::Get { name } => run_get(db, ctx, &name).await,
        PoolExtCmd::Edit {
            name,
            proxy_url,
            r#type,
            strict,
        } => run_edit(db, ctx, &name, proxy_url, r#type, strict).await,
        PoolExtCmd::Enable { name } => run_set_active(db, ctx, &name, true).await,
        PoolExtCmd::Disable { name } => run_set_active(db, ctx, &name, false).await,
        PoolExtCmd::Test { name, target } => run_test(db, ctx, &name, &target).await,
        PoolExtCmd::Stats { name } => run_stats(db, ctx, &name).await,
        PoolExtCmd::Apply { from_file, prune } => run_apply(db, ctx, &from_file, prune).await,
    }
}

fn find_pool(db: &Db, name: &str) -> Option<ProxyPool> {
    db.snapshot()
        .proxy_pools
        .iter()
        .find(|p| p.name == name || p.id == name)
        .cloned()
}

async fn run_get(db: &Db, ctx: OutputCtx, name: &str) -> anyhow::Result<()> {
    let Some(pool) = find_pool(db, name) else {
        let exit = emit_error(ctx, "not_found", &format!("pool '{name}' not found"))?;
        std::process::exit(exit);
    };
    if ctx.is_robot() {
        emit_robot("openproxy.v1.pool.get", serde_json::to_value(&pool)?)?;
    } else {
        humanln(ctx, format!("Pool: {} ({})", pool.name, pool.id));
        humanln(ctx, format!("  proxyUrl: {}", pool.proxy_url));
        humanln(ctx, format!("  type:     {}", pool.r#type));
        humanln(
            ctx,
            format!("  active:   {}", pool.is_active.unwrap_or(true)),
        );
    }
    Ok(())
}

async fn run_edit(
    db: &Db,
    ctx: OutputCtx,
    name: &str,
    proxy_url: Option<String>,
    pool_type: Option<String>,
    strict: Option<bool>,
) -> anyhow::Result<()> {
    if find_pool(db, name).is_none() {
        let exit = emit_error(ctx, "not_found", &format!("pool '{name}' not found"))?;
        std::process::exit(exit);
    }
    let mut updated: Option<ProxyPool> = None;
    db.update(|app| {
        if let Some(pool) = app
            .proxy_pools
            .iter_mut()
            .find(|p| p.name == name || p.id == name)
        {
            if let Some(url) = &proxy_url {
                pool.proxy_url = url.clone();
            }
            if let Some(kind) = &pool_type {
                pool.r#type = kind.clone();
            }
            if let Some(s) = strict {
                pool.strict_proxy = Some(s);
            }
            pool.updated_at = Some(chrono::Utc::now().to_rfc3339());
            updated = Some(pool.clone());
        }
    })
    .await?;
    let pool = updated.expect("pool existed");
    if ctx.is_robot() {
        emit_robot("openproxy.v1.pool.edit", serde_json::to_value(&pool)?)?;
    } else {
        humanln(ctx, format!("updated pool '{}'", pool.name));
    }
    Ok(())
}

async fn run_set_active(db: &Db, ctx: OutputCtx, name: &str, active: bool) -> anyhow::Result<()> {
    if find_pool(db, name).is_none() {
        let exit = emit_error(ctx, "not_found", &format!("pool '{name}' not found"))?;
        std::process::exit(exit);
    }
    db.update(|app| {
        if let Some(pool) = app
            .proxy_pools
            .iter_mut()
            .find(|p| p.name == name || p.id == name)
        {
            pool.is_active = Some(active);
            pool.updated_at = Some(chrono::Utc::now().to_rfc3339());
        }
    })
    .await?;
    let schema = if active {
        "openproxy.v1.pool.enable"
    } else {
        "openproxy.v1.pool.disable"
    };
    if ctx.is_robot() {
        emit_robot(schema, json!({ "name": name, "active": active }))?;
    } else {
        humanln(
            ctx,
            format!(
                "{} pool '{name}'",
                if active { "enabled" } else { "disabled" }
            ),
        );
    }
    Ok(())
}

async fn run_test(db: &Db, ctx: OutputCtx, name: &str, target: &str) -> anyhow::Result<()> {
    let Some(pool) = find_pool(db, name) else {
        let exit = emit_error(ctx, "not_found", &format!("pool '{name}' not found"))?;
        std::process::exit(exit);
    };

    let proxy = match reqwest::Proxy::all(&pool.proxy_url) {
        Ok(p) => p,
        Err(e) => {
            let exit = emit_error(
                ctx,
                "validation",
                &format!("invalid proxy_url '{}': {e}", pool.proxy_url),
            )?;
            std::process::exit(exit);
        }
    };

    let client = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(10))
        .build()?;
    let start = Instant::now();
    let result = client.get(target).send().await;
    let rtt_ms = start.elapsed().as_millis() as u64;

    let (valid, status, error) = match result {
        Ok(resp) => (
            resp.status().is_success(),
            Some(resp.status().as_u16()),
            None,
        ),
        Err(e) => (false, None, Some(e.to_string())),
    };

    let last_tested_at = chrono::Utc::now().to_rfc3339();
    db.update(|app| {
        if let Some(p) = app
            .proxy_pools
            .iter_mut()
            .find(|p| p.name == name || p.id == name)
        {
            p.test_status = Some(if valid {
                "ok".to_string()
            } else {
                "failed".to_string()
            });
            p.last_tested_at = Some(last_tested_at.clone());
            p.last_error = error.clone();
            p.rtt_ms = Some(rtt_ms);
        }
    })
    .await?;

    let payload = json!({
        "name": pool.name,
        "target": target,
        "valid": valid,
        "status": status,
        "rttMs": rtt_ms,
        "error": error,
    });
    if ctx.is_robot() {
        emit_robot("openproxy.v1.pool.test", payload)?;
    } else if valid {
        humanln(ctx, format!("OK   {name} ({rtt_ms}ms) — {target}"));
    } else {
        humanln(
            ctx,
            format!(
                "FAIL {name} ({rtt_ms}ms) — {} — {target}",
                error.as_deref().unwrap_or("?")
            ),
        );
    }
    Ok(())
}

async fn run_stats(db: &Db, ctx: OutputCtx, name: &str) -> anyhow::Result<()> {
    let Some(pool) = find_pool(db, name) else {
        let exit = emit_error(ctx, "not_found", &format!("pool '{name}' not found"))?;
        std::process::exit(exit);
    };
    let payload = json!({
        "name": pool.name,
        "id": pool.id,
        "successRate": pool.success_rate,
        "rttMs": pool.rtt_ms,
        "totalRequests": pool.total_requests,
        "failedRequests": pool.failed_requests,
        "testStatus": pool.test_status,
        "lastTestedAt": pool.last_tested_at,
        "lastError": pool.last_error,
    });
    if ctx.is_robot() {
        emit_robot("openproxy.v1.pool.stats", payload)?;
    } else {
        humanln(ctx, format!("Pool stats: {}", pool.name));
        humanln(
            ctx,
            format!(
                "  successRate:   {}",
                pool.success_rate
                    .map(|r| format!("{:.2}%", r * 100.0))
                    .unwrap_or_else(|| "-".into())
            ),
        );
        humanln(
            ctx,
            format!(
                "  rttMs:         {}",
                pool.rtt_ms
                    .map(|r| r.to_string())
                    .unwrap_or_else(|| "-".into())
            ),
        );
        humanln(
            ctx,
            format!("  totalRequests: {}", pool.total_requests.unwrap_or(0)),
        );
        humanln(
            ctx,
            format!("  failedRequests:{}", pool.failed_requests.unwrap_or(0)),
        );
    }
    Ok(())
}

async fn run_apply(db: &Db, ctx: OutputCtx, from_file: &str, prune: bool) -> anyhow::Result<()> {
    let (doc, _) = match load_document(from_file) {
        Ok(d) => d,
        Err(e) => {
            let exit = emit_error(ctx, "validation", &e.to_string())?;
            std::process::exit(exit);
        }
    };
    let items: Vec<PoolInput> = match into_items(doc) {
        Ok(items) => items,
        Err(e) => {
            let exit = emit_error(ctx, "validation", &e.to_string())?;
            std::process::exit(exit);
        }
    };

    let names_in_doc: HashSet<String> = items.iter().map(|i| i.name.clone()).collect();
    let mut diff = ApplyDiff::default();
    let now = chrono::Utc::now().to_rfc3339();

    db.update(|app| {
        for item in &items {
            if let Some(existing) = app.proxy_pools.iter_mut().find(|p| p.name == item.name) {
                let mut changed = false;
                if existing.proxy_url != item.proxy_url {
                    existing.proxy_url = item.proxy_url.clone();
                    changed = true;
                }
                if let Some(kind) = item.r#type.as_deref() {
                    if existing.r#type != kind {
                        existing.r#type = kind.to_string();
                        changed = true;
                    }
                }
                if let Some(active) = item.is_active {
                    if existing.is_active.unwrap_or(true) != active {
                        existing.is_active = Some(active);
                        changed = true;
                    }
                }
                if let Some(strict) = item.strict_proxy {
                    if existing.strict_proxy != Some(strict) {
                        existing.strict_proxy = Some(strict);
                        changed = true;
                    }
                }
                if let Some(no_proxy) = item.no_proxy.as_deref() {
                    if existing.no_proxy != no_proxy {
                        existing.no_proxy = no_proxy.to_string();
                        changed = true;
                    }
                }
                if changed {
                    existing.updated_at = Some(now.clone());
                    diff.updated.push(item.name.clone());
                } else {
                    diff.unchanged.push(item.name.clone());
                }
            } else {
                app.proxy_pools.push(ProxyPool {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: item.name.clone(),
                    proxy_url: item.proxy_url.clone(),
                    no_proxy: item.no_proxy.clone().unwrap_or_default(),
                    r#type: item.r#type.clone().unwrap_or_else(|| "http".to_string()),
                    is_active: Some(item.is_active.unwrap_or(true)),
                    strict_proxy: item.strict_proxy,
                    test_status: None,
                    last_tested_at: None,
                    last_error: None,
                    success_rate: None,
                    rtt_ms: None,
                    total_requests: None,
                    failed_requests: None,
                    created_at: Some(now.clone()),
                    updated_at: Some(now.clone()),
                    extra: BTreeMap::new(),
                });
                diff.created.push(item.name.clone());
            }
        }
        if prune {
            let to_delete: Vec<String> = app
                .proxy_pools
                .iter()
                .filter(|p| !names_in_doc.contains(&p.name))
                .map(|p| p.name.clone())
                .collect();
            for name in &to_delete {
                diff.deleted.push(name.clone());
            }
            app.proxy_pools.retain(|p| names_in_doc.contains(&p.name));
        }
    })
    .await?;

    let summary = diff.summary();
    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.pool.apply",
            json!({ "diff": diff, "summary": summary, "prune": prune }),
        )?;
    } else {
        humanln(ctx, format!("pool apply: {summary}"));
    }
    let _ = Value::Null;
    Ok(())
}
