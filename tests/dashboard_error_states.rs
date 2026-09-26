//! Bead openproxy-eie7 — "the dashboard lies to you".
//!
//! Two server-side halves of the batch that a Rust test can prove:
//!
//! 1. Dashboard mutation endpoints that returned HTTP 200 with
//!    `{"success": false}` on a DB write failure. A `fetch` with an
//!    `if (res.ok)` guard — which every dashboard mutation handler uses —
//!    cannot see that failure, so the UI reports success for an operation
//!    the backend rejected. These must return 5xx.
//! 2. `DELETE /api/provider-nodes/{id}` retained only `db.provider_nodes`,
//!    orphaning the node's connections: they stayed in `/api/providers` (and
//!    `filter_available_accounts`) with no UI left to find or remove them.
//!
//! Failure is forced by dropping the SQLite table the incremental diff writes
//! to, so `Db::update` returns `Err` for real rather than being mocked.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, CustomModel, ModelAliasTarget, ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

const BEARER: &str = "dash-error-bearer";

fn active_key(key: &str) -> ApiKey {
    ApiKey {
        id: format!("{key}-id"),
        name: "Local".into(),
        key: key.into(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        monthly_budget_usd: None,
        extra: BTreeMap::new(),
    }
}

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key(BEARER)];
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

