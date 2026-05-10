//! Integration test for `openproxy::db::watcher::spawn_watcher`.
//!
//! Verifies that an external write to `db.json` (simulating what the CLI does)
//! is picked up by the watcher and reflected in the in-memory ArcSwap snapshot
//! within a few hundred milliseconds.

use std::sync::Arc;
use std::time::{Duration, Instant};

use openproxy::db::watcher::spawn_watcher;
use openproxy::db::Db;
use serde_json::json;
use tempfile::tempdir;
use tokio::time::sleep;

#[tokio::test]
async fn watcher_picks_up_external_db_write() {
    let tmp = tempdir().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();

    let db = Db::load_from(&data_dir).await.expect("db load");
    let db = Arc::new(db);

    // Initial snapshot must be empty.
    assert_eq!(db.snapshot().provider_connections.len(), 0);

    spawn_watcher(db.clone());
    // Give the watcher a moment to install its inotify watch before we mutate.
    sleep(Duration::from_millis(120)).await;

    // Write a brand-new db.json with one provider, atomically (matches the CLI
    // path: temp file + rename).
    let new_db = json!({
        "providerConnections": [
            {
                "id": "watch-1",
                "provider": "openai",
                "name": "watcher-fixture",
                "authType": "apikey",
                "apiKey": "sk-test",
                "isActive": true
            }
        ],
        "providerNodes": [],
        "apiKeys": [],
        "proxyPools": [],
        "combos": [],
        "modelAliases": {},
        "modelAvailability": {},
        "settings": {}
    });
    let bytes = serde_json::to_vec_pretty(&new_db).unwrap();
    let tmp_path = data_dir.join(".db.json.write");
    let final_path = data_dir.join("db.json");
    tokio::fs::write(&tmp_path, &bytes).await.unwrap();
    tokio::fs::rename(&tmp_path, &final_path).await.unwrap();

    // Poll the snapshot for up to ~1.5s; succeed on first observed update.
    let deadline = Instant::now() + Duration::from_millis(1500);
    loop {
        if db.snapshot().provider_connections.len() == 1 {
            break;
        }
        if Instant::now() > deadline {
            panic!(
                "watcher did not reload db within 1.5s; provider count still {}",
                db.snapshot().provider_connections.len()
            );
        }
        sleep(Duration::from_millis(50)).await;
    }

    let snap = db.snapshot();
    assert_eq!(snap.provider_connections.len(), 1);
    assert_eq!(snap.provider_connections[0].id, "watch-1");
    assert_eq!(snap.provider_connections[0].provider, "openai");
}
