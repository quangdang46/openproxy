use std::collections::BTreeMap;

use axum::extract::State;
use axum::{
    routing::{delete, get, post, put},
    Json, Router,
};
use uuid::Uuid;

use crate::server::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/tags", get(list_tags).post(create_tag))
        .route("/api/tags/{id}", get(get_tag).put(update_tag).delete(delete_tag))
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    pub created_at: Option<String>,
}

impl Tag {
    pub fn new(name: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            color: None,
            created_at: Some(chrono::Utc::now().to_rfc3339()),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTagRequest {
    pub name: String,
    pub color: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateTagRequest {
    pub name: Option<String>,
    pub color: Option<String>,
}

type TagsStore = BTreeMap<String, Tag>;

fn get_tags_from_db(db: &crate::types::AppDb) -> TagsStore {
    db.extra
        .get("tags")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

fn save_tags_to_db(db: &mut crate::types::AppDb, tags: &TagsStore) {
    if let Ok(value) = serde_json::to_value(tags) {
        db.extra.insert("tags".to_string(), value);
    }
}

async fn list_tags(State(state): State<AppState>) -> Json<Vec<Tag>> {
    let snapshot = state.db.snapshot();
    let tags = get_tags_from_db(&snapshot);
    Json(tags.into_values().collect())
}

async fn get_tag(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<Option<Tag>> {
    let snapshot = state.db.snapshot();
    let tags = get_tags_from_db(&snapshot);
    Json(tags.get(&id).cloned())
}

async fn create_tag(
    State(state): State<AppState>,
    Json(req): Json<CreateTagRequest>,
) -> Json<serde_json::Value> {
    let mut tag = Tag::new(req.name);
    tag.color = req.color;

    let result = state
        .db
        .update(|db| {
            let mut tags = get_tags_from_db(db);
            tags.insert(tag.id.clone(), tag.clone());
            save_tags_to_db(db, &tags);
        })
        .await;

    match result {
        Ok(_) => Json(serde_json::json!({ "success": true, "tag": tag })),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e.to_string() })),
    }
}

async fn update_tag(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<UpdateTagRequest>,
) -> Json<serde_json::Value> {
    let result = state
        .db
        .update(|db| {
            let mut tags = get_tags_from_db(db);
            if let Some(tag) = tags.get_mut(&id) {
                if let Some(name) = req.name {
                    tag.name = name;
                }
                if let Some(color) = req.color {
                    tag.color = Some(color);
                }
            }
            save_tags_to_db(db, &tags);
        })
        .await;

    match result {
        Ok(_) => Json(serde_json::json!({ "success": true })),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e.to_string() })),
    }
}

async fn delete_tag(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<serde_json::Value> {
    let result = state
        .db
        .update(|db| {
            let mut tags = get_tags_from_db(db);
            tags.remove(&id);
            save_tags_to_db(db, &tags);
        })
        .await;

    match result {
        Ok(_) => Json(serde_json::json!({ "success": true })),
        Err(e) => Json(serde_json::json!({ "success": false, "error": e.to_string() })),
    }
}