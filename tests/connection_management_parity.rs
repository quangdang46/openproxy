//! Bead openproxy-erbh — connection management parity with 9router.
//!
//! Two server-side halves that a Rust test can prove:
//!
//! 1. `POST /api/providers` accepted any non-empty `provider` string. A typo
//!    (`openai-compatibble`) or a bare OAuth id created a connection that was
//!    stored active, dispatched to nothing, and sat in the list looking healthy
//!    until someone routed a model through it. 9router gates on
//!    `isValidProvider` (route.js:104-118) and resolves a `*-compatible-` node
//!    before building the row (route.js:131-162).
//! 2. The provider-detail breadcrumb built its icon by string interpolation
//!    (`/providers/${providerId}.png`), bypassing `getProviderIconSrc` — so
//!    the `ICON_ALIASES` table and the session 404 cache that `ProviderIcon`
//!    feeds were written and never read.
//!
//! The soft-success persistence half of this bead (9router's `softWarning`
//! rule) is unit-tested next to the code, in
//! `src/server/api/provider_connection_test.rs::tests` — the grok-cli probe URL
//! is a hardcoded constant, so wiremock cannot redirect it to a 402.
//!
//! `Header.tsx` is a `"use client"` React component importing zustand and the
//! i18n runtime with no DOM renderer available, so the icon wiring is covered by
//! source-contract assertions rather than execution — the same call
//! `tests/available_models_disabled.rs` makes for the provider-page wiring.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderNode};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

const BEARER: &str = "conn-mgmt-bearer";

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

async fn app_state(nodes: Vec<ProviderNode>) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key(BEARER)];
        state.provider_nodes = nodes;
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

