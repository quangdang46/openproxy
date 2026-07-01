//! `openproxy doctor` — agent self-test.
//!
//! Runs a fixed set of checks and reports them in `--robot` JSON or a human
//! summary. Designed to be the first command an agent runs to figure out
//! whether the local OpenProxy install is healthy enough to use.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::cli::config::ResolvedConfig;
use crate::cli::output::{emit_robot, humanln, OutputCtx};

#[derive(Debug, Clone)]
struct Check {
    name: &'static str,
    ok: bool,
    detail: String,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            ok: true,
            detail: detail.into(),
        }
    }
    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            ok: false,
            detail: detail.into(),
        }
    }
}

pub async fn run(ctx: OutputCtx, cfg: &ResolvedConfig) -> anyhow::Result<i32> {
    let mut checks: Vec<Check> = Vec::new();

    checks.push(check_data_dir(&cfg.data_dir));
    checks.push(check_db_file(&cfg.data_dir));
    checks.push(check_db_loadable(&cfg.data_dir).await);

    if let Some(url) = cfg.remote_url.as_deref() {
        checks.push(check_server_reachable(url).await);
    } else {
        checks.push(check_local_server_reachable(cfg.port).await);
    }

    let all_ok = checks.iter().all(|c| c.ok);

    if ctx.is_robot() {
        let payload = json!({
            "ok": all_ok,
            "checks": checks
                .iter()
                .map(|c| json!({"name": c.name, "ok": c.ok, "detail": c.detail}))
                .collect::<Vec<_>>(),
        });
        emit_robot("openproxy.v1.doctor", payload)?;
    } else {
        humanln(ctx, "openproxy doctor:");
        for c in &checks {
            let mark = if c.ok { "ok  " } else { "FAIL" };
            humanln(ctx, format!("  [{mark}] {} — {}", c.name, c.detail));
        }
        humanln(
            ctx,
            if all_ok {
                "result: all checks passed"
            } else {
                "result: at least one check failed"
            },
        );
    }

    Ok(if all_ok { 0 } else { 1 })
}

fn check_data_dir(dir: &Path) -> Check {
    if dir.exists() {
        Check::ok("data_dir", format!("{} exists", dir.display()))
    } else {
        Check::fail(
            "data_dir",
            format!(
                "{} does not exist (will be created on first write)",
                dir.display()
            ),
        )
    }
}

fn check_db_file(dir: &Path) -> Check {
    // SQLite is now the sole runtime store.
    let db = dir.join("openproxy.sqlite");
    if db.exists() {
        Check::ok("db_file", format!("{} present", db.display()))
    } else {
        Check::fail(
            "db_file",
            format!(
                "{} not found (run 'openproxy server start' once to initialize)",
                db.display()
            ),
        )
    }
}

async fn check_db_loadable(dir: &Path) -> Check {
    // Use a non-side-effecting probe: open SQLite read-only and export
    // the snapshot, instead of `Db::load()` which would *create* the file
    // (causing a misleading FAIL/ok flip on the very first run — bug #5).
    let db_path = dir.join("openproxy.sqlite");
    if !db_path.exists() {
        return Check::fail(
            "db_loadable",
            format!(
                "{} not present; run 'openproxy server init' to create it",
                db_path.display()
            ),
        );
    }
    let sqlite = match crate::db::sqlite::SqliteDb::open(&db_path) {
        Ok(db) => db,
        Err(e) => return Check::fail("db_loadable", format!("open {}: {e}", db_path.display())),
    };
    let app_db = sqlite.with_conn(|conn| {
        let val = crate::db::sqlite::export::export_all(conn)?;
        Ok(crate::types::AppDb::from_json_value(val))
    });
    match app_db {
        Ok(value) => Check::ok(
            "db_loadable",
            format!(
                "{} providers, {} keys, {} pools, {} combos, {} nodes",
                value.provider_connections.len(),
                value.api_keys.len(),
                value.proxy_pools.len(),
                value.combos.len(),
                value.provider_nodes.len(),
            ),
        ),
        Err(e) => Check::fail("db_loadable", format!("SQLite export error: {e}")),
    }
}

async fn check_local_server_reachable(port: u16) -> Check {
    let url = format!("http://127.0.0.1:{port}/health");
    probe(&url, "server_reachable").await
}

async fn check_server_reachable(base: &str) -> Check {
    let url = format!("{}/health", base.trim_end_matches('/'));
    probe(&url, "server_reachable").await
}

async fn probe(url: &str, name: &'static str) -> Check {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(800))
        .build()
    {
        Ok(c) => c,
        Err(e) => return Check::fail(name, format!("client init failed: {e}")),
    };

    match client.get(url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let body: Result<Value, _> = resp.json().await;
            let detail = body
                .ok()
                .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(str::to_string))
                .unwrap_or_else(|| "ok".to_string());
            Check::ok(name, format!("{url} → {detail}"))
        }
        Ok(resp) => Check::fail(name, format!("{url} → HTTP {}", resp.status())),
        Err(_) => Check::fail(
            name,
            format!(
                "{url} unreachable (server not running? start: openproxy server start --detach)"
            ),
        ),
    }
}
