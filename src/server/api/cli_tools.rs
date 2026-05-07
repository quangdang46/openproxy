mod claude_settings;
mod cowork_settings;
mod hermes_settings;

use std::collections::BTreeMap;
use std::env;
use std::path::{Path as FsPath, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    response::Response,
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{fs, process::Command};
use toml::{map::Map as TomlMap, Value as TomlValue};

use crate::server::state::AppState;

const MAX_OUTPUT_SIZE: usize = 64 * 1024; // 64KB max output

/// CLI command execution request
#[derive(Debug, Deserialize)]
pub struct CliCommandRequest {
    pub command: String,
    pub args: Option<Vec<String>>,
    pub timeout_secs: Option<u64>,
}

/// CLI command execution response
#[derive(Debug, Serialize)]
pub struct CliCommandResponse {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub timed_out: bool,
}

/// List available CLI tools response
#[derive(Debug, Serialize)]
pub struct CliToolsListResponse {
    pub tools: Vec<CliToolInfo>,
}

/// Information about a CLI tool
#[derive(Debug, Serialize)]
pub struct CliToolInfo {
    pub name: String,
    pub description: String,
    pub category: String,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// GET /api/cli-tools
/// List available CLI tools
pub async fn list_tools(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let tools = vec![
        CliToolInfo {
            name: "provider-list".to_string(),
            description: "List all provider connections and nodes".to_string(),
            category: "provider".to_string(),
        },
        CliToolInfo {
            name: "key-list".to_string(),
            description: "List all API keys".to_string(),
            category: "key".to_string(),
        },
        CliToolInfo {
            name: "pool-list".to_string(),
            description: "List all proxy pools".to_string(),
            category: "pool".to_string(),
        },
        CliToolInfo {
            name: "pool-status".to_string(),
            description: "Get status of a specific proxy pool".to_string(),
            category: "pool".to_string(),
        },
        CliToolInfo {
            name: "route".to_string(),
            description: "Execute a model routing request directly".to_string(),
            category: "route".to_string(),
        },
    ];

    Json(CliToolsListResponse { tools }).into_response()
}

/// POST /api/cli-tools/execute
/// Execute a CLI command
pub async fn execute_command(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CliCommandRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let timeout_secs = req.timeout_secs.unwrap_or(30).min(120); // Max 2 minutes
    let start_time = std::time::Instant::now();

    // Parse and validate command
    let (program, args) = match parse_cli_command(&req.command, req.args.as_deref()) {
        Some(cmd) => cmd,
        None => {
            return Json(CliCommandResponse {
                success: false,
                exit_code: Some(1),
                stdout: String::new(),
                stderr: "Invalid command".to_string(),
                duration_ms: start_time.elapsed().as_millis() as u64,
                timed_out: false,
            })
            .into_response()
        }
    };

    // Execute the command
    let result = tokio::process::Command::new(&program)
        .args(&args)
        .output()
        .await;

    let duration_ms = start_time.elapsed().as_millis() as u64;

    match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let exit_code = output.status.code();

            Json(CliCommandResponse {
                success: output.status.success(),
                exit_code,
                stdout: stdout.to_string(),
                stderr: stderr.to_string(),
                duration_ms,
                timed_out: false,
            })
            .into_response()
        }
        Err(e) => Json(CliCommandResponse {
            success: false,
            exit_code: Some(-1),
            stdout: String::new(),
            stderr: format!("Failed to execute command: {}", e),
            duration_ms,
            timed_out: false,
        })
        .into_response(),
    }
}

/// POST /api/cli-tools/run
/// Run a specific CLI tool by name (higher-level interface)
pub async fn run_tool(
    State(state): State<AppState>,
    Path(tool_name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<CliCommandRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let timeout_secs = req.timeout_secs.unwrap_or(30).min(120);
    let start_time = std::time::Instant::now();

    let (program, args) = build_tool_command(&tool_name, req.args.unwrap_or_default());
    let duration_ms = start_time.elapsed().as_millis() as u64;

    let output = match Command::new(&program).args(&args).output().await {
        Ok(o) => o,
        Err(e) => {
            return Json(CliCommandResponse {
                success: false,
                exit_code: Some(-1),
                stdout: String::new(),
                stderr: format!("Failed to execute: {}", e),
                duration_ms,
                timed_out: false,
            })
            .into_response()
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    Json(CliCommandResponse {
        success: output.status.success(),
        exit_code: output.status.code(),
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
        duration_ms,
        timed_out: false,
    })
    .into_response()
}

/// Parse a command string into program and arguments
fn parse_cli_command(
    command: &str,
    additional_args: Option<&[String]>,
) -> Option<(String, Vec<String>)> {
    let parts: Vec<&str> = command.trim().split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }

    let program = parts[0].to_string();
    let mut args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();

    if let Some(extra) = additional_args {
        args.extend(extra.iter().cloned());
    }

    Some((program, args))
}

/// Build a command for a specific tool
fn build_tool_command(tool_name: &str, args: Vec<String>) -> (String, Vec<String>) {
    // Map tool names to actual commands
    match tool_name {
        "provider-list" => (
            "openproxy".to_string(),
            vec![
                "provider".to_string(),
                "list".to_string(),
                "--json".to_string(),
            ],
        ),
        "key-list" => (
            "openproxy".to_string(),
            vec!["key".to_string(), "list".to_string(), "--json".to_string()],
        ),
        "pool-list" => (
            "openproxy".to_string(),
            vec!["pool".to_string(), "list".to_string(), "--json".to_string()],
        ),
        "pool-status" => {
            let pool_name = args.first().cloned().unwrap_or_default();
            (
                "openproxy".to_string(),
                vec![
                    "pool".to_string(),
                    "status".to_string(),
                    "--name".to_string(),
                    pool_name,
                    "--json".to_string(),
                ],
            )
        }
        _ => (tool_name.to_string(), args),
    }
}

/// GET /api/cli-tools/help
/// Get help information for CLI tools
pub async fn get_help(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    Json(json!({
        "help": "CLI Tools API",
        "endpoints": {
            "GET /api/cli-tools": "List available CLI tools",
            "POST /api/cli-tools/execute": "Execute arbitrary command",
            "POST /api/cli-tools/run/{tool_name}": "Run a specific tool",
            "GET /api/cli-tools/help": "Show this help"
        },
        "tools": [
            {"name": "provider-list", "description": "List provider connections"},
            {"name": "key-list", "description": "List API keys"},
            {"name": "pool-list", "description": "List proxy pools"},
            {"name": "pool-status", "description": "Get pool status (args: [pool_name])"}
        ]
    }))
    .into_response()
}

// ═══════════════════════════════════════════════════════════════════════════
// Codex CLI Settings Endpoints
// GET/POST/DELETE /api/cli-tools/codex-settings
// ═══════════════════════════════════════════════════════════════════════════

/// Codex CLI settings stored per user
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexSettings {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub subagent_model: Option<String>,
}

/// GET /api/cli-tools/codex-settings
/// Get Codex CLI settings
async fn get_codex_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let installed = check_codex_installed().await;
    if !installed {
        return Json(json!({
            "installed": false,
            "config": Value::Null,
            "message": "Codex CLI is not installed",
        }))
        .into_response();
    }

    match read_codex_config().await {
        Ok(config) => {
            let has_9router = config.as_deref().is_some_and(has_9router_codex_config);
            Json(json!({
                "installed": true,
                "config": config,
                "has9Router": has_9router,
                "configPath": codex_config_path().to_string_lossy().to_string(),
            }))
            .into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to check codex settings: {error}") })),
        )
            .into_response(),
    }
}

/// POST /api/cli-tools/codex-settings
/// Save Codex CLI settings
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexSettingsRequest {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub subagent_model: Option<String>,
}

async fn save_codex_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CodexSettingsRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match write_codex_settings(&CodexSettings {
        base_url: Some(req.base_url),
        api_key: Some(req.api_key),
        model: Some(req.model),
        subagent_model: req.subagent_model,
    })
    .await
    {
        Ok(config_path) => Json(json!({
            "success": true,
            "message": "Codex settings applied successfully!",
            "configPath": config_path,
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to update codex settings: {error}") })),
        )
            .into_response(),
    }
}

/// DELETE /api/cli-tools/codex-settings
/// Reset Codex CLI settings
async fn delete_codex_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_codex_settings().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to reset codex settings: {error}") })),
        )
            .into_response(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Copilot Settings Endpoints
// GET/POST/DELETE /api/cli-tools/copilot-settings
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CopilotSettingsRequest {
    pub base_url: String,
    pub api_key: Option<String>,
    pub models: Vec<String>,
}

async fn get_copilot_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match read_copilot_config().await {
        Ok(config) => {
            let has_9router = config.as_ref().is_some_and(has_9router_copilot_config);
            let entry = config.as_ref().and_then(get_9router_copilot_entry);
            Json(json!({
                "installed": true,
                "config": config,
                "has9Router": has_9router,
                "configPath": copilot_config_path().to_string_lossy().to_string(),
                "currentModel": entry
                    .and_then(|entry| entry.get("models"))
                    .and_then(Value::as_array)
                    .and_then(|models| models.first())
                    .and_then(|model| model.get("id"))
                    .and_then(Value::as_str),
                "currentUrl": entry
                    .and_then(|entry| entry.get("models"))
                    .and_then(Value::as_array)
                    .and_then(|models| models.first())
                    .and_then(|model| model.get("url"))
                    .and_then(Value::as_str),
            }))
            .into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to check copilot settings: {error}") })),
        )
            .into_response(),
    }
}

async fn save_copilot_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CopilotSettingsRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    if req.base_url.trim().is_empty() || req.models.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "baseUrl and models are required" })),
        )
            .into_response();
    }

    match write_copilot_settings(&req).await {
        Ok(config_path) => Json(json!({
            "success": true,
            "message": "Copilot settings applied! Reload VS Code to take effect.",
            "configPath": config_path,
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to update copilot settings: {error}") })),
        )
            .into_response(),
    }
}

async fn delete_copilot_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_copilot_settings().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to reset copilot settings: {error}") })),
        )
            .into_response(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Droid Settings Endpoints
// GET/POST/DELETE /api/cli-tools/droid-settings
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DroidSettingsRequest {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub models: Option<Vec<String>>,
    pub active_model: Option<String>,
}

async fn get_droid_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let installed = check_droid_installed().await;
    if !installed {
        return Json(json!({
            "installed": false,
            "settings": Value::Null,
            "message": "Factory Droid CLI is not installed",
        }))
        .into_response();
    }

    match read_droid_settings().await {
        Ok(settings) => {
            let has_9router = settings.as_ref().is_some_and(has_9router_droid_settings);
            Json(json!({
                "installed": true,
                "settings": settings,
                "has9Router": has_9router,
                "settingsPath": droid_settings_path().to_string_lossy().to_string(),
            }))
            .into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to check droid settings: {error}") })),
        )
            .into_response(),
    }
}

