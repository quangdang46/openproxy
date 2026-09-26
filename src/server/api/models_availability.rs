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
                clear_cooldown_on_connection(connection, &lock_key, &now);
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

/// Drop `lock_key` from a connection, and — when that connection was
/// `unavailable` — put it back in rotation.
///
/// The re-enable has to clear more than the one lock the click named. 9router
/// sends a patch carrying `testStatus: "active"`, and the repository expands any
/// such patch into a full health reset (connectionsRepo.js:15-33, reached from
/// `src/app/api/models/availability/route.js:81-92`): `errorCode`,
/// `rateLimitedUntil`, `backoffLevel` and EVERY `modelLock_*` key on the row.
/// Hand-picking four fields here cleared one lock and left the rest — a
/// connection could come back "active" while other models stayed locked.
///
/// Returns true when the connection held the lock and was touched; a connection
/// without it is skipped so the route never wakes a healthy row.
pub(crate) fn clear_cooldown_on_connection(
    connection: &mut ProviderConnection,
    lock_key: &str,
    now: &str,
) -> bool {
    let has_lock = connection
        .extra
        .get(lock_key)
        .is_some_and(|value| !value.is_null());
    if !has_lock {
        return false;
    }

    connection.extra.insert(lock_key.to_string(), Value::Null);
    if connection.test_status.as_deref() == Some("unavailable") {
        connection.test_status = Some("active".to_string());
        connection.last_error = None;
        connection.last_error_at = None;
        crate::core::account_fallback::reset_health_state_on_activation(connection);
    }
    connection.updated_at = Some(now.to_string());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProviderConnection;

    const FAR_FUTURE: &str = "2099-01-01T00:00:00Z";
    const NOW: &str = "2026-01-01T00:00:00+00:00";

    fn locked_connection() -> ProviderConnection {
        let mut conn = ProviderConnection {
            id: "c1".into(),
            provider: "openai".into(),
            test_status: Some("unavailable".into()),
            error_code: Some("429".into()),
            backoff_level: Some(6),
            rate_limited_until: Some(FAR_FUTURE.into()),
            ..Default::default()
        };
        conn.extra
            .insert("modelLock_gpt-4o".into(), Value::String(FAR_FUTURE.into()));
        conn.extra.insert(
            "modelLock_claude-opus-4".into(),
            Value::String(FAR_FUTURE.into()),
        );
        conn.extra
            .insert("poolId".into(), Value::String("p1".into()));
        conn
    }

    #[test]
    fn clearing_one_models_cooldown_clears_every_lock_and_the_health_state() {
        let mut conn = locked_connection();

        assert!(clear_cooldown_on_connection(
            &mut conn,
            "modelLock_gpt-4o",
            NOW
        ));

        assert!(
            conn.extra["modelLock_gpt-4o"].is_null(),
            "the requested model"
        );
        assert!(
            conn.extra["modelLock_claude-opus-4"].is_null(),
            "9router nulls EVERY modelLock_* key"
        );
        assert!(conn.error_code.is_none());
        assert!(conn.rate_limited_until.is_none());
        assert_eq!(conn.backoff_level, Some(0));
        assert_eq!(conn.consecutive_errors, Some(0));
        assert_eq!(conn.test_status.as_deref(), Some("active"));
        assert_eq!(
            conn.extra.get("poolId").and_then(Value::as_str),
            Some("p1"),
            "unrelated extra keys survive"
        );
    }

    // 9router route.js:83 — the `testStatus: "active"` expansion is only
    // attached when the connection is `unavailable`. An already-active
    // connection keeps everything except the one named lock.
    #[test]
    fn clearing_a_cooldown_on_an_active_connection_only_drops_that_one_lock() {
        let mut conn = locked_connection();
        conn.test_status = Some("active".into());

        assert!(clear_cooldown_on_connection(
            &mut conn,
            "modelLock_gpt-4o",
            NOW
        ));

        assert!(conn.extra["modelLock_gpt-4o"].is_null());
        assert!(!conn.extra["modelLock_claude-opus-4"].is_null());
        assert_eq!(conn.error_code.as_deref(), Some("429"));
    }

    #[test]
    fn a_connection_without_the_lock_is_skipped() {
        let mut conn = ProviderConnection {
            id: "c2".into(),
            provider: "openai".into(),
            test_status: Some("unavailable".into()),
            ..Default::default()
        };
        conn.extra
            .insert("poolId".into(), Value::String("p1".into()));

        assert!(!clear_cooldown_on_connection(
            &mut conn,
            "modelLock_gpt-4o",
            NOW
        ));
        assert_eq!(conn.test_status.as_deref(), Some("unavailable"));
        assert!(conn.updated_at.is_none(), "an untouched row is not stamped");
    }
}
