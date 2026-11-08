//! Hermes Agent CLI-tool integration — per-profile configuration.
//! Port of 9router `src/app/api/cli-tools/hermes-settings/route.js` +
//! `hermesProfiles.js` + `hermesYaml.js` (helpers live in `hermes_profiles` /
//! `hermes_yaml`).
//!
//! A Hermes profile is a separate home directory (`~/.hermes` = "default",
//! `~/.hermes/profiles/<name>` = named). Every handler accepts `?profile=` on GET
//! (all-statuses calls GET with no argument at all, so every reader must tolerate a
//! missing request) and a `profile` field on the JSON body for POST/DELETE —
//! callers reach for either.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result as AnyhowResult;
use axum::{
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{fs, process::Command};

use super::hermes_profiles::{self, HermesHome};
use super::hermes_yaml;
use crate::server::state::AppState;

const PROVIDER_NAME: &str = "openproxy";
const API_KEY_ENV: &str = "OPENAI_API_KEY";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/cli-tools/hermes-settings",
            get(get_hermes_settings)
                .post(save_hermes_settings)
                .delete(delete_hermes_settings),
        )
        .route("/api/cli-tools/hermes-profiles", get(get_hermes_profiles))
}

// ── request shapes ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveHermesSettingsRequest {
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    /// [{role, model}] — "default" plus any auxiliary/delegation slots.
    /// Legacy callers (CLI quick setup) send a bare `model` → treated as default role.
    selections: Option<Vec<RoleSelection>>,
    audio: Option<AudioSelection>,
    /// `true`/`"true"`/`"1"` — refresh endpoint + API key across every profile.
    apply_to_all: Option<Value>,
    profile: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RoleSelection {
    role: Option<String>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AudioSelection {
    enabled: Option<Value>,
    model: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct DeleteHermesSettingsRequest {
    profile: Option<String>,
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true" || s == "1",
        _ => false,
    }
}

fn query_param<'a>(query: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    query.get(key).map(String::as_str).filter(|v| !v.is_empty())
}

// ── shared fs helpers ────────────────────────────────────────────────────────

