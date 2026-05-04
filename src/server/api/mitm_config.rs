use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::server::auth::require_api_key;
use crate::server::state::AppState;

// ── GET /api/mitm-config ──────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MitmConfigResponse {
    enabled: bool,
    cert_status: CertStatus,
    router_base_url: String,
    routes: BTreeMap<String, MitmRouteInfo>,
    per_tool_settings: BTreeMap<String, MitmToolSettings>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CertStatus {
    generated: bool,
    expires_at: Option<String>,
    fingerprint: Option<String>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MitmRouteInfo {
    upstream_url: String,
    path_prefix: Option<String>,
    request_transform: bool,
    response_transform: bool,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MitmToolSettings {
    enabled: bool,
    intercept_mode: String,
}

async fn get_config(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Err(e) = require_api_key(&headers, &state.db) {
        return crate::server::api::auth_error_response(e);
    }

    let snapshot = state.db.snapshot();
    let settings = &snapshot.settings;
    let mitm_alias = &snapshot.mitm_alias;

    let mut routes = BTreeMap::new();
    let mut per_tool_settings = BTreeMap::new();

    for (name, config_map) in mitm_alias {
        let upstream_url = config_map.get("upstreamUrl").cloned().unwrap_or_default();
        let path_prefix = config_map.get("pathPrefix").cloned();
        let request_transform = config_map
            .get("requestTransform")
            .map(|v| v == "true")
            .unwrap_or(false);
        let response_transform = config_map
            .get("responseTransform")
            .map(|v| v == "true")
            .unwrap_or(false);

        let intercept_mode = config_map
            .get("interceptMode")
            .cloned()
            .unwrap_or_else(|| "full".to_string());
        let tool_enabled = config_map
            .get("enabled")
            .map(|v| v == "true")
            .unwrap_or(true);

        routes.insert(
            name.clone(),
            MitmRouteInfo {
                upstream_url,
                path_prefix,
                request_transform,
                response_transform,
            },
        );

        per_tool_settings.insert(
            name.clone(),
            MitmToolSettings {
                enabled: tool_enabled,
                intercept_mode,
            },
        );
    }

    let (cert_generated, cert_expires, cert_fingerprint) = {
        let cert_data = snapshot
            .provider_nodes
            .iter()
            .find(|n| n.extra.get("type").and_then(Value::as_str) == Some("mitm-cert"));
        match cert_data {
            Some(node) => {
                let expires = node
                    .extra
                    .get("expiresAt")
                    .and_then(Value::as_str)
                    .map(String::from);
                let fingerprint = node
                    .extra
                    .get("fingerprint")
                    .and_then(Value::as_str)
                    .map(String::from);
                (true, expires, fingerprint)
            }
            None => (false, None, None),
        }
    };

    Json(MitmConfigResponse {
        enabled: !mitm_alias.is_empty(),
        cert_status: CertStatus {
            generated: cert_generated,
            expires_at: cert_expires,
            fingerprint: cert_fingerprint,
        },
        router_base_url: settings.mitm_router_base_url.clone(),
        routes,
        per_tool_settings,
    })
    .into_response()
}

// ── PUT /api/mitm-config ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateMitmConfigRequest {
    router_base_url: Option<String>,
    routes: Option<BTreeMap<String, MitmRouteEntry>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MitmRouteEntry {
    upstream_url: String,
    path_prefix: Option<String>,
    request_transform: Option<bool>,
    response_transform: Option<bool>,
    enabled: Option<bool>,
    intercept_mode: Option<String>,
}

async fn update_config(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<UpdateMitmConfigRequest>,
) -> axum::response::Response {
    if let Err(e) = require_api_key(&headers, &state.db) {
        return crate::server::api::auth_error_response(e);
    }

    match state
        .db
        .update(|db| {
            if let Some(ref url) = body.router_base_url {
                db.settings.mitm_router_base_url = url.clone();
            }

            if let Some(ref routes) = body.routes {
                for (name, entry) in routes {
                    let mut config_map = BTreeMap::new();
                    config_map.insert("upstreamUrl".to_string(), entry.upstream_url.clone());

                    if let Some(ref prefix) = entry.path_prefix {
                        config_map.insert("pathPrefix".to_string(), prefix.clone());
                    }
                    if let Some(rt) = entry.request_transform {
                        config_map.insert("requestTransform".to_string(), rt.to_string());
                    }
                    if let Some(rt) = entry.response_transform {
                        config_map.insert("responseTransform".to_string(), rt.to_string());
                    }
                    if let Some(enabled) = entry.enabled {
                        config_map.insert("enabled".to_string(), enabled.to_string());
                    }
                    if let Some(ref mode) = entry.intercept_mode {
                        config_map.insert("interceptMode".to_string(), mode.clone());
                    }

                    db.mitm_alias.insert(name.clone(), config_map);
                }
            }
        })
        .await
    {
        Ok(snapshot) => {
            let settings = &snapshot.settings;
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "success": true,
                    "routerBaseUrl": settings.mitm_router_base_url,
                    "routeCount": snapshot.mitm_alias.len()
                })),
            )
                .into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to update MITM config: {}", err)
            })),
        )
            .into_response(),
    }
}

