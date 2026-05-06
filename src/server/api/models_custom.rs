use axum::extract::State;
use axum::{
    http::HeaderMap,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;

use crate::server::state::AppState;
use crate::types::CustomModel;

fn require_management_access(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    super::require_dashboard_or_management_api_key(headers, state)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/models/custom",
            get(list_custom_models)
                .post(create_custom_model)
                .delete(delete_custom_model_by_query),
        )
        .route(
            "/api/models/custom/{id}",
            get(get_custom_model)
                .put(update_custom_model)
                .delete(delete_custom_model),
        )
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCustomModelRequest {
    pub provider_alias: String,
    pub id: String,
    #[serde(default = "default_model_type", alias = "type")]
    pub r#type: String,
    pub name: Option<String>,
}

fn default_model_type() -> String {
    "llm".to_string()
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCustomModelRequest {
    pub provider_alias: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteCustomModelQuery {
    pub provider_alias: Option<String>,
    pub id: Option<String>,
    #[serde(default = "default_model_type")]
    pub r#type: String,
}

async fn list_custom_models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    Json(json!({ "models": snapshot.custom_models.clone() })).into_response()
}

async fn get_custom_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let model = snapshot.custom_models.iter().find(|m| m.id == id).cloned();
    Json(model).into_response()
}

async fn create_custom_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateCustomModelRequest>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    if req.provider_alias.is_empty() || req.id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "providerAlias and id required" })),
        )
            .into_response();
    }

    let exists_before = state.db.snapshot().custom_models.iter().any(|model| {
        model.provider_alias == req.provider_alias
            && model.id == req.id
            && model.r#type == req.r#type
    });

    let custom_model = CustomModel {
        provider_alias: req.provider_alias.clone(),
        id: req.id.clone(),
        r#type: req.r#type.clone(),
        name: req.name.clone(),
        extra: Default::default(),
    };

    let result = state
        .db
        .update(move |db| {
            let exists = db.custom_models.iter().any(|model| {
                model.provider_alias == req.provider_alias
                    && model.id == req.id
                    && model.r#type == req.r#type
            });
            if !exists {
                db.custom_models.push(custom_model);
            }
        })
        .await;

    match result {
        Ok(_) => Json(json!({ "success": true, "added": !exists_before })).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "Failed to add custom model" })),
        )
            .into_response(),
    }
}

async fn update_custom_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<UpdateCustomModelRequest>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let result = state
        .db
        .update(|db| {
            if let Some(model) = db.custom_models.iter_mut().find(|m| m.id == id) {
                if let Some(provider_alias) = req.provider_alias {
                    model.provider_alias = provider_alias;
                }
                if let Some(name) = req.name {
                    model.name = Some(name);
                }
            }
        })
        .await;

    match result {
        Ok(_) => Json(serde_json::json!({ "success": true })).into_response(),
        Err(e) => {
            Json(serde_json::json!({ "success": false, "error": e.to_string() })).into_response()
        }
    }
}

async fn delete_custom_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let result = state
        .db
        .update(|db| {
            db.custom_models.retain(|m| m.id != id);
        })
        .await;

    match result {
        Ok(_) => Json(serde_json::json!({ "success": true })).into_response(),
        Err(e) => {
            Json(serde_json::json!({ "success": false, "error": e.to_string() })).into_response()
        }
    }
}

async fn delete_custom_model_by_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<DeleteCustomModelQuery>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let provider_alias = query.provider_alias.unwrap_or_default();
    let id = query.id.unwrap_or_default();
    if provider_alias.is_empty() || id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "providerAlias and id required" })),
        )
            .into_response();
    }

    let model_type = query.r#type;
    let result = state
        .db
        .update(move |db| {
            db.custom_models.retain(|model| {
                !(model.provider_alias == provider_alias
                    && model.id == id
                    && model.r#type == model_type)
            });
        })
        .await;

    match result {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "Failed to delete custom model" })),
        )
            .into_response(),
    }
}
