//! Grok Build CLI settings — reads/writes `~/.grok/config.toml`.
//!
//! Port of 9router `src/app/api/cli-tools/grok-build-settings/route.js`.
//! Writes a `[model.openproxy]` custom model slot and sets it as `[models].default`.

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
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{fs, process::Command};

use crate::server::state::AppState;

const MODEL_SLOT: &str = "openproxy";
const BUILTIN_DEFAULT: &str = "grok-build";

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/cli-tools/grok-build-settings",
        get(get_grok_build_settings)
            .post(save_grok_build_settings)
            .delete(delete_grok_build_settings),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveGrokBuildSettingsRequest {
    base_url: String,
    #[serde(default)]
    api_key: Option<String>,
    model: String,
}

pub(super) async fn get_grok_build_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let installed = check_installed().await;
    if !installed {
        return Json(json!({
            "installed": false,
            "settings": Value::Null,
            "message": "Grok Build is not installed",
        }))
        .into_response();
    }

    match read_config_toml().await {
        Ok(toml) => {
            let model = parse_model_section(&toml);
            let default_model = parse_models_default(&toml);
            let has_openproxy = has_openproxy_config(model.as_ref());
            Json(json!({
                "installed": true,
                "settings": {
                    "model": model,
                    "default": default_model,
                },
                "hasOpenProxy": has_openproxy,
                "configPath": config_path().to_string_lossy().to_string(),
            }))
            .into_response()
        }
        Err(error) => {
            tracing::warn!(?error, "failed to read grok-build settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to check grok-build settings" })),
            )
                .into_response()
        }
    }
}

async fn save_grok_build_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SaveGrokBuildSettingsRequest>,
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

    match write_grok_config(&body).await {
        Ok(()) => Json(json!({
            "success": true,
            "message": "Grok Build settings applied successfully!",
            "configPath": config_path().to_string_lossy().to_string(),
            "modelSlot": MODEL_SLOT,
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to write grok-build settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to update grok-build settings" })),
            )
                .into_response()
        }
    }
}

async fn delete_grok_build_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_grok_config().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to reset grok-build settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to reset grok-build settings" })),
            )
                .into_response()
        }
    }
}

async fn check_installed() -> bool {
    if command_exists("grok").await {
        return true;
    }
    // Official installer drops binary under ~/.grok/bin/grok
    if fs::metadata(grok_bin_path()).await.is_ok() {
        return true;
    }
    fs::metadata(config_path()).await.is_ok()
}

