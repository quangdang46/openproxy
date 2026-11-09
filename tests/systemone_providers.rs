//! System One (Jev) decision endpoint — 9router
//! `src/sse/handlers/systemone.js` + `open-sse/handlers/systemoneCore.js`
//! parity (bead openproxy-9router-parity-v5104-sync-eidj.47).
//!
//! Covers the trust-boundary validation order, provider resolution through
//! the catalog `systemone` kind, per-provider header/URL fidelity
//! (OpenRouter referer headers, Zen session header, Cloudflare
//! `{accountId}`/`{model}` expansion + `upstreamModelId` body rewrite), the
//! no-auth lanes, and account rotation on 401/403/429.

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
use wiremock::{Mock, MockServer, Request as MockRequest, ResponseTemplate};

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
        access: Default::default(),
    }
}

fn connection(id: &str, provider: &str, key: Option<&str>) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: provider.into(),
        auth_type: "apikey".into(),
        name: Some(id.into()),
        is_active: Some(true),
        api_key: key.map(str::to_string),
        ..Default::default()
    }
}

/// Base state for the systemone route: one API key, auth off (the requireApiKey
/// gate is covered elsewhere).
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

async fn response_json(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

fn systemone_request(body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/v1/systemone")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn post_systemone(app: axum::Router, body: Value) -> (StatusCode, Value) {
    response_json(app.oneshot(systemone_request(body)).await.unwrap()).await
}

fn decision_body(model: &str) -> Value {
    json!({
        "model": model,
        "state": { "customer": "I was charged twice" },
        "questions": {
            "probe": { "type": "noul", "instructions": "Is this a billing problem?" }
        }
    })
}

// ── Catalog parity ────────────────────────────────────────────────────────

/// The registry `kind: "systemone"` models upstream declares must exist in
/// the catalog under the right provider alias.
#[test]
fn systemone_catalog_entries_match_upstream_registry() {
    use openproxy::core::model::{catalog::provider_catalog, parse_model, resolve_provider_alias};

    let catalog = provider_catalog();

    // v1m (registry/v1m.js)
    let v1m = catalog.models_for_alias("v1m").expect("v1m models");
    let v1m_ids: Vec<&str> = v1m.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(v1m_ids, ["rev-latest", "v1m-decision-engine"]);
    assert!(v1m.iter().all(|m| m.kind == "systemone"));

    // OpenRouter TypeSafe lane (registry/openrouter.js:46)
    let or = catalog
        .models_for_alias("openrouter")
        .expect("openrouter models");
    assert!(or
        .iter()
        .any(|m| m.id == "typesafe/jev-1.13" && m.kind == "systemone"));

    // OpenCode Zen (registry/opencode-zen.js:118-119) + free opencode
    // (registry/opencode.js:43).
    let zen = catalog
        .models_for_alias("opencode-zen")
        .expect("opencode-zen models");
    for id in ["jev-1.13", "jev-1.13-free"] {
        assert!(
            zen.iter().any(|m| m.id == id && m.kind == "systemone"),
            "opencode-zen missing {id}"
        );
    }
    let oc = catalog.models_for_alias("oc").expect("oc models");
    assert!(oc
        .iter()
        .any(|m| m.id == "jev-1.13-free" && m.kind == "systemone"));

    // Cloudflare Workers AI clef-flash (registry/cloudflare-ai.js:30).
    let cf = catalog
        .models_for_alias("cloudflare-ai")
        .expect("cloudflare-ai models");
    let clef = cf
        .iter()
        .find(|m| m.id == "@cf/cloudflare/clef-flash")
        .expect("clef-flash entry");
    assert_eq!(clef.kind, "systemone");
    assert_eq!(clef.upstream_model_id.as_deref(), Some("clef-flash"));

    // Alias resolution: `systemone/` and `jev/` prefixes land on v1m.
    for prefix in ["v1m", "systemone", "jev"] {
        let parsed = parse_model(&format!("{prefix}/rev-latest"));
        assert_eq!(
            parsed.provider.as_deref(),
            Some("v1m"),
            "{prefix}/rev-latest must resolve to v1m"
        );
        assert_eq!(resolve_provider_alias(prefix), "v1m");
    }
}

/// Every catalog provider advertising a `systemone`-kind model must have a
/// transport config — the catalog is the discovery surface, the config is
/// the wire. A new System One provider added to the catalog without a
/// `systemoneConfig` port fails here instead of 400ing at request time.
#[test]
fn every_catalog_systemone_provider_has_a_transport_config() {
    use openproxy::core::media::systemone::systemone_config;
    use openproxy::core::model::catalog::provider_catalog;

    let catalog = provider_catalog();
    let alias_to_id = catalog.alias_to_provider_id();
    for entry in catalog.iter_provider_models() {
        if !entry.models.iter().any(|m| m.kind == "systemone") {
            continue;
        }
        let provider_id = alias_to_id
            .get(&entry.alias)
            .cloned()
            .unwrap_or_else(|| entry.alias.clone());
        assert!(
            systemone_config(&provider_id).is_some(),
            "catalog provider `{provider_id}` (alias `{}`) advertises systemone \
             models but has no systemone_config transport",
            entry.alias
        );
    }
}

// ── Trust-boundary validation (systemone.js:56-72) ─────────────────────────

#[tokio::test]
async fn systemone_requires_model_state_and_questions() {
    let state = app_state_with(vec![]).await;
    let app = openproxy::build_app(state);

    for (body, expected) in [
        (json!({"state": {}, "questions": {}}), "Missing model"),
        (
            json!({"model": "v1m/rev-latest", "questions": {}}),
            "Missing required field: state",
        ),
        (
            json!({"model": "v1m/rev-latest", "state": null, "questions": {}}),
            "Missing required field: state",
        ),
        (
            json!({"model": "v1m/rev-latest", "state": {}}),
            "Missing required field: questions",
        ),
        (
            json!({"model": "v1m/rev-latest", "state": {}, "questions": []}),
            "Missing required field: questions",
        ),
    ] {
        let (status, json) = post_systemone(app.clone(), body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {json}");
        let message = json["error"]["message"].as_str().unwrap_or("");
        assert!(
            message.contains(expected),
            "expected '{expected}' in '{message}'"
        );
    }
}

#[tokio::test]
async fn systemone_rejects_providers_without_a_lane() {
    let state = app_state_with(vec![connection("c1", "deepseek", Some("k"))]).await;
    let app = openproxy::build_app(state);
    let (status, json) = post_systemone(app, decision_body("deepseek/deepseek-chat")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("does not support System One"),
        "unexpected body: {json}"
    );
}

#[tokio::test]
async fn systemone_requires_credentials_for_auth_providers() {
    let state = app_state_with(vec![]).await;
    let app = openproxy::build_app(state);
    let (status, json) = post_systemone(app, decision_body("v1m/rev-latest")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("No credentials for provider: v1m"),
        "unexpected body: {json}"
    );
}

// ── Upstream forwarding ────────────────────────────────────────────────────

/// v1m forwards the decision body untouched (model rewritten to the catalog
/// id, which here is the same), with Bearer auth, and passes the JSON reply
/// through verbatim.
#[tokio::test]
async fn v1m_forwards_decision_body_and_returns_json() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "answers": { "probe": { "noul": 0.97 } },
            "usage": { "input_tokens": 11, "output_tokens": 4 }
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let mut conn = connection("v1m-1", "v1m", Some("v1m-key"));
    conn.provider_specific_data.insert(
        "baseUrl".into(),
        json!(format!("{}/v1/systemone", upstream.uri())),
    );
    let state = app_state_with(vec![conn]).await;
    let app = openproxy::build_app(state);

    let (status, body) = post_systemone(app, decision_body("v1m/rev-latest")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["answers"]["probe"]["noul"], json!(0.97));
}

/// The upstream request carries the bearer token and the untouched
/// state/questions payload.
#[tokio::test]
async fn v1m_sends_bearer_and_verbatim_decision_fields() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(|req: &MockRequest| {
            let auth = req
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            assert_eq!(auth, "Bearer v1m-key");
            let sent: Value = serde_json::from_slice(&req.body).unwrap();
            assert_eq!(sent["model"], json!("rev-latest"));
            assert_eq!(sent["state"]["customer"], json!("I was charged twice"));
            assert_eq!(sent["questions"]["probe"]["type"], json!("noul"));
            ResponseTemplate::new(200).set_body_json(json!({ "ok": true }))
        })
        .expect(1)
        .mount(&upstream)
        .await;

    let mut conn = connection("v1m-1", "v1m", Some("v1m-key"));
    conn.provider_specific_data.insert(
        "baseUrl".into(),
        json!(format!("{}/v1/systemone", upstream.uri())),
    );
    let state = app_state_with(vec![conn]).await;
    let app = openproxy::build_app(state);

    let (status, _) = post_systemone(app, decision_body("v1m/rev-latest")).await;
    assert_eq!(status, StatusCode::OK);
}

