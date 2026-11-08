//! Smelt settings — port of 9router
//! `src/app/api/cli-tools/smelt-settings/route.js`.
//!
//! Config: `~/.smelt/config.json` — flat keys `baseUrl`, `apiKey`, `model`,
//! `_managedBy: "9router"` merged over the existing object.

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

use super::shared::{command_exists, home_dir, normalize_base_url, resolve_cli_api_key};
use crate::server::state::AppState;

const MANAGED_BY: &str = "9router";

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/cli-tools/smelt-settings",
        get(get_smelt_settings)
            .post(save_smelt_settings)
            .delete(delete_smelt_settings),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveSmeltSettingsRequest {
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
}

pub(super) async fn get_smelt_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    if !check_smelt_installed().await {
        return Json(json!({
            "installed": false,
            "config": Value::Null,
            "message": "Smelt CLI is not installed",
        }))
        .into_response();
    }

    let config = read_config().await.unwrap_or(Value::Null);
    Json(json!({
        "installed": true,
        "config": config,
        "has9Router": has_openproxy_config(config.as_object()),
        "configPath": smelt_config_path().to_string_lossy().to_string(),
    }))
    .into_response()
}

async fn save_smelt_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SaveSmeltSettingsRequest>,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let Some(base_url) = body.base_url.filter(|value| !value.trim().is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "baseUrl is required" })),
        )
            .into_response();
    };

    match write_smelt_settings(&state, &body, &base_url).await {
        Ok(config_path) => Json(json!({
            "success": true,
            "message": "Smelt settings applied successfully!",
            "configPath": config_path,
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to write smelt settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to update smelt settings: {error}") })),
            )
                .into_response()
        }
    }
}

async fn delete_smelt_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_smelt_settings().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to reset smelt settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to reset smelt settings: {error}") })),
            )
                .into_response()
        }
    }
}

async fn check_smelt_installed() -> bool {
    if command_exists("smelt").await {
        return true;
    }
    fs::metadata(smelt_config_path()).await.is_ok()
}

async fn read_config() -> AnyhowResult<Value> {
    match fs::read_to_string(smelt_config_path()).await {
        Ok(content) => Ok(serde_json::from_str(&content).unwrap_or(Value::Null)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
        Err(error) => Err(error.into()),
    }
}

fn has_openproxy_config(config: Option<&Map<String, Value>>) -> bool {
    let Some(config) = config else {
        return false;
    };
    if config.get("_managedBy").and_then(Value::as_str) == Some(MANAGED_BY) {
        return true;
    }
    config
        .get("baseUrl")
        .and_then(Value::as_str)
        .is_some_and(|url| !url.is_empty() && url.contains("20128"))
}

async fn write_smelt_settings(
    state: &AppState,
    body: &SaveSmeltSettingsRequest,
    base_url: &str,
) -> AnyhowResult<String> {
    let config_path = smelt_config_path();
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let mut existing = read_config()
        .await?
        .as_object()
        .cloned()
        .unwrap_or_default();

    let normalized_base_url = normalize_base_url(base_url);
    let model = body
        .model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .or_else(|| {
            existing
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "provider/model-id".to_string());

    existing.insert(
        "baseUrl".to_string(),
        Value::String(normalized_base_url),
    );
    existing.insert(
        "apiKey".to_string(),
        Value::String(resolve_cli_api_key(state, body.api_key.as_deref())),
    );
    existing.insert("model".to_string(), Value::String(model));
    existing.insert(
        "_managedBy".to_string(),
        Value::String(MANAGED_BY.to_string()),
    );

    let output = serde_json::to_string_pretty(&Value::Object(existing))?;
    fs::write(&config_path, format!("{output}\n")).await?;
    Ok(config_path.to_string_lossy().to_string())
}

async fn reset_smelt_settings() -> AnyhowResult<Value> {
    let config_path = smelt_config_path();
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
    // Upstream wraps readFile + JSON.parse in one try.
    let mut existing: Map<String, Value> = match serde_json::from_str(&raw) {
        Ok(Value::Object(map)) => map,
        _ => {
            return Ok(json!({
                "success": true,
                "message": "No config file to reset",
            }));
        }
    };

    for key in ["baseUrl", "apiKey", "model", "_managedBy"] {
        existing.remove(key);
    }

    if existing.is_empty() {
        match fs::remove_file(&config_path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    } else {
        let output = serde_json::to_string_pretty(&Value::Object(existing))?;
        fs::write(&config_path, format!("{output}\n")).await?;
    }

    Ok(json!({
        "success": true,
        "message": "Smelt 9Router settings removed",
    }))
}

fn smelt_dir() -> PathBuf {
    home_dir().join(".smelt")
}

fn smelt_config_path() -> PathBuf {
    smelt_dir().join("config.json")
}