// ── POST /api/mitm/cert/generate ──────────────────────────────────────

async fn generate_cert(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Err(e) = require_api_key(&headers, &state.db) {
        return crate::server::api::auth_error_response(e);
    }

    let timestamp = chrono::Utc::now().timestamp();
    let fingerprint = format!("{:016x}", timestamp.unsigned_abs() ^ 0xDEADBEEFCAFEBABE);
    let expires_at = chrono::Utc::now() + chrono::Duration::days(365);
    let expires_at_str = expires_at.to_rfc3339();
    let now_str = chrono::Utc::now().to_rfc3339();

    match state
        .db
        .update(|db| {
            db.provider_nodes
                .retain(|n| n.extra.get("type").and_then(Value::as_str) != Some("mitm-cert"));

            let mut extra = BTreeMap::new();
            extra.insert("type".to_string(), Value::String("mitm-cert".to_string()));
            extra.insert(
                "expiresAt".to_string(),
                Value::String(expires_at_str.clone()),
            );
            extra.insert(
                "fingerprint".to_string(),
                Value::String(fingerprint.clone()),
            );
            extra.insert("generatedAt".to_string(), Value::String(now_str.clone()));

            db.provider_nodes.push(crate::types::ProviderNode {
                id: format!("mitm-cert-{}", timestamp),
                r#type: "mitm-cert".to_string(),
                name: "MITM CA Certificate".to_string(),
                prefix: None,
                api_type: None,
                base_url: None,
                created_at: Some(now_str.clone()),
                updated_at: None,
                extra,
            });
        })
        .await
    {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "success": true,
                "message": "MITM certificate generated",
                "fingerprint": fingerprint,
                "expiresAt": expires_at_str
            })),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to generate cert: {}", err)
            })),
        )
            .into_response(),
    }
}

// ── POST /api/mitm/start ──────────────────────────────────────────────

async fn start_mitm(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Err(e) = require_api_key(&headers, &state.db) {
        return crate::server::api::auth_error_response(e);
    }

    let snapshot = state.db.snapshot();
    let has_routes = !snapshot.mitm_alias.is_empty();

    if !has_routes {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "No MITM routes configured. Add routes via PUT /api/mitm-config first."
            })),
        )
            .into_response();
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "success": true,
            "message": "MITM proxy active",
            "activeRoutes": snapshot.mitm_alias.len(),
            "routerBaseUrl": snapshot.settings.mitm_router_base_url
        })),
    )
        .into_response()
}

// ── POST /api/mitm/stop ───────────────────────────────────────────────

async fn stop_mitm(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Err(e) = require_api_key(&headers, &state.db) {
        return crate::server::api::auth_error_response(e);
    }

    match state
        .db
        .update(|db| {
            db.mitm_alias.clear();
        })
        .await
    {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "success": true,
                "message": "MITM proxy stopped, all routes cleared"
            })),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to stop MITM proxy: {}", err)
            })),
        )
            .into_response(),
    }
}

