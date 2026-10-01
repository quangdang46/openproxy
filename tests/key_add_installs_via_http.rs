//! `openproxy key add` must install the key the CLI minted, without a restart.
//!
//! The CLI mints its own secret and then installs it through `POST /api/keys`
//! so the running server picks it up in its in-memory `AppDb` snapshot. Two
//! things silently broke that path, and both made the CLI fall back to writing
//! SQLite directly — which leaves the snapshot stale, so the freshly issued key
//! 401s until the operator restarts the server:
//!
//! 1. The CLI read its bearer credential out of `<data_dir>/db.json`. SQLite is
//!    the only runtime store now, so that file does not exist and the lookup
//!    always failed.
//! 2. The endpoint ignored the `key` field in the request body and minted its
//!    own secret, so even once the POST went out, the value the CLI handed back
//!    to the user was never the one stored — and never authenticated.
//!
//! These tests pin the contract from both sides: the endpoint stores exactly
//! the secret it was handed, and that secret is immediately accepted by the
//! auth layer against the same snapshot.

use std::collections::BTreeMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;

const MGMT_KEY: &str = "key-add-http-mgmt";

fn management_key() -> ApiKey {
    ApiKey {
        id: "key-mgmt".into(),
        name: "management".into(),
        key: MGMT_KEY.into(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        extra: BTreeMap::new(),
        monthly_budget_usd: None,
    }
}

async fn state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = std::sync::Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| state.api_keys = vec![management_key()])
        .await
        .expect("seed");
    AppState::new(db)
}

async fn post_create_key(
    state: &AppState,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys")
                .header("authorization", format!("Bearer {MGMT_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// The secret the CLI generated must be the secret that lands on disk and in
/// the snapshot. If the endpoint minted its own instead, the user is handed a
/// credential that 401s on first use.
#[tokio::test]
async fn supplied_key_is_stored_verbatim() {
    let state = state().await;
    let minted = "op-28c40f6e67f7471f1fb05abcdef0123456789abcdef0123456789abcdef";

    let (status, body) = post_create_key(&state, json!({ "name": "cli", "key": minted })).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let snapshot = state.db.snapshot();
    let stored = snapshot
        .api_keys
        .iter()
        .find(|k| k.name == "cli")
        .expect("cli key persisted");
    assert_eq!(
        stored.key, minted,
        "endpoint replaced the caller-supplied secret with one of its own"
    );
    assert_eq!(body["key"], minted, "response echoed a different secret");
}

/// The whole point of the HTTP install path: the key works against the running
/// server's snapshot immediately, with no restart in between.
#[tokio::test]
async fn supplied_key_authenticates_against_the_live_snapshot() {
    let state = state().await;
    let minted = "op-aaaaaaaabbbbbbbbccccccccddddddddeeeeeeeeffffffff00000000";

    let (status, _) = post_create_key(&state, json!({ "name": "cli", "key": minted })).await;
    assert_eq!(status, StatusCode::CREATED);

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {minted}").parse().unwrap(),
    );
    let resolved = openproxy::server::auth::require_api_key(&headers, &state.db);
    assert!(
        resolved.is_ok(),
        "freshly added key must authenticate without a server restart: {:?}",
        resolved.err()
    );
}

/// `op-…` secrets carry no machine segment, so the machine id must fall back to
/// this host's rather than being left empty or invented.
#[tokio::test]
async fn op_key_without_machine_segment_gets_the_host_machine_id() {
    let state = state().await;
    let minted = "op-1111111122222222333333334444444455555555666666667777777788888888";

    let (status, body) = post_create_key(&state, json!({ "name": "cli", "key": minted })).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let machine = body["machineId"].as_str().expect("machineId in response");
    assert!(
        !machine.is_empty(),
        "machineId must not be empty for an op- key"
    );
}

/// The dashboard path is unchanged: no `key` in the body means the server mints
/// one in the `sk-` form it can parse.
#[tokio::test]
async fn omitted_key_still_mints_a_machine_bound_secret() {
    let state = state().await;

    let (status, body) = post_create_key(&state, json!({ "name": "dashboard" })).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let key = body["key"].as_str().expect("key");
    assert!(
        key.starts_with("sk-"),
        "server-generated key lost its sk- form: {key}"
    );
    let parsed = openproxy::core::auth::parse_api_key(key).expect("generated key must parse");
    assert_eq!(parsed.machine_id.as_deref(), body["machineId"].as_str());
}

/// A value that could never authenticate is rejected loudly rather than stored,
/// which would hand the user a dead credential with no error.
///
/// An empty string is deliberately not in this list: it is treated as "caller
/// did not supply a key" and falls through to the server minting one, the same
/// as omitting the field entirely.
#[tokio::test]
async fn unusable_key_is_rejected_rather_than_stored() {
    let state = state().await;

    for bad in ["garbage", "xyz-123", "sk", "op-"] {
        let (status, body) = post_create_key(&state, json!({ "name": "bad", "key": bad })).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "key {bad:?} should be rejected, got {status} {body}"
        );
    }

    let snapshot = state.db.snapshot();
    assert!(
        !snapshot.api_keys.iter().any(|k| k.name == "bad"),
        "a rejected key must not be persisted"
    );
}

/// An empty `key` is the "no secret supplied" case, not a malformed one, so it
/// takes the mint path and still yields a usable key.
#[tokio::test]
async fn empty_key_falls_back_to_minting() {
    let state = state().await;

    let (status, body) = post_create_key(&state, json!({ "name": "blank", "key": "" })).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert!(
        body["key"].as_str().expect("key").starts_with("sk-"),
        "an empty key should fall back to a server-minted secret, got {body}"
    );
}
