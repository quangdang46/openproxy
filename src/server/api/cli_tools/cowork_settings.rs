use std::env;
use std::path::{Path, PathBuf};

use anyhow::Result as AnyhowResult;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::fs;
use uuid::Uuid;

use crate::core::auth::CLI_TOKEN_HEADER;
use crate::core::mcp::plugins::local_stdio_plugins;
use crate::server::state::AppState;

const PROVIDER: &str = "gateway";
const DEFAULT_APP_PORT: u16 = 4623;
const CLI_TOKEN_SALT: &str = "9r-cli-auth";

// Hardcoded relax-security profile applied on every Apply (mirrors 9router).
fn security_relax() -> Value {
    json!({
        "coworkEgressAllowedHosts": ["*"],
        "disabledBuiltinTools": [],
        "isLocalDevMcpEnabled": true,
        "isDesktopExtensionEnabled": true,
        "isDesktopExtensionDirectoryEnabled": true,
        "isDesktopExtensionSignatureRequired": false,
        "isClaudeCodeForDesktopEnabled": true,
        "disableEssentialTelemetry": true,
        "disableNonessentialTelemetry": true,
        "disableNonessentialServices": true,
    })
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/cli-tools/cowork-settings",
            get(get_cowork_settings)
                .post(save_cowork_settings)
                .delete(delete_cowork_settings),
        )
        .route_layer(middleware::from_fn(
            crate::server::api::guard::require_local_only,
        ))
}

fn require_management_access(
    headers: &HeaderMap,
    state: &AppState,
) -> std::result::Result<(), Response> {
    super::super::require_dashboard_or_management_api_key(headers, state)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveCoworkSettingsRequest {
    base_url: Option<String>,
    api_key: Option<String>,
    models: Option<Vec<Value>>,
    plugins: Option<Vec<Value>>,
    local_plugins: Option<Vec<Value>>,
    custom_plugins: Option<Vec<Value>>,
}

async fn get_cowork_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    match load_cowork_status().await {
        Ok(status) => Json(status).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to read cowork settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to read cowork settings" })),
            )
                .into_response()
        }
    }
}

async fn save_cowork_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SaveCoworkSettingsRequest>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let base_url = body.base_url.unwrap_or_default();
    let api_key = body.api_key.unwrap_or_default();
    if base_url.is_empty() || api_key.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "baseUrl and apiKey are required" })),
        )
            .into_response();
    }

    if is_localhost_url(&base_url) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Claude Cowork sandbox cannot reach localhost. Enable Tunnel/Cloud Endpoint or use Tailscale/VPS."
            })),
        )
            .into_response();
    }

    let models = body
        .models
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .filter(|value| !value.trim().is_empty())
        .collect::<Vec<_>>();
    if models.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "At least one model is required" })),
        )
            .into_response();
    }

    // Respect empty array (user toggled all off); fallback to defaults only when undefined.
    let plugins = body.plugins.unwrap_or_else(default_plugins_json);
    let local_plugin_names: Vec<String> = body
        .local_plugins
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    // Only URL-based custom plugins allowed (no stdio command spawning).
    let custom_plugins: Vec<Value> = body
        .custom_plugins
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.get("url").and_then(Value::as_str).is_some())
        .collect();

    match write_cowork_settings(
        &base_url,
        &api_key,
        &models,
        &plugins,
        &local_plugin_names,
        &custom_plugins,
    )
    .await
    {
        Ok((bootstrapped, config_path, skip_count)) => Json(json!({
            "success": true,
            "bootstrapped": bootstrapped,
            "message": if bootstrapped {
                "Cowork enabled (3p mode set). Quit & reopen Claude Desktop."
            } else {
                "Cowork settings applied. Quit & reopen Claude Desktop."
            },
            "configPath": config_path,
            "skipApprovals": { "written": skip_count },
            "localMcp": { "applied": local_plugin_names, "via": "3p-sse-bridge" },
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to write cowork settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to apply cowork settings" })),
            )
                .into_response()
        }
    }
}

