//! Persistence and accounting behaviour of the SQLite usage store.
//!
//! Covers the three facts that must survive a restart and must not be
//! destroyed by a config import:
//! - the legacy JSON migration covers every legacy file, not just db.json/usage.json
//! - the per-day `usageDaily` rollup is written durably and outlives the
//!   `usageHistory` window it is derived from
//! - the lifetime request count lives in `_meta`, not in the history length

use std::sync::LazyLock;

use tempfile::TempDir;

// These tests mutate the process-global DATA_DIR env var; serialize them.
static ENV_MUTEX: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

/// One-time migration picks up `disabledModels.json` and
/// `request-details.json` even when `db.json` / `usage.json` are absent.
#[tokio::test]
async fn e2e_auto_import_legacy_disabled_models_and_request_details() {
    let _env_guard = ENV_MUTEX.lock().await;
    let tmp = TempDir::new().unwrap();
    std::env::set_var("DATA_DIR", tmp.path());

    // No db.json, no usage.json — the old two-file gate would import nothing.
    tokio::fs::write(
        tmp.path().join("disabledModels.json"),
        serde_json::to_vec(&serde_json::json!({
            "disabled": { "openai": ["gpt-4o-mini"] }
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    tokio::fs::write(
        tmp.path().join("request-details.json"),
        serde_json::to_vec(&serde_json::json!({
            "records": [{
                "id": "req-legacy-1",
                "timestamp": "2026-01-01T00:00:00Z",
                "provider": "openai",
                "model": "gpt-4o",
                "connectionId": "c1",
                "status": "ok",
                "request": { "messages": [{ "role": "user", "content": "hi" }] }
            }]
        }))
        .unwrap(),
    )
    .await
    .unwrap();

    let db = openproxy::db::Db::load().await.unwrap();
    let sq = db.sqlite_handle();

    let disabled: i64 = sq
        .with_conn(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM disabledModels WHERE provider = 'openai' AND model = 'gpt-4o-mini'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(disabled, 1, "legacy disabled models must be auto-imported");

    let details: i64 = sq
        .with_conn(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM requestDetails WHERE id = 'req-legacy-1'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(details, 1, "legacy request details must be auto-imported");
}

/// The per-day rollup is written alongside the history row, and a day whose
/// `usageHistory` rows have aged out of the load window is still reported.
#[tokio::test]
async fn e2e_daily_rollup_is_durable_beyond_history_window() {
    let _env_guard = ENV_MUTEX.lock().await;
    let tmp = TempDir::new().unwrap();
    std::env::set_var("DATA_DIR", tmp.path());

    let db = openproxy::db::Db::load().await.unwrap();
    db.update_usage(|usage| {
        for (model, timestamp) in [
            ("gpt-4o", "2026-01-01T00:00:00Z"),
            ("gpt-4o", "2026-01-01T00:01:00Z"),
            ("gpt-4o-mini", "2026-01-02T00:00:00Z"),
        ] {
            usage.history.push(openproxy::types::UsageEntry {
                model: model.into(),
                provider: Some("openai".into()),
                timestamp: Some(timestamp.into()),
                cost: Some(0.01),
                ..Default::default()
            });
        }
    })
    .await
    .unwrap();

    let sq = db.sqlite_handle();
    let day_count: i64 = sq
        .with_conn(|c| c.query_row("SELECT COUNT(*) FROM usageDaily", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(day_count, 2, "each touched day must have a rollup row");

    let jan_first: String = sq
        .with_conn(|c| {
            c.query_row(
                "SELECT data FROM usageDaily WHERE dateKey = '2026-01-01'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    let jan_first: serde_json::Value = serde_json::from_str(&jan_first).unwrap();
    assert_eq!(jan_first["requests"].as_u64(), Some(2));
    assert_eq!(jan_first["cost"].as_f64(), Some(0.02));

    // Push the two January days past any read window by burying them under
    // newer history rows — this is what retention eventually does for real.
    sq.with_transaction(|c| {
        for i in 0..10_050 {
            c.execute(
                "INSERT INTO usageHistory(timestamp, model, provider) VALUES(?1, 'gpt-4o', 'openai')",
                [format!("2027-01-01T00:00:{:02}Z", i % 60)],
            )?;
        }
        Ok(())
    })
    .unwrap();

    let reloaded = openproxy::db::Db::load().await.unwrap();
    let usage = reloaded.usage_snapshot();
    assert_eq!(
        usage.daily_summary.get("2026-01-01").map(|d| d.requests),
        Some(2),
        "a day older than the history window must still be summarized"
    );
    assert!(usage.daily_summary.contains_key("2026-01-02"));
}

/// The lifetime request count is a durable counter, so it keeps reporting the
/// stored total rather than the length of the surviving history.
#[tokio::test]
async fn e2e_lifetime_count_tracks_appends_across_reload() {
    let _env_guard = ENV_MUTEX.lock().await;
    let tmp = TempDir::new().unwrap();
    std::env::set_var("DATA_DIR", tmp.path());

    let db = openproxy::db::Db::load().await.unwrap();
    for i in 0..3 {
        db.update_usage(move |usage| {
            usage.history.push(openproxy::types::UsageEntry {
                model: "gpt-4o".into(),
                provider: Some("openai".into()),
                timestamp: Some(format!("2026-01-0{}T00:00:00Z", i + 1)),
                ..Default::default()
            });
        })
        .await
        .unwrap();
    }

    let reloaded = openproxy::db::Db::load().await.unwrap();
    assert_eq!(reloaded.usage_snapshot().total_requests_lifetime, 3);

    let stored: String = reloaded
        .sqlite_handle()
        .with_conn(|c| {
            c.query_row(
                "SELECT value FROM _meta WHERE key = 'totalRequestsLifetime'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(stored, "3");
}
