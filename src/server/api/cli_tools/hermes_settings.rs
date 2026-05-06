use std::env;
use std::path::PathBuf;

use anyhow::Result as AnyhowResult;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use once_cell::sync::Lazy;
use regex::{Captures, Regex};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{fs, process::Command};

use crate::server::state::AppState;

const PROVIDER_NAME: &str = "9router";
const API_KEY_ENV: &str = "OPENAI_API_KEY";

static MODEL_BLOCK_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^model:[ \t]*\r?\n((?:[ \t]+.*\r?\n?|[ \t]*\r?\n)*)").unwrap());

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/cli-tools/hermes-settings",
        get(get_hermes_settings)
            .post(save_hermes_settings)
            .delete(delete_hermes_settings),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveHermesSettingsRequest {
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
}

async fn get_hermes_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    if !check_hermes_installed().await {
        return Json(json!({
            "installed": false,
            "settings": Value::Null,
            "message": "Hermes Agent is not installed",
        }))
        .into_response();
    }

    match read_config_yaml().await {
        Ok(yaml) => {
            let model = parse_model_block(&yaml);
            Json(json!({
                "installed": true,
                "settings": { "model": model.clone() },
                "has9Router": has_9router_config(model.as_ref()),
                "configPath": hermes_config_path().to_string_lossy().to_string(),
            }))
            .into_response()
        }
        Err(error) => {
            tracing::warn!(?error, "failed to read hermes settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to check hermes settings" })),
            )
                .into_response()
        }
    }
}

async fn save_hermes_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SaveHermesSettingsRequest>,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let Some(base_url) = body.base_url.filter(|value| !value.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "baseUrl and model are required" })),
        )
            .into_response();
    };
    let Some(model) = body.model.filter(|value| !value.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "baseUrl and model are required" })),
        )
            .into_response();
    };

    match write_hermes_settings(&base_url, body.api_key.as_deref(), &model).await {
        Ok(config_path) => Json(json!({
            "success": true,
            "message": "Hermes settings applied successfully!",
            "configPath": config_path,
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to write hermes settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to update hermes settings" })),
            )
                .into_response()
        }
    }
}

async fn delete_hermes_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_hermes_settings().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to reset hermes settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to reset hermes settings" })),
            )
                .into_response()
        }
    }
}

async fn check_hermes_installed() -> bool {
    if command_exists("hermes").await {
        return true;
    }

    fs::metadata(hermes_config_path()).await.is_ok()
}

async fn command_exists(program: &str) -> bool {
    let finder = if cfg!(windows) { "where" } else { "which" };
    Command::new(finder)
        .arg(program)
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

async fn read_config_yaml() -> AnyhowResult<String> {
    read_file_or_empty(&hermes_config_path()).await
}

async fn read_env_file() -> AnyhowResult<String> {
    read_file_or_empty(&hermes_env_path()).await
}

async fn read_file_or_empty(path: &PathBuf) -> AnyhowResult<String> {
    match fs::read_to_string(path).await {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

async fn write_hermes_settings(
    base_url: &str,
    api_key: Option<&str>,
    model: &str,
) -> AnyhowResult<String> {
    fs::create_dir_all(hermes_dir()).await?;

    let normalized_base_url = normalize_base_url(base_url);
    let existing_yaml = read_config_yaml().await?;
    let new_yaml = upsert_model_block(
        &existing_yaml,
        &build_model_block(model, &normalized_base_url),
    );
    fs::write(hermes_config_path(), new_yaml).await?;

    if let Some(api_key) = api_key.filter(|value| !value.is_empty()) {
        let existing_env = read_env_file().await?;
        let new_env = upsert_env_var(&existing_env, API_KEY_ENV, api_key);
        fs::write(hermes_env_path(), new_env).await?;
    }

    Ok(hermes_config_path().to_string_lossy().to_string())
}

async fn reset_hermes_settings() -> AnyhowResult<Value> {
    let config_path = hermes_config_path();
    let yaml = match fs::read_to_string(&config_path).await {
        Ok(yaml) => yaml,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "success": true,
                "message": "No config file to reset",
            }));
        }
        Err(error) => return Err(error.into()),
    };

    let updated = remove_model_block(&yaml);
    fs::write(&config_path, updated).await?;
    Ok(json!({
        "success": true,
        "message": format!("{PROVIDER_NAME} model block removed"),
    }))
}

fn parse_model_block(yaml: &str) -> Option<Value> {
    let captures = MODEL_BLOCK_RE.captures(yaml)?;
    let body = captures
        .get(1)
        .map(|value| value.as_str())
        .unwrap_or_default();
    let default = extract_model_field(body, "default");
    let provider = extract_model_field(body, "provider");
    let base_url = extract_model_field(body, "base_url");

    Some(json!({
        "default": default,
        "provider": provider,
        "base_url": base_url,
    }))
}

fn extract_model_field(body: &str, key: &str) -> Option<String> {
    let pattern = format!(
        r#"(?m)^[ \t]+{}:[ \t]*["']?([^"'\r\n]+)["']?"#,
        regex::escape(key)
    );
    Regex::new(&pattern)
        .ok()
        .and_then(|regex| regex.captures(body))
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().trim().to_string())
}

fn has_9router_config(model: Option<&Value>) -> bool {
    let Some(model) = model else {
        return false;
    };
    let provider = model.get("provider").and_then(Value::as_str);
    let base_url = model.get("base_url").and_then(Value::as_str);
    provider == Some("custom")
        && base_url.is_some_and(|value| {
            let lower = value.to_ascii_lowercase();
            lower.contains("localhost") || lower.contains("127.0.0.1") || lower.contains("0.0.0.0")
        })
}

fn build_model_block(model: &str, base_url: &str) -> String {
    format!("model:\n  default: \"{model}\"\n  provider: \"custom\"\n  base_url: \"{base_url}\"\n")
}

fn upsert_model_block(yaml: &str, new_block: &str) -> String {
    if MODEL_BLOCK_RE.is_match(yaml) {
        MODEL_BLOCK_RE
            .replace(yaml, |_captures: &Captures<'_>| new_block.to_string())
            .into_owned()
    } else if yaml.is_empty() {
        new_block.to_string()
    } else {
        format!("{new_block}\n{yaml}")
    }
}

fn remove_model_block(yaml: &str) -> String {
    MODEL_BLOCK_RE
        .replace(yaml, "")
        .trim_start_matches('\n')
        .to_string()
}

fn upsert_env_var(env_text: &str, key: &str, value: &str) -> String {
    let line = format!("{key}={value}");
    let pattern = format!(r"(?m)^{}=.*$", regex::escape(key));
    let regex = Regex::new(&pattern).unwrap();
    if regex.is_match(env_text) {
        regex.replace(env_text, line).into_owned()
    } else if env_text.is_empty() {
        format!("{line}\n")
    } else if env_text.ends_with('\n') {
        format!("{env_text}{line}\n")
    } else {
        format!("{env_text}\n{line}\n")
    }
}

fn normalize_base_url(base_url: &str) -> String {
    if base_url.ends_with("/v1") {
        base_url.to_string()
    } else {
        format!("{base_url}/v1")
    }
}

fn hermes_dir() -> PathBuf {
    home_dir().join(".hermes")
}

fn hermes_config_path() -> PathBuf {
    hermes_dir().join("config.yaml")
}

fn hermes_env_path() -> PathBuf {
    hermes_dir().join(".env")
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}
