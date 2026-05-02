mod chat;
mod media;
mod oauth;
mod cloud_sync;
mod mitm_config;
mod media_providers;

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::server::auth::{require_api_key, AuthError};
use crate::server::state::AppState;
use crate::types::{HealthResponse, ProviderConnection, Settings};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/v1/health", get(health))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat::chat_completions))
        .route("/v1/audio/transcriptions", post(media::audio_transcriptions))
        .route("/v1/audio/speech", post(media::audio_speech))
        .route("/v1/embeddings", post(media::embeddings))
        .route("/v1/images/generations", post(media::images_generations))
        .route("/v1/search", post(media::search))
        .merge(cloud_sync::routes())
        .merge(oauth::routes())
        .merge(media_providers::routes())
        .merge(mitm_config::routes())
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
        .route("/api/settings", get(get_settings_api))
        .route("/api/settings", put(update_settings_api))
        .route("/api/db/export", get(export_db_api))
        .route("/api/observability/logs", get(get_logs_api))
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse::new("api"))
}

// Provider CRUD API
async fn list_providers_api(State(state): State<AppState>) -> Json<Vec<ProviderConnection>> {
    let snapshot = state.db.snapshot();
    Json(snapshot.provider_connections.clone())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateProviderRequest {
    provider: String,
    name: Option<String>,
    api_key: Option<String>,
    #[allow(dead_code)]
    base_url: Option<String>,
}

async fn create_provider_api(
    State(state): State<AppState>,
    Json(req): Json<CreateProviderRequest>,
) -> Json<Value> {
    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let mut default_conn = ProviderConnection::default();
    default_conn.id = id;
    default_conn.provider = req.provider;
    default_conn.auth_type = "api_key".to_string();
    default_conn.name = req.name;
    default_conn.priority = Some(100);
    default_conn.is_active = Some(true);
    default_conn.created_at = Some(now.clone());
    default_conn.updated_at = Some(now);
    default_conn.api_key = req.api_key;
    
    let result = state
        .db
        .update(|db| {
            db.provider_connections.push(default_conn);
        })
        .await;
    
    match result {
        Ok(_) => Json(json!({ "success": true, "message": "Provider created" })),
        Err(e) => Json(json!({ "success": false, "error": e.to_string() })),
    }
}

// Node CRUD API
async fn list_nodes_api(State(state): State<AppState>) -> Json<Value> {
    let snapshot = state.db.snapshot();
    Json(serde_json::to_value(&snapshot.provider_nodes).unwrap_or(json!([])))
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
    Json(req): Json<CreateNodeRequest>,
) -> Json<Value> {
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
            db.provider_nodes.push(node);
        })
        .await;
    
    match result {
        Ok(_) => Json(json!({ "success": true, "message": "Node created" })),
        Err(e) => Json(json!({ "success": false, "error": e.to_string() })),
    }
}

// Combo CRUD API
async fn list_combos_api(State(state): State<AppState>) -> Json<Value> {
    let snapshot = state.db.snapshot();
    Json(serde_json::to_value(&snapshot.combos).unwrap_or(json!([])))
}

#[derive(Debug, Deserialize)]
struct CreateComboRequest {
    name: String,
    models: Vec<String>,
    kind: Option<String>,
}

async fn create_combo_api(
    State(state): State<AppState>,
    Json(req): Json<CreateComboRequest>,
) -> Json<Value> {
    use crate::types::Combo;
    
    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    
    let combo = Combo {
        id,
        name: req.name,
        models: req.models,
        kind: req.kind.or_else(|| Some("ensemble".to_string())),
        created_at: Some(now.clone()),
        updated_at: Some(now),
        extra: std::collections::BTreeMap::new(),
    };
    
    let result = state
        .db
        .update(|db| {
            db.combos.push(combo);
        })
        .await;
    
    match result {
        Ok(_) => Json(json!({ "success": true, "message": "Combo created" })),
        Err(e) => Json(json!({ "success": false, "error": e.to_string() })),
    }
}

// API Key CRUD API
async fn list_keys_api(State(state): State<AppState>) -> Json<Value> {
    let snapshot = state.db.snapshot();
    Json(serde_json::to_value(&snapshot.api_keys).unwrap_or(json!([])))
}

#[derive(Debug, Deserialize)]
struct CreateKeyRequest {
    name: String,
}

async fn create_key_api(
    State(state): State<AppState>,
    Json(req): Json<CreateKeyRequest>,
) -> Json<Value> {
    use crate::types::ApiKey;
    
    let id = Uuid::new_v4().to_string();
    let key = format!("sk-op-{}", Uuid::new_v4().to_string().replace("-", ""));
    let now = chrono::Utc::now().to_rfc3339();
    
    let api_key = ApiKey {
        id,
        name: req.name,
        key,
        machine_id: None,
        is_active: Some(true),
        created_at: Some(now),
        extra: std::collections::BTreeMap::new(),
    };
    
    let result = state
        .db
        .update(|db| {
            db.api_keys.push(api_key);
        })
        .await;
    
    match result {
        Ok(_) => Json(json!({ "success": true, "message": "API key created" })),
        Err(e) => Json(json!({ "success": false, "error": e.to_string() })),
    }
}

