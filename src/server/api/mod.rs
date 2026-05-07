pub mod admin_items;
mod auth;
pub mod chat;
pub mod cli_tools;
pub mod cloud_credentials;
pub mod cloud_sync;
pub mod compat;
pub mod locale;
pub mod media;
pub mod media_providers;
pub mod mitm_config;
pub mod models_alias;
pub mod models_availability;
pub mod models_custom;
pub mod models_disabled;
pub mod oauth;
pub mod pricing;
mod provider_connection_test;
mod provider_model_tests;
mod provider_models;
pub mod provider_nodes;
pub mod providers;
pub mod shutdown;
pub mod tags;
pub mod translator;
pub mod tunnel;
pub mod usage;
pub mod v1_api_chat;
pub mod v1_models;
pub mod v1beta;
pub mod web_fetch;

use std::collections::BTreeMap;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::server::auth::{extract_api_key, require_api_key, require_dashboard_session, AuthError};
use crate::server::state::AppState;
use crate::types::{AppDb, HealthResponse, ProviderConnection};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/api/health", get(api_health))
        .route("/v1/health", get(health))
        .merge(v1_api_chat::routes())
        .merge(v1_models::routes())
        .merge(v1beta::routes())
        .merge(web_fetch::routes())
        .route(
            "/v1/chat/completions",
            post(chat::chat_completions).options(chat::cors_options),
        )
        .route(
            "/api/dashboard/chat/completions",
            post(chat::dashboard_chat_completions),
        )
        .route(
            "/v1/messages",
            post(compat::messages).options(compat::cors_options),
        )
        .route(
            "/v1/messages/count_tokens",
            post(compat::count_tokens).options(compat::cors_options),
        )
        .route(
            "/v1/responses",
            post(compat::responses).options(compat::cors_options),
        )
        .route(
            "/v1/responses/compact",
            post(compat::responses_compact).options(compat::cors_options),
        )
        .route(
            "/v1/audio/transcriptions",
            post(media::audio_transcriptions).options(media::cors_options),
        )
        .route(
            "/v1/audio/speech",
            post(media::audio_speech).options(media::cors_options),
        )
        .route(
            "/v1/embeddings",
            post(media::embeddings).options(media::cors_options),
        )
        .route(
            "/v1/images/generations",
            post(media::images_generations).options(media::cors_options),
        )
        .route(
            "/v1/search",
            post(media::search).options(media::cors_options),
        )
        .merge(cloud_sync::routes())
        .merge(cloud_credentials::routes())
        .merge(locale::routes())
        .merge(models_disabled::routes())
        .merge(models_alias::routes())
        .merge(models_availability::routes())
        .merge(models_custom::routes())
        .merge(oauth::routes())
        .merge(media_providers::routes())
        .merge(mitm_config::routes())
        .merge(pricing::routes())
        .merge(tags::routes())
        .merge(tunnel::routes())
        .merge(translator::routes())
        .merge(providers::routes())
        .merge(provider_nodes::routes())
        .merge(admin_items::routes())
        .merge(usage::routes())
        // Dashboard API endpoints
        .route("/api/providers", get(list_providers_api))
        .route("/api/providers", post(create_provider_api))
        .route("/api/nodes", get(list_nodes_api))
        .route("/api/nodes", post(create_node_api))
        .route("/api/combos", get(list_combos_api))
        .route("/api/combos", post(create_combo_api))
        .route("/api/keys", get(list_keys_api))
        .route("/api/keys", post(create_key_api))
        .route("/api/proxy-pools", get(list_pools_api))
        .route("/api/proxy-pools", post(create_pool_api))
        .route(
            "/api/settings",
            get(get_settings_api)
                .put(update_settings_api)
                .patch(update_settings_api),
        )
        .route("/api/settings/proxy-test", post(proxy_test_api))
        .route("/api/version", get(get_version_api))
        .route(
            "/api/settings/database",
            get(settings_database_export_api).post(settings_database_import_api),
        )
        .route("/api/settings/require-login", get(get_require_login_api))
        .route("/api/db/export", get(export_db_api))
        .route("/api/observability/logs", get(get_logs_api))
        // Auth, shutdown, cli-tools APIs
        .merge(auth::routes())
        .merge(shutdown::routes())
        .merge(cli_tools::routes())
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse::new("api"))
}

