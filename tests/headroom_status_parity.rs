//! 9router headroom status parity — `src/lib/headroom/detect.js:145-162`.
//!
//! `installed` there means `Boolean(findHeadroomBinary())`, and `canStart` is
//! `installed && localUrl` — deliberately independent of `running`, which is
//! what makes Start idempotent. OpenProxy derived `installed` from
//! `running || localUrl` (always true on the default URL) and inverted
//! `canStart` into `localUrl && !running`, so the Start control vanished the
//! moment the proxy came up.

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
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEST_KEY: &str = "headroom-status-test-key";

/// The TempDir comes back with the body: dropping it would delete the SQLite
/// file out from under the already-open handle.
async fn status_for(headroom_url: &str) -> (tempfile::TempDir, Value) {
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

    let app = openproxy::build_app(AppState::new(db));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/headroom/status")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.expect("body");
    (temp, serde_json::from_slice(&bytes).expect("json"))
}

fn headroom_installed() -> bool {
    std::process::Command::new("which")
        .arg("headroom")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// detect.js:157 — `canStart: installed && localUrl`. The old `localUrl &&
/// !running` rule reported `false` here while `installed` was `true`, which is
/// exactly the "Start disappears once the proxy is up" symptom.
#[tokio::test]
async fn can_start_tracks_installed_while_the_proxy_is_running() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let (_temp, body) = status_for(&server.uri()).await;

    assert_eq!(
        body["running"].as_bool(),
        Some(true),
        "wiremock should answer /health"
    );
    assert_eq!(body["localUrl"].as_bool(), Some(true));
    assert_eq!(body["canStart"].as_bool(), body["installed"].as_bool());
}

/// detect.js:147-156 — every field is derived from a real probe, so the shape
/// is the same whether or not headroom-ai is on this machine.
#[tokio::test]
async fn status_reports_path_version_and_extras() {
    let (_temp, body) = status_for("http://localhost:8787").await;

    for key in [
        "installed",
        "path",
        "running",
        "python",
        "localUrl",
        "canStart",
        "version",
        "extras",
        "managedPid",
    ] {
        assert!(body.get(key).is_some(), "status is missing `{key}`");
    }

    let extras = body["extras"].as_object().expect("extras object");
    let mut keys: Vec<&str> = extras.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["code", "ml"]);

    // `path` is the binary probe itself: present exactly when `installed`.
    let installed = body["installed"].as_bool().expect("installed bool");
    assert_eq!(body["path"].is_null(), !installed);
}

/// detect.js:151 — with no CLI on disk the extras probe never runs, so version
/// stays null. Skips where headroom-ai is installed.
#[tokio::test]
async fn status_reports_not_installed_without_a_headroom_binary() {
    if headroom_installed() {
        return;
    }

    let (_temp, body) = status_for("http://localhost:8787").await;

    assert_eq!(body["installed"].as_bool(), Some(false));
    assert_eq!(body["canStart"].as_bool(), Some(false));
    assert_eq!(body["version"], Value::Null);
}

/// status/route.js:11 substitutes DEFAULT_HEADROOM_URL for an empty setting
/// rather than short-circuiting the whole probe.
#[tokio::test]
async fn an_empty_url_falls_back_to_the_default_loopback_url() {
    let (_temp, body) = status_for("").await;

    assert_eq!(body["url"], "http://localhost:8787");
    assert_eq!(body["localUrl"].as_bool(), Some(true));
    assert_eq!(body["canStart"].as_bool(), body["installed"].as_bool());
}
