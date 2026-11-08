//! CodeWhale settings — port of 9router
//! `src/app/api/cli-tools/codewhale-settings/route.js`.
//!
//! Config: `~/.codewhale/config.toml`. The `[openai]` table is replaced with
//! `{base_url, api_key, model}` and the file gets a "managed by 9Router"
//! header comment on apply.

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
use serde_json::{json, Value};
use tokio::fs;
use toml::{map::Map as TomlMap, Value as TomlValue};

use super::shared::{command_exists, home_dir, normalize_base_url, resolve_cli_api_key};
use crate::server::state::AppState;

const MANAGED_HEADER: &str = "# CodeWhale config — managed by 9Router\n\n";

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/cli-tools/codewhale-settings",
        get(get_codewhale_settings)
            .post(save_codewhale_settings)
            .delete(delete_codewhale_settings),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveCodewhaleSettingsRequest {
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
}

pub(super) async fn get_codewhale_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    if !check_codewhale_installed().await {
        return Json(json!({
            "installed": false,
            "config": Value::Null,
            "message": "CodeWhale CLI is not installed",
        }))
        .into_response();
    }

    let content = read_config_raw().await;
    let config = content
        .as_deref()
        .and_then(|raw| toml::from_str::<TomlValue>(raw).ok())
        .and_then(|v| serde_json::to_value(v).ok())
        .filter(|v| !v.is_null());

    Json(json!({
        "installed": true,
        "config": config.unwrap_or(Value::Null),
        "has9Router": content.as_deref().is_some_and(has_openproxy_in_toml),
        "configPath": codewhale_config_path().to_string_lossy().to_string(),
    }))
    .into_response()
}

async fn save_codewhale_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SaveCodewhaleSettingsRequest>,
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

    match write_codewhale_settings(&state, &body, &base_url).await {
        Ok(config_path) => Json(json!({
            "success": true,
            "message": "CodeWhale settings applied successfully!",
            "configPath": config_path,
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to write codewhale settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to update codewhale settings: {error}") })),
            )
                .into_response()
        }
    }
}

async fn delete_codewhale_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_codewhale_settings().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to reset codewhale settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to reset codewhale settings: {error}") })),
            )
                .into_response()
        }
    }
}

async fn check_codewhale_installed() -> bool {
    if command_exists("codewhale").await {
        return true;
    }
    fs::metadata(codewhale_config_path()).await.is_ok()
}

async fn read_config_raw() -> Option<String> {
    fs::read_to_string(codewhale_config_path()).await.ok()
}

fn has_openproxy_in_toml(content: &str) -> bool {
    content.contains("managed by 9Router") || content.contains("localhost:20128")
}

async fn write_codewhale_settings(
    state: &AppState,
    body: &SaveCodewhaleSettingsRequest,
    base_url: &str,
) -> AnyhowResult<String> {
    let config_path = codewhale_config_path();
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let raw = read_config_raw().await.unwrap_or_default();
    let mut existing = toml::from_str::<TomlValue>(&raw)
        .unwrap_or(TomlValue::Table(TomlMap::new()));
    if !existing.is_table() {
        existing = TomlValue::Table(TomlMap::new());
    }

    let normalized_base_url = normalize_base_url(base_url);
    let api_key = resolve_cli_api_key(state, body.api_key.as_deref());
    let model = body
        .model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| "provider/model-id".to_string());

    let openai = TomlValue::Table(TomlMap::from_iter([
        ("base_url".to_string(), TomlValue::String(normalized_base_url)),
        ("api_key".to_string(), TomlValue::String(api_key)),
        ("model".to_string(), TomlValue::String(model)),
    ]));

    if let TomlValue::Table(ref mut table) = existing {
        table.insert("openai".to_string(), openai);
    }

    let body_toml = toml::to_string_pretty(&existing)?;
    fs::write(&config_path, format!("{MANAGED_HEADER}{body_toml}")).await?;
    Ok(config_path.to_string_lossy().to_string())
}

async fn reset_codewhale_settings() -> AnyhowResult<Value> {
    let config_path = codewhale_config_path();
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

    // Same as forge: read+parse share one upstream try — parse failure is the
    // early "No config file to reset" return.
    let mut existing: TomlValue = match toml::from_str(&raw) {
        Ok(value) => value,
        Err(_) => {
            return Ok(json!({
                "success": true,
                "message": "No config file to reset",
            }));
        }
    };
    if let TomlValue::Table(ref mut table) = existing {
        table.remove("openai");
    }

    let is_empty = existing
        .as_table()
        .map(|t| t.is_empty())
        .unwrap_or(true);
    if is_empty {
        match fs::remove_file(&config_path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    } else {
        let output = toml::to_string_pretty(&existing)?;
        fs::write(&config_path, output).await?;
    }

    Ok(json!({
        "success": true,
        "message": "9Router removed from CodeWhale",
    }))
}

fn codewhale_dir() -> PathBuf {
    home_dir().join(".codewhale")
}

fn codewhale_config_path() -> PathBuf {
    codewhale_dir().join("config.toml")
}
