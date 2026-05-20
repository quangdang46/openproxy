//! `/api/settings/payload-rules` and `/api/settings/system-prompt`.
//!
//! Modeled after OmniRoute's equivalent routes. Both are guarded by the
//! dashboard / management-API key auth used by the rest of `/api/settings`.
//!
//! - `GET  /api/settings/payload-rules`  → returns the current config
//! - `PUT  /api/settings/payload-rules`  → replaces the config (full
//!   document; normalized on save)
//! - `GET  /api/settings/system-prompt`  → returns the override config
//! - `PUT  /api/settings/system-prompt`  → updates the override config

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::payload_rules::{PayloadRulesConfig, SystemPromptConfig, SystemPromptMode};
use crate::server::state::AppState;

use super::require_dashboard_or_management_api_key;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/settings/payload-rules",
            get(get_payload_rules_api).put(update_payload_rules_api),
        )
        .route(
            "/api/settings/system-prompt",
            get(get_system_prompt_api).put(update_system_prompt_api),
        )
}

fn payload_rules_response(config: &PayloadRulesConfig) -> Value {
    json!({
        "config": config,
        "summary": config.summary(),
    })
}

async fn get_payload_rules_api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }
    let snapshot = state.db.snapshot();
    Json(payload_rules_response(&snapshot.settings.payload_rules)).into_response()
}

async fn update_payload_rules_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PayloadRulesConfig>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let result = state
        .db
        .update(|db| {
            db.settings.payload_rules = req;
            db.settings.payload_rules.normalize();
        })
        .await;

    match result {
        Ok(updated) => {
            Json(payload_rules_response(&updated.settings.payload_rules)).into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to save payload rules: {error}") })),
        )
            .into_response(),
    }
}

async fn get_system_prompt_api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }
    let snapshot = state.db.snapshot();
    Json(json!({
        "mode": snapshot.settings.system_prompt.mode,
        "content": snapshot.settings.system_prompt.content,
        "active": snapshot.settings.system_prompt.is_active(),
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSystemPromptRequest {
    mode: Option<SystemPromptMode>,
    content: Option<String>,
}

async fn update_system_prompt_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpdateSystemPromptRequest>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let result = state
        .db
        .update(|db| {
            if let Some(mode) = req.mode {
                db.settings.system_prompt.mode = mode;
            }
            if let Some(content) = req.content {
                db.settings.system_prompt.content = content;
            }
            db.settings.system_prompt.normalize();
        })
        .await;

    match result {
        Ok(updated) => Json(json!({
            "mode": updated.settings.system_prompt.mode,
            "content": updated.settings.system_prompt.content,
            "active": updated.settings.system_prompt.is_active(),
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to save system prompt: {error}") })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload_rules::SystemPromptMode;

    // Sanity-check the JSON shape we return to the dashboard so a future
    // refactor doesn't silently break the UI contract.
    #[test]
    fn payload_rules_response_contains_config_and_summary() {
        let cfg = PayloadRulesConfig::default();
        let body = payload_rules_response(&cfg);
        assert!(body.get("config").is_some());
        assert!(body.get("summary").is_some());
        let summary = &body["summary"];
        assert_eq!(summary["default"], 0);
        assert_eq!(summary["override"], 0);
        assert_eq!(summary["filter"], 0);
        assert_eq!(summary["defaultRaw"], 0);
    }

    #[test]
    fn system_prompt_mode_serializes_lowercase() {
        // We rely on lowercase tags in the dashboard radio buttons.
        assert_eq!(
            serde_json::to_value(SystemPromptMode::Off).unwrap(),
            json!("off")
        );
        assert_eq!(
            serde_json::to_value(SystemPromptMode::Prepend).unwrap(),
            json!("prepend")
        );
        assert_eq!(
            serde_json::to_value(SystemPromptMode::Override).unwrap(),
            json!("override")
        );
    }
}
