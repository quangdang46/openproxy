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

async fn response_json(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
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

fn claude_settings_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

fn hermes_config_path(home: &Path) -> PathBuf {
    home.join(".hermes").join("config.yaml")
}

fn hermes_env_path(home: &Path) -> PathBuf {
    home.join(".hermes").join(".env")
}

fn codex_config_path(home: &Path) -> PathBuf {
    home.join(".codex").join("config.toml")
}

fn codex_auth_path(home: &Path) -> PathBuf {
    home.join(".codex").join("auth.json")
}

fn copilot_config_path(home: &Path) -> PathBuf {
    home.join(".config")
        .join("Code")
        .join("User")
        .join("chatLanguageModels.json")
}

#[tokio::test]
async fn claude_settings_get_reports_not_installed_without_binary_or_config() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/claude-settings",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "installed": false,
            "settings": null,
            "message": "Claude CLI is not installed"
        })
    );
}

#[tokio::test]
async fn claude_settings_post_get_and_delete_match_9router_behavior() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    let settings_path = claude_settings_path(home.path());
    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    std::fs::write(
        &settings_path,
        serde_json::to_vec_pretty(&json!({
            "foo": "bar",
            "env": {
                "KEEP": "1",
                "ANTHROPIC_DEFAULT_OPUS_MODEL": "old-opus"
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let app = openproxy::build_app(app_state().await);
    let post = app
        .clone()
        .oneshot(authorized_request(
            Method::POST,
            "/api/cli-tools/claude-settings",
            Body::from(
                r#"{"env":{"ANTHROPIC_BASE_URL":"https://proxy.example.com","ANTHROPIC_AUTH_TOKEN":"token-123","OTHER":"value"}}"#,
            ),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(post).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "message": "Settings updated successfully"
        })
    );

    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(saved["hasCompletedOnboarding"], true);
    assert_eq!(saved["foo"], "bar");
    assert_eq!(saved["env"]["KEEP"], "1");
    assert_eq!(saved["env"]["OTHER"], "value");
    assert_eq!(
        saved["env"]["ANTHROPIC_BASE_URL"],
        "https://proxy.example.com/v1"
    );

    let get = app
        .clone()
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/claude-settings",
            Body::empty(),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(get).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["installed"], true);
    assert_eq!(json["has9Router"], true);
    assert_eq!(
        json["settingsPath"],
        settings_path.to_string_lossy().to_string()
    );

    let delete = app
        .clone()
        .oneshot(authorized_request(
            Method::DELETE,
            "/api/cli-tools/claude-settings",
            Body::empty(),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(delete).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "message": "Settings reset successfully"
        })
    );

    let reset: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(reset["env"]["KEEP"], "1");
    assert_eq!(reset["env"]["OTHER"], "value");
    assert!(reset["env"].get("ANTHROPIC_BASE_URL").is_none());
    assert!(reset["env"].get("ANTHROPIC_AUTH_TOKEN").is_none());
    assert!(reset["env"].get("ANTHROPIC_DEFAULT_OPUS_MODEL").is_none());
}

#[tokio::test]
async fn hermes_settings_get_reports_not_installed_without_binary_or_config() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/hermes-settings",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "installed": false,
            "settings": null,
            "message": "Hermes Agent is not installed"
        })
    );
}

