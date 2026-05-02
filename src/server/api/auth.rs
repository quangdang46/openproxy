use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    response::Response,
    routing::{get, post, delete},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::server::auth::require_api_key;
use crate::server::state::{AppState, SessionInfo};
use super::auth_error_response;

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub success: bool,
    pub session_id: Option<String>,
    pub expires_at: Option<i64>,
    pub message: Option<String>,
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
/// Creates a new session for the authenticated API key
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let api_key = match require_api_key(&headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };

    let session_id = uuid::Uuid::new_v4().to_string();
    let now = now_secs();
    let expires_at = now + 86400; // 24 hours

    let session = SessionInfo {
        session_id: session_id.clone(),
        api_key_id: api_key.id.clone(),
        created_at: now,
        last_active: now,
    };

    // Store session in memory (in production, you'd want persistent storage)
    let mut sessions = state.sessions.write().await;
    sessions.insert(session_id.clone(), session.clone());

    Json(LoginResponse {
        success: true,
        session_id: Some(session_id),
        expires_at: Some(expires_at),
        message: Some("Login successful".to_string()),
    })
    .into_response()
}

/// POST /api/auth/logout
/// Invalidates the current session
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<LogoutRequest>,
) -> Response {
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
                return (StatusCode::FORBIDDEN, Json(json!({
                    "success": false,
                    "error": "Session belongs to different user"
                })))
                .into_response();
            }
        }
        return (StatusCode::NOT_FOUND, Json(json!({
            "success": false,
            "error": "Session not found"
        })))
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
        None => (StatusCode::NOT_FOUND, Json(json!({
            "error": "Session not found"
        })))
        .into_response(),
    }
}

/// GET /api/auth/sessions
/// List all sessions for the current API key
pub async fn list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
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
pub async fn delete_all_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
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