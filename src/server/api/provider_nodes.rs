use axum::{
    extract::{Path, State},
    http::HeaderMap,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::server::state::AppState;
use crate::types::ProviderNode;

fn require_management_access(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    super::require_dashboard_or_management_api_key(headers, state)
}

// ============================================================
// Provider Nodes CRUD API - /api/provider-nodes
// ============================================================

#[derive(Debug, Serialize)]
pub struct ProviderNodesListResponse {
    pub nodes: Vec<ProviderNode>,
}

#[derive(Debug, Serialize)]
pub struct ProviderNodeResponse {
    pub node: ProviderNode,
}

// GET /api/provider-nodes - List all nodes
async fn list_provider_nodes(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    Json(ProviderNodesListResponse {
        nodes: snapshot.provider_nodes.clone(),
    })
    .into_response()
}

// GET /api/provider-nodes/{id} - Get specific node
async fn get_provider_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();

    match snapshot.provider_nodes.iter().find(|n| n.id == id) {
        Some(node) => Json(ProviderNodeResponse { node: node.clone() }).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Node not found" })),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProviderNodeRequest {
    pub name: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub api_type: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub r#type: Option<String>,
}

// POST /api/provider-nodes - Create node
async fn create_provider_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateProviderNodeRequest>,
) -> impl IntoResponse {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let node = ProviderNode {
        id: id.clone(),
        r#type: req
            .r#type
            .unwrap_or_else(|| "openai-compatible".to_string()),
        name: req.name,
        prefix: req.prefix,
        api_type: req.api_type,
        base_url: req.base_url,
        created_at: Some(now.clone()),
        updated_at: Some(now),
        extra: std::collections::BTreeMap::new(),
    };

    let result = state
        .db
        .update(|db| {
            db.provider_nodes.push(node.clone());
        })
        .await;

    match result {
        Ok(_) => (
            StatusCode::CREATED,
            Json(json!({ "success": true, "node": node })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProviderNodeRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub api_type: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub r#type: Option<String>,
}

// PUT /api/provider-nodes/{id} - Update node
async fn update_provider_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdateProviderNodeRequest>,
) -> impl IntoResponse {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    // First check if node exists
    let snapshot = state.db.snapshot();
    let node_exists = snapshot.provider_nodes.iter().any(|n| n.id == id);

    if !node_exists {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Node not found" })),
        )
            .into_response();
    }

    let result = state
        .db
        .update(|db| {
            if let Some(node) = db.provider_nodes.iter_mut().find(|n| n.id == id) {
                if let Some(name) = req.name {
                    node.name = name;
                }
                if let Some(prefix) = req.prefix {
                    node.prefix = Some(prefix);
                }
                if let Some(api_type) = req.api_type {
                    node.api_type = Some(api_type);
                }
                if let Some(base_url) = req.base_url {
                    node.base_url = Some(base_url);
                }
                if let Some(r#type) = req.r#type {
                    node.r#type = r#type;
                }
                node.updated_at = Some(chrono::Utc::now().to_rfc3339());
            }
        })
        .await;

    match result {
        Ok(_) => {
            // Fetch updated node
            let snapshot = state.db.snapshot();
            match snapshot.provider_nodes.iter().find(|n| n.id == id) {
                Some(node) => Json(ProviderNodeResponse { node: node.clone() }).into_response(),
                None => (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": "Node not found after update" })),
                )
                    .into_response(),
            }
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// DELETE /api/provider-nodes/{id} - Delete node
async fn delete_provider_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    // First check if node exists
    let snapshot = state.db.snapshot();
    let node_exists = snapshot.provider_nodes.iter().any(|n| n.id == id);

    if !node_exists {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Node not found" })),
        )
            .into_response();
    }

    let result = state
        .db
        .update(|db| {
            db.provider_nodes.retain(|n| n.id != id);
        })
        .await;

    match result {
        Ok(_) => Json(json!({ "success": true, "message": "Node deleted" })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ============================================================
// Route Registration
// ============================================================

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/provider-nodes", get(list_provider_nodes))
        .route("/api/provider-nodes", post(create_provider_node))
        .route("/api/provider-nodes/{id}", get(get_provider_node))
        .route("/api/provider-nodes/{id}", put(update_provider_node))
        .route("/api/provider-nodes/{id}", delete(delete_provider_node))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::server::state::AppState;
    use crate::types::ApiKey;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tower::ServiceExt;

    const TEST_KEY: &str = "provider-node-test-key";

    async fn create_test_state() -> AppState {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
        db.update(|state| {
            state.api_keys = vec![ApiKey {
                id: "test-key-id".to_string(),
                name: "test".to_string(),
                key: TEST_KEY.to_string(),
                machine_id: None,
                is_active: Some(true),
                created_at: None,
                extra: BTreeMap::new(),
            }];
        })
        .await
        .expect("seed auth");
        AppState::new(db)
    }

    #[tokio::test]
    async fn test_list_provider_nodes_empty() {
        let state = create_test_state().await;
        let app = routes().with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/provider-nodes")
                    .header("Authorization", format!("Bearer {TEST_KEY}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_create_provider_node() {
        let state = create_test_state().await;
        let app = routes().with_state(state);

        let request_body = serde_json::json!({
            "name": "Test Node",
            "prefix": "test-prefix",
            "apiType": "chat",
            "baseUrl": "https://api.test.com/v1",
            "type": "openai-compatible"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/provider-nodes")
                    .header("Authorization", format!("Bearer {TEST_KEY}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(request_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn test_get_provider_node_not_found() {
        let state = create_test_state().await;
        let app = routes().with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/provider-nodes/nonexistent-id")
                    .header("Authorization", format!("Bearer {TEST_KEY}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_delete_provider_node_not_found() {
        let state = create_test_state().await;
        let app = routes().with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/provider-nodes/nonexistent-id")
                    .header("Authorization", format!("Bearer {TEST_KEY}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
