//! Tail-end parity for the models API (9router `/v1/models` + `/api/models`).
//!
//! Covers the four halves that had no OpenProxy surface at all: the
//! cross-instance recursion marker, the single-model lookup, the model catalog
//! sync endpoints, and the empty-`ids` no-op on `/api/models/disabled`.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection};
use serde_json::{json, Value};
use sha2::Sha256;
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

const BEARER: &str = "models-tail-bearer";

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

fn active_machine_key(key: &str, machine_id: &str) -> ApiKey {
    ApiKey {
        machine_id: Some(machine_id.into()),
        ..active_key(key)
    }
}

fn cli_token(machine_id: &str, key_id: &str) -> String {
    type HmacSha256 = Hmac<Sha256>;

    // Never a hardcoded default: the token is only valid against the secret
    // the running process resolved.
    let secret = openproxy::core::auth::api_key_secret();
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(machine_id.as_bytes());
    mac.update(key_id.as_bytes());
    let crc = hex::encode(mac.finalize().into_bytes());
    format!("sk-{machine_id}-{key_id}-{}", &crc[..12])
}

fn connection(provider: &str) -> ProviderConnection {
    ProviderConnection {
        id: format!("{provider}-conn"),
        provider: provider.into(),
        auth_type: "apikey".into(),
        is_active: Some(true),
        api_key: Some("provider-key".into()),
        ..Default::default()
    }
}

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![
            active_key(BEARER),
            active_machine_key(&cli_token("machine1", "cli01"), "machine1"),
        ];
        state.provider_connections = vec![connection("openai")];
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

async fn send(state: &AppState, request: Request<Body>) -> (StatusCode, Value) {
    let app = openproxy::build_app(state.clone());
    let response = app.oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("authorization", format!("Bearer {BEARER}"))
        .body(Body::empty())
        .unwrap()
}

fn admin_request(method: Method, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {BEARER}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// A compatible connection with no `enabledModels`, so `/v1/models` has to ask
/// the upstream for its catalog — the exact hop the recursion marker rides on.
async fn compatible_state(server: &MockServer) -> AppState {
    let state = app_state().await;
    let uri = server.uri();
    state
        .db
        .update(move |db| {
            db.provider_connections = vec![ProviderConnection {
                id: "compat-conn".into(),
                provider: "openai-compatible-local".into(),
                auth_type: "apikey".into(),
                is_active: Some(true),
                api_key: Some("provider-key".into()),
                provider_specific_data: BTreeMap::from([
                    ("baseUrl".into(), json!(uri)),
                    ("prefix".into(), json!("compat")),
                ]),
                ..Default::default()
            }];
        })
        .await
        .expect("seed compatible connection");
    state
}

async fn compatible_mock() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "id": "gpt-4o-mini" }]
        })))
        .mount(&server)
        .await;
    server
}

// ── Finding: cross-instance recursion guard ───────────────────────────

#[tokio::test]
async fn models_endpoint_marks_outbound_compatible_models_fetch_with_internal_header() {
    // Two proxies pointed at each other must be able to tell "a client asked
    // me for my catalog" from "a sibling proxy is asking me for my catalog".
    let server = compatible_mock().await;
    let state = compatible_state(&server).await;

    let (status, _) = send(&state, get("/v1/models")).await;
    assert_eq!(status, StatusCode::OK);

    let requests = server.received_requests().await.expect("recorded requests");
    assert_eq!(
        requests.len(),
        1,
        "expected exactly one outbound /models hop, got {requests:?}"
    );
    assert_eq!(
        requests[0]
            .headers
            .get("x-9r-internal-models-fetch")
            .and_then(|value| value.to_str().ok()),
        Some("1"),
        "the outbound compatible-models fetch must carry the recursion marker"
    );
}

#[tokio::test]
async fn models_endpoint_with_internal_header_skips_compatible_model_discovery() {
    // The loop-breaking half: a marked request is answered from stored state
    // only, so two instances wired together settle instead of ping-ponging.
    let server = compatible_mock().await;
    let state = compatible_state(&server).await;

    let request = Request::builder()
        .uri("/v1/models")
        .header("x-9r-internal-models-fetch", "1")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(&state, request).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "a marked request must not fan out to a compatible provider"
    );
    assert_eq!(body["object"], json!("list"), "body must stay well-formed");
    assert!(
        !body["data"].to_string().contains("compat/gpt-4o-mini"),
        "the upstream catalog leaked into a request that was told not to fetch: {body}"
    );
}

