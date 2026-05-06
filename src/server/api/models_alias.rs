use axum::extract::State;
use axum::{
    http::HeaderMap,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, put},
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
        .route("/api/models", get(list_models).put(update_model_alias))
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
pub struct UpdateModelAliasRequest {
    pub model: String,
    pub alias: String,
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

// GET /api/models — list AI_MODELS with aliases and disabled-model filtering
async fn list_models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let disabled_map = super::models_disabled::disabled_models_from_db(&snapshot);
    let catalog = crate::core::model::catalog::provider_catalog();

    let mut models = Vec::new();

    for entry in catalog.iter_provider_models() {
        let provider_alias = &entry.alias;
        let disabled_ids: Vec<&str> = disabled_map
            .get(provider_alias)
            .map(|v| v.iter().map(String::as_str).collect())
            .unwrap_or_default();

        for model in &entry.models {
            if disabled_ids.contains(&model.id.as_str()) {
                continue;
            }

            let full_model = format!("{}/{}", provider_alias, model.id);
            let alias = snapshot
                .model_aliases
                .get(&full_model)
                .map(model_alias_path)
                .unwrap_or_else(|| model.id.clone());

            models.push(serde_json::json!({
                "provider": provider_alias,
                "model": model.id,
                "name": model.name,
                "kind": model.kind,
                "fullModel": full_model,
                "alias": alias,
            }));
        }
    }

    Json(serde_json::json!({ "models": models })).into_response()
}

// PUT /api/models — update model alias (with duplicate check)
async fn update_model_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpdateModelAliasRequest>,
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

    let snapshot = state.db.snapshot();

    // Check if alias already exists for a different model
    for (existing_alias, target) in &snapshot.model_aliases {
        if existing_alias == &req.alias {
            if model_alias_path(target) == req.model {
                return Json(json!({
                    "success": true,
                    "model": req.model,
                    "alias": req.alias,
                }))
                .into_response();
            }
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Alias already in use" })),
            )
                .into_response();
        }
    }

    // Also check if model already has this alias (idempotent)
    let existing_target = snapshot.model_aliases.get(&req.model);
    if let Some(existing) = existing_target {
        if model_alias_path(existing) == req.alias {
            return Json(json!({
                "success": true,
                "model": req.model,
                "alias": req.alias,
            }))
            .into_response();
        }
    }

    let result = state
        .db
        .update(|db| {
            db.model_aliases
                .insert(req.model.clone(), ModelAliasTarget::Path(req.alias.clone()));
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
