//! POST /api/providers/validate must not rubber-stamp a key it never checked.
//!
//! The route's catch-all arm returned `{valid: true, error: null}` for every
//! provider without an explicit case — no HTTP status change, no request sent.
//! 42 provider ids reached it, first-class ones included (`kilocode`, `cline`,
//! `venice`, `github-models`). `AddApiKeyModal` derives `isValid` from
//! `!!data.valid` and persists the outcome as `testStatus: "active"`, so a
//! mistyped provider id or a key the upstream rejects was stored with a green
//! badge. Commit 803fa6c0 introduced the pass-through deliberately, to stop
//! newly added free providers needing an explicit case; the cost was that the
//! gate stopped existing at all.
//!
//! 9router's `default:` arm is not a stub. It looks the provider up in
//! `PROVIDERS` and probes the entry when its declared transport format is
//! `"openai"` (the registry barrel defaults format-less entries to it,
//! `open-sse/providers/index.js:14`), and only a provider declaring another
//! transport, or missing from the registry, gets HTTP 400
//! `{error: "Provider validation not supported"}` (route.js:599-604).
//!
//! One assertion per direction, because the two failure modes are opposites:
//! under-gating reports a bad key as good, over-gating refuses a provider the
//! product serves. A test that only checked "unknown ids are rejected" would
//! still pass with a blanket 400.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use jsonwebtoken::{encode, EncodingKey, Header};
use openproxy::db::Db;
use openproxy::server::auth::{generate_jti, jwt_secret};
use openproxy::server::state::AppState;
use serde::Serialize;
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

#[derive(Debug, Serialize)]
struct DashboardClaims {
    authenticated: bool,
    exp: usize,
    jti: String,
}

/// Mint a dashboard JWT the way the login/OIDC/SAML issuers do — signed with the
/// shared secret and carrying a jti, which the session gate rejects without.
fn cookie() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as usize;
    let token = encode(
        &Header::default(),
        &DashboardClaims {
            authenticated: true,
            exp: now + 3600,
            jti: generate_jti(),
        },
        &EncodingKey::from_secret(jwt_secret().as_bytes()),
    )
    .expect("dashboard token");
    format!("auth_token={token}")
}

async fn post(provider: &str, api_key: &str) -> (StatusCode, Value) {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    let app = openproxy::build_app(AppState::new(db));

    let request = Request::builder()
        .method("POST")
        .uri("/api/providers/validate")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, cookie())
        .body(Body::from(
            json!({ "provider": provider, "apiKey": api_key }).to_string(),
        ))
        .expect("request");

    let response = app.oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The regression itself. Before the gate these ids walked the match to the
/// catch-all and came back 200 `{valid: true}`, which the modal stored as an
/// active connection.
#[tokio::test]
async fn mistyped_provider_is_rejected_as_unsupported() {
    for provider in ["not-a-provider", "typo-opanai", "gpt-4o"] {
        let (status, body) = post(provider, "sk-test-key").await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{provider:?} has no registry entry, so it must not be probed"
        );
        assert_eq!(
            body["error"], "Provider validation not supported",
            "{provider:?} must carry the 9router error string"
        );
        // 9router's 400 body has no `valid` key at all, and `!!data.valid` on
        // it is false — that is what drives the modal to `testStatus: unknown`.
        assert!(
            body.get("valid").is_none(),
            "{provider:?} must not report a validity verdict: {body}"
        );
    }
}

/// A provider whose 9router registry entry declares a transport other than
/// `"openai"` reaches the same 400, by a different condition.
#[tokio::test]
async fn non_openai_transport_provider_is_rejected_as_unsupported() {
    for provider in ["cursor", "perplexity-agent", "antigravity"] {
        let (status, body) = post(provider, "sk-test-key").await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{provider:?} declares a non-openai transport in the 9router registry"
        );
        assert_eq!(body["error"], "Provider validation not supported");
    }
}

/// The opposite failure. These declare `format: "openai"` by default in the
/// 9router registry, so 9router probes them; a blanket 400 would refuse a
/// provider the product serves. `vllm` is loopback, so its probe fails fast and
/// without leaving the machine — what matters is that the verdict is a probe
/// result rather than a refusal to validate.
#[tokio::test]
async fn openai_format_provider_is_probed_rather_than_refused() {
    let (status, body) = post("vllm", "sk-test-key").await;
    assert_ne!(
        status,
        StatusCode::BAD_REQUEST,
        "vllm is openai-format and must be probed, not refused: {body}"
    );
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["valid"].is_boolean(),
        "a probed provider reports a validity verdict: {body}"
    );
}

/// The over-gating guard for media providers. `topaz` is media-only and its
/// 9router entry declares no probe config, so `probeMediaProvider` short-circuits
/// to `true` (route.js:61-62). It must not be turned away — a 400 here would
/// report a provider the product serves as unsupported, and the key is never
/// sent anywhere.
#[tokio::test]
async fn known_media_provider_is_accepted_without_a_request() {
    let (status, body) = post("topaz", "sk-test-key").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["valid"],
        json!(true),
        "topaz short-circuits to valid: {body}"
    );
    assert!(
        body["error"].is_null(),
        "a passing verdict carries no error: {body}"
    );
}

/// `mimo-free` declares `noAuth: true` (registry/mimo-free.js:17), so 9router
/// returns `isValid = true` without contacting it (route.js:605-608). The
/// registry's default arm would otherwise send it a bearer probe.
#[tokio::test]
async fn no_auth_provider_is_accepted_without_a_request() {
    let (status, body) = post("mimo-free", "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["valid"],
        json!(true),
        "mimo-free needs no credential: {body}"
    );
}
