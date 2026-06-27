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

const DEFAULT_CONFIG: &str = "provider = \"deepseek\"\n";

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/cli-tools/deepseek-tui-settings",
        get(get_deepseek_tui_settings)
            .post(save_deepseek_tui_settings)
            .delete(delete_deepseek_tui_settings),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveDeepSeekSettingsRequest {
    base_url: String,
    #[serde(default)]
    api_key: Option<String>,
    model: String,
}

async fn get_deepseek_tui_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let installed = check_installed().await;
    if !installed {
        return Json(json!({
            "installed": false,
            "settings": Value::Null,
            "message": "DeepSeek TUI is not installed",
        }))
        .into_response();
    }

    match read_config_toml().await {
        Ok(config) => {
            let has_openproxy = has_openproxy_config(&config);
            Json(json!({
                "installed": true,
                "settings": config,
                "hasOpenProxy": has_openproxy,
                "configPath": config_path().to_string_lossy().to_string(),
            }))
            .into_response()
        }
        Err(error) => {
            tracing::warn!(?error, "failed to read deepseek-tui settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to check deepseek-tui settings" })),
            )
                .into_response()
        }
    }
}

async fn save_deepseek_tui_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SaveDeepSeekSettingsRequest>,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    if body.base_url.trim().is_empty() || body.model.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "baseUrl and model are required" })),
        )
            .into_response();
    }

    match write_deepseek_config(&body).await {
        Ok(()) => Json(json!({
            "success": true,
            "message": "DeepSeek TUI settings applied successfully!",
            "configPath": config_path().to_string_lossy().to_string(),
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to write deepseek-tui settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to update deepseek-tui settings" })),
            )
                .into_response()
        }
    }
}

async fn delete_deepseek_tui_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_deepseek_config().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to reset deepseek-tui settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to reset deepseek-tui settings" })),
            )
                .into_response()
        }
    }
}

async fn check_installed() -> bool {
    if command_exists("deepseek", true).await {
        return true;
    }
    fs::metadata(config_path()).await.is_ok()
}

async fn read_config_toml() -> AnyhowResult<Option<Value>> {
    let path = config_path();
    let content = match fs::read_to_string(&path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(parse_toml_flat(&content)))
}

/// Simple flat-key TOML parser matching the 9router JS custom parser behavior.
/// Dotted section headers like `[providers.openai]` become flat keys
/// `"providers.openai"` rather than nested objects — the frontend reads
/// `settings["providers.openai"].base_url`.
fn parse_toml_flat(content: &str) -> Value {
    let mut result = Map::new();
    let mut current_section: Option<String> = None;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Section header: [section] or [section.subsection]
        if let Some(rest) = trimmed.strip_prefix('[') {
            if let Some(section_name) = rest.strip_suffix(']') {
                if !result.contains_key(section_name) {
                    result.insert(section_name.to_string(), Value::Object(Map::new()));
                }
                current_section = Some(section_name.to_string());
                continue;
            }
        }

        // key = "value" or key = value
        if let Some(eq_pos) = trimmed.find('=') {
            let key = trimmed[..eq_pos].trim().to_string();
            let raw_value = trimmed[eq_pos + 1..].trim();

            let value = if raw_value.starts_with('"') && raw_value.ends_with('"') {
                Value::String(raw_value[1..raw_value.len() - 1].to_string())
            } else if raw_value.starts_with('\'') && raw_value.ends_with('\'') {
                Value::String(raw_value[1..raw_value.len() - 1].to_string())
            } else {
                Value::String(raw_value.to_string())
            };

            match &current_section {
                Some(section) => {
                    if let Some(Value::Object(obj)) = result.get_mut(section) {
                        obj.insert(key, value);
                    }
                }
                None => {
                    result.insert(key, value);
                }
            }
        }
    }

    Value::Object(result)
}

fn has_openproxy_config(config: &Option<Value>) -> bool {
    let Some(config) = config else {
        return false;
    };
    if config.get("provider").and_then(Value::as_str) != Some("openai") {
        return false;
    }
    let Some(base_url) = config
        .get("providers.openai")
        .and_then(|s| s.get("base_url"))
        .and_then(Value::as_str)
    else {
        return false;
    };
    base_url.contains("localhost")
        || base_url.contains("127.0.0.1")
        || base_url.contains("0.0.0.0")
        || base_url.contains("openproxy")
}

async fn write_deepseek_config(body: &SaveDeepSeekSettingsRequest) -> AnyhowResult<()> {
    let config_path = config_path();
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let normalized_base_url = if body.base_url.ends_with("/v1") {
        body.base_url.clone()
    } else {
        format!("{}/v1", body.base_url)
    };
    let api_key = body
        .api_key
        .clone()
        .filter(|k| !k.is_empty())
        .unwrap_or_else(|| "sk_9router".to_string());

    let config = format!(
        "provider = \"openai\"\n\
         \n\
         [providers.openai]\n\
         base_url = \"{normalized_base_url}\"\n\
         api_key = \"{api_key}\"\n\
         model = \"{model}\"\n",
        model = body.model
    );

    fs::write(&config_path, config).await?;
    Ok(())
}

async fn reset_deepseek_config() -> AnyhowResult<Value> {
    let config_path = config_path();
    match fs::metadata(&config_path).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "success": true,
                "message": "No config file to reset",
            }));
        }
        Err(error) => return Err(error.into()),
    }

    fs::write(&config_path, DEFAULT_CONFIG).await?;
    Ok(json!({
        "success": true,
        "message": "OpenProxy config reset to DeepSeek defaults",
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

fn config_path() -> PathBuf {
    home_dir().join(".deepseek").join("config.toml")
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}