#[tokio::test]
async fn hermes_settings_post_get_and_delete_preserve_other_files() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    let config_path = hermes_config_path(home.path());
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "foo: bar\n").unwrap();

    let app = openproxy::build_app(app_state().await);
    let post = app
        .clone()
        .oneshot(authorized_request(
            Method::POST,
            "/api/cli-tools/hermes-settings",
            Body::from(
                r#"{"baseUrl":"http://127.0.0.1:4623","apiKey":"sk-test","model":"oa/gpt-4.1"}"#,
            ),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(post).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(
        json["configPath"],
        config_path.to_string_lossy().to_string()
    );

    let saved_yaml = std::fs::read_to_string(&config_path).unwrap();
    assert!(saved_yaml.contains("model:"));
    assert!(saved_yaml.contains("default: \"oa/gpt-4.1\""));
    assert!(saved_yaml.contains("provider: \"custom\""));
    assert!(saved_yaml.contains("base_url: \"http://127.0.0.1:4623/v1\""));
    assert!(saved_yaml.contains("foo: bar"));
    assert_eq!(
        std::fs::read_to_string(hermes_env_path(home.path())).unwrap(),
        "OPENAI_API_KEY=sk-test\n"
    );

    let get = app
        .clone()
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/hermes-settings",
            Body::empty(),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(get).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["installed"], true);
    assert_eq!(json["has9Router"], true);
    assert_eq!(json["settings"]["model"]["default"], "oa/gpt-4.1");
    assert_eq!(json["settings"]["model"]["provider"], "custom");
    assert_eq!(
        json["settings"]["model"]["base_url"],
        "http://127.0.0.1:4623/v1"
    );

    let delete = app
        .clone()
        .oneshot(authorized_request(
            Method::DELETE,
            "/api/cli-tools/hermes-settings",
            Body::empty(),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(delete).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "message": "9router model block removed"
        })
    );

    let reset_yaml = std::fs::read_to_string(&config_path).unwrap();
    assert!(!reset_yaml.contains("model:"));
    assert_eq!(reset_yaml.trim(), "foo: bar");
    assert_eq!(
        std::fs::read_to_string(hermes_env_path(home.path())).unwrap(),
        "OPENAI_API_KEY=sk-test\n"
    );
}

#[tokio::test]
async fn codex_settings_get_reports_not_installed_without_binary_or_config() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/codex-settings",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "installed": false,
            "config": null,
            "message": "Codex CLI is not installed"
        })
    );
}

#[tokio::test]
async fn codex_settings_post_get_and_delete_match_9router_file_behavior() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    let config_path = codex_config_path(home.path());
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(
        &config_path,
        "[existing]\nvalue = \"keep\"\nmodel = \"other\"\n",
    )
    .unwrap();
    std::fs::write(
        codex_auth_path(home.path()),
        serde_json::to_vec_pretty(&json!({
            "refresh_token": "keep-me",
            "auth_mode": "chatgpt"
        }))
        .unwrap(),
    )
    .unwrap();

    let app = openproxy::build_app(app_state().await);
    let post = app
        .clone()
        .oneshot(authorized_request(
            Method::POST,
            "/api/cli-tools/codex-settings",
            Body::from(
                r#"{"baseUrl":"https://proxy.example.com","apiKey":"sk-9router","model":"oa/gpt-4.1","subagentModel":"oa/gpt-4.1-mini"}"#,
            ),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(post).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "message": "Codex settings applied successfully!",
            "configPath": config_path.to_string_lossy().to_string()
        })
    );

    let saved_config = std::fs::read_to_string(&config_path).unwrap();
    assert!(saved_config.contains("model = \"oa/gpt-4.1\""));
    assert!(saved_config.contains("model_provider = \"9router\""));
    assert!(saved_config.contains("[model_providers.9router]"));
    assert!(saved_config.contains("base_url = \"https://proxy.example.com/v1\""));
    assert!(saved_config.contains("wire_api = \"responses\""));
    assert!(saved_config.contains("[agents.subagent]"));
    assert!(saved_config.contains("model = \"oa/gpt-4.1-mini\""));
    assert!(saved_config.contains("[existing]"));

    let saved_auth: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(codex_auth_path(home.path())).unwrap())
            .unwrap();
    assert_eq!(saved_auth["OPENAI_API_KEY"], "sk-9router");
    assert_eq!(saved_auth["auth_mode"], "apikey");
    assert_eq!(saved_auth["refresh_token"], "keep-me");

    let get = app
        .clone()
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/codex-settings",
            Body::empty(),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(get).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["installed"], true);
    assert_eq!(json["has9Router"], true);
    assert_eq!(
        json["configPath"],
        config_path.to_string_lossy().to_string()
    );
    assert_eq!(json["config"], saved_config);

    let delete = app
        .clone()
        .oneshot(authorized_request(
            Method::DELETE,
            "/api/cli-tools/codex-settings",
            Body::empty(),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(delete).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "message": "9Router settings removed successfully"
        })
    );

    let reset_config = std::fs::read_to_string(&config_path).unwrap();
    assert!(!reset_config.contains("model_provider = \"9router\""));
    assert!(!reset_config.contains("[model_providers.9router]"));
    assert!(!reset_config.contains("[agents.subagent]"));
    assert!(reset_config.contains("[existing]"));

    let reset_auth: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(codex_auth_path(home.path())).unwrap())
            .unwrap();
    assert!(reset_auth.get("OPENAI_API_KEY").is_none());
    assert!(reset_auth.get("auth_mode").is_none());
    assert_eq!(reset_auth["refresh_token"], "keep-me");
}

