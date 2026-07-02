//! HTTP endpoints for the A2A (Agent-to-Agent) protocol.
//!
//! Implements the A2A wire protocol for agent discovery and task-based
//! communication:
//!
//!   * `GET  /.well-known/agent.json`       — Agent Card (discovery)
//!   * `GET  /api/a2a/agent-card`           — Agent Card (named path)
//!   * `POST /api/a2a/tasks/send`            — Create and execute a task
//!   * `GET  /api/a2a/tasks/{id}`            — Get task status + result
//!   * `POST /api/a2a/tasks/{id}/cancel`     — Cancel a running task

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{
    routing::{get, post},
    Json, Router,
};
use serde_json::json;

use crate::core::a2a;
use crate::server::state::AppState;

/// Build the sub-router mounted at these paths.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/.well-known/agent.json", get(agent_card_well_known))
        .route("/api/a2a/agent-card", get(agent_card_api))
        .route("/api/a2a/tasks/send", post(tasks_send))
        .route("/api/a2a/tasks/{id}", get(tasks_get))
        .route("/api/a2a/tasks/{id}/cancel", post(tasks_cancel))
}

/// Serve the A2A Agent Card at the well-known location.
async fn agent_card_well_known(State(state): State<AppState>) -> Response {
    let base_url = format!("http://{}:{}", "0.0.0.0", 4623);
    Json(a2a::AgentCard::new(&base_url)).into_response()
}

/// Serve the A2A Agent Card at the API path.
async fn agent_card_api(State(state): State<AppState>) -> Response {
    let base_url = format!("http://{}:{}", "0.0.0.0", 4623);
    Json(a2a::AgentCard::new(&base_url)).into_response()
}

/// Create and dispatch a task.
async fn tasks_send(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let request: a2a::TaskSendRequest = match serde_json::from_value(body) {
        Ok(req) => req,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("Invalid A2A task request: {e}") })),
            )
                .into_response();
        }
    };

    let task = a2a::dispatch_task(&state.a2a_task_store, request, &state).await;
    (StatusCode::OK, Json(task)).into_response()
}

/// Get a task by ID.
async fn tasks_get(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.a2a_task_store.get(&id).await {
        Some(task) => Json(task).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("Task '{id}' not found") })),
        )
            .into_response(),
    }
}

/// Cancel a running task.
async fn tasks_cancel(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state
        .a2a_task_store
        .update_state(&id, a2a::TaskState::Canceled)
        .await
    {
        Some(task) => Json(task).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("Task '{id}' not found") })),
        )
            .into_response(),
    }
}