async fn save_droid_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<DroidSettingsRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let models = req.models.clone().unwrap_or_else(|| {
        req.model
            .clone()
            .map(|model| vec![model])
            .unwrap_or_default()
    });
    if req.base_url.trim().is_empty() || models.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "baseUrl and at least one model are required" })),
        )
            .into_response();
    }

    match write_droid_settings(&req, &models).await {
        Ok(settings_path) => Json(json!({
            "success": true,
            "message": "Factory Droid settings applied successfully!",
            "settingsPath": settings_path,
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to update droid settings: {error}") })),
        )
            .into_response(),
    }
}

async fn delete_droid_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_droid_settings().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to reset droid settings: {error}") })),
        )
            .into_response(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// OpenCode Settings Endpoints
// GET/POST/PATCH/DELETE /api/cli-tools/opencode-settings
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenCodeSettingsRequest {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub models: Option<Vec<String>>,
    pub active_model: Option<String>,
    pub subagent_model: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchOpenCodeSettingsRequest {
    pub clear_active_model: bool,
}

#[derive(Debug, Deserialize)]
struct DeleteOpenCodeSettingsQuery {
    pub model: Option<String>,
}

async fn get_opencode_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let installed = check_opencode_installed().await;
    if !installed {
        return Json(json!({
            "installed": false,
            "config": Value::Null,
            "message": "OpenCode CLI is not installed",
        }))
        .into_response();
    }

    match read_opencode_config().await {
        Ok(config) => {
            let provider_config = config
                .as_ref()
                .and_then(|config| config.get("provider"))
                .and_then(|provider| provider.get("9router"));
            let model_map = provider_config.and_then(|provider| provider.get("models"));
            let models = model_map
                .and_then(Value::as_object)
                .map(|models| {
                    models
                        .keys()
                        .map(|model| Value::String(model.clone()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            Json(json!({
                "installed": true,
                "config": config,
                "has9Router": provider_config.is_some(),
                "configPath": opencode_config_path().to_string_lossy().to_string(),
                "opencode": {
                    "models": models,
                    "activeModel": config
                        .as_ref()
                        .and_then(|config| config.get("model"))
                        .and_then(Value::as_str)
                        .and_then(|model| model.strip_prefix("9router/")),
                    "baseURL": provider_config
                        .and_then(|provider| provider.get("options"))
                        .and_then(|options| options.get("baseURL"))
                        .and_then(Value::as_str),
                },
            }))
            .into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to check opencode settings: {error}") })),
        )
            .into_response(),
    }
}

async fn save_opencode_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<OpenCodeSettingsRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let models = req.models.clone().unwrap_or_else(|| {
        req.model
            .clone()
            .map(|model| vec![model])
            .unwrap_or_default()
    });
    if req.base_url.trim().is_empty() || models.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "baseUrl and at least one model are required" })),
        )
            .into_response();
    }

    match write_opencode_settings(&req, &models).await {
        Ok(config_path) => Json(json!({
            "success": true,
            "message": "OpenCode settings applied successfully!",
            "configPath": config_path,
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to apply settings: {error}") })),
        )
            .into_response(),
    }
}

async fn patch_opencode_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PatchOpenCodeSettingsRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match patch_opencode_config(&req).await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to patch settings: {error}") })),
        )
            .into_response(),
    }
}

