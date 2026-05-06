use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    response::Response,
    routing::{get, post},
    Json, Router,
};
use base64::{
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
    Engine,
};
use rand::RngCore;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Command;
use std::str;

use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::oauth::device_code;
use crate::oauth::pending::PendingOAuthFlow;
use crate::oauth::providers;
use crate::oauth::{OAuthProviderConfig, TokenResponse};
use crate::server::auth::require_api_key;
use crate::server::state::AppState;
use crate::types::ProviderConnection;

const PKCE_FLOW_TTL_SECS: i64 = 600;
const DEVICE_FLOW_TTL_SECS: i64 = 900;

#[derive(Debug, Deserialize)]
pub struct StartQuery {
    pub redirect_uri: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeviceCodeBody {
    pub redirect_uri: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RefreshBody {
    pub refresh_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StartResponse {
    pub auth_url: String,
    pub state: String,
    pub provider: String,
    pub expires_in: u64,
}

#[derive(Debug, Serialize)]
pub struct CallbackResponse {
    pub success: bool,
    pub provider: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
    pub expires_in: u64,
}

#[derive(Debug, Serialize)]
pub struct PollResponse {
    pub success: bool,
    pub provider: String,
    pub expires_in: Option<u64>,
    pub pending: Option<bool>,
    pub retry_after: Option<u64>,
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RefreshResponse {
    pub success: bool,
    pub access_token: String,
    pub expires_in: u64,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub provider: String,
    pub connected: bool,
    pub auth_type: String,
    pub expires_at: Option<String>,
    pub needs_refresh: Option<bool>,
    pub scope: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OAuthError {
    pub error: OAuthErrorDetail,
}

#[derive(Debug, Serialize)]
pub struct OAuthErrorDetail {
    pub message: String,
    pub code: String,
    pub provider: String,
}

fn make_error(message: &str, code: &str, provider: &str) -> Json<OAuthError> {
    Json(OAuthError {
        error: OAuthErrorDetail {
            message: message.to_string(),
            code: code.to_string(),
            provider: provider.to_string(),
        },
    })
}

fn make_error_response(status: StatusCode, message: &str, code: &str, provider: &str) -> Response {
    (status, make_error(message, code, provider)).into_response()
}

fn generate_code_verifier() -> String {
    let mut random_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut random_bytes);
    let charset = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    random_bytes
        .iter()
        .map(|&b| charset[(b as usize) % charset.len()] as char)
        .collect()
}

fn generate_code_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let hash = hasher.finalize();
    URL_SAFE_NO_PAD.encode(hash)
}

fn generate_state() -> String {
    let mut random_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut random_bytes);
    URL_SAFE_NO_PAD.encode(random_bytes)
}

fn get_provider_config(provider: &str) -> Option<OAuthProviderConfig> {
    providers::get_config(provider)
}

fn is_pkce_provider(provider: &str) -> bool {
    matches!(provider, "claude" | "codex" | "gitlab")
}

fn is_device_code_provider(provider: &str) -> bool {
    matches!(
        provider,
        "github" | "kiro" | "kimi-coding" | "kilocode" | "codebuddy"
    )
}

fn iflow_api_base_url() -> String {
    std::env::var("OPENPROXY_IFLOW_API_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "https://platform.iflow.cn".to_string())
        .trim_end_matches('/')
        .to_string()
}

fn kiro_auth_service_base_url() -> String {
    std::env::var("OPENPROXY_KIRO_AUTH_SERVICE_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "https://prod.us-east-1.auth.desktop.kiro.dev".to_string())
        .trim_end_matches('/')
        .to_string()
}

const GITLAB_DEFAULT_BASE: &str = "https://gitlab.com";
const CURSOR_ACCESS_TOKEN_KEYS: &[&str] = &["cursorAuth/accessToken", "cursorAuth/token"];
const CURSOR_MACHINE_ID_KEYS: &[&str] = &[
    "storage.serviceMachineId",
    "storage.machineId",
    "telemetry.machineId",
];

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

async fn store_connection(
    db: &crate::db::Db,
    account_id: &str,
    provider: &str,
    token_response: &TokenResponse,
    redirect_uri: Option<&str>,
) -> anyhow::Result<()> {
    let provider_config = get_provider_config(provider);
    let _client_id = provider_config
        .as_ref()
        .and_then(|c| c.extra_params.get("client_id"))
        .map(|v| v.as_str())
        .unwrap_or("openproxy")
        .to_string();

    let _now = now_secs();
    let expires_at = token_response.expires_in.map(|secs| {
        let expires = chrono::Utc::now() + chrono::Duration::seconds(secs);
        expires.to_rfc3339()
    });

    let _redirect_uri = redirect_uri
        .map(|s| s.to_string())
        .or_else(|| {
            provider_config
                .as_ref()
                .and_then(|c| c.extra_params.get("redirect_uri"))
                .map(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "http://localhost:4623/oauth/callback".to_string());

    db.update(|db| {
        let snapshot = db;
        if let Some(conn_idx) = snapshot
            .provider_connections
            .iter()
            .position(|conn| conn.provider == provider && conn.id.contains(account_id))
        {
            snapshot.provider_connections[conn_idx].access_token =
                Some(token_response.access_token.clone());
            snapshot.provider_connections[conn_idx].refresh_token =
                token_response.refresh_token.clone();
            snapshot.provider_connections[conn_idx].expires_at = expires_at;
            snapshot.provider_connections[conn_idx].scope = token_response.scope.clone();
            snapshot.provider_connections[conn_idx].updated_at =
                Some(chrono::Utc::now().to_rfc3339());
        } else {
            let connection_id = format!("{}-{}", account_id, Uuid::new_v4());
            let connection = ProviderConnection {
                id: connection_id,
                provider: provider.to_string(),
                auth_type: "oauth".to_string(),
                name: None,
                priority: Some(100),
                is_active: Some(true),
                created_at: Some(chrono::Utc::now().to_rfc3339()),
                updated_at: Some(chrono::Utc::now().to_rfc3339()),
                display_name: None,
                email: None,
                global_priority: None,
                default_model: None,
                access_token: Some(token_response.access_token.clone()),
                refresh_token: token_response.refresh_token.clone(),
                expires_at,
                token_type: token_response.token_type.clone(),
                scope: token_response.scope.clone(),
                id_token: token_response.id_token.clone(),
                project_id: None,
                api_key: None,
                test_status: None,
                last_tested: None,
                last_error: None,
                last_error_at: None,
                rate_limited_until: None,
                expires_in: token_response.expires_in,
                error_code: None,
                consecutive_use_count: None,
                backoff_level: None,
                consecutive_errors: None,
                proxy_url: None,
                proxy_label: None,
                use_connection_proxy: None,
                provider_specific_data: std::collections::BTreeMap::new(),
                extra: std::collections::BTreeMap::new(),
            };
            snapshot.provider_connections.push(connection);
        }
    })
    .await?;
    Ok(())
}

fn internal_error_response(message: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": message })),
    )
        .into_response()
}

fn next_provider_priority(connections: &[ProviderConnection], provider: &str) -> u32 {
    connections
        .iter()
        .filter(|connection| connection.provider == provider)
        .map(|connection| connection.priority.unwrap_or(0))
        .max()
        .unwrap_or(0)
        + 1
}

async fn create_imported_oauth_connection(
    db: &crate::db::Db,
    mut connection: ProviderConnection,
) -> anyhow::Result<ProviderConnection> {
    let now = chrono::Utc::now().to_rfc3339();
    let provider = connection.provider.clone();
    let email_for_upsert = connection
        .email
        .as_deref()
        .filter(|email| !email.is_empty())
        .map(str::to_string);
    let mut saved = None;

    db.update(|db| {
        if let Some(email) = email_for_upsert.as_deref() {
            if let Some(existing) = db.provider_connections.iter_mut().find(|candidate| {
                candidate.provider == provider
                    && candidate.auth_type == "oauth"
                    && candidate.email.as_deref() == Some(email)
            }) {
                existing.display_name = connection.display_name.clone();
                existing.email = connection.email.clone();
                existing.access_token = connection.access_token.clone();
                existing.refresh_token = connection.refresh_token.clone();
                existing.expires_at = connection.expires_at.clone();
                existing.test_status = connection.test_status.clone();
                existing.token_type = connection.token_type.clone();
                existing.scope = connection.scope.clone();
                existing.id_token = connection.id_token.clone();
                existing.provider_specific_data = connection.provider_specific_data.clone();
                existing.updated_at = Some(now.clone());
                saved = Some(existing.clone());
                return;
            }
        }

        if connection.name.is_none() {
            connection.name = Some(
                connection
                    .email
                    .as_deref()
                    .filter(|email| !email.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        format!(
                            "Account {}",
                            db.provider_connections
                                .iter()
                                .filter(|candidate| candidate.provider == provider)
                                .count()
                                + 1
                        )
                    }),
            );
        }

        if connection.priority.is_none() {
            connection.priority = Some(next_provider_priority(&db.provider_connections, &provider));
        }
        if connection.id.is_empty() {
            connection.id = Uuid::new_v4().to_string();
        }
        if connection.is_active.is_none() {
            connection.is_active = Some(true);
        }
        if connection.created_at.is_none() {
            connection.created_at = Some(now.clone());
        }
        connection.updated_at = Some(now.clone());

        db.provider_connections.push(connection.clone());
        saved = Some(connection.clone());
    })
    .await?;

    saved.ok_or_else(|| anyhow::anyhow!("Failed to save provider connection"))
}

fn decode_cursor_jwt_claims(access_token: &str) -> Option<Value> {
    let mut parts = access_token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let mut padded = payload.to_string();
    while padded.len() % 4 != 0 {
        padded.push('=');
    }

    let decoded = URL_SAFE.decode(padded).ok()?;
    serde_json::from_slice(&decoded).ok()
}

fn cursor_home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn cursor_candidate_paths() -> Vec<PathBuf> {
    let home = cursor_home_dir();
    match std::env::consts::OS {
        "macos" => vec![
            home.join("Library/Application Support/Cursor/User/globalStorage/state.vscdb"),
            home.join(
                "Library/Application Support/Cursor - Insiders/User/globalStorage/state.vscdb",
            ),
        ],
        "windows" => {
            let app_data = std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("AppData").join("Roaming"));
            let local_app_data = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("AppData").join("Local"));
            vec![
                app_data
                    .join("Cursor")
                    .join("User")
                    .join("globalStorage")
                    .join("state.vscdb"),
                app_data
                    .join("Cursor - Insiders")
                    .join("User")
                    .join("globalStorage")
                    .join("state.vscdb"),
                local_app_data
                    .join("Cursor")
                    .join("User")
                    .join("globalStorage")
                    .join("state.vscdb"),
                local_app_data
                    .join("Programs")
                    .join("Cursor")
                    .join("User")
                    .join("globalStorage")
                    .join("state.vscdb"),
            ]
        }
        _ => vec![
            home.join(".config")
                .join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb"),
            home.join(".config")
                .join("cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb"),
        ],
    }
}

fn normalize_cursor_db_value(value: &str) -> String {
    match serde_json::from_str::<Value>(value) {
        Ok(Value::String(parsed)) => parsed,
        _ => value.to_string(),
    }
}

fn extract_cursor_tokens_from_db(
    db_path: &std::path::Path,
) -> Result<(Option<String>, Option<String>), rusqlite::Error> {
    let connection =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;

    let query = |keys: &[&str]| -> Result<Option<String>, rusqlite::Error> {
        for key in keys {
            let value: Option<String> = connection
                .query_row(
                    "SELECT value FROM itemTable WHERE key=? LIMIT 1",
                    [key],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(value) = value {
                return Ok(Some(normalize_cursor_db_value(&value)));
            }
        }
        Ok(None)
    };

    Ok((
        query(CURSOR_ACCESS_TOKEN_KEYS)?,
        query(CURSOR_MACHINE_ID_KEYS)?,
    ))
}

fn cursor_is_installed() -> bool {
    if std::env::consts::OS != "linux" {
        return true;
    }

    if Command::new("which")
        .arg("cursor")
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
    {
        return true;
    }

    cursor_home_dir()
        .join(".local")
        .join("share")
        .join("applications")
        .join("cursor.desktop")
        .is_file()
}

fn kiro_sso_cache_path() -> PathBuf {
    cursor_home_dir().join(".aws").join("sso").join("cache")
}

fn cursor_import_instructions() -> Value {
    json!({
        "provider": "cursor",
        "method": "import_token",
        "instructions": {
            "title": "How to get your Cursor token",
            "steps": [
                "1. Open Cursor IDE and make sure you're logged in",
                "2. Find the state.vscdb file:",
                "   - Linux: ~/.config/Cursor/User/globalStorage/state.vscdb",
                "   - macOS: /Users/<user>/Library/Application Support/Cursor/User/globalStorage/state.vscdb",
                "   - Windows: %APPDATA%\\Cursor\\User\\globalStorage\\state.vscdb",
                "3. Open the database with SQLite browser or CLI:",
                "   sqlite3 state.vscdb \"SELECT value FROM itemTable WHERE key='cursorAuth/accessToken'\"",
                "4. Also get the machine ID:",
                "   sqlite3 state.vscdb \"SELECT value FROM itemTable WHERE key='storage.serviceMachineId'\"",
                "5. Paste both values in the form below"
            ],
            "alternativeMethod": [
                "Or use this one-liner to get both values:",
                "sqlite3 state.vscdb \"SELECT key, value FROM itemTable WHERE key IN ('cursorAuth/accessToken', 'storage.serviceMachineId')\""
            ]
        },
        "requiredFields": [
            {
                "name": "accessToken",
                "label": "Access Token",
                "description": "From cursorAuth/accessToken in state.vscdb",
                "type": "textarea"
            },
            {
                "name": "machineId",
                "label": "Machine ID",
                "description": "From storage.serviceMachineId in state.vscdb",
                "type": "text"
            }
        ]
    })
}

fn validate_cursor_import_token(
    access_token: &str,
    machine_id: &str,
) -> Result<(String, String), String> {
    if access_token.is_empty() {
        return Err("Access token is required".to_string());
    }
    if machine_id.is_empty() {
        return Err("Machine ID is required".to_string());
    }
    if access_token.len() < 50 {
        return Err("Invalid token format. Token appears too short.".to_string());
    }

    let normalized_machine_id = machine_id.replace('-', "");
    if normalized_machine_id.len() < 32
        || !normalized_machine_id
            .chars()
            .all(|ch| ch.is_ascii_hexdigit())
    {
        return Err("Invalid machine ID format. Expected UUID format.".to_string());
    }

    Ok((access_token.to_string(), machine_id.to_string()))
}

async fn iflow_cookie_auth(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> Response {
    let body = match axum::body::to_bytes(request.into_body(), 64 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) => return internal_error_response(error.to_string()),
    };

    let body: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => return internal_error_response(error.to_string()),
    };

    let Some(cookie) = body.get("cookie").and_then(Value::as_str) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Cookie is required" })),
        )
            .into_response();
    };

