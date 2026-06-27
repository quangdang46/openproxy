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
use toml::{map::Map as TomlMap, Value as TomlValue};

use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/cli-tools/jcode-settings",
        get(get_jcode_settings)
            .post(save_jcode_settings)
            .delete(delete_jcode_settings),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveJcodeSettingsRequest {
    base_url: String,
    api_key: String,
    #[serde(default)]
    models: Vec<String>,
}

async fn get_jcode_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let installed = check_installed().await;
    if !installed {
        return Json(json!({
            "installed": false,
            "message": "jcode not installed. Install via: curl -fsSL https://raw.githubusercontent.com/1jehuang/jcode/master/scripts/install.sh | bash",
        }))
        .into_response();
    }

    let config = read_config()
        .await
        .unwrap_or_else(|_| Value::Object(Default::default()));
    let has_openproxy = has_openproxy_config(&config);
    let env_api_key = read_provider_env()
        .await
        .and_then(|env| env.get("JCODE_OPENPROXY_API_KEY").cloned())
        .filter(|s| !s.is_empty());

    Json(json!({
        "installed": true,
        "config": config,
        "hasOpenProxy": has_openproxy,
        "configPath": config_path().to_string_lossy().to_string(),
        "envApiKey": env_api_key,
    }))
    .into_response()
}

async fn save_jcode_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SaveJcodeSettingsRequest>,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    if body.base_url.trim().is_empty() || body.api_key.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "baseUrl and apiKey are required" })),
        )
            .into_response();
    }

    match write_jcode_config(&body).await {
        Ok(()) => Json(json!({
            "success": true,
            "message": "jcode configured successfully. Use: jcode --provider-profile openproxy",
            "configPath": config_path().to_string_lossy().to_string(),
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to write jcode settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to configure jcode: {error}") })),
            )
                .into_response()
        }
    }
}

async fn delete_jcode_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_jcode_config().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to reset jcode settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to reset jcode settings" })),
            )
                .into_response()
        }
    }
}

async fn check_installed() -> bool {
    if command_exists("jcode", true).await {
        return true;
    }
    // Check if the config directory exists as a fallback
    if let Some(parent) = config_path().parent() {
        fs::metadata(parent).await.is_ok()
    } else {
        false
    }
}

/// Read the jcode config.toml and convert to serde_json::Value for the response.
/// Returns the parsed contents (or empty object on error/missing file).
async fn read_config() -> AnyhowResult<Value> {
    let path = config_path();
    let content = match fs::read_to_string(&path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Value::Object(Default::default()));
        }
        Err(error) => return Err(error.into()),
    };

    // Parse as toml::Value, then serialize to serde_json::Value
    let toml_value: TomlValue = toml::from_str(&content)?;
    let json_value = serde_json::to_value(&toml_value)?;
    Ok(json_value)
}

/// Detect whether the config has an openproxy-compatible provider entry.
fn has_openproxy_config(config: &Value) -> bool {
    let Some(providers) = config.get("providers").and_then(|v| v.as_object()) else {
        return false;
    };

    if providers.contains_key("openproxy") {
        return true;
    }

    // Also detect if any provider's base_url points to a local server
    for provider in providers.values() {
        if let Some(base_url) = provider.get("base_url").and_then(Value::as_str) {
            if base_url.contains("localhost")
                || base_url.contains("127.0.0.1")
                || base_url.contains("0.0.0.0")
                || base_url.contains("openproxy")
            {
                return true;
            }
        }
    }

    false
}

/// Read the provider env file and return key-value pairs.
async fn read_provider_env() -> Option<std::collections::HashMap<String, String>> {
    let env_path = provider_env_path();
    let content = fs::read_to_string(&env_path).await.ok()?;

    let mut env = std::collections::HashMap::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some(eq_pos) = trimmed.find('=') {
            let key = trimmed[..eq_pos].trim().to_string();
            let mut value = trimmed[eq_pos + 1..].trim().to_string();

            // Strip surrounding quotes
            if value.len() >= 2 {
                let first = value.chars().next().unwrap();
                let last = value.chars().last().unwrap();
                if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
                    value = value[1..value.len() - 1].to_string();
                }
            }

            env.insert(key, value);
        }
    }

    Some(env)
}

/// Write the provider env file with the API key.
async fn write_provider_env(api_key: &str) -> AnyhowResult<()> {
    let env_path = provider_env_path();
    if let Some(parent) = env_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let content = format!(
        "# jcode provider environment variables\n\
         JCODE_OPENPROXY_API_KEY=\"{api_key}\"\n"
    );
    fs::write(&env_path, content).await?;
    Ok(())
}

