use std::collections::BTreeMap;

use axum::extract::{Query, State};
use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::state::AppState;
use crate::types::AppDb;

fn require_management_access(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    super::require_dashboard_or_management_api_key(headers, state)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilterQuery {
    alias: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilterUpsertRequest {
    alias: String,
    free_only: bool,
}

/// Read the provider-filters map out of `AppDb.extra["providerFilters"]`.
pub(crate) fn filters_from_db(db: &AppDb) -> BTreeMap<String, Value> {
    let Some(value) = db.extra.get("providerFilters") else {
        return BTreeMap::new();
    };

    serde_json::from_value::<BTreeMap<String, Value>>(value.clone()).unwrap_or_default()
}

/// Write the provider-filters map into `AppDb.extra["providerFilters"]`.
pub(crate) fn set_filters(db: &mut AppDb, filters: &BTreeMap<String, Value>) {
    if filters.is_empty() {
        db.extra.remove("providerFilters");
        return;
    }

    db.extra.insert(
        "providerFilters".to_string(),
        serde_json::to_value(filters).unwrap_or(Value::Object(Default::default())),
    );
}

async fn list_provider_filters(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FilterQuery>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let filters = filters_from_db(&snapshot);
    if let Some(alias) = query.alias.as_deref().map(str::trim) {
        if alias.is_empty() {
            return Json(json!({ "filters": {} })).into_response();
        }
        let alias_filtered = filters
            .get(alias)
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        return Json(json!({ "filters": { alias: alias_filtered } })).into_response();
    }

    Json(json!({ "filters": filters })).into_response()
}

async fn upsert_provider_filter(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<FilterUpsertRequest>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let alias = req.alias.trim().to_string();
    if alias.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({ "error": "alias required" })),
        )
            .into_response();
    }

    let result = state
        .db
        .update(move |db| {
            let mut filters = filters_from_db(db);
            filters.insert(alias.clone(), json!({ "freeOnly": req.free_only }));
            set_filters(db, &filters);
        })
        .await;

    match result {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => Json(json!({ "success": false, "error": error.to_string() })).into_response(),
    }
}

pub(crate) fn favorites_from_db(db: &AppDb) -> BTreeMap<String, Value> {
    let Some(value) = db.extra.get("favoriteModels") else {
        return BTreeMap::new();
    };
    serde_json::from_value::<BTreeMap<String, Value>>(value.clone()).unwrap_or_default()
}

pub(crate) fn set_favorites(db: &mut AppDb, favorites: &BTreeMap<String, Value>) {
    if favorites.is_empty() {
        db.extra.remove("favoriteModels");
        return;
    }
    db.extra.insert(
        "favoriteModels".to_string(),
        serde_json::to_value(favorites).unwrap_or(Value::Object(Default::default())),
    );
}

async fn list_favorites(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<FilterQuery>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }
    let snapshot = state.db.snapshot();
    let favorites = favorites_from_db(&snapshot);
    if let Some(alias) = query.alias.as_deref().map(str::trim) {
        if alias.is_empty() {
            return Json(json!({ "favorites": {} })).into_response();
        }
        let alias_favs = favorites
            .get(alias)
            .cloned()
            .unwrap_or(Value::Array(vec![]));
        return Json(json!({ "favorites": { alias: alias_favs } })).into_response();
    }
    Json(json!({ "favorites": favorites })).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FavoritesUpsertRequest {
    alias: String,
    model_ids: Vec<String>,
}

async fn upsert_favorites(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<FavoritesUpsertRequest>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }
    let alias = req.alias.trim().to_string();
    if alias.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({ "error": "alias required" })),
        )
            .into_response();
    }
    let result = state
        .db
        .update(move |db| {
            let mut favorites = favorites_from_db(db);
            favorites.insert(alias.clone(), json!(req.model_ids));
            set_favorites(db, &favorites);
        })
        .await;
    match result {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => Json(json!({ "success": false, "error": error.to_string() })).into_response(),
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/providers/filters", get(list_provider_filters))
        .route("/api/providers/filters", put(upsert_provider_filter))
        .route("/api/models/favorites", get(list_favorites))
        .route("/api/models/favorites", put(upsert_favorites))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::patch::apply_app_db_diff;
    use crate::db::sqlite::repo::kv_repo;
    use crate::types::AppDb;
    use serde_json::json;

    fn open() -> crate::db::sqlite::SqliteDb {
        crate::db::sqlite::SqliteDb::open_in_memory().unwrap()
    }

    #[test]
    fn filter_upsert_roundtrip() {
        let db = open();
        let mut old = AppDb::default();
        let mut new = AppDb::default();

        // Upsert a filter for alias "kc" with freeOnly=true.
        new.extra.insert(
            "providerFilters".into(),
            json!({ "kc": { "freeOnly": true } }),
        );
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new))
            .unwrap();

        // Read back from KV table.
        let all = db
            .with_conn(|c| kv_repo::get_all(c, "providerFilters"))
            .unwrap();
        assert_eq!(all.len(), 1);
        let kc_val = all.get("kc").unwrap();
        assert_eq!(kc_val["freeOnly"], true);

        // Reload from DB and assert value survives by re-reading extra map.
        let filters = filters_from_db(&new);
        assert_eq!(filters.get("kc").unwrap()["freeOnly"], true);

        // Remove the filter.
        old = new.clone();
        new.extra.remove("providerFilters");
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new))
            .unwrap();

        // Assert it's gone from KV table.
        let all = db
            .with_conn(|c| kv_repo::get_all(c, "providerFilters"))
            .unwrap();
        assert_eq!(all.len(), 0);
    }

    #[test]
    fn get_returns_empty_for_fresh_db() {
        let db = open();
        // Fresh DB has no "providerFilters" in extra map.
        let filters = filters_from_db(&AppDb::default());
        assert_eq!(filters, BTreeMap::new());
    }

    #[test]
    fn favorites_upsert_roundtrip() {
        let db = open();
        let mut old = AppDb::default();
        let mut new = AppDb::default();
        new.extra.insert(
            "favoriteModels".into(),
            json!({ "kc": ["claude-sonnet-4-5", "opencode-zen"] }),
        );
        db.with_transaction(|tx| apply_app_db_diff(tx, &old, &new))
            .unwrap();

        // Read back from KV table.
        let all = db
            .with_conn(|c| kv_repo::get_all(c, "favoriteModels"))
            .unwrap();
        assert_eq!(all.len(), 1);
        let kc_val = all.get("kc").unwrap().as_array().unwrap();
        assert_eq!(kc_val.len(), 2);

        // Survives a snapshot reload from DB.
        let favorites = favorites_from_db(&new);
        let kc = favorites.get("kc").and_then(|v| v.as_array()).unwrap();
        assert_eq!(kc.len(), 2);
        assert_eq!(kc[0], "claude-sonnet-4-5");
        assert_eq!(kc[1], "opencode-zen");
    }

    #[test]
    fn favorites_get_returns_empty_for_fresh_db() {
        let favorites = favorites_from_db(&AppDb::default());
        assert_eq!(favorites, BTreeMap::new());
    }
}
