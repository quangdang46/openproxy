#![allow(clippy::await_holding_lock)]
//! API tests for Codex CLI multiple model profiles
//! (`/api/cli-tools/codex-profiles`), ported from 9router v0.5.112
//! `src/app/api/cli-tools/codex-profiles/route.js`.
//!
//! Each profile is a `~/.codex/<name>.config.toml` file invoked via
//! `codex -p <name>`. The round-trip covered here: POST two profiles → GET
//! lists both sorted with parsed models → DELETE removes one → GET reflects
//! it → DELETE of a missing profile is a 404.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use once_cell::sync::Lazy;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

static ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

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

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

fn authorized_request(method: Method, uri: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer valid-bearer")
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

struct EnvVarGuard {
    key: &'static str,
    old_value: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set_path(key: &'static str, value: &Path) -> Self {
        let old_value = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, old_value }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.old_value.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

fn codex_dir(home: &Path) -> PathBuf {
    home.join(".codex")
}

async fn response_json(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap();
    (status, json)
}

async fn get_profiles(app: axum::Router) -> (StatusCode, Value) {
    let response = app
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/codex-profiles",
            Body::empty(),
        ))
        .await
        .unwrap();
    response_json(response).await
}

async fn post_profile(app: axum::Router, body: Value) -> (StatusCode, Value) {
    let response = app
        .oneshot(authorized_request(
            Method::POST,
            "/api/cli-tools/codex-profiles",
            Body::from(body.to_string()),
        ))
        .await
        .unwrap();
    response_json(response).await
}

async fn delete_profile(app: axum::Router, body: Value) -> (StatusCode, Value) {
    let response = app
        .oneshot(authorized_request(
            Method::DELETE,
            "/api/cli-tools/codex-profiles",
            Body::from(body.to_string()),
        ))
        .await
        .unwrap();
    response_json(response).await
}

#[tokio::test]
async fn codex_profiles_round_trip_multiple_models() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    // Windows resolves home via USERPROFILE — keep it inside the tempdir too.
    let _userprofile = EnvVarGuard::set_path("USERPROFILE", home.path());
    let app = openproxy::build_app(app_state().await);

    // GET on an empty/missing dir returns an empty list.
    let (status, json) = get_profiles(app.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["profiles"], json!([]));

    // Create two profiles out of alphabetical order to prove sorting.
    let (status, json) = post_profile(
        app.clone(),
        json!({ "name": "zeta", "model": "openai/gpt-5" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["success"], true);
    assert_eq!(json["profile"]["name"], "zeta");
    assert_eq!(json["profile"]["model"], "openai/gpt-5");
    assert_eq!(json["profile"]["command"], "codex -p zeta");

    let (status, json) = post_profile(
        app.clone(),
        json!({ "name": "alpha", "model": "anthropic/claude-sonnet-4" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");

    // Profile files carry the pinned model + the openproxy provider key.
    let zeta_toml = std::fs::read_to_string(codex_dir(home.path()).join("zeta.config.toml"))
        .expect("zeta profile file");
    assert!(
        zeta_toml.contains("model = \"openai/gpt-5\""),
        "{zeta_toml}"
    );
    assert!(
        zeta_toml.contains("model_provider = \"openproxy\""),
        "{zeta_toml}"
    );
    assert!(zeta_toml.contains("# codex -p zeta"), "{zeta_toml}");

    // GET lists both, sorted by name, with parsed models.
    let (status, json) = get_profiles(app.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let profiles = json["profiles"].as_array().expect("profiles array");
    assert_eq!(profiles.len(), 2, "{json}");
    assert_eq!(profiles[0]["name"], "alpha");
    assert_eq!(profiles[0]["model"], "anthropic/claude-sonnet-4");
    assert_eq!(profiles[0]["command"], "codex -p alpha");
    assert_eq!(profiles[1]["name"], "zeta");
    assert_eq!(profiles[1]["model"], "openai/gpt-5");

    // main config.toml is never listed as a profile.
    std::fs::write(
        codex_dir(home.path()).join("config.toml"),
        "model = \"gpt-5\"\n",
    )
    .unwrap();
    let (_, json) = get_profiles(app.clone()).await;
    assert_eq!(json["profiles"].as_array().unwrap().len(), 2, "{json}");

    // Delete one profile; the other survives.
    let (status, json) = delete_profile(app.clone(), json!({ "name": "alpha" })).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert!(!codex_dir(home.path()).join("alpha.config.toml").exists());
    assert!(codex_dir(home.path()).join("zeta.config.toml").exists());

    let (_, json) = get_profiles(app.clone()).await;
    let profiles = json["profiles"].as_array().unwrap();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0]["name"], "zeta");

    // Deleting a missing profile is a 404.
    let (status, _) = delete_profile(app.clone(), json!({ "name": "alpha" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Name validation rejects traversal and the reserved "config" name.
    for bad in ["../evil", "bad name", "config", "CONFIG", ""] {
        let (status, _) = post_profile(app.clone(), json!({ "name": bad, "model": "m" })).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "name {bad:?} must be rejected"
        );
    }
    // Model validation rejects empty / control-char / oversized values.
    for bad in ["", "has\nnewline", "has\"quote"] {
        let (status, _) = post_profile(app.clone(), json!({ "name": "ok", "model": bad })).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "model {bad:?} must be rejected"
        );
    }
    // No profile file may have been created by the rejected writes.
    assert!(!codex_dir(home.path()).join("ok.config.toml").exists());
    assert!(!codex_dir(home.path()).join("evil.config.toml").exists());
}
