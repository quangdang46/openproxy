//! 9router headroom start/stop lifecycle parity.
//!
//! 9router `src/app/api/headroom/stop/route.js:9-10` maps `stopped === false`
//! to 409, and `start/route.js:30-33` returns `{ success: true, pid,
//! alreadyRunning }` with NOT_INSTALLED mapped to 400. OpenProxy answered 200
//! for a stop that never happened, 412 for a missing CLI, and tore down a
//! healthy proxy on a second Start.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use serde_json::Value;
use tempfile::tempdir;
use tower::util::ServiceExt;

const TEST_KEY: &str = "headroom-lifecycle-test-key";

/// The TempDir comes back with the app: dropping it would delete the SQLite
/// file out from under the already-open handle.
async fn app_with(headroom_url: &str) -> (tempfile::TempDir, axum::Router) {
    let temp = tempdir().unwrap();
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![ApiKey {
            id: "key-1".into(),
            name: "Local".into(),
            key: TEST_KEY.into(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            extra: BTreeMap::new(),
            monthly_budget_usd: None,
        }];
        state.settings.require_login = true;
        state.settings.headroom_url = headroom_url.into();
    })
    .await
    .expect("seed db");
    (temp, openproxy::build_app(AppState::new(db)))
}

async fn post(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), usize::MAX).await.expect("body");
    (status, serde_json::from_slice(&bytes).expect("json"))
}

fn headroom_installed() -> bool {
    std::process::Command::new("which")
        .arg("headroom")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// 9router process.js:114 — with no managed pid, `stopHeadroomProxy` returns
/// `{ stopped: false, reason: "not_running" }` and stop/route.js:9 turns that
/// into a 409. OpenProxy answered 200 with `stopped: true`.
///
/// The URL is left empty so the handler's loopback fallback does not run
/// `pkill -f headroom` against whatever the host happens to be running.
#[tokio::test]
async fn stop_with_no_managed_pid_returns_409_not_running() {
    let (_temp, app) = app_with("").await;
    let (status, body) = post(&app, "/api/headroom/stop").await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["stopped"].as_bool(), Some(false));
    assert_eq!(body["reason"], "not_running");
}

/// 9router start/route.js:22 — a non-loopback proxy URL is the caller's problem.
#[tokio::test]
async fn start_with_an_external_url_returns_400_external_proxy() {
    let (_temp, app) = app_with("https://headroom.example.com").await;
    let (status, body) = post(&app, "/api/headroom/start").await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "EXTERNAL_PROXY");
}

/// 9router start/route.js:32 — `error.code === "NOT_INSTALLED" ? 400 : 500`.
/// OpenProxy answered 412 with no `code`. Skips where headroom-ai is installed.
#[tokio::test]
async fn start_without_headroom_binary_returns_400_not_installed() {
    if headroom_installed() {
        return;
    }

    let (_temp, app) = app_with("http://localhost:8787").await;
    let (status, body) = post(&app, "/api/headroom/start").await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "NOT_INSTALLED");
}
