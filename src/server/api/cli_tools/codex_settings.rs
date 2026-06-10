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

use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/cli-tools/codex-settings",
        get(get_codex_settings)
            .post(save_codex_settings)
            .delete(delete_codex_settings),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveCodexSettingsRequest {
    base_url: String,
    api_key: Option<String>,
    default_model: Option<String>,
}

async fn get_codex_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let installed = check_codex_installed().await;
    if !installed {
        return Json(json!({
            "installed": false,
            "settings": Value::Null,
            "message": "Codex CLI is not installed",
        }))
        .into_response();
    }

    match read_codex_config().await {
        Ok(settings) => {
            let has_openproxy = has_openproxy_config(&settings);
            Json(json!({
                "installed": true,
                "settings": settings,
                "hasOpenProxy": has_openproxy,
                "settingsPath": codex_config_path().to_string_lossy().to_string(),
            }))
            .into_response()
        }
        Err(error) => {
            tracing::warn!(?error, "failed to read codex settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to check codex settings" })),
            )
                .into_response()
        }
    }
}

async fn save_codex_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SaveCodexSettingsRequest>,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    if body.base_url.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "baseUrl is required" })),
        )
            .into_response();
    }

    match write_codex_settings(&body).await {
        Ok(config_path) => Json(json!({
            "success": true,
            "message": "Codex CLI settings applied successfully!",
            "settingsPath": config_path,
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to write codex settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to update codex settings" })),
            )
                .into_response()
        }
    }
}

async fn delete_codex_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_codex_settings().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to reset codex settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to reset codex settings" })),
            )
                .into_response()
        }
    }
}

async fn check_codex_installed() -> bool {
    command_exists("codex").await || fs::metadata(codex_config_path()).await.is_ok()
}

async fn command_exists(program: &str) -> bool {
    let finder = if cfg!(windows) { "where" } else { "which" };
    tokio::process::Command::new(finder)
        .arg(program)
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

async fn read_codex_config() -> AnyhowResult<Option<Value>> {
    read_json_optional(&codex_config_path()).await
}

fn has_openproxy_config(settings: &Option<Value>) -> bool {
    let Some(settings) = settings else {
        return false;
    };
    let base_url = settings
        .get("baseUrl")
        .and_then(Value::as_str)
        .unwrap_or("");
    base_url.contains("localhost")
        || base_url.contains("127.0.0.1")
        || base_url.contains("openproxy")
        || base_url.contains("0.0.0.0")
}

async fn write_codex_settings(body: &SaveCodexSettingsRequest) -> AnyhowResult<String> {
    let config_path = codex_config_path();
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    // Build config object with lowercase keys and NO /v1 suffix on baseUrl
    let mut config = Map::new();
    let normalized_base_url = body
        .base_url
        .strip_suffix("/v1")
        .map(str::to_string)
        .unwrap_or_else(|| body.base_url.clone());
    config.insert(
        "baseUrl".to_string(),
        Value::String(normalized_base_url.clone()),
    );
    config.insert(
        "apiKey".to_string(),
        Value::String(body.api_key.clone().unwrap_or_default()),
    );
    config.insert(
        "defaultModel".to_string(),
        Value::String(
            body.default_model
                .clone()
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| "cx/gpt-5.2-codex".to_string()),
        ),
    );

    let config_value = Value::Object(config);
    let json_bytes = serde_json::to_vec_pretty(&config_value)?;
    fs::write(&config_path, &json_bytes).await?;

    // chmod 600 on Unix
    set_permissions_600(&config_path).await;

    // Set OPENAI_BASE_URL env in ~/.bashrc and ~/.zshrc (idempotent)
    upsert_env_var_in_profiles(&normalized_base_url).await?;

    Ok(config_path.to_string_lossy().to_string())
}

async fn reset_codex_settings() -> AnyhowResult<Value> {
    let config_path = codex_config_path();

    // Remove the config file
    match fs::remove_file(&config_path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // File already gone; still clean up env vars
        }
        Err(error) => {
            tracing::warn!(?error, "failed to remove codex config");
            return Err(error.into());
        }
    }

    // Remove env vars from profiles
    remove_env_var_from_profiles().await?;

    Ok(json!({
        "success": true,
        "message": "Codex CLI settings removed successfully",
    }))
}

/// Set file permissions to 600 (owner read/write only) on Unix.
/// No-op on Windows.
async fn set_permissions_600(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = match fs::metadata(path).await {
            Ok(m) => m,
            Err(_) => return,
        };
        let mut perms = metadata.permissions();
        perms.set_mode(0o600);
        let _ = fs::set_permissions(path, perms).await;
    }
    // Windows: no-op
    let _ = path;
}