async fn check_hermes_installed() -> bool {
    if command_exists("hermes").await {
        return true;
    }
    fs::metadata(
        hermes_profiles::resolve_hermes_home(None)
            .map(|h| h.config_path)
            .unwrap_or_default(),
    )
    .await
    .is_ok()
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

async fn read_config_yaml(home: &HermesHome) -> AnyhowResult<String> {
    read_file_or_empty(&home.config_path).await
}

async fn read_env_file(home: &HermesHome) -> AnyhowResult<String> {
    read_file_or_empty(&home.env_path).await
}

async fn read_file_or_empty(path: &PathBuf) -> AnyhowResult<String> {
    match fs::read_to_string(path).await {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

/// Named profile homes are created by `hermes profile create` — never by us: a directory
/// without identity files is not a profile, and we must not fabricate one.
async fn write_home_config(home: &HermesHome, yaml: &str) -> AnyhowResult<()> {
    if home.is_default {
        fs::create_dir_all(&home.dir).await?;
    }
    fs::write(&home.config_path, yaml).await?;
    Ok(())
}

/// Only ever upserts the OPENAI_API_KEY line — per-profile .env files also carry bot
/// tokens and messaging-channel credentials that must stay untouched.
async fn write_home_env(home: &HermesHome, api_key: Option<&str>) -> AnyhowResult<()> {
    let Some(api_key) = api_key.filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    if home.is_default {
        fs::create_dir_all(&home.dir).await?;
    }
    let env = hermes_yaml::upsert_env_var(&read_env_file(home).await?, API_KEY_ENV, api_key);
    fs::write(&home.env_path, env).await?;
    Ok(())
}

fn normalize_base_url(base_url: &str) -> String {
    if base_url.ends_with("/v1") {
        base_url.to_string()
    } else {
        format!("{base_url}/v1")
    }
}

// ── GET handlers ─────────────────────────────────────────────────────────────

pub(super) async fn get_hermes_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
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

    let home = match resolve_profile(query_param(&query, "profile")) {
        Ok(home) => home,
        Err(response) => return response,
    };
    if !home.is_default && !hermes_profiles::dir_exists(&home.dir).await {
        return profile_not_found(&home.name);
    }

    match read_config_yaml(&home).await {
        Ok(yaml) => {
            let model = hermes_yaml::parse_model_block(&yaml);
            let delegation = hermes_yaml::parse_delegation_block(&yaml);
            let auxiliary = hermes_yaml::parse_aux_roles(&yaml);
            let stt = hermes_yaml::parse_stt_block(&yaml);
            let has_openproxy = hermes_yaml::has_openproxy_config(model.as_ref())
                || hermes_yaml::has_openproxy_config(delegation.as_ref())
                || auxiliary
                    .values()
                    .any(|cfg| hermes_yaml::has_openproxy_config(Some(cfg)));
            Json(json!({
                "installed": true,
                "profile": home.to_json(),
                "settings": {
                    "model": model,
                    "delegation": delegation,
                    "auxiliary": Value::Object(auxiliary),
                    "stt": stt,
                },
                "hasOpenProxy": has_openproxy,
                "hasOpenProxyStt": hermes_yaml::is_openproxy_stt(stt.as_ref()),
                "configPath": home.config_path.to_string_lossy().to_string(),
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

/// GET /api/cli-tools/hermes-profiles — every Hermes profile on this machine
/// (default home + ~/.hermes/profiles/*). Discovery is filesystem-based: a directory
/// only counts as a profile when it carries one of Hermes' identity files.
async fn get_hermes_profiles(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }
    match hermes_profiles::list_profiles().await {
        Ok(profiles) => Json(json!({ "profiles": profiles })).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to list hermes profiles");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to list hermes profiles" })),
            )
                .into_response()
        }
    }
}

// ── POST handler ─────────────────────────────────────────────────────────────

async fn save_hermes_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<SaveHermesSettingsRequest>,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    // selections: [{role, model}] — "default" plus any auxiliary/delegation slots.
    // Legacy callers (CLI quick setup) send a bare `model` → treat as default role.
    let selections: Vec<(String, String)> = match &body.selections {
        Some(list) if list.iter().any(|s| s.role.is_some() && s.model.is_some()) => list
            .iter()
            .filter_map(|s| {
                let role = s.role.as_deref()?;
                let model = s.model.as_deref()?;
                Some((role.to_string(), model.to_string()))
            })
            .collect(),
        _ => body
            .model
            .as_deref()
            .map(|m| vec![("default".to_string(), m.to_string())])
            .unwrap_or_default(),
    };
    let default_sel = selections
        .iter()
        .find(|(role, _)| role == "default")
        .map(|(_, m)| m.clone());

    let Some(base_url) = body.base_url.as_deref().filter(|v| !v.is_empty()) else {
        return bad_request("baseUrl and model are required");
    };
    if default_sel.is_none() && body.audio.is_none() {
        return bad_request("baseUrl and model are required");
    }

    // audio: { enabled, model } — wire Hermes speech-to-text through openproxy's
    // /v1/audio/transcriptions. Absent = leave the stt section untouched (legacy CLI
    // callers). enabled:true needs a provider-qualified STT model (openproxy routes on it);
    // enabled:false removes exactly the wiring this endpoint writes.
    let audio_enabled = body
        .audio
        .as_ref()
        .is_some_and(|a| truthy(a.enabled.as_ref()));
    let audio_model = body
        .audio
        .as_ref()
        .and_then(|a| a.model.as_deref())
        .unwrap_or_default()
        .trim()
        .to_string();
    if audio_enabled && audio_model.is_empty() {
        return bad_request("audio.model is required when enabling audio transcription");
    }

    let normalized_base_url = normalize_base_url(base_url);

    // applyToAll refreshes endpoint + API key only (and the model blocks those belong to);
    // the stt wiring is per-profile and follows the single-profile path below.
    let apply_to_all = truthy(body.apply_to_all.as_ref())
        || matches!(query_param(&query, "applyToAll"), Some("true" | "1"));
    if apply_to_all {
        return apply_to_all_profiles(
            &normalized_base_url,
            body.api_key.as_deref(),
            default_sel.as_deref(),
        )
        .await;
    }

    // Profile target: JSON body first, then ?profile= — callers reach for either.
    let profile_param = body
        .profile
        .as_deref()
        .filter(|v| !v.is_empty())
        .or_else(|| query_param(&query, "profile"));
    let home = match resolve_profile(profile_param) {
        Ok(home) => home,
        Err(response) => return response,
    };
    if !home.is_default && !hermes_profiles::dir_exists(&home.dir).await {
        return profile_not_found(&home.name);
    }

    // Update config.yaml — upsert each role block, keep everything else.
    let mut new_yaml = match read_config_yaml(&home).await {
        Ok(yaml) => yaml,
        Err(error) => {
            tracing::warn!(?error, "failed to read hermes config");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to update hermes settings" })),
            )
                .into_response();
        }
    };
    for (role, role_model) in &selections {
        if role == "default" {
            new_yaml = hermes_yaml::upsert_model_block(
                &new_yaml,
                &hermes_yaml::build_model_block(role_model, &normalized_base_url),
            );
        } else if role == "delegation" {
            new_yaml = hermes_yaml::upsert_delegation_block(
                &new_yaml,
                &hermes_yaml::build_delegation_block(role_model, &normalized_base_url),
            );
        } else {
            new_yaml = hermes_yaml::upsert_aux_role(
                &new_yaml,
                role,
                &hermes_yaml::build_aux_role_block(role, role_model, &normalized_base_url),
            );
        }
    }
    if body.audio.is_some() {
        if audio_enabled {
            new_yaml = hermes_yaml::upsert_stt_openproxy(
                &new_yaml,
                audio_model.trim(),
                &normalized_base_url,
            );
        } else if hermes_yaml::is_openproxy_stt(hermes_yaml::parse_stt_block(&new_yaml).as_ref()) {
            // Never remove stt wiring this endpoint did not write — a user's own STT
            // provider choice survives an audio-disabled Apply.
            new_yaml = hermes_yaml::remove_stt_openproxy(&new_yaml);
        }
    }

    if let Err(error) = write_home_config(&home, &new_yaml).await {
        tracing::warn!(?error, "failed to write hermes config");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "Failed to update hermes settings" })),
        )
            .into_response();
    }

    // Update .env — upsert OPENAI_API_KEY only when caller provides one.
    if let Err(error) = write_home_env(&home, body.api_key.as_deref()).await {
        tracing::warn!(?error, "failed to write hermes env");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "Failed to update hermes settings" })),
        )
            .into_response();
    }

    let stt_cfg = hermes_yaml::parse_stt_block(&new_yaml);
    Json(json!({
        "success": true,
        "message": if home.is_default {
            "Hermes settings applied successfully!".to_string()
        } else {
            format!("Hermes settings applied to profile \"{}\"!", home.name)
        },
        "profile": home.to_json(),
        "sttOpenProxy": hermes_yaml::is_openproxy_stt(stt_cfg.as_ref()),
        "configPath": home.config_path.to_string_lossy().to_string(),
    }))
    .into_response()
}

