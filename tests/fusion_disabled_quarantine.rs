//! openproxy-s2oc — the Fusion strategy must honour the operator's mute
//! list and the auto-quarantine map, and the combo test-model probe must not
//! report an openproxy-internal 400 rejection as success.
//!
//! Defect 1: the Fusion branch of the chat dispatcher passed the RAW combo
//! member list to `handle_fusion_chat*`, so muted / quarantined / degraded
//! members were still fanned out to (and billed). Only the sequential
//! strategies (Fallback, RoundRobin, …) consulted `disabled_models` and
//! `combo_quarantine_for`.
//!
//! Defect 2: `test_combo_model` / `ping_model` treated `400 Bad Request` as
//! "model responded" (`ok = status == OK || status == BAD_REQUEST`). A 400
//! from openproxy's own dispatcher ("No credentials for provider: X") is a
//! *rejection*, not a model answer, so the UI rendered a green tick for an
//! unconfigured provider.

#![allow(clippy::await_holding_lock)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::core::combo::{clear_combo_quarantine, mark_combo_member_quarantined};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, Combo, ProviderConnection, ProviderNode, Settings};
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEST_KEY: &str = "fusion-quarantine-test-key";

fn active_key() -> ApiKey {
    ApiKey {
        id: "key-1".into(),
        name: "Local".into(),
        key: TEST_KEY.into(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        extra: BTreeMap::new(),
        monthly_budget_usd: None,
    }
}

fn provider_node(id: &str, prefix: &str, base_url: &str) -> ProviderNode {
    ProviderNode {
        id: id.into(),
        r#type: "openai-compatible".into(),
        name: "Compatible".into(),
        prefix: Some(prefix.into()),
        api_type: Some("chat".into()),
        base_url: Some(base_url.into()),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

fn connection(id: &str, provider: &str, api_key: &str, enabled: &[&str]) -> ProviderConnection {
    let mut connection = ProviderConnection {
        id: id.into(),
        provider: provider.into(),
        auth_type: "apikey".into(),
        name: Some(id.into()),
        priority: Some(1),
        is_active: Some(true),
        created_at: None,
        updated_at: None,
        display_name: None,
        email: None,
        global_priority: None,
        // `None` so credential selection is driven purely by `enabledModels`.
        default_model: None,
        access_token: None,
        refresh_token: None,
        expires_at: None,
        token_type: None,
        scope: None,
        id_token: None,
        project_id: None,
        api_key: Some(api_key.into()),
        test_status: None,
        last_tested: None,
        last_error: None,
        last_error_at: None,
        rate_limited_until: None,
        expires_in: None,
        error_code: None,
        consecutive_use_count: None,
        backoff_level: None,
        consecutive_errors: None,
        proxy_url: None,
        proxy_label: None,
        use_connection_proxy: None,
        provider_specific_data: BTreeMap::new(),
        runtime_transport: None,
        extra: BTreeMap::new(),
    };
    connection
        .provider_specific_data
        .insert("enabledModels".into(), json!(enabled));
    connection
}

/// A two-member Fusion combo served by one mock upstream.
async fn fusion_state(combo_name: &str, disabled: Vec<String>, upstream: &MockServer) -> AppState {
    let mut extra = BTreeMap::new();
    extra.insert("strategy".to_string(), json!("fusion"));

    let combo = Combo {
        id: format!("{combo_name}-id"),
        name: combo_name.to_string(),
        models: vec!["custom/panel-a".into(), "custom/panel-b".into()],
        disabled_models: disabled,
        kind: None,
        created_at: None,
        updated_at: None,
        extra,
    };

    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key()];
        state.provider_nodes = vec![provider_node(
            "node-openai",
            "custom",
            &format!("{}/v1", upstream.uri()),
        )];
        state.provider_connections = vec![connection(
            "conn-1",
            "node-openai",
            "upstream-key",
            &["panel-a", "panel-b"],
        )];
        state.combos = vec![combo];
        let mut settings = Settings::default();
        // Auth is not under test; the chat guard is disabled so the dispatch
        // reaches the combo pipeline.
        settings.require_login = false;
        state.settings = settings;
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

async fn mount_upstream() -> MockServer {
    let upstream = MockServer::start().await;
    // A catch-all: every panel/judge leg succeeds, so the panel SET (not the
    // response) is the only signal the assertions need.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "fusion-panel",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "panel answer" },
                "finish_reason": "stop"
            }]
        })))
        .mount(&upstream)
        .await;
    upstream
}