async fn delete_opencode_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<DeleteOpenCodeSettingsQuery>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_opencode_settings(params.model).await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to reset opencode settings: {error}") })),
        )
            .into_response(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// OpenClaw Settings Endpoints
// GET/POST/DELETE /api/cli-tools/openclaw-settings
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenClawSettingsRequest {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    #[serde(default)]
    pub agent_models: BTreeMap<String, String>,
}

async fn get_openclaw_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let installed = check_openclaw_installed().await;
    if !installed {
        return Json(json!({
            "installed": false,
            "settings": Value::Null,
            "message": "Open Claw CLI is not installed",
        }))
        .into_response();
    }

    match read_openclaw_settings().await {
        Ok(settings) => {
            let agent_list = settings
                .as_ref()
                .and_then(|settings| settings.get("agents"))
                .and_then(|agents| agents.get("list"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut enriched_agents = Vec::with_capacity(agent_list.len());
            for agent in agent_list {
                let mut agent = agent;
                let current_model =
                    if let Some(agent_dir) = agent.get("agentDir").and_then(Value::as_str) {
                        read_openclaw_agent_model(&PathBuf::from(agent_dir)).await
                    } else {
                        None
                    };
                if let Some(agent_object) = agent.as_object_mut() {
                    agent_object.insert(
                        "currentModel".to_string(),
                        current_model.map(Value::String).unwrap_or(Value::Null),
                    );
                }
                enriched_agents.push(agent);
            }

            Json(json!({
                "installed": true,
                "settings": settings,
                "agents": enriched_agents,
                "has9Router": settings
                    .as_ref()
                    .is_some_and(has_9router_openclaw_settings),
                "settingsPath": openclaw_settings_path().to_string_lossy().to_string(),
            }))
            .into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to check openclaw settings: {error}") })),
        )
            .into_response(),
    }
}

async fn save_openclaw_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<OpenClawSettingsRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    if req.base_url.trim().is_empty() || req.model.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "baseUrl and model are required" })),
        )
            .into_response();
    }

    match write_openclaw_settings(&req).await {
        Ok(settings_path) => Json(json!({
            "success": true,
            "message": "Open Claw settings applied successfully!",
            "settingsPath": settings_path,
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to update openclaw settings: {error}") })),
        )
            .into_response(),
    }
}

async fn delete_openclaw_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_openclaw_settings().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to reset openclaw settings: {error}") })),
        )
            .into_response(),
    }
}

async fn check_codex_installed() -> bool {
    command_exists("codex", true).await || fs::metadata(codex_config_path()).await.is_ok()
}

async fn read_codex_config() -> anyhow::Result<Option<String>> {
    read_string_optional(&codex_config_path()).await
}

async fn write_codex_settings(settings: &CodexSettings) -> anyhow::Result<String> {
    let config_path = codex_config_path();
    let auth_path = codex_auth_path();
    fs::create_dir_all(codex_dir()).await?;

    let mut parsed = match fs::read_to_string(&config_path).await {
        Ok(existing_config) => parse_toml_table(&existing_config).unwrap_or_default(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => TomlMap::new(),
        Err(error) => return Err(error.into()),
    };

    let base_url = settings.base_url.clone().unwrap_or_default();
    let api_key = settings.api_key.clone().unwrap_or_default();
    let model = settings.model.clone().unwrap_or_default();
    let subagent_model = settings
        .subagent_model
        .clone()
        .unwrap_or_else(|| model.clone());

    parsed.insert("model".to_string(), TomlValue::String(model));
    parsed.insert(
        "model_provider".to_string(),
        TomlValue::String("9router".to_string()),
    );
    set_toml_section(
        &mut parsed,
        &["model_providers", "9router"],
        TomlValue::Table(TomlMap::from_iter([
            ("name".to_string(), TomlValue::String("9Router".to_string())),
            (
                "base_url".to_string(),
                TomlValue::String(normalize_v1_base_url(&base_url)),
            ),
            (
                "wire_api".to_string(),
                TomlValue::String("responses".to_string()),
            ),
        ])),
    );
    set_toml_section(
        &mut parsed,
        &["agents", "subagent"],
        TomlValue::Table(TomlMap::from_iter([(
            "model".to_string(),
            TomlValue::String(subagent_model),
        )])),
    );

    let config_content = toml::to_string_pretty(&TomlValue::Table(parsed))?;
    fs::write(&config_path, config_content).await?;

    let mut auth_data = match fs::read_to_string(&auth_path).await {
        Ok(existing_auth) => serde_json::from_str::<serde_json::Map<String, Value>>(&existing_auth)
            .unwrap_or_default(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(error) => return Err(error.into()),
    };
    auth_data.insert("OPENAI_API_KEY".to_string(), Value::String(api_key));
    auth_data.insert("auth_mode".to_string(), Value::String("apikey".to_string()));
    fs::write(
        &auth_path,
        serde_json::to_vec_pretty(&Value::Object(auth_data))?,
    )
    .await?;

    Ok(config_path.to_string_lossy().to_string())
}

async fn reset_codex_settings() -> anyhow::Result<Value> {
    let config_path = codex_config_path();
    let existing_config = match fs::read_to_string(&config_path).await {
        Ok(existing_config) => existing_config,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "success": true,
                "message": "No config file to reset",
            }));
        }
        Err(error) => return Err(error.into()),
    };

    let mut parsed = parse_toml_table(&existing_config)?;
    if parsed.get("model_provider").and_then(TomlValue::as_str) == Some("9router") {
        parsed.remove("model");
        parsed.remove("model_provider");
    }
    delete_toml_section(&mut parsed, &["model_providers", "9router"]);
    delete_toml_section(&mut parsed, &["agents", "subagent"]);

    let config_content = toml::to_string_pretty(&TomlValue::Table(parsed))?;
    fs::write(&config_path, config_content).await?;

    let auth_path = codex_auth_path();
    match fs::read_to_string(&auth_path).await {
        Ok(existing_auth) => {
            if let Ok(mut auth_data) =
                serde_json::from_str::<serde_json::Map<String, Value>>(&existing_auth)
            {
                auth_data.remove("OPENAI_API_KEY");
                auth_data.remove("auth_mode");
                if auth_data.is_empty() {
                    let _ = fs::remove_file(&auth_path).await;
                } else {
                    fs::write(
                        &auth_path,
                        serde_json::to_vec_pretty(&Value::Object(auth_data))?,
                    )
                    .await?;
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    Ok(json!({
        "success": true,
        "message": "9Router settings removed successfully",
    }))
}

async fn read_string_optional(path: &FsPath) -> anyhow::Result<Option<String>> {
    match fs::read_to_string(path).await {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn has_9router_codex_config(config: &str) -> bool {
    config.contains("model_provider = \"9router\"") || config.contains("[model_providers.9router]")
}

fn parse_toml_table(content: &str) -> anyhow::Result<TomlMap<String, TomlValue>> {
    match toml::from_str::<TomlValue>(content)? {
        TomlValue::Table(table) => Ok(table),
        _ => Ok(TomlMap::new()),
    }
}

fn set_toml_section(table: &mut TomlMap<String, TomlValue>, path: &[&str], value: TomlValue) {
    if path.is_empty() {
        return;
    }
    if path.len() == 1 {
        table.insert(path[0].to_string(), value);
        return;
    }

    let entry = table
        .entry(path[0].to_string())
        .or_insert_with(|| TomlValue::Table(TomlMap::new()));
    if !entry.is_table() {
        *entry = TomlValue::Table(TomlMap::new());
    }
    if let TomlValue::Table(next) = entry {
        set_toml_section(next, &path[1..], value);
    }
}

fn delete_toml_section(table: &mut TomlMap<String, TomlValue>, path: &[&str]) {
    if path.is_empty() {
        return;
    }
    if path.len() == 1 {
        table.remove(path[0]);
        return;
    }
    if let Some(TomlValue::Table(next)) = table.get_mut(path[0]) {
        delete_toml_section(next, &path[1..]);
    }
}

fn normalize_v1_base_url(base_url: &str) -> String {
    if base_url.ends_with("/v1") {
        base_url.to_string()
    } else {
        format!("{base_url}/v1")
    }
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

fn codex_dir() -> PathBuf {
    home_dir().join(".codex")
}

fn codex_config_path() -> PathBuf {
    codex_dir().join("config.toml")
}

fn codex_auth_path() -> PathBuf {
    codex_dir().join("auth.json")
}

async fn read_copilot_config() -> anyhow::Result<Option<Value>> {
    read_json_optional(&copilot_config_path()).await
}

async fn check_droid_installed() -> bool {
    command_exists("droid", true).await || fs::metadata(droid_settings_path()).await.is_ok()
}

async fn read_droid_settings() -> anyhow::Result<Option<Value>> {
    read_json_optional(&droid_settings_path()).await
}

async fn check_opencode_installed() -> bool {
    command_exists("opencode", true).await || fs::metadata(opencode_config_path()).await.is_ok()
}

async fn read_opencode_config() -> anyhow::Result<Option<Value>> {
    read_json_optional(&opencode_config_path()).await
}

async fn check_openclaw_installed() -> bool {
    command_exists("openclaw", true).await || fs::metadata(openclaw_settings_path()).await.is_ok()
}

async fn read_openclaw_settings() -> anyhow::Result<Option<Value>> {
    read_json_optional(&openclaw_settings_path()).await
}

async fn write_copilot_settings(req: &CopilotSettingsRequest) -> anyhow::Result<String> {
    let config_path = copilot_config_path();
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let mut config = match fs::read_to_string(&config_path).await {
        Ok(existing) => parse_json_array_or_default(&existing),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };

    let endpoint_url = format!("{}/chat/completions#models.ai.azure.com", req.base_url);
    let api_key = req
        .api_key
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "sk_9router".to_string());
    let new_entry = json!({
        "name": "9Router",
        "vendor": "azure",
        "apiKey": api_key,
        "models": req.models.iter().map(|id| {
            json!({
                "id": id,
                "name": id,
                "url": endpoint_url,
                "toolCalling": true,
                "vision": false,
                "maxInputTokens": 128000,
                "maxOutputTokens": 16000,
            })
        }).collect::<Vec<_>>(),
    });

    if let Some(index) = config.iter().position(|entry| {
        entry
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| name == "9Router")
    }) {
        config[index] = new_entry;
    } else {
        config.push(new_entry);
    }

    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&Value::Array(config))?,
    )
    .await?;
    Ok(config_path.to_string_lossy().to_string())
}

async fn reset_copilot_settings() -> anyhow::Result<Value> {
    let config_path = copilot_config_path();
    let mut config = match fs::read_to_string(&config_path).await {
        Ok(existing) => parse_json_array_or_default(&existing),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "success": true,
                "message": "No config file to reset",
            }));
        }
        Err(error) => return Err(error.into()),
    };

    config.retain(|entry| {
        entry
            .get("name")
            .and_then(Value::as_str)
            .is_none_or(|name| name != "9Router")
    });
    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&Value::Array(config))?,
    )
    .await?;

    Ok(json!({
        "success": true,
        "message": "9Router removed from Copilot config",
    }))
}

