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

const RESET_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "API_TIMEOUT_MS",
];

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/cli-tools/claude-settings",
        get(get_claude_settings)
            .post(save_claude_settings)
            .delete(delete_claude_settings),
    )
}

#[derive(Debug, Deserialize)]
struct SaveClaudeSettingsRequest {
    env: Option<Map<String, Value>>,
}

async fn get_claude_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match check_claude_installed().await {
        true => match read_settings().await {
            Ok(settings) => {
                let has_9router = settings
                    .as_ref()
                    .and_then(|value| value.get("env"))
                    .and_then(|value| value.get("ANTHROPIC_BASE_URL"))
                    .and_then(Value::as_str)
                    .is_some();
                Json(json!({
                    "installed": true,
                    "settings": settings,
                    "has9Router": has_9router,
                    "settingsPath": claude_settings_path().to_string_lossy().to_string(),
                }))
                .into_response()
            }
            Err(error) => {
                tracing::warn!(?error, "failed to read claude settings");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "Failed to check claude settings" })),
                )
                    .into_response()
            }
        },
        false => Json(json!({
            "installed": false,
            "settings": Value::Null,
            "message": "Claude CLI is not installed",
        }))
        .into_response(),
    }
}

async fn save_claude_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SaveClaudeSettingsRequest>,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let Some(mut env_values) = body.env else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid env object" })),
        )
            .into_response();
    };

    normalize_claude_base_url(&mut env_values);

    match write_claude_settings(env_values).await {
        Ok(()) => Json(json!({
            "success": true,
            "message": "Settings updated successfully",
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to write claude settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to update claude settings" })),
            )
                .into_response()
        }
    }
}

async fn delete_claude_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_claude_settings().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to reset claude settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to reset claude settings" })),
            )
                .into_response()
        }
    }
}

async fn check_claude_installed() -> bool {
    if command_exists("claude", true).await {
        return true;
    }

    fs::metadata(claude_settings_path()).await.is_ok()
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

async fn read_settings() -> AnyhowResult<Option<Value>> {
    read_json_optional(&claude_settings_path()).await
}

async fn write_claude_settings(env_values: Map<String, Value>) -> AnyhowResult<()> {
    let settings_path = claude_settings_path();
    let claude_dir = settings_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home_dir().join(".claude"));
    fs::create_dir_all(&claude_dir).await?;

    let mut current_settings = match read_json_optional(&settings_path).await? {
        Some(Value::Object(fields)) => fields,
        _ => Map::new(),
    };

    let mut merged_env = current_settings
        .remove("env")
        .and_then(|value| match value {
            Value::Object(fields) => Some(fields),
            _ => None,
        })
        .unwrap_or_default();
    merged_env.extend(env_values);

    current_settings.insert("hasCompletedOnboarding".to_string(), Value::Bool(true));
    current_settings.insert("env".to_string(), Value::Object(merged_env));

    write_json(&settings_path, &Value::Object(current_settings)).await
}

async fn reset_claude_settings() -> AnyhowResult<Value> {
    let settings_path = claude_settings_path();
    let Some(mut current_settings) = read_json_optional(&settings_path).await?.and_then(|value| {
        if let Value::Object(fields) = value {
            Some(fields)
        } else {
            None
        }
    }) else {
        return Ok(json!({
            "success": true,
            "message": "No settings file to reset",
        }));
    };

    if let Some(Value::Object(env_values)) = current_settings.get_mut("env") {
        for key in RESET_ENV_KEYS {
            env_values.remove(*key);
        }
        if env_values.is_empty() {
            current_settings.remove("env");
        }
    }

    write_json(&settings_path, &Value::Object(current_settings)).await?;
    Ok(json!({
        "success": true,
        "message": "Settings reset successfully",
    }))
}

fn normalize_claude_base_url(env_values: &mut Map<String, Value>) {
    let Some(Value::String(base_url)) = env_values.get_mut("ANTHROPIC_BASE_URL") else {
        return;
    };
    if !base_url.ends_with("/v1") {
        *base_url = format!("{base_url}/v1");
    }
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

fn claude_settings_path() -> PathBuf {
    home_dir().join(".claude").join("settings.json")
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}