fn node(id: &str, name: &str) -> ProviderNode {
    ProviderNode {
        id: id.into(),
        r#type: "openai-compatible".into(),
        name: name.into(),
        prefix: Some("local".into()),
        api_type: Some("chat".into()),
        base_url: Some("https://node.example/v1".into()),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

async fn create(state: &AppState, body: Value) -> (StatusCode, Value) {
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/providers")
                .header("authorization", format!("Bearer {BEARER}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// The connection just created, read back out of the store rather than echoed
/// from the response — the response body is redacted.
fn created_connection(state: &AppState, provider: &str) -> Value {
    let snapshot = state.db.snapshot();
    let connection = snapshot
        .provider_connections
        .iter()
        .find(|connection| connection.provider == provider)
        .unwrap_or_else(|| panic!("a {provider} connection was created"));
    serde_json::to_value(connection).expect("serialize connection")
}

fn created_psd(state: &AppState, provider: &str) -> Value {
    created_connection(state, provider)["providerSpecificData"].clone()
}

// ── isValidProvider ─────────────────────────────────────────────────

/// The typo case. `openai-compatibble` is one character off a real prefix, so
/// the request looks valid to a user and would otherwise create a connection
/// with no baseUrl at all.
#[tokio::test]
async fn unknown_provider_id_is_rejected_with_400() {
    let state = app_state(vec![]).await;
    let (status, json) = create(
        &state,
        json!({ "provider": "openai-compatibble", "name": "Typo", "apiKey": "sk-x" }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "Invalid provider");
    // OpenProxy's envelope, not 9router's bare `{ error }`.
    assert_eq!(json["success"], false);
    assert!(
        state.db.snapshot().provider_connections.is_empty(),
        "the rejected row must not reach the store"
    );
}

/// OAuth-only ids are absent from every api-key table, so 9router rejects them
/// here too — `github` reaches the database through the OAuth device flow.
#[tokio::test]
async fn oauth_only_provider_id_is_rejected() {
    let state = app_state(vec![]).await;
    let (status, json) = create(
        &state,
        json!({ "provider": "github", "name": "Copilot", "apiKey": "sk-x" }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "Invalid provider");
}

/// The guard against over-tightening. A registry narrower than the dashboard's
/// would make every Add Connection fail, which is the worse regression.
#[tokio::test]
async fn registry_providers_are_accepted() {
    // `codebuddy-cn` and `xai` sit in the oauth category but declare
    // `authModes: ["oauth", "apikey"]`; `qoder` is free-tier. None of them is
    // in a plain api-key table, so each passes only via the dual-auth arm.
    for provider in [
        "openai",
        "anthropic",
        "gemini",
        "codebuddy-cn",
        "xai",
        "qoder",
    ] {
        let state = app_state(vec![]).await;
        let (status, _) = create(
            &state,
            json!({ "provider": provider, "name": "Test", "apiKey": "sk-x" }),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::CREATED,
            "{provider} must be creatable through the Add Connection form"
        );
        assert_eq!(created_connection(&state, provider)["provider"], provider);
    }
}

/// The web-cookie arm is a hardcoded triple in `is_web_cookie_provider` (9router
/// derives it from an empty `webCookie` registry category, but the auth_type it
/// selects is load-bearing here). It must not be gated out.
#[tokio::test]
async fn web_cookie_providers_are_accepted_with_cookie_auth_type() {
    for provider in ["grok-web", "perplexity-web", "deepseek-web"] {
        let state = app_state(vec![]).await;
        let (status, _) = create(
            &state,
            json!({ "provider": provider, "name": "Web", "apiKey": "sso=abc" }),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED, "{provider} must be creatable");
        assert_eq!(
            created_connection(&state, provider)["authType"],
            "cookie",
            "{provider} stores a session cookie and must keep auth_type cookie"
        );
    }
}

// ── compatible-node resolution ──────────────────────────────────────

/// A `*-compatible-` connection with no node has no baseUrl, so it is created
/// active and dials nothing — the exact shape the gate exists to prevent.
#[tokio::test]
async fn compatible_prefix_without_a_node_is_404() {
    let state = app_state(vec![]).await;
    let (status, json) = create(
        &state,
        json!({ "provider": "openai-compatible-local", "name": "Local", "apiKey": "sk-x" }),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        json["error"].as_str().unwrap_or_default().contains("node"),
        "the miss must name the node it could not find, got {json}"
    );
    assert!(state.db.snapshot().provider_connections.is_empty());
}

#[tokio::test]
async fn compatible_prefix_resolves_node_fields() {
    let state = app_state(vec![node("openai-compatible-local", "Local Node")]).await;
    let (status, _) = create(
        &state,
        json!({ "provider": "openai-compatible-local", "name": "Local", "apiKey": "sk-x" }),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    let psd = created_psd(&state, "openai-compatible-local");
    assert_eq!(psd["prefix"], "local");
    assert_eq!(psd["apiType"], "chat");
    assert_eq!(psd["baseUrl"], "https://node.example/v1");
    assert_eq!(psd["nodeName"], "Local Node");
}

/// The anthropic and embedding arms carry no `apiType` — 9router omits the key
/// entirely there (route.js:144-160), rather than writing a null.
#[tokio::test]
async fn anthropic_and_embedding_arms_omit_api_type() {
    for provider in ["anthropic-compatible-local", "custom-embedding-local"] {
        let state = app_state(vec![node(provider, "Local Node")]).await;
        let (status, _) = create(
            &state,
            json!({ "provider": provider, "name": "Local", "apiKey": "sk-x" }),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED, "{provider} must resolve");
        let psd = created_psd(&state, provider);
        assert!(
            psd.get("apiType").is_none(),
            "{provider} must not set apiType"
        );
        assert_eq!(psd["baseUrl"], "https://node.example/v1");
    }
}

#[tokio::test]
async fn each_compatible_prefix_names_its_own_miss() {
    for (provider, needle) in [
        ("openai-compatible-x", "OpenAI Compatible"),
        ("anthropic-compatible-x", "Anthropic Compatible"),
        ("custom-embedding-x", "Custom Embedding"),
    ] {
        let state = app_state(vec![]).await;
        let (status, json) = create(
            &state,
            json!({ "provider": provider, "name": "Local", "apiKey": "sk-x" }),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            json["error"].as_str().unwrap_or_default().contains(needle),
            "{provider} must report a `{needle} node not found` miss, got {json}"
        );
    }
}

/// 9router overwrites `providerSpecificData` with the node's (route.js:136-141),
/// so the node — not a baseUrl typed into the body — decides where this
/// connection dials. A stale body value would silently point it elsewhere.
#[tokio::test]
async fn node_base_url_wins_over_the_request_body() {
    let state = app_state(vec![node("openai-compatible-local", "Local Node")]).await;
    let (status, _) = create(
        &state,
        json!({
            "provider": "openai-compatible-local",
            "name": "Local",
            "apiKey": "sk-x",
            "baseUrl": "https://attacker.example/v1"
        }),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        created_psd(&state, "openai-compatible-local")["baseUrl"],
        "https://node.example/v1"
    );
}

// ── Header breadcrumb icon (source contract) ────────────────────────

fn read_web_src(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("web/src")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn assert_contains(haystack: &str, needle: &str, file: &str, why: &str) {
    assert!(
        haystack.contains(needle),
        "{file} must contain `{needle}` — {why}\n(the dashboard serves web/dist, so this must ship via `cd web && pnpm build`)"
    );
}

fn assert_not_contains(haystack: &str, needle: &str, file: &str, why: &str) {
    assert!(
        !haystack.contains(needle),
        "{file} must NOT contain `{needle}` — {why}"
    );
}

#[test]
fn header_breadcrumb_resolves_icons_through_the_alias_table() {
    let header = read_web_src("shared/components/Header.tsx");

    assert_contains(
        &header,
        "getProviderIconSrc",
        "Header.tsx",
        "the breadcrumb must resolve icons through the alias table + session cache",
    );
    assert_contains(
        &header,
        "image: getProviderIconSrc(providerId)",
        "Header.tsx",
        "the media-provider crumb must not interpolate the raw id",
    );
    assert_contains(
        &header,
        "image: getProviderIconSrc(providerInfo.id)",
        "Header.tsx",
        "the provider-detail crumb must not interpolate the raw id",
    );
    assert_contains(
        &header,
        "image?: string | null;",
        "Header.tsx",
        "getProviderIconSrc returns `string | null` and the render guard skips a null image",
    );
    assert_not_contains(
        &header,
        "/providers/${providerId}.png",
        "Header.tsx",
        "string interpolation bypasses ICON_ALIASES and the session 404 cache",
    );
    assert_not_contains(
        &header,
        "/providers/${providerInfo.id}.png",
        "Header.tsx",
        "string interpolation bypasses ICON_ALIASES and the session 404 cache",
    );
}
