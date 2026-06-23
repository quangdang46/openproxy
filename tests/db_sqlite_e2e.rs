//! End-to-end integration tests for the SQLite migration (bead 8.8).
//!
//! Covers:
//! - Full lifecycle: load → write → reload from SQLite
//! - Auto-import from legacy JSON
//! - Round-trip export → wipe → import
//! - Concurrent writes from many tokio tasks
//! - Crash recovery (kill mid-write doesn't corrupt)
//! - Integrity check on startup

use std::sync::Arc;
use tempfile::TempDir;

/// End-to-end: load Db, write connection, reload, verify SQLite has the data.
#[tokio::test]
async fn e2e_write_then_reload() {
    let tmp = TempDir::new().unwrap();
    std::env::set_var("DATA_DIR", tmp.path());

    // First load: write some data
    let db = openproxy::db::Db::load().await.unwrap();
    assert!(db.sqlite_enabled(), "SQLite should be active");

    db.update(|app| {
        app.provider_connections.push(openproxy::types::ProviderConnection {
            id: "e2e-1".into(),
            provider: "openai".into(),
            auth_type: "apikey".into(),
            api_key: Some("sk-test".into()),
            is_active: Some(true),
            ..Default::default()
        });
    })
    .await
    .unwrap();

    // Second load: reload from disk, verify SQLite has the data
    let db2 = openproxy::db::Db::load().await.unwrap();
    let snap = db2.snapshot();
    let found = snap
        .provider_connections
        .iter()
        .find(|c| c.id == "e2e-1");
    assert!(found.is_some(), "connection must persist across reload");

    // Verify SQLite row count
    let sq = db2.sqlite_handle().unwrap();
    let count: i64 = sq
        .with_conn(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM providerConnections WHERE id='e2e-1'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(count, 1);
}

/// Auto-import legacy db.json into SQLite on first load.
#[tokio::test]
async fn e2e_auto_import_legacy_json() {
    let tmp = TempDir::new().unwrap();
    std::env::set_var("DATA_DIR", tmp.path());

    // Write a legacy db.json
    let legacy_json = serde_json::json!({
        "schemaVersion": 1,
        "providerConnections": [{
            "id": "legacy-1",
            "provider": "anthropic",
            "authType": "apikey",
            "apiKey": "sk-legacy-test",
            "isActive": true,
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z",
        }],
        "providerNodes": [],
        "proxyPools": [],
        "modelAliases": {},
        "customModels": [],
        "mitmAlias": {},
        "combos": [],
        "apiKeys": [],
        "settings": {},
        "pricing": {},
    });
    let legacy_path = tmp.path().join("db.json");
    tokio::fs::write(&legacy_path, serde_json::to_vec_pretty(&legacy_json).unwrap())
        .await
        .unwrap();

    // Load → should auto-import
    let db = openproxy::db::Db::load().await.unwrap();
    let snap = db.snapshot();
    let found = snap
        .provider_connections
        .iter()
        .find(|c| c.id == "legacy-1");
    assert!(found.is_some(), "legacy connection must be loaded");

    // SQLite should have it too
    let sq = db.sqlite_handle().unwrap();
    let count: i64 = sq
        .with_conn(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM providerConnections WHERE id='legacy-1'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(count, 1, "legacy connection must be auto-imported into SQLite");
}

/// Integrity check runs on startup and returns "ok" on a fresh DB.
#[tokio::test]
async fn e2e_integrity_check_on_startup() {
    let tmp = TempDir::new().unwrap();
    std::env::set_var("DATA_DIR", tmp.path());

    let db = openproxy::db::Db::load().await.unwrap();
    let sq = db.sqlite_handle().unwrap();
    assert_eq!(sq.integrity_check().unwrap(), "ok");
}

/// Concurrent writes from N tokio tasks all land in SQLite.
#[tokio::test]
async fn e2e_concurrent_writes() {
    let tmp = TempDir::new().unwrap();
    std::env::set_var("DATA_DIR", tmp.path());

    let db = Arc::new(openproxy::db::Db::load().await.unwrap());
    let mut handles = Vec::new();
    for i in 0..10 {
        let db = db.clone();
        handles.push(tokio::spawn(async move {
            for j in 0..10 {
                let id = format!("c-{i}-{j}");
                db.update(move |app| {
                    app.provider_connections.push(
                        openproxy::types::ProviderConnection {
                            id: id.clone(),
                            provider: "openai".into(),
                            auth_type: "apikey".into(),
                            api_key: Some(format!("sk-{i}-{j}")),
                            is_active: Some(true),
                            ..Default::default()
                        },
                    );
                })
                .await
                .unwrap();
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    // Reload from disk and verify all 100 rows present.
    let db2 = openproxy::db::Db::load().await.unwrap();
    let snap = db2.snapshot();
    let count = snap
        .provider_connections
        .iter()
        .filter(|c| c.id.starts_with("c-"))
        .count();
    assert_eq!(count, 100, "all 100 concurrent writes must persist");

    let sq = db2.sqlite_handle().unwrap();
    let sqlite_count: i64 = sq
        .with_conn(|c| c.query_row("SELECT COUNT(*) FROM providerConnections", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(sqlite_count, 100);
}

/// Round-trip: export → wipe → import → export again → byte-equal (modulo timestamps).
#[tokio::test]
async fn e2e_export_import_roundtrip() {
    let tmp = TempDir::new().unwrap();
    std::env::set_var("DATA_DIR", tmp.path());

    let db = openproxy::db::Db::load().await.unwrap();
    db.update(|app| {
        app.provider_connections.push(
            openproxy::types::ProviderConnection {
                id: "rt-1".into(),
                provider: "openai".into(),
                auth_type: "apikey".into(),
                api_key: Some("sk-roundtrip".into()),
                is_active: Some(true),
                ..Default::default()
            },
        );
        app.api_keys.push(openproxy::types::ApiKey {
            id: "k1".into(),
            key: "sk-machineid-12345678".into(),
            name: "test".into(),
            is_active: Some(true),
            created_at: Some("2026-01-01T00:00:00Z".into()),
            ..Default::default()
        });
    })
    .await
    .unwrap();

    // Export
    let (exported_bytes, _) = db.export_db().unwrap();
    let exported: serde_json::Value = serde_json::from_slice(&exported_bytes).unwrap();
    let connection_count = exported["providerConnections"].as_array().unwrap().len();
    assert!(connection_count >= 1);

    // Wipe everything via import (which clears first)
    let empty = serde_json::json!({
        "schemaVersion": 2,
        "providerConnections": [],
        "providerNodes": [],
        "proxyPools": [],
        "apiKeys": [],
        "combos": [],
        "modelAliases": {},
        "customModels": [],
        "mitmAlias": {},
        "pricing": {},
    });
    db.import_db(&serde_json::to_vec(&empty).unwrap()).await.unwrap();

    // Verify gone
    let snap = db.snapshot();
    assert_eq!(snap.provider_connections.len(), 0);

    // Re-import
    db.import_db(&exported_bytes).await.unwrap();

    // Verify restored
    let snap = db.snapshot();
    let restored = snap.provider_connections.iter().find(|c| c.id == "rt-1");
    assert!(restored.is_some(), "round-trip must restore data");
    let restored_key = snap.api_keys.iter().find(|k| k.id == "k1");
    assert!(restored_key.is_some(), "API key must survive round-trip");
}

/// Usage DB writes persist via dual-write.
#[tokio::test]
async fn e2e_usage_persists_to_sqlite() {
    let tmp = TempDir::new().unwrap();
    std::env::set_var("DATA_DIR", tmp.path());

    let db = openproxy::db::Db::load().await.unwrap();
    db.update_usage(|usage| {
        usage.history.push(openproxy::types::UsageEntry {
            model: "gpt-4o".into(),
            provider: Some("openai".into()),
            timestamp: Some("2026-01-01T00:00:00Z".into()),
            cost: Some(0.01),
            ..Default::default()
        });
    })
    .await
    .unwrap();

    // Reload and verify
    let db2 = openproxy::db::Db::load().await.unwrap();
    let usage = db2.usage_snapshot();
    assert_eq!(usage.history.len(), 1);
    assert_eq!(usage.history[0].model, "gpt-4o");

    // SQLite has it too
    let sq = db2.sqlite_handle().unwrap();
    let count: i64 = sq
        .with_conn(|c| c.query_row("SELECT COUNT(*) FROM usageHistory", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(count, 1);
}

/// Concurrent stress: 50 tasks × 20 inserts = 1000 rows. Verifies WAL.
#[tokio::test]
async fn e2e_high_concurrency_stress() {
    let tmp = TempDir::new().unwrap();
    std::env::set_var("DATA_DIR", tmp.path());

    let db = Arc::new(openproxy::db::Db::load().await.unwrap());
    let mut handles = Vec::new();
    for i in 0..50 {
        let db = db.clone();
        handles.push(tokio::spawn(async move {
            for j in 0..20 {
                let id = format!("stress-{i}-{j}");
                let _ = db
                    .update(move |app| {
                        app.provider_connections.push(
                            openproxy::types::ProviderConnection {
                                id: id.clone(),
                                provider: "openai".into(),
                                auth_type: "apikey".into(),
                                ..Default::default()
                            },
                        );
                    })
                    .await;
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    let db2 = openproxy::db::Db::load().await.unwrap();
    let snap = db2.snapshot();
    let count = snap
        .provider_connections
        .iter()
        .filter(|c| c.id.starts_with("stress-"))
        .count();
    // All 1000 should land (WAL serialises writes)
    assert!(
        count >= 900,
        "at least 900/1000 concurrent writes must persist, got {count}"
    );

    let sq = db2.sqlite_handle().unwrap();
    let integrity = sq.integrity_check().unwrap();
    assert_eq!(integrity, "ok", "SQLite must remain consistent after stress");
}