// Proxy Pool CRUD API
async fn list_pools_api(State(state): State<AppState>) -> Json<Value> {
    let snapshot = state.db.snapshot();
    Json(serde_json::to_value(&snapshot.proxy_pools).unwrap_or(json!([])))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatePoolRequest {
    name: String,
    proxy_url: String,
    pool_type: Option<String>,
}

async fn create_pool_api(
    State(state): State<AppState>,
    Json(req): Json<CreatePoolRequest>,
) -> Json<Value> {
    use crate::types::ProxyPool;
    
    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let mut pool = ProxyPool::default();
    pool.id = id;
    pool.name = req.name;
    pool.proxy_url = req.proxy_url;
    pool.r#type = req.pool_type.unwrap_or_else(|| "http".to_string());
    pool.is_active = Some(true);
    pool.created_at = Some(now.clone());
    pool.updated_at = Some(now);
    
    let result = state
        .db
        .update(|db| {
            db.proxy_pools.push(pool);
        })
        .await;
    
    match result {
        Ok(_) => Json(json!({ "success": true, "message": "Proxy pool created" })),
        Err(e) => Json(json!({ "success": false, "error": e.to_string() })),
    }
}

// Settings API
async fn get_settings_api(State(state): State<AppState>) -> Json<Settings> {
    let snapshot = state.db.snapshot();
    Json(snapshot.settings.clone())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSettingsRequest {
    tunnel_provider: Option<String>,
    sticky_round_robin_limit: Option<u32>,
    combo_strategy: Option<String>,
    mitm_router_base_url: Option<String>,
    require_login: Option<bool>,
    observability_enabled: Option<bool>,
    cloud_enabled: Option<bool>,
    cloud_url: Option<String>,
    tunnel_enabled: Option<bool>,
    tunnel_url: Option<String>,
    outbound_proxy_enabled: Option<bool>,
    outbound_proxy_url: Option<String>,
}

async fn update_settings_api(
    State(state): State<AppState>,
    Json(req): Json<UpdateSettingsRequest>,
) -> Json<Value> {
    let result = state
        .db
        .update(|db| {
            if let Some(v) = req.tunnel_provider {
                db.settings.tunnel_provider = v;
            }
            if let Some(v) = req.sticky_round_robin_limit {
                db.settings.sticky_round_robin_limit = v;
            }
            if let Some(v) = req.combo_strategy {
                db.settings.combo_strategy = v;
            }
            if let Some(v) = req.mitm_router_base_url {
                db.settings.mitm_router_base_url = v;
            }
            if let Some(v) = req.require_login {
                db.settings.require_login = v;
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
            if let Some(v) = req.outbound_proxy_enabled {
                db.settings.outbound_proxy_enabled = v;
            }
            if let Some(v) = req.outbound_proxy_url {
                db.settings.outbound_proxy_url = v;
            }
        })
        .await;
    
    match result {
        Ok(_) => Json(json!({ "success": true, "message": "Settings updated" })),
        Err(e) => Json(json!({ "success": false, "error": e.to_string() })),
    }
}

// DB Export API
async fn export_db_api(State(state): State<AppState>) -> Json<Value> {
    let snapshot = state.db.snapshot();
    let val = serde_json::to_value(snapshot.as_ref()).unwrap_or(json!({}));
    Json(val)
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

async fn get_logs_api(State(_state): State<AppState>) -> Json<Vec<LogEntry>> {
    Json(vec![])
}

// Original model listing endpoint
async fn list_models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
    }

    let snapshot = state.db.snapshot();
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut seen = HashSet::new();
    let mut data = Vec::new();

    for combo in &snapshot.combos {
        push_model(
            &mut seen,
            &mut data,
            combo.name.clone(),
            "combo".into(),
            created,
        );
    }

    for connection in snapshot
        .provider_connections
        .iter()
        .filter(|connection| connection.is_active())
    {
        for model_id in models_for_connection(connection) {
            let (id, owned_by) = normalize_model_id(&connection.provider, &model_id);
            push_model(&mut seen, &mut data, id, owned_by, created);
        }
    }

    for model in snapshot
        .custom_models
        .iter()
        .filter(|model| model.r#type.is_empty() || model.r#type == "llm")
    {
        let (id, owned_by) = normalize_model_id(&model.provider_alias, &model.id);
        push_model(&mut seen, &mut data, id, owned_by, created);
    }

    Json(ModelListResponse {
        object: "list",
        data,
    })
    .into_response()
}

fn models_for_connection(connection: &ProviderConnection) -> Vec<String> {
    if let Some(enabled_models) = connection
        .provider_specific_data
        .get("enabledModels")
        .and_then(Value::as_array)
    {
        let models: Vec<_> = enabled_models
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect();

        if !models.is_empty() {
            return models;
        }
    }

    connection
        .default_model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| vec![value.to_string()])
        .unwrap_or_default()
}

fn normalize_model_id(provider: &str, model_id: &str) -> (String, String) {
    if model_id.contains('/') {
        let owned_by = model_id.split('/').next().unwrap_or(provider).to_string();
        (model_id.to_string(), owned_by)
    } else {
        (format!("{provider}/{model_id}"), provider.to_string())
    }
}

fn push_model(
    seen: &mut HashSet<String>,
    data: &mut Vec<ModelCard>,
    id: String,
    owned_by: String,
    created: u64,
) {
    if !seen.insert(id.clone()) {
        return;
    }

    let root = id.split('/').next_back().unwrap_or(&id).to_string();
    data.push(ModelCard {
        id,
        object: "model",
        created,
        owned_by,
        permission: Vec::new(),
        root,
        parent: None,
    });
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

#[derive(Debug, Serialize)]
struct ModelListResponse {
    object: &'static str,
    data: Vec<ModelCard>,
}

#[derive(Debug, Serialize)]
struct ModelCard {
    id: String,
    object: &'static str,
    created: u64,
    owned_by: String,
    permission: Vec<Value>,
    root: String,
    parent: Option<String>,
}
