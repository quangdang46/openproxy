use axum::extract::State;
use axum::{
    routing::{delete, get, post},
    Json, Router,
};
use std::collections::BTreeMap;

use crate::server::state::AppState;
use crate::types::ModelAliasTarget;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/models/alias", get(list_aliases).post(create_alias).delete(delete_alias))
        .route("/api/models/alias/{alias}", get(get_alias).put(update_alias))
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

async fn list_aliases(State(state): State<AppState>) -> Json<BTreeMap<String, ModelAliasTarget>> {
    let snapshot = state.db.snapshot();
    Json(snapshot.model_aliases.clone())
}

async fn get_alias(
    State(state): State<AppState>,
    axum::extract::Path(alias): axum::extract::Path<String>,
) -> Json<Option<ModelAliasTarget>> {
    let snapshot = state.db.snapshot();
    Json(snapshot.model_aliases.get(&alias).cloned())
}

async fn create_alias(
    State(state): State<AppState>,
    Json(req): Json<CreateAliasRequest>,
) -> Json<serde_json::Value> {
    let result = state
        .db
        .update(|db| {
            db.model_aliases.insert(req.alias.clone(), req.target);
        })
        .await;

    match result {
        Ok(_) => Json(serde_json::json!({ "success": true, "alias": req.alias })),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e.to_string() })),
    }
}

async fn update_alias(
    State(state): State<AppState>,
    axum::extract::Path(alias): axum::extract::Path<String>,
    Json(req): Json<UpdateAliasRequest>,
) -> Json<serde_json::Value> {
    let result = state
        .db
        .update(|db| {
            if let Some(existing) = db.model_aliases.get_mut(&alias) {
                *existing = req.target;
            }
        })
        .await;

    match result {
        Ok(_) => Json(serde_json::json!({ "success": true, "alias": alias })),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e.to_string() })),
    }
}

async fn delete_alias(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<DeleteAliasQuery>,
) -> Json<serde_json::Value> {
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
        Ok(_) => Json(serde_json::json!({ "success": true })),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e.to_string() })),
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct DeleteAliasQuery {
    pub alias: Option<String>,
}