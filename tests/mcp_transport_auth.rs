//! MCP transport-level auth tests.
//!
//! `mcp_server::routes()` was merged into the unguarded `remaining` router, so
//! `/api/mcp`, `/api/mcp-server/sse` and `/api/mcp-server/message` had no
//! authentication of their own. Auth was pushed down into a handful of
//! mutating tool handlers via a `_api_key` JSON-RPC *argument*, which left
//! every read-only tool — tool inventory, `key_list`, `settings_get`, model
//! list, health — callable by any unauthenticated local process.
//!
//! These tests pin the transport to the same admin-tier gate every other
//! dashboard surface uses (dashboard session OR management API key), while
//! keeping the per-tool `_api_key` check working as defence in depth.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;

const ADMIN_KEY: &str = "admin-key";

/// `require_login = true` so the admin gate is actually exercised — the admin
/// middleware is a pass-through when login is disabled, exactly as it is for
/// every other admin route.
async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.settings.require_login = true;
        state.api_keys = vec![openproxy::types::ApiKey {
            id: "admin-1".into(),
            name: "Local".into(),
            key: ADMIN_KEY.into(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            extra: Default::default(),
            monthly_budget_usd: None,
        }];
    })
    .await
    .expect("db update");
    AppState::new(db)
}

fn rpc(method: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": 1, "method": method }).to_string()
}

fn call_rpc(name: &str, args: serde_json::Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": args },
    })
    .to_string()
}

async fn post_mcp(app: &axum::Router, body: String) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/mcp")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
}

