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
use std::sync::Arc;

use crate::core::mitm;
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

    let ca_dir = state.db.data_dir.join("mitm");
    let (cert_path, key_path) = match mitm::cert::generate_ca_persisted(&ca_dir) {
        Ok(paths) => paths,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to generate CA certificate: {e}")
                })),
            )
                .into_response();
        }
    };

    let cert_pem = match std::fs::read(&cert_path) {
        Ok(bytes) => bytes,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to read generated cert: {e}")
                })),
            )
                .into_response();
        }
    };

    let fingerprint = mitm::cert::sha256_fingerprint(&cert_pem);
    let now_str = chrono::Utc::now().to_rfc3339();
    let expires_at = chrono::Utc::now() + chrono::Duration::days(365);
    let expires_at_str = expires_at.to_rfc3339();
    let timestamp = chrono::Utc::now().timestamp();
    let cert_pem_string = String::from_utf8_lossy(&cert_pem).to_string();

    let update_result = state
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
            extra.insert(
                "certPath".to_string(),
                Value::String(cert_path.to_string_lossy().to_string()),
            );
            extra.insert(
                "keyPath".to_string(),
                Value::String(key_path.to_string_lossy().to_string()),
            );

            db.provider_nodes.push(crate::types::ProviderNode {
                id: format!("mitm-cert-{timestamp}"),
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
        .await;

    if let Err(err) = update_result {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to persist cert metadata: {err}")
            })),
        )
            .into_response();
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "success": true,
            "message": "MITM certificate generated",
            "certificate": cert_pem_string,
            "certPath": cert_path.to_string_lossy(),
            "keyPath": key_path.to_string_lossy(),
            "fingerprint": fingerprint,
            "expiresAt": expires_at_str,
            "generatedAt": now_str
        })),
    )
        .into_response()
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

    {
        let guard = state.mitm_handle.lock().await;
        if let Some(handle) = guard.as_ref() {
            return (
                StatusCode::OK,
                Json(serde_json::json!({
                    "success": true,
                    "message": "MITM proxy already running",
                    "port": handle.port(),
                    "bindAddr": handle.bind_addr(),
                    "activeRoutes": snapshot.mitm_alias.len(),
                    "routerBaseUrl": snapshot.settings.mitm_router_base_url
                })),
            )
                .into_response();
        }
    }

    let ca_dir = state.db.data_dir.join("mitm");
    let (cert_path, key_path) = match mitm::cert::generate_ca_persisted(&ca_dir) {
        Ok(paths) => paths,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to provision MITM CA: {e}")
                })),
            )
                .into_response();
        }
    };

    let cert_pem = match std::fs::read(&cert_path) {
        Ok(bytes) => bytes,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to read MITM CA cert: {e}")
                })),
            )
                .into_response();
        }
    };
    let key_pem = match std::fs::read(&key_path) {
        Ok(bytes) => bytes,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to read MITM CA key: {e}")
                })),
            )
                .into_response();
        }
    };

    let ca_material = match mitm::cert::generate_ca() {
        Ok(material) => material,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to load MITM CA in memory: {e}")
                })),
            )
                .into_response();
        }
    };

    let capture_dir = state.db.data_dir.join("mitm-captures");
    if let Err(e) = std::fs::create_dir_all(&capture_dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to create capture dir: {e}")
            })),
        )
            .into_response();
    }

    let config = mitm::server::MitmProxyConfig {
        ca_cert: Arc::new(ca_material.cert),
        ca_cert_pem: cert_pem.clone(),
        ca_key_pem: key_pem.clone(),
        capture_dir: capture_dir.clone(),
        state: state.clone(),
    };

    let handle = match mitm::server::start_mitm_proxy(config).await {
        Ok(handle) => handle,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to start MITM proxy listener: {e}")
                })),
            )
                .into_response();
        }
    };

    let port = handle.port();
    let bind_addr = handle.bind_addr().to_string();

    {
        let mut guard = state.mitm_handle.lock().await;
        *guard = Some(handle);
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "success": true,
            "message": "MITM proxy active",
            "port": port,
            "bindAddr": bind_addr,
            "activeRoutes": snapshot.mitm_alias.len(),
            "routerBaseUrl": snapshot.settings.mitm_router_base_url,
            "certPath": cert_path.to_string_lossy(),
            "captureDir": capture_dir.to_string_lossy()
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

    let stopped_port = {
        let mut guard = state.mitm_handle.lock().await;
        if let Some(mut handle) = guard.take() {
            let port = handle.port();
            handle.stop().await;
            Some(port)
        } else {
            None
        }
    };

    let clear_result = state
        .db
        .update(|db| {
            db.mitm_alias.clear();
        })
        .await;

    match (stopped_port, clear_result) {
        (Some(port), Ok(_)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "success": true,
                "message": "MITM proxy stopped, all routes cleared",
                "port": port
            })),
        )
            .into_response(),
        (None, Ok(_)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "success": true,
                "message": "MITM proxy was not running, all routes cleared"
            })),
        )
            .into_response(),
        (_, Err(err)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to stop MITM proxy: {err}")
            })),
        )
            .into_response(),
    }
}

