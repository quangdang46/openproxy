//! Cloud credentials and model resolution API endpoints.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::server::auth::require_api_key;
use crate::server::state::AppState;
use crate::types::ModelAliasTarget;

use super::auth_error_response;

pub fn routes() -> Router<AppState> {
    Router::new()
        // Cloud credentials management
        .route("/api/cloud/credentials", get(get_cloud_credentials))
        .route(
            "/api/cloud/credentials/update",
            put(update_cloud_credentials),
        )
        // Model resolution for cloud
        .route("/api/cloud/model/resolve", post(resolve_cloud_model))
        // Cloud model aliases
        .route(
            "/api/cloud/models/alias",
            get(get_cloud_model_aliases).put(set_cloud_model_alias),
        )
}

async fn get_cloud_credentials(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
    }

    let snapshot = state.db.snapshot();

    // Map active provider connections for cloud sync
    let connections: Vec<_> = snapshot
        .provider_connections
        .iter()
        .filter(|conn| conn.is_active())
        .map(|conn| CloudProviderConnection {
            provider: conn.provider.clone(),
            auth_type: conn.auth_type.clone(),
            api_key: conn.api_key.clone(),
            access_token: conn.access_token.clone(),
            refresh_token: conn.refresh_token.clone(),
            expires_at: conn.expires_at.clone(),
            project_id: conn.project_id.clone(),
            priority: conn.priority,
            global_priority: conn.global_priority,
            default_model: conn.default_model.clone(),
            is_active: conn.is_active(),
        })
        .collect();

    // Get model aliases as a map of alias -> target string
    let model_aliases: std::collections::BTreeMap<String, serde_json::Value> = snapshot
        .model_aliases
        .iter()
        .map(|(alias, target)| {
            let value = match target {
                ModelAliasTarget::Path(path) => json!({ "path": path }),
                ModelAliasTarget::Mapping(m) => json!({ "provider": m.provider, "model": m.model }),
            };
            (alias.clone(), value)
        })
        .collect();

    Json(json!({
        "connections": connections,
        "modelAliases": model_aliases
    }))
    .into_response()
}

#[derive(Serialize)]
struct CloudProviderConnection {
    provider: String,
    auth_type: String,
    api_key: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_at: Option<String>,
    project_id: Option<String>,
    priority: Option<u32>,
    global_priority: Option<u32>,
    default_model: Option<String>,
    is_active: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateCredentialsRequest {
    provider: String,
    credentials: CloudCredentials,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloudCredentials {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

async fn update_cloud_credentials(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpdateCredentialsRequest>,
) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
    }

    let provider = req.provider;
    let credentials = req.credentials;

    // Find active connection for provider
    let snapshot = state.db.snapshot();
    let connection_id = snapshot
        .provider_connections
        .iter()
        .find(|conn| conn.provider == provider && conn.is_active())
        .map(|conn| conn.id.clone());

    let connection_id = match connection_id {
        Some(id) => id,
        None => {
            return Json(json!({
                "success": false,
                "error": format!("No active connection found for provider: {}", provider)
            }))
            .into_response();
        }
    };

    // Build update data
    let expires_at = credentials
        .expires_in
        .map(|seconds| {
            chrono::Utc::now()
                .checked_add_signed(chrono::Duration::seconds(seconds))
                .map(|dt| dt.to_rfc3339())
        })
        .flatten();

    let result = state
        .db
        .update(|db| {
            if let Some(conn) = db
                .provider_connections
                .iter_mut()
                .find(|c| c.id == connection_id)
            {
                if let Some(token) = credentials.access_token {
                    conn.access_token = Some(token);
                }
                if let Some(token) = credentials.refresh_token {
                    conn.refresh_token = Some(token);
                }
                if let Some(exp) = expires_at {
                    conn.expires_at = Some(exp);
                }
            }
        })
        .await;

    match result {
        Ok(_) => Json(json!({
            "success": true,
            "message": format!("Credentials updated for provider: {}", provider)
        }))
        .into_response(),
        Err(e) => Json(json!({
            "success": false,
            "error": e.to_string()
        }))
        .into_response(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveModelRequest {
    alias: String,
}

async fn resolve_cloud_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ResolveModelRequest>,
) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
    }

    let snapshot = state.db.snapshot();
    let resolved = snapshot.model_aliases.get(&req.alias);

    match resolved {
        Some(target) => {
            let (provider, model) = match target {
                ModelAliasTarget::Path(path) => {
                    let parts: Vec<&str> = path.split('/').collect();
                    if parts.len() >= 2 {
                        (parts[0].to_string(), parts[1..].join("/"))
                    } else {
                        (path.clone(), path.clone())
                    }
                }
                ModelAliasTarget::Mapping(m) => (m.provider.clone(), m.model.clone()),
            };
            Json(json!({
                "alias": req.alias,
                "provider": provider,
                "model": model
            }))
            .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "Alias not found"
            })),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetAliasRequest {
    model: String,
    alias: String,
}

async fn get_cloud_model_aliases(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
    }

    let snapshot = state.db.snapshot();

    // Convert model aliases to a serializable format
    let aliases: std::collections::BTreeMap<String, serde_json::Value> = snapshot
        .model_aliases
        .iter()
        .map(|(alias, target)| {
            let value = match target {
                ModelAliasTarget::Path(path) => json!({ "path": path }),
                ModelAliasTarget::Mapping(m) => json!({ "provider": m.provider, "model": m.model }),
            };
            (alias.clone(), value)
        })
        .collect();

    Json(json!({
        "aliases": aliases
    }))
    .into_response()
}

async fn set_cloud_model_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SetAliasRequest>,
) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
    }

    let snapshot = state.db.snapshot();

    // Check if alias already exists for different model
    if let Some(existing) = snapshot.model_aliases.get(&req.alias) {
        let existing_model = match existing {
            ModelAliasTarget::Path(path) => path.clone(),
            ModelAliasTarget::Mapping(m) => format!("{}/{}", m.provider, m.model),
        };
        if existing_model != req.model {
            return Json(json!({
                "success": false,
                "error": format!("Alias '{}' already in use for model '{}'", req.alias, existing_model)
            })).into_response();
        }
    }

    let result = state
        .db
        .update(|db| {
            let parts: Vec<&str> = req.model.split('/').collect();
            let (provider, model_str) = if parts.len() >= 2 {
                (parts[0].to_string(), parts[1..].join("/"))
            } else {
                ("unknown".to_string(), req.model.clone())
            };
            db.model_aliases.insert(
                req.alias.clone(),
                ModelAliasTarget::Mapping(crate::types::ProviderModelRef {
                    provider,
                    model: model_str,
                    extra: Default::default(),
                }),
            );
        })
        .await;

    match result {
        Ok(_) => Json(json!({
            "success": true,
            "model": req.model,
            "alias": req.alias,
            "message": format!("Alias '{}' set for model '{}'", req.alias, req.model)
        }))
        .into_response(),
        Err(e) => Json(json!({
            "success": false,
            "error": e.to_string()
        }))
        .into_response(),
    }
}