fn request(method: Method, uri: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {BEARER}"))
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

async fn response_json(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// Make the next `Db::update` that touches `table` fail at the SQLite layer.
fn break_table(state: &AppState, table: &str) {
    state
        .db
        .sqlite
        .with_conn(|conn| conn.execute_batch(&format!("DROP TABLE IF EXISTS {table}")))
        .unwrap_or_else(|e| panic!("drop {table}: {e}"));
}

fn node(id: &str, name: &str) -> ProviderNode {
    ProviderNode {
        id: id.into(),
        r#type: "openai-compatible".into(),
        name: name.into(),
        prefix: None,
        api_type: Some("chat".into()),
        base_url: Some("https://example.test/v1".into()),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

fn connection(id: &str, provider: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: provider.into(),
        api_key: Some("sk-test".into()),
        ..ProviderConnection::default()
    }
}

fn custom_model(provider_alias: &str, id: &str) -> CustomModel {
    CustomModel {
        provider_alias: provider_alias.into(),
        id: id.into(),
        r#type: "llm".into(),
        name: None,
        extra: BTreeMap::new(),
    }
}

// ── models_disabled.rs ───────────────────────────────────────────
// "Disable All" is the highest-value path in this bead: it is how a user stops
// a model from being billed, and its client `catch { /* ignore */ }` was dead
// code because the endpoint answered 200.

#[tokio::test]
async fn disable_models_returns_500_when_db_write_fails() {
    let state = app_state().await;
    break_table(&state, "disabledModels");
    let app = openproxy::build_app(state);

    let response = app
        .oneshot(request(
            Method::POST,
            "/api/models/disabled",
            Body::from(r#"{"providerAlias":"openai","ids":["gpt-4o"]}"#),
        ))
        .await
        .unwrap();

    let (status, _) = response_json(response).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "POST /api/models/disabled must not answer 200 when the DB write fails"
    );
}

#[tokio::test]
async fn enable_models_returns_500_when_db_write_fails() {
    let state = app_state().await;
    state
        .db
        .update(|db| {
            db.extra.insert(
                "disabledModels".to_string(),
                json!({ "openai": ["gpt-4o"] }),
            );
        })
        .await
        .unwrap();
    break_table(&state, "disabledModels");
    let app = openproxy::build_app(state);

    let response = app
        .oneshot(request(
            Method::DELETE,
            "/api/models/disabled?providerAlias=openai&id=gpt-4o",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, _) = response_json(response).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "DELETE /api/models/disabled must not answer 200 when the DB write fails"
    );
}

// ── models_alias.rs ──────────────────────────────────────────────

#[tokio::test]
async fn update_model_alias_returns_500_when_db_write_fails() {
    let state = app_state().await;
    state
        .db
        .update(|db| {
            db.model_aliases.insert(
                "draft".into(),
                ModelAliasTarget::Path("openai/gpt-4.1".into()),
            );
        })
        .await
        .unwrap();
    break_table(&state, "kv");
    let app = openproxy::build_app(state);

    let response = app
        .oneshot(request(
            Method::PUT,
            "/api/models/alias/draft",
            Body::from(r#"{"target":"openai/gpt-4o-mini"}"#),
        ))
        .await
        .unwrap();

    let (status, _) = response_json(response).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "PUT /api/models/alias/{{alias}} must not answer 200 when the DB write fails"
    );
}

// ── models_custom.rs ─────────────────────────────────────────────

#[tokio::test]
async fn update_custom_model_returns_500_when_db_write_fails() {
    let state = app_state().await;
    state
        .db
        .update(|db| db.custom_models.push(custom_model("openai", "my-model")))
        .await
        .unwrap();
    // customModels is diffed through the shared `kv` table (patch.rs
    // diff_kv_scope), so that is the table to break.
    break_table(&state, "kv");
    let app = openproxy::build_app(state);

    let response = app
        .oneshot(request(
            Method::PUT,
            "/api/models/custom/my-model",
            Body::from(r#"{"name":"Renamed"}"#),
        ))
        .await
        .unwrap();

    let (status, _) = response_json(response).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "PUT /api/models/custom/{{id}} must not answer 200 when the DB write fails"
    );
}

#[tokio::test]
async fn delete_custom_model_returns_500_when_db_write_fails() {
    let state = app_state().await;
    state
        .db
        .update(|db| db.custom_models.push(custom_model("openai", "my-model")))
        .await
        .unwrap();
    break_table(&state, "kv");
    let app = openproxy::build_app(state);

    let response = app
        .oneshot(request(
            Method::DELETE,
            "/api/models/custom/my-model",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, _) = response_json(response).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "DELETE /api/models/custom/{{id}} must not answer 200 when the DB write fails"
    );
}

// ── provider_filters.rs ──────────────────────────────────────────

#[tokio::test]
async fn upsert_provider_filter_returns_500_when_db_write_fails() {
    let state = app_state().await;
    break_table(&state, "kv");
    let app = openproxy::build_app(state);

    let response = app
        .oneshot(request(
            Method::PUT,
            "/api/providers/filters",
            Body::from(r#"{"alias":"openai","freeOnly":true}"#),
        ))
        .await
        .unwrap();

    let (status, _) = response_json(response).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "PUT /api/providers/filters must not answer 200 when the DB write fails"
    );
}

#[tokio::test]
async fn upsert_favorites_returns_500_when_db_write_fails() {
    let state = app_state().await;
    break_table(&state, "kv");
    let app = openproxy::build_app(state);

    let response = app
        .oneshot(request(
            Method::PUT,
            "/api/models/favorites",
            Body::from(r#"{"alias":"openai","modelIds":["gpt-4o"]}"#),
        ))
        .await
        .unwrap();

    let (status, _) = response_json(response).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "PUT /api/models/favorites must not answer 200 when the DB write fails"
    );
}

// ── provider_nodes.rs ────────────────────────────────────────────
// Deleting a compatible node used to retain only `db.provider_nodes`. Its
// connections stayed behind: still listed by /api/providers, still selectable
// in filter_available_accounts, and with no UI left to find or remove them.
// The connections cascade with the node; the custom models do not.

#[tokio::test]
async fn delete_provider_node_removes_its_connections_but_preserves_custom_models() {
    let state = app_state().await;
    state
        .db
        .update(|db| {
            db.provider_nodes.push(node("node-a", "Node A"));
            db.provider_nodes.push(node("node-b", "Node B"));
            db.provider_connections.push(connection("conn-a", "node-a"));
            db.provider_connections.push(connection("conn-b", "node-b"));
            db.custom_models.push(custom_model("node-a", "model-a"));
            db.custom_models.push(custom_model("node-b", "model-b"));
        })
        .await
        .unwrap();

    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(request(
            Method::DELETE,
            "/api/provider-nodes/node-a",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, body) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.get("success").and_then(Value::as_bool), Some(true));

    let snapshot = state.db.snapshot();
    assert!(
        !snapshot.provider_nodes.iter().any(|n| n.id == "node-a"),
        "the node itself must be gone"
    );
    assert!(
        !snapshot
            .provider_connections
            .iter()
            .any(|c| c.id == "conn-a"),
        "connection conn-a was orphaned by the delete"
    );
    assert!(
        snapshot.custom_models.iter().any(|m| m.id == "model-a"),
        "custom models are user data keyed by provider alias, not owned by the node row: \
         9router's delete leaves them behind so a node recreated with the same id gets \
         its custom model list back"
    );

    // The other node's rows must survive untouched.
    assert!(snapshot.provider_nodes.iter().any(|n| n.id == "node-b"));
    assert!(snapshot
        .provider_connections
        .iter()
        .any(|c| c.id == "conn-b"));
    assert!(snapshot.custom_models.iter().any(|m| m.id == "model-b"));
}