// ── Finding: single-model lookup ──────────────────────────────────────

#[tokio::test]
async fn single_model_lookup_returns_bare_model_card() {
    let state = app_state().await;
    let (status, body) = send(&state, get("/v1/models/openai/gpt-4.1")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], json!("openai/gpt-4.1"));
    assert_eq!(body["object"], json!("model"));
    assert_eq!(body["owned_by"], json!("openai"));
    assert!(
        body.get("data").is_none(),
        "9router returns the matched model bare, not in a list envelope: {body}"
    );
}

#[tokio::test]
async fn single_model_lookup_unknown_id_returns_model_not_found_404() {
    let state = app_state().await;
    let (status, body) = send(&state, get("/v1/models/openai/definitely-not-a-model")).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["type"], json!("invalid_request_error"));
    assert_eq!(body["error"]["code"], json!("model_not_found"));
    assert_eq!(
        body["error"]["message"],
        json!("The model 'openai/definitely-not-a-model' does not exist or you do not have access to it.")
    );
}

#[tokio::test]
async fn single_segment_path_falls_through_to_the_model_lookup() {
    // 9router's kind map has no entry for a bare provider name, so the
    // request is a model lookup that misses — not an "unknown kind" error.
    let state = app_state().await;
    let (status, body) = send(&state, get("/v1/models/openai")).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], json!("model_not_found"));
    assert_eq!(
        body["error"]["message"],
        json!("The model 'openai' does not exist or you do not have access to it.")
    );
}

#[tokio::test]
async fn v1_v1_models_mirrors_the_single_model_lookup() {
    let state = app_state().await;
    let (status, body) = send(&state, get("/v1/v1/models/openai/gpt-4.1")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], json!("openai/gpt-4.1"));
}

// ── Finding: /v1/models is never API-key gated ────────────────────────

#[tokio::test]
async fn models_listing_is_unauthenticated_even_when_require_login_is_on() {
    // 9router's handler has no isValidApiKey call, and a client model picker
    // discovers the catalog before it has a key to offer.
    let state = app_state().await;
    assert!(
        state.db.snapshot().settings.require_login,
        "this test only means something with require_login on"
    );

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("*")
    );
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["object"], json!("list"));
}

fn chat_request(auth: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/v1/chat/completions")
        .header("content-type", "application/json");
    for (name, value) in auth {
        builder = builder.header(*name, *value);
    }
    builder
        .body(Body::from(
            json!({
                "model": "openai/gpt-4.1",
                "messages": [{ "role": "user", "content": "hi" }]
            })
            .to_string(),
        ))
        .unwrap()
}

#[tokio::test]
async fn missing_invalid_and_inactive_keys_return_unauthorized() {
    // Auth coverage lives on the dispatch route now that /v1/models is open.
    for auth in [
        vec![],
        vec![("authorization", "Bearer missing-key")],
        vec![("authorization", "Bearer inactive-key")],
    ] {
        let state = app_state().await;
        let (status, _) = send(&state, chat_request(&auth)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "auth headers: {auth:?}");
    }
}

