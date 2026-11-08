//! Pi (pi-coding-agent) settings — port of 9router
//! `src/app/api/cli-tools/pi-settings/route.js` (v0.5.111+).
//!
//! Config file: `~/.pi/agent/models.json` (preferred) or `~/.pi/models.json`.
//! The provider block lives at `providers["9router"]` as an OpenAI-compatible
//! endpoint with an explicit `models` array carrying per-model
//! `contextWindow`/`maxTokens` caps.

use std::collections::HashMap;
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
use serde_json::{json, Map, Value};
use tokio::fs;

use super::shared::{command_exists, home_dir, normalize_base_url, resolve_cli_api_key};
use crate::server::state::AppState;

const PROVIDER_ID: &str = "9router";
const PROVIDER_API: &str = "openai-completions";

// v0.5.111 caps: contextWindow <= 2M, maxTokens <= 200k.
const DEFAULT_CONTEXT_WINDOW: f64 = 200_000.0;
const DEFAULT_MAX_TOKENS: f64 = 64_000.0;
const MAX_CONTEXT_WINDOW: f64 = 2_000_000.0;
const MAX_MAX_TOKENS: f64 = 200_000.0;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/cli-tools/pi-settings",
        get(get_pi_settings)
            .post(save_pi_settings)
            .delete(delete_pi_settings),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavePiSettingsRequest {
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    models: Option<Vec<Value>>,
}

pub(super) async fn get_pi_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    if !check_pi_installed().await {
        return Json(json!({
            "installed": false,
            "config": Value::Null,
            "message": "Pi CLI is not installed",
        }))
        .into_response();
    }

    let config_path = resolve_models_json_path().await;
    match read_config(&config_path).await {
        Ok(config) => Json(json!({
            "installed": true,
            "config": config,
            "has9Router": has_openproxy_config(config.as_ref()),
            "configPath": config_path.to_string_lossy().to_string(),
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to read pi settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to check pi settings: {error}") })),
            )
                .into_response()
        }
    }
}

async fn save_pi_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SavePiSettingsRequest>,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let Some(base_url) = body
        .base_url
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "baseUrl is required" })),
        )
            .into_response();
    };

    match write_pi_settings(&state, &body, base_url).await {
        Ok(config_path) => Json(json!({
            "success": true,
            "message": "Pi settings applied! Use /model in Pi to select the 9Router model.",
            "configPath": config_path,
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to write pi settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to apply pi settings: {error}") })),
            )
                .into_response()
        }
    }
}

async fn delete_pi_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_pi_settings().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to reset pi settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to reset pi settings: {error}") })),
            )
                .into_response()
        }
    }
}

async fn check_pi_installed() -> bool {
    if command_exists("pi").await {
        return true;
    }
    for path in [pi_agent_models_path(), pi_root_models_path()] {
        if fs::metadata(path).await.is_ok() {
            return true;
        }
    }
    false
}

/// `~/.pi/agent/models.json` when present, else `~/.pi/models.json` when
/// present, else the agent path (the write target).
async fn resolve_models_json_path() -> PathBuf {
    let agent_path = pi_agent_models_path();
    if fs::metadata(&agent_path).await.is_ok() {
        return agent_path;
    }
    let root_path = pi_root_models_path();
    if fs::metadata(&root_path).await.is_ok() {
        return root_path;
    }
    agent_path
}