/// Remove the openproxy API key from the provider env file.
async fn clear_provider_env() -> AnyhowResult<()> {
    let env_path = provider_env_path();
    let mut env = match fs::read_to_string(&env_path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };

    // Re-read and write back without the JCODE_OPENPROXY_API_KEY line
    let mut parsed = std::collections::HashMap::new();
    for line in env.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some(eq_pos) = trimmed.find('=') {
            let key = trimmed[..eq_pos].trim().to_string();
            let mut value = trimmed[eq_pos + 1..].trim().to_string();
            if value.len() >= 2 {
                let first = value.chars().next().unwrap();
                let last = value.chars().last().unwrap();
                if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
                    value = value[1..value.len() - 1].to_string();
                }
            }
            parsed.insert(key, value);
        }
    }

    parsed.remove("JCODE_OPENPROXY_API_KEY");

    let mut output = "# jcode provider environment variables\n".to_string();
    for (key, value) in &parsed {
        output.push_str(&format!("{key}=\"{value}\"\n"));
    }

    fs::write(&env_path, output).await?;
    Ok(())
}

/// Write the jcode config.toml with an openproxy provider entry.
async fn write_jcode_config(body: &SaveJcodeSettingsRequest) -> AnyhowResult<()> {
    let config_path = config_path();
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    // Read existing config
    let content = match fs::read_to_string(&config_path).await {
        Ok(content) => content,
        Err(_) => String::new(),
    };
    let mut root: TomlValue = toml::from_str(&content).unwrap_or(TomlValue::Table(TomlMap::new()));

    // Ensure we have a Table
    if !root.is_table() {
        root = TomlValue::Table(TomlMap::new());
    }

    let normalized_base_url = if body.base_url.ends_with("/v1") {
        body.base_url.clone()
    } else {
        format!("{}/v1", body.base_url)
    };
    let default_model = if body.models.is_empty() || body.models[0].is_empty() {
        "cc/claude-opus-4-7".to_string()
    } else {
        body.models[0].clone()
    };

    // Build the provider entry
    let provider_entry = TomlValue::Table(TomlMap::from_iter([
        (
            "type".to_string(),
            TomlValue::String("openai-compatible".to_string()),
        ),
        (
            "base_url".to_string(),
            TomlValue::String(normalized_base_url),
        ),
        ("auth".to_string(), TomlValue::String("bearer".to_string())),
        (
            "api_key_env".to_string(),
            TomlValue::String("JCODE_OPENPROXY_API_KEY".to_string()),
        ),
        (
            "env_file".to_string(),
            TomlValue::String("provider-openproxy.env".to_string()),
        ),
        (
            "default_model".to_string(),
            TomlValue::String(default_model),
        ),
        ("requires_api_key".to_string(), TomlValue::Boolean(true)),
    ]));

    // Navigate to root.providers
    if let TomlValue::Table(ref mut table) = root {
        let providers = table
            .entry("providers".to_string())
            .or_insert_with(|| TomlValue::Table(TomlMap::new()));
        if !providers.is_table() {
            *providers = TomlValue::Table(TomlMap::new());
        }
        if let TomlValue::Table(ref mut providers_table) = providers {
            providers_table.insert("openproxy".to_string(), provider_entry);
        }
    }

    // Write config
    let output = toml::to_string_pretty(&root)?;
    fs::write(&config_path, output).await?;

    // Write env file
    write_provider_env(&body.api_key).await?;

    Ok(())
}

/// Remove the openproxy provider entry from jcode config.
async fn reset_jcode_config() -> AnyhowResult<Value> {
    let config_path = config_path();
    let content = match fs::read_to_string(&config_path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "success": true,
                "message": "No configuration to remove",
            }));
        }
        Err(error) => return Err(error.into()),
    };

    let mut root: TomlValue = toml::from_str(&content)?;
    if let TomlValue::Table(ref mut table) = root {
        if let Some(TomlValue::Table(providers)) = table.get_mut("providers") {
            providers.remove("openproxy");
            if providers.is_empty() {
                table.remove("providers");
            }
        }
    }

    let output = toml::to_string_pretty(&root)?;
    fs::write(&config_path, output).await?;

    // Clear env file
    clear_provider_env().await?;

    Ok(json!({
        "success": true,
        "message": "OpenProxy configuration removed from jcode",
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
    home_dir().join(".jcode").join("config.toml")
}

fn provider_env_path() -> PathBuf {
    let config_dir = env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".config"));
    config_dir.join("jcode").join("provider-openproxy.env")
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}
