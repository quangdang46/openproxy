#![allow(clippy::await_holding_lock)]
//! openproxy-fnss: media / embeddings parity with 9router.
//!
//! One file per topic, so this carries the embeddings surface the audit
//! flagged: upstream status passthrough, the `[n]: message` error text, the
//! `!body.input` gate, the unsupported-provider 400, and the single-call rule
//! on a 401 with no refresh token. Plus the video selection-level behaviour
//! that needs no upstream at all.
//!
//! The video request/response shaping — the error envelope and its 2000-char
//! cap, `Accept`/`Content-Type` per verb, the 120 s deadline, the connection
//! pin — lives in `#[cfg(test)] mod tests` beside `src/server/api/media.rs`.
//! It cannot be reached from here: `XAI_VIDEO_BASE_URL` is a public constant
//! and the video routes take no per-connection base URL, so an end-to-end
//! create/poll would bill and bill-visible traffic against api.x.ai.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request as MockRequest, Respond, ResponseTemplate};

fn active_key(key: &str) -> ApiKey {
    ApiKey {
        id: format!("{key}-id"),
        name: "Local".into(),
        key: key.into(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        extra: BTreeMap::new(),
        monthly_budget_usd: None,
    }
}

fn connection(id: &str, provider: &str, key: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: provider.into(),
        auth_type: "apikey".into(),
        name: Some(id.into()),
        is_active: Some(true),
        api_key: Some(key.into()),
        ..Default::default()
    }
}

/// Base state for a media route: one API key, auth off (the gate is not what
/// these tests are about, and a `db.update` does not rebuild the key map).
async fn app_state_with(connections: Vec<ProviderConnection>) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
        state.settings.require_login = false;
        state.settings.require_api_key = Some(false);
        state.provider_connections = connections;
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

/// A custom-embedding connection whose traffic a mock can answer. The named
/// providers (`openai`, `jina-ai`, …) carry a hardcoded endpoint in
/// `base.rs`, so only the `custom-embedding-*` node adapter resolves its URL
/// from `baseUrl` — and so only it can be pointed at a mock.
fn embedding_connection(id: &str, provider: &str, key: &str, base: &str) -> ProviderConnection {
    let mut conn = connection(id, provider, key);
    conn.provider_specific_data
        .insert("baseUrl".into(), json!(base));
    conn
}

async fn response_json(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

fn embeddings_request(body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/v1/embeddings")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn post_embeddings(app: axum::Router, body: Value) -> (StatusCode, Value) {
    response_json(app.oneshot(embeddings_request(body)).await.unwrap()).await
}

async fn embed_upstream(status: u16, body: Value) -> MockServer {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&upstream)
        .await;
    upstream
}

// ── Embeddings: upstream status and error text (findings 1 + 5) ───────────

/// The provider's own status reaches the client.
///
/// `json_error_response` runs `infer_status_from_message`, which substring-
/// matches the error TEXT: a 400 whose body says "Incorrect API key provided"
/// came back as 401, and a 500 saying "quota exceeded" came back as 403. The
/// status also drives account rotation, so a mislabel cost an extra upstream
/// call on a request that was never an auth failure.
#[tokio::test]
async fn embeddings_upstream_status_is_passed_through_verbatim() {
    // Both texts trip an infer_status_from_message arm, in opposite directions.
    for (upstream_status, message, expected_type) in [
        (400, "Incorrect API key provided", "invalid_request_error"),
        (500, "quota exceeded", "server_error"),
    ] {
        let upstream =
            embed_upstream(upstream_status, json!({ "error": { "message": message } })).await;
        let state = app_state_with(vec![embedding_connection(
            "conn",
            "custom-embedding-a",
            "sk-x",
            &upstream.uri(),
        )])
        .await;

        let (status, body) = post_embeddings(
            openproxy::build_app(state),
            json!({"model": "custom-embedding-a/some-embed", "input": "hi"}),
        )
        .await;

        assert_eq!(
            status.as_u16(),
            upstream_status,
            "upstream {upstream_status} surfaced as {status} for message {message:?}"
        );
        assert_eq!(
            body["error"]["type"], expected_type,
            "the type must follow the status, not the text: {body}"
        );
    }
}

