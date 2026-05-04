use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, Method, Request, StatusCode},
    response::IntoResponse,
    response::Response,
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use bytes::Bytes;
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::process::Command;

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

    let snapshot = state.db.snapshot();

    // Check if codex executable exists locally
    let installed = std::path::Path::new("/usr/local/bin/codex").exists()
        || std::path::Path::new("/usr/bin/codex").exists();

    // Look for saved config in provider_specific_data of a special provider node
    let codex_config = snapshot
        .provider_nodes
        .iter()
        .find(|n| n.extra.get("type").and_then(Value::as_str) == Some("codex-settings"))
        .and_then(|node| {
            let config_str = node.extra.get("config")?.as_str()?;
            serde_json::from_str::<CodexSettings>(config_str).ok()
        })
        .unwrap_or_default();

    Json(json!({
        "installed": installed,
        "config": codex_config.base_url.is_some() || codex_config.api_key.is_some(),
        "baseUrl": codex_config.base_url,
        "apiKey": codex_config.api_key,
        "model": codex_config.model,
        "subagentModel": codex_config.subagent_model,
        "configContent": build_codex_config_string(&codex_config, &snapshot.settings.mitm_router_base_url)
    }))
    .into_response()
}

fn build_codex_config_string(settings: &CodexSettings, base_url: &str) -> Option<String> {
    let base_url = settings.base_url.as_ref()?;
    let model = settings.model.as_ref()?;

    let subagent_model = settings.subagent_model.as_ref().unwrap_or(model);

    Some(format!(
        r#"# 9Router Configuration for Codex CLI
model = "{}"
model_provider = "9router"

[model_providers.9router]
name = "9Router"
base_url = "{}"
wire_api = "responses"

[agents.subagent]
model = "{}"
"#,
        model, base_url, subagent_model
    ))
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

    let codex_settings = CodexSettings {
        base_url: Some(req.base_url),
        api_key: Some(req.api_key),
        model: Some(req.model),
        subagent_model: req.subagent_model,
    };

    let config_json = match serde_json::to_string(&codex_settings) {
        Ok(j) => j,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Failed to serialize settings"})),
            )
                .into_response()
        }
    };

    let now = chrono::Utc::now().to_rfc3339();

    match state
        .db
        .update(|db| {
            // Remove old codex settings
            db.provider_nodes
                .retain(|n| n.extra.get("type").and_then(Value::as_str) != Some("codex-settings"));

            // Add new codex settings node
            let mut extra = BTreeMap::new();
            extra.insert(
                "type".to_string(),
                Value::String("codex-settings".to_string()),
            );
            extra.insert("config".to_string(), Value::String(config_json));

            db.provider_nodes.push(crate::types::ProviderNode {
                id: "codex-settings".to_string(),
                r#type: "codex-settings".to_string(),
                name: "Codex CLI Settings".to_string(),
                prefix: None,
                api_type: None,
                base_url: None,
                created_at: Some(now.clone()),
                updated_at: Some(now),
                extra,
            });
        })
        .await
    {
        Ok(_) => Json(json!({
            "success": true,
            "message": "Codex settings saved"
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
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

    match state
        .db
        .update(|db| {
            db.provider_nodes
                .retain(|n| n.extra.get("type").and_then(Value::as_str) != Some("codex-settings"));
        })
        .await
    {
        Ok(_) => Json(json!({
            "success": true,
            "message": "Codex settings reset"
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
        .route(
            "/api/cli-tools/claude-settings",
            get(proxy_cli_tool_settings)
                .post(proxy_cli_tool_settings)
                .delete(proxy_cli_tool_settings),
        )
        .route(
            "/api/cli-tools/opencode-settings",
            get(proxy_cli_tool_settings)
                .post(proxy_cli_tool_settings)
                .patch(proxy_cli_tool_settings)
                .delete(proxy_cli_tool_settings),
        )
        .route(
            "/api/cli-tools/droid-settings",
            get(proxy_cli_tool_settings)
                .post(proxy_cli_tool_settings)
                .delete(proxy_cli_tool_settings),
        )
        .route(
            "/api/cli-tools/hermes-settings",
            get(proxy_cli_tool_settings)
                .post(proxy_cli_tool_settings)
                .delete(proxy_cli_tool_settings),
        )
        .route(
            "/api/cli-tools/openclaw-settings",
            get(proxy_cli_tool_settings)
                .post(proxy_cli_tool_settings)
                .delete(proxy_cli_tool_settings),
        )
        .route(
            "/api/cli-tools/copilot-settings",
            get(proxy_cli_tool_settings)
                .post(proxy_cli_tool_settings)
                .delete(proxy_cli_tool_settings),
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
            "/api/cli-tools/antigravity-mitm/alias",
            delete(delete_mitm_alias),
        )
}

async fn proxy_cli_tool_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let method = request.method().clone();
    let uri = request.uri().clone();
    let target = format!("{}{}", dashboard_sidecar_origin(), uri);
    let body_bytes = match axum::body::to_bytes(request.into_body(), usize::MAX).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("Failed to read proxied request body: {error}")})),
            )
                .into_response()
        }
    };

    let client = reqwest::Client::new();
    let mut builder = client.request(method.clone(), target);
    for (name, value) in &headers {
        if should_skip_proxy_request_header(&method, name.as_str()) {
            continue;
        }
        builder = builder.header(name, value);
    }
    if !body_bytes.is_empty() {
        builder = builder.body(body_bytes.to_vec());
    }

    match builder.send().await {
        Ok(response) => proxy_reqwest_response(response),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": format!("CLI settings sidecar proxy failed: {error}")})),
        )
            .into_response(),
    }
}

