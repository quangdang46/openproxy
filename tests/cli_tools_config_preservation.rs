#![allow(clippy::await_holding_lock)]
//! Regression tests for openproxy-muwe: a CLI-tool settings write must never
//! destroy a config file it could not parse.
//!
//! Each write path reads the user's file, mutates one key, and writes the
//! result back. They used to funnel every parse failure through a helper that
//! returns an empty map/vec, so a config containing a trailing comma — or a
//! BOM, or a half-written file from an unclean shutdown — was silently replaced
//! by a stub holding only the key being written. Every other setting the user
//! had was gone, with a 200 back.
//!
//! Covered: openclaw, droid (object), copilot (array), codex config.toml and
//! codex auth.json. Codex auth.json is the sharpest case — it holds the user's
//! OAuth tokens, so a swallowed parse error did not merely drop a setting, it
//! discarded the refresh_token and switched the CLI to apikey auth.
//!
//! NOTE: `src/server/api/cli_tools/droid_settings.rs` also carries this
//! pattern, but that module is not declared anywhere and never compiles into
//! the binary. Editing it changes nothing at runtime; the live droid path is
//! inline in `cli_tools.rs`. It is left untouched rather than half-fixed.
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

// ── Codex ────────────────────────────────────────────────────────────────────
// auth.json holds the user's OAuth tokens. A parse failure used to start from
// an empty map, write back a file containing only OPENAI_API_KEY, and silently
// switch the CLI to apikey auth — the refresh_token simply disappeared.

fn codex_auth_path(home: &Path) -> PathBuf {
    home.join(".codex").join("auth.json")
}

fn codex_config_path(home: &Path) -> PathBuf {
    home.join(".codex").join("config.toml")
}

/// Malformed JSON that still *contains* a refresh token, so a test can prove
/// the token survived rather than merely that the file is unchanged.
const CODEX_AUTH_MALFORMED: &str = r#"{
  "tokens": {"access_token": "at-1", "refresh_token": "rt-SECRET"},
  "trailing": 1,
}"#;

#[tokio::test]
async fn codex_settings_post_refuses_to_rewrite_malformed_auth_json() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let auth = codex_auth_path(home.path());
    std::fs::create_dir_all(auth.parent().unwrap()).unwrap();
    std::fs::write(&auth, CODEX_AUTH_MALFORMED).unwrap();

    let status = post_json(
        openproxy::build_app(app_state().await),
        "/api/cli-tools/codex-settings",
        json!({
            "baseUrl": "http://127.0.0.1:4623/v1",
            "apiKey": "sk-test",
            "model": "gpt-5",
        }),
    )
    .await;

    assert!(
        status.is_server_error(),
        "expected an error status, got {status}"
    );
    let after = std::fs::read_to_string(&auth).unwrap();
    assert_eq!(
        after, CODEX_AUTH_MALFORMED,
        "auth.json was rewritten; the OAuth tokens would be gone"
    );
    assert!(
        after.contains("rt-SECRET"),
        "the refresh token must survive a refused save"
    );
}

#[tokio::test]
async fn codex_settings_post_refuses_to_rewrite_malformed_config_toml() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let config = codex_config_path(home.path());
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let seed = "model = \"gpt-5\"\nthis is = = not toml\n";
    std::fs::write(&config, seed).unwrap();

    let status = post_json(
        openproxy::build_app(app_state().await),
        "/api/cli-tools/codex-settings",
        json!({
            "baseUrl": "http://127.0.0.1:4623/v1",
            "apiKey": "sk-test",
            "model": "gpt-5",
        }),
    )
    .await;

    assert!(
        status.is_server_error(),
        "expected an error status, got {status}"
    );
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        seed,
        "config.toml was rewritten even though it could not be parsed"
    );
}

// ── Copilot ─────────────────────────────────────────────────────────────────
// Copilot's config is a top-level JSON ARRAY, so the object-shaped fix could
// not reach it: a malformed file was replaced by a one-entry array holding
// only OpenProxy, dropping every other provider the user had configured.

fn copilot_config_path(home: &Path) -> PathBuf {
    if cfg!(windows) {
        home.join("Code")
            .join("User")
            .join("chatLanguageModels.json")
    } else if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("Code")
            .join("User")
            .join("chatLanguageModels.json")
    } else {
        home.join(".config")
            .join("Code")
            .join("User")
            .join("chatLanguageModels.json")
    }
}

#[tokio::test]
async fn copilot_settings_post_refuses_to_rewrite_malformed_config() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    // On Windows the path resolves through APPDATA, not HOME. Set both so the
    // test never writes to the real machine's VS Code config.
    let _appdata = EnvVarGuard::set_path("APPDATA", home.path());
    let config = copilot_config_path(home.path());
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let seed = r#"[{"name":"Someone Else","vendor":"other"},]"#;
    std::fs::write(&config, seed).unwrap();

    let status = post_json(
        openproxy::build_app(app_state().await),
        "/api/cli-tools/copilot-settings",
        json!({
            "baseUrl": "http://127.0.0.1:4623/v1",
            "apiKey": "sk-test",
            "models": ["gpt-4.1"],
        }),
    )
    .await;

    assert!(
        status.is_server_error(),
        "expected an error status, got {status}"
    );
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        seed,
        "the other provider in the array was dropped by a refused save"
    );
}