// ── POST /api/proxy-pools/vercel-deploy (M9.6) ────────────────────────

const VERCEL_API: &str = "https://api.vercel.com";
const VERCEL_DEPLOY_POLL_INTERVAL_MS: u64 = 3_000;
const VERCEL_DEPLOY_POLL_MAX_MS: u64 = 120_000;
const RELAY_FUNCTION_CODE: &str = r#"
export const config = { runtime: "edge" };

export default async function handler(req) {
  const target = req.headers.get("x-relay-target");
  const relayPath = req.headers.get("x-relay-path") || "/";
  if (!target) {
    return new Response(JSON.stringify({ error: "Missing x-relay-target header" }), {
      status: 400,
      headers: { "content-type": "application/json" },
    });
  }

  const targetUrl = target.replace(/\/$/, "") + relayPath;

  const headers = new Headers(req.headers);
  headers.delete("x-relay-target");
  headers.delete("x-relay-path");
  headers.delete("host");

  const response = await fetch(targetUrl, {
    method: req.method,
    headers,
    body: req.method !== "GET" && req.method !== "HEAD" ? req.body : undefined,
    duplex: "half",
  });

  return new Response(response.body, {
    status: response.status,
    headers: response.headers,
  });
}
"#;

// ── POST /api/proxy-pools/cloudflare-deploy ────────────────────

const RELAY_WORKER_CODE_CLOUDFLARE: &str = r#"
export default {
  async fetch(request, env, ctx) {
    const target = request.headers.get("x-relay-target");
    const relayPath = request.headers.get("x-relay-path") || "/";

    if (!target) {
      return new Response(JSON.stringify({ error: "Missing x-relay-target header" }), {
        status: 400,
        headers: { "content-type": "application/json" },
      });
    }

    const targetUrl = target.replace(/\/$/, "") + relayPath;
    const newRequestInit = {
      method: request.method,
      headers: new Headers(request.headers),
    };

    if (request.method !== "GET" && request.method !== "HEAD") {
      newRequestInit.body = request.body;
      newRequestInit.duplex = "half";
    }

    newRequestInit.headers.delete("x-relay-target");
    newRequestInit.headers.delete("x-relay-path");
    newRequestInit.headers.delete("host");

    try {
      const response = await fetch(targetUrl, newRequestInit);
      return new Response(response.body, {
        status: response.status,
        headers: response.headers,
      });
    } catch (error) {
      return new Response(JSON.stringify({ error: error.message }), {
        status: 502,
        headers: { "content-type": "application/json" },
      });
    }
  },
};
"#;

// ── POST /api/proxy-pools/deno-deploy ────────────────────────────

const DENO_V2_API: &str = "https://api.deno.com/v2";