async fn api_health() -> Response {
    Json(json!({ "ok": true })).into_response()
}

async fn get_version_api() -> Response {
    let current_version = dashboard_package_version().to_string();
    let latest_version = fetch_latest_dashboard_version().await;
    let has_update = latest_version
        .as_deref()
        .map(|latest| compare_semver_like(latest, &current_version) > 0)
        .unwrap_or(false);

    Json(json!({
        "currentVersion": current_version,
        "latestVersion": latest_version,
        "hasUpdate": has_update,
    }))
    .into_response()
}

fn dashboard_package_version() -> &'static str {
    static PACKAGE_JSON: &str = include_str!("../../../package.json");
    serde_json::from_str::<Value>(PACKAGE_JSON)
        .ok()
        .and_then(|value| {
            value
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .map(|version| Box::leak(version.into_boxed_str()) as &'static str)
        .unwrap_or(env!("CARGO_PKG_VERSION"))
}

async fn fetch_latest_dashboard_version() -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(4))
        .build()
        .ok()?;

    client
        .get("https://registry.npmjs.org/9router/latest")
        .send()
        .await
        .ok()?
        .json::<Value>()
        .await
        .ok()?
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn compare_semver_like(a: &str, b: &str) -> i32 {
    let parse = |input: &str| {
        input
            .split('.')
            .take(3)
            .map(|part| part.parse::<u32>().unwrap_or(0))
            .collect::<Vec<_>>()
    };

    let mut a_parts = parse(a);
    let mut b_parts = parse(b);
    while a_parts.len() < 3 {
        a_parts.push(0);
    }
    while b_parts.len() < 3 {
        b_parts.push(0);
    }

    for (left, right) in a_parts.iter().zip(b_parts.iter()) {
        if left > right {
            return 1;
        }
        if left < right {
            return -1;
        }
    }

    0
}

pub(super) fn require_management_api_key(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<(), Response> {
    require_api_key(headers, &state.db)
        .map(|_| ())
        .map_err(auth_error_response)
}

pub(super) fn require_dashboard_or_management_api_key(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<(), Response> {
    if extract_api_key(headers).is_some() {
        return require_management_api_key(headers, state);
    }

    match require_dashboard_session(headers, &state.db) {
        Ok(_) => Ok(()),
        Err(error) => Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": error.message() })),
        )
            .into_response()),
    }
}

pub(super) fn redact_provider_connection(connection: &ProviderConnection) -> ProviderConnection {
    let mut redacted = connection.clone();
    redacted.access_token = None;
    redacted.refresh_token = None;
    redacted.id_token = None;
    redacted.api_key = None;

    for secret_field in [
        "accessToken",
        "refreshToken",
        "idToken",
        "apiKey",
        "cookie",
        "password",
    ] {
        redacted.provider_specific_data.remove(secret_field);
    }

    redacted
}

fn safe_settings_payload(settings: &crate::types::Settings) -> Value {
    let mut value = serde_json::to_value(settings).unwrap_or_else(|_| json!({}));
    if let Some(fields) = value.as_object_mut() {
        let has_password = fields.remove("password").is_some();
        fields.insert(
            "enableRequestLogs".to_string(),
            Value::Bool(std::env::var("ENABLE_REQUEST_LOGS").ok().as_deref() == Some("true")),
        );
        fields.insert(
            "enableTranslator".to_string(),
            Value::Bool(std::env::var("ENABLE_TRANSLATOR").ok().as_deref() == Some("true")),
        );
        fields.insert("hasPassword".to_string(), Value::Bool(has_password));
    }

    value
}

// Provider CRUD API
async fn list_providers_api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let connections: Vec<_> = snapshot
        .provider_connections
        .iter()
        .map(redact_provider_connection)
        .collect();
    Json(json!({ "connections": connections })).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateProviderRequest {
    provider: String,
    name: Option<String>,
    api_key: Option<String>,
    priority: Option<u32>,
    global_priority: Option<u32>,
    default_model: Option<String>,
    test_status: Option<String>,
    provider_specific_data: Option<serde_json::Map<String, Value>>,
    connection_proxy_enabled: Option<bool>,
    connection_proxy_url: Option<String>,
    connection_no_proxy: Option<String>,
    proxy_pool_id: Option<Value>,
    base_url: Option<String>,
}

async fn create_provider_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateProviderRequest>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let provider = req.provider.trim();
    if provider.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "Provider is required" })),
        )
            .into_response();
    }

    let Some(name) = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "Name is required" })),
        )
            .into_response();
    };

    let Some(api_key) = req
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "API key is required" })),
        )
            .into_response();
    };

    let (connection_proxy_enabled, connection_proxy_url, connection_no_proxy) =
        match normalize_create_provider_proxy(&req) {
            Ok(proxy) => proxy,
            Err(response) => return response,
        };
    let proxy_pool_id = match normalize_create_provider_proxy_pool(
        &state.db.snapshot().proxy_pools,
        req.proxy_pool_id.as_ref(),
    ) {
        Ok(proxy_pool_id) => proxy_pool_id,
        Err(message) => return bad_request_response(&message),
    };

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let mut default_conn = ProviderConnection::default();
    default_conn.id = id;
    default_conn.provider = provider.to_string();
    default_conn.auth_type = "apikey".to_string();
    default_conn.name = Some(name);
    default_conn.priority = Some(req.priority.unwrap_or(1));
    default_conn.is_active = Some(true);
    default_conn.created_at = Some(now.clone());
    default_conn.updated_at = Some(now);
    default_conn.global_priority = req.global_priority;
    default_conn.default_model = req.default_model.filter(|value| !value.trim().is_empty());
    default_conn.api_key = Some(api_key);
    default_conn.test_status = Some(
        req.test_status
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "unknown".to_string()),
    );
    if let Some(provider_specific_data) = req.provider_specific_data {
        default_conn.provider_specific_data = provider_specific_data.into_iter().collect();
    }
    if let Some(base_url) = req
        .base_url
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        default_conn
            .provider_specific_data
            .insert("baseUrl".to_string(), Value::String(base_url));
    }
    if let Some(enabled) = connection_proxy_enabled {
        default_conn
            .provider_specific_data
            .insert("connectionProxyEnabled".to_string(), Value::Bool(enabled));
    }
    if let Some(url) = connection_proxy_url {
        default_conn
            .provider_specific_data
            .insert("connectionProxyUrl".to_string(), Value::String(url));
    }
    if let Some(no_proxy) = connection_no_proxy {
        default_conn
            .provider_specific_data
            .insert("connectionNoProxy".to_string(), Value::String(no_proxy));
    }
    if let Some(proxy_pool_id) = proxy_pool_id {
        default_conn
            .provider_specific_data
            .insert("proxyPoolId".to_string(), Value::String(proxy_pool_id));
    }

    let result = state
        .db
        .update(|db| {
            db.provider_connections.push(default_conn.clone());
        })
        .await;

    match result {
        Ok(_) => (
            StatusCode::CREATED,
            Json(json!({
                "success": true,
                "connection": redact_provider_connection(&default_conn)
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

// Node CRUD API
async fn list_nodes_api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    Json(json!({ "nodes": snapshot.provider_nodes.clone() })).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateNodeRequest {
    node_type: String,
    name: String,
    base_url: Option<String>,
    api_type: Option<String>,
}

async fn create_node_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateNodeRequest>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    use crate::types::ProviderNode;

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let node = ProviderNode {
        id,
        r#type: req.node_type,
        name: req.name,
        prefix: None,
        base_url: req.base_url,
        api_type: req.api_type,
        created_at: Some(now.clone()),
        updated_at: Some(now),
        extra: std::collections::BTreeMap::new(),
    };

    let result = state
        .db
        .update(|db| {
            db.provider_nodes.push(node.clone());
        })
        .await;

    match result {
        Ok(_) => (
            StatusCode::CREATED,
            Json(json!({ "success": true, "node": node })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

// Combo CRUD API
async fn list_combos_api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    Json(json!({ "combos": snapshot.combos.clone() })).into_response()
}

#[derive(Debug, Deserialize)]
struct CreateComboRequest {
    name: Option<String>,
    #[serde(default)]
    models: Vec<String>,
    kind: Option<String>,
}

async fn create_combo_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateComboRequest>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    use crate::types::Combo;

    let Some(name) = req.name.as_deref() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Name is required" })),
        )
            .into_response();
    };

    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Name is required" })),
        )
            .into_response();
    }

    if !admin_items::valid_combo_name(name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Name can only contain letters, numbers, -, _ and ." })),
        )
            .into_response();
    }

    if state
        .db
        .snapshot()
        .combos
        .iter()
        .any(|combo| combo.name == name)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Combo name already exists" })),
        )
            .into_response();
    }

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let combo = Combo {
        id,
        name: name.to_string(),
        models: req.models,
        kind: req.kind.filter(|kind| !kind.is_empty()),
        created_at: Some(now.clone()),
        updated_at: Some(now),
        extra: std::collections::BTreeMap::new(),
    };

    let result = state
        .db
        .update(|db| {
            db.combos.push(combo.clone());
        })
        .await;

    match result {
        Ok(_) => (StatusCode::CREATED, Json(combo)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

// API Key CRUD API
async fn list_keys_api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    Json(json!({ "keys": snapshot.api_keys.clone() })).into_response()
}

#[derive(Debug, Deserialize)]
struct CreateKeyRequest {
    name: Option<String>,
}

async fn create_key_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateKeyRequest>,
) -> Response {
    if !state.db.snapshot().api_keys.is_empty() {
        if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
            return response;
        }
    }

    use crate::types::ApiKey;

    let Some(name) = req.name.as_deref() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Name is required" })),
        )
            .into_response();
    };

    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Name is required" })),
        )
            .into_response();
    }

    let id = Uuid::new_v4().to_string();
    let machine_id = consistent_machine_id();
    let key = crate::core::auth::generate_api_key_with_machine(&machine_id);
    let now = chrono::Utc::now().to_rfc3339();

    let api_key = ApiKey {
        id,
        name: name.to_string(),
        key,
        machine_id: Some(machine_id),
        is_active: Some(true),
        created_at: Some(now),
        extra: std::collections::BTreeMap::new(),
    };

    let result = state
        .db
        .update(|db| {
            db.api_keys.push(api_key.clone());
        })
        .await;

    match result {
        Ok(_) => (
            StatusCode::CREATED,
            Json(json!({
                "key": api_key.key,
                "name": api_key.name,
                "id": api_key.id,
                "machineId": api_key.machine_id,
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

fn consistent_machine_id() -> String {
    let salt =
        std::env::var("MACHINE_ID_SALT").unwrap_or_else(|_| "endpoint-proxy-salt".to_string());

    match raw_machine_id() {
        Some(raw_machine_id) => {
            use sha2::Digest;

            let mut hasher = sha2::Sha256::new();
            hasher.update(raw_machine_id.as_bytes());
            hasher.update(salt.as_bytes());
            hex::encode(hasher.finalize())[..16].to_string()
        }
        None => Uuid::new_v4().to_string(),
    }
}

fn raw_machine_id() -> Option<String> {
    ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_bool_query(value: Option<&str>) -> Option<bool> {
    match value {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    }
}

// Proxy Pool CRUD API
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListPoolsQuery {
    is_active: Option<String>,
    include_usage: Option<String>,
}

async fn list_pools_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListPoolsQuery>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let mut proxy_pools = snapshot.proxy_pools.clone();

    if let Some(is_active) = parse_bool_query(query.is_active.as_deref()) {
        proxy_pools.retain(|proxy_pool| proxy_pool.is_active == Some(is_active));
    }

    proxy_pools.sort_by(|a, b| {
        b.updated_at
            .as_deref()
            .unwrap_or("")
            .cmp(a.updated_at.as_deref().unwrap_or(""))
    });

    if parse_bool_query(query.include_usage.as_deref()) == Some(true) {
        let usage_map = snapshot.provider_connections.iter().fold(
            std::collections::BTreeMap::<String, u64>::new(),
            |mut map, connection| {
                if let Some(proxy_pool_id) = connection
                    .provider_specific_data
                    .get("proxyPoolId")
                    .and_then(Value::as_str)
                {
                    *map.entry(proxy_pool_id.to_string()).or_insert(0) += 1;
                }
                map
            },
        );

        let proxy_pools = proxy_pools
            .into_iter()
            .map(|proxy_pool| {
                let mut value = serde_json::to_value(&proxy_pool).unwrap_or_else(|_| json!({}));
                if let Some(object) = value.as_object_mut() {
                    object.insert(
                        "boundConnectionCount".to_string(),
                        json!(usage_map.get(&proxy_pool.id).copied().unwrap_or(0)),
                    );
                }
                value
            })
            .collect::<Vec<_>>();

        return Json(json!({ "proxyPools": proxy_pools })).into_response();
    }

    Json(json!({ "proxyPools": proxy_pools })).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatePoolRequest {
    name: Option<String>,
    proxy_url: Option<String>,
    no_proxy: Option<String>,
    is_active: Option<bool>,
    strict_proxy: Option<bool>,
    r#type: Option<String>,
}

async fn create_pool_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreatePoolRequest>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    use crate::types::ProxyPool;

    let Some(name) = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Name is required" })),
        )
            .into_response();
    };

    let Some(proxy_url) = req
        .proxy_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Proxy URL is required" })),
        )
            .into_response();
    };

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let mut pool = ProxyPool::default();
    pool.id = id;
    pool.name = name.to_string();
    pool.proxy_url = proxy_url.to_string();
    pool.no_proxy = req.no_proxy.unwrap_or_default().trim().to_string();
    pool.r#type = match req.r#type.as_deref() {
        Some("http" | "vercel") => req.r#type.unwrap(),
        _ => "http".to_string(),
    };
    pool.is_active = Some(req.is_active.unwrap_or(true));
    pool.strict_proxy = Some(req.strict_proxy.unwrap_or(false));
    pool.test_status = Some("unknown".to_string());
    pool.last_tested_at = None;
    pool.last_error = None;
    pool.created_at = Some(now.clone());
    pool.updated_at = Some(now);

    let result = state
        .db
        .update(|db| {
            db.proxy_pools.push(pool.clone());
        })
        .await;

    match result {
        Ok(_) => (StatusCode::CREATED, Json(json!({ "proxyPool": pool }))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

// Settings API
async fn get_settings_api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    Json(safe_settings_payload(&snapshot.settings)).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSettingsRequest {
    tunnel_provider: Option<String>,
    sticky_round_robin_limit: Option<u32>,
    provider_strategies: Option<BTreeMap<String, String>>,
    combo_strategy: Option<String>,
    combo_strategies: Option<BTreeMap<String, String>>,
    mitm_router_base_url: Option<String>,
    require_login: Option<bool>,
    rtk_enabled: Option<bool>,
    caveman_enabled: Option<bool>,
    caveman_level: Option<String>,
    observability_enabled: Option<bool>,
    cloud_enabled: Option<bool>,
    cloud_url: Option<String>,
    tunnel_enabled: Option<bool>,
    tunnel_url: Option<String>,
    tailscale_enabled: Option<bool>,
    tailscale_url: Option<String>,
    tunnel_dashboard_access: Option<bool>,
    outbound_proxy_enabled: Option<bool>,
    outbound_proxy_url: Option<String>,
    outbound_no_proxy: Option<String>,
    new_password: Option<String>,
    current_password: Option<String>,
}

async fn update_settings_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpdateSettingsRequest>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    if req.new_password.is_some() || req.current_password.is_some() {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({ "error": "Password updates are not implemented in the Rust server yet" })),
        )
            .into_response();
    }

    let result = state
        .db
        .update(|db| {
            if let Some(v) = req.tunnel_provider {
                db.settings.tunnel_provider = v;
            }
            if let Some(v) = req.sticky_round_robin_limit {
                db.settings.sticky_round_robin_limit = v;
            }
            if let Some(v) = req.provider_strategies {
                db.settings.provider_strategies = v;
            }
            if let Some(v) = req.combo_strategy {
                db.settings.combo_strategy = v;
            }
            if let Some(v) = req.combo_strategies {
                db.settings.combo_strategies = v;
            }
            if let Some(v) = req.mitm_router_base_url {
                db.settings.mitm_router_base_url = v;
            }
            if let Some(v) = req.require_login {
                db.settings.require_login = v;
            }
            if let Some(v) = req.rtk_enabled {
                db.settings.rtk_enabled = v;
            }
            if let Some(v) = req.caveman_enabled {
                db.settings.caveman_enabled = v;
            }
            if let Some(v) = req.caveman_level {
                db.settings.caveman_level = v;
            }
            if let Some(v) = req.observability_enabled {
                db.settings.observability_enabled = v;
            }
            if let Some(v) = req.cloud_enabled {
                db.settings.cloud_enabled = v;
            }
            if let Some(v) = req.cloud_url {
                db.settings.cloud_url = v;
            }
            if let Some(v) = req.tunnel_enabled {
                db.settings.tunnel_enabled = v;
            }
            if let Some(v) = req.tunnel_url {
                db.settings.tunnel_url = v;
            }
            if let Some(v) = req.tailscale_enabled {
                db.settings.tailscale_enabled = v;
            }
            if let Some(v) = req.tailscale_url {
                db.settings.tailscale_url = v;
            }
            if let Some(v) = req.tunnel_dashboard_access {
                db.settings.tunnel_dashboard_access = v;
            }
            if let Some(v) = req.outbound_proxy_enabled {
                db.settings.outbound_proxy_enabled = v;
            }
            if let Some(v) = req.outbound_proxy_url {
                db.settings.outbound_proxy_url = v;
            }
            if let Some(v) = req.outbound_no_proxy {
                db.settings.outbound_no_proxy = v;
            }
            db.settings.normalize();
        })
        .await;

    match result {
        Ok(snapshot) => Json(safe_settings_payload(&snapshot.settings)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

// DB Export API
async fn export_db_api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let val = serde_json::to_value(snapshot.as_ref()).unwrap_or(json!({}));
    Json(val).into_response()
}

async fn settings_database_export_api(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    export_db_api(State(state), headers).await
}

async fn settings_database_import_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if let Err(response) = require_management_api_key(&headers, &state) {
        return response;
    }

    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid database payload" })),
            )
                .into_response()
        }
    };

    if !body.is_object() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid database payload" })),
        )
            .into_response();
    }

    let imported = AppDb::from_json_value(body);

    match state
        .db
        .update(move |db| {
            // Merge: only overwrite collections that are explicitly present in the import payload.
            // This prevents accidentally wiping providers/nodes/aliases when the caller
            // only intends to update settings or apiKeys.
            if !imported.provider_connections.is_empty() {
                db.provider_connections = imported.provider_connections.clone();
            }
            if !imported.provider_nodes.is_empty() {
                db.provider_nodes = imported.provider_nodes.clone();
            }
            if !imported.api_keys.is_empty() {
                db.api_keys = imported.api_keys.clone();
            }
            if !imported.combos.is_empty() {
                db.combos = imported.combos.clone();
            }
            if !imported.proxy_pools.is_empty() {
                db.proxy_pools = imported.proxy_pools.clone();
            }
            if !imported.custom_models.is_empty() {
                db.custom_models = imported.custom_models.clone();
            }
            if !imported.model_aliases.is_empty() {
                db.model_aliases = imported.model_aliases.clone();
            }
            if !imported.pricing.is_empty() {
                db.pricing = imported.pricing.clone();
            }
            // Settings: always merge individual fields from import
            merge_settings(&mut db.settings, &imported.settings);
        })
        .await
    {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

/// Merge settings field-by-field: only overwrite what the import explicitly provides.
fn merge_settings(target: &mut crate::types::Settings, source: &crate::types::Settings) {
    if source.cloud_enabled != target.cloud_enabled {
        target.cloud_enabled = source.cloud_enabled;
    }
    if source.cloud_url != target.cloud_url {
        target.cloud_url = source.cloud_url.clone();
    }
    if source.tunnel_enabled != target.tunnel_enabled {
        target.tunnel_enabled = source.tunnel_enabled;
    }
    if source.tunnel_url != target.tunnel_url {
        target.tunnel_url = source.tunnel_url.clone();
    }
    if source.tunnel_provider != target.tunnel_provider {
        target.tunnel_provider = source.tunnel_provider.clone();
    }
    if source.tailscale_enabled != target.tailscale_enabled {
        target.tailscale_enabled = source.tailscale_enabled;
    }
    if source.tailscale_url != target.tailscale_url {
        target.tailscale_url = source.tailscale_url.clone();
    }
    if source.require_login != target.require_login {
        target.require_login = source.require_login;
    }
    if source.tunnel_dashboard_access != target.tunnel_dashboard_access {
        target.tunnel_dashboard_access = source.tunnel_dashboard_access;
    }
    if source.provider_strategies != target.provider_strategies {
        target.provider_strategies = source.provider_strategies.clone();
    }
    if source.combo_strategy != target.combo_strategy {
        target.combo_strategy = source.combo_strategy.clone();
    }
    if source.combo_strategies != target.combo_strategies {
        target.combo_strategies = source.combo_strategies.clone();
    }
    if source.observability_enabled != target.observability_enabled {
        target.observability_enabled = source.observability_enabled;
    }
    if source.outbound_proxy_enabled != target.outbound_proxy_enabled {
        target.outbound_proxy_enabled = source.outbound_proxy_enabled;
    }
    if source.outbound_proxy_url != target.outbound_proxy_url {
        target.outbound_proxy_url = source.outbound_proxy_url.clone();
    }
    if source.outbound_no_proxy != target.outbound_no_proxy {
        target.outbound_no_proxy = source.outbound_no_proxy.clone();
    }
    if source.rtk_enabled != target.rtk_enabled {
        target.rtk_enabled = source.rtk_enabled;
    }
    if source.caveman_enabled != target.caveman_enabled {
        target.caveman_enabled = source.caveman_enabled;
    }
    if source.caveman_level != target.caveman_level {
        target.caveman_level = source.caveman_level.clone();
    }
    if source.sticky_round_robin_limit != target.sticky_round_robin_limit {
        target.sticky_round_robin_limit = source.sticky_round_robin_limit;
    }
    // Merge extra fields from import
    for (key, value) in &source.extra {
        target.extra.insert(key.clone(), value.clone());
    }
    target.normalize();
}

async fn get_require_login_api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    Json(json!({
        "requireLogin": snapshot.settings.require_login,
        "tunnelDashboardAccess": snapshot.settings.tunnel_dashboard_access,
        "tunnelUrl": snapshot.settings.tunnel_url,
        "tailscaleUrl": snapshot.settings.tailscale_url,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProxyTestRequest {
    proxy_url: Option<String>,
    test_url: Option<String>,
    timeout_ms: Option<u64>,
}

async fn proxy_test_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ProxyTestRequest>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let Some(proxy_url) = req
        .proxy_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "proxyUrl is required" })),
        )
            .into_response();
    };

    let test_url = req
        .test_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("https://google.com/");
    let timeout_ms = req.timeout_ms.unwrap_or(8_000).clamp(1, 30_000);

    let proxy = match reqwest::Proxy::all(proxy_url) {
        Ok(proxy) => proxy,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "ok": false,
                    "error": format!("Invalid proxy URL: {error}"),
                })),
            )
                .into_response()
        }
    };

    let client = match reqwest::Client::builder()
        .proxy(proxy)
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .user_agent("openproxy")
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": error.to_string() })),
            )
                .into_response()
        }
    };

    let started_at = std::time::Instant::now();
    match client.head(test_url).send().await {
        Ok(response) => Json(json!({
            "ok": response.status().is_success(),
            "status": response.status().as_u16(),
            "statusText": response.status().canonical_reason().unwrap_or(""),
            "url": test_url,
            "elapsedMs": started_at.elapsed().as_millis() as u64,
        }))
        .into_response(),
        Err(error) => {
            let message = if error.is_timeout() {
                "Proxy test timed out".to_string()
            } else {
                error.to_string()
            };
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": message })),
            )
                .into_response()
        }
    }
}