/// The message is 9router's extracted `error.message` behind a `[n]: ` marker,
/// not the raw upstream JSON body — which leaked provider-internal field names
/// and broke every client that matches on the message.
#[tokio::test]
async fn embeddings_error_message_is_extracted_and_status_tagged() {
    for (upstream_body, expected) in [
        (
            json!({"error": {"message": "Incorrect API key provided", "type": "invalid_request_error"}}),
            "[400]: Incorrect API key provided",
        ),
        (json!({"message": "nope"}), "[400]: nope"),
    ] {
        let upstream = embed_upstream(400, upstream_body).await;
        let state = app_state_with(vec![embedding_connection(
            "conn",
            "custom-embedding-a",
            "sk-x",
            &upstream.uri(),
        )])
        .await;

        let (status, body) = post_embeddings(
            openproxy::build_app(state),
            json!({"model": "custom-embedding-a/some-embed", "input": "hi"}),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], json!(expected));
    }
}

/// A non-JSON upstream body passes through as the message, still tagged.
#[tokio::test]
async fn embeddings_error_message_survives_a_non_json_body() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_string("upstream exploded"))
        .mount(&upstream)
        .await;
    let state = app_state_with(vec![embedding_connection(
        "conn",
        "custom-embedding-a",
        "sk-x",
        &upstream.uri(),
    )])
    .await;

    let (status, body) = post_embeddings(
        openproxy::build_app(state),
        json!({"model": "custom-embedding-a/some-embed", "input": "hi"}),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["message"], json!("[400]: upstream exploded"));
}

// ── Embeddings: the `!body.input` gate (findings 9 + 18) ──────────────────