// ── Unauthenticated access is refused ─────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn unauthenticated_tools_list_is_rejected() {
    let app = openproxy::build_app(app_state().await);

    let response = post_mcp(&app, rpc("tools/list")).await;

    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "tools/list must not be enumerable without transport auth"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unauthenticated_read_only_tool_call_is_rejected() {
    let app = openproxy::build_app(app_state().await);

    // `key_list` is read-only and was callable with no credentials at all.
    for tool in ["key_list", "settings_get", "health", "models_list"] {
        let response = post_mcp(&app, call_rpc(tool, json!({}))).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "read-only tool '{tool}' must not be callable without transport auth"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unauthenticated_sse_stream_does_not_open() {
    let app = openproxy::build_app(app_state().await);

    // Must be refused before the `event: endpoint` frame is emitted.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/mcp-server/sse")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn unauthenticated_message_endpoint_is_rejected() {
    let app = openproxy::build_app(app_state().await);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/mcp-server/message")
                .header("content-type", "application/json")
                .body(Body::from(rpc("tools/list")))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn bogus_transport_credentials_are_rejected() {
    let app = openproxy::build_app(app_state().await);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/mcp")
                .header("content-type", "application/json")
                .header("authorization", "Bearer not-a-real-key")
                .body(Body::from(rpc("tools/list")))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// ── Authorized clients still work end to end ──────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn authorized_client_can_list_and_call_tools() {
    let app = openproxy::build_app(app_state().await);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/mcp")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {ADMIN_KEY}"))
                .body(Body::from(rpc("tools/list")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 65_536)
        .await
        .expect("body");
    let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let tools = parsed["result"]["tools"].as_array().expect("tools array");
    assert!(!tools.is_empty(), "authorized client gets a tool inventory");

    // And a read-only tool actually executes.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/mcp")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {ADMIN_KEY}"))
                .body(Body::from(call_rpc("key_list", json!({}))))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 65_536)
        .await
        .expect("body");
    let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert!(
        parsed["result"]["content"][0]["text"].is_string(),
        "authorized key_list returns content, got: {parsed}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn authorized_sse_stream_still_opens() {
    let app = openproxy::build_app(app_state().await);

    // Do not consume the body — the stream stays open by design. Status is
    // enough to prove the transport guard did not break the SSE client path.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/mcp-server/sse")
                .header("authorization", format!("Bearer {ADMIN_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );
}

// ── Defence in depth: the per-tool `_api_key` check still applies ────

#[tokio::test(flavor = "multi_thread")]
async fn bogus_api_key_argument_still_rejected_at_tool_layer() {
    let app = openproxy::build_app(app_state().await);

    // Transport auth passes, but the mutating tool's own `_api_key` argument
    // is bogus — the tool layer must still refuse.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/mcp")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {ADMIN_KEY}"))
                .body(Body::from(call_rpc(
                    "key_create",
                    json!({ "name": "sneaky", "_api_key": "bogus" }),
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 65_536)
        .await
        .expect("body");
    let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        parsed["error"]["message"].as_str(),
        Some("Invalid or inactive API key"),
        "tool-layer _api_key check must remain: {parsed}"
    );
}

// ── Regression: the credential-leak PoC ────────────────────────────────────
// The first pass at these tests ran against an EMPTY database, where
// `provider_list` returns `[]` and every assertion passes — so the leak was
// missed. These seed a connection that actually carries credentials, including
// the two provider-specific secrets that live in `provider_specific_data`
// rather than on the struct, and assert none of them survive.

const CANARY_API_KEY: &str = "sk-ANT-CANARY-999";
const CANARY_ACCESS: &str = "at-ant-canary-999";
const CANARY_REFRESH: &str = "rt-ant-canary-999";
const CANARY_CLIENT_SECRET: &str = "kiro-client-secret-canary";
const CANARY_MIMO_TOKEN: &str = "mimo-pass-token-canary";

async fn seeded_state_with_credentials() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    let mut conn = openproxy::types::ProviderConnection::default();
    conn.id = "conn-canary".into();
    conn.provider = "kiro".into();
    conn.auth_type = "oauth".into();
    conn.name = Some("Canary".into());
    conn.api_key = Some(CANARY_API_KEY.to_string());
    conn.access_token = Some(CANARY_ACCESS.to_string());
    conn.refresh_token = Some(CANARY_REFRESH.to_string());
    conn.provider_specific_data
        .insert("clientSecret".into(), json!(CANARY_CLIENT_SECRET));
    conn.provider_specific_data
        .insert("mimoPassToken".into(), json!(CANARY_MIMO_TOKEN));
    db.update(move |state| {
        state.settings.require_login = true;
        state.provider_connections = vec![conn];
        state.api_keys = vec![openproxy::types::ApiKey {
            id: "admin-1".into(),
            name: "Local".into(),
            key: ADMIN_KEY.into(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            extra: Default::default(),
            monthly_budget_usd: None,
        }];
    })
    .await
    .expect("db update");
    AppState::new(db)
}

#[tokio::test]
async fn unauthenticated_mcp_never_returns_credentials() {
    let app = openproxy::build_app(seeded_state_with_credentials().await);

    for tool in ["provider_list", "key_list", "settings_get"] {
        let response = post_mcp(&app, call_rpc(tool, json!({}))).await;
        assert_ne!(
            response.status(),
            StatusCode::OK,
            "{tool} answered an unauthenticated caller"
        );
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        for canary in [
            CANARY_API_KEY,
            CANARY_ACCESS,
            CANARY_REFRESH,
            CANARY_CLIENT_SECRET,
            CANARY_MIMO_TOKEN,
        ] {
            assert!(
                !text.contains(canary),
                "{tool} leaked {canary} to an unauthenticated caller: {text}"
            );
        }
    }
}

// multi_thread flavor: the MCP tool dispatcher uses tokio::task::block_in_place,
// which panics on a current-thread runtime.
#[tokio::test(flavor = "multi_thread")]
async fn authenticated_mcp_redacts_credentials() {
    // Auth is necessary but not sufficient: an authenticated caller is still
    // an agent, and `provider_list` must hand back a redacted projection, the
    // same way the dashboard and the REST API do.
    let app = openproxy::build_app(seeded_state_with_credentials().await);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/mcp")
                .header("authorization", format!("Bearer {ADMIN_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(call_rpc("provider_list", json!({}))))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);
    for canary in [
        CANARY_API_KEY,
        CANARY_ACCESS,
        CANARY_REFRESH,
        CANARY_CLIENT_SECRET,
        CANARY_MIMO_TOKEN,
    ] {
        assert!(
            !text.contains(canary),
            "authenticated provider_list leaked {canary}: {text}"
        );
    }
    // The connection itself must still be listed — redaction, not omission.
    assert!(
        text.contains("conn-canary"),
        "provider_list dropped the row"
    );
}
