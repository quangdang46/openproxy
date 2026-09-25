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
    let alias_to_provider = catalog.alias_to_provider_id();

    for entry in catalog.iter_provider_models() {
        let provider_alias = &entry.alias;
        let disabled_ids: Vec<&str> = disabled_map
            .get(provider_alias)
            .map(|v| v.iter().map(String::as_str).collect())
            .unwrap_or_default();
        let provider_info = alias_to_provider
            .get(provider_alias)
            .and_then(|pid| catalog.provider_info(pid));

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

            // Derive lightweight caps for dashboard CapacityBadges.
            // Prefer explicit catalog capabilities; fall back to name heuristics.
            let caps = {
                let (mut vision, mut reasoning) =
                    heuristic_caps(&model.id, model.name.as_deref().unwrap_or_default());
                if let Some(list) = model.capabilities.as_ref() {
                    for c in list {
                        let lower = c.to_ascii_lowercase();
                        if lower.contains("vision") || lower.contains("image") {
                            vision = true;
                        }
                        if lower.contains("reason") || lower.contains("think") {
                            reasoning = true;
                        }
                    }
                }
                if let Some(pi) = provider_info {
                    if pi.vision == Some(true) {
                        vision = true;
                    }
                    if pi.reasoning == Some(true) {
                        reasoning = true;
                    }
                }
                serde_json::json!({ "vision": vision, "reasoning": reasoning })
            };

            models.push(serde_json::json!({
                "provider": provider_alias,
                "model": model.id,
                "name": model.name,
                "kind": model.kind,
                "fullModel": full_model,
                "alias": alias,
                "caps": caps,
            }));
        }
    }

    // Custom models ride along; a stored `caps` overrides the name heuristic
    // (9router `api/models/route.js:55-61`). Catalog rows already claimed their
    // own `<alias>/<id>`, so a custom model that duplicates one is skipped.
    let seen: std::collections::HashSet<String> = models
        .iter()
        .filter_map(|model| model["fullModel"].as_str().map(str::to_string))
        .collect();
    for custom in &snapshot.custom_models {
        if !(custom.r#type.is_empty() || custom.r#type == "llm" || custom.r#type == "chat") {
            continue;
        }
        let model_id = custom.id.trim();
        let provider_alias = custom.provider_alias.trim();
        if model_id.is_empty() || provider_alias.is_empty() {
            continue;
        }
        let full_model = format!("{provider_alias}/{model_id}");
        if seen.contains(&full_model) {
            continue;
        }
        let alias = snapshot
            .model_aliases
            .get(&full_model)
            .map(model_alias_path)
            .unwrap_or_else(|| model_id.to_string());
        let (vision, reasoning) = heuristic_caps(model_id, "");
        let stored_caps = custom
            .extra
            .get("caps")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut caps = serde_json::Map::new();
        caps.insert("vision".to_string(), json!(vision));
        caps.insert("reasoning".to_string(), json!(reasoning));
        caps.extend(stored_caps);

        models.push(serde_json::json!({
            "provider": provider_alias,
            "model": model_id,
            "name": custom.name,
            "kind": custom.r#type,
            "fullModel": full_model,
            "alias": alias,
            "caps": caps,
        }));
    }

    Json(serde_json::json!({ "models": models })).into_response()
}

/// Capability flags read off a model id or display name — 9router's
/// `getCapabilitiesForModel` floor, minus the catalog lookups a custom model
/// cannot satisfy. Catalog rows also feed this from their display name; a
/// custom row is derived from its id alone, as upstream does.
fn heuristic_caps(id: &str, name: &str) -> (bool, bool) {
    let id = id.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    let vision = id.contains("vision") || id.contains("vl") || name.contains("vision");
    let reasoning = id.contains("reason")
        || id.contains("thinking")
        || id.contains("o1")
        || id.contains("o3")
        || id.contains("o4")
        || name.contains("reason");
    (vision, reasoning)
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
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
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