/// OpenRouter's systemoneConfig headers (HTTP-Referer + X-Title) must reach
/// the upstream — 9router registry/openrouter.js:51-53.
#[tokio::test]
async fn openrouter_lane_sends_registry_headers() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/systemone"))
        .respond_with(|req: &MockRequest| {
            let referer = req
                .headers
                .get("http-referer")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let title = req
                .headers
                .get("x-title")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            assert_eq!(referer, "https://endpoint-proxy.local");
            assert_eq!(title, "Endpoint Proxy");
            ResponseTemplate::new(200).set_body_json(json!({ "ok": true }))
        })
        .expect(1)
        .mount(&upstream)
        .await;

    let mut conn = connection("or-1", "openrouter", Some("or-key"));
    // openrouter's systemoneConfig has no `path`, so a saved baseUrl is a
    // full-URL override (buildSystemoneUrl).
    conn.provider_specific_data.insert(
        "baseUrl".into(),
        json!(format!("{}/api/v1/systemone", upstream.uri())),
    );
    let state = app_state_with(vec![conn]).await;
    let app = openproxy::build_app(state);

    let (status, _) = post_systemone(app, decision_body("openrouter/typesafe/jev-1.13")).await;
    assert_eq!(status, StatusCode::OK);
}

/// Zen lanes send `x-opencode-session` + the registry client headers and the
/// "public" noAuth token when no credential is stored.
#[tokio::test]
async fn zen_lane_sends_session_and_client_headers() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/zen/v1/systemone"))
        .respond_with(|req: &MockRequest| {
            let session = req
                .headers
                .get("x-opencode-session")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            assert!(session.starts_with("ses_"), "session: {session}");
            assert_eq!(
                req.headers
                    .get("x-opencode-client")
                    .and_then(|v| v.to_str().ok()),
                Some("desktop")
            );
            assert_eq!(
                req.headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok()),
                Some("Bearer public")
            );
            ResponseTemplate::new(200).set_body_json(json!({ "ok": true }))
        })
        .expect(1)
        .mount(&upstream)
        .await;

    // opencode-zen is a noAuth lane: a stored connection carries no
    // credential but may carry providerSpecificData. baseUrl override is a
    // full URL for this config (no `path` in systemoneConfig).
    let mut conn = connection("zen-1", "opencode-zen", None);
    conn.provider_specific_data.insert(
        "baseUrl".into(),
        json!(format!("{}/zen/v1/systemone", upstream.uri())),
    );
    let state = app_state_with(vec![conn]).await;
    let app = openproxy::build_app(state);

    let (status, _) = post_systemone(app, decision_body("opencode-zen/jev-1.13-free")).await;
    assert_eq!(status, StatusCode::OK);
}