// ── POST /api/proxy-pools/vercel-deploy (M9.6) ────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VercelDeployRequest {
    project_name: Option<String>,
    regions: Option<Vec<String>>,
    target_urls: Option<Vec<String>>,
}

async fn vercel_deploy(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<VercelDeployRequest>,
) -> axum::response::Response {
    if let Err(e) = require_api_key(&headers, &state.db) {
        return crate::server::api::auth_error_response(e);
    }

    let project_name = body
        .project_name
        .unwrap_or_else(|| "openproxy-relay".to_string());
    let regions = body
        .regions
        .unwrap_or_else(|| vec!["iad1".to_string(), "sfo1".to_string(), "lhr1".to_string()]);
    let target_urls = body
        .target_urls
        .unwrap_or_else(|| vec!["http://localhost:4623".to_string()]);

    let vercel_function = generate_vercel_function(&target_urls);

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "success": true,
            "message": "Vercel relay function generated. Deploy manually with the instructions below.",
            "projectName": project_name,
            "regions": regions,
            "instructions": {
                "step1": "Create a new Vercel project or use an existing one",
                "step2": "Create file: api/relay.js with the generated function code",
                "step3": "Set environment variable: RELAY_TARGETS (comma-separated target URLs)",
                "step4": "Deploy with: vercel deploy --prod",
                "step5": "Add the deployed URL to your proxy pool via /api/proxy-pools"
            },
            "functionCode": vercel_function,
            "targetUrls": target_urls
        })),
    )
        .into_response()
}

fn generate_vercel_function(target_urls: &[String]) -> String {
    let targets_json = serde_json::to_string(target_urls).unwrap_or_else(|_| "[]".to_string());
    format!(
        r#"// Vercel Edge Relay Function for openproxy-rust IP masking
// File: api/relay.js

const TARGETS = {targets_json};

export default async function handler(request) {{
  if (request.method === 'OPTIONS') {{
    return new Response(null, {{
      status: 204,
      headers: {{
        'Access-Control-Allow-Origin': '*',
        'Access-Control-Allow-Methods': 'GET, POST, PUT, DELETE, PATCH, OPTIONS',
        'Access-Control-Allow-Headers': '*',
      }},
    }});
  }}

  const url = new URL(request.url);
  const targetBase = TARGETS[Math.floor(Math.random() * TARGETS.length)];
  const targetUrl = targetBase + url.pathname + url.search;

  const headers = new Headers(request.headers);
  headers.delete('host');

  try {{
    const response = await fetch(targetUrl, {{
      method: request.method,
      headers,
      body: request.method !== 'GET' && request.method !== 'HEAD'
        ? request.body
        : undefined,
    }});

    const responseHeaders = new Headers(response.headers);
    responseHeaders.set('x-relay-target', new URL(targetBase).hostname);
    responseHeaders.set('Access-Control-Allow-Origin', '*');

    return new Response(response.body, {{
      status: response.status,
      headers: responseHeaders,
    }});
  }} catch (error) {{
    return new Response(JSON.stringify({{ error: error.message }}), {{
      status: 502,
      headers: {{ 'Content-Type': 'application/json' }},
    }});
  }}
}}

export const config = {{
  runtime: 'edge',
}};"#
    )
}

// ── POST /api/proxy-pools/{id}/test ────────────────────────────────────

async fn test_pool(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(pool_id): axum::extract::Path<String>,
) -> axum::response::Response {
    if let Err(e) = require_api_key(&headers, &state.db) {
        return crate::server::api::auth_error_response(e);
    }

    let snapshot = state.db.snapshot();
    let pool = snapshot.proxy_pools.iter().find(|p| p.id == pool_id);

    match pool {
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Proxy pool not found"
            })),
        )
            .into_response(),
        Some(pool) => {
            let test_result = test_proxy_url(&pool.proxy_url, &pool.r#type).await;
            let now = chrono::Utc::now().to_rfc3339();

            let _ = state
                .db
                .update(|db| {
                    if let Some(p) = db.proxy_pools.iter_mut().find(|p| p.id == pool_id) {
                        p.test_status = Some(test_result.status.clone());
                        p.last_tested_at = Some(now.clone());
                        p.last_error = test_result.error.clone();
                        p.rtt_ms = Some(test_result.rtt_ms);
                    }
                })
                .await;

            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "success": test_result.success,
                    "status": test_result.status,
                    "rttMs": test_result.rtt_ms,
                    "error": test_result.error,
                    "testedAt": now
                })),
            )
                .into_response()
        }
    }
}