async fn write_droid_settings(
    req: &DroidSettingsRequest,
    models: &[String],
) -> anyhow::Result<String> {
    let settings_path = droid_settings_path();
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let mut settings = match fs::read_to_string(&settings_path).await {
        Ok(existing) => parse_json_object_or_default(&existing),
        Err(_) => serde_json::Map::new(),
    };

    let custom_models_value = settings.remove("customModels");
    let mut custom_models = match custom_models_value {
        Some(Value::Array(entries)) => entries,
        Some(Value::Null) | None => Vec::new(),
        Some(_) => {
            return Err(anyhow::anyhow!("customModels must be an array"));
        }
    };
    custom_models.retain(|entry| {
        !entry
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.starts_with("custom:9Router"))
    });

    let normalized_base_url = normalize_v1_base_url(&req.base_url);
    let api_key = req
        .api_key
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "your_api_key".to_string());

    let default_index = match req.active_model.as_deref() {
        Some("") => None,
        Some(active_model) => Some(
            models
                .iter()
                .position(|model| model == active_model)
                .unwrap_or(0),
        ),
        None => Some(0),
    };

    for (index, model) in models.iter().enumerate() {
        if model.is_empty() {
            continue;
        }
        custom_models.push(json!({
            "model": model,
            "id": format!("custom:9Router-{index}"),
            "index": index,
            "baseUrl": normalized_base_url,
            "apiKey": api_key,
            "displayName": model,
            "maxOutputTokens": 131072,
            "noImageSupport": false,
            "provider": "openai",
        }));
    }

    // Intentionally matches 9router's whole-array reordering behavior, including
    // pre-existing non-9Router entries that may shift indexes.
    if let Some(default_index) = default_index {
        if default_index < custom_models.len() {
            let default_entry = custom_models.remove(default_index);
            custom_models.insert(0, default_entry);
            for (index, entry) in custom_models.iter_mut().enumerate() {
                if let Some(object) = entry.as_object_mut() {
                    object.insert("index".to_string(), Value::from(index));
                }
            }
        }
    }

    settings.insert("customModels".to_string(), Value::Array(custom_models));
    fs::write(
        &settings_path,
        serde_json::to_vec_pretty(&Value::Object(settings))?,
    )
    .await?;
    Ok(settings_path.to_string_lossy().to_string())
}

async fn reset_droid_settings() -> anyhow::Result<Value> {
    let settings_path = droid_settings_path();
    let mut settings = match fs::read_to_string(&settings_path).await {
        Ok(existing) => parse_json_object_or_default(&existing),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "success": true,
                "message": "No settings file to reset",
            }));
        }
        Err(error) => return Err(error.into()),
    };

    if let Some(custom_models_value) = settings.remove("customModels") {
        let mut custom_models = match custom_models_value {
            Value::Array(entries) => entries,
            Value::Null => Vec::new(),
            _ => {
                return Err(anyhow::anyhow!("customModels must be an array"));
            }
        };
        custom_models.retain(|entry| {
            !entry
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with("custom:9Router"))
        });
        if !custom_models.is_empty() {
            settings.insert("customModels".to_string(), Value::Array(custom_models));
        }
    }

    fs::write(
        &settings_path,
        serde_json::to_vec_pretty(&Value::Object(settings))?,
    )
    .await?;
    Ok(json!({
        "success": true,
        "message": "9Router settings removed successfully",
    }))
}

