use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    response::Response,
    routing::{delete, get, post},
    Json, Router,
};
use bcrypt::verify;
use jsonwebtoken::{encode, EncodingKey, Header as JwtHeader};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::server::auth::require_api_key;
use crate::server::state::AppState;
use crate::types::Settings;

#[derive(Debug, Deserialize)]
pub struct PasswordLoginRequest {
    pub password: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AuthTokenClaims {
    authenticated: bool,
    exp: usize,
}

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub session_id: String,
    pub api_key_id: String,
    pub created_at: i64,
    pub last_active: i64,
    pub is_valid: bool,
}

#[derive(Debug, Deserialize)]
pub struct LogoutRequest {
    pub session_id: Option<String>,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// POST /api/auth/login
/// Creates a JWT cookie for browser dashboard auth.
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PasswordLoginRequest>,
) -> Response {
    let snapshot = state.db.snapshot();
    if is_tunnel_request(&headers, &snapshot.settings)
        && snapshot.settings.tunnel_dashboard_access != true
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "Dashboard access via tunnel is disabled" })),
        )
            .into_response();
    }

    let provided_password = req.password;
    let valid = match settings_password_hash(&snapshot.settings) {
        Some(hash) => verify(&provided_password, hash).unwrap_or(false),
        None => {
            let initial_password =
                std::env::var("INITIAL_PASSWORD").unwrap_or_else(|_| "123456".to_string());
            provided_password == initial_password
        }
    };

    if !valid {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "Invalid password" })),
        )
            .into_response();
    }

    let expires_at = now_secs() + 86400;
    let secret = std::env::var("JWT_SECRET")
        .unwrap_or_else(|_| "9router-default-secret-change-me".to_string());
    let token = match encode(
        &JwtHeader::default(),
        &AuthTokenClaims {
            authenticated: true,
            exp: expires_at as usize,
        },
        &EncodingKey::from_secret(secret.as_bytes()),
    ) {
        Ok(token) => token,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to issue auth token: {error}") })),
            )
                .into_response();
        }
    };

    let secure_cookie = std::env::var("AUTH_COOKIE_SECURE").ok().as_deref() == Some("true")
        || headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.eq_ignore_ascii_case("https"))
            .unwrap_or(false);

    let mut response = Json(json!({ "success": true })).into_response();
    let cookie = build_auth_cookie(&token, 86400, secure_cookie);
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

/// POST /api/auth/logout
/// Invalidates the current session
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<LogoutRequest>,
) -> Response {
    if crate::server::auth::extract_auth_token(&headers).is_some() {
        let mut response = Json(json!({
            "success": true,
            "message": "Logged out"
        }))
        .into_response();
        response.headers_mut().append(
            header::SET_COOKIE,
            HeaderValue::from_static("auth_token=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"),
        );
        return response;
    }

    let api_key = match require_api_key(&headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };

    let mut sessions = state.sessions.write().await;

    // If session_id provided, remove that specific session
    if let Some(session_id) = req.session_id {
        if let Some(session) = sessions.get(&session_id) {
            if session.api_key_id == api_key.id {
                sessions.remove(&session_id);
                return Json(json!({
                    "success": true,
                    "message": "Session logged out"
                }))
                .into_response();
            } else {
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({
                        "success": false,
                        "error": "Session belongs to different user"
                    })),
                )
                    .into_response();
            }
        }
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "success": false,
                "error": "Session not found"
            })),
        )
            .into_response();
    }

    // Otherwise, remove all sessions for this API key
    sessions.retain(|_, session| session.api_key_id != api_key.id);

    Json(json!({
        "success": true,
        "message": "All sessions logged out"
    }))
    .into_response()
}

/// GET /api/auth/session/:session_id
/// Get session info
pub async fn get_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let _api_key = match require_api_key(&headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };

    let sessions = state.sessions.read().await;

    match sessions.get(&session_id) {
        Some(session) => {
            let now = now_secs();
            let is_valid = now < (session.created_at + 86400);
            Json(SessionResponse {
                session_id: session.session_id.clone(),
                api_key_id: session.api_key_id.clone(),
                created_at: session.created_at,
                last_active: session.last_active,
                is_valid,
            })
            .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "Session not found"
            })),
        )
            .into_response(),
    }
}

/// GET /api/auth/sessions
/// List all sessions for the current API key
pub async fn list_sessions(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let api_key = match require_api_key(&headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };

    let sessions = state.sessions.read().await;
    let now = now_secs();

    let session_list: Vec<SessionResponse> = sessions
        .values()
        .filter(|s| s.api_key_id == api_key.id)
        .map(|session| {
            let is_valid = now < (session.created_at + 86400);
            SessionResponse {
                session_id: session.session_id.clone(),
                api_key_id: session.api_key_id.clone(),
                created_at: session.created_at,
                last_active: session.last_active,
                is_valid,
            }
        })
        .collect();

    Json(json!({
        "sessions": session_list,
        "count": session_list.len()
    }))
    .into_response()
}

/// DELETE /api/auth/sessions
/// Invalidate all sessions for the current API key
pub async fn delete_all_sessions(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let api_key = match require_api_key(&headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };

    let mut sessions = state.sessions.write().await;
    let before = sessions.len();
    sessions.retain(|_, session| session.api_key_id != api_key.id);
    let after = sessions.len();

    Json(json!({
        "success": true,
        "message": format!("Invalidated {} sessions", before - after)
    }))
    .into_response()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/sessions", get(list_sessions))
        .route("/api/auth/sessions", delete(delete_all_sessions))
        .route("/api/auth/session/{session_id}", get(get_session))
}

fn settings_password_hash(settings: &Settings) -> Option<&str> {
    settings.extra.get("password")?.as_str()
}

fn is_tunnel_request(headers: &HeaderMap, settings: &Settings) -> bool {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(':')
                .next()
                .unwrap_or(value)
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    if host.is_empty() {
        return false;
    }

    tunnel_host(&settings.tunnel_url).is_some_and(|tunnel_host| tunnel_host == host)
        || tunnel_host(&settings.tailscale_url).is_some_and(|tailscale_host| tailscale_host == host)
}

fn tunnel_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    url::Url::parse(trimmed)
        .ok()
        .and_then(|parsed| parsed.host_str().map(|host| host.to_ascii_lowercase()))
}

fn build_auth_cookie(token: &str, max_age_seconds: i64, secure: bool) -> String {
    let secure_flag = if secure { "; Secure" } else { "" };
    format!(
        "auth_token={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_seconds}{secure_flag}"
    )
}