const DENO_RELAY_CODE: &str = r#"Deno.serve(async (request) => {
  const target = request.headers.get("x-relay-target");
  const relayPath = request.headers.get("x-relay-path") || "/";

  if (!target) {
    return new Response(JSON.stringify({ error: "Missing x-relay-target header" }), {
      status: 400,
      headers: { "content-type": "application/json" },
    });
  }

  const targetUrl = target.replace(/\/$/, "") + relayPath;
  const newHeaders = new Headers(request.headers);
  newHeaders.delete("x-relay-target");
  newHeaders.delete("x-relay-path");
  newHeaders.delete("host");

  const init = {
    method: request.method,
    headers: newHeaders,
  };

  if (request.method !== "GET" && request.method !== "HEAD") {
    init.body = request.body;
    init.duplex = "half";
  }

  try {
    const response = await fetch(targetUrl, init);
    return new Response(response.body, {
      status: response.status,
      headers: response.headers,
    });
  } catch (error) {
    return new Response(JSON.stringify({ error: error.message }), {
      status: 502,
      headers: { "content-type": "application/json" },
    });
  }
});"#;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VercelDeployRequest {
    vercel_token: Option<String>,
    project_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloudflareDeployRequest {
    cloudflare_account_id: Option<String>,
    cloudflare_api_token: Option<String>,
    project_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DenoDeployRequest {
    deno_token: Option<String>,
    org_domain: Option<String>,
    project_name: Option<String>,
}

async fn vercel_deploy(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<VercelDeployRequest>,
) -> axum::response::Response {
    use crate::types::ProxyPool;

    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let Some(vercel_token) = body
        .vercel_token
        .as_deref()
        .filter(|value| !value.is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Vercel API token is required"
            })),
        )
            .into_response();
    };

    let project_name = body
        .project_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(default_vercel_project_name);

    let client = reqwest::Client::new();
    let api_base_url = vercel_api_base_url();
    let deploy_res = match client
        .post(format!("{api_base_url}/v13/deployments"))
        .bearer_auth(vercel_token)
        .json(&vercel_deployment_payload(&project_name))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => return vercel_deploy_failed_response(error.to_string()),
    };

    if !deploy_res.status().is_success() {
        let status = StatusCode::from_u16(deploy_res.status().as_u16())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let error_body: Value = deploy_res
            .json()
            .await
            .unwrap_or_else(|_| serde_json::json!({}));
        let error_message = error_body
            .get("error")
            .and_then(|value| value.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("Failed to create Vercel deployment");

        return (
            status,
            Json(serde_json::json!({
                "error": error_message
            })),
        )
            .into_response();
    }

    let deployment: Value = match deploy_res.json().await {
        Ok(value) => value,
        Err(error) => return vercel_deploy_failed_response(error.to_string()),
    };
    let deployment_id = deployment
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            deployment
                .get("uid")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("undefined");
    let project_id = deployment
        .get("projectId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(project_name.as_str());

    if let Err(error) = client
        .patch(format!("{api_base_url}/v9/projects/{project_id}"))
        .bearer_auth(vercel_token)
        .json(&serde_json::json!({
            "ssoProtection": Value::Null
        }))
        .send()
        .await
    {
        return vercel_deploy_failed_response(error.to_string());
    }

    let ready = match poll_vercel_deployment(
        &client,
        &api_base_url,
        deployment_id,
        vercel_token,
        VERCEL_DEPLOY_POLL_MAX_MS,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return vercel_deploy_failed_response(error),
    };
    let ready_url = ready
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("undefined");
    let deploy_url = format!("https://{ready_url}");

    let now = chrono::Utc::now().to_rfc3339();
    let mut proxy_pool = ProxyPool::default();
    proxy_pool.id = uuid::Uuid::new_v4().to_string();
    proxy_pool.name = project_name.clone();
    proxy_pool.proxy_url = deploy_url.clone();
    proxy_pool.no_proxy = String::new();
    proxy_pool.r#type = "vercel".to_string();
    proxy_pool.is_active = Some(true);
    proxy_pool.strict_proxy = Some(false);
    proxy_pool.test_status = Some("unknown".to_string());
    proxy_pool.last_tested_at = None;
    proxy_pool.last_error = None;
    proxy_pool.created_at = Some(now.clone());
    proxy_pool.updated_at = Some(now);

    let save_result = state
        .db
        .update(|db| {
            db.proxy_pools.push(proxy_pool.clone());
        })
        .await;

    match save_result {
        Ok(_) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "proxyPool": proxy_pool,
                "deployUrl": deploy_url
            })),
        )
            .into_response(),
        Err(error) => vercel_deploy_failed_response(error.to_string()),
    }
}