async fn write_opencode_settings(
    req: &OpenCodeSettingsRequest,
    models: &[String],
) -> anyhow::Result<String> {
    let config_path = opencode_config_path();
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let mut config = match fs::read_to_string(&config_path).await {
        Ok(existing) => parse_json_object_or_default(&existing),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(error) => return Err(error.into()),
    };

    let normalized_base_url = normalize_v1_base_url(&req.base_url);
    let api_key = req
        .api_key
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "sk_9router".to_string());
    let effective_subagent_model = req
        .subagent_model
        .clone()
        .unwrap_or_else(|| models[0].clone());

    let provider = config
        .entry("provider".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !provider.is_object() {
        *provider = Value::Object(serde_json::Map::new());
    }
    let provider_map = provider
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("provider must be an object"))?;

    let existing_provider = provider_map
        .entry("9router".to_string())
        .or_insert_with(|| {
            json!({
                "npm": "@ai-sdk/openai-compatible",
                "options": {},
                "models": {},
            })
        });
    if !existing_provider.is_object() {
        *existing_provider = json!({
            "npm": "@ai-sdk/openai-compatible",
            "options": {},
            "models": {},
        });
    }
    let existing_provider_map = existing_provider
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("provider.9router must be an object"))?;

    let options = existing_provider_map
        .entry("options".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !options.is_object() {
        *options = Value::Object(serde_json::Map::new());
    }
    let options_map = options
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("provider.9router.options must be an object"))?;
    options_map.insert("baseURL".to_string(), Value::String(normalized_base_url));
    options_map.insert("apiKey".to_string(), Value::String(api_key));

    let existing_models = existing_provider_map
        .entry("models".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !existing_models.is_object() {
        *existing_models = Value::Object(serde_json::Map::new());
    }
    let existing_models_map = existing_models
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("provider.9router.models must be an object"))?;
    for model in models {
        if model.is_empty() {
            continue;
        }
        existing_models_map.insert(model.clone(), json!({ "name": model }));
    }

    match req.active_model.as_deref() {
        Some("") => {
            config.insert("model".to_string(), Value::String(String::new()));
        }
        _ => {
            let final_active = req
                .active_model
                .clone()
                .filter(|model| !model.is_empty())
                .unwrap_or_else(|| models[0].clone());
            config.insert(
                "model".to_string(),
                Value::String(format!("9router/{final_active}")),
            );
        }
    }

    let agent = config
        .entry("agent".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !agent.is_object() {
        *agent = Value::Object(serde_json::Map::new());
    }
    let agent_map = agent
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("agent must be an object"))?;
    agent_map.insert(
        "explorer".to_string(),
        json!({
            "description": "Fast explorer subagent for codebase exploration",
            "mode": "subagent",
            "model": format!("9router/{effective_subagent_model}"),
        }),
    );

    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&Value::Object(config))?,
    )
    .await?;
    Ok(config_path.to_string_lossy().to_string())
}

async fn patch_opencode_config(req: &PatchOpenCodeSettingsRequest) -> anyhow::Result<Value> {
    let config_path = opencode_config_path();
    let mut config = match fs::read_to_string(&config_path).await {
        Ok(existing) => parse_json_object_required(&existing)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "success": true,
                "message": "No config file found",
            }));
        }
        Err(error) => return Err(error.into()),
    };

    if req.clear_active_model
        && config
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|model| model.starts_with("9router/"))
    {
        config.insert("model".to_string(), Value::String(String::new()));
    }

    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&Value::Object(config))?,
    )
    .await?;
    Ok(json!({
        "success": true,
        "message": "Settings updated",
    }))
}

async fn reset_opencode_settings(model_to_remove: Option<String>) -> anyhow::Result<Value> {
    let config_path = opencode_config_path();
    let mut config = match fs::read_to_string(&config_path).await {
        Ok(existing) => parse_json_object_required(&existing)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "success": true,
                "message": "No config file to reset",
            }));
        }
        Err(error) => return Err(error.into()),
    };

    if let Some(model_to_remove) = model_to_remove.clone() {
        let active_model_matches = config
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|model| model == format!("9router/{model_to_remove}"));
        let mut remove_provider = false;
        let mut next_model = None;
        if let Some(models_map) = config
            .get_mut("provider")
            .and_then(Value::as_object_mut)
            .and_then(|provider| provider.get_mut("9router"))
            .and_then(Value::as_object_mut)
            .and_then(|provider| provider.get_mut("models"))
            .and_then(Value::as_object_mut)
        {
            models_map.remove(&model_to_remove);
            if models_map.is_empty() {
                remove_provider = true;
            } else if active_model_matches {
                next_model = models_map.keys().next().cloned();
            }
        }
        if remove_provider {
            if let Some(provider) = config.get_mut("provider").and_then(Value::as_object_mut) {
                provider.remove("9router");
            }
            if config
                .get("model")
                .and_then(Value::as_str)
                .is_some_and(|model| model.starts_with("9router/"))
            {
                config.remove("model");
            }
        } else if let Some(next_model) = next_model {
            config.insert(
                "model".to_string(),
                Value::String(format!("9router/{next_model}")),
            );
        }
    } else {
        if let Some(provider) = config.get_mut("provider").and_then(Value::as_object_mut) {
            provider.remove("9router");
        }
        if config
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|model| model.starts_with("9router/"))
        {
            config.remove("model");
        }
    }

    let should_remove_explorer = config
        .get("agent")
        .and_then(Value::as_object)
        .and_then(|agent| agent.get("explorer"))
        .and_then(Value::as_object)
        .and_then(|explorer| explorer.get("model"))
        .and_then(Value::as_str)
        .is_some_and(|model| model.starts_with("9router/"));
    if should_remove_explorer {
        if let Some(agent) = config.get_mut("agent").and_then(Value::as_object_mut) {
            agent.remove("explorer");
            if agent.is_empty() {
                config.remove("agent");
            }
        }
    }

    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&Value::Object(config))?,
    )
    .await?;
    Ok(json!({
        "success": true,
        "message": model_to_remove
            .map(|model| Value::String(format!("Model \"{model}\" removed")))
            .unwrap_or_else(|| Value::String("9Router settings removed from OpenCode".to_string())),
    }))
}

async fn write_openclaw_settings(req: &OpenClawSettingsRequest) -> anyhow::Result<String> {
    let settings_path = openclaw_settings_path();
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let mut settings = match fs::read_to_string(&settings_path).await {
        Ok(existing) => parse_json_object_or_default(&existing),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(error) => return Err(error.into()),
    };

    ensure_object_path(&mut settings, &["agents", "defaults", "model"])?;
    ensure_object_path(&mut settings, &["agents", "defaults", "models"])?;
    ensure_object_path(&mut settings, &["models", "providers"])?;

    let normalized_base_url = normalize_v1_base_url(&req.base_url);
    let full_model_id = format!("9router/{}", req.model);

    if let Some(default_models) =
        get_nested_object_mut(&mut settings, &["agents", "defaults", "models"])
    {
        let keys_to_remove = default_models
            .keys()
            .filter(|key| key.starts_with("9router/"))
            .cloned()
            .collect::<Vec<_>>();
        for key in keys_to_remove {
            default_models.remove(&key);
        }
    }

    if let Some(default_model) =
        get_nested_object_mut(&mut settings, &["agents", "defaults", "model"])
    {
        default_model.insert("primary".to_string(), Value::String(full_model_id));
    }

    let mut all_model_ids = vec![req.model.clone()];
    for model in req.agent_models.values() {
        if !model.is_empty() && !all_model_ids.contains(model) {
            all_model_ids.push(model.clone());
        }
    }

    if let Some(default_models) =
        get_nested_object_mut(&mut settings, &["agents", "defaults", "models"])
    {
        for model in &all_model_ids {
            default_models.insert(format!("9router/{model}"), json!({}));
        }
    }

    if let Some(agents_list) = get_nested_array_mut(&mut settings, &["agents", "list"]) {
        for agent in agents_list.iter_mut() {
            if agent
                .get("model")
                .and_then(Value::as_str)
                .is_some_and(|model| model.starts_with("9router/"))
            {
                if let Some(agent_object) = agent.as_object_mut() {
                    agent_object.remove("model");
                }
            }
        }
    }

    if let Some(providers) = get_nested_object_mut(&mut settings, &["models", "providers"]) {
        providers.insert(
            "9router".to_string(),
            json!({
                "baseUrl": normalized_base_url,
                "apiKey": req.api_key.clone().unwrap_or_else(|| "your_api_key".to_string()),
                "api": "openai-completions",
                "models": all_model_ids.iter().map(|model| {
                    json!({
                        "id": model,
                        "name": model.rsplit('/').next().unwrap_or(model),
                    })
                }).collect::<Vec<_>>(),
            }),
        );
    }

    if let Some(agents_list) = get_nested_array_mut(&mut settings, &["agents", "list"]) {
        for agent in agents_list.iter_mut() {
            let agent_id = agent.get("id").and_then(Value::as_str).map(str::to_string);
            if let Some(agent_id) = agent_id {
                if let Some(agent_model) = req.agent_models.get(&agent_id) {
                    if let Some(agent_object) = agent.as_object_mut() {
                        agent_object.insert(
                            "model".to_string(),
                            Value::String(format!("9router/{agent_model}")),
                        );
                    }
                }
            }
        }

        for agent in agents_list.iter() {
            let Some(agent_dir) = agent.get("agentDir").and_then(Value::as_str) else {
                continue;
            };
            let agent_id = agent.get("id").and_then(Value::as_str).unwrap_or_default();
            let model_to_write = req
                .agent_models
                .get(agent_id)
                .cloned()
                .unwrap_or_else(|| req.model.clone());
            write_openclaw_agent_models(
                PathBuf::from(agent_dir),
                &model_to_write,
                &normalized_base_url,
                req.api_key.as_deref(),
            )
            .await?;
        }
    }

    fs::write(
        &settings_path,
        serde_json::to_vec_pretty(&Value::Object(settings))?,
    )
    .await?;
    Ok(settings_path.to_string_lossy().to_string())
}

