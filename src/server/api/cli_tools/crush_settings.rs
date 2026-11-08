//! Crush (Charmbracelet) settings — port of 9router
//! `src/app/api/cli-tools/crush-settings/route.js`.
//!
//! Config: `$XDG_CONFIG_HOME/crush/crush.json` (default `~/.config/crush/crush.json`).
//! Provider block at `providers["9router"]` with `type: "openai-compat"`.

use std::path::PathBuf;

use anyhow::Result as AnyhowResult;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::fs;

use super::shared::{command_exists, config_home_dir, normalize_base_url, resolve_cli_api_key};
use crate::server::state::AppState;

const PROVIDER_ID: &str = "9router";

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/cli-tools/crush-settings",
        get(get_crush_settings)
            .post(save_crush_settings)
            .delete(delete_crush_settings),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveCrushSettingsRequest {
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
}

pub(super) async fn get_crush_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    if !check_crush_installed().await {
        return Json(json!({
            "installed": false,
            "config": Value::Null,
            "message": "Crush CLI is not installed",
        }))
        .into_response();
    }

    let config = read_config().await.unwrap_or(Value::Null);
    Json(json!({
        "installed": true,
        "config": config,
        "has9Router": has_openproxy_config(config.as_object()),
        "configPath": crush_config_path().to_string_lossy().to_string(),
    }))
    .into_response()
}

async fn save_crush_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SaveCrushSettingsRequest>,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let Some(base_url) = body
        .base_url
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "baseUrl is required" })),
        )
            .into_response();
    };

    match write_crush_settings(&state, &body, base_url).await {
        Ok(config_path) => Json(json!({
            "success": true,
            "message": "Crush settings applied successfully!",
            "configPath": config_path,
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to write crush settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to update crush settings: {error}") })),
            )
                .into_response()
        }
    }
}

async fn delete_crush_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_crush_settings().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to reset crush settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to reset crush settings: {error}") })),
            )
                .into_response()
        }
    }
}

async fn check_crush_installed() -> bool {
    if command_exists("crush").await {
        return true;
    }
    fs::metadata(crush_config_path()).await.is_ok()
}

async fn read_config() -> AnyhowResult<Value> {
    match fs::read_to_string(crush_config_path()).await {
        Ok(content) => Ok(serde_json::from_str(&content).unwrap_or(Value::Null)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
        Err(error) => Err(error.into()),
    }
}

fn has_openproxy_config(config: Option<&Map<String, Value>>) -> bool {
    let Some(providers) = config
        .and_then(|c| c.get("providers"))
        .and_then(Value::as_object)
    else {
        return false;
    };
    if providers
        .get(PROVIDER_ID)
        .and_then(|p| p.get("base_url"))
        .and_then(Value::as_str)
        .is_some()
    {
        return true;
    }
    providers.values().any(|prov| {
        prov.get("base_url")
            .and_then(Value::as_str)
            .is_some_and(|url| url.contains("20128"))
    })
}

async fn write_crush_settings(
    state: &AppState,
    body: &SaveCrushSettingsRequest,
    base_url: &str,
) -> AnyhowResult<String> {
    let config_path = crush_config_path();
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let mut existing = read_config()
        .await?
        .as_object()
        .cloned()
        .unwrap_or_default();
    let normalized_base_url = normalize_base_url(base_url);
    let model_id = body
        .model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| "provider/model-id".to_string());
    let api_key = resolve_cli_api_key(state, body.api_key.as_deref());

    let provider = json!({
        "type": "openai-compat",
        "base_url": normalized_base_url,
        "api_key": api_key,
        "models": [
            {
                "id": model_id,
                "name": model_id,
                "context_window": 128000,
            }
        ],
    });

    let providers = existing
        .entry("providers".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !providers.is_object() {
        *providers = Value::Object(Map::new());
    }
    if let Value::Object(ref mut providers_map) = providers {
        providers_map.insert(PROVIDER_ID.to_string(), provider);
    }

    let output = serde_json::to_string_pretty(&Value::Object(existing))?;
    fs::write(&config_path, format!("{output}\n")).await?;
    Ok(config_path.to_string_lossy().to_string())
}

async fn reset_crush_settings() -> AnyhowResult<Value> {
    let config_path = crush_config_path();
    // Distinguish "file absent/unparseable" (upstream's catch → early return)
    // from "valid JSON without our provider" (still reports removal).
    let raw = match fs::read_to_string(&config_path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "success": true,
                "message": "No config file to reset",
            }));
        }
        Err(error) => return Err(error.into()),
    };
    // Upstream's try wraps both readFile and JSON.parse — either failure is
    // the "No config file to reset" early return.
    let mut existing: Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => {
            return Ok(json!({
                "success": true,
                "message": "No config file to reset",
            }));
        }
    };

    let mut changed = false;
    if let Some(providers) = existing.get_mut("providers").and_then(Value::as_object_mut) {
        if providers.remove(PROVIDER_ID).is_some() {
            changed = true;
            if providers.is_empty() {
                if let Some(root) = existing.as_object_mut() {
                    root.remove("providers");
                }
            }
        }
    }

    if changed {
        let output = serde_json::to_string_pretty(&existing)?;
        fs::write(&config_path, format!("{output}\n")).await?;
    }

    Ok(json!({
        "success": true,
        "message": "9Router removed from Crush",
    }))
}

fn crush_config_path() -> PathBuf {
    config_home_dir().join("crush").join("crush.json")
}