fn vercel_api_base_url() -> String {
    std::env::var("OPENPROXY_VERCEL_API_BASE_URL")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| VERCEL_API.to_string())
}

fn default_vercel_project_name() -> String {
    let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    format!("relay-{}", to_base36(now_ms))
}

fn to_base36(mut value: u64) -> String {
    if value == 0 {
        return "0".to_string();
    }

    let mut digits = Vec::new();
    while value > 0 {
        let digit = (value % 36) as u8;
        digits.push(match digit {
            0..=9 => (b'0' + digit) as char,
            _ => (b'a' + (digit - 10)) as char,
        });
        value /= 36;
    }

    digits.into_iter().rev().collect()
}

fn vercel_deployment_payload(project_name: &str) -> serde_json::Value {
    serde_json::json!({
        "name": project_name,
        "files": [
            {
                "file": "api/relay.js",
                "data": RELAY_FUNCTION_CODE
            },
            {
                "file": "package.json",
                "data": serde_json::to_string(&serde_json::json!({
                    "name": project_name,
                    "version": "1.0.0"
                }))
                .unwrap_or_else(|_| "{\"name\":\"relay\",\"version\":\"1.0.0\"}".to_string())
            },
            {
                "file": "vercel.json",
                "data": r#"{"rewrites":[{"source":"/(.*)","destination":"/api/relay"}]}"#
            }
        ],
        "projectSettings": {
            "framework": Value::Null
        },
        "target": "production"
    })
}

async fn poll_vercel_deployment(
    client: &reqwest::Client,
    api_base_url: &str,
    deployment_id: &str,
    vercel_token: &str,
    max_ms: u64,
) -> Result<Value, String> {
    let started_at = std::time::Instant::now();

    while started_at.elapsed().as_millis() < u128::from(max_ms) {
        let response = client
            .get(format!("{api_base_url}/v13/deployments/{deployment_id}"))
            .bearer_auth(vercel_token)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let payload: Value = response.json().await.map_err(|error| error.to_string())?;

        match payload.get("readyState").and_then(Value::as_str) {
            Some("READY") => return Ok(payload),
            Some("ERROR") | Some("CANCELED") => {
                return Err(format!(
                    "Deployment failed: {}",
                    payload
                        .get("readyState")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                ))
            }
            _ => {}
        }

        tokio::time::sleep(std::time::Duration::from_millis(
            VERCEL_DEPLOY_POLL_INTERVAL_MS,
        ))
        .await;
    }

    Err("Deployment timed out".to_string())
}

fn vercel_deploy_failed_response(message: String) -> axum::response::Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({
            "error": if message.is_empty() { "Deploy failed" } else { &message }
        })),
    )
        .into_response()
}

// ── POST /api/proxy-pools/cloudflare-deploy ──────────────────────────