/// 9router `if (!body.input)` is a JS falsy test run BEFORE model resolution,
/// so `""`, `null`, `0` and `false` are all "missing" — and the rejection costs
/// no model lookup, no credential lookup and no network call. `[]` is truthy in
/// JS and must still reach the provider.
#[tokio::test]
async fn falsy_embedding_input_is_rejected_before_any_upstream_call() {
    let upstream = embed_upstream(
        200,
        json!({
            "object": "list",
            "data": [{"object": "embedding", "index": 0, "embedding": [0.1]}],
        }),
    )
    .await;
    let state = app_state_with(vec![embedding_connection(
        "conn",
        "custom-embedding-a",
        "sk-x",
        &upstream.uri(),
    )])
    .await;

    for input in [json!(""), Value::Null, json!(0), json!(false)] {
        let (status, body) = post_embeddings(
            openproxy::build_app(state.clone()),
            json!({"model": "custom-embedding-a/some-embed", "input": input}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "input {input} must be rejected as missing"
        );
        assert_eq!(
            body["error"]["message"],
            json!("Missing required field: input"),
            "input {input} produced the wrong message: {body}"
        );
    }

    assert_eq!(
        upstream.received_requests().await.unwrap().len(),
        0,
        "a rejected input must not reach the provider"
    );
}

/// The same gate on a provider with NO embedding adapter. That path validated
/// nothing and POSTed the request (and the API key) to a synthesised
/// `.../chat/completions/embeddings` URL.
#[tokio::test]
async fn falsy_embedding_input_is_rejected_on_the_non_adapter_path() {
    let upstream = embed_upstream(200, json!({})).await;
    // `anthropic` has no embedding adapter, so this takes the generic
    // fall-through path.
    let state = app_state_with(vec![embedding_connection(
        "conn",
        "anthropic",
        "sk-x",
        &upstream.uri(),
    )])
    .await;

    let (status, body) = post_embeddings(
        openproxy::build_app(state),
        json!({"model": "anthropic/claude-3-5-haiku", "input": ""}),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"]["message"],
        json!("Missing required field: input")
    );
    assert_eq!(
        upstream.received_requests().await.unwrap().len(),
        0,
        "the non-adapter path forwarded a request with no input"
    );
}

/// An empty array is truthy in JS and must still be embedded.
#[tokio::test]
async fn an_empty_input_array_still_reaches_the_provider() {
    let upstream = embed_upstream(
        200,
        json!({
            "object": "list",
            "data": [],
            "usage": {"prompt_tokens": 0, "total_tokens": 0},
        }),
    )
    .await;
    let state = app_state_with(vec![embedding_connection(
        "conn",
        "custom-embedding-a",
        "sk-x",
        &upstream.uri(),
    )])
    .await;

    let (status, _body) = post_embeddings(
        openproxy::build_app(state),
        json!({"model": "custom-embedding-a/some-embed", "input": []}),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "[] is truthy in JS and must pass");
    assert_eq!(upstream.received_requests().await.unwrap().len(), 1);
}

// ── Embeddings: unsupported provider (findings 25 + 26) ───────────────────

/// 9router refuses a provider with no embedding adapter up front
/// (`Provider 'X' does not support embeddings.`). Both cases here leaked the
/// caller's text and Bearer key: `venice` by POSTing to
/// `.../chat/completions/embeddings`, and an id in no registry at all by
/// POSTing to a synthesised `https://api.<provider>.com/v1/embeddings`.
#[tokio::test]
async fn a_provider_without_an_embedding_adapter_is_refused_locally() {
    let upstream = embed_upstream(200, json!({})).await;

    for provider in ["venice", "not-a-provider"] {
        let state = app_state_with(vec![embedding_connection(
            "conn",
            provider,
            "sk-secret",
            &upstream.uri(),
        )])
        .await;

        let (status, body) = post_embeddings(
            openproxy::build_app(state),
            json!({"model": format!("{provider}/some-embed"), "input": "secret text"}),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "for {provider}: {body}");
        assert_eq!(
            body["error"]["message"],
            json!(format!(
                "Provider '{provider}' does not support embeddings."
            ))
        );
    }

    assert_eq!(
        upstream.received_requests().await.unwrap().len(),
        0,
        "an unsupported provider must not be sent the request or the key"
    );
}

// ── Embeddings: one call per failed auth (findings 7 + 8) ─────────────────

/// Counts hits and always answers 401.
struct Counting401 {
    hits: Arc<std::sync::atomic::AtomicUsize>,
}

impl Respond for Counting401 {
    fn respond(&self, _: &MockRequest) -> ResponseTemplate {
        self.hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        ResponseTemplate::new(401).set_body_json(json!({"error": {"message": "revoked"}}))
    }
}

/// An API-key provider has no refresh token, so 9router's core never re-fires
/// (embeddingsCore.js:88 gates the retry on new credentials). The handler
/// re-POSTed the identical request with the same rejected key, doubling the
/// upstream call count on every auth failure.
#[tokio::test]
async fn a_401_on_an_api_key_provider_costs_exactly_one_upstream_call() {
    for provider in ["custom-embedding-a", "custom-embedding-b"] {
        let upstream = MockServer::start().await;
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        Mock::given(method("POST"))
            .respond_with(Counting401 { hits: hits.clone() })
            .mount(&upstream)
            .await;
        let state = app_state_with(vec![embedding_connection(
            "conn",
            provider,
            "sk-bad",
            &upstream.uri(),
        )])
        .await;

        let (status, _body) = post_embeddings(
            openproxy::build_app(state),
            json!({"model": format!("{provider}/some-embed"), "input": "hi"}),
        )
        .await;

        assert_eq!(status, StatusCode::UNAUTHORIZED, "for {provider}");
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "{provider} has no refresh token, so 9router issues one upstream call"
        );
    }
}

// ── Video: account cooldown and Retry-After (findings 13 + 22) ────────────

fn video_request(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"model": "xai/grok-imagine-video", "prompt": "a cat"}).to_string(),
        ))
        .unwrap()
}

