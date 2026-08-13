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
/// 9router GROK_SUBAGENT_TYPES.
const GROK_SUBAGENT_TYPES: [&str; 3] = ["general-purpose", "explore", "plan"];
/// 9router SUBAGENT_MODELS_SECTION.
const SUBAGENT_MODELS_SECTION: &str = "subagents.models";
/// Subagent model-slot prefix: `${MODEL_SLOT}-${type}`.
const SUBAGENT_SLOT_PREFIX: &str = "openproxy-";
/// 9router UNSET_SENTINEL — a subagent with no previous value.
const UNSET_SENTINEL: &str = "__9router_unset__";

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
    #[serde(default)]
    context_window: Option<u64>,
    #[serde(default)]
    subagent_models: Option<serde_json::Map<String, Value>>,
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
            let subagents = parse_subagent_mappings(&toml);
            let has_openproxy = has_openproxy_config(model.as_ref());
            Json(json!({
                "installed": true,
                "settings": {
                    "model": model,
                    "default": default_model,
                    "subagents": subagents,
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
    // [model.openproxy] ... until next [section] header or EOF.
    // No look-around (rust regex crate): match the header, then non-header
    // lines are collected by section_body() instead.
    Regex::new(&format!(r"(?m)^\[model\.{MODEL_SLOT}\][ \t]*\r?\n"))
        .expect("valid model section regex")
}

fn models_section_re() -> Regex {
    Regex::new(r"(?m)^\[models\][ \t]*\r?\n").expect("valid models section regex")
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

/// Body of a TOML section: everything from just after its header line until
/// the next `[section]` header or EOF.
fn section_body(toml: &str, header_end: usize) -> &str {
    let rest = &toml[header_end..];
    match rest.find("\n[") {
        Some(idx) => &rest[..idx],
        None => rest,
    }
}

/// Parse a `context_window` integer field (positive finite number).
fn get_toml_number(body: &str, key: &str) -> Option<u64> {
    let re = Regex::new(&format!(
        r#"(?m)^[ \t]*{key}[ \t]*=[ \t]*([0-9]+(?:\.[0-9]+)?)"#
    ))
    .expect("valid number regex");
    re.captures(body)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .map(|v| v as u64)
}

fn parse_model_section(toml: &str) -> Option<Value> {
    let re = model_section_re();
    let m = re.find(toml)?;
    let body = section_body(toml, m.end());
    Some(json!({
        "model": get_toml_field(body, "model"),
        "base_url": get_toml_field(body, "base_url"),
        "name": get_toml_field(body, "name"),
        "api_key": get_toml_field(body, "api_key"),
        "api_backend": get_toml_field(body, "api_backend"),
        "context_window": get_toml_number(body, "context_window"),
    }))
}

/// Parse a model section by an explicit slot name (9router parseModelSection).
fn parse_model_section_for_slot(toml: &str, slot: &str) -> Option<Value> {
    let re = Regex::new(&format!(r"(?m)^\[model\.{slot}\][ \t]*\r?\n")).expect("valid slot regex");
    let m = re.find(toml)?;
    let body = section_body(toml, m.end());
    Some(json!({
        "model": get_toml_field(body, "model"),
        "base_url": get_toml_field(body, "base_url"),
        "name": get_toml_field(body, "name"),
        "api_key": get_toml_field(body, "api_key"),
        "api_backend": get_toml_field(body, "api_backend"),
        "context_window": get_toml_number(body, "context_window"),
    }))
}

/// Read the `[subagents.models]` mapping for all subagent types.
/// 9router parseGrokBuildConfig subagentMappings/subagentModels.
fn parse_subagent_mappings(toml: &str) -> Value {
    let re = Regex::new(&format!(r"(?m)^\[{SUBAGENT_MODELS_SECTION}\][ \t]*\r?\n"))
        .expect("valid subagents section regex");
    let m = match re.find(toml) {
        Some(m) => m,
        None => {
            // No section: all types unset.
            let mut all = serde_json::Map::new();
            for t in GROK_SUBAGENT_TYPES {
                all.insert(t.to_string(), Value::Null);
            }
            return Value::Object(all);
        }
    };
    let body = section_body(toml, m.end());
    let mut mappings = serde_json::Map::new();
    for t in GROK_SUBAGENT_TYPES {
        let mapping = get_toml_field(body, t);
        let slot = format!("{SUBAGENT_SLOT_PREFIX}{t}");
        let model = if mapping.as_deref() == Some(&slot) {
            parse_model_section_for_slot(toml, &slot)
        } else {
            None
        };
        mappings.insert(
            t.to_string(),
            json!({
                "mapping": mapping,
                "model": model,
            }),
        );
    }
    Value::Object(mappings)
}

fn parse_models_default(toml: &str) -> Option<String> {
    let re = models_section_re();
    let m = re.find(toml)?;
    get_toml_field(section_body(toml, m.end()), "default")
}

fn build_model_section(model: &str, base_url: &str, api_key: &str) -> String {
    build_model_section_for_slot(MODEL_SLOT, model, base_url, api_key, "OpenProxy", None)
}

/// Build a `[model.{slot}]` section, optionally with a `context_window` field
/// and a custom display name (9router buildModelSection).
fn build_model_section_for_slot(
    slot: &str,
    model: &str,
    base_url: &str,
    api_key: &str,
    name: &str,
    context_window: Option<u64>,
) -> String {
    let mut section = format!(
        "[model.{slot}]\n\
         model = \"{model}\"\n\
         base_url = \"{base_url}\"\n\
         name = \"{name}\"\n\
         description = \"Routed via OpenProxy gateway\"\n\
         api_backend = \"chat_completions\"\n"
    );
    if !api_key.is_empty() {
        section.push_str(&format!("api_key = \"{api_key}\"\n"));
    }
    if let Some(cw) = context_window.filter(|cw| *cw > 0) {
        section.push_str(&format!("context_window = {cw}\n"));
    }
    section
}

fn upsert_model_section(toml: &str, section: &str) -> String {
    let re = model_section_re();
    if let Some(m) = re.find(toml) {
        let end = m.end();
        let body = section_body(toml, end);
        let replaced = format!("{section}{body}");
        return format!("{}{replaced}", &toml[..m.start()]);
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
    let mut next = toml.to_string();
    while let Some(m) = re.find(&next) {
        let end = section_end(&next, m.end());
        next = format!("{}{}", &next[..m.start()], &next[end..]);
    }
    Regex::new(r"\n{3,}")
        .expect("newline collapse")
        .replace_all(&next, "\n\n")
        .into_owned()
}

/// Index just past the end of the section body starting at `header_end`.
fn section_end(toml: &str, header_end: usize) -> usize {
    let rest = &toml[header_end..];
    match rest.find("\n[") {
        Some(idx) => header_end + idx,
        None => toml.len(),
    }
}

/// Upsert a `[model.{slot}]` section with the given body.
fn upsert_model_section_for_slot(toml: &str, slot: &str, section: &str) -> String {
    let re = Regex::new(&format!(r"(?m)^\[model\.{slot}\][ \t]*\r?\n")).expect("valid slot regex");
    if let Some(m) = re.find(toml) {
        let end = m.end();
        let body = section_body(toml, end);
        let replaced = format!("{section}{body}");
        return format!("{}{replaced}", &toml[..m.start()]);
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

/// Remove a `[model.{slot}]` section.
fn remove_model_section_for_slot(toml: &str, slot: &str) -> String {
    let re = Regex::new(&format!(r"(?m)^\[model\.{slot}\][ \t]*\r?\n")).expect("valid slot regex");
    let mut next = toml.to_string();
    while let Some(m) = re.find(&next) {
        let end = section_end(&next, m.end());
        next = format!("{}{}", &next[..m.start()], &next[end..]);
    }
    Regex::new(r"\n{3,}")
        .expect("newline collapse")
        .replace_all(&next, "\n\n")
        .into_owned()
}

/// Set a field in the `[subagents.models]` section (9router setSectionField).
fn set_subagent_field(toml: &str, key: &str, value: &str) -> String {
    let re = Regex::new(&format!(r"(?m)^\[{SUBAGENT_MODELS_SECTION}\][ \t]*\r?\n"))
        .expect("valid subagents section regex");
    if let Some(m) = re.find(toml) {
        let body = section_body(toml, m.end());
        let field_re = Regex::new(&format!(r#"(?m)^[ \t]*{key}[ \t]*=[ \t]*"[^"]*""#))
            .expect("valid subagent field regex");
        let line = format!("{key} = \"{value}\"");
        let new_body = if field_re.is_match(body) {
            field_re.replace(body, &line).into_owned()
        } else {
            format!("{line}\n{body}")
        };
        return format!(
            "{}[{SUBAGENT_MODELS_SECTION}]\n{new_body}",
            &toml[..m.start()]
        );
    }
    let block = format!("[{SUBAGENT_MODELS_SECTION}]\n{key} = \"{value}\"\n\n");
    if toml.is_empty() {
        block
    } else {
        format!("{block}{toml}")
    }
}

/// Delete a field from `[subagents.models]` (9router deleteSectionField).
fn delete_subagent_field(toml: &str, key: &str) -> String {
    let re = Regex::new(&format!(r"(?m)^\[{SUBAGENT_MODELS_SECTION}\][ \t]*\r?\n"))
        .expect("valid subagents section regex");
    let Some(m) = re.find(toml) else {
        return toml.to_string();
    };
    let body = section_body(toml, m.end());
    let field_re = Regex::new(&format!(r#"(?m)^[ \t]*{key}[ \t]*=[^\r\n]*\r?\n?"#))
        .expect("valid subagent field regex");
    let next_body = field_re.replace_all(body, "").into_owned();
    if next_body.trim().is_empty() {
        let end = section_end(toml, m.end());
        let next = format!("{}{}", &toml[..m.start()], &toml[end..]);
        Regex::new(r"\n{3,}")
            .expect("newline collapse")
            .replace_all(&next, "\n\n")
            .into_owned()
    } else {
        format!(
            "{}[{SUBAGENT_MODELS_SECTION}]\n{next_body}",
            &toml[..m.start()]
        )
    }
}

/// 9router rememberPreviousSubagent: record the current `[subagents.models]`
/// mapping for a type under a marker comment before we override it.
fn remember_prev_subagent(toml: &str, slot: &str) -> String {
    let marker = format!("# openproxy-prev-subagent-{slot} = ");
    if toml.contains(&format!("# openproxy-prev-subagent-{slot} =")) {
        return toml.to_string();
    }
    let current = get_subagent_mapping(toml, slot);
    let value = current.unwrap_or_else(|| UNSET_SENTINEL.to_string());
    let marker_line = format!("# openproxy-prev-subagent-{slot} = \"{value}\"\n");
    // Insert before the main [model.openproxy] section if present.
    let main_re = model_section_re();
    if let Some(m) = main_re.find(toml) {
        return format!("{}{marker_line}{}", &toml[..m.start()], &toml[m.start()..]);
    }
    let needs_nl = !toml.is_empty() && !toml.ends_with('\n');
    format!("{toml}{}{marker_line}", if needs_nl { "\n" } else { "" })
}

/// Read a type's mapping from `[subagents.models]`.
fn get_subagent_mapping(toml: &str, slot: &str) -> Option<String> {
    let re = Regex::new(&format!(r"(?m)^\[{SUBAGENT_MODELS_SECTION}\][ \t]*\r?\n"))
        .expect("valid subagents section regex");
    let m = re.find(toml)?;
    get_toml_field(section_body(toml, m.end()), slot)
}

/// 9router restorePreviousSubagent: restore a subagent's mapping from its
/// marker, or delete the field if it was unset before.
fn restore_prev_subagent(toml: &str, slot: &str) -> String {
    let marker_re = Regex::new(&format!(
        r#"(?m)^# openproxy-prev-subagent-{slot} = "([^"]*)"[ \t]*\r?\n?"#
    ))
    .expect("valid prev-subagent marker regex");
    let previous = marker_re
        .captures(toml)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()));
    let mut next = marker_re.replace_all(toml, "").into_owned();
    let current = get_subagent_mapping(&next, slot);
    if current.as_deref() != Some(&format!("{SUBAGENT_SLOT_PREFIX}{slot}")) {
        return next;
    }
    match previous {
        Some(p) if p != UNSET_SENTINEL => set_subagent_field(&next, slot, &p),
        _ => delete_subagent_field(&next, slot),
    }
}

fn set_models_default(toml: &str, value: &str) -> String {
    let re = models_section_re();
    if let Some(m) = re.find(toml) {
        let body = section_body(toml, m.end());
        let default_re =
            Regex::new(r#"(?m)^[ \t]*default[ \t]*=[ \t]*"[^"]*""#).expect("default field");
        let new_body = if default_re.is_match(body) {
            default_re
                .replace(body, format!(r#"default = "{value}""#))
                .into_owned()
        } else {
            format!("default = \"{value}\"\n{body}")
        };
        return format!("{}[models]\n{new_body}", &toml[..m.start()]);
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
        &build_model_section_for_slot(
            MODEL_SLOT,
            &body.model,
            &normalized_base_url,
            &api_key,
            "OpenProxy",
            body.context_window,
        ),
    );
    toml = set_models_default(&toml, MODEL_SLOT);

    // Subagent overrides (9router applyGrokBuildConfig subagentModels).
    if let Some(subagents) = &body.subagent_models {
        for ty in GROK_SUBAGENT_TYPES {
            let slot = format!("{SUBAGENT_SLOT_PREFIX}{ty}");
            let selected = subagents.get(ty);
            let model = selected
                .and_then(|v| v.get("model"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|m| !m.is_empty());
            if let Some(model) = model {
                let cw = selected
                    .and_then(|v| v.get("contextWindow"))
                    .and_then(Value::as_u64);
                toml = remember_prev_subagent(&toml, ty);
                toml = upsert_model_section_for_slot(
                    &toml,
                    &slot,
                    &build_model_section_for_slot(
                        &slot,
                        model,
                        &normalized_base_url,
                        &api_key,
                        &format!("OpenProxy {ty}"),
                        cw,
                    ),
                );
                toml = set_subagent_field(&toml, ty, &slot);
            } else {
                toml = restore_prev_subagent(&toml, ty);
                toml = remove_model_section_for_slot(&toml, &slot);
            }
        }
    }
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

    let mut next = toml;
    // Restore each subagent's previous mapping + remove its model slot.
    for ty in GROK_SUBAGENT_TYPES {
        let slot = format!("{SUBAGENT_SLOT_PREFIX}{ty}");
        next = restore_prev_subagent(&next, ty);
        next = remove_model_section_for_slot(&next, &slot);
    }
    next = remove_model_section(&next);
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

    #[test]
    fn subagent_mappings_parsed() {
        let toml = format!(
            "[models]\ndefault = \"grok-build\"\n\n\
             [model.openproxy]\nmodel = \"gcli/grok-build\"\nbase_url = \"http://127.0.0.1:4623/v1\"\n\n\
             [model.openproxy-general-purpose]\nmodel = \"gcli/grok-4\"\nbase_url = \"http://127.0.0.1:4623/v1\"\n\n\
             [subagents.models]\ngeneral-purpose = \"openproxy-general-purpose\"\nexplore = \"x\"\n"
        );
        let sub = parse_subagent_mappings(&toml);
        // general-purpose mapped to our slot → model parsed.
        let gp = &sub["general-purpose"];
        assert_eq!(gp["mapping"], "openproxy-general-purpose");
        assert_eq!(gp["model"]["model"], "gcli/grok-4");
        // explore mapped to something else → no model.
        assert_eq!(sub["explore"]["mapping"], "x");
        assert!(sub["explore"]["model"].is_null());
        // plan unset → null.
        assert!(sub["plan"]["mapping"].is_null());
    }

    #[test]
    fn subagent_upsert_and_restore_roundtrip() {
        let toml = "[models]\ndefault = \"grok-build\"\n";
        let slot = format!("{SUBAGENT_SLOT_PREFIX}general-purpose");
        // Remember previous (unset) + upsert a subagent model + set mapping.
        let mut next = remember_prev_subagent(toml, "general-purpose");
        next = upsert_model_section_for_slot(
            &next,
            &slot,
            &build_model_section_for_slot(
                &slot,
                "gcli/grok-4",
                "http://127.0.0.1:4623/v1",
                "sk",
                "OpenProxy general-purpose",
                Some(200000),
            ),
        );
        next = set_subagent_field(&next, "general-purpose", &slot);
        let sub = parse_subagent_mappings(&next);
        assert_eq!(
            sub["general-purpose"]["mapping"],
            "openproxy-general-purpose"
        );
        assert_eq!(sub["general-purpose"]["model"]["model"], "gcli/grok-4");
        assert_eq!(
            sub["general-purpose"]["model"]["context_window"],
            serde_json::json!(200000)
        );
        // Restore → field removed (was unset), slot section removed.
        let restored = restore_prev_subagent(&next, "general-purpose");
        let restored = remove_model_section_for_slot(&restored, &slot);
        let sub2 = parse_subagent_mappings(&restored);
        assert!(sub2["general-purpose"]["mapping"].is_null());
        assert!(!restored.contains("[model.openproxy-general-purpose]"));
    }

    #[test]
    fn context_window_parsed_from_section() {
        let toml = "[model.openproxy]\nmodel = \"m\"\nbase_url = \"http://x/v1\"\ncontext_window = 200000\n";
        let model = parse_model_section(toml).expect("section");
        assert_eq!(model["context_window"], serde_json::json!(200000));
        // Missing / invalid → null.
        let toml2 = "[model.openproxy]\nmodel = \"m\"\n";
        let model2 = parse_model_section(toml2).expect("section");
        assert!(model2["context_window"].is_null());
    }
}
