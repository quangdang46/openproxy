use axum::{
    extract::State,
    http::HeaderMap,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::server::state::AppState;

fn require_management_access(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    super::require_management_api_key(headers, state)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/init", get(get_init))
        .route("/api/init", post(post_init))
        .route("/api/init", delete(delete_init))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InitResponse {
    enabled: bool,
    url: Option<String>,
    setup_required: bool,
}

async fn get_init(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let settings = &snapshot.settings;

    let enabled = settings.cloud_enabled;
    let url = if settings.cloud_url.is_empty() {
        None
    } else {
        Some(settings.cloud_url.clone())
    };
    let setup_required = !enabled;

    Json(InitResponse {
        enabled,
        url,
        setup_required,
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitRequest {
    url: String,
}

async fn post_init(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<InitRequest>,
) -> impl IntoResponse {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let url = body.url.trim().to_string();
    if url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "url is required"
            })),
        )
            .into_response();
    }

    let result = state
        .db
        .update(|db| {
            db.settings.cloud_enabled = true;
            db.settings.cloud_url = url.clone();
        })
        .await;

    match result {
        Ok(snapshot) => {
            let settings = &snapshot.settings;
            Json(InitResponse {
                enabled: settings.cloud_enabled,
                url: if settings.cloud_url.is_empty() {
                    None
                } else {
                    Some(settings.cloud_url.clone())
                },
                setup_required: false,
            })
            .into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": err.to_string()
            })),
        )
            .into_response(),
    }
}

async fn delete_init(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let result = state
        .db
        .update(|db| {
            db.settings.cloud_enabled = false;
            db.settings.cloud_url.clear();
        })
        .await;

    match result {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "enabled": false,
                "url": null,
                "setupRequired": true
            })),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": err.to_string()
            })),
        )
            .into_response(),
    }
}
