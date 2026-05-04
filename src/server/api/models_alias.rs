use axum::extract::State;
use axum::{
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};

use crate::server::state::AppState;
use crate::types::ModelAliasTarget;

fn require_management_access(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    super::require_dashboard_or_management_api_key(headers, state)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/models/alias",
            get(list_aliases).post(create_alias).delete(delete_alias),
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
#[serde(rename_all = "camelCase")]
pub struct UpdateAliasRequest {
    pub target: ModelAliasTarget,
}

async fn list_aliases(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    Json(snapshot.model_aliases.clone()).into_response()
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

async fn create_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateAliasRequest>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let result = state
        .db
        .update(|db| {
            db.model_aliases.insert(req.alias.clone(), req.target);
        })
        .await;

    match result {
        Ok(_) => Json(serde_json::json!({ "success": true, "alias": req.alias })).into_response(),
        Err(e) => {
            Json(serde_json::json!({ "success": false, "error": e.to_string() })).into_response()
        }
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

    let result = state
        .db
        .update(|db| {
            if let Some(alias) = &params.alias {
                db.model_aliases.remove(alias);
            } else {
                db.model_aliases.clear();
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

#[derive(Debug, serde::Deserialize)]
pub struct DeleteAliasQuery {
    pub alias: Option<String>,
}
