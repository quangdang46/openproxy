use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use once_cell::sync::Lazy;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;

static ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

struct EnvVarGuard {
    old_home: Option<String>,
}

impl EnvVarGuard {
    fn set_home(value: &Path) -> Self {
        let old_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", value);
        Self { old_home }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = self.old_home.take() {
            std::env::set_var("HOME", value);
        } else {
            std::env::remove_var("HOME");
        }
    }
}

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    AppState::new(db)
}

fn request() -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri("/api/oauth/kiro/auto-import")
        .body(Body::empty())
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

fn cache_dir(home: &Path) -> std::path::PathBuf {
    home.join(".aws").join("sso").join("cache")
}

fn write_cache_file(home: &Path, name: &str, content: &str) {
    let path = cache_dir(home).join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[tokio::test]
async fn kiro_auto_import_reports_missing_cache_like_9router() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set_home(home.path());

    let app = openproxy::build_app(app_state().await);
    let response = app.oneshot(request()).await.unwrap();
    let (status, json) = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "found": false,
            "error": "AWS SSO cache not found. Please login to Kiro IDE first."
        })
    );
}

#[tokio::test]
async fn kiro_auto_import_prefers_kiro_auth_token_file_like_9router() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set_home(home.path());
    write_cache_file(
        home.path(),
        "kiro-auth-token.json",
        r#"{"refreshToken":"aorAAAAAG-primary-token"}"#,
    );
    write_cache_file(
        home.path(),
        "other.json",
        r#"{"refreshToken":"aorAAAAAG-secondary-token"}"#,
    );

    let app = openproxy::build_app(app_state().await);
    let response = app.oneshot(request()).await.unwrap();
    let (status, json) = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "found": true,
            "refreshToken": "aorAAAAAG-primary-token",
            "source": "kiro-auth-token.json"
        })
    );
}

#[tokio::test]
async fn kiro_auto_import_scans_other_json_files_when_needed() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set_home(home.path());
    write_cache_file(
        home.path(),
        "kiro-auth-token.json",
        r#"{"refreshToken":"invalid"}"#,
    );
    write_cache_file(home.path(), "broken.json", "{");
    write_cache_file(home.path(), "notes.txt", "ignore me");
    write_cache_file(
        home.path(),
        "match.json",
        r#"{"refreshToken":"aorAAAAAG-fallback-token"}"#,
    );

    let app = openproxy::build_app(app_state().await);
    let response = app.oneshot(request()).await.unwrap();
    let (status, json) = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "found": true,
            "refreshToken": "aorAAAAAG-fallback-token",
            "source": "match.json"
        })
    );
}

#[tokio::test]
async fn kiro_auto_import_reports_missing_token_when_cache_has_no_match() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set_home(home.path());
    write_cache_file(home.path(), "one.json", r#"{"refreshToken":"invalid"}"#);
    write_cache_file(
        home.path(),
        "two.json",
        r#"{"refreshToken":"something-else"}"#,
    );

    let app = openproxy::build_app(app_state().await);
    let response = app.oneshot(request()).await.unwrap();
    let (status, json) = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "found": false,
            "error": "Kiro token not found in AWS SSO cache. Please login to Kiro IDE first."
        })
    );
}
