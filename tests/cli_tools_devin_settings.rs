#![allow(clippy::await_holding_lock)]
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use once_cell::sync::Lazy;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use serde_json::Value;
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
        extra: std::collections::BTreeMap::new(),
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

/// First entry of the executor's candidate list, resolved against `home`.
fn devin_candidate(home: &Path) -> PathBuf {
    home.join(".local/share/devin/bin/devin")
}

/// The route spawns `--version` on the resolved path, so the stub has to be
/// runnable. No-op on Windows, where the executor's candidates don't apply.
fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}

async fn get_devin_settings() -> (StatusCode, Value) {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/devin-settings",
            Body::empty(),
        ))
        .await
        .unwrap();
    response_json(response).await
}

#[tokio::test]
async fn devin_settings_get_reports_install_status() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    let (status, json) = get_devin_settings().await;

    assert_eq!(status, StatusCode::OK);
    // Detection probe, not a settings blob: `installed` is a bool and `path` is
    // the resolved binary (null when nothing is found).
    assert!(
        json["installed"].is_boolean(),
        "installed must be a bool: {json}"
    );
    assert!(
        json["path"].is_string() || json["path"].is_null(),
        "path must be a string or null: {json}"
    );
}

#[tokio::test]
async fn devin_settings_is_reachable_without_a_devin_install() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    let (status, json) = get_devin_settings().await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        json["installed"].is_boolean(),
        "installed must be a bool: {json}"
    );
    assert!(
        json["path"].is_string() || json["path"].is_null(),
        "path must be a string or null: {json}"
    );
    // Same shape whether or not CI happens to have Devin installed — that is
    // the point of the route, so assert the keys rather than one machine's value.
    assert!(
        json["message"].is_string(),
        "message must be a string: {json}"
    );
    assert!(json["version"].is_string() || json["version"].is_null());
}

#[tokio::test]
async fn devin_settings_reports_the_installer_path_when_the_binary_is_present() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    // Empty PATH rules out the `which devin` branch, so a hit here can only
    // come from the executor's own candidate list.
    let binary = devin_candidate(home.path());
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::write(&binary, b"#!/bin/sh\necho 9.9.9\n").unwrap();
    make_executable(&binary);

    let (status, json) = get_devin_settings().await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["installed"], true);
    assert_eq!(json["path"], binary.to_string_lossy().as_ref());
    assert_eq!(json["version"], "9.9.9");
}

#[tokio::test]
async fn devin_settings_reports_not_installed_when_nothing_resolves() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    let (status, json) = get_devin_settings().await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["installed"], false);
    assert_eq!(json["path"], Value::Null);
    assert_eq!(json["version"], Value::Null);
    assert_eq!(json["installUrl"], "https://cli.devin.ai");
}

#[tokio::test]
async fn devin_settings_requires_dashboard_or_management_auth() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/cli-tools/devin-settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