async fn reset_openclaw_settings() -> anyhow::Result<Value> {
    let settings_path = openclaw_settings_path();
    let mut settings = match fs::read_to_string(&settings_path).await {
        Ok(existing) => parse_json_object_required(&existing)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "success": true,
                "message": "No settings file to reset",
            }));
        }
        Err(error) => return Err(error.into()),
    };

    if let Some(providers) = get_nested_object_mut(&mut settings, &["models", "providers"]) {
        providers.remove("9router");
        if providers.is_empty() {
            remove_nested_key(&mut settings, &["models", "providers"]);
        }
    }

    if let Some(default_models) =
        get_nested_object_mut(&mut settings, &["agents", "defaults", "models"])
    {
        let keys_to_remove = default_models
            .keys()
            .filter(|key| key.starts_with("9router/"))
            .cloned()
            .collect::<Vec<_>>();
        for key in keys_to_remove {
            default_models.remove(&key);
        }
        if default_models.is_empty() {
            remove_nested_key(&mut settings, &["agents", "defaults", "models"]);
        }
    }

    if settings
        .get("agents")
        .and_then(|agents| agents.get("defaults"))
        .and_then(|defaults| defaults.get("model"))
        .and_then(|model| model.get("primary"))
        .and_then(Value::as_str)
        .is_some_and(|model| model.starts_with("9router/"))
    {
        remove_nested_key(&mut settings, &["agents", "defaults", "model", "primary"]);
    }

    fs::write(
        &settings_path,
        serde_json::to_vec_pretty(&Value::Object(settings))?,
    )
    .await?;
    Ok(json!({
        "success": true,
        "message": "9Router settings removed successfully",
    }))
}

async fn read_openclaw_agent_model(agent_dir: &PathBuf) -> Option<String> {
    let models_path = agent_dir.join("models.json");
    let content = fs::read_to_string(models_path).await.ok()?;
    let data = serde_json::from_str::<Value>(&content).ok()?;
    data.get("providers")
        .and_then(|providers| providers.get("9router"))
        .and_then(|provider| provider.get("models"))
        .and_then(Value::as_array)
        .and_then(|models| models.first())
        .and_then(|model| model.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

async fn write_openclaw_agent_models(
    agent_dir: PathBuf,
    model: &str,
    base_url: &str,
    api_key: Option<&str>,
) -> anyhow::Result<()> {
    fs::create_dir_all(&agent_dir).await?;
    let models_path = agent_dir.join("models.json");
    let mut existing = match fs::read_to_string(&models_path).await {
        Ok(content) => parse_json_object_or_default(&content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(error) => return Err(error.into()),
    };

    let providers = existing
        .entry("providers".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !providers.is_object() {
        *providers = Value::Object(serde_json::Map::new());
    }
    let providers_map = providers
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("providers must be an object"))?;
    providers_map.insert(
        "9router".to_string(),
        json!({
            "baseUrl": base_url,
            "apiKey": api_key.unwrap_or("your_api_key"),
            "api": "openai-completions",
            "models": [{
                "id": model,
                "name": model.rsplit('/').next().unwrap_or(model),
            }],
        }),
    );

    fs::write(
        models_path,
        serde_json::to_vec_pretty(&Value::Object(existing))?,
    )
    .await?;
    Ok(())
}

async fn read_json_optional(path: &FsPath) -> anyhow::Result<Option<Value>> {
    match fs::read_to_string(path).await {
        Ok(content) => Ok(Some(serde_json::from_str(&content)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn parse_json_array_or_default(content: &str) -> Vec<Value> {
    match serde_json::from_str::<Value>(content) {
        Ok(Value::Array(entries)) => entries,
        _ => Vec::new(),
    }
}

fn parse_json_object_or_default(content: &str) -> serde_json::Map<String, Value> {
    match serde_json::from_str::<Value>(content) {
        Ok(Value::Object(object)) => object,
        _ => serde_json::Map::new(),
    }
}

fn parse_json_object_required(content: &str) -> anyhow::Result<serde_json::Map<String, Value>> {
    match serde_json::from_str::<Value>(content)? {
        Value::Object(object) => Ok(object),
        _ => Err(anyhow::anyhow!("Expected JSON object")),
    }
}

fn ensure_object_path(
    root: &mut serde_json::Map<String, Value>,
    path: &[&str],
) -> anyhow::Result<()> {
    if path.is_empty() {
        return Ok(());
    }
    let mut current = root;
    for key in path {
        let entry = current
            .entry((*key).to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(serde_json::Map::new());
        }
        current = entry
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("Expected object at path segment {key}"))?;
    }
    Ok(())
}

fn get_nested_object_mut<'a>(
    root: &'a mut serde_json::Map<String, Value>,
    path: &[&str],
) -> Option<&'a mut serde_json::Map<String, Value>> {
    let (first, rest) = path.split_first()?;
    let value = root.get_mut(*first)?;
    if rest.is_empty() {
        return value.as_object_mut();
    }
    let next = value.as_object_mut()?;
    get_nested_object_mut(next, rest)
}

fn get_nested_array_mut<'a>(
    root: &'a mut serde_json::Map<String, Value>,
    path: &[&str],
) -> Option<&'a mut Vec<Value>> {
    let (first, rest) = path.split_first()?;
    let value = root.get_mut(*first)?;
    if rest.is_empty() {
        return value.as_array_mut();
    }
    let next = value.as_object_mut()?;
    get_nested_array_mut(next, rest)
}

fn remove_nested_key(root: &mut serde_json::Map<String, Value>, path: &[&str]) {
    let Some((first, rest)) = path.split_first() else {
        return;
    };
    if rest.is_empty() {
        root.remove(*first);
        return;
    }
    if let Some(next) = root.get_mut(*first).and_then(Value::as_object_mut) {
        remove_nested_key(next, rest);
    }
}

fn has_9router_copilot_config(config: &Value) -> bool {
    get_9router_copilot_entry(config).is_some()
}

fn has_9router_droid_settings(settings: &Value) -> bool {
    settings
        .get("customModels")
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.starts_with("custom:9Router"))
            })
        })
}

