//! Model catalog sync surface (9router `/api/models/catalog-sync`).
//!
//! The sync loop and its state live in the core overlay module; this is only
//! the HTTP shell, so an operator can read the timer state, see what the
//! catalog file currently holds, and force a run instead of waiting for it.

use std::path::Path;

use axum::extract::State;
use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};

use crate::core::model::catalog_overlay::{get_sync_state, sync_model_catalog, CATALOG_FILE_NAME};
use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/models/catalog-sync",
        get(catalog_sync_status).post(catalog_sync_now),
    )
}

/// What the catalog file on disk currently holds. `None` until the first
/// successful sync — an unsynced install is a normal state, not an error, so
/// the caller reports `catalog: null` exactly as 9router's `catch` arm does.
fn catalog_summary(data_dir: &Path) -> Option<Value> {
    let path = data_dir.join(CATALOG_FILE_NAME);
    let bytes = std::fs::metadata(&path).ok()?.len();
    let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
    let count = |key: &str| {
        parsed
            .get(key)
            .and_then(Value::as_object)
            .map(|map| map.len())
    };

    Some(json!({
        "syncedAt": parsed.get("syncedAt"),
        "models": count("models").unwrap_or(0),
        "providers": count("providers").unwrap_or(0),
        "bytes": bytes,
    }))
}

async fn catalog_sync_status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let catalog = catalog_summary(&state.db.data_dir);
    let mut body = get_sync_state(&state.db.data_dir);
    if let Some(fields) = body.as_object_mut() {
        fields.insert("catalog".to_string(), catalog.unwrap_or(Value::Null));
    }

    Json(body).into_response()
}

async fn catalog_sync_now(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match sync_model_catalog(&state.db.data_dir).await {
        Some(result) => Json(json!({ "success": true, "result": result })).into_response(),
        // 503, not 200 + success:false — the caller gates on the status, and a
        // 200 here reads as "synced" while the state still carries the error.
        None => {
            let last_error = get_sync_state(&state.db.data_dir)
                .get("lastError")
                .and_then(Value::as_str)
                .unwrap_or("sync in progress")
                .to_string();
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": last_error })),
            )
                .into_response()
        }
    }
}
