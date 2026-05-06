use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::server::state::AppState;
use crate::types::ProviderConnection;

const MODEL_LOCK_PREFIX: &str = "modelLock_";

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/models/availability",
        get(get_availability).post(clear_cooldown),
    )
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelAvailabilityIssue {
    provider: String,
    model: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    until: Option<String>,
    connection_id: String,
    connection_name: String,
    last_error: Option<String>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelAvailabilityResponse {
    models: Vec<ModelAvailabilityIssue>,
    unavailable_count: usize,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClearCooldownRequest {
    action: Option<String>,
    provider: Option<String>,
    model: Option<String>,
}

#[derive(Debug)]
struct ActiveModelLock {
    model: String,
    until: String,
}

fn parse_active_lock_until(value: &Value, now: DateTime<Utc>) -> Option<String> {
    let until = match value {
        Value::String(text) if !text.is_empty() => text,
        _ => return None,
    };
    let parsed = DateTime::parse_from_rfc3339(until).ok()?;
    (parsed.with_timezone(&Utc) > now).then(|| until.clone())
}

fn active_model_locks(connection: &ProviderConnection, now: DateTime<Utc>) -> Vec<ActiveModelLock> {
    connection
        .extra
        .iter()
        .filter_map(|(key, value)| {
            if !key.starts_with(MODEL_LOCK_PREFIX) {
                return None;
            }

            let until = parse_active_lock_until(value, now)?;
            let model = key
                .strip_prefix(MODEL_LOCK_PREFIX)
                .filter(|value| !value.is_empty())
                .unwrap_or("__all")
                .to_string();

            Some(ActiveModelLock { model, until })
        })
        .collect()
}

fn connection_name(connection: &ProviderConnection) -> String {
    connection
        .name
        .as_deref()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            connection
                .email
                .as_deref()
                .filter(|value| !value.is_empty())
        })
        .unwrap_or(connection.id.as_str())
        .to_string()
}

async fn get_availability(State(state): State<AppState>) -> Response {
    let snapshot = state.db.snapshot();
    let now = Utc::now();
    let mut connections = snapshot.provider_connections.clone();
    connections.sort_by_key(|connection| connection.priority.unwrap_or(999));

    let mut models = Vec::new();

    for connection in connections {
        let locks = active_model_locks(&connection, now);

        for lock in &locks {
            models.push(ModelAvailabilityIssue {
                provider: connection.provider.clone(),
                model: lock.model.clone(),
                status: "cooldown".to_string(),
                until: Some(lock.until.clone()),
                connection_id: connection.id.clone(),
                connection_name: connection_name(&connection),
                last_error: connection.last_error.clone(),
            });
        }

        if locks.is_empty() && connection.test_status.as_deref() == Some("unavailable") {
            models.push(ModelAvailabilityIssue {
                provider: connection.provider.clone(),
                model: "__all".to_string(),
                status: "unavailable".to_string(),
                until: None,
                connection_id: connection.id.clone(),
                connection_name: connection_name(&connection),
                last_error: connection.last_error.clone(),
            });
        }
    }

    Json(ModelAvailabilityResponse {
        unavailable_count: models.len(),
        models,
    })
    .into_response()
}

async fn clear_cooldown(
    State(state): State<AppState>,
    Json(req): Json<ClearCooldownRequest>,
) -> Response {
    let action = req.action.as_deref().unwrap_or_default();
    let provider = req.provider.as_deref().unwrap_or_default();
    let model = req.model.as_deref().unwrap_or_default();

    if action != "clearCooldown" || provider.is_empty() || model.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Invalid request" })),
        )
            .into_response();
    }

    let lock_key = format!("{MODEL_LOCK_PREFIX}{model}");
    let now = Utc::now().to_rfc3339();
    let update_result = state
        .db
        .update(|db| {
            for connection in db
                .provider_connections
                .iter_mut()
                .filter(|connection| connection.provider == provider)
            {
                let has_lock = connection
                    .extra
                    .get(&lock_key)
                    .is_some_and(|value| !value.is_null());
                if !has_lock {
                    continue;
                }

                connection.extra.insert(lock_key.clone(), Value::Null);
                if connection.test_status.as_deref() == Some("unavailable") {
                    connection.test_status = Some("active".to_string());
                    connection.last_error = None;
                    connection.last_error_at = None;
                    connection.backoff_level = Some(0);
                }
                connection.updated_at = Some(now.clone());
            }
        })
        .await;

    match update_result {
        Ok(_) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "Failed to clear cooldown" })),
        )
            .into_response(),
    }
}