async fn read_config(path: &PathBuf) -> AnyhowResult<Option<Value>> {
    match fs::read_to_string(path).await {
        Ok(content) => Ok(serde_json::from_str(&content).ok()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn has_openproxy_config(config: Option<&Value>) -> bool {
    let Some(providers) = config
        .and_then(|c| c.get("providers"))
        .and_then(Value::as_object)
    else {
        return false;
    };
    if providers
        .get(PROVIDER_ID)
        .and_then(|p| p.get("baseUrl"))
        .and_then(Value::as_str)
        .is_some()
    {
        return true;
    }
    providers.values().any(|prov| {
        prov.get("baseUrl")
            .and_then(Value::as_str)
            .is_some_and(|url| url.contains("20128"))
    })
}

/// `cap` from upstream: finite number, > 0, <= max, else fallback.
fn sanitize_cap(value: Option<&Value>, fallback: f64, max: f64) -> f64 {
    value
        .and_then(|v| match v {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        })
        .filter(|n| n.is_finite() && *n > 0.0 && *n <= max)
        .unwrap_or(fallback)
}

fn model_entry_id(model: &Value) -> String {
    match model {
        Value::String(id) => id.clone(),
        Value::Object(_) => model
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "provider/model-id".to_string()),
        _ => "provider/model-id".to_string(),
    }
}

/// Merge one incoming model spec with the previously stored entry for the
/// same id. Port of upstream `withDefaults`:
/// - `name === id` is treated as "unset" (the card always sends name=id), so a
///   hand-edited display name in models.json is preserved.
/// - caps merge `incoming ?? previous` then sanitize with defaults.
fn merge_model_entry(prev: Option<&Value>, incoming: &Value) -> Value {
    let id = model_entry_id(incoming);
    let prev_obj = prev.and_then(Value::as_object);

    let mut merged = Map::new();
    if let Some(prev) = prev_obj {
        for (key, value) in prev {
            merged.insert(key.clone(), value.clone());
        }
    }
    if let Value::Object(incoming_obj) = incoming {
        for (key, value) in incoming_obj {
            merged.insert(key.clone(), value.clone());
        }
    }
    merged.insert("id".to_string(), Value::String(id.clone()));

    // name: incoming name wins unless it equals the id (unset sentinel).
    let incoming_name = incoming.get("name").and_then(Value::as_str);
    let name = match incoming_name {
        Some(name) if !name.is_empty() && name != id => name.to_string(),
        _ => prev_obj
            .and_then(|p| p.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| id.clone()),
    };
    merged.insert("name".to_string(), Value::String(name));

    let context_window = sanitize_cap(
        incoming
            .get("contextWindow")
            .or_else(|| prev_obj.and_then(|p| p.get("contextWindow"))),
        DEFAULT_CONTEXT_WINDOW,
        MAX_CONTEXT_WINDOW,
    );
    let max_tokens = sanitize_cap(
        incoming
            .get("maxTokens")
            .or_else(|| prev_obj.and_then(|p| p.get("maxTokens"))),
        DEFAULT_MAX_TOKENS,
        MAX_MAX_TOKENS,
    );
    // Emit as integers when whole — upstream writes plain numbers.
    merged.insert("contextWindow".to_string(), json!(context_window as u64));
    merged.insert("maxTokens".to_string(), json!(max_tokens as u64));

    Value::Object(merged)
}

async fn write_pi_settings(
    state: &AppState,
    body: &SavePiSettingsRequest,
    base_url: &str,
) -> AnyhowResult<String> {
    let config_path = resolve_models_json_path().await;
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let mut existing = read_config(&config_path)
        .await?
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    // Previous models keyed by id so Apply merges caps instead of resetting.
    let mut prev_models: HashMap<String, Value> = HashMap::new();
    if let Some(prev_provider) = existing
        .get("providers")
        .and_then(|p| p.get(PROVIDER_ID))
        .and_then(|p| p.get("models"))
        .and_then(Value::as_array)
    {
        for m in prev_provider {
            prev_models.insert(model_entry_id(m), m.clone());
        }
    }

    let normalized_base_url = normalize_base_url(base_url);

    let model_list: Vec<Value> = match body.models.as_ref().filter(|m| !m.is_empty()) {
        Some(models) => models
            .iter()
            .map(|m| merge_model_entry(prev_models.get(&model_entry_id(m)), m))
            .collect(),
        None => {
            let model_id = body
                .model
                .clone()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| "provider/model-id".to_string());
            vec![merge_model_entry(
                prev_models.get(&model_id),
                &json!({ "id": model_id }),
            )]
        }
    };

    // Resolve apiKey: body → existing provider block → first active db key.
    let existing_provider = existing
        .get("providers")
        .and_then(Value::as_object)
        .and_then(|p| p.get(PROVIDER_ID))
        .cloned();
    let api_key = body
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty() && *k != "sk_9router")
        .map(str::to_string)
        .or_else(|| {
            existing_provider
                .as_ref()
                .and_then(|p| p.get("apiKey"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| resolve_cli_api_key(state, None));

    let existing_api = existing_provider
        .as_ref()
        .and_then(|p| p.get("api"))
        .and_then(Value::as_str)
        .unwrap_or(PROVIDER_API)
        .to_string();

    let mut provider = existing_provider
        .as_ref()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    provider.insert("baseUrl".to_string(), Value::String(normalized_base_url));
    provider.insert("apiKey".to_string(), Value::String(api_key));
    provider.insert("api".to_string(), Value::String(existing_api));
    provider.insert("models".to_string(), Value::Array(model_list));

    let providers = existing
        .entry("providers".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !providers.is_object() {
        *providers = Value::Object(Map::new());
    }
    if let Value::Object(ref mut providers_map) = providers {
        providers_map.insert(PROVIDER_ID.to_string(), Value::Object(provider));
    }

    let output = serde_json::to_string_pretty(&Value::Object(existing))?;
    fs::write(&config_path, format!("{output}\n")).await?;
    Ok(config_path.to_string_lossy().to_string())
}

async fn reset_pi_settings() -> AnyhowResult<Value> {
    let config_path = resolve_models_json_path().await;
    let mut existing = match read_config(&config_path).await? {
        Some(config) => config,
        None => {
            return Ok(json!({
                "success": true,
                "message": "No config file to reset",
            }));
        }
    };

    let mut changed = false;
    if let Some(providers) = existing.get_mut("providers").and_then(Value::as_object_mut) {
        if providers.remove(PROVIDER_ID).is_some() {
            changed = true;
            if providers.is_empty() {
                if let Some(root) = existing.as_object_mut() {
                    root.remove("providers");
                }
            }
        }
    }

    if changed {
        let output = serde_json::to_string_pretty(&existing)?;
        fs::write(&config_path, format!("{output}\n")).await?;
    }

    Ok(json!({
        "success": true,
        "message": "9Router removed from Pi",
    }))
}

fn pi_dir() -> PathBuf {
    home_dir().join(".pi")
}

fn pi_agent_models_path() -> PathBuf {
    pi_dir().join("agent").join("models.json")
}

fn pi_root_models_path() -> PathBuf {
    pi_dir().join("models.json")
}