    let trimmed = cookie.trim();
    if !trimmed.contains("BXAuth=") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Cookie must contain BXAuth field" })),
        )
            .into_response();
    }

    let mut normalized_cookie = trimmed.to_string();
    if !normalized_cookie.ends_with(';') {
        normalized_cookie.push(';');
    }

    let base_url = iflow_api_base_url();
    let client = reqwest::Client::new();

    let get_response = match client
        .get(format!("{base_url}/api/openapi/apikey"))
        .header("Cookie", normalized_cookie.clone())
        .header("Accept", "application/json, text/plain, */*")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36",
        )
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .header("Accept-Encoding", "gzip, deflate, br")
        .header("Connection", "keep-alive")
        .header("Sec-Fetch-Dest", "empty")
        .header("Sec-Fetch-Mode", "cors")
        .header("Sec-Fetch-Site", "same-origin")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => return internal_error_response(error.to_string()),
    };

    if !get_response.status().is_success() {
        let status = get_response.status();
        let error_text = get_response.text().await.unwrap_or_default();
        return (
            status,
            Json(json!({
                "error": format!("Failed to fetch API key info: {}", error_text)
            })),
        )
            .into_response();
    }

    let get_result: Value = match get_response.json().await {
        Ok(value) => value,
        Err(error) => return internal_error_response(error.to_string()),
    };

    if get_result.get("success").and_then(Value::as_bool) != Some(true) {
        let message = get_result
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("");
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!("API key fetch failed: {message}")
            })),
        )
            .into_response();
    }

    let key_data = get_result.get("data").cloned().unwrap_or(Value::Null);
    let Some(key_name) = key_data.get("name").and_then(Value::as_str) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing name in API key info" })),
        )
            .into_response();
    };

    let post_response = match client
        .post(format!("{base_url}/api/openapi/apikey"))
        .header("Cookie", normalized_cookie.clone())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/plain, */*")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36",
        )
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .header("Accept-Encoding", "gzip, deflate, br")
        .header("Connection", "keep-alive")
        .header("Origin", base_url.clone())
        .header("Referer", format!("{base_url}/"))
        .json(&json!({ "name": key_name }))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => return internal_error_response(error.to_string()),
    };

    if !post_response.status().is_success() {
        let status = post_response.status();
        let error_text = post_response.text().await.unwrap_or_default();
        return (
            status,
            Json(json!({
                "error": format!("Failed to refresh API key: {}", error_text)
            })),
        )
            .into_response();
    }

    let post_result: Value = match post_response.json().await {
        Ok(value) => value,
        Err(error) => return internal_error_response(error.to_string()),
    };

    if post_result.get("success").and_then(Value::as_bool) != Some(true) {
        let message = post_result
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("");
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!("API key refresh failed: {message}")
            })),
        )
            .into_response();
    }

    let refreshed_key = post_result.get("data").cloned().unwrap_or(Value::Null);
    let Some(refreshed_api_key) = refreshed_key.get("apiKey").and_then(Value::as_str) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing API key in response" })),
        )
            .into_response();
    };

    let bx_auth = normalized_cookie
        .split(';')
        .find_map(|segment| segment.trim().strip_prefix("BXAuth="))
        .unwrap_or("");
    let cookie_to_save = if bx_auth.is_empty() {
        String::new()
    } else {
        format!("BXAuth={bx_auth};")
    };

    let connection_id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let connection_name = refreshed_key
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(key_name)
        .to_string();
    let expire_time = refreshed_key
        .get("expireTime")
        .cloned()
        .unwrap_or(Value::Null);

    let result = state
        .db
        .update(|db| {
            let mut provider_specific_data = std::collections::BTreeMap::new();
            provider_specific_data
                .insert("cookie".to_string(), Value::String(cookie_to_save.clone()));
            provider_specific_data.insert("expireTime".to_string(), expire_time.clone());

            db.provider_connections.push(ProviderConnection {
                id: connection_id.clone(),
                provider: "iflow".to_string(),
                auth_type: "cookie".to_string(),
                name: Some(connection_name.clone()),
                is_active: Some(true),
                created_at: Some(now.clone()),
                updated_at: Some(now.clone()),
                email: Some(connection_name.clone()),
                api_key: Some(refreshed_api_key.to_string()),
                test_status: Some("active".to_string()),
                provider_specific_data,
                ..Default::default()
            });
        })
        .await;

    if let Err(error) = result {
        return internal_error_response(error.to_string());
    }

    let masked_api_key = format!(
        "{}...",
        refreshed_api_key.chars().take(10).collect::<String>()
    );

    Json(json!({
        "success": true,
        "connection": {
            "id": connection_id,
            "provider": "iflow",
            "email": connection_name,
            "apiKey": masked_api_key,
            "expireTime": expire_time
        }
    }))
    .into_response()
}

async fn gitlab_pat_auth(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> Response {
    let body = match axum::body::to_bytes(request.into_body(), 64 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid request body" })),
            )
                .into_response()
        }
    };

    let body: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid request body" })),
            )
                .into_response()
        }
    };

    let token = body
        .get("token")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if token.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Personal Access Token is required" })),
        )
            .into_response();
    }

    let base = body
        .get("baseUrl")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(GITLAB_DEFAULT_BASE)
        .trim_end_matches('/')
        .to_string();

    let user_response = match reqwest::Client::new()
        .get(format!("{base}/api/v4/user"))
        .header("Private-Token", token.clone())
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => return internal_error_response(error.to_string()),
    };

    if !user_response.status().is_success() {
        let err = user_response.text().await.unwrap_or_default();
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": format!("GitLab token verification failed: {err}")
            })),
        )
            .into_response();
    }

    let user: Value = match user_response.json().await {
        Ok(value) => value,
        Err(error) => return internal_error_response(error.to_string()),
    };
    let email = user
        .get("email")
        .and_then(Value::as_str)
        .or_else(|| user.get("public_email").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    let display_name = user
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| user.get("username").and_then(Value::as_str))
        .unwrap_or(email.as_str())
        .to_string();

    let connection = ProviderConnection {
        provider: "gitlab".to_string(),
        auth_type: "oauth".to_string(),
        display_name: Some(display_name),
        email: Some(email.clone()),
        access_token: Some(token),
        test_status: Some("active".to_string()),
        provider_specific_data: std::collections::BTreeMap::from([
            (
                "username".to_string(),
                Value::String(
                    user.get("username")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ),
            ),
            ("email".to_string(), Value::String(email)),
            (
                "name".to_string(),
                Value::String(
                    user.get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ),
            ),
            ("baseUrl".to_string(), Value::String(base.clone())),
            (
                "authKind".to_string(),
                Value::String("personal_access_token".to_string()),
            ),
        ]),
        ..Default::default()
    };

    match create_imported_oauth_connection(&state.db, connection).await {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(error) => internal_error_response(error.to_string()),
    }
}

async fn cursor_import_instructions_route() -> Response {
    Json(cursor_import_instructions()).into_response()
}

async fn cursor_import_auth(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> Response {
    let body = match axum::body::to_bytes(request.into_body(), 64 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) => return internal_error_response(error.to_string()),
    };

    let body: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => return internal_error_response(error.to_string()),
    };

    let Some(access_token_raw) = body.get("accessToken").and_then(Value::as_str) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Access token is required" })),
        )
            .into_response();
    };
    if access_token_raw.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Access token is required" })),
        )
            .into_response();
    }

    let Some(machine_id_raw) = body.get("machineId").and_then(Value::as_str) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Machine ID is required" })),
        )
            .into_response();
    };
    if machine_id_raw.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Machine ID is required" })),
        )
            .into_response();
    }

    let access_token = access_token_raw.trim();
    let machine_id = machine_id_raw.trim();
    let (validated_access_token, validated_machine_id) =
        match validate_cursor_import_token(access_token, machine_id) {
            Ok(value) => value,
            Err(error) => return internal_error_response(error),
        };

    let claims = decode_cursor_jwt_claims(&validated_access_token);
    let email = claims
        .as_ref()
        .and_then(|value| value.get("email"))
        .or_else(|| claims.as_ref().and_then(|value| value.get("sub")))
        .and_then(Value::as_str)
        .map(str::to_string);
    let user_id = claims
        .as_ref()
        .and_then(|value| value.get("sub"))
        .or_else(|| claims.as_ref().and_then(|value| value.get("user_id")))
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut provider_specific_data = std::collections::BTreeMap::from([
        (
            "machineId".to_string(),
            Value::String(validated_machine_id.clone()),
        ),
        (
            "authMethod".to_string(),
            Value::String("imported".to_string()),
        ),
        (
            "provider".to_string(),
            Value::String("Imported".to_string()),
        ),
    ]);
    if let Some(user_id) = user_id {
        provider_specific_data.insert("userId".to_string(), Value::String(user_id));
    }

    let connection = ProviderConnection {
        provider: "cursor".to_string(),
        auth_type: "oauth".to_string(),
        email: email.clone(),
        access_token: Some(validated_access_token),
        refresh_token: None,
        expires_at: Some((chrono::Utc::now() + chrono::Duration::seconds(86_400)).to_rfc3339()),
        test_status: Some("active".to_string()),
        provider_specific_data,
        ..Default::default()
    };

    match create_imported_oauth_connection(&state.db, connection).await {
        Ok(connection) => Json(json!({
            "success": true,
            "connection": {
                "id": connection.id,
                "provider": connection.provider,
                "email": connection.email
            }
        }))
        .into_response(),
        Err(error) => internal_error_response(error.to_string()),
    }
}

async fn cursor_auto_import_route() -> Response {
    let candidates = cursor_candidate_paths();
    let db_path = candidates
        .iter()
        .find(|candidate| std::fs::File::open(candidate).is_ok())
        .cloned();

    let Some(db_path) = db_path else {
        let checked_locations = candidates
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        return Json(json!({
            "found": false,
            "error": format!(
                "Cursor database not found. Checked locations:\n{}\n\nMake sure Cursor IDE is installed and opened at least once.",
                checked_locations
            )
        }))
        .into_response();
    };

    if std::env::consts::OS == "linux" && !cursor_is_installed() {
        return Json(json!({
            "found": false,
            "error": "Cursor config files found but Cursor IDE does not appear to be installed. Skipping auto-import."
        }))
        .into_response();
    }

    if let Ok((Some(access_token), Some(machine_id))) = extract_cursor_tokens_from_db(&db_path) {
        return Json(json!({
            "found": true,
            "accessToken": access_token,
            "machineId": machine_id
        }))
        .into_response();
    }

    Json(json!({
        "found": false,
        "windowsManual": true,
        "dbPath": db_path.to_string_lossy().to_string()
    }))
    .into_response()
}

async fn kiro_auto_import_route() -> Response {
    let cache_path = kiro_sso_cache_path();
    let files = match std::fs::read_dir(&cache_path) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect::<Vec<_>>(),
        Err(_) => {
            return Json(json!({
                "found": false,
                "error": "AWS SSO cache not found. Please login to Kiro IDE first."
            }))
            .into_response()
        }
    };

    let mut refresh_token = None;
    let mut found_file = None;
    let kiro_token_file = "kiro-auth-token.json";

    if files.iter().any(|file| file == kiro_token_file) {
        let path = cache_path.join(kiro_token_file);
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(data) = serde_json::from_str::<Value>(&content) {
                if let Some(token) = data.get("refreshToken").and_then(Value::as_str) {
                    if token.starts_with("aorAAAAAG") {
                        refresh_token = Some(token.to_string());
                        found_file = Some(kiro_token_file.to_string());
                    }
                }
            }
        }
    }

    if refresh_token.is_none() {
        for file in &files {
            if !file.ends_with(".json") {
                continue;
            }

            let path = cache_path.join(file);
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(data) = serde_json::from_str::<Value>(&content) else {
                continue;
            };
            let Some(token) = data.get("refreshToken").and_then(Value::as_str) else {
                continue;
            };
            if token.starts_with("aorAAAAAG") {
                refresh_token = Some(token.to_string());
                found_file = Some(file.clone());
                break;
            }
        }
    }

    match refresh_token {
        Some(refresh_token) => Json(json!({
            "found": true,
            "refreshToken": refresh_token,
            "source": found_file
        }))
        .into_response(),
        None => Json(json!({
            "found": false,
            "error": "Kiro token not found in AWS SSO cache. Please login to Kiro IDE first."
        }))
        .into_response(),
    }
}

async fn kiro_import_auth(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> Response {
    let body = match axum::body::to_bytes(request.into_body(), 64 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) => return internal_error_response(error.to_string()),
    };

    let body: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => return internal_error_response(error.to_string()),
    };

    let Some(refresh_token_raw) = body.get("refreshToken").and_then(Value::as_str) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Refresh token is required" })),
        )
            .into_response();
    };
    if refresh_token_raw.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Refresh token is required" })),
        )
            .into_response();
    }

    let refresh_token = refresh_token_raw.trim();
    if !refresh_token.starts_with("aorAAAAAG") {
        return internal_error_response(
            "Invalid token format. Token should start with aorAAAAAG...".to_string(),
        );
    }

    let response = match reqwest::Client::new()
        .post(format!("{}/refreshToken", kiro_auth_service_base_url()))
        .header("Content-Type", "application/json")
        .json(&json!({ "refreshToken": refresh_token }))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return internal_error_response(format!("Token validation failed: {}", error))
        }
    };

    if !response.status().is_success() {
        let error = response.text().await.unwrap_or_default();
        return internal_error_response(format!(
            "Token validation failed: Token refresh failed: {}",
            error
        ));
    }

    let payload: Value = match response.json().await {
        Ok(value) => value,
        Err(error) => return internal_error_response(error.to_string()),
    };

    let Some(access_token) = payload
        .get("accessToken")
        .or_else(|| payload.get("access_token"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
    else {
        return internal_error_response(
            "Token validation failed: Kiro refresh response did not include access token"
                .to_string(),
        );
    };

    let saved_refresh_token = payload
        .get("refreshToken")
        .or_else(|| payload.get("refresh_token"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| refresh_token.to_string());
    let profile_arn = payload
        .get("profileArn")
        .and_then(Value::as_str)
        .map(str::to_string);
    let expires_in = payload
        .get("expiresIn")
        .or_else(|| payload.get("expires_in"))
        .and_then(Value::as_i64)
        .unwrap_or(3600);

    let claims = decode_cursor_jwt_claims(access_token);
    let email = claims
        .as_ref()
        .and_then(|value| value.get("email"))
        .or_else(|| {
            claims
                .as_ref()
                .and_then(|value| value.get("preferred_username"))
        })
        .or_else(|| claims.as_ref().and_then(|value| value.get("sub")))
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut provider_specific_data = std::collections::BTreeMap::from([
        (
            "authMethod".to_string(),
            Value::String("imported".to_string()),
        ),
        (
            "provider".to_string(),
            Value::String("Imported".to_string()),
        ),
    ]);
    if let Some(profile_arn) = profile_arn {
        provider_specific_data.insert("profileArn".to_string(), Value::String(profile_arn));
    }

    let connection = ProviderConnection {
        provider: "kiro".to_string(),
        auth_type: "oauth".to_string(),
        email: email.clone(),
        access_token: Some(access_token.to_string()),
        refresh_token: Some(saved_refresh_token),
        expires_at: Some((chrono::Utc::now() + chrono::Duration::seconds(expires_in)).to_rfc3339()),
        test_status: Some("active".to_string()),
        provider_specific_data,
        ..Default::default()
    };

    match create_imported_oauth_connection(&state.db, connection).await {
        Ok(connection) => Json(json!({
            "success": true,
            "connection": {
                "id": connection.id,
                "provider": connection.provider,
                "email": connection.email
            }
        }))
        .into_response(),
        Err(error) => internal_error_response(error.to_string()),
    }
}

// GET /api/oauth/:provider/start
pub async fn start_oauth_flow(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<StartQuery>,
    headers: axum::http::HeaderMap,
) -> Response {
    let api_key = match require_api_key(&headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };
    let account_id = &api_key.id;

    let provider_config = match get_provider_config(&provider) {
        Some(config) => config,
        None => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Unknown provider",
                "unknown_provider",
                &provider,
            )
        }
    };

    let code_verifier = generate_code_verifier();
    let code_challenge = generate_code_challenge(&code_verifier);
    let state_value = generate_state();

    let redirect_uri = query
        .redirect_uri
        .as_deref()
        .unwrap_or("http://localhost:4623/oauth/callback");

    let auth_url =
        provider_config.build_auth_url("openproxy", redirect_uri, &state_value, &code_challenge);

    let now = now_secs();
    let flow = PendingOAuthFlow {
        state: state_value.clone(),
        code_verifier: code_verifier.clone(),
        provider: provider.clone(),
        account_id: account_id.clone(),
        redirect_uri: Some(redirect_uri.to_string()),
        device_code: None,
        user_code: None,
        created_at: now,
        expires_at: now + PKCE_FLOW_TTL_SECS,
        kiro_credentials: None,
    };

    if state.pending_flows.insert(flow).is_err() {
        return make_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to store flow",
            "internal_error",
            &provider,
        );
    }

    Json(StartResponse {
        auth_url,
        state: state_value,
        provider: provider.clone(),
        expires_in: PKCE_FLOW_TTL_SECS as u64,
    })
    .into_response()
}

// GET /api/oauth/:provider/callback
pub async fn oauth_callback(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    if let Some(error) = &query.error {
        let desc = query.error_description.as_deref().unwrap_or(error);
        return make_error_response(StatusCode::BAD_REQUEST, desc, error, &provider);
    }

    let state_param = match &query.state {
        Some(s) => s,
        None => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Missing state parameter",
                "missing_state",
                &provider,
            )
        }
    };

    let code = match &query.code {
        Some(c) => c,
        None => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Missing code parameter",
                "missing_code",
                &provider,
            )
        }
    };

    let flow = match state.pending_flows.remove(state_param) {
        Some(f) => f,
        None => {
            return make_error_response(
                StatusCode::NOT_FOUND,
                "Flow not found or expired",
                "flow_not_found",
                &provider,
            )
        }
    };

    let provider_config = match get_provider_config(&provider) {
        Some(config) => config,
        None => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Unknown provider",
                "unknown_provider",
                &provider,
            )
        }
    };

    let redirect_uri = flow
        .redirect_uri
        .as_deref()
        .unwrap_or("http://localhost:4623/oauth/callback");

    let token_response = match device_code::exchange_code_for_token(
        &provider_config,
        code,
        &flow.code_verifier,
        redirect_uri,
        "openproxy",
    )
    .await
    {
        Ok(resp) => resp,
        Err(e) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                &e.error_description.unwrap_or_else(|| e.error.clone()),
                &e.error,
                &provider,
            );
        }
    };

    if let Err(e) = store_connection(
        &state.db,
        &flow.account_id,
        &provider,
        &token_response,
        Some(redirect_uri),
    )
    .await
    {
        return make_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to store connection: {}", e),
            "storage_error",
            &provider,
        );
    }

    Json(CallbackResponse {
        success: true,
        provider: provider.clone(),
        message: "OAuth flow completed successfully".to_string(),
    })
    .into_response()
}

// POST /api/oauth/:provider/device_code
pub async fn start_device_code(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(_query): Query<DeviceCodeBody>,
    headers: axum::http::HeaderMap,
) -> Response {
    let api_key = match require_api_key(&headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };
    let account_id = api_key.id;

    if !is_device_code_provider(&provider) {
        return make_error_response(
            StatusCode::BAD_REQUEST,
            "Provider does not support device code flow",
            "unsupported_flow",
            &provider,
        );
    }

    let provider_config = match get_provider_config(&provider) {
        Some(config) => config,
        None => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Unknown provider",
                "unknown_provider",
                &provider,
            )
        }
    };

    // Kiro uses a special combined flow: register client + start device code
    let (device_resp, kiro_credentials) = if provider == "kiro" {
        match device_code::kiro_start_device_flow().await {
            Ok(kiro_flow) => {
                let creds = Some((kiro_flow.client_id.clone(), kiro_flow.client_secret.clone()));
                (kiro_flow.device_code, creds)
            }
            Err(e) => {
                return make_error_response(
                    StatusCode::BAD_REQUEST,
                    &e.error_description.unwrap_or_else(|| e.error.clone()),
                    &e.error,
                    &provider,
                );
            }
        }
    } else {
        let client_id = provider_config
            .extra_params
            .get("client_id")
            .map(|v| v.as_str())
            .unwrap_or("openproxy")
            .to_string();

        match device_code::start_device_flow(&provider_config, &client_id).await {
            Ok(resp) => (resp, None),
            Err(e) => {
                return make_error_response(
                    StatusCode::BAD_REQUEST,
                    &e.error_description.unwrap_or_else(|| e.error.clone()),
                    &e.error,
                    &provider,
                );
            }
        }
    };

    let now = now_secs();
    let flow = PendingOAuthFlow {
        state: device_resp.device_code.clone(),
        code_verifier: String::new(),
        provider: provider.clone(),
        account_id: account_id.clone(),
        redirect_uri: None,
        device_code: Some(device_resp.device_code.clone()),
        user_code: Some(device_resp.user_code.clone()),
        created_at: now,
        expires_at: now + DEVICE_FLOW_TTL_SECS,
        kiro_credentials: kiro_credentials.map(|(id, secret)| {
            crate::oauth::pending::KiroCredentials {
                client_id: id,
                client_secret: secret,
            }
        }),
    };

    if state.pending_flows.insert(flow).is_err() {
        return make_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to store flow",
            "internal_error",
            &provider,
        );
    }

    Json(DeviceCodeResponse {
        device_code: device_resp.device_code,
        user_code: device_resp.user_code,
        verification_uri: device_resp.verification_uri,
        interval: device_resp.interval,
        expires_in: device_resp.expires_in.unwrap_or(DEVICE_FLOW_TTL_SECS) as u64,
    })
    .into_response()
}

// POST /api/oauth/:provider/poll
pub async fn poll_device_code(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    request: axum::extract::Request,
) -> Response {
    let headers = request.headers();
    let api_key = match require_api_key(headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };
    let account_id = api_key.id;

    let body = match axum::body::to_bytes(request.into_body(), 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Invalid request body",
                "invalid_body",
                &provider,
            );
        }
    };
    let body: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Invalid JSON body",
                "invalid_body",
                &provider,
            );
        }
    };
    let device_code = match body.get("device_code").and_then(|v| v.as_str()) {
        Some(code) => code.trim().to_string(),
        None => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Missing device_code in request body",
                "missing_device_code",
                &provider,
            );
        }
    };

    let _account_id = account_id;

    let pending_flow = state.pending_flows.get(&device_code);
    let flow = match pending_flow {
        Some(f) => f,
        None => {
            return make_error_response(
                StatusCode::NOT_FOUND,
                "Device code flow not found or expired",
                "flow_not_found",
                &provider,
            );
        }
    };

    let provider_config = match get_provider_config(&provider) {
        Some(config) => config,
        None => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Unknown provider",
                "unknown_provider",
                &provider,
            )
        }
    };

    let user_code = flow.user_code.clone().unwrap_or_default();
    let interval = provider_config
        .extra_params
        .get("interval")
        .map(|v| v.as_str())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5);

    match device_code::poll_for_token(&provider_config, &device_code, &user_code, interval).await {
        Ok(token_response) => {
            state.pending_flows.remove(&device_code);

            // GitHub special: exchange OAuth token for Copilot token
            let final_token_response = if provider == "github" {
                match device_code::exchange_github_copilot_token(&token_response.access_token).await
                {
                    Ok(copilot_token) => copilot_token,
                    Err(e) => {
                        return make_error_response(
                            StatusCode::BAD_REQUEST,
                            &format!(
                                "Copilot token exchange failed: {}",
                                e.error_description.unwrap_or_else(|| e.error.clone())
                            ),
                            "copilot_exchange_failed",
                            &provider,
                        );
                    }
                }
            } else {
                token_response
            };

            if let Err(e) = store_connection(
                &state.db,
                &flow.account_id,
                &provider,
                &final_token_response,
                None,
            )
            .await
            {
                return make_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("Failed to store connection: {}", e),
                    "storage_error",
                    &provider,
                );
            }

            Json(PollResponse {
                success: true,
                provider: provider.clone(),
                expires_in: final_token_response.expires_in.map(|e| e as u64),
                pending: Some(false),
                retry_after: None,
                message: Some("Authorization successful".to_string()),
            })
            .into_response()
        }
        Err(e) => {
            if e.error == "authorization_pending" || e.error == "slow_down" {
                let retry_after = provider_config
                    .extra_params
                    .get("interval")
                    .map(|v| v.as_str())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(5);
                return Json(PollResponse {
                    success: false,
                    provider: provider.clone(),
                    expires_in: None,
                    pending: Some(true),
                    retry_after: Some(retry_after),
                    message: Some("Pending authorization".to_string()),
                })
                .into_response();
            }

            state.pending_flows.remove(&device_code);

            make_error_response(
                StatusCode::BAD_REQUEST,
                &e.error_description.unwrap_or_else(|| e.error.clone()),
                &e.error,
                &provider,
            )
        }
    }
}

// POST /api/oauth/:provider/refresh
pub async fn refresh_token(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    request: axum::extract::Request,
) -> Response {
    let headers = request.headers();
    let api_key = match require_api_key(headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };
    let account_id = api_key.id;

    let body_bytes = match axum::body::to_bytes(request.into_body(), 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Invalid request body",
                "invalid_body",
                &provider,
            );
        }
    };
    let body: RefreshBody = match serde_json::from_slice(&body_bytes) {
        Ok(b) => b,
        Err(_) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Invalid JSON body",
                "invalid_body",
                &provider,
            );
        }
    };

    let snapshot = state.db.snapshot();
    let connection = snapshot
        .provider_connections
        .iter()
        .find(|conn| conn.provider == provider && conn.id.contains(&account_id));

    let refresh_token = match body.refresh_token {
        Some(ref token) => token.clone(),
        None => connection
            .and_then(|c| c.refresh_token.clone())
            .unwrap_or_default(),
    };

    if refresh_token.is_empty() {
        return make_error_response(
            StatusCode::BAD_REQUEST,
            "No refresh token available",
            "no_refresh_token",
            &provider,
        );
    }

    let provider_config = match get_provider_config(&provider) {
        Some(config) => config,
        None => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Unknown provider",
                "unknown_provider",
                &provider,
            )
        }
    };

    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", &refresh_token),
        ("client_id", "openproxy"),
    ];

    let resp = match client
        .post(&provider_config.token_url)
        .form(&params)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                &format!("Request failed: {}", e),
                "request_failed",
                &provider,
            );
        }
    };

    let token_response: TokenResponse = match resp.json().await {
        Ok(t) => t,
        Err(_) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Failed to parse token response",
                "parse_error",
                &provider,
            );
        }
    };

    if let Err(e) = store_connection(&state.db, &account_id, &provider, &token_response, None).await
    {
        return make_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to store connection: {}", e),
            "storage_error",
            &provider,
        );
    }

    Json(RefreshResponse {
        success: true,
        access_token: token_response.access_token.clone(),
        expires_in: token_response.expires_in.unwrap_or(3600) as u64,
        refresh_token: token_response.refresh_token.or(Some(refresh_token)),
    })
    .into_response()
}

// GET /api/oauth/:provider/status
pub async fn oauth_status(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    headers: axum::http::HeaderMap,
) -> Response {
    let api_key = match require_api_key(&headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };
    let account_id = api_key.id;

    let snapshot = state.db.snapshot();
    let connection = snapshot
        .provider_connections
        .iter()
        .find(|conn| conn.provider == provider && conn.id.contains(&account_id));

    match connection {
        Some(conn) => {
            let needs_refresh = crate::oauth::needs_refresh(&conn.expires_at);
            Json(StatusResponse {
                provider: provider.clone(),
                connected: true,
                auth_type: conn.auth_type.clone(),
                expires_at: conn.expires_at.clone(),
                needs_refresh: Some(needs_refresh),
                scope: conn.scope.clone(),
            })
            .into_response()
        }
        None => Json(StatusResponse {
            provider: provider.clone(),
            connected: false,
            auth_type: "oauth".to_string(),
            expires_at: None,
            needs_refresh: None,
            scope: None,
        })
        .into_response(),
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/oauth/cursor/auto-import",
            get(cursor_auto_import_route),
        )
        .route(
            "/api/oauth/cursor/import",
            get(cursor_import_instructions_route).post(cursor_import_auth),
        )
        .route("/api/oauth/kiro/auto-import", get(kiro_auto_import_route))
        .route("/api/oauth/kiro/import", post(kiro_import_auth))
        .route("/api/oauth/iflow/cookie", post(iflow_cookie_auth))
        .route("/api/oauth/gitlab/pat", post(gitlab_pat_auth))
        .route("/api/oauth/{provider}/start", get(start_oauth_flow))
        .route("/api/oauth/{provider}/callback", get(oauth_callback))
        .route("/api/oauth/{provider}/device_code", post(start_device_code))
        .route("/api/oauth/{provider}/poll", post(poll_device_code))
        .route("/api/oauth/{provider}/refresh", post(refresh_token))
        .route("/api/oauth/{provider}/status", get(oauth_status))
}
