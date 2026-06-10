use std::env;
use std::path::{Path, PathBuf};

use anyhow::Result as AnyhowResult;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::fs;

use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/cli-tools/continue-settings",
        get(get_continue_settings)
            .post(save_continue_settings)
            .delete(delete_continue_settings),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveContinueSettingsRequest {
    base_url: String,
    api_key: String,
    model: String,
}

async fn get_continue_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let installed = fs::metadata(continue_config_path()).await.is_ok();
    if !installed {
        return Json(json!({
            "installed": false,
            "settings": Value::Null,
            "message": "Continue extension config not found",
        }))
        .into_response();
    }

    match read_config().await {
        Ok(config) => {
            let has_openproxy = has_openproxy_config(&config);
            let models = config
                .as_ref()
                .and_then(|c| c.get("models"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Json(json!({
                "installed": true,
                "settings": {
                    "models": models,
                },
                "hasOpenProxy": has_openproxy,
                "configPath": continue_config_path().to_string_lossy().to_string(),
            }))
            .into_response()
        }
        Err(error) => {
            tracing::warn!(?error, "failed to read continue config");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to read Continue config" })),
            )
                .into_response()
        }
    }
}

async fn save_continue_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SaveContinueSettingsRequest>,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    if body.base_url.trim().is_empty()
        || body.api_key.trim().is_empty()
        || body.model.trim().is_empty()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "baseUrl, apiKey and model are required" })),
        )
            .into_response();
    }

    match write_continue_settings(&body).await {
        Ok(path) => Json(json!({
            "success": true,
            "message": "Continue config updated successfully! Reload VS Code to take effect.",
            "configPath": path,
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to write continue config");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to update Continue config" })),
            )
                .into_response()
        }
    }
}

async fn delete_continue_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_continue_settings().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to reset continue config");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to reset Continue config" })),
            )
                .into_response()
        }
    }
}

async fn read_config() -> AnyhowResult<Option<Value>> {
    let content = match fs::read_to_string(continue_config_path()).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(serde_json::from_str(&content)?))
}

fn has_openproxy_config(config: &Option<Value>) -> bool {
    let Some(Value::Object(parsed)) = config else {
        return false;
    };
    let Some(Value::Array(models)) = parsed.get("models") else {
        return false;
    };
    models.iter().any(|model| {
        model.get("provider").and_then(Value::as_str) == Some("openai")
            && model
                .get("apiBase")
                .and_then(Value::as_str)
                .is_some_and(|base| {
                    let lower = base.to_ascii_lowercase();
                    lower.contains("localhost")
                        || lower.contains("127.0.0.1")
                        || lower.contains("openproxy")
                })
    })
}

async fn write_continue_settings(body: &SaveContinueSettingsRequest) -> AnyhowResult<String> {
    let config_path = continue_config_path();
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let config = read_existing_or_default(&config_path).await;

    // Remove existing OpenProxy entries (provider="openai" pointing to localhost/openproxy)
    let mut models: Vec<Value> = config
        .get("models")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    models.retain(|model| {
        let provider = model.get("provider").and_then(Value::as_str);
        let api_base = model.get("apiBase").and_then(Value::as_str).unwrap_or("");
        provider != Some("openai")
            || !(api_base.to_ascii_lowercase().contains("localhost")
                || api_base.to_ascii_lowercase().contains("127.0.0.1")
                || api_base.to_ascii_lowercase().contains("openproxy"))
    });

    // Add new OpenProxy entry
    let normalized_base_url = if body.base_url.ends_with("/v1") {
        body.base_url.clone()
    } else {
        format!("{}/v1", body.base_url)
    };
    models.push(json!({
        "title": body.model,
        "provider": "openai",
        "model": body.model,
        "apiKey": body.api_key,
        "apiBase": normalized_base_url,
    }));

    let mut out = config.clone();
    out.insert("models".to_string(), Value::Array(models));

    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&Value::Object(out))?,
    )
    .await?;

    Ok(config_path.to_string_lossy().to_string())
}

async fn reset_continue_settings() -> AnyhowResult<Value> {
    let config_path = continue_config_path();
    let config = match fs::read_to_string(&config_path).await {
        Ok(content) => match serde_json::from_str::<serde_json::Map<String, Value>>(&content) {
            Ok(map) => map,
            Err(_) => {
                return Ok(json!({
                    "success": true,
                    "message": "No config file to reset",
                }));
            }
        },
        Err(_) => {
            return Ok(json!({
                "success": true,
                "message": "No config file to reset",
            }));
        }
    };

    let mut models: Vec<Value> = config
        .get("models")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    models.retain(|model| model.get("provider").and_then(Value::as_str) != Some("openai"));

    let mut out = config.clone();
    if !models.is_empty() {
        out.insert("models".to_string(), Value::Array(models));
    } else {
        out.insert("models".to_string(), Value::Array(Vec::new()));
    }

    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&Value::Object(out))?,
    )
    .await?;

    Ok(json!({
        "success": true,
        "message": "OpenProxy entry removed from Continue config",
    }))
}

async fn read_existing_or_default(path: &Path) -> serde_json::Map<String, Value> {
    match fs::read_to_string(path).await {
        Ok(content) => match serde_json::from_str::<serde_json::Map<String, Value>>(&content) {
            Ok(map) => map,
            Err(_) => serde_json::Map::new(),
        },
        Err(_) => serde_json::Map::new(),
    }
}

fn continue_config_path() -> PathBuf {
    if cfg!(windows) {
        env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".continue")
            .join("config.json")
    } else {
        home_dir().join(".continue").join("config.json")
    }
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}
