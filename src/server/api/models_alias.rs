use axum::extract::State;
use axum::{
    http::HeaderMap,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use serde_json::json;

use crate::server::state::AppState;
use crate::types::{ModelAliasTarget, ProviderModelRef};

fn require_management_access(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    super::require_dashboard_or_management_api_key(headers, state)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/models/alias",
            get(list_aliases).put(set_alias).delete(delete_alias),
        )
        .route(
            "/api/models/alias/{alias}",
            get(get_alias).put(update_alias),
        )
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAliasRequest {
    pub alias: String,
    pub target: ModelAliasTarget,
}

#[derive(Debug, serde::Deserialize)]
pub struct SetAliasRequest {
    pub model: String,
    pub alias: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAliasRequest {
    pub target: ModelAliasTarget,
}

#[derive(Debug, Serialize)]
struct AliasesResponse {
    aliases: std::collections::BTreeMap<String, String>,
}

async fn list_aliases(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let aliases = snapshot
        .model_aliases
        .iter()
        .map(|(alias, target)| (alias.clone(), model_alias_path(target)))
        .collect();

    Json(AliasesResponse { aliases }).into_response()
}

async fn get_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(alias): axum::extract::Path<String>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    Json(snapshot.model_aliases.get(&alias).cloned()).into_response()
}

async fn set_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SetAliasRequest>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    if req.model.is_empty() || req.alias.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Model and alias required" })),
        )
            .into_response();
    }

    let result = state
        .db
        .update(|db| {
            db.model_aliases
                .insert(req.alias.clone(), ModelAliasTarget::Path(req.model.clone()));
        })
        .await;

    match result {
        Ok(_) => Json(json!({
            "success": true,
            "model": req.model,
            "alias": req.alias,
        }))
        .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "Failed to update alias" })),
        )
            .into_response(),
    }
}

async fn update_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(alias): axum::extract::Path<String>,
    Json(req): Json<UpdateAliasRequest>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let result = state
        .db
        .update(|db| {
            if let Some(existing) = db.model_aliases.get_mut(&alias) {
                *existing = req.target;
            }
        })
        .await;

    match result {
        Ok(_) => Json(serde_json::json!({ "success": true, "alias": alias })).into_response(),
        Err(e) => {
            Json(serde_json::json!({ "success": false, "error": e.to_string() })).into_response()
        }
    }
}

async fn delete_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<DeleteAliasQuery>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let Some(alias) = params.alias else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Alias required" })),
        )
            .into_response();
    };

    if alias.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Alias required" })),
        )
            .into_response();
    }

    let result = state
        .db
        .update(|db| {
            db.model_aliases.remove(&alias);
        })
        .await;

    match result {
        Ok(_) => Json(serde_json::json!({ "success": true })).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "Failed to delete alias" })),
        )
            .into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct DeleteAliasQuery {
    pub alias: Option<String>,
}

fn model_alias_path(target: &ModelAliasTarget) -> String {
    match target {
        ModelAliasTarget::Path(path) => path.clone(),
        ModelAliasTarget::Mapping(ProviderModelRef {
            provider, model, ..
        }) => format!("{provider}/{model}"),
    }
}
