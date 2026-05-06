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
use tokio::fs;
use uuid::Uuid;

use crate::server::state::AppState;

const PROVIDER: &str = "gateway";

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/cli-tools/cowork-settings",
        get(get_cowork_settings)
            .post(save_cowork_settings)
            .delete(delete_cowork_settings),
    )
}

fn require_management_access(
    headers: &HeaderMap,
    state: &AppState,
) -> std::result::Result<(), Response> {
    super::super::require_dashboard_or_management_api_key(headers, state)
        .map_err(|response| response)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveCoworkSettingsRequest {
    base_url: Option<String>,
    api_key: Option<String>,
    models: Option<Vec<Value>>,
}

async fn get_cowork_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    match load_cowork_status().await {
        Ok(status) => Json(status).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to read cowork settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to read cowork settings" })),
            )
                .into_response()
        }
    }
}

async fn save_cowork_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SaveCoworkSettingsRequest>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let base_url = body.base_url.unwrap_or_default();
    let api_key = body.api_key.unwrap_or_default();
    if base_url.is_empty() || api_key.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "baseUrl and apiKey are required" })),
        )
            .into_response();
    }

    if is_localhost_url(&base_url) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Claude Cowork sandbox cannot reach localhost. Enable Tunnel/Cloud Endpoint or use Tailscale/VPS."
            })),
        )
            .into_response();
    }

    let models = body
        .models
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .filter(|value| !value.trim().is_empty())
        .collect::<Vec<_>>();
    if models.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "At least one model is required" })),
        )
            .into_response();
    }

    match write_cowork_settings(&base_url, &api_key, &models).await {
        Ok((bootstrapped, config_path)) => Json(json!({
            "success": true,
            "bootstrapped": bootstrapped,
            "message": if bootstrapped {
                "Cowork enabled (3p mode set). Quit & reopen Claude Desktop."
            } else {
                "Cowork settings applied. Quit & reopen Claude Desktop."
            },
            "configPath": config_path,
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to write cowork settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to apply cowork settings" })),
            )
                .into_response()
        }
    }
}

async fn delete_cowork_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    match reset_cowork_settings().await {
        Ok(response) => Json(response).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to reset cowork settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to reset cowork settings" })),
            )
                .into_response()
        }
    }
}