// ── DELETE handler ───────────────────────────────────────────────────────────

async fn delete_hermes_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    // CLI reset sends an empty body → default profile. Profile target: JSON body
    // first, then ?profile= — callers reach for either.
    let body_profile = serde_json::from_slice::<DeleteHermesSettingsRequest>(&body)
        .ok()
        .and_then(|b| b.profile)
        .filter(|v| !v.is_empty());
    let profile_param = body_profile
        .as_deref()
        .or_else(|| query_param(&query, "profile"));
    let home = match resolve_profile(profile_param) {
        Ok(home) => home,
        Err(response) => return response,
    };
    if !home.is_default && !hermes_profiles::dir_exists(&home.dir).await {
        return profile_not_found(&home.name);
    }

    let yaml = match fs::read_to_string(&home.config_path).await {
        Ok(yaml) => yaml,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Json(json!({
                "success": true,
                "profile": home.name,
                "message": "No config file to reset",
            }))
            .into_response();
        }
        Err(error) => {
            tracing::warn!(?error, "failed to read hermes config for reset");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to reset hermes settings" })),
            )
                .into_response();
        }
    };

    // Only drop blocks openproxy wrote (custom provider) — a model/delegation/aux block
    // owned by another provider is this profile's own configuration and must survive.
    let mut removed = json!({
        "model": false,
        "delegation": false,
        "auxiliary": [],
        "stt": false,
    });
    let mut kept: Vec<&str> = Vec::new();
    let mut yaml = yaml;

    if let Some(main) = hermes_yaml::parse_model_block(&yaml) {
        if hermes_yaml::is_custom_block(Some(&main)) {
            yaml = hermes_yaml::remove_model_block(&yaml);
            removed["model"] = json!(true);
        } else {
            kept.push("model");
        }
    }

    if let Some(delegation) = hermes_yaml::parse_delegation_block(&yaml) {
        if hermes_yaml::is_custom_block(Some(&delegation)) {
            yaml = hermes_yaml::remove_delegation_block(&yaml);
            removed["delegation"] = json!(true);
        } else {
            kept.push("delegation");
        }
    }

    for (role, cfg) in hermes_yaml::parse_aux_roles(&yaml) {
        if hermes_yaml::is_custom_block(Some(&cfg)) {
            yaml = hermes_yaml::remove_aux_role(&yaml, &role);
            removed["auxiliary"]
                .as_array_mut()
                .unwrap()
                .push(json!(role));
        }
    }

    if let Some(stt_cfg) = hermes_yaml::parse_stt_block(&yaml) {
        if hermes_yaml::is_openproxy_stt(Some(&stt_cfg)) {
            yaml = hermes_yaml::remove_stt_openproxy(&yaml);
            removed["stt"] = json!(true);
            // User settings living inside the block (language, local, ...) survive the
            // key removal — report the remainder as kept, like any other preserved section.
            if hermes_yaml::parse_stt_block(&yaml).is_some() {
                kept.push("stt");
            }
        } else {
            kept.push("stt");
        }
    }

    let yaml = yaml.trim_start_matches('\n').to_string();
    if let Err(error) = write_home_config(&home, &yaml).await {
        tracing::warn!(?error, "failed to write hermes config for reset");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "Failed to reset hermes settings" })),
        )
            .into_response();
    }

    let scope = if home.is_default {
        String::new()
    } else {
        format!(" in profile \"{}\"", home.name)
    };
    Json(json!({
        "success": true,
        "profile": home.name,
        "removed": removed,
        "kept": kept,
        "message": format!("{PROVIDER_NAME} model blocks removed{scope}"),
    }))
    .into_response()
}

