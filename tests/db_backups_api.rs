use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

const TEST_KEY: &str = "db-backups-api-test-key";

fn active_key() -> ApiKey {
    ApiKey {
        id: "key-1".into(),
        name: "Local".into(),
        key: TEST_KEY.into(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        extra: BTreeMap::new(),
    }
}

async fn app_state_with(temp_path: &std::path::Path) -> AppState {
    let db = Arc::new(Db::load_from(temp_path).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key()];
        state
            .settings
            .extra
            .insert("password".into(), Value::String("hashed-secret".into()));
        state.settings.require_login = true;
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

async fn drain_body(body: Body) -> Value {
    let bytes = to_bytes(body, usize::MAX).await.expect("body");
    serde_json::from_slice(&bytes).expect("json")
}

#[tokio::test]
async fn list_requires_auth() {
    let temp = tempdir().unwrap();
    let app = openproxy::build_app(app_state_with(temp.path()).await);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/db-backups")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn put_then_list_then_delete_round_trip() {
    let temp = tempdir().unwrap();
    let app = openproxy::build_app(app_state_with(temp.path()).await);

    // PUT: create manual backup
    let put = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/db-backups")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let put_body = drain_body(put.into_body()).await;
    assert_eq!(put_body["created"], Value::Bool(true));
    let backup_id = put_body["backup"]["id"]
        .as_str()
        .expect("backup id present")
        .to_string();
    assert!(backup_id.starts_with("db_"));
    assert!(backup_id.ends_with("_manual.json"));

    // GET: list
    let list = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/db-backups")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list_body = drain_body(list.into_body()).await;
    let backups = list_body["backups"].as_array().expect("backups array");
    assert_eq!(backups.len(), 1);
    assert_eq!(backups[0]["id"], backup_id);
    assert_eq!(backups[0]["reason"], "manual");
    assert_eq!(backups[0]["apiKeyCount"], 1);

    // DELETE one
    let del = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/db-backups/{backup_id}"))
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::OK);

    // GET: empty
    let list2 = app
        .oneshot(
            Request::builder()
                .uri("/api/db-backups")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list2_body = drain_body(list2.into_body()).await;
    assert!(list2_body["backups"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn restore_takes_pre_restore_snapshot_and_swaps_db() {
    let temp = tempdir().unwrap();
    let app = openproxy::build_app(app_state_with(temp.path()).await);

    // Create first snapshot with 1 api key.
    let snap = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/db-backups")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let snap_body = drain_body(snap.into_body()).await;
    let backup_id = snap_body["backup"]["id"].as_str().unwrap().to_string();

    // Mutate db.json on disk: rewrite the same file directly via fs so the
    // in-memory snapshot drifts. Simpler: hit our own settings PATCH? We
    // instead just clear api_keys via direct write — the restore handler
    // will rebuild AppDb from the saved backup file.
    let db_path = temp.path().join("db.json");
    let cleared = json!({
        "providerConnections": [],
        "providerNodes": [],
        "combos": [],
        "apiKeys": [],
        "settings": {},
    });
    tokio::fs::write(&db_path, serde_json::to_vec_pretty(&cleared).unwrap())
        .await
        .unwrap();
    // We do not need to reload the in-memory snapshot for this assertion —
    // we're checking that restore writes the backup back to disk and updates
    // the in-memory snapshot.

    // Tiny delay so the pre-restore snapshot lands in a distinct timestamp.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    // POST /restore
    let restore = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/db-backups/restore")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "backupId": backup_id }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(restore.status(), StatusCode::OK);
    let restore_body = drain_body(restore.into_body()).await;
    assert_eq!(restore_body["restored"], Value::Bool(true));
    assert_eq!(restore_body["apiKeyCount"], 1);

    // A pre-restore snapshot should now exist alongside the manual one.
    let list = app
        .oneshot(
            Request::builder()
                .uri("/api/db-backups")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = drain_body(list.into_body()).await;
    let backups = body["backups"].as_array().unwrap();
    assert!(
        backups.iter().any(|b| b["reason"] == "pre-restore"),
        "expected a pre-restore snapshot to be created: {body:?}"
    );
}

#[tokio::test]
async fn export_returns_attachment() {
    let temp = tempdir().unwrap();
    let app = openproxy::build_app(app_state_with(temp.path()).await);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/db-backups/export")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let disposition = res
        .headers()
        .get(axum::http::header::CONTENT_DISPOSITION)
        .expect("content-disposition")
        .to_str()
        .unwrap()
        .to_string();
    assert!(disposition.contains("openproxy-backup-"));
    assert!(disposition.contains(".json"));
    let body = drain_body(res.into_body()).await;
    assert_eq!(body["apiKeys"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn restore_rejects_path_traversal() {
    let temp = tempdir().unwrap();
    let app = openproxy::build_app(app_state_with(temp.path()).await);
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/db-backups/restore")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "backupId": "../db.json" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