async fn cloudflare_deploy(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CloudflareDeployRequest>,
) -> axum::response::Response {
    use crate::types::ProxyPool;

    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let Some(account_id) = body
        .cloudflare_account_id
        .as_deref()
        .filter(|v| !v.is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Cloudflare Account ID is required"
            })),
        )
            .into_response();
    };

    let Some(api_token) = body
        .cloudflare_api_token
        .as_deref()
        .filter(|v| !v.is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Cloudflare API Token is required"
            })),
        )
            .into_response();
    };

    let project_name = body
        .project_name
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_else(default_vercel_project_name);

    let client = reqwest::Client::new();

    // 1. Upload Worker Script via multipart form
    let worker_script_url = format!(
        "https://api.cloudflare.com/client/v4/accounts/{}/workers/scripts/{}",
        account_id, project_name
    );

    let metadata_value = serde_json::json!({
        "main_module": "index.js",
        "compatibility_date": "2024-03-20",
        "observability": { "enabled": true }
    });

    let upload_form = reqwest::multipart::Form::new()
        .part(
            "index.js",
            reqwest::multipart::Part::stream(RELAY_WORKER_CODE_CLOUDFLARE.as_bytes().to_vec())
                .file_name("index.js")
                .mime_str("application/javascript+module")
                .unwrap(),
        )
        .part(
            "metadata",
            reqwest::multipart::Part::stream(
                serde_json::to_vec(&metadata_value).unwrap_or_default(),
            )
            .file_name("metadata.json")
            .mime_str("application/json")
            .unwrap(),
        );

    let upload_res = match client
        .put(&worker_script_url)
        .bearer_auth(api_token)
        .multipart(upload_form)
        .send()
        .await
    {
        Ok(res) => res,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to upload Worker to Cloudflare: {}", err)
                })),
            )
                .into_response();
        }
    };

    if !upload_res.status().is_success() {
        let status = StatusCode::from_u16(upload_res.status().as_u16())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let error_body: Value = upload_res
            .json()
            .await
            .unwrap_or_else(|_| serde_json::json!({}));
        let error_message = error_body
            .get("errors")
            .and_then(|arr| arr.as_array())
            .and_then(|arr| arr.first())
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("Failed to upload Worker to Cloudflare");

        return (
            status,
            Json(serde_json::json!({
                "error": error_message
            })),
        )
            .into_response();
    }

    // 2. Enable workers.dev subdomain for the script
    let _ = client
        .post(format!("{worker_script_url}/subdomain"))
        .bearer_auth(api_token)
        .json(&serde_json::json!({ "enabled": true }))
        .send()
        .await;

    // 3. Get the workers.dev subdomain for the account
    let deploy_url = match client
        .get(format!(
            "https://api.cloudflare.com/client/v4/accounts/{account_id}/workers/subdomain"
        ))
        .bearer_auth(api_token)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let subdomain_data: Value = resp.json().await.unwrap_or_default();
            subdomain_data
                .get("result")
                .and_then(|r| r.get("subdomain"))
                .and_then(Value::as_str)
                .map(|subdomain| format!("https://{project_name}.{subdomain}.workers.dev"))
                .unwrap_or_default()
        }
        _ => String::new(),
    };

    if deploy_url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Worker deployed but failed to retrieve workers.dev subdomain. Make sure you have setup a workers.dev subdomain in Cloudflare Dashboard."
            })),
        )
            .into_response();
    }

    // 4. Create proxy pool entry with type cloudflare
    let now = chrono::Utc::now().to_rfc3339();
    let mut proxy_pool = ProxyPool::default();
    proxy_pool.id = uuid::Uuid::new_v4().to_string();
    proxy_pool.name = project_name.clone();
    proxy_pool.proxy_url = deploy_url.clone();
    proxy_pool.no_proxy = String::new();
    proxy_pool.r#type = "cloudflare".to_string();
    proxy_pool.is_active = Some(true);
    proxy_pool.strict_proxy = Some(false);
    proxy_pool.test_status = Some("unknown".to_string());
    proxy_pool.last_tested_at = None;
    proxy_pool.last_error = None;
    proxy_pool.created_at = Some(now.clone());
    proxy_pool.updated_at = Some(now);

    let save_result = state
        .db
        .update(|db| {
            db.proxy_pools.push(proxy_pool.clone());
        })
        .await;

    match save_result {
        Ok(_) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "proxyPool": proxy_pool,
                "deployUrl": deploy_url
            })),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to save proxy pool: {}", err)
            })),
        )
            .into_response(),
    }
}

// ── POST /api/proxy-pools/deno-deploy ─────────────────────────────

fn deno_api_base_url() -> String {
    std::env::var("OPENPROXY_DENO_API_BASE_URL")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DENO_V2_API.to_string())
}

fn default_deno_project_name() -> String {
    let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    format!("relay-{}", to_base36(now_ms))
}

