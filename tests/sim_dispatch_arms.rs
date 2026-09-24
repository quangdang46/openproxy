//! Bead openproxy-umtq: simulation mock must actually engage for the
//! provider dispatch ladder (26 of 27 arms), not just the OpenAI-shaped
//! `DefaultExecutor` else-arm.
//!
//! Defect under test: `ExecutionRequest` carries `sim_headers`/`force_mock`,
//! but only the OpenAI-shaped dispatch arm (chat.rs) threads them into
//! `DefaultExecutor`. Every dedicated provider arm (kiro, vertex, codex, …)
//! builds its own request type and drops the simulation inputs, so mock mode
//! silently no-ops for them while `/api/mock/status` reports `effective:mock`.
//!
//! RED (pre-fix) behaviour for a credentialless kiro connection set to mock:
//! the request enters the kiro dispatch arm, `KiroExecutor` fails fast on
//! missing credentials, and the caller gets an error — NOT a simulated
//! envelope, and no proof the mock branch ran.
//!
//! Fix under test: short-circuit BEFORE the dispatch ladder. When the
//! effective mode for the provider is `mock`, route to `DefaultExecutor` (the
//! only executor that implements simulation) with `force_mock`, instead of
//! entering the per-provider match.
//!
//! These tests are hermetic: kiro is dispatched with a credentialless stub
//! and never reaches a real upstream (it fails on missing credentials before
//! any network I/O), and the simulated branch performs zero network calls.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::core::simulation::ProviderExecutionMode;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

const TEST_KEY: &str = "sim-dispatch-arms-key";

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

/// A credentialless, active `kiro` connection. The simulation stub path
/// (chat.rs `sim-stub-*`) synthesizes the same shape when no stored
/// credentials exist, so this mirrors the real credentialless case.
fn kiro_connection() -> ProviderConnection {
    ProviderConnection {
        id: "kiro-conn-1".into(),
        provider: "kiro".into(),
        auth_type: "api_key".into(),
        name: Some("Kiro".into()),
        priority: Some(1),
        is_active: Some(true),
        api_key: None,
        access_token: None,
        default_model: Some("kiro/gpt-4o".into()),
        provider_specific_data: BTreeMap::new(),
        ..ProviderConnection::default()
    }
}

async fn app_state() -> (AppState, tempfile::TempDir) {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key()];
        state.provider_connections = vec![kiro_connection()];
    })
    .await
    .expect("seed db");
    (AppState::new(db), temp)
}

async fn post_chat(app: axum::Router, body: Value) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, json)
}

fn chat_body(model: &str) -> Value {
    json!({
        "model": model,
        "stream": false,
        "messages": [{"role": "user", "content": "hello from the sim test"}]
    })
}

/// Set a provider's persisted simulation mode to `mock` via the public
/// persistence API (same path the status endpoint and the PUT surface use).
fn set_mode(state: &AppState, provider: &str, mode: ProviderExecutionMode) {
    state
        .db
        .sqlite
        .with_conn(|conn| {
            openproxy::core::simulation::persistence::set_provider_mode(conn, provider, mode)
        })
        .expect("set provider mode");
}

/// A credentialless kiro provider set to mock must return a SIMULATED
/// envelope (the simulator's `chatcmpl-sim-*` id / `Echo:` content), not a
/// credential error. Pre-fix the request drops the simulation inputs in the
/// kiro dispatch arm and fails on missing credentials, so this is the RED
/// assertion that proves the mock branch actually ran.
#[tokio::test]
async fn kiro_mock_mode_returns_simulated_envelope() {
    let (state, _temp) = app_state().await;
    set_mode(&state, "kiro", ProviderExecutionMode::Mock);

    let app = openproxy::build_app(state);
    let (status, body) = post_chat(app, chat_body("kiro/gpt-4o")).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "mock kiro must succeed with a simulated envelope, got: {body}"
    );
    // The simulated OpenAI envelope is protocol-shaped and echoes the prompt.
    assert_eq!(body["object"], "chat.completion", "body: {body}");
    let content = body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_else(|| panic!("simulated envelope missing content: {body}"));
    assert!(
        content.contains("hello from the sim test"),
        "simulated envelope should echo the prompt, got: {content}"
    );
    // The simulator stamps its own id prefix — proof the simulation engine,
    // not a real provider, produced this response.
    let id = body["id"].as_str().unwrap_or_default();
    assert!(
        id.starts_with("chatcmpl-sim-"),
        "expected simulated id prefix, got: {id}"
    );
}

/// A provider set to mock must not fall through to the real (credentialed /
/// no-credential) path. Even with NO credentials available, the simulated
/// branch must answer — a real executor would error or (worse) reach upstream.
///
/// This is the "status and execution agree" invariant from the bead: anything
/// `/api/mock/status` reports as `effective:mock` must actually execute in
/// mock mode, not silently no-op.
#[tokio::test]
async fn mock_mode_is_honored_even_without_credentials() {
    // No stored connection at all: the credentialless sim-stub path applies.
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key()];
        state.provider_connections = Vec::new();
    })
    .await
    .expect("seed db");
    let state = AppState::new(db);
    set_mode(&state, "kiro", ProviderExecutionMode::Mock);

    let app = openproxy::build_app(state);
    let (status, body) = post_chat(app, chat_body("kiro/gpt-4o")).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "mock kiro with no credentials must still simulate, got: {body}"
    );
    assert_eq!(body["object"], "chat.completion", "body: {body}");
    let id = body["id"].as_str().unwrap_or_default();
    assert!(
        id.starts_with("chatcmpl-sim-"),
        "expected simulated id prefix, got: {id}"
    );
}

