//! Bead openproxy-xq7a — STT error-status parity.
//!
//! 9router's `errorResponse` (open-sse/utils/error.js:30-38) answers with the
//! status it is handed and the message it is handed. The status is an input to
//! `buildErrorBody` (error.js:9-21), never re-derived from the text — so
//! `type` and `code` follow the status the caller chose, and nothing else.
//!
//! OpenProxy's STT route ran every local error through a substring rewriter
//! first, so the status a client saw was a guess about the sentence rather than
//! the one the handler produced:
//!
//! * `Combos not supported for audio/transcriptions` — a 400 malformed-request
//!   answer — matched `"not supported"` and came back **406**.
//! * An upstream 400 `insufficient balance` matched `"insufficient"` and came
//!   back **403**, indistinguishable from a credential rejection.
//!
//! Both are pinned below through the real router, so the assertion is about
//! what a client receives rather than about a helper's return value.

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
        // without a key reaches the STT pipeline.
        state.settings = Settings {
            require_login: false,
            ..Settings::default()
        };
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

/// A `selfhosted-stt` account pointed at a local mock. The per-connection
/// `baseUrl` override is the only way to aim an STT provider at a mock
/// (`resolve_stt_config`, stt.rs:148-163).
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

fn transcription(model: &str) -> Value {
    json!({
        "model": model,
        "file_b64": B64.encode(b"RIFF....WAVEfmt "),
        "file_name": "clip.wav",
    })
}

/// A combo has no single provider to dispatch to, so the route rejects it. The
/// rejection is a malformed request — 400 — and the sentence says "not
/// supported", which is exactly what the rewriter keyed on.
#[tokio::test]
async fn stt_combo_model_answers_400_not_406() {
    let state = seeded_state(Vec::new()).await;
    let response = post(
        state,
        "/v1/audio/transcriptions",
        transcription("combo:any-name"),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "handlers/stt.js:43 answers BAD_REQUEST; a combo has no provider to \
         dispatch to, and nothing about the sentence turns that into 406"
    );
    let body = body_json(response).await;
    assert_eq!(body["error"]["type"], "invalid_request_error");
}

/// The upstream chose 400 and meant 400. `insufficient balance` read as a quota
/// problem, so the client was told 403 — the same status a rejected credential
/// produces, and the wrong one to retry against.
#[tokio::test]
async fn stt_upstream_400_keeps_its_status() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"error": {"message": "insufficient balance"}})),
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

    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "error.js:86 captures the upstream status and :98-106 passes it \
         through; a 400 that happens to mention a balance is still a 400"
    );
    let body = body_json(response).await;
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["message"], "insufficient balance");
}

/// The other direction: a rate-limit sentence arriving on a 400 must not be
/// promoted to 429. 9router's chat handler already has a dedicated
/// cooling-down body for that case, and it is written where the status is
/// chosen — not inferred after the fact.
#[tokio::test]
async fn stt_upstream_400_rate_limit_text_stays_400() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({"error": {"message": "Rate limit reached for whisper-large in organization org-abc"}}),
        ))
        .mount(&upstream)
        .await;

    let state = seeded_state(vec![stt_connection("conn-1", &upstream.uri())]).await;
    let response = post(
        state,
        "/v1/audio/transcriptions",
        transcription("selfhosted-stt/whisper-large"),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "the status the upstream chose is the status the client gets"
    );
    assert_eq!(
        body_json(response).await["error"]["message"],
        "Rate limit reached for whisper-large in organization org-abc"
    );
}
