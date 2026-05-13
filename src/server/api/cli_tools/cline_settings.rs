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
        "/api/cli-tools/cline-settings",
        get(get_cline_settings)
            .post(save_cline_settings)
            .delete(delete_cline_settings),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveClineSettingsRequest {
    base_url: String,
    api_key: String,
    model: String,
}

async fn get_cline_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let installed = check_installed().await;
    if !installed {
        return Json(json!({
            "installed": false,
            "settings": Value::Null,
            "message": "Cline CLI is not installed",
        }))
        .into_response();
    }

    match read_global_state().await {
        Ok(global_state) => {
            let has_openproxy = has_openproxy_config(&global_state);
            let settings = json!({
                "actModeApiProvider": global_state.as_ref().and_then(|s| s.get("actModeApiProvider")).cloned().unwrap_or(Value::Null),
                "planModeApiProvider": global_state.as_ref().and_then(|s| s.get("planModeApiProvider")).cloned().unwrap_or(Value::Null),
                "openAiBaseUrl": global_state.as_ref().and_then(|s| s.get("openAiBaseUrl")).cloned().unwrap_or(Value::Null),
                "openAiModelId": global_state.as_ref().and_then(|s| s.get("openAiModelId")).cloned().unwrap_or(Value::Null),
            });
            Json(json!({
                "installed": true,
                "settings": settings,
                "hasOpenProxy": has_openproxy,
                "globalStatePath": global_state_path().to_string_lossy().to_string(),
            }))
            .into_response()
        }
        Err(error) => {
            tracing::warn!(?error, "failed to read cline settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to check cline settings" })),
            )
                .into_response()
        }
    }
}

async fn save_cline_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SaveClineSettingsRequest>,
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

    match write_cline_settings(&body).await {
        Ok(()) => Json(json!({
            "success": true,
            "message": "Cline settings applied successfully!",
            "globalStatePath": global_state_path().to_string_lossy().to_string(),
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to write cline settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to update cline settings" })),
            )
                .into_response()
        }
    }
}

async fn delete_cline_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_cline_settings().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to reset cline settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to reset cline settings" })),
            )
                .into_response()
        }
    }
}

async fn check_installed() -> bool {
    if command_exists("cline", true).await {
        return true;
    }
    fs::metadata(global_state_path()).await.is_ok()
}

async fn read_global_state() -> AnyhowResult<Option<Value>> {
    read_json_optional(&global_state_path()).await
}

fn has_openproxy_config(global_state: &Option<Value>) -> bool {
    let Some(state) = global_state else {
        return false;
    };
    let is_openai = matches!(
        state.get("actModeApiProvider").and_then(Value::as_str),
        Some("openai")
    ) || matches!(
        state.get("planModeApiProvider").and_then(Value::as_str),
        Some("openai")
    );
    if !is_openai {
        return false;
    }
    let base_url = state
        .get("openAiBaseUrl")
        .and_then(Value::as_str)
        .unwrap_or("");
    base_url.contains("localhost")
        || base_url.contains("127.0.0.1")
        || base_url.contains("openproxy")
}

async fn write_cline_settings(body: &SaveClineSettingsRequest) -> AnyhowResult<()> {
    fs::create_dir_all(&data_dir()).await?;

    // Cline expects base WITHOUT /v1
    let normalized_base_url = body
        .base_url
        .strip_suffix("/v1")
        .map(str::to_string)
        .unwrap_or_else(|| body.base_url.clone());

    let mut global_state = read_json_optional(&global_state_path())
        .await?
        .and_then(|value| match value {
            Value::Object(fields) => Some(fields),
            _ => None,
        })
        .unwrap_or_default();

    global_state.insert(
        "actModeApiProvider".to_string(),
        Value::String("openai".to_string()),
    );
    global_state.insert(
        "planModeApiProvider".to_string(),
        Value::String("openai".to_string()),
    );
    global_state.insert(
        "openAiBaseUrl".to_string(),
        Value::String(normalized_base_url),
    );
    global_state.insert(
        "openAiModelId".to_string(),
        Value::String(body.model.clone()),
    );
    global_state.insert(
        "planModeOpenAiModelId".to_string(),
        Value::String(body.model.clone()),
    );

    write_json(&global_state_path(), &Value::Object(global_state)).await?;

    let mut secrets = read_json_optional(&secrets_path())
        .await?
        .and_then(|value| match value {
            Value::Object(fields) => Some(fields),
            _ => None,
        })
        .unwrap_or_default();
    secrets.insert(
        "openAiApiKey".to_string(),
        Value::String(body.api_key.clone()),
    );
    write_json(&secrets_path(), &Value::Object(secrets)).await
}

async fn reset_cline_settings() -> AnyhowResult<Value> {
    let global_state = read_json_optional(&global_state_path()).await?;
    let Some(Value::Object(mut state)) = global_state else {
        return Ok(json!({
            "success": true,
            "message": "No settings file to reset",
        }));
    };

    if matches!(
        state.get("actModeApiProvider").and_then(Value::as_str),
        Some("openai")
    ) {
        state.remove("openAiBaseUrl");
        state.remove("openAiModelId");
        state.remove("planModeOpenAiModelId");
        state.insert(
            "actModeApiProvider".to_string(),
            Value::String("cline".to_string()),
        );
        state.insert(
            "planModeApiProvider".to_string(),
            Value::String("cline".to_string()),
        );
    }
    write_json(&global_state_path(), &Value::Object(state)).await?;

    let mut secrets: Map<String, Value> = read_json_optional(&secrets_path())
        .await?
        .and_then(|value| match value {
            Value::Object(fields) => Some(fields),
            _ => None,
        })
        .unwrap_or_default();
    secrets.remove("openAiApiKey");
    write_json(&secrets_path(), &Value::Object(secrets)).await?;

    Ok(json!({
        "success": true,
        "message": "OpenProxy settings removed from Cline",
    }))
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

async fn write_json(path: &Path, value: &Value) -> AnyhowResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(path, serde_json::to_vec_pretty(value)?).await?;
    Ok(())
}

fn data_dir() -> PathBuf {
    home_dir().join(".cline").join("data")
}

fn global_state_path() -> PathBuf {
    data_dir().join("globalState.json")
}

fn secrets_path() -> PathBuf {
    data_dir().join("secrets.json")
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}