#[tokio::test]
async fn bearer_takes_precedence_over_x_api_key() {
    let state = app_state().await;
    let (status, _) = send(
        &state,
        chat_request(&[
            ("authorization", "Bearer wrong-key"),
            ("x-api-key", BEARER),
            ("x-9r-cli-token", &cli_token("machine1", "cli01")),
        ]),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ── Finding: /api/models/catalog-sync ─────────────────────────────────

#[tokio::test]
async fn catalog_sync_get_reports_state_and_catalog_summary() {
    let state = app_state().await;
    let (status, body) = send(&state, get("/api/models/catalog-sync")).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body["running"].is_boolean(), "missing `running`: {body}");
    assert!(body.get("lastSync").is_some(), "missing `lastSync`: {body}");
    assert!(
        body.get("lastError").is_some(),
        "missing `lastError`: {body}"
    );
    assert!(
        body.get("lastResult").is_some(),
        "missing `lastResult`: {body}"
    );
    assert!(body.get("etag").is_some(), "missing `etag`: {body}");
    assert_eq!(body["url"], json!("https://models.dev/api.json"));
    assert!(
        body["intervalMs"].is_number(),
        "missing `intervalMs`: {body}"
    );
    let file = body["file"].as_str().expect("file path");
    assert!(
        file.contains("model-catalog.json"),
        "`file` should point at the catalog the sync writes: {file}"
    );

    let catalog = &body["catalog"];
    assert!(
        catalog.is_null() || catalog.is_object(),
        "`catalog` is null until the first sync, never absent: {body}"
    );
    if let Some(catalog) = catalog.as_object() {
        for key in ["syncedAt", "models", "providers", "bytes"] {
            assert!(
                catalog.contains_key(key),
                "catalog summary missing {key}: {catalog:?}"
            );
        }
    }
}

#[tokio::test]
async fn catalog_sync_get_requires_admin_credentials() {
    let state = app_state().await;
    let app = openproxy::build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/models/catalog-sync")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        response.status() == StatusCode::UNAUTHORIZED || response.status() == StatusCode::FORBIDDEN,
        "catalog sync must sit behind the admin tier, got {}",
        response.status()
    );
}

#[tokio::test]
async fn catalog_sync_post_is_wired_and_returns_a_defined_outcome() {
    // The sync reaches models.dev, so the branch depends on the sandbox: assert
    // that whichever fires is well-formed, never that a specific one does.
    let state = app_state().await;
    let (status, body) = send(
        &state,
        admin_request(Method::POST, "/api/models/catalog-sync", json!({})),
    )
    .await;

    match status {
        StatusCode::OK => {
            assert_eq!(body["success"], json!(true));
            assert!(body.get("result").is_some(), "no `result`: {body}");
        }
        StatusCode::SERVICE_UNAVAILABLE => {
            assert!(
                body.get("error").and_then(Value::as_str).is_some(),
                "a 503 must carry the sync error: {body}"
            );
        }
        other => panic!("POST /api/models/catalog-sync answered {other}: {body}"),
    }
}

// ── Finding: an empty ids array is a no-op, not a 400 ─────────────────

#[tokio::test]
async fn disable_models_with_empty_ids_returns_ok_noop() {
    let state = app_state().await;
    let (status, _) = send(
        &state,
        admin_request(
            Method::POST,
            "/api/models/disabled",
            json!({ "providerAlias": "openai", "ids": ["gpt-4o"] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(
        &state,
        admin_request(
            Method::POST,
            "/api/models/disabled",
            json!({ "providerAlias": "openai", "ids": [] }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "empty ids must not be rejected: {body}"
    );
    assert_eq!(body["success"], json!(true));

    let (_, body) = send(&state, get("/api/models/disabled?providerAlias=openai")).await;
    assert_eq!(
        body["ids"],
        json!(["gpt-4o"]),
        "the existing disabled set must survive an empty add"
    );
}

#[tokio::test]
async fn disable_models_without_provider_alias_still_returns_bad_request() {
    // The half of 9router's guard that must stay: `!providerAlias`.
    let state = app_state().await;
    let (_, body) = send(
        &state,
        admin_request(
            Method::POST,
            "/api/models/disabled",
            json!({ "providerAlias": "   ", "ids": ["gpt-4o"] }),
        ),
    )
    .await;
    assert_eq!(body["error"], json!("providerAlias and ids[] required"));

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(admin_request(
            Method::POST,
            "/api/models/disabled",
            json!({ "ids": ["gpt-4o"] }),
        ))
        .await
        .unwrap();
    assert!(
        response.status().is_client_error(),
        "a request with no providerAlias at all must be rejected, got {}",
        response.status()
    );
}

#[tokio::test]
async fn disable_models_with_empty_ids_on_unknown_alias_returns_ok() {
    let state = app_state().await;
    let (status, body) = send(
        &state,
        admin_request(
            Method::POST,
            "/api/models/disabled",
            json!({ "providerAlias": "never-seen", "ids": [] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["success"], json!(true));

    let (_, body) = send(&state, get("/api/models/disabled")).await;
    assert!(
        body["disabled"].get("never-seen").is_none(),
        "an empty add must not leave an empty entry behind: {body}"
    );
}
