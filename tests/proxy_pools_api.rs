use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use jsonwebtoken::{encode, EncodingKey, Header as JwtHeader};
use once_cell::sync::Lazy;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection, ProxyPool};
use serde::Serialize;
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::{
    matchers::{body_json, header, method, path},
    Mock, MockServer, ResponseTemplate,
};

const TEST_KEY: &str = "proxy-pools-api-test-key";
const VERCEL_RELAY_FUNCTION_CODE: &str = r#"
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
static VERCEL_API_ENV_LOCK: Lazy<tokio::sync::Mutex<()>> =
    Lazy::new(|| tokio::sync::Mutex::new(()));

struct VercelApiEnvGuard {
    previous: Option<String>,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

impl Drop for VercelApiEnvGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.as_deref() {
            std::env::set_var("OPENPROXY_VERCEL_API_BASE_URL", previous);
        } else {
            std::env::remove_var("OPENPROXY_VERCEL_API_BASE_URL");
        }
    }
}

async fn set_vercel_api_base_url(base_url: &str) -> VercelApiEnvGuard {
    let lock = VERCEL_API_ENV_LOCK.lock().await;
    let previous = std::env::var("OPENPROXY_VERCEL_API_BASE_URL").ok();
    std::env::set_var("OPENPROXY_VERCEL_API_BASE_URL", base_url);
    VercelApiEnvGuard {
        previous,
        _lock: lock,
    }
}

fn active_key() -> ApiKey {
    ApiKey {
        id: "key-1".into(),
        name: "Local".into(),
        key: TEST_KEY.into(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        extra: BTreeMap::new(),
    }
}

fn provider_connection(id: &str, proxy_pool_id: &str) -> ProviderConnection {
    let mut provider_specific_data = BTreeMap::new();
    provider_specific_data.insert("proxyPoolId".into(), Value::String(proxy_pool_id.into()));

    ProviderConnection {
        id: id.into(),
        provider: "openai".into(),
        auth_type: "api_key".into(),
        name: Some(format!("Conn {id}")),
        priority: Some(1),
        is_active: Some(true),
        created_at: None,
        updated_at: None,
        display_name: None,
        email: None,
        global_priority: None,
        default_model: Some("gpt-4o-mini".into()),
        access_token: None,
        refresh_token: None,
        expires_at: None,
        token_type: None,
        scope: None,
        id_token: None,
        project_id: None,
        api_key: None,
        test_status: None,
        last_tested: None,
        last_error: None,
        last_error_at: None,
        rate_limited_until: None,
        expires_in: None,
        error_code: None,
        consecutive_use_count: None,
        backoff_level: None,
        consecutive_errors: None,
        proxy_url: None,
        proxy_label: None,
        use_connection_proxy: None,
        provider_specific_data,
        extra: BTreeMap::new(),
    }
}

fn proxy_pool(id: &str, name: &str, is_active: bool, updated_at: &str) -> ProxyPool {
    ProxyPool {
        id: id.into(),
        name: name.into(),
        proxy_url: format!("http://{id}.proxy.test:8080"),
        no_proxy: String::new(),
        r#type: "http".into(),
        is_active: Some(is_active),
        strict_proxy: Some(false),
        test_status: Some("unknown".into()),
        last_tested_at: None,
        last_error: None,
        success_rate: None,
        rtt_ms: None,
        total_requests: None,
        failed_requests: None,
        created_at: Some(updated_at.into()),
        updated_at: Some(updated_at.into()),
        extra: BTreeMap::new(),
    }
}

#[derive(Debug, Serialize)]
struct DashboardClaims {
    authenticated: bool,
    exp: usize,
}

fn dashboard_cookie() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as usize;
    let token = encode(
        &JwtHeader::default(),
        &DashboardClaims {
            authenticated: true,
            exp: now + 3600,
        },
        &EncodingKey::from_secret(b"9router-default-secret-change-me"),
    )
    .expect("dashboard token");
    format!("auth_token={token}")
}

async fn app_state(proxy_pools: Vec<ProxyPool>, connections: Vec<ProviderConnection>) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key()];
        state.proxy_pools = proxy_pools;
        state.provider_connections = connections;
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

async fn app_state_with_login(
    proxy_pools: Vec<ProxyPool>,
    connections: Vec<ProviderConnection>,
    require_login: bool,
) -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key()];
        state.proxy_pools = proxy_pools;
        state.provider_connections = connections;
        state.settings.require_login = require_login;
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

