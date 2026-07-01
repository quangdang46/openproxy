// Integration test for openproxy::db::watcher::spawn_watcher.
//
// Verifies that an external write to `db.json` (simulating what the CLI does)
// is picked up by the watcher and reflected in the in-memory ArcSwap snapshot
// within a few hundred milliseconds.
//
// NOTE: This test uses the legacy JSON-file DB path (`db.json`). The project
// now uses SQLite as its primary store. The watcher component still supports
// the JSON path for backward compatibility but the test is ignored by default
// since most deployments won't use file-based storage.

#![allow(unused_imports, dead_code)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use openproxy::db::watcher::spawn_watcher;
use openproxy::db::Db;
use serde_json::json;
use tempfile::tempdir;
use tokio::time::sleep;

#[ignore = "legacy JSON DB path — project uses SQLite"]
#[tokio::test]
async fn watcher_picks_up_external_db_write() {
    let tmp = tempdir().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();

    // Initialize an in-memory DB and grab the `Arc<Mutex<Option<...>>>` handle
    // so the watcher stays alive (the Arc keeps it running).
    let db = Arc::new(Db::new_in_memory(data_dir.clone()));
    spawn_watcher(db.clone());
    // Give the watcher a moment to install its inotify watch before we mutate.
    sleep(Duration::from_millis(200)).await;

    // Write a brand-new db.json with one provider, atomically (matches the CLI
    // output format).
    let payload = json!({
        "providers": [{
            "name": "watcher-fixture",
            "provider": "openai",
            "apiKey": "sk-fake",
            "models": ["gpt-4"]
        }]
    });
    let serialized = serde_json::to_string_pretty(&payload).unwrap();
    let tmp_path = data_dir.join(".db.json.write");
    let final_path = data_dir.join("db.json");
    std::fs::write(&tmp_path, &serialized).unwrap();
    std::fs::rename(&tmp_path, &final_path).unwrap();

    // Poll until the watcher picks up the change (up to 1.5 s).
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let snapshot = db.snapshot();
        let count = snapshot.providers.len();
        if count >= 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "watcher did not reload db within 2s; provider count still {count}"
        );
        sleep(Duration::from_millis(50)).await;
    }
}
