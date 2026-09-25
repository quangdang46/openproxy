//! `GET /api/tunnel/status` must report the user's intent (`settingsEnabled`)
//! separately from the run state (`enabled` / `running`), as 9router's
//! `getTunnelStatus` / `getTailscaleStatus` do. The dashboard reads
//! `settingsEnabled` so a tunnel the watchdog is restarting does not read back
//! as "user turned it off".

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use serde_json::Value;
use tempfile::tempdir;
use tower::util::ServiceExt;

async fn build_test_app(seed: impl FnOnce(&mut openproxy::types::AppDb)) -> axum::Router {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.settings.require_login = false;
        seed(state);
    })
    .await
    .expect("seed settings");
    openproxy::build_app(AppState::new(db))
}

async fn get(app: axum::Router, uri: &str) -> Value {
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK, "GET {uri}");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).expect("json body")
}

#[tokio::test]
async fn tunnel_status_reports_settings_intent_separately_from_run_state() {
    let app = build_test_app(|state| {
        state.settings.tunnel_enabled = true;
        state.settings.tailscale_enabled = true;
    })
    .await;

    let body = get(app, "/api/tunnel/status").await;

    // Intent is true while run state is false: nothing was actually spawned in
    // this process, which is the only state that distinguishes "user asked for
    // this" from "user turned it off".
    assert_eq!(body["tunnel"]["settingsEnabled"], Value::Bool(true));
    assert_eq!(body["tunnel"]["enabled"], Value::Bool(false));
    assert_eq!(body["tunnel"]["running"], Value::Bool(false));
    assert_eq!(body["tailscale"]["settingsEnabled"], Value::Bool(true));
    assert_eq!(body["tailscale"]["enabled"], Value::Bool(false));
    assert_eq!(body["tailscale"]["running"], Value::Bool(false));
}

#[tokio::test]
async fn tunnel_status_settings_enabled_false_is_representable() {
    let app = build_test_app(|_| {}).await;

    let body = get(app, "/api/tunnel/status").await;

    // Intent=false has to arrive as an explicit boolean, not collapse into the
    // run state the way an absent key does.
    assert_eq!(body["tunnel"]["settingsEnabled"], Value::Bool(false));
    assert_eq!(body["tunnel"]["enabled"], Value::Bool(false));
    assert_eq!(body["tailscale"]["settingsEnabled"], Value::Bool(false));
    assert_eq!(body["tailscale"]["enabled"], Value::Bool(false));
}

#[tokio::test]
async fn tunnel_status_tailscale_block_carries_a_boolean_logged_in() {
    let app = build_test_app(|state| {
        state.settings.tailscale_enabled = true;
    })
    .await;

    let body = get(app.clone(), "/api/tunnel/status").await;

    // `tailscale` is absent on CI, so the probe must come back quickly with a
    // real `false` — a missing key here is `Value::Null`, and an unbounded
    // probe would hang the request instead of failing this assert.
    assert!(
        body["tailscale"]["loggedIn"].is_boolean(),
        "loggedIn must be a boolean, got {:?}",
        body["tailscale"]["loggedIn"]
    );
    assert_eq!(body["tailscale"]["loggedIn"], Value::Bool(false));

    // The dashboard's login poller (EndpointPageClient.tsx:725) loops on
    // /api/tunnel/tailscale-check instead; a hardcoded `false` there meant it
    // could never leave "Waiting for login...". Both call sites have to report
    // the same probed value.
    let check = get(app, "/api/tunnel/tailscale-check").await;
    assert_eq!(check["loggedIn"], body["tailscale"]["loggedIn"]);
}

#[tokio::test]
async fn tunnel_status_skips_the_login_probe_when_tailscale_is_off() {
    let app = build_test_app(|_| {}).await;

    let body = get(app, "/api/tunnel/status").await;

    // 9router gates the probe on the setting (manager.js:124); with the
    // funnel disabled the answer is false without ever shelling out.
    assert_eq!(body["tailscale"]["loggedIn"], Value::Bool(false));
}
