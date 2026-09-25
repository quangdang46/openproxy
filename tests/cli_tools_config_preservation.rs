#![allow(clippy::await_holding_lock)]
//! Regression tests for openproxy-muwe: a CLI-tool settings write must never
//! destroy a config file it could not parse.
//!
//! The four (six, counting the `droid_settings` submodule) write paths read the
//! user's file, mutate one key, and write the map back. They used to funnel
//! every parse failure through a helper that returns an empty map, so a config
//! containing a trailing comma — or a BOM, or a half-written file from an
//! unclean shutdown — was silently replaced by a stub holding only the one key
//! being written. Every other setting the user had was gone, with a 200 back.
//!
//! These tests assert on BYTES, not on parsed content. That is the whole point:
//! after the destructive write the file still parses cleanly, so a
//! content-level assertion would pass on the broken behaviour.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use once_cell::sync::Lazy;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use serde_json::json;
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
        if let Some(value) = self.old_value.take() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn openclaw_settings_path(home: &Path) -> PathBuf {
    home.join(".openclaw").join("openclaw.json")
}

fn droid_settings_path(home: &Path) -> PathBuf {
    home.join(".factory").join("settings.json")
}

/// A config that is valid JSONC but NOT valid JSON: the trailing comma is
/// enough to make `serde_json::from_str` fail. This is the shape a hand-edited
/// or tool-written config realistically has.
const MALFORMED: &str = r#"{
  "theme": "dark",
  "unrelated": {"keep": "me"},
  "trailing": 1,
}"#;

async fn post_json(app: axum::Router, uri: &str, body: serde_json::Value) -> StatusCode {
    app.oneshot(authorized_request(
        Method::POST,
        uri,
        Body::from(body.to_string()),
    ))
    .await
    .unwrap()
    .status()
}

#[tokio::test]
async fn openclaw_settings_post_refuses_to_rewrite_malformed_config() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let settings = openclaw_settings_path(home.path());
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(&settings, MALFORMED).unwrap();

    let status = post_json(
        openproxy::build_app(app_state().await),
        "/api/cli-tools/openclaw-settings",
        json!({
            "baseUrl": "http://127.0.0.1:4623/v1",
            "apiKey": "sk-test",
            "model": "gpt-4o-mini",
        }),
    )
    .await;

    assert!(
        status.is_server_error(),
        "expected an error status, got {status}"
    );
    assert_eq!(
        std::fs::read_to_string(&settings).unwrap(),
        MALFORMED,
        "openclaw.json was rewritten even though it could not be parsed"
    );
}

#[tokio::test]
async fn droid_settings_post_refuses_to_rewrite_malformed_config() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let settings = droid_settings_path(home.path());
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(&settings, MALFORMED).unwrap();

    let status = post_json(
        openproxy::build_app(app_state().await),
        "/api/cli-tools/droid-settings",
        json!({
            "baseUrl": "http://127.0.0.1:4623/v1",
            "apiKey": "sk-test",
            "models": ["claude-sonnet-4"],
        }),
    )
    .await;

    assert!(
        status.is_server_error(),
        "expected an error status, got {status}"
    );
    assert_eq!(
        std::fs::read_to_string(&settings).unwrap(),
        MALFORMED,
        "droid settings.json was rewritten even though it could not be parsed"
    );
}

/// The negative tests above could be satisfied by refusing every write. Pin the
/// other half: a config that DOES parse must still be updated, and keys the
/// caller never mentioned must survive.
#[tokio::test]
async fn openclaw_settings_post_preserves_unrelated_keys() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let settings = openclaw_settings_path(home.path());
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let seed = json!({
        "theme": "dark",
        "unrelated": {"keep": "me"},
        "agents": {"defaults": {"model": "old-model"}},
    })
    .to_string();
    std::fs::write(&settings, &seed).unwrap();

    let status = post_json(
        openproxy::build_app(app_state().await),
        "/api/cli-tools/openclaw-settings",
        json!({
            "baseUrl": "http://127.0.0.1:4623/v1",
            "apiKey": "sk-test",
            "model": "new-model",
        }),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "a valid config must still be writable"
    );
    let updated: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(updated["theme"], "dark", "unrelated key was dropped");
    assert_eq!(updated["unrelated"]["keep"], "me", "nested key was dropped");
    // The handler stores the model as a routing object, not a bare string.
    assert_eq!(
        updated["agents"]["defaults"]["model"]["primary"], "openproxy/new-model",
        "the requested key was not updated"
    );
}
