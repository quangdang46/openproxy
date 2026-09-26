//! Bead openproxy-mv0w.3 — error-message fidelity on the web-fetch and STT
//! routes.
//!
//! Two halves, both pinned to 9router source rather than to a plausible
//! reading of the chat route:
//!
//! 1. **Nothing rewrites the provider's words.** `buildErrorBody`
//!    (open-sse/utils/error.js:9-22) returns `message` as handed to it, and
//!    `createErrorResult` (error.js:98-105) feeds it a message that was never
//!    edited. The detail a caller needs to act — which organization hit a
//!    limit, which limit, which account — is exactly what a phrase→prose
//!    rewrite deletes, and the same canned sentence came back for every
//!    provider, so it could not be used to triage anything.
//!
//! 2. **"No usable credential" is not one status.** The chat route answers 404
//!    `No active credentials for provider: X` (handlers/chat.js:245-246), but
//!    the two routes here deliberately do not: web fetch answers 400
//!    `No credentials for provider: X` (handlers/fetch.js:151-152) and STT
//!    answers the same (handlers/stt.js:66). Both are pinned below so the
//!    chat route's wording is not propagated onto them by analogy.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection, Settings};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn seeded_state(connections: Vec<ProviderConnection>) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![ApiKey {
            id: "local-id".into(),
            name: "Local".into(),
            key: "valid-bearer".into(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            extra: BTreeMap::new(),
            monthly_budget_usd: None,
        }];
        state.provider_connections = connections;
        // Auth is not under test here — disable the login guard so a request
        // without a key reaches the media pipeline.
        state.settings = Settings {
            require_login: false,
            ..Settings::default()
        };
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

/// A `selfhosted-stt` account pointing at `base_url`. The per-connection
/// `baseUrl` override is the only way to aim an STT provider at a local mock
/// (`resolve_stt_config`, stt.rs:148-163); the registry URL is localhost.
fn stt_connection(id: &str, base_url: &str) -> ProviderConnection {
    let mut connection = ProviderConnection {
        id: id.into(),
        provider: "selfhosted-stt".into(),
        auth_type: "apikey".into(),
        name: Some(id.into()),
        priority: Some(1),
        is_active: Some(true),
        api_key: Some("sk-local".into()),
        ..ProviderConnection::default()
    };
    connection
        .provider_specific_data
        .insert("baseUrl".into(), json!(base_url));
    connection
}

async fn post(state: AppState, uri: &str, body: Value) -> axum::response::Response {
    openproxy::build_app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("authorization", "Bearer valid-bearer")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Read the message out of either error envelope: `{"error": "…"}` or
/// `{"error": {"message": "…"}}`. The extraction is deliberately
/// shape-agnostic so a later wave fixing one route's envelope does not have to
/// come back here to re-pin what these tests are actually about.
fn error_message(json: &Value) -> String {
    json["error"]
        .as_str()
        .or_else(|| json["error"]["message"].as_str())
        .unwrap_or_default()
        .to_string()
}

fn transcription(model: &str) -> Value {
    json!({
        "model": model,
        "file_b64": B64.encode(b"RIFF....WAVEfmt "),
        "file_name": "clip.wav",
    })
}

/// The upstream's own rate-limit sentence reaches the client with the
/// organization id and the limit still in it. Before the phrase→prose rewrite
/// it came back as "Rate limit exceeded. Wait a moment and try again, or use
/// another account." — identical for every provider and every account.
#[tokio::test]
async fn stt_upstream_error_text_reaches_the_client_verbatim() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429).set_body_json(
                json!({"error": {"message": "Rate limit reached for whisper-large in organization org-abc on requests per day"}}),
            ),
        )
        .mount(&upstream)
        .await;

    let state = seeded_state(vec![stt_connection("conn-1", &upstream.uri())]).await;
    let response = post(
        state,
        "/v1/audio/transcriptions",
        transcription("selfhosted-stt/whisper-large"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let message = error_message(&body_json(response).await);
    assert_eq!(
        message, "Rate limit reached for whisper-large in organization org-abc on requests per day",
        "the provider's own text is the only thing that names the account and the limit"
    );
}

/// A provider that has never been configured is a bad request to the fetch
/// route, not a 404: 9router handlers/fetch.js:151-152 answers
/// `errorResponse(BAD_REQUEST, "No credentials for provider: …")`, and the
/// route never gained the chat handler's 404.
#[tokio::test]
async fn web_fetch_without_credentials_answers_400_no_credentials() {
    let state = seeded_state(Vec::new()).await;
    let response = post(
        state,
        "/v1/web/fetch",
        json!({"provider": "firecrawl", "url": "https://example.com/a"}),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "handlers/fetch.js:151 — the fetch route answers 400, not the chat route's 404"
    );
    assert_eq!(
        error_message(&body_json(response).await),
        "No credentials for provider: firecrawl"
    );
}

/// Same answer on STT, and for the same reason: handlers/stt.js:66 branches on
/// `excludeConnectionIds.size === 0` and returns BAD_REQUEST with the "No
/// credentials" wording, not the chat handler's "No active credentials".
#[tokio::test]
async fn stt_without_credentials_answers_400_no_credentials() {
    let state = seeded_state(Vec::new()).await;
    let response = post(
        state,
        "/v1/audio/transcriptions",
        transcription("selfhosted-stt/whisper-large"),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "handlers/stt.js:66 — STT answers 400, not the chat route's 404"
    );
    assert_eq!(
        error_message(&body_json(response).await),
        "No credentials for provider: selfhosted-stt"
    );
}