async fn dispatch_combo(app: axum::Router, combo: &str) -> (StatusCode, serde_json::Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": combo,
                        "messages": [{"role": "user", "content": "hi"}],
                        "stream": false,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = if body.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null)
    };
    (status, json)
}

/// The models the upstream panel executor actually received.
async fn dispatched_models(upstream: &MockServer) -> Vec<String> {
    let requests = upstream
        .received_requests()
        .await
        .expect("received requests");
    requests
        .iter()
        .filter(|request| request.url.path() == "/v1/chat/completions")
        .map(|request| {
            request
                .body_json::<serde_json::Value>()
                .expect("chat body")
                .get("model")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The UI promise: "Disable — keep in list but never dispatch to it".
#[tokio::test]
async fn fusion_dispatch_never_reaches_a_muted_member() {
    let upstream = mount_upstream().await;
    let state = fusion_state(
        "fusion-muted",
        vec!["custom/panel-b".to_string()],
        &upstream,
    )
    .await;
    let app = openproxy::build_app(state);

    let (status, _body) = dispatch_combo(app, "fusion-muted").await;
    assert_eq!(status, StatusCode::OK);

    let models = dispatched_models(&upstream).await;
    assert!(
        !models.is_empty(),
        "the surviving member must still be dispatched"
    );
    assert!(
        !models.iter().any(|model| model == "panel-b"),
        "muted member was dispatched to and billed: {models:?}"
    );
}

/// A member quarantined by a previous failure must be excluded from the next
/// Fusion dispatch, exactly like the sequential strategies exclude it.
#[tokio::test]
async fn fusion_dispatch_never_reaches_a_quarantined_member() {
    let upstream = mount_upstream().await;
    let combo = "fusion-quarantined";
    let state = fusion_state(combo, Vec::new(), &upstream).await;
    let app = openproxy::build_app(state);

    mark_combo_member_quarantined(combo, "custom/panel-b", Duration::from_secs(60));

    let (status, _body) = dispatch_combo(app, combo).await;
    clear_combo_quarantine(combo);

    assert_eq!(status, StatusCode::OK);
    let models = dispatched_models(&upstream).await;
    assert!(
        !models.is_empty(),
        "the healthy member must still be dispatched"
    );
    assert!(
        !models.iter().any(|model| model == "panel-b"),
        "quarantined member was dispatched to again: {models:?}"
    );
}

/// Muting every member must surface the same 400 the non-fusion path uses
/// instead of fanning out to a now-empty panel set.
#[tokio::test]
async fn fusion_with_every_member_muted_returns_400_and_dispatches_nothing() {
    let upstream = mount_upstream().await;
    let combo = "fusion-all-muted";
    let state = fusion_state(
        combo,
        vec!["custom/panel-a".to_string(), "custom/panel-b".to_string()],
        &upstream,
    )
    .await;
    let app = openproxy::build_app(state);

    let (status, body) = dispatch_combo(app, combo).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        message.contains("disabled"),
        "operator muted everything and must be told so: {body}"
    );
    let models = dispatched_models(&upstream).await;
    assert!(
        models.is_empty(),
        "no member is dispatchable, so nothing may be sent upstream: {models:?}"
    );
}

/// Defect 2: an unconfigured member must report `ok: false` with the
/// dispatcher's reason, not a green tick.
#[tokio::test]
async fn combo_test_model_reports_unconfigured_provider_as_failure() {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key()];
        let mut settings = Settings::default();
        settings.require_login = false;
        state.settings = settings;
        // No provider node and no connection for `unconfigured/*`, so the
        // chat dispatcher rejects the request in-process with a 400.
    })
    .await
    .expect("seed db");
    let app = openproxy::build_app(AppState::new(db));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/combos/test-model")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "model": "unconfigured/never-configured" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(
        json["ok"], false,
        "an in-process 400 rejection is not a model answer: {json}"
    );
    let error = json["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("No credentials for provider"),
        "the operator must see why the probe failed: {json}"
    );
}
