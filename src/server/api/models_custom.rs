use std::collections::BTreeMap;

use axum::extract::State;
use axum::{
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};

use crate::server::state::AppState;
use crate::types::CustomModel;

fn require_management_access(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    super::require_management_api_key(headers, state)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/models/custom",
            get(list_custom_models).post(create_custom_model),
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
    #[serde(default = "default_model_type")]
    pub model_type: String,
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

async fn list_custom_models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    Json(snapshot.custom_models.clone()).into_response()
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

    let custom_model = CustomModel {
        provider_alias: req.provider_alias,
        id: req.id,
        r#type: req.model_type,
        name: req.name,
        extra: BTreeMap::new(),
    };

    let result = state
        .db
        .update(|db| {
            db.custom_models.push(custom_model);
        })
        .await;

    match result {
        Ok(_) => Json(serde_json::json!({ "success": true })).into_response(),
        Err(e) => {
            Json(serde_json::json!({ "success": false, "error": e.to_string() })).into_response()
        }
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
