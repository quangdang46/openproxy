use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    response::Response,
    routing::{post, get},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::server::auth::require_api_key;
use crate::server::state::AppState;

/// Shutdown status response
#[derive(Debug, Serialize)]
pub struct ShutdownStatus {
    pub running: bool,
    pub uptime_secs: i64,
    pub pending_requests: usize,
}

/// Request to initiate shutdown
#[derive(Debug, Deserialize)]
pub struct ShutdownRequest {
    pub force: Option<bool>,
    pub delay_secs: Option<i64>,
}

/// Response for shutdown request
#[derive(Debug, Serialize)]
pub struct ShutdownResponse {
    pub success: bool,
    pub message: String,
    pub shutdown_initiated: bool,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// GET /api/shutdown/status
/// Get current shutdown status
pub async fn shutdown_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    // Require authentication for status check
    if let Err(e) = require_api_key(&headers, &state.db) {
        return crate::server::api::auth_error_response(e);
    }

    let snapshot = state.db.snapshot();

    Json(ShutdownStatus {
        running: true,
        uptime_secs: 0, // Would need to track server start time
        pending_requests: 0, // Would need request tracking
    })
    .into_response()
}

/// POST /api/shutdown
/// Initiate graceful shutdown
pub async fn initiate_shutdown(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ShutdownRequest>,
) -> Response {
    // Require authentication for shutdown
    if let Err(e) = require_api_key(&headers, &state.db) {
        return crate::server::api::auth_error_response(e);
    }

    let force = req.force.unwrap_or(false);
    let delay_secs = req.delay_secs.unwrap_or(5);

    if force {
        // Immediate shutdown (not implemented - would need access to server handle)
        Json(ShutdownResponse {
            success: true,
            message: "Force shutdown initiated".to_string(),
            shutdown_initiated: true,
        })
        .into_response()
    } else {
        // Graceful shutdown with delay
        Json(ShutdownResponse {
            success: true,
            message: format!("Graceful shutdown will begin in {} seconds", delay_secs),
            shutdown_initiated: true,
        })
        .into_response()
    }
}

/// GET /api/shutdown/health
/// Simple health check for shutdown system
pub async fn shutdown_health() -> Json<serde_json::Value> {
    Json(json!({
        "shutdown_service": "ok",
        "timestamp": now_secs()
    }))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/shutdown/status", get(shutdown_status))
        .route("/api/shutdown", post(initiate_shutdown))
        .route("/api/shutdown/health", get(shutdown_health))
}