/// When every account for a provider is cooling down, the create route answers
/// 429 with `Retry-After` (9router `unavailableResponse`,
/// open-sse/utils/error.js:116-129) instead of re-hitting a known-dead
/// account. The old selection consulted no cooldown state at all, so a
/// rate-limited account was retried on every request until the operator
/// noticed.
///
/// No upstream mock is needed: the request must never leave the process.
#[tokio::test]
async fn a_fully_cooled_down_video_account_set_answers_503_with_retry_after() {
    let mut cooled = connection("conn-xai", "xai", "sk-xai");
    cooled.rate_limited_until =
        Some((chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc3339());
    let state = app_state_with(vec![cooled]).await;

    let response = openproxy::build_app(state)
        .oneshot(video_request("/v1/videos/generations"))
        .await
        .unwrap();

    // 9router videoGeneration.js:139-142 — the all-rate-limited arm calls
    // unavailableResponse with `lastStatus || lastErrorCode || SERVICE_UNAVAILABLE`.
    // Nothing attempted a request, so there is no upstream 429 to echo back;
    // 429 is not a status this route can invent.
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .expect("Retry-After must tell the client when to come back")
        .to_string();
    let seconds: i64 = retry_after
        .parse()
        .expect("Retry-After is a delta-seconds value");
    assert!(
        (1..=120).contains(&seconds),
        "Retry-After {seconds} should point inside the cooldown window"
    );
}

/// A provider with no usable account at all is still a 400, not a 429 — the
/// two are different operator problems (configure an account vs wait).
#[tokio::test]
async fn a_video_provider_with_no_account_is_a_plain_400() {
    let state = app_state_with(vec![]).await;
    let response = openproxy::build_app(state)
        .oneshot(video_request("/v1/videos/generations"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let (_status, body) = response_json(response).await;
    assert_eq!(
        body["error"]["message"],
        json!("No credentials for provider: xai")
    );
}

/// The other half of 9router's precedence: when a previous attempt DID record
/// an error code, that code is the status — not 503.
///
/// The 503 case above is the cold path (nothing attempted). This is the warm
/// one: a connection left cooling down after a real 402 must answer 402, or a
/// client cannot tell a billing problem from a scheduling one and will retry
/// the wrong thing.
#[tokio::test]
async fn a_cooled_down_account_echoes_its_recorded_error_code_as_the_status() {
    let mut cooled = connection("conn-xai", "xai", "sk-xai");
    cooled.rate_limited_until =
        Some((chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc3339());
    cooled.error_code = Some("402".to_string());
    let state = app_state_with(vec![cooled]).await;

    let response = openproxy::build_app(state)
        .oneshot(video_request("/v1/videos/generations"))
        .await
        .unwrap();

    assert_eq!(
        response.status().as_u16(),
        402,
        "9router: `lastStatus || Number(credentials.lastErrorCode) || 503`"
    );
}

/// `Number("")` is 0 in JS, and 0 is falsy, so an empty error code must fall
/// through to 503 rather than become status 0. A recorded code that is not a
/// number at all (a slug, an upstream error name) behaves the same way.
#[tokio::test]
async fn an_unusable_recorded_error_code_falls_back_to_503() {
    for code in ["", "   ", "not-a-number", "0"] {
        let mut cooled = connection("conn-xai", "xai", "sk-xai");
        cooled.rate_limited_until =
            Some((chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc3339());
        cooled.error_code = Some(code.to_string());
        let state = app_state_with(vec![cooled]).await;

        let response = openproxy::build_app(state)
            .oneshot(video_request("/v1/videos/generations"))
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "error_code {code:?} must not be used as a status"
        );
    }
}

/// The message names the provider and carries the real recorded error, per
/// 9router `[${provider}/${model || "video"}] ${lastError}`. The old canned
/// "All accounts are rate limited; retry later" said nothing about which
/// upstream failed or why.
#[tokio::test]
async fn the_unavailable_message_carries_the_recorded_error() {
    let mut cooled = connection("conn-xai", "xai", "sk-xai");
    cooled.rate_limited_until =
        Some((chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc3339());
    cooled.last_error = Some("monthly credit exhausted".to_string());
    let state = app_state_with(vec![cooled]).await;

    let response = openproxy::build_app(state)
        .oneshot(video_request("/v1/videos/generations"))
        .await
        .unwrap();

    let body = String::from_utf8_lossy(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body"),
    )
    .into_owned();

    assert!(
        body.contains("monthly credit exhausted"),
        "the recorded error must reach the client, got: {body}"
    );
    assert!(
        body.contains("[xai/"),
        "the message must name the provider and route, got: {body}"
    );
}

/// A multipart body on /v1/videos/generations must not be rejected as invalid
/// JSON — that is an image-to-video client, and 9router forwards those exact
/// bytes because re-encoding FormData would change the multipart boundary
/// (`readForwardableBody`, videoGeneration.js:60-76).
///
/// The old route took `Json<Value>`, so axum rejected the body during
/// extraction and the client got "Invalid JSON body" — the feature was
/// unreachable rather than degraded. The assertion is therefore about NOT being
/// rejected: the request gets past the body layer and fails on account
/// selection instead, which is the next stage down.
#[tokio::test]
async fn a_multipart_video_generation_body_is_not_rejected_as_invalid_json() {
    let state = app_state_with(vec![]).await;
    let boundary = "----openproxyTestBoundary";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nxai/grok-imagine-video\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"a.png\"\r\n\
         Content-Type: image/png\r\n\r\n\r\n--{boundary}--\r\n"
    );

    let response = openproxy::build_app(state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/videos/generations")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    let payload = String::from_utf8_lossy(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body"),
    )
    .into_owned();

    assert!(
        !payload.contains("Invalid JSON body"),
        "multipart was rejected by the JSON extractor; the raw arm is not reached: {payload}"
    );
    // The body must have reached ACCOUNT SELECTION, not the JSON extractor.
    // With no connections configured that is a 400 no-credentials, which is
    // what proves the request got past body handling.
    assert!(
        payload.contains("No credentials for provider"),
        "multipart never reached account selection: {payload}"
    );
}

/// The JSON arm of the same route must still work — sharing the handler with
/// edits/extensions must not have cost the JSON path its body handling.
///
/// With no accounts configured this correctly answers 400 "No credentials"
/// (see `a_video_provider_with_no_account_is_a_plain_400`), so the status is
/// NOT the assertion. What matters is that the body was parsed and the request
/// reached account selection, rather than dying in the JSON extractor — which
/// is the same "Invalid JSON body" string the multipart test forbids.
#[tokio::test]
async fn a_json_video_generation_body_still_reaches_account_selection() {
    let state = app_state_with(vec![]).await;
    let response = openproxy::build_app(state)
        .oneshot(video_request("/v1/videos/generations"))
        .await
        .unwrap();

    let status = response.status();
    let payload = String::from_utf8_lossy(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body"),
    )
    .into_owned();

    assert!(
        !payload.contains("Invalid JSON body"),
        "valid JSON must parse and reach account selection: {payload}"
    );
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "no accounts configured, so account selection is where it stops: {payload}"
    );
}

// ── Video: verbatim body forwarding (finding 6) ────────────────────────────

/// The model actually forwarded upstream for `xai/grok-imagine-video`.
const XAI_VIDEO_MODEL: &str = "grok-imagine-video";

/// Run `f` with OPENPROXY_XAI_VIDEO_URL pointed at `uri`, restoring it after.
///
/// The override is process-wide, so the restore matters: another test must not
/// inherit a mock URL. The lock serialises the read-modify-write against other
/// tests in this binary that touch the same variable.
async fn with_video_base_url<T>(uri: &str, f: impl std::future::Future<Output = T>) -> T {
    use std::sync::Mutex;
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let previous = std::env::var("OPENPROXY_XAI_VIDEO_URL").ok();
    // Process-global; the lock above is what makes the window safe, and the
    // restore below is what stops the next test inheriting a mock URL.
    std::env::set_var("OPENPROXY_XAI_VIDEO_URL", uri);
    let out = f.await;
    match previous {
        Some(v) => std::env::set_var("OPENPROXY_XAI_VIDEO_URL", v),
        None => std::env::remove_var("OPENPROXY_XAI_VIDEO_URL"),
    }
    out
}

/// THE WIRE-LEVEL GUARD for finding 6, and the one the previous commit said was
/// impossible.
///
/// "Forwards the caller's bytes untouched" is a statement about what the UPSTREAM
/// received. A unit test that compares a copy of the decision expression cannot
/// establish it — I shipped exactly that and it would have passed even if the
/// handler stopped computing the flag. This asserts the bytes on the wire.
///
/// The override exists for exactly this: the base was a const, so every video
/// test stopped at account selection before the forward ever happened.
#[tokio::test]
async fn a_rewritten_video_body_is_reserialised_with_the_prefix_stripped() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/videos/generations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "job-1"})))
        .expect(1)
        .mount(&upstream)
        .await;

    let state = app_state_with(vec![connection("conn-xai", "xai", "sk-xai")]).await;

    // Deliberately NOT alphabetical key order. If the body were re-serialised
    // from a serde_json::Value (a BTreeMap) this order would be lost — which is
    // exactly what the control below establishes.
    let raw = br#"{"zeta":1,"model":"xai/grok-imagine-video","prompt":"a cat","alpha":2}"#;

    let (status, body) = with_video_base_url(&format!("{}/v1/videos", upstream.uri()), async {
        let response = openproxy::build_app(state)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/videos/generations")
                    .header("content-type", "application/json")
                    .body(Body::from(raw.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_default();
        (status, bytes.to_vec())
    })
    .await;

    assert!(
        status.is_success(),
        "forward failed: {status} {:?}",
        String::from_utf8_lossy(&body)
    );

    let sent = upstream
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .next()
        .expect("upstream must have received the request");

    let text = String::from_utf8_lossy(&sent.body).into_owned();
    assert!(
        !text.contains("xai/"),
        "the provider prefix must be stripped for the upstream: {text}"
    );
    assert!(
        text.contains(XAI_VIDEO_MODEL),
        "the bare model id must reach the upstream: {text}"
    );
    // A REWRITTEN body goes through serde_json::to_vec, and serde_json::Value is
    // a BTreeMap — so keys come out ALPHABETICAL. Asserted in that direction;
    // my first version checked the caller's order here, which inverted the
    // meaning of the test and failed against correct behaviour.
    let alphabetical = ["alpha", "model", "prompt", "zeta"];
    let positions: Vec<usize> = alphabetical
        .iter()
        .map(|k| text.find(&format!("\"{k}\"")).unwrap_or(usize::MAX))
        .collect();
    assert!(
        positions.windows(2).all(|w| w[0] < w[1]),
        "a rewritten body is re-serialised, so keys sort alphabetically: \
         {positions:?} in {text}"
    );
    // Which is exactly why the untouched case below is the one that must NOT
    // sort — it is the only one of the two that keeps the caller's order.
    assert_ne!(
        text, r#"{"zeta":1,"model":"grok-imagine-video","prompt":"a cat","alpha":2}"#,
        "this body was rewritten, so it must NOT have kept the caller's order"
    );
}

/// The untouched case: when the model needs no rewrite the ORIGINAL bytes go
/// upstream, key order intact.
///
/// This is the assertion the previous commit could not make. It fails if anything
/// routes the body through `serde_json::to_vec` on the untouched path.
#[tokio::test]
async fn a_body_needing_no_rewrite_reaches_the_upstream_in_caller_order() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/videos/generations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "job-1"})))
        .expect(1)
        .mount(&upstream)
        .await;

    let state = app_state_with(vec![connection("conn-xai", "xai", "sk-xai")]).await;

    // Model already bare — no prefix to strip, so nothing changes. Key order is
    // deliberately non-alphabetical and survives ONLY on the verbatim path.
    let raw = br#"{"zeta":1,"model":"grok-imagine-video","prompt":"a cat","alpha":2}"#;

    with_video_base_url(&format!("{}/v1/videos", upstream.uri()), async {
        let response = openproxy::build_app(state)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/videos/generations")
                    .header("content-type", "application/json")
                    .body(Body::from(raw.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let st = response.status();
        let b = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_default();
        assert!(
            st.is_success(),
            "forward failed: {st} {}",
            String::from_utf8_lossy(&b)
        );
    })
    .await;

    let sent = upstream
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .next()
        .expect("upstream must have received the request");

    assert_eq!(
        sent.body.as_slice(),
        raw,
        "an untouched body must be forwarded byte-for-byte, in the caller's order"
    );
}
