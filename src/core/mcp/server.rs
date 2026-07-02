//! Native MCP protocol handler for OpenProxy.
//!
//! Implements the JSON-RPC 2.0-based Model Context Protocol so external MCP
//! clients (Claude Desktop, Cursor, Cline, etc.) can discover and invoke
//! OpenProxy's built-in administrative tools directly — without going through
//! a child process bridge.
//!
//! Protocol verbs handled:
//!   * `initialize`        — version negotiation + server capabilities
//!   * `tools/list`         — enumerate available tools
//!   * `tools/call`         — execute a tool by name with arguments
//!   * `resources/list`     — list available resources (read-only data)
//!   * `resources/read`     — read a resource by URI

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::Db;
use crate::server::state::AppState;
use crate::types::{Combo, ModelAliasTarget, ProviderConnection, ProxyPool};

// ─── Protocol types ────────────────────────────────────────────────────────

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// JSON-RPC 2.0 success response.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    pub result: Value,
}

/// JSON-RPC 2.0 error response.
#[derive(Debug, Serialize)]
pub struct JsonRpcErrorResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    pub error: JsonRpcError,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// Standard JSON-RPC error codes
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;

// MCP-specific error codes
const MCP_TOOL_NOT_FOUND: i32 = -32001;
const MCP_TOOL_EXECUTION_ERROR: i32 = -32002;

// ─── Tool definition ───────────────────────────────────────────────────────

/// Describes a tool as required by the MCP protocol.
#[derive(Debug, Clone, Serialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
}

/// MCP `tools/list` result.
#[derive(Debug, Serialize)]
pub struct McpToolsResult {
    pub tools: Vec<McpTool>,
}