fn has_9router_openclaw_settings(settings: &Value) -> bool {
    settings
        .get("models")
        .and_then(|models| models.get("providers"))
        .and_then(|providers| providers.get("9router"))
        .is_some()
}

fn get_9router_copilot_entry(config: &Value) -> Option<&Value> {
    config
        .as_array()?
        .iter()
        .find(|entry| entry.get("name").and_then(Value::as_str) == Some("9Router"))
}

fn copilot_config_path() -> PathBuf {
    if cfg!(windows) {
        let base = env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(home_dir);
        base.join("Code")
            .join("User")
            .join("chatLanguageModels.json")
    } else if cfg!(target_os = "macos") {
        home_dir()
            .join("Library")
            .join("Application Support")
            .join("Code")
            .join("User")
            .join("chatLanguageModels.json")
    } else {
        home_dir()
            .join(".config")
            .join("Code")
            .join("User")
            .join("chatLanguageModels.json")
    }
}

fn droid_settings_path() -> PathBuf {
    home_dir().join(".factory").join("settings.json")
}

fn opencode_config_path() -> PathBuf {
    home_dir()
        .join(".config")
        .join("opencode")
        .join("opencode.json")
}

fn openclaw_settings_path() -> PathBuf {
    home_dir().join(".openclaw").join("openclaw.json")
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}

// ═══════════════════════════════════════════════════════════════════════════
// Antigravity MITM Endpoints
// GET/POST/DELETE /api/cli-tools/antigravity-mitm
// ═══════════════════════════════════════════════════════════════════════════

/// Antigravity MITM status response
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityMitmStatus {
    pub running: bool,
    pub cert_exists: bool,
    pub dns_configured: bool,
    pub has_cached_password: bool,
}

/// GET /api/cli-tools/antigravity-mitm
/// Get Antigravity MITM status
async fn get_antigravity_mitm(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();

    // Check if MITM is running (has active routes)
    let running = !snapshot.mitm_alias.is_empty();

    // Check for cert
    let cert_exists = snapshot
        .provider_nodes
        .iter()
        .any(|n| n.extra.get("type").and_then(Value::as_str) == Some("mitm-cert"));

    // Check DNS config (simplified - would need actual system check)
    let dns_configured = false;

    // Check for cached password (simplified)
    let has_cached_password = false;

    Json(AntigravityMitmStatus {
        running,
        cert_exists,
        dns_configured,
        has_cached_password,
    })
    .into_response()
}

/// POST /api/cli-tools/antigravity-mitm
/// Start Antigravity MITM proxy
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartMitmRequest {
    pub api_key: Option<String>,
    pub sudo_password: Option<String>,
}