#[tokio::test]
async fn copilot_settings_get_reports_installed_without_existing_config() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());

    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/copilot-settings",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "installed": true,
            "config": null,
            "has9Router": false,
            "configPath": copilot_config_path(home.path()).to_string_lossy().to_string(),
            "currentModel": null,
            "currentUrl": null
        })
    );
}

#[tokio::test]
async fn copilot_settings_post_get_and_delete_match_9router_file_behavior() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());

    let config_path = copilot_config_path(home.path());
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(
        &config_path,
        serde_json::to_vec_pretty(&json!([
            {
                "name": "Other",
                "vendor": "other",
                "models": [{ "id": "other/model" }]
            },
            {
                "name": "9Router",
                "vendor": "azure",
                "models": [{ "id": "old/model", "url": "https://old.example.com/chat/completions#models.ai.azure.com" }]
            }
        ]))
        .unwrap(),
    )
    .unwrap();

    let app = openproxy::build_app(app_state().await);
    let post = app
        .clone()
        .oneshot(authorized_request(
            Method::POST,
            "/api/cli-tools/copilot-settings",
            Body::from(
                r#"{"baseUrl":"https://proxy.example.com/v1","apiKey":"sk-9router","models":["oa/gpt-4.1","oa/gpt-4.1-mini"]}"#,
            ),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(post).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "message": "Copilot settings applied! Reload VS Code to take effect.",
            "configPath": config_path.to_string_lossy().to_string(),
        })
    );

    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    let saved_array = saved.as_array().unwrap();
    assert_eq!(saved_array.len(), 2);
    assert_eq!(saved_array[0]["name"], "Other");
    assert_eq!(saved_array[1]["name"], "9Router");
    assert_eq!(saved_array[1]["vendor"], "azure");
    assert_eq!(saved_array[1]["apiKey"], "sk-9router");
    assert_eq!(saved_array[1]["models"][0]["id"], "oa/gpt-4.1");
    assert_eq!(saved_array[1]["models"][0]["name"], "oa/gpt-4.1");
    assert_eq!(
        saved_array[1]["models"][0]["url"],
        "https://proxy.example.com/v1/chat/completions#models.ai.azure.com"
    );
    assert_eq!(saved_array[1]["models"][0]["toolCalling"], true);
    assert_eq!(saved_array[1]["models"][0]["vision"], false);
    assert_eq!(saved_array[1]["models"][0]["maxInputTokens"], 128000);
    assert_eq!(saved_array[1]["models"][0]["maxOutputTokens"], 16000);
    assert_eq!(saved_array[1]["models"][1]["id"], "oa/gpt-4.1-mini");

    let get = app
        .clone()
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/copilot-settings",
            Body::empty(),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(get).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["installed"], true);
    assert_eq!(json["has9Router"], true);
    assert_eq!(json["config"], saved);
    assert_eq!(json["currentModel"], "oa/gpt-4.1");
    assert_eq!(
        json["currentUrl"],
        "https://proxy.example.com/v1/chat/completions#models.ai.azure.com"
    );

    let delete = app
        .clone()
        .oneshot(authorized_request(
            Method::DELETE,
            "/api/cli-tools/copilot-settings",
            Body::empty(),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(delete).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "message": "9Router removed from Copilot config"
        })
    );

    let reset: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(
        reset,
        json!([
            {
                "name": "Other",
                "vendor": "other",
                "models": [{ "id": "other/model" }]
            }
        ])
    );
}