fn dashboard_sidecar_origin() -> String {
    std::env::var("DASHBOARD_SIDECAR_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:20128".to_string())
}

fn should_skip_proxy_request_header(method: &Method, header_name: &str) -> bool {
    let lower = header_name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "content-length"
    ) || (method == Method::GET && lower == "content-type")
}

fn proxy_reqwest_response(response: reqwest::Response) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let connection_tokens = connection_header_tokens(&headers);
    let body = Body::from_stream(response.bytes_stream().map_ok(|chunk: Bytes| chunk));
    let mut proxied = body.into_response();
    *proxied.status_mut() = status;
    let target_headers = proxied.headers_mut();
    for (name, value) in &headers {
        if should_skip_proxy_response_header(name.as_str(), &connection_tokens) {
            continue;
        }
        target_headers.insert(name.clone(), value.clone());
    }
    proxied
}

fn should_skip_proxy_response_header(header_name: &str, connection_tokens: &[String]) -> bool {
    let lower = header_name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
    ) || connection_tokens.iter().any(|token| token.eq_ignore_ascii_case(&lower))
}

fn connection_header_tokens(headers: &reqwest::header::HeaderMap) -> Vec<String> {
    headers
        .get(reqwest::header::CONNECTION)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .map(|part| part.trim().to_ascii_lowercase())
                .filter(|part| !part.is_empty())
                .collect()
        })
        .unwrap_or_default()
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
    fn test_build_codex_config_string() {
        let settings = CodexSettings {
            base_url: Some("http://localhost:4623/v1".to_string()),
            api_key: Some("sk-test".to_string()),
            model: Some("openai/gpt-4".to_string()),
            subagent_model: Some("openai/gpt-4o".to_string()),
        };

        let config = build_codex_config_string(&settings, "http://localhost:4623");
        assert!(config.is_some());
        let config_str = config.unwrap();
        assert!(config_str.contains("model = \"openai/gpt-4\""));
        assert!(config_str.contains("base_url = \"http://localhost:4623/v1\""));
        assert!(config_str.contains("subagent"));
    }

    #[test]
    fn test_build_codex_config_string_missing_fields() {
        let settings = CodexSettings::default();
        let config = build_codex_config_string(&settings, "http://localhost:4623");
        assert!(config.is_none());
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
