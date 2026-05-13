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
use tokio::{fs, process::Command};

use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/cli-tools/kilo-settings",
        get(get_kilo_settings)
            .post(save_kilo_settings)
            .delete(delete_kilo_settings),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveKiloSettingsRequest {
    base_url: String,
    api_key: String,
    model: String,
}

async fn get_kilo_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let installed = check_installed().await;
    if !installed {
        return Json(json!({
            "installed": false,
            "settings": Value::Null,
            "message": "Kilo Code CLI is not installed",
        }))
        .into_response();
    }

    match read_auth().await {
        Ok(auth) => {
            let has_openproxy = has_openproxy_config(&auth);
            let auth_keys = auth
                .as_ref()
                .and_then(|value| value.as_object())
                .map(|object| object.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            Json(json!({
                "installed": true,
                "settings": { "auth": auth_keys },
                "hasOpenProxy": has_openproxy,
                "authPath": auth_path().to_string_lossy().to_string(),
            }))
            .into_response()
        }
        Err(error) => {
            tracing::warn!(?error, "failed to read kilo settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to check kilo settings" })),
            )
                .into_response()
        }
    }
}

async fn save_kilo_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SaveKiloSettingsRequest>,
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

    match write_kilo_settings(&body).await {
        Ok(()) => Json(json!({
            "success": true,
            "message": "Kilo Code settings applied successfully!",
            "authPath": auth_path().to_string_lossy().to_string(),
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to write kilo settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to update kilo settings" })),
            )
                .into_response()
        }
    }
}

async fn delete_kilo_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_kilo_settings().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to reset kilo settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to reset kilo settings" })),
            )
                .into_response()
        }
    }
}

async fn check_installed() -> bool {
    if command_exists("kilo", true).await {
        return true;
    }
    fs::metadata(auth_path()).await.is_ok()
}

async fn read_auth() -> AnyhowResult<Option<Value>> {
    read_json_optional(&auth_path()).await
}

fn has_openproxy_config(auth: &Option<Value>) -> bool {
    let Some(auth) = auth else { return false };
    let entry = auth
        .get("openai-compatible")
        .or_else(|| auth.get("openproxy"))
        .or_else(|| auth.get("9router"));
    let Some(entry) = entry else { return false };
    let base_url = entry
        .get("baseUrl")
        .or_else(|| entry.get("baseURL"))
        .and_then(Value::as_str)
        .unwrap_or("");
    base_url.contains("localhost")
        || base_url.contains("127.0.0.1")
        || base_url.contains("openproxy")
}

async fn write_kilo_settings(body: &SaveKiloSettingsRequest) -> AnyhowResult<()> {
    fs::create_dir_all(&data_dir()).await?;

    let normalized_base_url = if body.base_url.ends_with("/v1") {
        body.base_url.clone()
    } else {
        format!("{}/v1", body.base_url)
    };

    let mut auth = read_json_optional(&auth_path())
        .await?
        .and_then(|value| match value {
            Value::Object(fields) => Some(fields),
            _ => None,
        })
        .unwrap_or_default();
    auth.insert(
        "openai-compatible".to_string(),
        json!({
            "type": "api-key",
            "apiKey": body.api_key,
            "baseUrl": normalized_base_url,
            "model": body.model,
        }),
    );
    write_json(&auth_path(), &Value::Object(auth)).await?;

    // Best-effort: update VS Code extension settings (ignore failures).
    if let Some(vscode_path) = vscode_settings_path() {
        if let Ok(Some(Value::Object(mut vscode))) = read_json_optional(&vscode_path).await {
            vscode.insert(
                "kilocode.customProvider".to_string(),
                json!({
                    "name": "OpenProxy",
                    "baseURL": normalized_base_url,
                    "apiKey": body.api_key,
                }),
            );
            vscode.insert(
                "kilocode.defaultModel".to_string(),
                Value::String(body.model.clone()),
            );
            let _ = write_json(&vscode_path, &Value::Object(vscode)).await;
        }
    }

    Ok(())
}

async fn reset_kilo_settings() -> AnyhowResult<Value> {
    let auth = read_json_optional(&auth_path()).await?;
    let Some(Value::Object(mut auth)) = auth else {
        return Ok(json!({
            "success": true,
            "message": "No settings file to reset",
        }));
    };
    auth.remove("openai-compatible");
    auth.remove("openproxy");
    auth.remove("9router");
    write_json(&auth_path(), &Value::Object(auth)).await?;

    if let Some(vscode_path) = vscode_settings_path() {
        if let Ok(Some(Value::Object(mut vscode))) = read_json_optional(&vscode_path).await {
            let modified = vscode.remove("kilocode.customProvider").is_some()
                | vscode.remove("kilocode.defaultModel").is_some();
            if modified {
                let _ = write_json(&vscode_path, &Value::Object(vscode)).await;
            }
        }
    }

    Ok(json!({
        "success": true,
        "message": "OpenProxy settings removed from Kilo Code",
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
    home_dir().join(".local").join("share").join("kilo")
}

fn auth_path() -> PathBuf {
    data_dir().join("auth.json")
}

fn vscode_settings_path() -> Option<PathBuf> {
    Some(
        home_dir()
            .join(".config")
            .join("Code")
            .join("User")
            .join("settings.json"),
    )
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}