async fn delete_cowork_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    match reset_cowork_settings().await {
        Ok(response) => Json(response).into_response(),
        Err(error) => {
            tracing::warn!(?error, "failed to reset cowork settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to reset cowork settings" })),
            )
                .into_response()
        }
    }
}

fn default_plugins_json() -> Vec<Value> {
    vec![
        json!({
            "name": "exa",
            "title": "Exa",
            "description": "Real-time web search and code documentation",
            "url": "https://mcp.exa.ai/mcp",
            "transport": "http",
            "oauth": false,
            "toolNames": ["web_search_exa", "web_fetch_exa"],
        }),
        json!({
            "name": "tavily",
            "title": "Tavily",
            "description": "Real-time web search optimized for LLM agents",
            "url": "https://mcp.tavily.com/mcp",
            "transport": "http",
            "oauth": true,
            "toolNames": ["tavily_search", "tavily_extract", "tavily_crawl", "tavily_map"],
        }),
    ]
}

fn local_stdio_plugins_json() -> Vec<Value> {
    local_stdio_plugins()
        .into_iter()
        .map(|p| {
            json!({
                "name": p.name,
                "title": p.title,
                "description": p.description,
                "extensionUrl": p.extension_url,
                "command": p.command,
                "args": p.args,
                "toolNames": p.tool_names,
            })
        })
        .collect()
}

fn app_port() -> u16 {
    env::var("PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_APP_PORT)
}

fn cli_token() -> String {
    // Match 9router's getConsistentMachineId("9r-cli-auth") as closely as practical.
    // OpenProxy's consistent_machine_id uses MACHINE_ID_SALT (default endpoint-proxy-salt).
    // For CLI bridge headers we hash with the dedicated CLI_TOKEN_SALT.
    use sha2::Digest;
    let salt = CLI_TOKEN_SALT;
    match raw_machine_id() {
        Some(raw) => {
            let mut hasher = sha2::Sha256::new();
            hasher.update(raw.as_bytes());
            hasher.update(salt.as_bytes());
            hex::encode(hasher.finalize())[..16].to_string()
        }
        None => {
            // Fall back to the process-stable consistent machine id so headers are stable
            // across requests within a process even without /etc/machine-id.
            crate::server::api::consistent_machine_id()
        }
    }
}

fn raw_machine_id() -> Option<String> {
    ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn build_managed_mcp_servers(plugins: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for p in plugins {
        let name = p.get("name").and_then(Value::as_str).unwrap_or("");
        let url = p.get("url").and_then(Value::as_str).unwrap_or("");
        if name.is_empty() || url.is_empty() || seen.contains(name) {
            continue;
        }
        seen.insert(name.to_string());

        let transport = p
            .get("transport")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                if regex_is_sse_url(url) {
                    "sse".to_string()
                } else {
                    "http".to_string()
                }
            });

        let mut entry = json!({
            "name": name,
            "url": url,
            "transport": transport,
        });

        if p.get("oauth").and_then(Value::as_bool).unwrap_or(false) {
            entry
                .as_object_mut()
                .unwrap()
                .insert("oauth".to_string(), Value::Bool(true));
        }

        if let Some(tool_names) = p.get("toolNames").and_then(Value::as_array) {
            let prefix = format!("{name}-");
            let mut bare = std::collections::BTreeSet::new();
            for raw in tool_names {
                let Some(mut t) = raw.as_str().map(str::to_string) else {
                    continue;
                };
                if t.is_empty() {
                    continue;
                }
                while t.starts_with(&prefix) {
                    t = t[prefix.len()..].to_string();
                }
                bare.insert(t);
            }
            if !bare.is_empty() {
                let mut policy = Map::new();
                for t in bare {
                    policy.insert(t.clone(), Value::String("allow".to_string()));
                    policy.insert(format!("{prefix}{t}"), Value::String("allow".to_string()));
                }
                entry
                    .as_object_mut()
                    .unwrap()
                    .insert("toolPolicy".to_string(), Value::Object(policy));
            }
        }

        out.push(entry);
    }
    out
}

fn regex_is_sse_url(url: &str) -> bool {
    // Simple equivalent of /\/sse(\b|\/)/i
    let lower = url.to_ascii_lowercase();
    if let Some(idx) = lower.find("/sse") {
        let after = &lower[idx + 4..];
        return after.is_empty() || after.starts_with('/') || after.starts_with('?');
    }
    false
}

