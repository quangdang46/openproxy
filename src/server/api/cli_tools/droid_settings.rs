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
use serde_json::{json, Map, Value};
use tokio::{fs, process::Command};

use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/cli-tools/droid-settings",
        get(get_droid_settings)
            .post(save_droid_settings)
            .delete(delete_droid_settings),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DroidSettingsRequest {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub models: Option<Vec<String>>,
    pub active_model: Option<String>,
}

async fn get_droid_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let installed = check_droid_installed().await;
    if !installed {
        return Json(json!({
            "installed": false,
            "settings": Value::Null,
            "message": "Factory Droid CLI is not installed",
        }))
        .into_response();
    }

    match read_droid_settings().await {
        Ok(settings) => {
            let has_openproxy = settings.as_ref().is_some_and(has_openproxy_droid_settings);
            Json(json!({
                "installed": true,
                "settings": settings,
                "hasOpenProxy": has_openproxy,
                "settingsPath": droid_settings_path().to_string_lossy().to_string(),
            }))
            .into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to check droid settings: {error}") })),
        )
            .into_response(),
    }
}

async fn save_droid_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<DroidSettingsRequest>,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let models = req.models.clone().unwrap_or_else(|| {
        req.model
            .clone()
            .map(|model| vec![model])
            .unwrap_or_default()
    });
    if req.base_url.trim().is_empty() || models.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "baseUrl and at least one model are required" })),
        )
            .into_response();
    }

    match write_droid_settings(&req, &models).await {
        Ok(settings_path) => Json(json!({
            "success": true,
            "message": "Factory Droid settings applied successfully!",
            "settingsPath": settings_path,
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to update droid settings: {error}") })),
        )
            .into_response(),
    }
}

async fn delete_droid_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_droid_settings().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to reset droid settings: {error}") })),
        )
            .into_response(),
    }
}

async fn check_droid_installed() -> bool {
    command_exists("droid", true).await || fs::metadata(droid_settings_path()).await.is_ok()
}

async fn read_droid_settings() -> AnyhowResult<Option<Value>> {
    read_json_optional(&droid_settings_path()).await
}

async fn write_droid_settings(
    req: &DroidSettingsRequest,
    models: &[String],
) -> AnyhowResult<String> {
    let settings_path = droid_settings_path();
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let mut settings = match fs::read_to_string(&settings_path).await {
        Ok(existing) => parse_json_object_or_default(&existing),
        Err(_) => serde_json::Map::new(),
    };

    let custom_models_value = settings.remove("customModels");
    let mut custom_models = match custom_models_value {
        Some(Value::Array(entries)) => entries,
        Some(Value::Null) | None => Vec::new(),
        Some(_) => {
            return Err(anyhow::anyhow!("customModels must be an array"));
        }
    };
    custom_models.retain(|entry| {
        !entry
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.starts_with("custom:OpenProxy"))
    });

    let normalized_base_url = normalize_v1_base_url(&req.base_url);
    let api_key = req
        .api_key
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "your_api_key".to_string());

    let default_index = match req.active_model.as_deref() {
        Some("") => None,
        Some(active_model) => Some(
            models
                .iter()
                .position(|model| model == active_model)
                .unwrap_or(0),
        ),
        None => Some(0),
    };

    for (index, model) in models.iter().enumerate() {
        if model.is_empty() {
            continue;
        }
        custom_models.push(json!({
            "model": model,
            "id": format!("custom:OpenProxy-{index}"),
            "index": index,
            "baseUrl": normalized_base_url,
            "apiKey": api_key,
            "displayName": model,
            "maxOutputTokens": 131072,
            "noImageSupport": false,
            "provider": "openai",
        }));
    }

    // Intentionally matches openproxy's whole-array reordering behavior, including
    // pre-existing non-OpenProxy entries that may shift indexes.
    if let Some(default_index) = default_index {
        if default_index < custom_models.len() {
            let default_entry = custom_models.remove(default_index);
            custom_models.insert(0, default_entry);
            for (index, entry) in custom_models.iter_mut().enumerate() {
                if let Some(object) = entry.as_object_mut() {
                    object.insert("index".to_string(), Value::from(index));
                }
            }
        }
    }

    settings.insert("customModels".to_string(), Value::Array(custom_models));
    fs::write(
        &settings_path,
        serde_json::to_vec_pretty(&Value::Object(settings))?,
    )
    .await?;
    Ok(settings_path.to_string_lossy().to_string())
}

async fn reset_droid_settings() -> AnyhowResult<Value> {
    let settings_path = droid_settings_path();
    let mut settings = match fs::read_to_string(&settings_path).await {
        Ok(existing) => parse_json_object_or_default(&existing),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "success": true,
                "message": "No settings file to reset",
            }));
        }
        Err(error) => return Err(error.into()),
    };

    if let Some(custom_models_value) = settings.remove("customModels") {
        let mut custom_models = match custom_models_value {
            Value::Array(entries) => entries,
            Value::Null => Vec::new(),
            _ => {
                return Err(anyhow::anyhow!("customModels must be an array"));
            }
        };
        custom_models.retain(|entry| {
            !entry
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with("custom:OpenProxy"))
        });
        if !custom_models.is_empty() {
            settings.insert("customModels".to_string(), Value::Array(custom_models));
        }
    }

    fs::write(
        &settings_path,
        serde_json::to_vec_pretty(&Value::Object(settings))?,
    )
    .await?;
    Ok(json!({
        "success": true,
        "message": "OpenProxy settings removed successfully",
    }))
}

fn has_openproxy_droid_settings(settings: &Value) -> bool {
    settings
        .get("customModels")
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.starts_with("custom:OpenProxy"))
            })
        })
}

fn droid_settings_path() -> PathBuf {
    home_dir().join(".factory").join("settings.json")
}

fn normalize_v1_base_url(base_url: &str) -> String {
    if base_url.ends_with("/v1") {
        base_url.to_string()
    } else {
        format!("{base_url}/v1")
    }
}

fn parse_json_object_or_default(content: &str) -> Map<String, Value> {
    match serde_json::from_str::<Value>(content) {
        Ok(Value::Object(object)) => object,
        _ => serde_json::Map::new(),
    }
}

async fn command_exists(program: &str, inject_windows_npm_path: bool) -> bool {
    let finder = if cfg!(windows) { "where" } else { "which" };
    let mut command = Command::new(finder);
    command.arg(program);

    if cfg!(windows) && inject_windows_npm_path {
        if let Some(path) = windows_npm_augmented_path() {
            command.env("PATH", path);
        }
    }

    command
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

fn windows_npm_augmented_path() -> Option<String> {
    let appdata = env::var_os("APPDATA")?;
    let current_path = env::var_os("PATH").unwrap_or_default();
    let npm_dir = PathBuf::from(appdata).join("npm");
    Some(format!(
        "{};{}",
        npm_dir.to_string_lossy(),
        PathBuf::from(current_path).to_string_lossy()
    ))
}

async fn read_json_optional(path: &Path) -> AnyhowResult<Option<Value>> {
    let content = match fs::read_to_string(path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(serde_json::from_str(&content)?))
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}