/// MCP `resources/list` result.
#[derive(Debug, Serialize)]
pub struct McpResourcesResult {
    pub resources: Vec<McpResource>,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpResource {
    pub uri: String,
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// MCP `resources/read` result.
#[derive(Debug, Serialize)]
pub struct McpResourceReadResult {
    pub contents: Vec<McpResourceContent>,
}

#[derive(Debug, Serialize)]
pub struct McpResourceContent {
    pub uri: String,
    pub mime_type: String,
    pub text: String,
}

/// MCP `tools/call` result.
#[derive(Debug, Serialize)]
pub struct McpToolCallResult {
    pub content: Vec<McpToolContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct McpToolContent {
    #[serde(rename = "type")]
    pub r#type: String,
    pub text: String,
}

/// Server capabilities sent in the `initialize` response.
#[derive(Debug, Serialize)]
pub struct McpServerCapabilities {
    pub tools: HashMap<String, Value>,
    pub resources: HashMap<String, Value>,
}

#[derive(Debug, Serialize)]
pub struct McpInitializeResult {
    pub protocol_version: &'static str,
    pub capabilities: McpServerCapabilities,
    pub server_info: McpServerInfo,
}

#[derive(Debug, Serialize)]
pub struct McpServerInfo {
    pub name: &'static str,
    pub version: &'static str,
}

// ─── Tool registry ─────────────────────────────────────────────────────────

/// Internal handler for a single MCP tool.
struct ToolHandler {
    def: McpTool,
    handler: fn(state: &AppState, args: Value) -> Result<Value, String>,
}

fn make_schema(properties: Value, required: Vec<&str>) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

macro_rules! mcp_tool {
    ($name:expr, $desc:expr, $schema:expr, $handler:expr) => {
        ToolHandler {
            def: McpTool {
                name: $name.to_string(),
                description: $desc.to_string(),
                input_schema: Some(make_schema($schema, vec![])),
            },
            handler: $handler,
        }
    };
    ($name:expr, $desc:expr, $schema:expr, $required:expr, $handler:expr) => {
        ToolHandler {
            def: McpTool {
                name: $name.to_string(),
                description: $desc.to_string(),
                input_schema: Some(make_schema($schema, $required)),
            },
            handler: $handler,
        }
    };
}

/// Run a `Db::update` call synchronously from within a `block_in_place`
/// context. The MCP tool handlers are synchronous but `Db::update` is now
/// async; this helper uses the current tokio handle to block on it.
fn db_update_sync<F>(db: &Arc<Db>, updater: F) -> Result<(), String>
where
    F: FnOnce(&mut crate::types::AppDb) + Send + 'static,
{
    let handle = tokio::runtime::Handle::current();
    handle
        .block_on(async { db.clone().update(updater).await })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn tool_table() -> Vec<ToolHandler> {
    vec![
        // ── Provider tools ────────────────────────────────────────────────
        mcp_tool!(
            "provider_list",
            "List all provider connections with their status",
            json!({}),
            |state, _args| {
                let snap = state.db.snapshot();
                Ok(json!(snap.provider_connections))
            }
        ),
        mcp_tool!(
            "provider_create",
            "Create a new provider connection",
            json!({
                "provider": { "type": "string", "description": "Provider type (e.g. openai, anthropic)" },
                "name": { "type": "string", "description": "Display name" },
                "api_key": { "type": "string", "description": "API key" },
                "base_url": { "type": "string", "description": "Optional base URL override" }
            }),
            vec!["provider", "name", "api_key"],
            |state, args| {
                let provider = args
                    .get("provider")
                    .and_then(Value::as_str)
                    .ok_or("Missing 'provider'")?;
                let name = args
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("Missing 'name'")?;
                let api_key = args
                    .get("api_key")
                    .and_then(Value::as_str)
                    .ok_or("Missing 'api_key'")?;
                let base_url = args.get("base_url").and_then(Value::as_str);

                let id = uuid::Uuid::new_v4().to_string();
                let now = chrono::Utc::now().to_rfc3339();
                let mut conn = ProviderConnection::default();
                conn.id = id.clone();
                conn.provider = provider.to_string();
                conn.auth_type = "apikey".to_string();
                conn.name = Some(name.to_string());
                conn.priority = Some(1);
                conn.is_active = Some(true);
                conn.created_at = Some(now.clone());
                conn.updated_at = Some(now);
                conn.api_key = Some(api_key.to_string());
                if let Some(url) = base_url {
                    conn.provider_specific_data
                        .insert("baseUrl".to_string(), Value::String(url.to_string()));
                }

                let conn_clone = conn.clone();
                db_update_sync(&state.db, move |db| {
                    db.provider_connections.push(conn_clone);
                })?;
                Ok(json!({ "id": id, "provider": provider, "name": name, "success": true }))
            }
        ),
        mcp_tool!(
            "provider_delete",
            "Delete a provider connection by ID or name",
            json!({
                "id": { "type": "string", "description": "Provider ID or name" }
            }),
            vec!["id"],
            |state, args| {
                let id = args
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("Missing 'id'")?;
                let id_str = id.to_string();
                db_update_sync(&state.db, move |db| {
                    db.provider_connections.retain(|c| {
                        c.id != id_str && c.name.as_deref().map(|n| n != &id_str).unwrap_or(true)
                    });
                })?;
                Ok(json!({ "success": true }))
            }
        ),
        mcp_tool!(
            "provider_test",
            "Test a provider connection by querying its status",
            json!({
                "id": { "type": "string", "description": "Provider ID or name" }
            }),
            vec!["id"],
            |state, args| {
                let id = args
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("Missing 'id'")?;
                let snap = state.db.snapshot();
                let conn = snap
                    .provider_connections
                    .iter()
                    .find(|c| c.id == id || c.name.as_deref().map(|n| n == id).unwrap_or(false))
                    .ok_or_else(|| format!("Provider '{id}' not found"))?;
                let url = conn
                    .provider_specific_data
                    .get("baseUrl")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("https://api.{}", conn.provider));
                Ok(json!({
                    "provider": conn.provider,
                    "name": conn.name,
                    "status": conn.test_status.as_deref().unwrap_or("unknown"),
                    "base_url": url,
                }))
            }
        ),
        // ── API Key tools ─────────────────────────────────────────────────
        mcp_tool!(
            "key_list",
            "List all API keys (secrets excluded)",
            json!({}),
            |state, _args| {
                let snap = state.db.snapshot();
                let keys: Vec<Value> = snap
                    .api_keys
                    .iter()
                    .map(|k| {
                        json!({
                            "id": k.id,
                            "name": k.name,
                            "is_active": k.is_active,
                            "created_at": k.created_at,
                        })
                    })
                    .collect();
                Ok(json!(keys))
            }
        ),
        mcp_tool!(
            "key_create",
            "Create a new API key",
            json!({
                "name": { "type": "string", "description": "Key display name" }
            }),
            vec!["name"],
            |state, args| {
                let name = args
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("Missing 'name'")?;
                let id = uuid::Uuid::new_v4().to_string();
                let machine_id = crate::server::api::consistent_machine_id();
                let key = crate::core::auth::generate_api_key_with_machine(&machine_id);
                let now = chrono::Utc::now().to_rfc3339();
                let k = crate::types::ApiKey {
                    id: id.clone(),
                    name: name.to_string(),
                    key: key.clone(),
                    machine_id: Some(machine_id),
                    is_active: Some(true),
                    created_at: Some(now),
                    extra: std::collections::BTreeMap::new(),
                };
                db_update_sync(&state.db, move |db| {
                    db.api_keys.push(k);
                })?;
                Ok(json!({ "id": id, "key": key, "name": name, "success": true }))
            }
        ),
        mcp_tool!(
            "key_delete",
            "Delete an API key by ID",
            json!({
                "id": { "type": "string", "description": "Key ID" }
            }),
            vec!["id"],
            |state, args| {
                let id = args
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("Missing 'id'")?;
                let id_str = id.to_string();
                db_update_sync(&state.db, move |db| {
                    db.api_keys.retain(|k| k.id != id_str);
                })?;
                Ok(json!({ "success": true }))
            }
        ),
        // ── Combo tools ───────────────────────────────────────────────────
        mcp_tool!(
            "combo_list",
            "List all model combos",
            json!({}),
            |state, _args| {
                let snap = state.db.snapshot();
                Ok(json!(snap.combos))
            }
        ),
        mcp_tool!(
            "combo_create",
            "Create a new model combo",
            json!({
                "name": { "type": "string", "description": "Combo name" },
                "models": { "type": "array", "items": { "type": "string" }, "description": "List of model IDs" },
                "kind": { "type": "string", "description": "Optional combo kind" }
            }),
            vec!["name", "models"],
            |state, args| {
                let name = args
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("Missing 'name'")?;
                let models = args
                    .get("models")
                    .and_then(Value::as_array)
                    .ok_or("Missing 'models'")?;
                let model_ids: Vec<String> = models
                    .iter()
                    .filter_map(|m| m.as_str().map(String::from))
                    .collect();
                let kind = args.get("kind").and_then(Value::as_str).map(String::from);
                let id = uuid::Uuid::new_v4().to_string();
                let now = chrono::Utc::now().to_rfc3339();
                let combo = Combo {
                    id: id.clone(),
                    name: name.to_string(),
                    models: model_ids,
                    disabled_models: Vec::new(),
                    kind,
                    created_at: Some(now.clone()),
                    updated_at: Some(now),
                    extra: std::collections::BTreeMap::new(),
                };
                db_update_sync(&state.db, move |db| {
                    db.combos.push(combo);
                })?;
                Ok(json!({ "id": id, "name": name, "success": true }))
            }
        ),
        // ── Proxy Pool tools ──────────────────────────────────────────────
        mcp_tool!(
            "pool_list",
            "List all proxy pools",
            json!({}),
            |state, _args| {
                let snap = state.db.snapshot();
                Ok(json!(snap.proxy_pools))
            }
        ),
        mcp_tool!(
            "pool_create",
            "Create a new proxy pool",
            json!({
                "name": { "type": "string", "description": "Pool name" },
                "proxy_url": { "type": "string", "description": "Proxy URL" },
                "type": { "type": "string", "description": "Proxy type (http, cloudflare, vercel, deno)" }
            }),
            vec!["name", "proxy_url"],
            |state, args| {
                let name = args
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("Missing 'name'")?;
                let proxy_url = args
                    .get("proxy_url")
                    .and_then(Value::as_str)
                    .ok_or("Missing 'proxy_url'")?;
                let pool_type = args.get("type").and_then(Value::as_str).unwrap_or("http");
                let id = uuid::Uuid::new_v4().to_string();
                let now = chrono::Utc::now().to_rfc3339();
                let mut pool = ProxyPool::default();
                pool.id = id.clone();
                pool.name = name.to_string();
                pool.proxy_url = proxy_url.to_string();
                pool.r#type = pool_type.to_string();
                pool.is_active = Some(true);
                pool.test_status = Some("unknown".to_string());
                pool.created_at = Some(now.clone());
                pool.updated_at = Some(now);
                db_update_sync(&state.db, move |db| {
                    db.proxy_pools.push(pool);
                })?;
                Ok(json!({ "id": id, "name": name, "success": true }))
            }
        ),
        mcp_tool!(
            "pool_delete",
            "Delete a proxy pool by ID",
            json!({
                "id": { "type": "string", "description": "Pool ID" }
            }),
            vec!["id"],
            |state, args| {
                let id = args
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("Missing 'id'")?;
                let id_str = id.to_string();
                db_update_sync(&state.db, move |db| {
                    db.proxy_pools.retain(|p| p.id != id_str);
                })?;
                Ok(json!({ "success": true }))
            }
        ),
        // ── Node tools ────────────────────────────────────────────────────
        mcp_tool!(
            "node_list",
            "List all provider nodes",
            json!({}),
            |state, _args| {
                let snap = state.db.snapshot();
                Ok(json!(snap.provider_nodes))
            }
        ),
        // ── Model tools ───────────────────────────────────────────────────
        mcp_tool!(
            "models_list",
            "List all model aliases",
            json!({}),
            |state, _args| {
                let snap = state.db.snapshot();
                let aliases: Vec<Value> = snap
                    .model_aliases
                    .iter()
                    .map(|(alias, target)| {
                        let (target_model, kind) = match target {
                            ModelAliasTarget::Path(path) => (path.clone(), None),
                            ModelAliasTarget::Mapping(m) => (
                                m.model.clone(),
                                m.extra
                                    .get("kind")
                                    .and_then(|v| v.as_str())
                                    .map(String::from),
                            ),
                        };
                        json!({
                            "alias": alias,
                            "target": target_model,
                            "kind": kind,
                        })
                    })
                    .collect();
                Ok(json!(aliases))
            }
        ),
        // ── Health tools ──────────────────────────────────────────────────
        mcp_tool!(
            "health",
            "Get server health status",
            json!({}),
            |_state, _args| { Ok(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") })) }
        ),
        // ── Settings tools ────────────────────────────────────────────────
        mcp_tool!(
            "settings_get",
            "Get current server settings",
            json!({}),
            |state, _args| {
                let snap = state.db.snapshot();
                Ok(crate::server::api::safe_settings_payload(&snap.settings))
            }
        ),
        // ── Usage tools ───────────────────────────────────────────────────
        mcp_tool!(
            "usage_status",
            "Get current usage tracking status",
            json!({}),
            |_state, _args| {
                Ok(json!({ "message": "Usage data is available via the REST API" }))
            }
        ),
    ]
}

// ─── Public API ────────────────────────────────────────────────────────────

/// Handle an incoming JSON-RPC 2.0 MCP request against the OpenProxy tool
/// surface. Returns a JSON-serialised response.
pub fn handle_mcp_request(state: &AppState, body: &Value) -> Value {
    let request: JsonRpcRequest = match serde_json::from_value(body.clone()) {
        Ok(req) => req,
        Err(e) => {
            return json!(JsonRpcErrorResponse {
                jsonrpc: "2.0",
                id: body.get("id").cloned().unwrap_or(Value::Null),
                error: JsonRpcError {
                    code: INTERNAL_ERROR,
                    message: format!("Failed to parse request: {e}"),
                    data: None,
                },
            });
        }
    };

    let id = request.id.clone();

    match request.method.as_str() {
        "initialize" => handle_initialize(id),
        "tools/list" => handle_tools_list(id),
        "tools/call" => handle_tools_call(state, id, &request.params),
        "resources/list" => handle_resources_list(id),
        "resources/read" => handle_resources_read(state, id, &request.params),
        _ => {
            json!(JsonRpcErrorResponse {
                jsonrpc: "2.0",
                id,
                error: JsonRpcError {
                    code: METHOD_NOT_FOUND,
                    message: format!("Method '{}' not found", request.method),
                    data: None,
                },
            })
        }
    }
}

fn handle_initialize(id: Value) -> Value {
    json!(JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: serde_json::to_value(McpInitializeResult {
            protocol_version: "2025-03-26",
            capabilities: McpServerCapabilities {
                tools: [("listChanged".to_string(), json!(false))].into(),
                resources: [("listChanged".to_string(), json!(false))].into(),
            },
            server_info: McpServerInfo {
                name: "openproxy",
                version: env!("CARGO_PKG_VERSION"),
            },
        })
        .unwrap_or_default(),
    })
}

fn handle_tools_list(id: Value) -> Value {
    let tools: Vec<McpTool> = tool_table().into_iter().map(|t| t.def).collect();
    json!(JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: serde_json::to_value(McpToolsResult { tools }).unwrap_or_default(),
    })
}

fn handle_tools_call(state: &AppState, id: Value, params: &Value) -> Value {
    let name = match params.get("name").and_then(Value::as_str) {
        Some(n) => n,
        None => {
            return json!(JsonRpcErrorResponse {
                jsonrpc: "2.0",
                id,
                error: JsonRpcError {
                    code: INVALID_PARAMS,
                    message: "Missing 'name' field".to_string(),
                    data: None,
                },
            });
        }
    };

    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let registry = tool_table();
    let handler = match registry.into_iter().find(|t| t.def.name == name) {
        Some(h) => h,
        None => {
            return json!(JsonRpcErrorResponse {
                jsonrpc: "2.0",
                id,
                error: JsonRpcError {
                    code: MCP_TOOL_NOT_FOUND,
                    message: format!("Tool '{name}' not found"),
                    data: None,
                },
            });
        }
    };

    let result = tokio::task::block_in_place(|| (handler.handler)(state, args));

    match result {
        Ok(value) => json!(JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: serde_json::to_value(McpToolCallResult {
                content: vec![McpToolContent {
                    r#type: "text".to_string(),
                    text: serde_json::to_string_pretty(&value).unwrap_or_default(),
                }],
                is_error: None,
            })
            .unwrap_or_default(),
        }),
        Err(err_msg) => json!(JsonRpcErrorResponse {
            jsonrpc: "2.0",
            id,
            error: JsonRpcError {
                code: MCP_TOOL_EXECUTION_ERROR,
                message: err_msg,
                data: None,
            },
        }),
    }
}

fn handle_resources_list(id: Value) -> Value {
    let resources = vec![
        McpResource {
            uri: "openproxy://health".to_string(),
            name: "Server Health".to_string(),
            description: "Server health status and version info".to_string(),
            mime_type: Some("application/json".to_string()),
        },
        McpResource {
            uri: "openproxy://models".to_string(),
            name: "Available Models".to_string(),
            description: "List of available model aliases".to_string(),
            mime_type: Some("application/json".to_string()),
        },
        McpResource {
            uri: "openproxy://providers".to_string(),
            name: "Provider Connections".to_string(),
            description: "List of configured provider connections".to_string(),
            mime_type: Some("application/json".to_string()),
        },
        McpResource {
            uri: "openproxy://combos".to_string(),
            name: "Model Combos".to_string(),
            description: "List of model combos".to_string(),
            mime_type: Some("application/json".to_string()),
        },
        McpResource {
            uri: "openproxy://pools".to_string(),
            name: "Proxy Pools".to_string(),
            description: "List of proxy pools".to_string(),
            mime_type: Some("application/json".to_string()),
        },
        McpResource {
            uri: "openproxy://keys".to_string(),
            name: "API Keys".to_string(),
            description: "List of API keys (without secrets)".to_string(),
            mime_type: Some("application/json".to_string()),
        },
        McpResource {
            uri: "openproxy://nodes".to_string(),
            name: "Provider Nodes".to_string(),
            description: "List of provider nodes".to_string(),
            mime_type: Some("application/json".to_string()),
        },
    ];
    json!(JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: serde_json::to_value(McpResourcesResult { resources }).unwrap_or_default(),
    })
}

fn handle_resources_read(state: &AppState, id: Value, params: &Value) -> Value {
    let uri = match params.get("uri").and_then(Value::as_str) {
        Some(u) => u,
        None => {
            return json!(JsonRpcErrorResponse {
                jsonrpc: "2.0",
                id,
                error: JsonRpcError {
                    code: INVALID_PARAMS,
                    message: "Missing 'uri' field".to_string(),
                    data: None,
                },
            });
        }
    };

    let text = match uri {
        "openproxy://health" => serde_json::to_string_pretty(&json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
        })),
        "openproxy://models" => {
            let snap = state.db.snapshot();
            serde_json::to_string_pretty(&snap.model_aliases)
        }
        "openproxy://providers" => {
            let snap = state.db.snapshot();
            serde_json::to_string_pretty(&snap.provider_connections)
        }
        "openproxy://combos" => {
            let snap = state.db.snapshot();
            serde_json::to_string_pretty(&snap.combos)
        }
        "openproxy://pools" => {
            let snap = state.db.snapshot();
            serde_json::to_string_pretty(&snap.proxy_pools)
        }
        "openproxy://keys" => {
            let snap = state.db.snapshot();
            let sanitised: Vec<Value> = snap
                .api_keys
                .iter()
                .map(|k| {
                    json!({
                        "id": k.id,
                        "name": k.name,
                        "is_active": k.is_active,
                        "created_at": k.created_at,
                    })
                })
                .collect();
            serde_json::to_string_pretty(&sanitised)
        }
        "openproxy://nodes" => {
            let snap = state.db.snapshot();
            serde_json::to_string_pretty(&snap.provider_nodes)
        }
        other => {
            return json!(JsonRpcErrorResponse {
                jsonrpc: "2.0",
                id,
                error: JsonRpcError {
                    code: INVALID_PARAMS,
                    message: format!("Resource '{other}' not found"),
                    data: None,
                },
            });
        }
    };

    match text {
        Ok(t) => json!(JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: serde_json::to_value(McpResourceReadResult {
                contents: vec![McpResourceContent {
                    uri: uri.to_string(),
                    mime_type: "application/json".to_string(),
                    text: t,
                }],
            })
            .unwrap_or_default(),
        }),
        Err(e) => json!(JsonRpcErrorResponse {
            jsonrpc: "2.0",
            id,
            error: JsonRpcError {
                code: INTERNAL_ERROR,
                message: format!("Failed to serialise resource: {e}"),
                data: None,
            },
        }),
    }
}

/// Convenience wrapper: parses a JSON string, processes it, returns a JSON string.
pub fn handle_mcp_request_json(state: &AppState, json_body: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(json_body).map_err(|e| format!("Invalid JSON: {e}"))?;
    let response = handle_mcp_request(state, &value);
    serde_json::to_string(&response).map_err(|e| format!("Failed to serialise response: {e}"))
}