fn build_local_bridge_entries(local_plugin_names: &[String]) -> Vec<Value> {
    let port = app_port();
    let token = cli_token();
    let catalog = local_stdio_plugins();
    let mut out = Vec::new();

    for n in local_plugin_names {
        let Some(def) = catalog.iter().find(|p| &p.name == n) else {
            continue;
        };
        let url = format!("http://localhost:{port}/api/mcp/{}/sse", def.name);
        let mut headers = Map::new();
        headers.insert(CLI_TOKEN_HEADER.to_string(), Value::String(token.clone()));
        let mut entry = json!({
            "name": def.name,
            "url": url,
            "transport": "sse",
        });
        entry
            .as_object_mut()
            .unwrap()
            .insert("headers".to_string(), Value::Object(headers));

        if !def.tool_names.is_empty() {
            let prefix = format!("{}-", def.name);
            let mut policy = Map::new();
            for t in &def.tool_names {
                policy.insert(t.clone(), Value::String("allow".to_string()));
                policy.insert(format!("{prefix}{t}"), Value::String("allow".to_string()));
            }
            entry
                .as_object_mut()
                .unwrap()
                .insert("toolPolicy".to_string(), Value::Object(policy));
        }
        out.push(entry);
    }
    out
}

fn build_custom_entries(custom_plugins: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for p in custom_plugins {
        let name = p.get("name").and_then(Value::as_str).unwrap_or("");
        let url = p.get("url").and_then(Value::as_str).unwrap_or("");
        if name.is_empty() || url.is_empty() {
            continue;
        }
        let transport = p.get("transport").and_then(Value::as_str).unwrap_or("sse");
        out.push(json!({
            "name": name,
            "url": url,
            "transport": transport,
            "custom": true,
        }));
    }
    out
}

async fn write_skip_approvals(managed_servers: &[Value]) -> AnyhowResult<usize> {
    let cfg_path = write_root().join("config.json");
    let mut cfg = match read_json_optional(&cfg_path).await? {
        Some(Value::Object(fields)) => fields,
        _ => Map::new(),
    };
    let mut skip = Map::new();
    for srv in managed_servers {
        if let Some(name) = srv.get("name").and_then(Value::as_str) {
            skip.insert(name.to_string(), Value::Bool(true));
        }
    }
    let count = skip.len();
    cfg.insert("operonSkipMcpApprovals".to_string(), Value::Object(skip));
    write_json(&cfg_path, &Value::Object(cfg)).await?;
    Ok(count)
}

async fn cleanup_1p_legacy() -> AnyhowResult<()> {
    let cfg_path = one_party_root().join("claude_desktop_config.json");
    let mut cfg = match read_json_optional(&cfg_path).await? {
        Some(Value::Object(fields)) => fields,
        _ => return Ok(()),
    };
    let Some(Value::Object(mcp)) = cfg.get_mut("mcpServers") else {
        return Ok(());
    };
    let managed_names: std::collections::HashSet<String> =
        local_stdio_plugins().into_iter().map(|p| p.name).collect();
    mcp.retain(|k, _| !managed_names.contains(k));
    if mcp.is_empty() {
        cfg.remove("mcpServers");
    }
    write_json(&cfg_path, &Value::Object(cfg)).await?;
    Ok(())
}

