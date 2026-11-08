#![allow(clippy::await_holding_lock)]
//! API round-trip tests for the v0.5.111+ generic CLI-tool settings routes:
//! pi, omp, crush, forge, smelt, codewhale. Each test drives
//! apply (POST) → show (GET) → revert (DELETE) against a temp HOME.

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
        state.api_keys = vec![active_key("sk-test-key")];
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

fn authorized_request(method: Method, uri: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer sk-test-key")
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

async fn response_json(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap();
    (status, json)
}

struct EnvVarGuard {
    key: &'static str,
    old_value: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &Path) -> Self {
        let old_value = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, old_value }
    }
    fn set_str(key: &'static str, value: &str) -> Self {
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

/// Isolated HOME (+ XDG_CONFIG_HOME) + empty PATH so `which <tool>` fails and
/// only the config-file fallback marks the tool "installed".
struct TestEnv {
    _home: EnvVarGuard,
    _xdg: EnvVarGuard,
    _path: EnvVarGuard,
    home: PathBuf,
}

fn test_env() -> TestEnv {
    let home = tempdir().unwrap();
    let xdg = tempdir().unwrap();
    let path = tempdir().unwrap();
    // Leak the tempdirs for the duration of the test — HOME must outlive the
    // guards, which drop at end of scope in reverse order. Keeping the
    // TempDir alive inside the struct is not possible with env guards, so we
    // intentionally leak: tests are short-lived processes.
    let home_path = home.path().to_path_buf();
    let xdg_path = xdg.path().to_path_buf();
    let path_path = path.path().to_path_buf();
    std::mem::forget(home);
    std::mem::forget(xdg);
    std::mem::forget(path);
    TestEnv {
        _home: EnvVarGuard::set("HOME", &home_path),
        _xdg: EnvVarGuard::set("XDG_CONFIG_HOME", &xdg_path),
        _path: EnvVarGuard::set("PATH", &path_path),
        home: home_path,
    }
}

async fn call(app: &axum::Router, method: Method, uri: &str, body: Body) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(authorized_request(method, uri, body))
        .await
        .unwrap();
    response_json(response).await
}

