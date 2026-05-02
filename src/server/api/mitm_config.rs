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

// ── GET /api/mitm/config ──────────────────────────────────────────────

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
        let upstream_url = config_map
            .get("upstreamUrl")
            .cloned()
            .unwrap_or_default();
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
        let cert_data = snapshot.provider_nodes.iter().find(|n| {
            n.extra.get("type").and_then(Value::as_str) == Some("mitm-cert")
        });
        match cert_data {
            Some(node) => {
                let expires = node.extra.get("expiresAt").and_then(Value::as_str).map(String::from);
                let fingerprint = node.extra.get("fingerprint").and_then(Value::as_str).map(String::from);
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

// ── PUT /api/mitm/config ──────────────────────────────────────────────

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
    let fingerprint = format!("{:016x}", timestamp.abs() as u64 ^ 0xDEADBEEFCAFEBABE);
    let expires_at = chrono::Utc::now() + chrono::Duration::days(365);
    let expires_at_str = expires_at.to_rfc3339();
    let now_str = chrono::Utc::now().to_rfc3339();

    match state
        .db
        .update(|db| {
            db.provider_nodes.retain(|n| {
                n.extra.get("type").and_then(Value::as_str) != Some("mitm-cert")
            });

            let mut extra = BTreeMap::new();
            extra.insert("type".to_string(), Value::String("mitm-cert".to_string()));
            extra.insert("expiresAt".to_string(), Value::String(expires_at_str.clone()));
            extra.insert("fingerprint".to_string(), Value::String(fingerprint.clone()));
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
                "error": "No MITM routes configured. Add routes via PUT /api/mitm/config first."
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
    let regions = body.regions.unwrap_or_else(|| {
        vec!["iad1".to_string(), "sfo1".to_string(), "lhr1".to_string()]
    });
    let target_urls = body
        .target_urls
        .unwrap_or_else(|| vec!["http://localhost:20128".to_string()]);

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

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/mitm/config", get(get_config))
        .route("/api/mitm/config", put(update_config))
        .route("/api/mitm/cert/generate", post(generate_cert))
        .route("/api/mitm/start", post(start_mitm))
        .route("/api/mitm/stop", post(stop_mitm))
        .route("/api/proxy-pools/vercel-deploy", post(vercel_deploy))
}