/// Cloudflare expands `{accountId}`/`{model}` in the URL with the catalog
/// model id, while the body `model` field gets the `upstreamModelId`
/// (`clef-flash`) — systemoneCore.js's two different substitutions.
#[tokio::test]
async fn cloudflare_expands_placeholders_and_rewrites_body_model() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/client/v4/accounts/acct-9/ai/run/@cf/cloudflare/clef-flash",
        ))
        .respond_with(|req: &MockRequest| {
            let sent: Value = serde_json::from_slice(&req.body).unwrap();
            assert_eq!(sent["model"], json!("clef-flash"));
            ResponseTemplate::new(200).set_body_json(json!({ "ok": true }))
        })
        .expect(1)
        .mount(&upstream)
        .await;

    let mut conn = connection("cf-1", "cloudflare-ai", Some("cf-token"));
    conn.provider_specific_data
        .insert("accountId".into(), json!("acct-9"));
    conn.provider_specific_data.insert(
        "baseUrl".into(),
        json!(format!(
            "{}/client/v4/accounts/{{accountId}}/ai/run/{{model}}",
            upstream.uri()
        )),
    );
    let state = app_state_with(vec![conn]).await;
    let app = openproxy::build_app(state);

    let (status, _) = post_systemone(
        app,
        decision_body("cloudflare-ai/@cf/cloudflare/clef-flash"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// Missing accountId is a 400 before any upstream call (systemoneCore.js).
/// No baseUrl override, so the registry template's `{accountId}` placeholder
/// survives URL building and expansion fails.
#[tokio::test]
async fn cloudflare_requires_account_id() {
    let state = app_state_with(vec![connection("cf-1", "cloudflare-ai", Some("cf-token"))]).await;
    let app = openproxy::build_app(state);

    let (status, json) = post_systemone(
        app,
        decision_body("cloudflare-ai/@cf/cloudflare/clef-flash"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("accountId"),
        "unexpected body: {json}"
    );
}

/// ollama-local needs no credential at all — the daemon may be unauthenticated.
#[tokio::test]
async fn ollama_local_works_without_credentials() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(|req: &MockRequest| {
            assert!(
                req.headers.get("authorization").is_none(),
                "ollama-local must not send Authorization without a token"
            );
            ResponseTemplate::new(200).set_body_json(json!({ "ok": true }))
        })
        .expect(1)
        .mount(&upstream)
        .await;

    // A stored active connection with providerSpecificData.baseUrl but no
    // credential — select_media_connections drops it (no credentials), the
    // no-auth fallback reuses it for the host override.
    let mut conn = connection("oll-1", "ollama-local", None);
    conn.provider_specific_data
        .insert("baseUrl".into(), json!(upstream.uri()));
    let state = app_state_with(vec![conn]).await;
    let app = openproxy::build_app(state);

    let (status, _) = post_systemone(app, decision_body("ollama-local/clef-flash")).await;
    assert_eq!(status, StatusCode::OK);
}

/// Auth failures rotate to the next account (401/403/429), mirroring the
/// embeddings credential-rotation loop.
#[tokio::test]
async fn systemone_rotates_accounts_on_auth_errors() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(|req: &MockRequest| {
            let auth = req
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if auth == "Bearer bad-key" {
                ResponseTemplate::new(401).set_body_json(json!({
                    "error": { "message": "invalid key" }
                }))
            } else {
                ResponseTemplate::new(200).set_body_json(json!({ "ok": true }))
            }
        })
        .expect(2)
        .mount(&upstream)
        .await;

    let mut bad = connection("v1m-bad", "v1m", Some("bad-key"));
    bad.priority = Some(1);
    bad.provider_specific_data.insert(
        "baseUrl".into(),
        json!(format!("{}/v1/systemone", upstream.uri())),
    );
    let mut good = connection("v1m-good", "v1m", Some("good-key"));
    good.priority = Some(2);
    good.provider_specific_data.insert(
        "baseUrl".into(),
        json!(format!("{}/v1/systemone", upstream.uri())),
    );
    let state = app_state_with(vec![bad, good]).await;
    let app = openproxy::build_app(state);

    let (status, body) = post_systemone(app, decision_body("v1m/rev-latest")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["ok"], json!(true));
}

/// Upstream error statuses surface through the standard error envelope with
/// the upstream message (parseUpstreamError + formatProviderError parity).
#[tokio::test]
async fn systemone_surfaces_upstream_error_message() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "error": { "message": "questions.probe.type is not supported" }
        })))
        .mount(&upstream)
        .await;

    let mut conn = connection("v1m-1", "v1m", Some("k"));
    conn.provider_specific_data.insert(
        "baseUrl".into(),
        json!(format!("{}/v1/systemone", upstream.uri())),
    );
    let state = app_state_with(vec![conn]).await;
    let app = openproxy::build_app(state);

    let (status, json) = post_systemone(app, decision_body("v1m/rev-latest")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let message = json["error"]["message"].as_str().unwrap_or("");
    assert!(
        message.contains("questions.probe.type is not supported"),
        "unexpected body: {json}"
    );
}
