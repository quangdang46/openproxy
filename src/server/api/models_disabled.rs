use std::collections::{BTreeMap, BTreeSet};

use axum::extract::{Query, State};
use axum::{
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::state::AppState;
use crate::types::AppDb;

fn require_management_access(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    super::require_dashboard_or_management_api_key(headers, state)
}

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/models/disabled",
        get(list_disabled_models)
            .post(disable_models_handler)
            .delete(enable_models_handler),
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DisabledModelsQuery {
    provider_alias: Option<String>,
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DisableModelsRequest {
    provider_alias: String,
    ids: Vec<String>,
}

pub(crate) fn disabled_models_from_db(db: &AppDb) -> BTreeMap<String, Vec<String>> {
    let Some(value) = db.extra.get("disabledModels") else {
        return BTreeMap::new();
    };

    serde_json::from_value::<BTreeMap<String, Vec<String>>>(value.clone()).unwrap_or_default()
}

pub(crate) fn is_model_disabled(db: &AppDb, provider_keys: &[&str], model_id: &str) -> bool {
    let model_id = model_id.trim();
    if model_id.is_empty() {
        return false;
    }

    let disabled = disabled_models_from_db(db);
    provider_keys.iter().any(|key| {
        disabled
            .get(*key)
            .is_some_and(|ids| ids.iter().any(|id| id == model_id))
    })
}

fn set_disabled_models(db: &mut AppDb, models: &BTreeMap<String, Vec<String>>) {
    if models.is_empty() {
        db.extra.remove("disabledModels");
        return;
    }

    db.extra.insert(
        "disabledModels".to_string(),
        serde_json::to_value(models).unwrap_or(Value::Object(Default::default())),
    );
}

fn normalize_disabled_models(
    models: &BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, Vec<String>> {
    let mut normalized = BTreeMap::new();
    for (provider_alias, ids) in models {
        let provider_alias = provider_alias.trim();
        if provider_alias.is_empty() {
            continue;
        }

        let unique = ids
            .iter()
            .map(String::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        if unique.is_empty() {
            continue;
        }

        normalized.insert(provider_alias.to_string(), unique.into_iter().collect());
    }
    normalized
}

async fn list_disabled_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<DisabledModelsQuery>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let disabled = disabled_models_from_db(&snapshot);
    if let Some(provider_alias) = query.provider_alias.as_deref().map(str::trim) {
        return Json(json!({
            "ids": disabled.get(provider_alias).cloned().unwrap_or_default()
        }))
        .into_response();
    }

    Json(json!({ "disabled": disabled })).into_response()
}

async fn disable_models_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<DisableModelsRequest>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let provider_alias = req.provider_alias.trim();
    if provider_alias.is_empty() || req.ids.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({ "error": "providerAlias and ids[] required" })),
        )
            .into_response();
    }

    let result = state
        .db
        .update(move |db| {
            let mut disabled = disabled_models_from_db(db);
            let entry = disabled.entry(provider_alias.to_string()).or_default();
            let mut existing = entry.iter().cloned().collect::<BTreeSet<_>>();
            for id in &req.ids {
                let id = id.trim();
                if !id.is_empty() {
                    existing.insert(id.to_string());
                }
            }
            *entry = existing.into_iter().collect();
            let normalized = normalize_disabled_models(&disabled);
            set_disabled_models(db, &normalized);
        })
        .await;

    match result {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(error) => Json(json!({ "success": false, "error": error.to_string() })).into_response(),
    }
}

async fn enable_models_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<DisabledModelsQuery>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let provider_alias = query.provider_alias.as_deref().map(str::trim).unwrap_or("");
    if provider_alias.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({ "error": "providerAlias required" })),
        )
            .into_response();
    }

    let id = query
        .id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let provider_alias = provider_alias.to_string();

    let result = state
        .db
        .update(move |db| {
            let mut disabled = disabled_models_from_db(db);
            if let Some(model_id) = id {
                if let Some(existing) = disabled.get_mut(&provider_alias) {
                    existing.retain(|candidate| candidate != &model_id);
                    if existing.is_empty() {
                        disabled.remove(&provider_alias);
                    }
                }
            } else {
                disabled.remove(&provider_alias);
            }
            let normalized = normalize_disabled_models(&disabled);
            set_disabled_models(db, &normalized);
        })
        .await;

    match result {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(error) => Json(json!({ "success": false, "error": error.to_string() })).into_response(),
    }
}