async fn read_config_toml() -> AnyhowResult<String> {
    let path = config_path();
    match fs::read_to_string(&path).await {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

fn model_section_re() -> Regex {
    // [model.openproxy] ... until next [section] header or EOF
    Regex::new(&format!(
        r"(?m)^\[model\.{MODEL_SLOT}\][ \t]*\r?\n(?:(?!\[)[^\r\n]*\r?\n?)*"
    ))
    .expect("valid model section regex")
}

fn models_section_re() -> Regex {
    Regex::new(r"(?m)^\[models\][ \t]*\r?\n((?:(?!\[)[^\r\n]*\r?\n?)*)")
        .expect("valid models section regex")
}

fn prev_default_re() -> Regex {
    Regex::new(r#"(?m)^# openproxy-prev-default = "([^"]*)"[ \t]*\r?\n?"#)
        .expect("valid prev-default regex")
}

fn get_toml_field(body: &str, key: &str) -> Option<String> {
    let re = Regex::new(&format!(r#"(?m)^[ \t]*{key}[ \t]*=[ \t]*"([^"]*)""#))
        .expect("valid field regex");
    re.captures(body)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

fn parse_model_section(toml: &str) -> Option<Value> {
    let re = model_section_re();
    let m = re.find(toml)?;
    let body = Regex::new(r"(?m)^\[model\.[^\]]+\][ \t]*\r?\n")
        .expect("header strip")
        .replace(m.as_str(), "");
    Some(json!({
        "model": get_toml_field(&body, "model"),
        "base_url": get_toml_field(&body, "base_url"),
        "name": get_toml_field(&body, "name"),
        "api_key": get_toml_field(&body, "api_key"),
        "api_backend": get_toml_field(&body, "api_backend"),
    }))
}

fn parse_models_default(toml: &str) -> Option<String> {
    let re = models_section_re();
    let caps = re.captures(toml)?;
    get_toml_field(caps.get(1).map(|m| m.as_str()).unwrap_or(""), "default")
}

fn build_model_section(model: &str, base_url: &str, api_key: &str) -> String {
    format!(
        "[model.{MODEL_SLOT}]\n\
         model = \"{model}\"\n\
         base_url = \"{base_url}\"\n\
         name = \"OpenProxy\"\n\
         description = \"Routed via OpenProxy gateway\"\n\
         api_backend = \"chat_completions\"\n\
         api_key = \"{api_key}\"\n"
    )
}

fn upsert_model_section(toml: &str, section: &str) -> String {
    let re = model_section_re();
    if re.is_match(toml) {
        return re.replace(toml, section).into_owned();
    }
    let needs_nl = !toml.is_empty() && !toml.ends_with('\n');
    format!(
        "{toml}{}{}",
        if needs_nl { "\n" } else { "" },
        if toml.is_empty() {
            section.to_string()
        } else {
            format!("\n{section}")
        }
    )
}

fn remove_model_section(toml: &str) -> String {
    let re = model_section_re();
    let next = re.replace_all(toml, "").into_owned();
    Regex::new(r"\n{3,}")
        .expect("newline collapse")
        .replace_all(&next, "\n\n")
        .into_owned()
}

fn set_models_default(toml: &str, value: &str) -> String {
    let re = models_section_re();
    if let Some(caps) = re.captures(toml) {
        let full = caps.get(0).map(|m| m.as_str()).unwrap_or("");
        let body = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let default_re =
            Regex::new(r#"(?m)^[ \t]*default[ \t]*=[ \t]*"[^"]*""#).expect("default field");
        let new_body = if default_re.is_match(body) {
            default_re
                .replace(body, format!(r#"default = "{value}""#))
                .into_owned()
        } else {
            format!("default = \"{value}\"\n{body}")
        };
        return toml.replacen(full, &format!("[models]\n{new_body}"), 1);
    }
    let block = format!("[models]\ndefault = \"{value}\"\n\n");
    if toml.is_empty() {
        block
    } else {
        format!("{block}{toml}")
    }
}

fn remember_prev_default(toml: &str) -> String {
    let prev_re = prev_default_re();
    if prev_re.is_match(toml) {
        return toml.to_string();
    }
    let current = parse_models_default(toml);
    match current {
        Some(ref c) if c != MODEL_SLOT => {
            let marker = format!("# openproxy-prev-default = \"{c}\"\n");
            let model_re = model_section_re();
            if model_re.is_match(toml) {
                return model_re
                    .replace(toml, |caps: &regex::Captures| {
                        format!("{marker}{}", &caps[0])
                    })
                    .into_owned();
            }
            let needs_nl = !toml.is_empty() && !toml.ends_with('\n');
            format!("{toml}{}{marker}", if needs_nl { "\n" } else { "" })
        }
        _ => toml.to_string(),
    }
}

fn clear_models_default_if_ours(toml: &str) -> String {
    let prev_re = prev_default_re();
    let restore_to = prev_re
        .captures(toml)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
        .unwrap_or_else(|| BUILTIN_DEFAULT.to_string());
    let mut next = prev_re.replace_all(toml, "").into_owned();
    if parse_models_default(&next).as_deref() == Some(MODEL_SLOT) {
        next = set_models_default(&next, &restore_to);
    }
    next
}

fn has_openproxy_config(model: Option<&Value>) -> bool {
    model
        .and_then(|m| m.get("base_url"))
        .and_then(Value::as_str)
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

async fn write_grok_config(body: &SaveGrokBuildSettingsRequest) -> AnyhowResult<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let normalized_base_url = if body.base_url.ends_with("/v1") {
        body.base_url.clone()
    } else {
        format!("{}/v1", body.base_url.trim_end_matches('/'))
    };
    let api_key = body
        .api_key
        .clone()
        .filter(|k| !k.is_empty())
        .unwrap_or_else(|| "sk_openproxy".to_string());

    let mut toml = read_config_toml().await?;
    toml = remember_prev_default(&toml);
    toml = upsert_model_section(
        &toml,
        &build_model_section(&body.model, &normalized_base_url, &api_key),
    );
    toml = set_models_default(&toml, MODEL_SLOT);
    fs::write(&path, toml).await?;
    Ok(())
}

async fn reset_grok_config() -> AnyhowResult<Value> {
    let path = config_path();
    let toml = match fs::read_to_string(&path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "success": true,
                "message": "No config file to reset",
            }));
        }
        Err(error) => return Err(error.into()),
    };

    let mut next = remove_model_section(&toml);
    next = clear_models_default_if_ours(&next);
    fs::write(&path, next).await?;
    Ok(json!({
        "success": true,
        "message": "openproxy model slot removed from Grok Build",
    }))
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

fn config_path() -> PathBuf {
    home_dir().join(".grok").join("config.toml")
}

fn grok_bin_path() -> PathBuf {
    home_dir().join(".grok").join("bin").join("grok")
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
    fn parse_and_upsert_model_section() {
        let toml = r#"[models]
default = "grok-build"

[model.other]
model = "x"
"#;
        let section = build_model_section(
            "gcli/grok-build",
            "http://127.0.0.1:4623/v1",
            "sk_openproxy",
        );
        let next = upsert_model_section(toml, &section);
        let model = parse_model_section(&next).expect("section present");
        assert_eq!(
            model.get("model").and_then(Value::as_str),
            Some("gcli/grok-build")
        );
        assert_eq!(
            model.get("base_url").and_then(Value::as_str),
            Some("http://127.0.0.1:4623/v1")
        );

        let with_default = set_models_default(&next, MODEL_SLOT);
        assert_eq!(
            parse_models_default(&with_default).as_deref(),
            Some(MODEL_SLOT)
        );

        let remembered = remember_prev_default(toml);
        assert!(remembered.contains("openproxy-prev-default"));
        let cleared = clear_models_default_if_ours(&set_models_default(&remembered, MODEL_SLOT));
        assert_eq!(
            parse_models_default(&cleared).as_deref(),
            Some("grok-build")
        );
        assert!(!prev_default_re().is_match(&cleared));
    }

    #[test]
    fn remove_model_section_keeps_other_content() {
        let toml = format!(
            "[models]\ndefault = \"{MODEL_SLOT}\"\n\n{}\n[other]\nx = \"1\"\n",
            build_model_section("m", "http://x/v1", "k")
        );
        let next = remove_model_section(&toml);
        assert!(parse_model_section(&next).is_none());
        assert!(next.contains("[other]"));
    }
}