// ── applyToAll ───────────────────────────────────────────────────────────────

/// Endpoint + API key across every profile: refresh the openproxy blocks each profile
/// already has (models stay per-profile), and wire up profiles that have no main model
/// yet using the model chosen in this request. Profiles on another provider are
/// reported, never rewritten.
async fn apply_to_all_profiles(
    normalized_base_url: &str,
    api_key: Option<&str>,
    model: Option<&str>,
) -> Response {
    let profiles = match hermes_profiles::list_profiles().await {
        Ok(profiles) => profiles,
        Err(error) => {
            tracing::warn!(?error, "failed to list hermes profiles for applyToAll");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to list hermes profiles" })),
            )
                .into_response();
        }
    };

    let mut results: Vec<Value> = Vec::new();
    for profile in &profiles {
        let name = profile
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("default")
            .to_string();
        // Names come from list_profiles() discovery, so resolution cannot fail —
        // fall back to the default home defensively.
        let home = hermes_profiles::resolve_hermes_home(Some(&name))
            .unwrap_or_else(|_| hermes_profiles::resolve_hermes_home(None).unwrap());

        let outcome: AnyhowResult<Value> = async {
            let mut yaml = read_config_yaml(&home).await?;
            let main = hermes_yaml::parse_model_block(&yaml);

            if let Some(main_model) = main
                .as_ref()
                .and_then(|m| m.get("default"))
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
            {
                if hermes_yaml::is_custom_block(main.as_ref()) {
                    yaml = hermes_yaml::upsert_model_block(
                        &yaml,
                        &hermes_yaml::build_model_block(main_model, normalized_base_url),
                    );
                    if let Some(delegation) = hermes_yaml::parse_delegation_block(&yaml) {
                        if let Some(d_model) = delegation
                            .get("model")
                            .and_then(Value::as_str)
                            .filter(|v| !v.is_empty())
                        {
                            if hermes_yaml::is_custom_block(Some(&delegation)) {
                                yaml = hermes_yaml::upsert_delegation_block(
                                    &yaml,
                                    &hermes_yaml::build_delegation_block(
                                        d_model,
                                        normalized_base_url,
                                    ),
                                );
                            }
                        }
                    }
                    for (role, cfg) in hermes_yaml::parse_aux_roles(&yaml) {
                        if let Some(a_model) = cfg.get("model").and_then(Value::as_str).filter(|v| !v.is_empty()) {
                            if hermes_yaml::is_custom_block(Some(&cfg)) {
                                yaml = hermes_yaml::upsert_aux_role(
                                    &yaml,
                                    &role,
                                    &hermes_yaml::build_aux_role_block(
                                        &role,
                                        a_model,
                                        normalized_base_url,
                                    ),
                                );
                            }
                        }
                    }
                    write_home_config(&home, &yaml).await?;
                    write_home_env(&home, api_key).await?;
                    Ok(json!({"profile": name, "status": "updated", "model": main_model}))
                } else {
                    let provider = main
                        .as_ref()
                        .and_then(|m| m.get("provider"))
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    Ok(json!({
                        "profile": name,
                        "status": "skipped",
                        "reason": format!("main model uses provider \"{provider}\" — configure this profile individually"),
                    }))
                }
            } else if let Some(model) = model.filter(|m| !m.is_empty()) {
                yaml = hermes_yaml::upsert_model_block(
                    &yaml,
                    &hermes_yaml::build_model_block(model, normalized_base_url),
                );
                write_home_config(&home, &yaml).await?;
                write_home_env(&home, api_key).await?;
                Ok(json!({"profile": name, "status": "updated", "model": model}))
            } else {
                Ok(json!({
                    "profile": name,
                    "status": "skipped",
                    "reason": "no main model configured",
                }))
            }
        }
        .await;

        match outcome {
            Ok(value) => results.push(value),
            Err(error) => results.push(json!({
                "profile": name,
                "status": "failed",
                "reason": error.to_string(),
            })),
        }
    }

    let updated = results
        .iter()
        .filter(|r| r.get("status").and_then(Value::as_str) == Some("updated"))
        .count();
    let total = results.len();
    Json(json!({
        "success": updated > 0,
        "bulk": true,
        "results": results,
        "updated": updated,
        "skipped": total - updated,
        "message": format!("Updated endpoint on {updated} profile(s)"),
    }))
    .into_response()
}

// ── small helpers ────────────────────────────────────────────────────────────

fn resolve_profile(profile: Option<&str>) -> Result<HermesHome, Response> {
    hermes_profiles::resolve_hermes_home(profile).map_err(|message| {
        (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
    })
}

fn profile_not_found(name: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": format!("Hermes profile \"{name}\" not found") })),
    )
        .into_response()
}

fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
}