async fn deno_deploy(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<DenoDeployRequest>,
) -> axum::response::Response {
    use crate::types::ProxyPool;

    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let Some(deno_token) = body.deno_token.as_deref().filter(|value| !value.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Deno Deploy API token is required"
            })),
        )
            .into_response();
    };

    let Some(org_domain) = body.org_domain.as_deref().filter(|value| !value.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Organization domain is required"
            })),
        )
            .into_response();
    };

    let project_name = body
        .project_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(default_deno_project_name);

    let client = reqwest::Client::new();
    let api_base_url = deno_api_base_url();

    // 1. Create the app on Deno Deploy
    let create_app_res = match client
        .post(format!("{api_base_url}/apps"))
        .bearer_auth(deno_token)
        .json(&serde_json::json!({
            "slug": project_name,
            "labels": { "custom.kind": "9router-relay" },
            "config": {
                "install": "deno install",
                "runtime": {
                    "type": "dynamic",
                    "entrypoint": "main.ts",
                },
            },
        }))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to create Deno Deploy app: {}", error)
                })),
            )
                .into_response();
        }
    };

    if !create_app_res.status().is_success() {
        let status = StatusCode::from_u16(create_app_res.status().as_u16())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let error_text = create_app_res.text().await.unwrap_or_default();

        if status == StatusCode::CONFLICT {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": format!("App \"{}\" already exists. Choose a different name.", project_name)
                })),
            )
                .into_response();
        }

        return (
            status,
            Json(serde_json::json!({
                "error": format!("Failed to create app ({}): {}", status.as_u16(), error_text)
            })),
        )
            .into_response();
    }

    let app: Value = match create_app_res.json().await {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to parse app response: {}", error)
                })),
            )
                .into_response();
        }
    };

    let app_id = app
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("undefined")
        .to_string();

    // 2. Deploy the relay code to the app
    let deploy_res = match client
        .post(format!("{api_base_url}/apps/{app_id}/deploy"))
        .bearer_auth(deno_token)
        .json(&serde_json::json!({
            "assets": {
                "main.ts": {
                    "kind": "file",
                    "content": DENO_RELAY_CODE,
                    "encoding": "utf-8",
                },
            },
        }))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            // Cleanup: delete the created app
            let _ = client
                .delete(format!("{api_base_url}/apps/{app_id}"))
                .bearer_auth(deno_token)
                .send()
                .await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to deploy to Deno Deploy: {}", error)
                })),
            )
                .into_response();
        }
    };

    if !deploy_res.status().is_success() {
        let status = StatusCode::from_u16(deploy_res.status().as_u16())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let error_text = deploy_res.text().await.unwrap_or_default();
        // Cleanup: delete the created app
        let _ = client
            .delete(format!("{api_base_url}/apps/{app_id}"))
            .bearer_auth(deno_token)
            .send()
            .await;
        return (
            status,
            Json(serde_json::json!({
                "error": format!("Deploy failed ({}): {}", status.as_u16(), error_text)
            })),
        )
            .into_response();
    }

    // 3. Extract revision ID and poll for success
    let revision: Value = match deploy_res.json().await {
        Ok(value) => value,
        Err(error) => {
            let _ = client
                .delete(format!("{api_base_url}/apps/{app_id}"))
                .bearer_auth(deno_token)
                .send()
                .await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to parse deploy response: {}", error)
                })),
            )
                .into_response();
        }
    };

    let revision_id = revision
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("undefined");

    // Poll revision status every 2s up to 60s
    const POLL_INTERVAL_MS: u64 = 2_000;
    const POLL_MAX_MS: u64 = 60_000;
    let poll_started_at = std::time::Instant::now();

    let mut status = revision
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let mut deploy_succeeded = false;

    while (status == "queued" || status == "building")
        && poll_started_at.elapsed().as_millis() < u128::from(POLL_MAX_MS)
    {
        tokio::time::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS)).await;

        let status_res = match client
            .get(format!("{api_base_url}/revisions/{revision_id}"))
            .bearer_auth(deno_token)
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => break,
        };

        if !status_res.status().is_success() {
            break;
        }

        let status_data: Value = match status_res.json().await {
            Ok(value) => value,
            Err(_) => break,
        };

        status = status_data
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();

        if status == "succeeded" {
            deploy_succeeded = true;
            break;
        }

        if status == "failed" {
            break;
        }
    }

    if !deploy_succeeded {
        // Cleanup: delete the created app
        let _ = client
            .delete(format!("{api_base_url}/apps/{app_id}"))
            .bearer_auth(deno_token)
            .send()
            .await;

        if status == "failed" {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Deploy failed with status: failed"
                })),
            )
                .into_response();
        }

        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "Deploy timed out after 60 seconds"
            })),
        )
            .into_response();
    }

    // 4. Construct the deploy URL
    let org_slug = org_domain.split('.').next().unwrap_or(org_domain);
    let deploy_url = format!("https://{}.{}.deno.net", project_name, org_slug);

    // 5. Create proxy pool entry with type deno
    let now = chrono::Utc::now().to_rfc3339();
    let mut proxy_pool = ProxyPool::default();
    proxy_pool.id = uuid::Uuid::new_v4().to_string();
    proxy_pool.name = project_name.clone();
    proxy_pool.proxy_url = deploy_url.clone();
    proxy_pool.no_proxy = String::new();
    proxy_pool.r#type = "deno".to_string();
    proxy_pool.is_active = Some(true);
    proxy_pool.strict_proxy = Some(false);
    proxy_pool.test_status = Some("unknown".to_string());
    proxy_pool.last_tested_at = None;
    proxy_pool.last_error = None;
    proxy_pool.created_at = Some(now.clone());
    proxy_pool.updated_at = Some(now);

    let save_result = state
        .db
        .update(|db| {
            db.proxy_pools.push(proxy_pool.clone());
        })
        .await;

    match save_result {
        Ok(_) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "proxyPool": proxy_pool,
                "deployUrl": deploy_url
            })),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to save proxy pool: {}", err)
            })),
        )
            .into_response(),
    }
}