#[tokio::test]
async fn list_proxy_pools_filters_sorts_and_counts_usage() {
    let state = app_state(
        vec![
            proxy_pool("pool-1", "Primary", true, "2026-05-05T10:00:00Z"),
            proxy_pool("pool-2", "Backup", true, "2026-05-05T11:00:00Z"),
            proxy_pool("pool-3", "Disabled", false, "2026-05-05T12:00:00Z"),
        ],
        vec![
            provider_connection("conn-1", "pool-1"),
            provider_connection("conn-2", "pool-3"),
        ],
    )
    .await;
    let app = openproxy::build_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/proxy-pools?isActive=true&includeUsage=true")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let proxy_pools = json["proxyPools"].as_array().expect("proxyPools array");

    assert_eq!(proxy_pools.len(), 2);
    assert_eq!(proxy_pools[0]["id"], "pool-2");
    assert_eq!(proxy_pools[0]["boundConnectionCount"], 0);
    assert_eq!(proxy_pools[1]["id"], "pool-1");
    assert_eq!(proxy_pools[1]["boundConnectionCount"], 1);
}

#[tokio::test]
async fn create_proxy_pool_matches_js_defaults_and_shape() {
    let state = app_state(vec![], vec![]).await;
    let app = openproxy::build_app(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/proxy-pools")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": " Relay ",
                        "proxyUrl": " http://relay.proxy.test:8080 ",
                        "noProxy": " localhost,127.0.0.1 ",
                        "strictProxy": true,
                        "type": "invalid"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert!(json.get("success").is_none());
    let proxy_pool = &json["proxyPool"];
    assert_eq!(proxy_pool["name"], "Relay");
    assert_eq!(proxy_pool["proxyUrl"], "http://relay.proxy.test:8080");
    assert_eq!(proxy_pool["noProxy"], "localhost,127.0.0.1");
    assert_eq!(proxy_pool["type"], "http");
    assert_eq!(proxy_pool["isActive"], true);
    assert_eq!(proxy_pool["strictProxy"], true);
    assert_eq!(proxy_pool["testStatus"], "unknown");
    assert!(proxy_pool["createdAt"].is_string());
    assert!(proxy_pool["updatedAt"].is_string());

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.proxy_pools.len(), 1);
    assert_eq!(snapshot.proxy_pools[0].name, "Relay");
    assert_eq!(snapshot.proxy_pools[0].r#type, "http");
    assert_eq!(snapshot.proxy_pools[0].strict_proxy, Some(true));
}

#[tokio::test]
async fn update_proxy_pool_matches_js_normalization() {
    let state = app_state(
        vec![proxy_pool(
            "pool-1",
            "Primary",
            true,
            "2026-05-05T10:00:00Z",
        )],
        vec![],
    )
    .await;
    let app = openproxy::build_app(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/proxy-pools/pool-1")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": " Renamed Pool ",
                        "proxyUrl": " http://renamed.proxy.test:8080 ",
                        "noProxy": " localhost ",
                        "isActive": false,
                        "strictProxy": false,
                        "type": "not-real"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["proxyPool"]["name"], "Renamed Pool");
    assert_eq!(
        json["proxyPool"]["proxyUrl"],
        "http://renamed.proxy.test:8080"
    );
    assert_eq!(json["proxyPool"]["noProxy"], "localhost");
    assert_eq!(json["proxyPool"]["isActive"], false);
    assert_eq!(json["proxyPool"]["strictProxy"], false);
    assert_eq!(json["proxyPool"]["type"], "http");

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.proxy_pools[0].name, "Renamed Pool");
    assert_eq!(
        snapshot.proxy_pools[0].proxy_url,
        "http://renamed.proxy.test:8080"
    );
    assert_eq!(snapshot.proxy_pools[0].r#type, "http");
}

#[tokio::test]
async fn test_proxy_pool_vercel_matches_js_payload_and_dashboard_cookie_auth() {
    let relay = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .and(header("x-relay-target", "https://httpbin.org"))
        .and(header("x-relay-path", "/get"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&relay)
        .await;

    let mut pool = proxy_pool("pool-1", "Relay", false, "2026-05-05T10:00:00Z");
    pool.r#type = "vercel".into();
    pool.proxy_url = relay.uri();
    let state = app_state_with_login(vec![pool], vec![], true).await;
    let app = openproxy::build_app(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/proxy-pools/pool-1/test")
                .header("cookie", dashboard_cookie())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["ok"], true);
    assert_eq!(json["status"], 200);
    assert_eq!(json["statusText"], "OK");
    assert_eq!(json["error"], Value::Null);
    assert!(json["elapsedMs"].as_u64().is_some());
    assert!(json["testedAt"].as_str().is_some());

    let snapshot = state.db.snapshot();
    let pool = snapshot
        .proxy_pools
        .iter()
        .find(|pool| pool.id == "pool-1")
        .unwrap();
    assert_eq!(pool.test_status.as_deref(), Some("active"));
    assert_eq!(pool.last_error, None);
    assert_eq!(pool.is_active, Some(true));
    assert_eq!(pool.last_tested_at.as_deref(), json["testedAt"].as_str());
}

#[tokio::test]
async fn test_proxy_pool_failure_matches_js_response_shape_and_updates_db() {
    let mut pool = proxy_pool("pool-1", "Broken", true, "2026-05-05T10:00:00Z");
    pool.proxy_url = "http://127.0.0.1:1".into();
    let state = app_state(vec![pool], vec![]).await;
    let app = openproxy::build_app(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/proxy-pools/pool-1/test")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["ok"], false);
    assert_eq!(json["status"], 500);
    assert_eq!(json["statusText"], Value::Null);
    assert!(json["error"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert_eq!(json["elapsedMs"], 0);
    assert!(json["testedAt"].as_str().is_some());

    let snapshot = state.db.snapshot();
    let pool = snapshot
        .proxy_pools
        .iter()
        .find(|pool| pool.id == "pool-1")
        .unwrap();
    assert_eq!(pool.test_status.as_deref(), Some("error"));
    assert_eq!(pool.is_active, Some(false));
    assert_eq!(pool.last_tested_at.as_deref(), json["testedAt"].as_str());
    assert_eq!(pool.last_error.as_deref(), json["error"].as_str());
}

#[tokio::test]
async fn vercel_deploy_matches_js_flow_and_dashboard_cookie_auth() {
    let vercel = MockServer::start().await;
    let _guard = set_vercel_api_base_url(&vercel.uri()).await;

    Mock::given(method("POST"))
        .and(path("/v13/deployments"))
        .and(header("authorization", "Bearer vercel-token"))
        .and(body_json(json!({
            "name": "vercel-relay",
            "files": [
                {
                    "file": "api/relay.js",
                    "data": VERCEL_RELAY_FUNCTION_CODE
                },
                {
                    "file": "package.json",
                    "data": "{\"name\":\"vercel-relay\",\"version\":\"1.0.0\"}"
                },
                {
                    "file": "vercel.json",
                    "data": "{\"rewrites\":[{\"source\":\"/(.*)\",\"destination\":\"/api/relay\"}]}"
                }
            ],
            "projectSettings": {
                "framework": Value::Null
            },
            "target": "production"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "uid": "dep_123"
        })))
        .mount(&vercel)
        .await;

    Mock::given(method("PATCH"))
        .and(path("/v9/projects/vercel-relay"))
        .and(header("authorization", "Bearer vercel-token"))
        .and(body_json(json!({
            "ssoProtection": Value::Null
        })))
        .respond_with(ResponseTemplate::new(200))
        .mount(&vercel)
        .await;

    Mock::given(method("GET"))
        .and(path("/v13/deployments/dep_123"))
        .and(header("authorization", "Bearer vercel-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "readyState": "READY",
            "url": "vercel-relay.example.vercel.app"
        })))
        .mount(&vercel)
        .await;

    let state = app_state_with_login(vec![], vec![], true).await;
    let app = openproxy::build_app(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/proxy-pools/vercel-deploy")
                .header("cookie", dashboard_cookie())
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "vercelToken": "vercel-token",
                        "projectName": " vercel-relay "
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["deployUrl"], "https://vercel-relay.example.vercel.app");
    assert_eq!(json["proxyPool"]["name"], "vercel-relay");
    assert_eq!(
        json["proxyPool"]["proxyUrl"],
        "https://vercel-relay.example.vercel.app"
    );
    assert_eq!(json["proxyPool"]["type"], "vercel");
    assert_eq!(json["proxyPool"]["noProxy"], "");
    assert_eq!(json["proxyPool"]["isActive"], true);
    assert_eq!(json["proxyPool"]["strictProxy"], false);
    assert_eq!(json["proxyPool"]["testStatus"], "unknown");
    assert!(json["proxyPool"]["createdAt"].as_str().is_some());
    assert!(json["proxyPool"]["updatedAt"].as_str().is_some());

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.proxy_pools.len(), 1);
    let pool = &snapshot.proxy_pools[0];
    assert_eq!(pool.name, "vercel-relay");
    assert_eq!(pool.proxy_url, "https://vercel-relay.example.vercel.app");
    assert_eq!(pool.r#type, "vercel");
    assert_eq!(pool.no_proxy, "");
    assert_eq!(pool.is_active, Some(true));
    assert_eq!(pool.strict_proxy, Some(false));
    assert_eq!(pool.test_status.as_deref(), Some("unknown"));
}

#[tokio::test]
async fn vercel_deploy_returns_upstream_error_shape_without_creating_pool() {
    let vercel = MockServer::start().await;
    let _guard = set_vercel_api_base_url(&vercel.uri()).await;

    Mock::given(method("POST"))
        .and(path("/v13/deployments"))
        .and(header("authorization", "Bearer bad-token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {
                "message": "Unauthorized"
            }
        })))
        .mount(&vercel)
        .await;

    let state = app_state(vec![], vec![]).await;
    let app = openproxy::build_app(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/proxy-pools/vercel-deploy")
                .header("authorization", format!("Bearer {TEST_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "vercelToken": "bad-token",
                        "projectName": "vercel-relay"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["error"], "Unauthorized");
    assert!(state.db.snapshot().proxy_pools.is_empty());
}
