//! Oh My Pi (omp) settings — port of 9router
//! `src/app/api/cli-tools/omp-settings/route.js`.
//!
//! Config: `~/.omp/agent/models.yml` (primary) plus a best-effort credential
//! row in `~/.omp/agent/agent.db` (`auth_credentials` table) when that
//! SQLite file already exists.

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

use super::shared::{command_exists, home_dir, normalize_base_url, resolve_cli_api_key};
use crate::server::state::AppState;

const PROVIDER_ID: &str = "9router";

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/cli-tools/omp-settings",
        get(get_omp_settings)
            .post(save_omp_settings)
            .delete(delete_omp_settings),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveOmpSettingsRequest {
    base_url: Option<String>,
    api_key: Option<String>,
}

pub(super) async fn get_omp_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    if !check_omp_installed().await {
        return Json(json!({
            "installed": false,
            "config": Value::Null,
            "message": "Oh My Pi is not installed",
        }))
        .into_response();
    }

    let yml = read_models_yml().await.unwrap_or_default();
    Json(json!({
        "installed": true,
        "has9Router": has_openproxy_in_yml(&yml),
        "configPath": models_yml_path().to_string_lossy().to_string(),
    }))
    .into_response()
}

async fn save_omp_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SaveOmpSettingsRequest>,
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

    match write_omp_settings(&state, &body, &base_url).await {
        Ok(config_path) => Json(json!({
            "success": true,
            "message": "Oh My Pi settings applied! Run 'omp' and all 9Router models appear under 9router in /model.",
            "configPath": config_path,
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to write omp settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to apply omp settings: {error}") })),
            )
                .into_response()
        }
    }
}

async fn delete_omp_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_omp_settings().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to reset omp settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to reset omp settings: {error}") })),
            )
                .into_response()
        }
    }
}

async fn check_omp_installed() -> bool {
    if command_exists("omp").await {
        return true;
    }
    for path in [db_path(), models_yml_path()] {
        if fs::metadata(path).await.is_ok() {
            return true;
        }
    }
    false
}

async fn read_models_yml() -> AnyhowResult<String> {
    match fs::read_to_string(models_yml_path()).await {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

fn has_openproxy_in_yml(content: &str) -> bool {
    !content.is_empty() && (content.contains("9router:") || content.contains("localhost:20128"))
}

/// Remove the `9router:` provider block (and its indented children) from the
/// YAML. Upstream uses the regex `\s*9router:[\s\S]*?(?=\n\s*\w+:|$)` — Rust
/// `regex` has no lookahead, so this is a line-based equivalent: drop the
/// `9router:` line plus every following line more indented than it.
fn remove_provider_block(yml: &str) -> String {
    let lines: Vec<&str> = yml.lines().collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut skipping_indent: Option<usize> = None;

    for line in lines {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim_start();

        if let Some(skip_indent) = skipping_indent {
            // Still inside the removed block while the line is blank or more
            // indented than the `9router:` key line.
            if trimmed.is_empty() || indent > skip_indent {
                continue;
            }
            skipping_indent = None;
        }

        if trimmed.starts_with(&format!("{PROVIDER_ID}:")) {
            skipping_indent = Some(indent);
            continue;
        }

        out.push(line);
    }

    // Preserve a trailing newline like the source file had.
    let mut result = out.join("\n");
    if yml.ends_with('\n') && !result.is_empty() {
        result.push('\n');
    }
    result
}

fn build_provider_block(base_url: &str, api_key: &str) -> String {
    let normalized = normalize_base_url(base_url);
    format!(
        "  {PROVIDER_ID}:\n    baseUrl: {normalized}\n    apiKey: {api_key}\n    api: openai-completions\n    authHeader: true\n    disableStrictTools: true\n    discovery:\n      type: proxy"
    )
}

fn insert_provider_block(yml: &str, block: &str) -> String {
    if yml.trim().is_empty() {
        return format!("providers:\n{block}\n");
    }
    if let Some(pos) = yml.find("providers:") {
        let insert_at = pos + "providers:".len();
        format!("{}\n{block}{}", &yml[..insert_at], &yml[insert_at..])
    } else {
        format!("{}\n\nproviders:\n{}\n", yml.trim_end(), block)
    }
}

async fn write_omp_settings(
    state: &AppState,
    body: &SaveOmpSettingsRequest,
    base_url: &str,
) -> AnyhowResult<String> {
    let dir = omp_dir();
    fs::create_dir_all(&dir).await?;

    let api_key = resolve_cli_api_key(state, body.api_key.as_deref());

    let yml = read_models_yml().await.unwrap_or_default();
    let without = remove_provider_block(&yml);
    let block = build_provider_block(base_url, &api_key);
    let updated = insert_provider_block(&without, &block);
    fs::write(models_yml_path(), updated).await?;

    // Best-effort agent.db credential row — only when the db already exists
    // (opening it unconditionally would create an empty file).
    if fs::metadata(db_path()).await.is_ok() {
        let _ = update_agent_db(&api_key, base_url);
    }

    Ok(models_yml_path().to_string_lossy().to_string())
}

/// Mirror upstream's best-effort `auth_credentials` upsert. Non-fatal: the
/// models.yml provider block is the primary config.
fn update_agent_db(api_key: &str, base_url: &str) -> AnyhowResult<()> {
    let conn = rusqlite::Connection::open(db_path())?;
    conn.execute(
        "DELETE FROM auth_credentials WHERE provider = ?1",
        rusqlite::params![PROVIDER_ID],
    )?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let data = serde_json::json!({ "apiKey": api_key, "baseUrl": base_url }).to_string();
    conn.execute(
        "INSERT INTO auth_credentials (provider, credential_type, data, disabled_cause, identity_key, created_at, updated_at) VALUES (?1, ?2, ?3, NULL, NULL, ?4, ?4)",
        rusqlite::params![PROVIDER_ID, "api_key", data, now],
    )?;
    Ok(())
}

async fn reset_omp_settings() -> AnyhowResult<Value> {
    let yml = read_models_yml().await.unwrap_or_default();
    let updated = remove_provider_block(&yml);

    if updated.trim() == "providers:" {
        match fs::remove_file(models_yml_path()).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    } else {
        fs::write(models_yml_path(), updated).await?;
    }

    Ok(json!({
        "success": true,
        "message": "9Router removed from Oh My Pi",
    }))
}

fn omp_dir() -> PathBuf {
    home_dir().join(".omp").join("agent")
}

fn models_yml_path() -> PathBuf {
    omp_dir().join("models.yml")
}

fn db_path() -> PathBuf {
    omp_dir().join("agent.db")
}