// ── POST /api/proxy-pools/{id}/test ────────────────────────────────────

async fn test_pool(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(pool_id): axum::extract::Path<String>,
) -> axum::response::Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
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
            let test_result = if pool.r#type == "vercel"
                || pool.r#type == "cloudflare"
                || pool.r#type == "deno"
            {
                test_vercel_relay(&pool.proxy_url, 10_000).await
            } else {
                test_proxy_url(&pool.proxy_url, None, None).await
            };
            let now = chrono::Utc::now().to_rfc3339();

            let last_error = if test_result.ok {
                None
            } else {
                Some(test_result.error.clone().unwrap_or_else(|| {
                    format!("Proxy test failed with status {}", test_result.status)
                }))
            };

            let update_result = state
                .db
                .update(|db| {
                    if let Some(p) = db.proxy_pools.iter_mut().find(|p| p.id == pool_id) {
                        p.test_status = Some(if test_result.ok {
                            "active".to_string()
                        } else {
                            "error".to_string()
                        });
                        p.last_tested_at = Some(now.clone());
                        p.last_error = last_error.clone();
                        p.is_active = Some(test_result.ok);
                    }
                })
                .await;

            if update_result.is_err() {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": "Failed to test proxy pool"
                    })),
                )
                    .into_response();
            }

            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "ok": test_result.ok,
                    "status": test_result.status,
                    "statusText": test_result.status_text,
                    "error": test_result.error,
                    "elapsedMs": test_result.elapsed_ms.unwrap_or(0),
                    "testedAt": now
                })),
            )
                .into_response()
        }
    }
}

#[derive(Debug)]
struct TestResult {
    ok: bool,
    status: u16,
    status_text: Option<String>,
    elapsed_ms: Option<u64>,
    error: Option<String>,
}

fn normalize_string(value: Option<&str>) -> String {
    value.unwrap_or_default().trim().to_string()
}

fn status_text(status: reqwest::StatusCode) -> Option<String> {
    status.canonical_reason().map(str::to_string)
}