async fn load_cowork_status() -> AnyhowResult<Value> {
    let installed = check_installed().await;
    if !installed {
        return Ok(json!({
            "installed": false,
            "config": Value::Null,
            "message": "Claude Desktop (Cowork mode) not detected",
            "defaultPlugins": default_plugins_json(),
            "localStdioPlugins": local_stdio_plugins_json(),
        }));
    }

    let meta = read_json_optional(&meta_path(resolve_app_root_for_read().await).await?).await?;
    let applied_id = meta.as_ref().and_then(meta_applied_id);
    let config_dir = config_dir(resolve_app_root_for_read().await).await?;
    let config_path = applied_id
        .as_deref()
        .map(|id| config_dir.join(format!("{id}.json")));
    let config = match config_path.as_ref() {
        Some(path) => read_json_optional(path).await?,
        None => None,
    };

    let base_url = config
        .as_ref()
        .and_then(|value| value.get("inferenceGatewayBaseUrl"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let models = config
        .as_ref()
        .and_then(|value| value.get("inferenceModels"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| match value {
                    Value::String(name) => Some(name.clone()),
                    Value::Object(fields) => fields
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let provider = config
        .as_ref()
        .and_then(|value| value.get("inferenceProvider"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let has_openproxy = provider.as_deref() == Some(PROVIDER)
        && base_url.as_deref().is_some_and(|value| !value.is_empty());

    let managed_mcp: Vec<Value> = config
        .as_ref()
        .and_then(|value| value.get("managedMcpServers"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let stdio_names: std::collections::HashSet<String> =
        local_stdio_plugins().into_iter().map(|p| p.name).collect();

    // Active local plugins = managedMcp entries whose URL points at our inline bridge.
    let active_local_names: Vec<String> = managed_mcp
        .iter()
        .filter_map(|m| {
            let name = m.get("name").and_then(Value::as_str)?;
            let url = m.get("url").and_then(Value::as_str).unwrap_or("");
            if stdio_names.contains(name) && url.contains("/api/mcp/") {
                Some(name.to_string())
            } else {
                None
            }
        })
        .collect();

    // Custom plugins = bridge entries not in preset LOCAL_STDIO_PLUGINS.
    let active_custom_plugins: Vec<Value> = managed_mcp
        .iter()
        .filter(|m| {
            let name = m.get("name").and_then(Value::as_str).unwrap_or("");
            let url = m.get("url").and_then(Value::as_str).unwrap_or("");
            let is_custom = m.get("custom").and_then(Value::as_bool).unwrap_or(false);
            is_custom || (!stdio_names.contains(name) && url.contains("/api/mcp/"))
        })
        .map(|m| {
            json!({
                "name": m.get("name"),
                "url": m.get("url"),
                "transport": m.get("transport"),
                "custom": true,
            })
        })
        .collect();

    let defaults = default_plugins_json();
    let plugins: Vec<Value> = managed_mcp
        .iter()
        .filter(|m| {
            let name = m.get("name").and_then(Value::as_str).unwrap_or("");
            let url = m.get("url").and_then(Value::as_str).unwrap_or("");
            let is_custom = m.get("custom").and_then(Value::as_bool).unwrap_or(false);
            !is_custom && !(stdio_names.contains(name) && url.contains("/api/mcp/"))
        })
        .map(|m| {
            let name = m.get("name").and_then(Value::as_str).unwrap_or("");
            let keys = m
                .get("toolPolicy")
                .and_then(Value::as_object)
                .map(|o| o.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            let prefix = format!("{name}-");
            let mut bare = std::collections::BTreeSet::new();
            for k in keys {
                let mut t = k;
                while t.starts_with(&prefix) {
                    t = t[prefix.len()..].to_string();
                }
                bare.insert(t);
            }
            // Prefer curated default toolNames when plugin matches a default.
            let tool_names = defaults
                .iter()
                .find(|d| d.get("name").and_then(Value::as_str) == Some(name))
                .and_then(|d| d.get("toolNames").cloned())
                .unwrap_or_else(|| Value::Array(bare.into_iter().map(Value::String).collect()));
            json!({
                "name": name,
                "url": m.get("url"),
                "transport": m.get("transport"),
                "oauth": m.get("oauth").and_then(Value::as_bool).unwrap_or(false),
                "toolNames": tool_names,
            })
        })
        .collect();

    Ok(json!({
        "installed": true,
        "config": config,
        "hasOpenProxy": has_openproxy,
        "configPath": config_path.map(|path| path.to_string_lossy().to_string()),
        "cowork": {
            "appliedId": applied_id,
            "baseUrl": base_url,
            "models": models,
            "provider": provider,
            "plugins": plugins,
            "localPlugins": active_local_names,
            "customPlugins": active_custom_plugins,
        },
        "defaultPlugins": defaults,
        "localStdioPlugins": local_stdio_plugins_json(),
    }))
}

async fn write_cowork_settings(
    base_url: &str,
    api_key: &str,
    models: &[String],
    plugins: &[Value],
    local_plugin_names: &[String],
    custom_plugins: &[Value],
) -> AnyhowResult<(bool, String, usize)> {
    let bootstrapped = bootstrap_deployment_mode().await?;
    let meta = ensure_meta().await?;
    let applied_id = meta_applied_id(&meta).unwrap_or_else(|| Uuid::new_v4().to_string());
    let config_path = write_config_dir().await?.join(format!("{applied_id}.json"));

    let bridge_entries = build_local_bridge_entries(local_plugin_names);
    let custom_entries = build_custom_entries(custom_plugins);
    let mut managed = build_managed_mcp_servers(plugins);
    managed.extend(bridge_entries);
    managed.extend(custom_entries);

    let mut new_config = security_relax();
    if let Some(obj) = new_config.as_object_mut() {
        obj.insert(
            "inferenceProvider".to_string(),
            Value::String(PROVIDER.to_string()),
        );
        obj.insert(
            "inferenceGatewayBaseUrl".to_string(),
            Value::String(base_url.to_string()),
        );
        obj.insert(
            "inferenceGatewayApiKey".to_string(),
            Value::String(api_key.to_string()),
        );
        obj.insert(
            "inferenceModels".to_string(),
            Value::Array(models.iter().map(|name| json!({ "name": name })).collect()),
        );
        if !managed.is_empty() {
            obj.insert(
                "managedMcpServers".to_string(),
                Value::Array(managed.clone()),
            );
        }
    }

    write_json(&config_path, &new_config).await?;

    let skip_count = write_skip_approvals(&managed).await.unwrap_or(0);
    let _ = cleanup_1p_legacy().await;

    Ok((
        bootstrapped,
        config_path.to_string_lossy().to_string(),
        skip_count,
    ))
}

async fn reset_cowork_settings() -> AnyhowResult<Value> {
    let read_root = resolve_app_root_for_read().await;
    let meta = read_json_optional(&meta_path(read_root.clone()).await?).await?;
    let Some(applied_id) = meta.as_ref().and_then(meta_applied_id) else {
        return Ok(json!({
            "success": true,
            "message": "No active config to reset",
        }));
    };

    let config_path = config_dir(read_root)
        .await?
        .join(format!("{applied_id}.json"));
    let result = fs::write(&config_path, "{}").await;
    if let Err(error) = result {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.into());
        }
    }

    Ok(json!({
        "success": true,
        "message": "Cowork config reset",
    }))
}

async fn bootstrap_deployment_mode() -> AnyhowResult<bool> {
    let cfg_path = one_party_root().join("claude_desktop_config.json");
    let mut cfg = match read_json_optional(&cfg_path).await? {
        Some(Value::Object(fields)) => fields,
        _ => Map::new(),
    };

    if cfg
        .get("deploymentMode")
        .and_then(Value::as_str)
        .is_some_and(|value| value == "3p")
    {
        return Ok(false);
    }

    cfg.insert(
        "deploymentMode".to_string(),
        Value::String("3p".to_string()),
    );
    if let Some(parent) = cfg_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    write_json(&cfg_path, &Value::Object(cfg)).await?;
    Ok(true)
}

async fn ensure_meta() -> AnyhowResult<Value> {
    let write_meta_path = write_meta_path().await?;
    let mut meta = read_json_optional(&write_meta_path).await?;
    if meta.as_ref().and_then(meta_applied_id).is_none() {
        let existing_meta =
            read_json_optional(&meta_path(resolve_app_root_for_read().await).await?).await?;
        meta = existing_meta.filter(|value| meta_applied_id(value).is_some());

        if meta.as_ref().and_then(meta_applied_id).is_none() {
            let new_id = Uuid::new_v4().to_string();
            meta = Some(json!({
                "appliedId": new_id,
                "entries": [
                    {
                        "id": new_id,
                        "name": "Default"
                    }
                ]
            }));
        }

        if let Some(parent) = write_meta_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        if let Some(value) = meta.as_ref() {
            write_json(&write_meta_path, value).await?;
        }
    }

    Ok(meta.unwrap_or_else(|| json!({})))
}

fn meta_applied_id(value: &Value) -> Option<String> {
    value
        .get("appliedId")
        .and_then(Value::as_str)
        .map(str::to_string)
}

async fn read_json_optional(path: &Path) -> AnyhowResult<Option<Value>> {
    let content = match fs::read_to_string(path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };

    // Tolerate JSONC (trailing commas) like 9router does.
    let stripped = strip_trailing_commas(&content);
    match serde_json::from_str(&stripped).or_else(|_| serde_json::from_str(&content)) {
        Ok(v) => Ok(Some(v)),
        Err(_) => Ok(None),
    }
}

fn strip_trailing_commas(input: &str) -> String {
    // Remove commas that appear immediately before } or ]
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b',' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                // skip the comma
                i += 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

async fn write_json(path: &Path, value: &Value) -> AnyhowResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(path, serde_json::to_vec_pretty(value)?).await?;
    Ok(())
}

async fn resolve_app_root_for_read() -> PathBuf {
    let candidates = candidate_roots();
    for dir in &candidates {
        if path_exists(&dir.join("configLibrary")).await {
            return dir.clone();
        }
    }
    candidates
        .into_iter()
        .next()
        .unwrap_or_else(|| home_dir().join(".config").join("Claude-3p"))
}

async fn check_installed() -> bool {
    for dir in candidate_roots().into_iter().chain(app_install_paths()) {
        if path_exists(&dir).await {
            return true;
        }
    }
    false
}

async fn path_exists(path: &Path) -> bool {
    fs::metadata(path).await.is_ok()
}

fn is_localhost_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("localhost") || lower.contains("127.0.0.1") || lower.contains("0.0.0.0")
}

fn candidate_roots() -> Vec<PathBuf> {
    match env::consts::OS {
        "macos" => {
            let base = home_dir().join("Library").join("Application Support");
            vec![base.join("Claude-3p"), base.join("Claude")]
        }
        "windows" => {
            let local_app = env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home_dir().join("AppData").join("Local"));
            let roaming = env::var_os("APPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home_dir().join("AppData").join("Roaming"));
            vec![
                local_app.join("Claude-3p"),
                roaming.join("Claude-3p"),
                local_app.join("Claude"),
                roaming.join("Claude"),
            ]
        }
        _ => vec![
            home_dir().join(".config").join("Claude-3p"),
            home_dir().join(".config").join("Claude"),
        ],
    }
}

fn app_install_paths() -> Vec<PathBuf> {
    match env::consts::OS {
        "macos" => vec![
            PathBuf::from("/Applications/Claude.app"),
            home_dir().join("Applications").join("Claude.app"),
        ],
        "windows" => {
            let local_app = env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home_dir().join("AppData").join("Local"));
            let program_files = env::var_os("ProgramFiles")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
            vec![
                local_app.join("AnthropicClaude"),
                program_files.join("Claude"),
                program_files.join("AnthropicClaude"),
            ]
        }
        _ => Vec::new(),
    }
}

fn one_party_root() -> PathBuf {
    match env::consts::OS {
        "macos" => home_dir()
            .join("Library")
            .join("Application Support")
            .join("Claude"),
        "windows" => env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData").join("Roaming"))
            .join("Claude"),
        _ => home_dir().join(".config").join("Claude"),
    }
}

fn write_root() -> PathBuf {
    candidate_roots()
        .into_iter()
        .next()
        .unwrap_or_else(|| home_dir().join(".config").join("Claude-3p"))
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}

async fn config_dir(root: PathBuf) -> AnyhowResult<PathBuf> {
    Ok(root.join("configLibrary"))
}

async fn write_config_dir() -> AnyhowResult<PathBuf> {
    Ok(write_root().join("configLibrary"))
}

async fn meta_path(root: PathBuf) -> AnyhowResult<PathBuf> {
    Ok(config_dir(root).await?.join("_meta.json"))
}

async fn write_meta_path() -> AnyhowResult<PathBuf> {
    Ok(write_config_dir().await?.join("_meta.json"))
}
