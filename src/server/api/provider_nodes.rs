use axum::{
    extract::{Path, State},
    http::HeaderMap,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::server::state::AppState;
use crate::types::ProviderNode;

fn require_management_access(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    super::require_dashboard_or_management_api_key(headers, state)
}

/// Strip the trailing slash and the executor's own path segment from a node's
/// base URL. 9router does this per node type on both create and edit
/// (`provider-nodes/route.js:66-86`, `[id]/route.js:33-49`) because the
/// executor appends that segment on the way out — a stored URL that still ends
/// in it is suffixed twice and the upstream 404s.
fn sanitize_node_base_url(node_type: &str, base_url: &str) -> String {
    let trimmed = base_url.trim();
    let path_suffix = match node_type {
        "anthropic-compatible" => "/messages",
        "custom-embedding" => "/embeddings",
        _ => return trimmed.to_string(),
    };
    let stripped = trimmed.strip_suffix('/').unwrap_or(trimmed);
    stripped
        .strip_suffix(path_suffix)
        .unwrap_or(stripped)
        .to_string()
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

    let node_type = req
        .r#type
        .unwrap_or_else(|| "openai-compatible".to_string());
    // 9router normalises on create too (provider-nodes/route.js:66-86): a node
    // that is never edited still has to store a URL the executor can suffix.
    let base_url = req
        .base_url
        .as_deref()
        .map(|base_url| sanitize_node_base_url(&node_type, base_url));

    let node = ProviderNode {
        id: id.clone(),
        r#type: node_type,
        name: req.name,
        prefix: req.prefix,
        api_type: req.api_type,
        base_url,
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

    // 9router reads the node once and never writes `type` in this handler,
    // so the `apiType` gate and the base-URL sanitization below key off the
    // type as stored, not the one this PUT writes (route.js:25,36,44,57).
    let snapshot = state.db.snapshot();
    let Some(stored) = snapshot.provider_nodes.iter().find(|n| n.id == id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Node not found" })),
        )
            .into_response();
    };

    let node_type = stored.r#type.clone();
    let node_name = req.name.clone().unwrap_or(stored.name.clone());
    let now = chrono::Utc::now().to_rfc3339();
    let sanitized_base_url = req
        .base_url
        .as_deref()
        .map(|base_url| sanitize_node_base_url(&node_type, base_url));

    let result = state
        .db
        .update(|db| {
            if let Some(node) = db.provider_nodes.iter_mut().find(|n| n.id == id) {
                if let Some(name) = &req.name {
                    node.name = name.clone();
                }
                if let Some(prefix) = &req.prefix {
                    node.prefix = Some(prefix.clone());
                }
                if let Some(api_type) = &req.api_type {
                    node.api_type = Some(api_type.clone());
                }
                if let Some(base_url) = &sanitized_base_url {
                    node.base_url = Some(base_url.clone());
                }
                if let Some(r#type) = &req.r#type {
                    node.r#type = r#type.clone();
                }
                node.updated_at = Some(now.clone());
            }

            // A connection resolves its URL, prefix and API dialect from the
            // node's fields copied into `providerSpecificData`, so editing the
            // node has to reach every connection that references it or the edit
            // is invisible until each key is re-entered by hand
            // (route.js:63-74). Merge key by key: a provider-specific key this
            // PUT did not carry must survive.
            for connection in db
                .provider_connections
                .iter_mut()
                .filter(|c| c.provider == id)
            {
                let data = &mut connection.provider_specific_data;
                if let Some(prefix) = &req.prefix {
                    data.insert("prefix".into(), Value::String(prefix.trim().to_string()));
                }
                if node_type == "openai-compatible" {
                    if let Some(api_type) = &req.api_type {
                        data.insert("apiType".into(), Value::String(api_type.clone()));
                    }
                }
                if let Some(base_url) = &sanitized_base_url {
                    data.insert("baseUrl".into(), Value::String(base_url.clone()));
                }
                data.insert("nodeName".into(), Value::String(node_name.clone()));
                connection.updated_at = Some(now.clone());
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

    // Cascade: a connection references a node by using the node id as its
    // provider alias, so leaving those rows behind would keep them listed by
    // /api/providers and selectable in filter_available_accounts with no UI
    // left to remove them. Custom models are keyed `providerAlias|id|type` in
    // their own scope and are deliberately not cascaded: 9router's delete
    // leaves them behind too, so a node deleted and recreated with the same id
    // gets the user's custom model list back instead of losing it.
    let result = state
        .db
        .update(|db| {
            db.provider_nodes.retain(|n| n.id != id);
            db.provider_connections
                .retain(|c| c.provider.as_str() != id.as_str());
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
                monthly_budget_usd: None,
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