async fn test_vercel_relay(relay_url: &str, timeout_ms: u64) -> TestResult {
    let started_at = std::time::Instant::now();
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return TestResult {
                ok: false,
                status: 500,
                status_text: None,
                elapsed_ms: None,
                error: Some(error.to_string()),
            }
        }
    };

    match client
        .get(relay_url)
        .header("x-relay-target", "https://httpbin.org")
        .header("x-relay-path", "/get")
        .send()
        .await
    {
        Ok(response) => TestResult {
            ok: response.status().is_success(),
            status: response.status().as_u16(),
            status_text: status_text(response.status()),
            elapsed_ms: Some(started_at.elapsed().as_millis() as u64),
            error: None,
        },
        Err(error) => TestResult {
            ok: false,
            status: 500,
            status_text: None,
            elapsed_ms: None,
            error: Some(if error.is_timeout() {
                "Relay test timed out".to_string()
            } else {
                error.to_string()
            }),
        },
    }
}

async fn test_proxy_url(
    proxy_url: &str,
    test_url: Option<&str>,
    timeout_ms: Option<u64>,
) -> TestResult {
    const DEFAULT_TEST_URL: &str = "https://google.com/";
    const DEFAULT_TIMEOUT_MS: u64 = 8_000;

    let normalized_proxy_url = normalize_string(Some(proxy_url));
    if normalized_proxy_url.is_empty() {
        return TestResult {
            ok: false,
            status: 400,
            status_text: None,
            elapsed_ms: None,
            error: Some("proxyUrl is required".to_string()),
        };
    }

    if let Err(error) = reqwest::Url::parse(&normalized_proxy_url) {
        return TestResult {
            ok: false,
            status: 400,
            status_text: None,
            elapsed_ms: None,
            error: Some(format!("Invalid proxy URL: {error}")),
        };
    }

    let normalized_test_url = normalize_string(test_url).chars().collect::<String>();
    let normalized_test_url = if normalized_test_url.is_empty() {
        DEFAULT_TEST_URL.to_string()
    } else {
        normalized_test_url
    };
    let normalized_timeout_ms = timeout_ms
        .filter(|value| *value > 0)
        .map(|value| value.min(30_000))
        .unwrap_or(DEFAULT_TIMEOUT_MS);

    let proxy = match reqwest::Proxy::all(&normalized_proxy_url) {
        Ok(proxy) => proxy,
        Err(error) => {
            return TestResult {
                ok: false,
                status: 400,
                status_text: None,
                elapsed_ms: None,
                error: Some(format!("Invalid proxy URL: {error}")),
            }
        }
    };

    let client = match reqwest::Client::builder()
        .proxy(proxy)
        .timeout(std::time::Duration::from_millis(normalized_timeout_ms))
        .user_agent("OpenProxy")
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return TestResult {
                ok: false,
                status: 500,
                status_text: None,
                elapsed_ms: None,
                error: Some(error.to_string()),
            }
        }
    };

    let started_at = std::time::Instant::now();
    match client.head(&normalized_test_url).send().await {
        Ok(response) => TestResult {
            ok: response.status().is_success(),
            status: response.status().as_u16(),
            status_text: status_text(response.status()),
            elapsed_ms: Some(started_at.elapsed().as_millis() as u64),
            error: None,
        },
        Err(error) => TestResult {
            ok: false,
            status: 500,
            status_text: None,
            elapsed_ms: None,
            error: Some(if error.is_timeout() {
                "Proxy test timed out".to_string()
            } else {
                error.to_string()
            }),
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
        .route(
            "/api/proxy-pools/cloudflare-deploy",
            post(cloudflare_deploy),
        )
        .route("/api/proxy-pools/deno-deploy", post(deno_deploy))
        .route("/api/proxy-pools/{id}/test", post(test_pool))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proxy_url_invalid_url() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(test_proxy_url("not-a-valid-url", None, None));
        assert!(!result.ok);
        assert_eq!(result.status, 400);
        assert!(result
            .error
            .as_deref()
            .unwrap_or_default()
            .starts_with("Invalid proxy URL: "));
    }

    #[test]
    fn test_proxy_url_nonexistent_host() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(test_proxy_url("http://192.0.2.1:12345", None, Some(100)));
        assert!(!result.ok);
        assert_eq!(result.status, 500);
        assert!(result.error.is_some());
    }
}
