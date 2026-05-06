//! Cloud auth, credential refresh, and alias APIs that must match 9router.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::server::auth::AUTHORIZATION_HEADER;
use crate::server::state::AppState;
use crate::types::ModelAliasTarget;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/cloud/auth", post(cloud_auth))
        .route(
            "/api/cloud/credentials/update",
            put(update_cloud_credentials),
        )
        .route("/api/cloud/model/resolve", post(resolve_cloud_model))
        .route(
            "/api/cloud/models/alias",
            get(get_cloud_model_aliases).put(set_cloud_model_alias),
        )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CloudConnection {
    provider: String,
    auth_type: String,
    api_key: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
    project_id: Option<String>,
    expires_at: Option<String>,
    priority: Option<u32>,
    global_priority: Option<u32>,
    default_model: Option<String>,
    is_active: bool,
}

fn error_response(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn require_cloud_bearer_api_key(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    let Some(auth_header) = headers
        .get(AUTHORIZATION_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(error_response(StatusCode::UNAUTHORIZED, "Missing API key"));
    };

    let Some(api_key) = auth_header.strip_prefix("Bearer ") else {
        return Err(error_response(StatusCode::UNAUTHORIZED, "Missing API key"));
    };

    let snapshot = state.db.snapshot();
    let is_valid = snapshot
        .api_keys
        .iter()
        .any(|candidate| candidate.key == api_key && candidate.is_active());

    if !is_valid {
        return Err(error_response(StatusCode::UNAUTHORIZED, "Invalid API key"));
    }

    Ok(())
}

fn model_alias_value(target: &ModelAliasTarget) -> String {
    match target {
        ModelAliasTarget::Path(path) => path.clone(),
        ModelAliasTarget::Mapping(mapping) => {
            format!("{}/{}", mapping.provider, mapping.model)
        }
    }
}

fn split_provider_model(path: &str) -> Option<(String, String)> {
    let first_slash = path.find('/')?;
    if first_slash == 0 {
        return None;
    }

    Some((
        path[..first_slash].to_string(),
        path[first_slash + 1..].to_string(),
    ))
}

async fn cloud_auth(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_cloud_bearer_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let connections: Vec<_> = snapshot
        .provider_connections
        .iter()
        .filter(|conn| conn.is_active())
        .map(|conn| CloudConnection {
            provider: conn.provider.clone(),
            auth_type: conn.auth_type.clone(),
            api_key: conn.api_key.clone(),
            access_token: conn.access_token.clone(),
            refresh_token: conn.refresh_token.clone(),
            project_id: conn.project_id.clone(),
            expires_at: conn.expires_at.clone(),
            priority: conn.priority,
            global_priority: conn.global_priority,
            default_model: conn.default_model.clone(),
            is_active: conn.is_active(),
        })
        .collect();

    let model_aliases = snapshot
        .model_aliases
        .iter()
        .map(|(alias, target)| (alias.clone(), model_alias_value(target)))
        .collect::<std::collections::BTreeMap<_, _>>();

    Json(json!({
        "connections": connections,
        "modelAliases": model_aliases,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateCredentialsRequest {
    provider: Option<String>,
    credentials: Option<CloudCredentials>,
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
    if let Err(response) = require_cloud_bearer_api_key(&headers, &state) {
        return response;
    }

    let provider = req.provider.filter(|value| !value.is_empty());
    let credentials = req.credentials;
    let (Some(provider), Some(credentials)) = (provider, credentials) else {
        return error_response(StatusCode::BAD_REQUEST, "Provider and credentials required");
    };

    let snapshot = state.db.snapshot();
    let connection_id = snapshot
        .provider_connections
        .iter()
        .find(|conn| conn.provider == provider && conn.is_active())
        .map(|conn| conn.id.clone());

    let Some(connection_id) = connection_id else {
        return error_response(
            StatusCode::NOT_FOUND,
            &format!("No active connection found for provider: {provider}"),
        );
    };

    let access_token = credentials.access_token.filter(|value| !value.is_empty());
    let refresh_token = credentials.refresh_token.filter(|value| !value.is_empty());
    let expires_at = credentials
        .expires_in
        .filter(|seconds| *seconds != 0)
        .and_then(|seconds| {
            chrono::Utc::now()
                .checked_add_signed(chrono::Duration::seconds(seconds))
                .map(|dt| dt.to_rfc3339())
        });

    let result = state
        .db
        .update(|db| {
            if let Some(conn) = db
                .provider_connections
                .iter_mut()
                .find(|conn| conn.id == connection_id)
            {
                if let Some(token) = access_token.clone() {
                    conn.access_token = Some(token);
                }
                if let Some(token) = refresh_token.clone() {
                    conn.refresh_token = Some(token);
                }
                if let Some(expiry) = expires_at.clone() {
                    conn.expires_at = Some(expiry);
                }
            }
        })
        .await;

    match result {
        Ok(_) => Json(json!({
            "success": true,
            "message": format!("Credentials updated for provider: {provider}")
        }))
        .into_response(),
        Err(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update credentials",
        ),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveModelRequest {
    alias: Option<String>,
}

async fn resolve_cloud_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ResolveModelRequest>,
) -> Response {
    if let Err(response) = require_cloud_bearer_api_key(&headers, &state) {
        return response;
    }

    let Some(alias) = req.alias.filter(|value| !value.is_empty()) else {
        return error_response(StatusCode::BAD_REQUEST, "Missing alias");
    };

    let snapshot = state.db.snapshot();
    let Some(target) = snapshot.model_aliases.get(&alias) else {
        return error_response(StatusCode::NOT_FOUND, "Alias not found");
    };

    let Some((provider, model)) = split_provider_model(&model_alias_value(target)) else {
        return error_response(StatusCode::NOT_FOUND, "Alias not found");
    };

    Json(json!({
        "alias": alias,
        "provider": provider,
        "model": model,
    }))
    .into_response()
}

async fn get_cloud_model_aliases(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_cloud_bearer_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let aliases = snapshot
        .model_aliases
        .iter()
        .map(|(alias, target)| (alias.clone(), model_alias_value(target)))
        .collect::<std::collections::BTreeMap<_, _>>();

    Json(json!({
        "aliases": aliases,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetAliasRequest {
    model: Option<String>,
    alias: Option<String>,
}

async fn set_cloud_model_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SetAliasRequest>,
) -> Response {
    if let Err(response) = require_cloud_bearer_api_key(&headers, &state) {
        return response;
    }

    let model = req.model.filter(|value| !value.is_empty());
    let alias = req.alias.filter(|value| !value.is_empty());
    let (Some(model), Some(alias)) = (model, alias) else {
        return error_response(StatusCode::BAD_REQUEST, "Model and alias required");
    };

    let snapshot = state.db.snapshot();
    if let Some(existing) = snapshot.model_aliases.get(&alias) {
        let existing_model = model_alias_value(existing);
        if existing_model != model {
            return error_response(
                StatusCode::BAD_REQUEST,
                &format!("Alias '{alias}' already in use for model '{existing_model}'"),
            );
        }
    }

    let result = state
        .db
        .update(|db| {
            db.model_aliases
                .insert(alias.clone(), ModelAliasTarget::Path(model.clone()));
        })
        .await;

    match result {
        Ok(_) => Json(json!({
            "success": true,
            "model": model,
            "alias": alias,
            "message": format!("Alias '{alias}' set for model '{model}'")
        }))
        .into_response(),
        Err(_) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to update alias"),
    }
}
