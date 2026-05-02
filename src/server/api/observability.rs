//! Observability API endpoints.
//!
//! Provides endpoints for viewing and managing request logs.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use serde_json::json;

use crate::server::auth::require_api_key;
use crate::server::state::AppState;
use crate::types::{ObservableLogEntry, ObservableStatsResponse};

use super::auth_error_response;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/observability/logs", get(get_logs))
        .route("/api/observability/stats", get(get_stats))
        .route("/api/observability/clear", post(clear_logs))
}

async fn get_logs(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
    }

    let logs = state.get_observability_logs();
    let entries: Vec<ObservableLogEntry> = logs
        .into_iter()
        .map(|log| ObservableLogEntry {
            id: log.id,
            timestamp: log.timestamp.to_rfc3339(),
            method: log.method,
            path: log.path,
            model: log.model,
            request_tokens: log.request_tokens,
            response_tokens: log.response_tokens,
            status_code: log.status_code,
            duration_ms: log.duration_ms,
            error: log.error,
        })
        .collect();

    Json(json!({
        "logs": entries,
        "count": entries.len(),
    }))
    .into_response()
}

async fn get_stats(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
    }

    let stats = state.get_observability_stats();
    let response = ObservableStatsResponse {
        total_requests: stats.total_requests,
        total_request_tokens: stats.total_request_tokens,
        total_response_tokens: stats.total_response_tokens,
        total_duration_ms: stats.total_duration_ms,
        avg_duration_ms: stats.avg_duration_ms,
        success_count: stats.success_count,
        error_count: stats.error_count,
        status_codes: stats.status_codes,
        top_models: stats.top_models,
    };

    Json(response).into_response()
}

async fn clear_logs(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
    }

    state.clear_observability_logs();

    Json(json!({
        "success": true,
        "message": "Logs cleared",
    }))
    .into_response()
}