async fn load_cowork_status() -> AnyhowResult<Value> {
    let installed = check_installed().await;
    if !installed {
        return Ok(json!({
            "installed": false,
            "config": Value::Null,
            "message": "Claude Desktop (Cowork mode) not detected",
        }));
    }

    let meta = read_json_optional(&meta_path(resolve_app_root_for_read().await).await?).await?;
    let applied_id = meta.as_ref().and_then(meta_applied_id);
    let config_dir = config_dir(resolve_app_root_for_read().await).await?;
    let config_path = applied_id
        .as_deref()
        .map(|id| config_dir.join(format!("{id}.json")));
    let config = match config_path.as_ref() {
        Some(path) => read_json_optional(path).await?,
        None => None,
    };

    let base_url = config
        .as_ref()
        .and_then(|value| value.get("inferenceGatewayBaseUrl"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let models = config
        .as_ref()
        .and_then(|value| value.get("inferenceModels"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| match value {
                    Value::String(name) => Some(name.clone()),
                    Value::Object(fields) => fields
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let provider = config
        .as_ref()
        .and_then(|value| value.get("inferenceProvider"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let has_9router = provider.as_deref() == Some(PROVIDER)
        && base_url.as_deref().is_some_and(|value| !value.is_empty());

    Ok(json!({
        "installed": true,
        "config": config,
        "has9Router": has_9router,
        "configPath": config_path.map(|path| path.to_string_lossy().to_string()),
        "cowork": {
            "appliedId": applied_id,
            "baseUrl": base_url,
            "models": models,
            "provider": provider,
        },
    }))
}

async fn write_cowork_settings(
    base_url: &str,
    api_key: &str,
    models: &[String],
) -> AnyhowResult<(bool, String)> {
    let bootstrapped = bootstrap_deployment_mode().await?;
    let meta = ensure_meta().await?;
    let applied_id = meta_applied_id(&meta).unwrap_or_else(|| Uuid::new_v4().to_string());
    let config_path = write_config_dir().await?.join(format!("{applied_id}.json"));

    write_json(
        &config_path,
        &json!({
            "inferenceProvider": PROVIDER,
            "inferenceGatewayBaseUrl": base_url,
            "inferenceGatewayApiKey": api_key,
            "inferenceModels": models
                .iter()
                .map(|name| json!({ "name": name }))
                .collect::<Vec<_>>(),
        }),
    )
    .await?;

    Ok((bootstrapped, config_path.to_string_lossy().to_string()))
}

async fn reset_cowork_settings() -> AnyhowResult<Value> {
    let read_root = resolve_app_root_for_read().await;
    let meta = read_json_optional(&meta_path(read_root.clone()).await?).await?;
    let Some(applied_id) = meta.as_ref().and_then(meta_applied_id) else {
        return Ok(json!({
            "success": true,
            "message": "No active config to reset",
        }));
    };

    let config_path = config_dir(read_root)
        .await?
        .join(format!("{applied_id}.json"));
    let result = fs::write(&config_path, "{}").await;
    if let Err(error) = result {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.into());
        }
    }

    Ok(json!({
        "success": true,
        "message": "Cowork config reset",
    }))
}

async fn bootstrap_deployment_mode() -> AnyhowResult<bool> {
    let cfg_path = one_party_root().join("claude_desktop_config.json");
    let mut cfg = match read_json_optional(&cfg_path).await? {
        Some(Value::Object(fields)) => fields,
        _ => Map::new(),
    };

    if cfg
        .get("deploymentMode")
        .and_then(Value::as_str)
        .is_some_and(|value| value == "3p")
    {
        return Ok(false);
    }

    cfg.insert(
        "deploymentMode".to_string(),
        Value::String("3p".to_string()),
    );
    if let Some(parent) = cfg_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    write_json(&cfg_path, &Value::Object(cfg)).await?;
    Ok(true)
}

async fn ensure_meta() -> AnyhowResult<Value> {
    let write_meta_path = write_meta_path().await?;
    let mut meta = read_json_optional(&write_meta_path).await?;
    if meta.as_ref().and_then(meta_applied_id).is_none() {
        let existing_meta =
            read_json_optional(&meta_path(resolve_app_root_for_read().await).await?).await?;
        meta = existing_meta.filter(|value| meta_applied_id(value).is_some());

        if meta.as_ref().and_then(meta_applied_id).is_none() {
            let new_id = Uuid::new_v4().to_string();
            meta = Some(json!({
                "appliedId": new_id,
                "entries": [
                    {
                        "id": new_id,
                        "name": "Default"
                    }
                ]
            }));
        }

        if let Some(parent) = write_meta_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        if let Some(value) = meta.as_ref() {
            write_json(&write_meta_path, value).await?;
        }
    }

    Ok(meta.unwrap_or_else(|| json!({})))
}

fn meta_applied_id(value: &Value) -> Option<String> {
    value
        .get("appliedId")
        .and_then(Value::as_str)
        .map(str::to_string)
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

async fn resolve_app_root_for_read() -> PathBuf {
    let candidates = candidate_roots();
    for dir in &candidates {
        if path_exists(&dir.join("configLibrary")).await {
            return dir.clone();
        }
    }
    candidates
        .into_iter()
        .next()
        .unwrap_or_else(|| home_dir().join(".config").join("Claude-3p"))
}

async fn check_installed() -> bool {
    for dir in candidate_roots().into_iter().chain(app_install_paths()) {
        if path_exists(&dir).await {
            return true;
        }
    }
    false
}

async fn path_exists(path: &Path) -> bool {
    fs::metadata(path).await.is_ok()
}

fn is_localhost_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("localhost") || lower.contains("127.0.0.1") || lower.contains("0.0.0.0")
}

fn candidate_roots() -> Vec<PathBuf> {
    match env::consts::OS {
        "macos" => {
            let base = home_dir().join("Library").join("Application Support");
            vec![base.join("Claude-3p"), base.join("Claude")]
        }
        "windows" => {
            let local_app = env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home_dir().join("AppData").join("Local"));
            let roaming = env::var_os("APPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home_dir().join("AppData").join("Roaming"));
            vec![
                local_app.join("Claude-3p"),
                roaming.join("Claude-3p"),
                local_app.join("Claude"),
                roaming.join("Claude"),
            ]
        }
        _ => vec![
            home_dir().join(".config").join("Claude-3p"),
            home_dir().join(".config").join("Claude"),
        ],
    }
}

fn app_install_paths() -> Vec<PathBuf> {
    match env::consts::OS {
        "macos" => vec![
            PathBuf::from("/Applications/Claude.app"),
            home_dir().join("Applications").join("Claude.app"),
        ],
        "windows" => {
            let local_app = env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home_dir().join("AppData").join("Local"));
            let program_files = env::var_os("ProgramFiles")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
            vec![
                local_app.join("AnthropicClaude"),
                program_files.join("Claude"),
                program_files.join("AnthropicClaude"),
            ]
        }
        _ => Vec::new(),
    }
}

fn one_party_root() -> PathBuf {
    match env::consts::OS {
        "macos" => home_dir()
            .join("Library")
            .join("Application Support")
            .join("Claude"),
        "windows" => env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData").join("Roaming"))
            .join("Claude"),
        _ => home_dir().join(".config").join("Claude"),
    }
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}

async fn config_dir(root: PathBuf) -> AnyhowResult<PathBuf> {
    Ok(root.join("configLibrary"))
}

async fn write_config_dir() -> AnyhowResult<PathBuf> {
    Ok(candidate_roots()
        .into_iter()
        .next()
        .unwrap_or_else(|| home_dir().join(".config").join("Claude-3p"))
        .join("configLibrary"))
}

async fn meta_path(root: PathBuf) -> AnyhowResult<PathBuf> {
    Ok(config_dir(root).await?.join("_meta.json"))
}

async fn write_meta_path() -> AnyhowResult<PathBuf> {
    Ok(write_config_dir().await?.join("_meta.json"))
}