// Logs API (observability)
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LogEntry {
    timestamp: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    endpoint: Option<String>,
    tokens: Option<u64>,
    cost: Option<f64>,
}

async fn get_logs_api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_management_api_key(&headers, &state) {
        return response;
    }

    Json(Vec::<LogEntry>::new()).into_response()
}

pub(super) fn auth_error_response(error: AuthError) -> Response {
    let status = StatusCode::UNAUTHORIZED;
    (
        status,
        Json(json!({
            "error": {
                "message": error.message(),
                "type": "authentication_error",
                "code": "invalid_api_key"
            }
        })),
    )
        .into_response()
}

fn bad_request_response(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "success": false, "error": message })),
    )
        .into_response()
}

fn normalize_create_provider_proxy(
    req: &CreateProviderRequest,
) -> Result<(Option<bool>, Option<String>, Option<String>), Response> {
    let has_proxy_fields = req.connection_proxy_enabled.is_some()
        || req.connection_proxy_url.is_some()
        || req.connection_no_proxy.is_some();

    if !has_proxy_fields {
        return Ok((None, None, None));
    }

    let enabled = req.connection_proxy_enabled.unwrap_or(false);
    let url = req
        .connection_proxy_url
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let no_proxy = req
        .connection_no_proxy
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();

    if enabled && url.is_empty() {
        return Err(bad_request_response(
            "Connection proxy URL is required when connection proxy is enabled",
        ));
    }

    Ok((Some(enabled), Some(url), Some(no_proxy)))
}

fn normalize_create_provider_proxy_pool(
    proxy_pools: &[crate::types::ProxyPool],
    proxy_pool_id_input: Option<&Value>,
) -> Result<Option<String>, String> {
    let Some(proxy_pool_id_input) = proxy_pool_id_input else {
        return Ok(None);
    };

    if proxy_pool_id_input.is_null() {
        return Ok(None);
    }

    let raw = proxy_pool_id_input
        .as_str()
        .map(str::trim)
        .unwrap_or_default();

    if raw.is_empty() || raw == "__none__" {
        return Ok(None);
    }

    if !proxy_pools.iter().any(|proxy_pool| proxy_pool.id == raw) {
        return Err("Proxy pool not found".into());
    }

    Ok(Some(raw.to_string()))
}
