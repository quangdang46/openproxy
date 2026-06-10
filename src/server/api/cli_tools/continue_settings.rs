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

/// OpenProxy apiBase value — used to identify our own entries.
const OPENPROXY_API_BASE: &str = "http://localhost:4623/v1";

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/cli-tools/continue-settings",
        get(get_continue_settings)
            .post(save_continue_settings)
            .delete(delete_continue_settings),
    )
}

// ---------------------------------------------------------------------------
// GET
// ---------------------------------------------------------------------------

async fn get_continue_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let settings_path = continue_config_path();

    if !fs::metadata(&settings_path).await.is_ok() {
        return Json(json!({
            "installed": false,
            "settings": Value::Null,
            "message": "Continue config file not found",
        }))
        .into_response();
    }

    match read_json_optional(&settings_path).await {
        Ok(Some(settings)) => {
            let has_openproxy = has_openproxy_config(&settings);
            Json(json!({
                "installed": true,
                "settings": settings,
                "hasOpenProxy": has_openproxy,
                "settingsPath": settings_path.to_string_lossy().to_string(),
            }))
            .into_response()
        }
        Ok(None) => Json(json!({
            "installed": false,
            "settings": Value::Null,
            "message": "Continue config file is empty or invalid",
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to read continue settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to read continue settings" })),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// POST
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveContinueSettingsRequest {
    model: String,
    api_key: String,
}

async fn save_continue_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SaveContinueSettingsRequest>,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    if body.model.trim().is_empty() || body.api_key.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "model and apiKey are required" })),
        )
            .into_response();
    }

    match write_continue_settings(&body).await {
        Ok(settings_path) => Json(json!({
            "success": true,
            "message": "Continue settings updated successfully",
            "settingsPath": settings_path.to_string_lossy().to_string(),
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to write continue settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to update continue settings" })),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// DELETE
// ---------------------------------------------------------------------------

async fn delete_continue_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_continue_settings().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to reset continue settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to reset continue settings" })),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check whether the config has at least one OpenProxy model entry.
fn has_openproxy_config(config: &Value) -> bool {
    config
        .get("models")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .any(|m| m.get("apiBase").and_then(Value::as_str) == Some(OPENPROXY_API_BASE))
        })
        .unwrap_or(false)
}

/// Write (merge) an OpenProxy model entry into the Continue config.
///
/// - Loads the existing config (if any).
/// - Removes any entry that already has `apiBase` set to ours.
/// - Appends the new entry.
/// - Writes atomically via temp file + rename, with mode 0600.
async fn write_continue_settings(body: &SaveContinueSettingsRequest) -> AnyhowResult<PathBuf> {
    let config_path = continue_config_path();
    let parent = config_path.parent().map(Path::to_path_buf).unwrap_or_default();
    fs::create_dir_all(&parent).await?;

    // Read existing config
    let mut config: Map<String, Value> = read_json_optional(&config_path)
        .await?
        .and_then(|v| match v {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default();

    // Build the new model entry
    let new_entry = json!({
        "title": body.model,
        "provider": "openai",
        "model": body.model,
        "apiKey": body.api_key,
        "apiBase": OPENPROXY_API_BASE,
    });

    // Get or create the models array
    let mut models: Vec<Value> = config
        .remove("models")
        .and_then(|v| match v {
            Value::Array(arr) => Some(arr),
            _ => None,
        })
        .unwrap_or_default();

    // Remove any existing entry with the same apiBase
    models.retain(|m| m.get("apiBase").and_then(Value::as_str) != Some(OPENPROXY_API_BASE));

    // Append the new entry
    models.push(new_entry);
    config.insert("models".to_string(), Value::Array(models));

    // Atomic write: temp file + rename
    let value = Value::Object(config);
    atomic_write_json(&config_path, &value).await?;

    Ok(config_path)
}

/// Remove all OpenProxy model entries from the Continue config, preserving all others.
async fn reset_continue_settings() -> AnyhowResult<Value> {
    let config_path = continue_config_path();

    let mut config: Map<String, Value> = match read_json_optional(&config_path).await? {
        Some(Value::Object(map)) => map,
        _ => {
            return Ok(json!({
                "success": true,
                "message": "No settings file to reset",
            }));
        }
    };

    let mut changed = false;

    if let Some(Value::Array(models)) = config.get_mut("models") {
        let before = models.len();
        models.retain(|m| m.get("apiBase").and_then(Value::as_str) != Some(OPENPROXY_API_BASE));
        if models.len() != before {
            changed = true;
        }
    }

    if !changed {
        return Ok(json!({
            "success": true,
            "message": "No OpenProxy entries found in Continue settings",
        }));
    }

    let value = Value::Object(config);
    atomic_write_json(&config_path, &value).await?;

    Ok(json!({
        "success": true,
        "message": "OpenProxy entries removed from Continue settings",
    }))
}

// ---------------------------------------------------------------------------
// File I/O
// ---------------------------------------------------------------------------

/// Read a JSON file, returning `None` if the file does not exist.
async fn read_json_optional(path: &Path) -> AnyhowResult<Option<Value>> {
    let content = match fs::read_to_string(path).await {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(Some(serde_json::from_str(&content)?))
}

/// Write a JSON value to disk atomically using a temp file + rename, with
/// mode 0600 (owner read/write only, no group/other).
async fn atomic_write_json(path: &Path, value: &Value) -> AnyhowResult<()> {
    // Serialise first so we can propagate format errors before I/O.
    let bytes = serde_json::to_vec_pretty(value)?;

    let parent = path.parent().unwrap_or(Path::new("."));
    let tmp_path = {
        let mut p = parent.to_path_buf();
        p.push(format!(
            ".continue_config_{}.tmp",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        p
    };

    // Write to temp file.
    fs::write(&tmp_path, &bytes).await?;

    // chmod 600 on Unix — tokio doesn't expose set_permissions directly on
    // write, so we call it explicitly.
    set_permissions_600(&tmp_path).await?;

    // Atomic rename (overwrites target on Unix, may fail on Windows if
    // target exists — we remove first to be safe).
    let _ = fs::remove_file(path).await;
    fs::rename(&tmp_path, path).await?;

    Ok(())
}

/// Set file permissions to 0o600 (owner rw-------).  On Windows this is a
/// no-op because the temp-file + rename pattern already inherits the correct
/// ACL from the parent directory.
async fn set_permissions_600(path: &Path) -> AnyhowResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path).await?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(path, perms).await?;
    }

    // On Windows the file will inherit parent-directory ACL; we fall through.
    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Path resolution
// ---------------------------------------------------------------------------

/// Returns the path to the Continue config file:
///   Linux/macOS:  $HOME/.continue/config.json
///   Windows:      %USERPROFILE%\.continue\config.json
fn continue_config_path() -> PathBuf {
    home_dir().join(".continue").join("config.json")
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}
