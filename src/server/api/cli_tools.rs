use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    response::Response,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::process::Command;

use crate::server::auth::require_api_key;
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
pub async fn list_tools(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = require_api_key(&headers, &state.db) {
        return crate::server::api::auth_error_response(e);
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
    if let Err(e) = require_api_key(&headers, &state.db) {
        return crate::server::api::auth_error_response(e);
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
    if let Err(e) = require_api_key(&headers, &state.db) {
        return crate::server::api::auth_error_response(e);
    }

    let timeout_secs = req.timeout_secs.unwrap_or(30).min(120);
    let start_time = std::time::Instant::now();

    let (program, args) = build_tool_command(&tool_name, req.args.unwrap_or_default());
    let duration_ms = start_time.elapsed().as_millis() as u64;

    let output = match Command::new(&program)
        .args(&args)
        .output()
        .await
    {
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
fn parse_cli_command(command: &str, additional_args: Option<&[String]>) -> Option<(String, Vec<String>)> {
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
        "provider-list" => ("openproxy".to_string(), vec!["provider".to_string(), "list".to_string(), "--json".to_string()]),
        "key-list" => ("openproxy".to_string(), vec!["key".to_string(), "list".to_string(), "--json".to_string()]),
        "pool-list" => ("openproxy".to_string(), vec!["pool".to_string(), "list".to_string(), "--json".to_string()]),
        "pool-status" => {
            let pool_name = args.first().cloned().unwrap_or_default();
            ("openproxy".to_string(), vec!["pool".to_string(), "status".to_string(), "--name".to_string(), pool_name, "--json".to_string()])
        }
        _ => (tool_name.to_string(), args),
    }
}

/// GET /api/cli-tools/help
/// Get help information for CLI tools
pub async fn get_help(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = require_api_key(&headers, &state.db) {
        return crate::server::api::auth_error_response(e);
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

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/cli-tools", get(list_tools))
        .route("/api/cli-tools/execute", post(execute_command))
        .route("/api/cli-tools/run/{tool_name}", post(run_tool))
        .route("/api/cli-tools/help", get(get_help))
}