// ─── Pi ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn pi_settings_apply_show_revert_round_trip() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = test_env();
    let app = openproxy::build_app(app_state().await);

    // Not installed before any config exists.
    let (status, json) = call(
        &app,
        Method::GET,
        "/api/cli-tools/pi-settings",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["installed"], false);

    // Apply with two models + explicit caps.
    let (status, json) = call(
        &app,
        Method::POST,
        "/api/cli-tools/pi-settings",
        Body::from(
            r#"{"baseUrl":"http://127.0.0.1:4623","apiKey":"sk-test-key","models":[{"id":"cc/claude-opus-5","name":"cc/claude-opus-5","contextWindow":400000,"maxTokens":128000},{"id":"cx/gpt-5.5"}]}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);

    let config_path = env.home.join(".pi/agent/models.json");
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    let provider = &written["providers"]["9router"];
    assert_eq!(provider["baseUrl"], "http://127.0.0.1:4623/v1");
    assert_eq!(provider["api"], "openai-completions");
    assert_eq!(provider["models"][0]["contextWindow"], 400000);
    assert_eq!(provider["models"][0]["maxTokens"], 128000);
    // Second model gets defaults.
    assert_eq!(provider["models"][1]["contextWindow"], 200000);
    assert_eq!(provider["models"][1]["maxTokens"], 64000);

    // GET reports installed + has9Router.
    let (status, json) = call(
        &app,
        Method::GET,
        "/api/cli-tools/pi-settings",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["installed"], true);
    assert_eq!(json["has9Router"], true);

    // Re-apply merges caps: lower the first model's contextWindow, keep the
    // hand-set name on the second (name===id is "unset" → previous name kept).
    std::fs::write(
        &config_path,
        serde_json::to_string_pretty(&json!({
            "providers": {
                "9router": {
                    "baseUrl": "http://127.0.0.1:4623/v1",
                    "apiKey": "sk-test-key",
                    "api": "openai-completions",
                    "models": [
                        {"id": "cx/gpt-5.5", "name": "GPT Five Point Five", "contextWindow": 1000000, "maxTokens": 32000}
                    ]
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let (status, _) = call(
        &app,
        Method::POST,
        "/api/cli-tools/pi-settings",
        Body::from(
            r#"{"baseUrl":"http://127.0.0.1:4623","models":[{"id":"cx/gpt-5.5","name":"cx/gpt-5.5","contextWindow":99999999,"maxTokens":16}]}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let merged: Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    let m = &merged["providers"]["9router"]["models"][0];
    // contextWindow over the 2M cap falls back to the previous stored value.
    assert_eq!(m["contextWindow"], 1000000);
    assert_eq!(m["maxTokens"], 16);
    // name===id treated as unset → keeps the hand-written display name.
    assert_eq!(m["name"], "GPT Five Point Five");

    // Revert removes the provider block.
    let (status, json) = call(
        &app,
        Method::DELETE,
        "/api/cli-tools/pi-settings",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);

    let reverted: Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert!(reverted.get("providers").is_none() || reverted["providers"].get("9router").is_none());
}

// ─── OMP ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn omp_settings_apply_show_revert_round_trip() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = test_env();
    let app = openproxy::build_app(app_state().await);

    let yml_path = env.home.join(".omp/agent/models.yml");
    std::fs::create_dir_all(yml_path.parent().unwrap()).unwrap();
    std::fs::write(&yml_path, "theme: dark\n").unwrap();

    let (status, json) = call(
        &app,
        Method::POST,
        "/api/cli-tools/omp-settings",
        Body::from(r#"{"baseUrl":"http://127.0.0.1:4623","apiKey":"sk-test-key"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);

    let yml = std::fs::read_to_string(&yml_path).unwrap();
    assert!(yml.contains("providers:"), "yml: {yml}");
    assert!(yml.contains("  9router:"), "yml: {yml}");
    assert!(
        yml.contains("baseUrl: http://127.0.0.1:4623/v1"),
        "yml: {yml}"
    );
    assert!(yml.contains("apiKey: sk-test-key"), "yml: {yml}");
    assert!(yml.contains("type: proxy"), "yml: {yml}");
    assert!(yml.contains("theme: dark"), "yml: {yml}");

    let (status, json) = call(
        &app,
        Method::GET,
        "/api/cli-tools/omp-settings",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["installed"], true);
    assert_eq!(json["has9Router"], true);

    // Re-apply replaces the block instead of duplicating it.
    let (status, _) = call(
        &app,
        Method::POST,
        "/api/cli-tools/omp-settings",
        Body::from(r#"{"baseUrl":"http://localhost:9999"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let yml2 = std::fs::read_to_string(&yml_path).unwrap();
    assert_eq!(yml2.matches("9router:").count(), 1, "yml: {yml2}");
    // Empty caller key → falls back to the first active db key.
    assert!(yml2.contains("apiKey: sk-test-key"), "yml: {yml2}");

    let (status, json) = call(
        &app,
        Method::DELETE,
        "/api/cli-tools/omp-settings",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    let yml3 = std::fs::read_to_string(&yml_path).unwrap();
    assert!(!yml3.contains("9router:"), "yml: {yml3}");
    assert!(yml3.contains("theme: dark"), "yml: {yml3}");
}

// ─── Crush ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn crush_settings_apply_show_revert_round_trip() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = test_env();
    let app = openproxy::build_app(app_state().await);

    let config_path = env.home.join(".config/crush/crush.json");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, r#"{"other": true}"#).unwrap();

    let (status, json) = call(
        &app,
        Method::POST,
        "/api/cli-tools/crush-settings",
        Body::from(r#"{"baseUrl":"http://127.0.0.1:4623","model":"cc/claude-opus-5"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);

    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(written["other"], true);
    let provider = &written["providers"]["9router"];
    assert_eq!(provider["type"], "openai-compat");
    assert_eq!(provider["base_url"], "http://127.0.0.1:4623/v1");
    assert_eq!(provider["api_key"], "sk-test-key");
    assert_eq!(provider["models"][0]["id"], "cc/claude-opus-5");
    assert_eq!(provider["models"][0]["context_window"], 128000);

    let (status, json) = call(
        &app,
        Method::GET,
        "/api/cli-tools/crush-settings",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["installed"], true);
    assert_eq!(json["has9Router"], true);

    let (status, json) = call(
        &app,
        Method::DELETE,
        "/api/cli-tools/crush-settings",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    let reverted: Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(reverted["other"], true);
    assert!(reverted["providers"].get("9router").is_none());
}

// ─── Forge ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn forge_settings_apply_show_revert_round_trip() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = test_env();
    let app = openproxy::build_app(app_state().await);

    let config_path = env.home.join(".forge/config.toml");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "[other]\nfoo = \"bar\"\n").unwrap();

    let (status, json) = call(
        &app,
        Method::POST,
        "/api/cli-tools/forge-settings",
        Body::from(r#"{"baseUrl":"http://127.0.0.1:4623","model":"cc/claude-opus-5"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);

    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(written.contains("managed by 9Router"), "toml: {written}");
    assert!(written.contains("[openai]"), "toml: {written}");
    assert!(
        written.contains("base_url = \"http://127.0.0.1:4623/v1\""),
        "toml: {written}"
    );
    assert!(
        written.contains("api_key = \"sk-test-key\""),
        "toml: {written}"
    );
    assert!(
        written.contains("model = \"cc/claude-opus-5\""),
        "toml: {written}"
    );

    let (status, json) = call(
        &app,
        Method::GET,
        "/api/cli-tools/forge-settings",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["installed"], true);
    assert_eq!(json["has9Router"], true);
    assert_eq!(json["config"]["openai"]["model"], "cc/claude-opus-5");

    let (status, json) = call(
        &app,
        Method::DELETE,
        "/api/cli-tools/forge-settings",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    let reverted = std::fs::read_to_string(&config_path).unwrap();
    assert!(!reverted.contains("[openai]"), "toml: {reverted}");
    assert!(reverted.contains("foo = \"bar\""), "toml: {reverted}");
}

// ─── Smelt ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn smelt_settings_apply_show_revert_round_trip() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = test_env();
    let app = openproxy::build_app(app_state().await);

    let config_path = env.home.join(".smelt/config.json");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, r#"{"theme":"dark","model":"keep-me"}"#).unwrap();

    let (status, json) = call(
        &app,
        Method::POST,
        "/api/cli-tools/smelt-settings",
        Body::from(r#"{"baseUrl":"http://127.0.0.1:4623"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);

    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(written["theme"], "dark");
    assert_eq!(written["baseUrl"], "http://127.0.0.1:4623/v1");
    assert_eq!(written["apiKey"], "sk-test-key");
    // Existing model preserved when none is sent.
    assert_eq!(written["model"], "keep-me");
    assert_eq!(written["_managedBy"], "9router");

    let (status, json) = call(
        &app,
        Method::GET,
        "/api/cli-tools/smelt-settings",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["installed"], true);
    assert_eq!(json["has9Router"], true);

    let (status, json) = call(
        &app,
        Method::DELETE,
        "/api/cli-tools/smelt-settings",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    let reverted: Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(reverted["theme"], "dark");
    for key in ["baseUrl", "apiKey", "model", "_managedBy"] {
        assert!(
            reverted.get(key).is_none(),
            "key {key} survived: {reverted}"
        );
    }
}

// ─── CodeWhale ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn codewhale_settings_apply_show_revert_round_trip() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = test_env();
    let app = openproxy::build_app(app_state().await);

    let config_path = env.home.join(".codewhale/config.toml");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "[ui]\ncompact = true\n").unwrap();

    let (status, json) = call(
        &app,
        Method::POST,
        "/api/cli-tools/codewhale-settings",
        Body::from(r#"{"baseUrl":"http://127.0.0.1:4623","model":"cx/gpt-5.5"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);

    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(written.contains("managed by 9Router"), "toml: {written}");
    assert!(
        written.contains("base_url = \"http://127.0.0.1:4623/v1\""),
        "toml: {written}"
    );
    assert!(
        written.contains("api_key = \"sk-test-key\""),
        "toml: {written}"
    );
    assert!(
        written.contains("model = \"cx/gpt-5.5\""),
        "toml: {written}"
    );

    let (status, json) = call(
        &app,
        Method::GET,
        "/api/cli-tools/codewhale-settings",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["installed"], true);
    assert_eq!(json["has9Router"], true);

    let (status, json) = call(
        &app,
        Method::DELETE,
        "/api/cli-tools/codewhale-settings",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    let reverted = std::fs::read_to_string(&config_path).unwrap();
    assert!(!reverted.contains("[openai]"), "toml: {reverted}");
    assert!(reverted.contains("compact = true"), "toml: {reverted}");
}

// ─── all-statuses includes the new tools ───────────────────────────────────

#[tokio::test]
async fn all_statuses_includes_generic_cli_tools() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = test_env();
    let app = openproxy::build_app(app_state().await);

    let (status, json) = call(
        &app,
        Method::GET,
        "/api/cli-tools/all-statuses",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for tool in ["pi", "omp", "crush", "forge", "smelt", "codewhale"] {
        assert!(
            json.get(tool).is_some(),
            "missing status for {tool}: {json}"
        );
        assert_eq!(
            json[tool]["installed"], false,
            "{tool} should be not-installed"
        );
    }
}