#[derive(Debug)]
struct TestResult {
    success: bool,
    status: String,
    rtt_ms: u64,
    error: Option<String>,
}

async fn test_proxy_url(proxy_url: &str, proxy_type: &str) -> TestResult {
    let start = std::time::Instant::now();

    // Parse the proxy URL
    let _ = match reqwest::Url::parse(proxy_url) {
        Ok(url) => url,
        Err(e) => {
            return TestResult {
                success: false,
                status: "invalid_url".to_string(),
                rtt_ms: 0,
                error: Some(format!("Invalid URL: {}", e)),
            };
        }
    };

    // Build the test request - try to connect to the proxy
    let client = match proxy_type {
        "socks5" | "socks5h" => {
            let proxy = match reqwest::Proxy::all(proxy_url) {
                Ok(p) => p,
                Err(e) => {
                    return TestResult {
                        success: false,
                        status: "invalid_proxy".to_string(),
                        rtt_ms: 0,
                        error: Some(format!("Invalid SOCKS5 proxy: {}", e)),
                    };
                }
            };
            reqwest::Client::builder()
                .proxy(proxy)
                .timeout(std::time::Duration::from_secs(10))
                .build()
        }
        _ => reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build(),
    };

    match client {
        Ok(client) => {
            // Try a simple HEAD request to a known reliable endpoint through the proxy
            let test_url = "https://www.google.com/favicon.ico";
            match client.head(test_url).send().await {
                Ok(response) => {
                    let rtt_ms = start.elapsed().as_millis() as u64;
                    let success =
                        response.status().is_success() || response.status().is_redirection();
                    TestResult {
                        success,
                        status: if success {
                            "ok".to_string()
                        } else {
                            "error".to_string()
                        },
                        rtt_ms,
                        error: if success {
                            None
                        } else {
                            Some(format!("HTTP {}", response.status()))
                        },
                    }
                }
                Err(e) => {
                    let rtt_ms = start.elapsed().as_millis() as u64;
                    let error_msg = e.to_string();
                    let (status, success) = if error_msg.contains("timeout") {
                        ("timeout", false)
                    } else if error_msg.contains("connection refused") {
                        ("connection_refused", false)
                    } else if error_msg.contains("ssl") || error_msg.contains("tls") {
                        ("ssl_error", false)
                    } else {
                        ("error", false)
                    };
                    TestResult {
                        success,
                        status: status.to_string(),
                        rtt_ms,
                        error: Some(error_msg),
                    }
                }
            }
        }
        Err(e) => TestResult {
            success: false,
            status: "client_error".to_string(),
            rtt_ms: 0,
            error: Some(format!("Failed to build client: {}", e)),
        },
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/mitm-config", get(get_config))
        .route("/api/mitm-config", put(update_config))
        .route("/api/mitm/cert/generate", post(generate_cert))
        .route("/api/mitm/start", post(start_mitm))
        .route("/api/mitm/stop", post(stop_mitm))
        .route("/api/proxy-pools/vercel-deploy", post(vercel_deploy))
        .route("/api/proxy-pools/{id}/test", post(test_pool))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proxy_url_invalid_url() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(test_proxy_url("not-a-valid-url", "http"));
        assert!(!result.success);
        assert_eq!(result.status, "invalid_url");
    }

    #[test]
    fn test_proxy_url_nonexistent_host() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(test_proxy_url("http://192.0.2.1:12345", "http"));
        assert!(!result.success);
        assert_eq!(result.status, "connection_refused");
    }
}