/// Marker line used to identify the OpenProxy Codex block in shell profiles.
const BASHRC_ENV_LINE: &str = "# OpenProxy Codex settings";

async fn upsert_env_var_in_profiles(base_url: &str) -> AnyhowResult<()> {
    let export_line = format!("export OPENAI_BASE_URL=\"{}\"", base_url);
    let profiles = shell_profiles();
    for profile_path in &profiles {
        upsert_env_block(profile_path, &export_line).await?;
    }
    Ok(())
}

async fn remove_env_var_from_profiles() -> AnyhowResult<()> {
    let profiles = shell_profiles();
    for profile_path in &profiles {
        remove_env_block(profile_path).await?;
    }
    Ok(())
}

fn shell_profiles() -> Vec<PathBuf> {
    let home = home_dir();
    let mut profiles = Vec::new();
    profiles.push(home.join(".bashrc"));
    profiles.push(home.join(".zshrc"));
    profiles.retain(|p| p.exists());
    profiles
}

/// Insert or update the environment variable block in a shell profile.
/// The block is delimited by marker comments so we can remove it later.
async fn upsert_env_block(profile_path: &Path, export_line: &str) -> AnyhowResult<()> {
    let content = match fs::read_to_string(profile_path).await {
        Ok(c) => c,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };

    // Check if our block already exists
    if content.contains(BASHRC_ENV_LINE) {
        // Replace existing block
        let re = regex_block_pattern();
        if re.is_match(&content) {
            let new_block = format!(
                "{}\n{export_line}\n# End OpenProxy Codex settings",
                BASHRC_ENV_LINE
            );
            let updated = re.replace(&content, &new_block);
            fs::write(profile_path, updated.as_bytes()).await?;
        }
        return Ok(());
    }

    // Append at the end
    let block = format!(
        "\n{}\n{export_line}\n# End OpenProxy Codex settings\n",
        BASHRC_ENV_LINE
    );
    let updated = format!("{content}{block}");
    fs::write(profile_path, updated.as_bytes()).await?;
    Ok(())
}

async fn remove_env_block(profile_path: &Path) -> AnyhowResult<()> {
    let content = match fs::read_to_string(profile_path).await {
        Ok(c) => c,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };

    let re = regex_block_pattern();
    if re.is_match(&content) {
        let updated = re.replace(&content, "");
        let updated = updated
            .trim_start_matches('\n')
            .trim_end_matches('\n')
            .to_string();
        let updated = if updated.is_empty() {
            String::new()
        } else {
            format!("{updated}\n")
        };
        fs::write(profile_path, updated.as_bytes()).await?;
    }
    Ok(())
}

fn regex_block_pattern() -> regex::Regex {
    regex::Regex::new(&format!(
        r"(?m)^{}\n(?:export OPENAI_BASE_URL=.*\n)?# End OpenProxy Codex settings\n?",
        regex::escape(BASHRC_ENV_LINE)
    ))
    .unwrap()
}

async fn read_json_optional(path: &Path) -> AnyhowResult<Option<Value>> {
    let content = match fs::read_to_string(path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(serde_json::from_str(&content)?))
}

fn codex_config_path() -> PathBuf {
    home_dir().join(".codex").join("config.json")
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_has_openproxy_config_none() {
        assert!(!has_openproxy_config(&None));
    }

    #[test]
    fn test_has_openproxy_config_localhost() {
        let settings = json!({
            "baseUrl": "http://localhost:4623",
        });
        assert!(has_openproxy_config(&Some(settings)));
    }

    #[test]
    fn test_has_openproxy_config_openproxy() {
        let settings = json!({
            "baseUrl": "http://openproxy.example.com",
        });
        assert!(has_openproxy_config(&Some(settings)));
    }

    #[test]
    fn test_has_openproxy_config_other() {
        let settings = json!({
            "baseUrl": "https://api.openai.com",
        });
        assert!(!has_openproxy_config(&Some(settings)));
    }

    #[test]
    fn test_regex_block_pattern_matches() {
        let re = regex_block_pattern();
        let content = "# OpenProxy Codex settings\nexport OPENAI_BASE_URL=\"http://localhost:4623\"\n# End OpenProxy Codex settings\n";
        assert!(re.is_match(content));
    }

    #[test]
    fn test_regex_block_pattern_no_match() {
        let re = regex_block_pattern();
        let content = "export OPENAI_BASE_URL=\"http://other.com\"\n";
        assert!(!re.is_match(content));
    }
}