/// Data-driven guard: for a set of providers that dispatch through dedicated
/// arms, flipping the provider to mock must make execution return a simulated
/// envelope. A newly-added dedicated-arm provider cannot silently opt out of
/// the simulation path because this iterates provider names, not one hardcoded
/// executor. These are all credentialless, so the pre-fix real path fails
/// fast (missing credentials) and the test stays hermetic.
#[tokio::test]
async fn dedicated_arm_providers_honor_mock_mode() {
    // Representative providers that each have their own dispatch arm in the
    // chat.rs ladder (i.e. NOT the OpenAI-shaped else-arm). Keep credentialless.
    let providers = ["kiro", "vertex", "codex", "cursor"];

    for provider in providers {
        let temp = tempdir().expect("tempdir");
        let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
        db.update(|state| {
            state.api_keys = vec![active_key()];
            state.provider_connections = vec![ProviderConnection {
                id: format!("{provider}-conn-1"),
                provider: provider.to_string(),
                auth_type: "api_key".into(),
                is_active: Some(true),
                default_model: Some(format!("{provider}/gpt-4o")),
                ..ProviderConnection::default()
            }];
        })
        .await
        .expect("seed db");
        let state = AppState::new(db);
        set_mode(&state, provider, ProviderExecutionMode::Mock);

        let app = openproxy::build_app(state);
        let (status, body) = post_chat(app, chat_body(&format!("{provider}/gpt-4o"))).await;

        assert_eq!(
            status,
            StatusCode::OK,
            "{provider} set to mock must return a simulated envelope, got: {body}"
        );
        assert_eq!(body["object"], "chat.completion", "{provider} body: {body}");
        let id = body["id"].as_str().unwrap_or_default();
        assert!(
            id.starts_with("chatcmpl-sim-"),
            "{provider} should be simulated (id prefix), got: {id}"
        );
    }
}

/// The strongest form of the bead's requirement: a **credentialed**
/// dedicated-arm provider (ollama) whose real upstream is a wiremock server.
/// With the provider set to mock, the request must be served by the simulator
/// and the real upstream must receive ZERO calls (`.expect(0)`).
///
/// This is the dangerous case the bead calls out — "a user flips a provider
/// to mock, the status endpoint confirms it, and the traffic silently goes to
/// the real provider and real billing." ollama is chosen because it is a
/// dedicated-arm provider whose base URL is controllable via
/// `provider_specific_data.baseUrl`, so the wiremock genuinely stands in for
/// the real upstream. Pre-fix the request entered the ollama arm and hit the
/// wiremock (proving the silent no-op); post-fix the short-circuit serves it
/// from the simulator and the wiremock is never called.
#[tokio::test]
async fn credentialed_dedicated_arm_in_mock_never_calls_upstream() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let upstream = MockServer::start().await;
    // Any request to the real upstream is a regression. `.expect(0)` asserts
    // the simulator handled the request and the upstream was never touched.
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "gpt-4o",
            "created": 0,
            "message": {"role": "assistant", "content": "REAL UPSTREAM REACHED"},
            "done": true,
        })))
        .expect(0)
        .mount(&upstream)
        .await;

    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key()];
        state.provider_connections = vec![ProviderConnection {
            id: "ollama-conn-1".into(),
            provider: "ollama".into(),
            auth_type: "api_key".into(),
            is_active: Some(true),
            // A real credential, so `select_connection` keeps this connection:
            // mock must win even when credentials are present.
            api_key: Some("sk-ollama-test".into()),
            default_model: Some("ollama/gpt-4o".into()),
            provider_specific_data: BTreeMap::from([(
                "baseUrl".to_string(),
                Value::String(upstream.uri()),
            )]),
            ..ProviderConnection::default()
        }];
    })
    .await
    .expect("seed db");
    let state = AppState::new(db);
    set_mode(&state, "ollama", ProviderExecutionMode::Mock);

    let app = openproxy::build_app(state);
    let (status, body) = post_chat(app, chat_body("ollama/gpt-4o")).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "mock ollama must return a simulated envelope, got: {body}"
    );
    assert_eq!(body["object"], "chat.completion", "body: {body}");
    let id = body["id"].as_str().unwrap_or_default();
    assert!(
        id.starts_with("chatcmpl-sim-"),
        "expected simulated id prefix, got: {id}"
    );
    let content = body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default();
    assert!(
        !content.contains("REAL UPSTREAM REACHED"),
        "response must come from the simulator, not the real upstream: {content}"
    );
    // wiremock's `.expect(0)` fails the test (on drop/verify) if the real
    // upstream received any request; reaching here with 3 passing asserts and
    // a clean drop proves zero upstream calls.
}
