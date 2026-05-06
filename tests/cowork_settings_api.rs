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

struct HomeEnvGuard {
    old_home: Option<std::ffi::OsString>,
}

impl HomeEnvGuard {
    fn new(home: &Path) -> Self {
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        Self { old_home }
    }
}

impl Drop for HomeEnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.old_home.take() {
            std::env::set_var("HOME", value);
        } else {
            std::env::remove_var("HOME");
        }
    }
}

fn linux_claude_root(home: &Path) -> PathBuf {
    home.join(".config").join("Claude")
}

fn linux_cowork_root(home: &Path) -> PathBuf {
    home.join(".config").join("Claude-3p")
}

#[tokio::test]
async fn cowork_settings_get_returns_not_installed_without_claude_dirs() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let _guard = HomeEnvGuard::new(home.path());

    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/cowork-settings",
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
            "message": "Claude Desktop (Cowork mode) not detected"
        })
    );
}

#[tokio::test]
async fn cowork_settings_post_bootstraps_and_get_reads_config() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let _guard = HomeEnvGuard::new(home.path());

    let app = openproxy::build_app(app_state().await);
    let response = app
        .clone()
        .oneshot(authorized_request(
            Method::POST,
            "/api/cli-tools/cowork-settings",
            Body::from(
                r#"{"baseUrl":"https://proxy.example.com/v1","apiKey":"sk-test","models":["oa/gpt-4.1","oa/gpt-4.1-mini"]}"#,
            ),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["bootstrapped"], true);

    let config_path = PathBuf::from(json["configPath"].as_str().unwrap());
    assert!(config_path.exists());
    assert!(linux_cowork_root(home.path())
        .join("configLibrary")
        .join("_meta.json")
        .exists());

    let desktop_config = linux_claude_root(home.path()).join("claude_desktop_config.json");
    let desktop_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(desktop_config).unwrap()).unwrap();
    assert_eq!(desktop_json["deploymentMode"], "3p");

    let response = app
        .clone()
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/cowork-settings",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["installed"], true);
    assert_eq!(json["has9Router"], true);
    assert_eq!(json["cowork"]["baseUrl"], "https://proxy.example.com/v1");
    assert_eq!(
        json["cowork"]["models"],
        json!(["oa/gpt-4.1", "oa/gpt-4.1-mini"])
    );
    assert_eq!(json["cowork"]["provider"], "gateway");
}

#[tokio::test]
async fn cowork_settings_post_rejects_localhost_urls() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let _guard = HomeEnvGuard::new(home.path());

    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::POST,
            "/api/cli-tools/cowork-settings",
            Body::from(
                r#"{"baseUrl":"http://127.0.0.1:4623/v1","apiKey":"sk-test","models":["oa/gpt-4.1"]}"#,
            ),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        json,
        json!({
            "error": "Claude Cowork sandbox cannot reach localhost. Enable Tunnel/Cloud Endpoint or use Tailscale/VPS."
        })
    );
}

#[tokio::test]
async fn cowork_settings_delete_clears_existing_config() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let _guard = HomeEnvGuard::new(home.path());

    let app = openproxy::build_app(app_state().await);
    let post_response = app
        .clone()
        .oneshot(authorized_request(
            Method::POST,
            "/api/cli-tools/cowork-settings",
            Body::from(
                r#"{"baseUrl":"https://proxy.example.com/v1","apiKey":"sk-test","models":["oa/gpt-4.1"]}"#,
            ),
        ))
        .await
        .unwrap();
    let (_, post_json) = response_json(post_response).await;
    let config_path = PathBuf::from(post_json["configPath"].as_str().unwrap());

    let delete_response = app
        .clone()
        .oneshot(authorized_request(
            Method::DELETE,
            "/api/cli-tools/cowork-settings",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(delete_response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "message": "Cowork config reset"
        })
    );
    assert_eq!(std::fs::read_to_string(config_path).unwrap().trim(), "{}");

    let get_response = app
        .clone()
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/cowork-settings",
            Body::empty(),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(get_response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["installed"], true);
    assert_eq!(json["has9Router"], false);
    assert_eq!(json["cowork"]["baseUrl"], serde_json::Value::Null);
}