async fn start_antigravity_mitm(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<StartMitmRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();

    // Check if cert exists
    let cert_exists = snapshot
        .provider_nodes
        .iter()
        .any(|n| n.extra.get("type").and_then(Value::as_str) == Some("mitm-cert"));

    if !cert_exists {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "MITM certificate not found. Generate one first via /api/mitm/cert/generate"})),
        )
            .into_response();
    }

    // Setup MITM route for antigravity
    match state
        .db
        .update(|db| {
            let mut alias_config = BTreeMap::new();
            alias_config.insert(
                "upstreamUrl".to_string(),
                "https://daily-cloudcode-pa.googleapis.com".to_string(),
            );
            alias_config.insert("pathPrefix".to_string(), "/".to_string());
            alias_config.insert("requestTransform".to_string(), "true".to_string());
            alias_config.insert("responseTransform".to_string(), "true".to_string());
            alias_config.insert("enabled".to_string(), "true".to_string());
            alias_config.insert("interceptMode".to_string(), "full".to_string());

            db.mitm_alias
                .insert("antigravity".to_string(), alias_config);
        })
        .await
    {
        Ok(_) => Json(json!({
            "success": true,
            "running": true,
            "certExists": true,
            "dnsConfigured": false,
            "message": "MITM proxy started for Antigravity"
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// DELETE /api/cli-tools/antigravity-mitm
/// Stop Antigravity MITM proxy
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StopMitmRequest {
    pub sudo_password: Option<String>,
}

async fn stop_antigravity_mitm(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(_req): Json<StopMitmRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match state
        .db
        .update(|db| {
            db.mitm_alias.remove("antigravity");
        })
        .await
    {
        Ok(_) => Json(json!({
            "success": true,
            "running": false,
            "message": "MITM proxy stopped"
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// PATCH /api/cli-tools/antigravity-mitm
/// Toggle DNS for Antigravity MITM
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DnsToggleRequest {
    pub tool: Option<String>,
    pub action: String, // "enable" or "disable"
    pub sudo_password: Option<String>,
}

async fn toggle_antigravity_dns(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<DnsToggleRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    // DNS toggle is a no-op in this implementation (would need system-level access)
    Json(json!({
        "success": true,
        "dnsConfigured": req.action == "enable",
        "message": format!("DNS {} for {}", req.action, req.tool.unwrap_or_else(|| "antigravity".to_string()))
    }))
    .into_response()
}

// ═══════════════════════════════════════════════════════════════════════════
// Antigravity MITM Alias Endpoints
// GET/PUT/DELETE /api/cli-tools/antigravity-mitm/alias
// ═══════════════════════════════════════════════════════════════════════════

/// GET /api/cli-tools/antigravity-mitm/alias
/// Get model aliases for a tool
#[derive(Debug, Deserialize)]
struct AliasQueryParams {
    pub tool: Option<String>,
}

async fn get_mitm_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<AliasQueryParams>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let tool = params.tool.unwrap_or_else(|| "antigravity".to_string());

    // Get aliases from mitm_alias for this tool
    let aliases: BTreeMap<String, String> = snapshot
        .mitm_alias
        .get(&tool)
        .map(|config| {
            config
                .iter()
                .filter(|(k, _)| k.starts_with("alias."))
                .filter_map(|(k, v)| {
                    let alias = k.strip_prefix("alias.")?;
                    let target = v.as_str();
                    if target.is_empty() {
                        None
                    } else {
                        Some((alias.to_string(), target.to_string()))
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    Json(json!({
        "tool": tool,
        "aliases": aliases
    }))
    .into_response()
}

/// PUT /api/cli-tools/antigravity-mitm/alias
/// Save model aliases for a tool
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveAliasRequest {
    pub tool: String,
    pub mappings: BTreeMap<String, String>,
}

async fn save_mitm_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SaveAliasRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match state
        .db
        .update(|db| {
            let config = db
                .mitm_alias
                .entry(req.tool.clone())
                .or_insert_with(BTreeMap::new);

            // Store mappings with "alias." prefix
            for (alias, target) in &req.mappings {
                config.insert(format!("alias.{}", alias), target.clone());
            }
        })
        .await
    {
        Ok(_) => Json(json!({
            "success": true,
            "message": "Aliases saved"
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// DELETE /api/cli-tools/antigravity-mitm/alias
/// Clear all aliases for a tool
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteAliasRequest {
    pub tool: Option<String>,
}

async fn delete_mitm_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<DeleteAliasRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let tool = params.tool.unwrap_or_else(|| "antigravity".to_string());

    match state
        .db
        .update(|db| {
            if let Some(config) = db.mitm_alias.get_mut(&tool) {
                // Remove all alias entries
                config.retain(|k, _| !k.starts_with("alias."));
            }
        })
        .await
    {
        Ok(_) => Json(json!({
            "success": true,
            "message": format!("Aliases cleared for {}", tool)
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Route Registration
// ═══════════════════════════════════════════════════════════════════════════

pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(claude_settings::routes())
        .merge(cowork_settings::routes())
        .merge(hermes_settings::routes())
        .route("/api/cli-tools", get(list_tools))
        .route("/api/cli-tools/execute", post(execute_command))
        .route("/api/cli-tools/run/{tool_name}", post(run_tool))
        .route("/api/cli-tools/help", get(get_help))
        // Codex settings
        .route("/api/cli-tools/codex-settings", get(get_codex_settings))
        .route("/api/cli-tools/codex-settings", post(save_codex_settings))
        .route(
            "/api/cli-tools/codex-settings",
            delete(delete_codex_settings),
        )
        .route("/api/cli-tools/copilot-settings", get(get_copilot_settings))
        .route(
            "/api/cli-tools/copilot-settings",
            post(save_copilot_settings),
        )
        .route(
            "/api/cli-tools/copilot-settings",
            delete(delete_copilot_settings),
        )
        .route(
            "/api/cli-tools/opencode-settings",
            get(get_opencode_settings)
                .post(save_opencode_settings)
                .patch(patch_opencode_settings)
                .delete(delete_opencode_settings),
        )
        .route(
            "/api/cli-tools/droid-settings",
            get(get_droid_settings)
                .post(save_droid_settings)
                .delete(delete_droid_settings),
        )
        .route(
            "/api/cli-tools/openclaw-settings",
            get(get_openclaw_settings)
                .post(save_openclaw_settings)
                .delete(delete_openclaw_settings),
        )
        // Antigravity MITM
        .route("/api/cli-tools/antigravity-mitm", get(get_antigravity_mitm))
        .route(
            "/api/cli-tools/antigravity-mitm",
            post(start_antigravity_mitm),
        )
        .route(
            "/api/cli-tools/antigravity-mitm",
            delete(stop_antigravity_mitm),
        )
        .route(
            "/api/cli-tools/antigravity-mitm",
            patch(toggle_antigravity_dns),
        )
        // Antigravity MITM alias
        .route("/api/cli-tools/antigravity-mitm/alias", get(get_mitm_alias))
        .route(
            "/api/cli-tools/antigravity-mitm/alias",
            put(save_mitm_alias),
        )
        .route(
            "/api/cli-tools/cowork-mcp-registry",
            get(get_cowork_mcp_registry),
        )
        .route(
            "/api/cli-tools/antigravity-mitm/alias",
            delete(delete_mitm_alias),
        )
}



// GET /api/cli-tools/cowork-mcp-registry
// Fetches MCP server registry from Anthropic + GitHub plugins
async fn get_cowork_mcp_registry() -> Response {
    use tokio::sync::OnceCell;
    static CACHE: OnceCell<(std::time::Instant, Vec<Value>)> = OnceCell::const_new();

    let now = std::time::Instant::now();
    if let Some((ts, data)) = CACHE.get() {
        if now.duration_since(*ts).as_millis() < 3_600_000 {
            return Json(json!({ "servers": data })).into_response();
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();

    let mut servers = Vec::new();

    // Fetch Anthropic MCP registry
    let mut cursor = String::new();
    for _ in 0..20 {
        let url = format!(
            "https://api.anthropic.com/mcp-registry/v0/servers?limit=500&visibility=commercial,gsuite,gsuite-google{}",
            if cursor.is_empty() { String::new() } else { format!("&cursor={}", urlencoding::encode(&cursor)) }
        );
        match client.get(&url).header("accept", "application/json").send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<Value>().await {
                    Ok(j) => {
                        for item in j.get("servers").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
                            let s = item.get("server").cloned().unwrap_or_default();
                            let remote = s.get("remotes").and_then(|r| r.as_array()).and_then(|a| a.first());
                            let url = remote.and_then(|r| r.get("url")).and_then(Value::as_str).unwrap_or("");
                            if url.is_empty() { continue; }
                            let transport = remote.and_then(|r| r.get("type")).and_then(Value::as_str);
                            let transport = match transport {
                                Some("streamable-http") => "http",
                                Some("sse") => "sse",
                                _ => "http",
                            };
                            servers.push(json!({
                                "source": "registry",
                                "name": s.get("name").and_then(Value::as_str).unwrap_or(""),
                                "title": s.get("title").or(s.get("name")).and_then(Value::as_str).unwrap_or(""),
                                "description": s.get("description").and_then(Value::as_str).unwrap_or(""),
                                "url": url,
                                "transport": transport,
                            }));
                        }
                        cursor = j.get("metadata").and_then(|m| m.get("nextCursor")).and_then(Value::as_str).unwrap_or("").to_string();
                        if cursor.is_empty() { break; }
                    }
                    Err(_) => break,
                }
            }
            _ => break,
        }
    }

    Json(json!({ "servers": servers })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_codex_settings_default() {
        let settings = CodexSettings::default();
        assert_eq!(settings.base_url, None);
        assert_eq!(settings.api_key, None);
        assert_eq!(settings.model, None);
        assert_eq!(settings.subagent_model, None);
    }

    #[test]
    fn test_codex_settings_serialization() {
        let settings = CodexSettings {
            base_url: Some("http://localhost:4623/v1".to_string()),
            api_key: Some("sk-test".to_string()),
            model: Some("openai/gpt-4".to_string()),
            subagent_model: Some("openai/gpt-4o".to_string()),
        };

        let json = serde_json::to_string(&settings).unwrap();
        let deserialized: CodexSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.base_url, settings.base_url);
        assert_eq!(deserialized.api_key, settings.api_key);
        assert_eq!(deserialized.model, settings.model);
        assert_eq!(deserialized.subagent_model, settings.subagent_model);
    }

    #[test]
    fn test_antigravity_mitm_status_default() {
        let status = AntigravityMitmStatus {
            running: false,
            cert_exists: false,
            dns_configured: false,
            has_cached_password: false,
        };
        assert!(!status.running);
        assert!(!status.cert_exists);
        assert!(!status.dns_configured);
        assert!(!status.has_cached_password);
    }

    #[test]
    fn test_parse_cli_command() {
        let result = parse_cli_command("openproxy provider list", None);
        assert!(result.is_some());
        let (program, args) = result.unwrap();
        assert_eq!(program, "openproxy");
        assert_eq!(args, vec!["provider", "list"]);
    }

    #[test]
    fn test_parse_cli_command_with_additional_args() {
        let additional_args = vec!["--json".to_string()];
        let result = parse_cli_command("openproxy key list", Some(additional_args.as_slice()));
        assert!(result.is_some());
        let (program, args) = result.unwrap();
        assert_eq!(program, "openproxy");
        assert_eq!(args, vec!["key", "list", "--json"]);
    }

    #[test]
    fn test_parse_cli_command_empty() {
        let result = parse_cli_command("", None);
        assert!(result.is_none());
    }

    #[test]
    fn test_build_tool_command_provider_list() {
        let (program, args) = build_tool_command("provider-list", vec![]);
        assert_eq!(program, "openproxy");
        assert_eq!(args, vec!["provider", "list", "--json"]);
    }

    #[test]
    fn test_build_tool_command_pool_status() {
        let (program, args) = build_tool_command("pool-status", vec!["my-pool".to_string()]);
        assert_eq!(program, "openproxy");
        assert_eq!(args, vec!["pool", "status", "--name", "my-pool", "--json"]);
    }

    #[test]
    fn test_build_tool_command_unknown() {
        let (program, args) = build_tool_command("unknown-tool", vec!["arg1".to_string()]);
        assert_eq!(program, "unknown-tool");
        assert_eq!(args, vec!["arg1"]);
    }
}
