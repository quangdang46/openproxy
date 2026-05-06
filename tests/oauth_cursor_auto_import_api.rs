use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use once_cell::sync::Lazy;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use rusqlite::Connection;
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;

static ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

struct EnvVarGuard {
    key: &'static str,
    old_value: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &Path) -> Self {
        let old_value = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, old_value }
    }

    fn set_str(key: &'static str, value: &str) -> Self {
        let old_value = std::env::var(key).ok();
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

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    AppState::new(db)
}

fn request() -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri("/api/oauth/cursor/auto-import")
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

fn cursor_db_path(home: &Path) -> PathBuf {
    match std::env::consts::OS {
        "macos" => home
            .join("Library")
            .join("Application Support")
            .join("Cursor")
            .join("User")
            .join("globalStorage")
            .join("state.vscdb"),
        "windows" => home
            .join("AppData")
            .join("Roaming")
            .join("Cursor")
            .join("User")
            .join("globalStorage")
            .join("state.vscdb"),
        _ => home
            .join(".config")
            .join("Cursor")
            .join("User")
            .join("globalStorage")
            .join("state.vscdb"),
    }
}

fn checked_locations(home: &Path) -> Vec<String> {
    match std::env::consts::OS {
        "macos" => vec![
            home.join("Library")
                .join("Application Support")
                .join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb")
                .to_string_lossy()
                .to_string(),
            home.join("Library")
                .join("Application Support")
                .join("Cursor - Insiders")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb")
                .to_string_lossy()
                .to_string(),
        ],
        "windows" => vec![
            home.join("AppData")
                .join("Roaming")
                .join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb")
                .to_string_lossy()
                .to_string(),
            home.join("AppData")
                .join("Roaming")
                .join("Cursor - Insiders")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb")
                .to_string_lossy()
                .to_string(),
            home.join("AppData")
                .join("Local")
                .join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb")
                .to_string_lossy()
                .to_string(),
            home.join("AppData")
                .join("Local")
                .join("Programs")
                .join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb")
                .to_string_lossy()
                .to_string(),
        ],
        _ => vec![
            home.join(".config")
                .join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb")
                .to_string_lossy()
                .to_string(),
            home.join(".config")
                .join("cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb")
                .to_string_lossy()
                .to_string(),
        ],
    }
}

fn create_cursor_db(path: &Path, access_token: Option<&str>, machine_id: Option<&str>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute("CREATE TABLE itemTable (key TEXT, value TEXT)", [])
        .unwrap();
    if let Some(access_token) = access_token {
        connection
            .execute(
                "INSERT INTO itemTable(key, value) VALUES(?1, ?2)",
                ("cursorAuth/accessToken", access_token),
            )
            .unwrap();
    }
    if let Some(machine_id) = machine_id {
        connection
            .execute(
                "INSERT INTO itemTable(key, value) VALUES(?1, ?2)",
                ("storage.serviceMachineId", machine_id),
            )
            .unwrap();
    }
}

fn install_cursor_desktop_file(home: &Path) {
    if std::env::consts::OS == "linux" {
        let desktop = home
            .join(".local")
            .join("share")
            .join("applications")
            .join("cursor.desktop");
        std::fs::create_dir_all(desktop.parent().unwrap()).unwrap();
        std::fs::write(desktop, "[Desktop Entry]\nName=Cursor\n").unwrap();
    }
}

#[tokio::test]
async fn cursor_auto_import_returns_missing_database_error_like_9router() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", home.path());
    let _path = EnvVarGuard::set_str("PATH", home.path().to_string_lossy().as_ref());

    let app = openproxy::build_app(app_state().await);
    let response = app.oneshot(request()).await.unwrap();
    let (status, json) = response_json(response).await;

    let expected = format!(
        "Cursor database not found. Checked locations:\n{}\n\nMake sure Cursor IDE is installed and opened at least once.",
        checked_locations(home.path()).join("\n")
    );
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "found": false, "error": expected }));
}

#[tokio::test]
async fn cursor_auto_import_returns_not_installed_error_on_linux_like_9router() {
    if std::env::consts::OS != "linux" {
        return;
    }

    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", home.path());
    let _path = EnvVarGuard::set_str("PATH", home.path().to_string_lossy().as_ref());
    create_cursor_db(
        &cursor_db_path(home.path()),
        Some("\"token\""),
        Some("\"machine-id\""),
    );

    let app = openproxy::build_app(app_state().await);
    let response = app.oneshot(request()).await.unwrap();
    let (status, json) = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "found": false,
            "error": "Cursor config files found but Cursor IDE does not appear to be installed. Skipping auto-import."
        })
    );
}

#[tokio::test]
async fn cursor_auto_import_reads_tokens_from_local_db_like_9router() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", home.path());
    let _path = EnvVarGuard::set_str("PATH", home.path().to_string_lossy().as_ref());
    install_cursor_desktop_file(home.path());
    create_cursor_db(
        &cursor_db_path(home.path()),
        Some("\"cursor-access-token\""),
        Some("\"550e8400-e29b-41d4-a716-446655440000\""),
    );

    let app = openproxy::build_app(app_state().await);
    let response = app.oneshot(request()).await.unwrap();
    let (status, json) = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "found": true,
            "accessToken": "cursor-access-token",
            "machineId": "550e8400-e29b-41d4-a716-446655440000"
        })
    );
}

#[tokio::test]
async fn cursor_auto_import_falls_back_to_manual_mode_when_tokens_missing() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", home.path());
    let _path = EnvVarGuard::set_str("PATH", home.path().to_string_lossy().as_ref());
    install_cursor_desktop_file(home.path());
    create_cursor_db(&cursor_db_path(home.path()), None, None);

    let app = openproxy::build_app(app_state().await);
    let response = app.oneshot(request()).await.unwrap();
    let (status, json) = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "found": false,
            "windowsManual": true,
            "dbPath": cursor_db_path(home.path()).to_string_lossy().to_string()
        })
    );
}
