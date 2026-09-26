//! Provider-node edit parity with 9router (`api/provider-nodes/[id]/route.js`).
//!
//! A connection does not read its node at request time — it holds its own
//! copy of the node's URL, prefix and API dialect in `providerSpecificData`.
//! Editing the node therefore has to reach every connection that references
//! it, or the node editor shows a new base URL while traffic keeps going to
//! the old upstream. 9router fans the write out (`route.js:63-74`); the Rust
//! PUT only touched the node row.
//!
//! The same handler normalises the base URL per node type before storing it
//! (`route.js:33-49`), because the executor appends `/messages` or
//! `/embeddings` itself and a stored URL that still ends in that segment is
//! suffixed twice.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, ProviderConnection, ProviderNode};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

// The provider-node router on its own: this file is about these four routes,
// and mounting the whole app would couple it to every other route table.
fn app(state: &AppState) -> axum::Router {
    openproxy::server::api::provider_nodes::routes().with_state(state.clone())
}

const BEARER: &str = "provider-node-bearer";

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![ApiKey {
            id: "bearer-id".into(),
            name: "Local".into(),
            key: BEARER.into(),
            machine_id: None,
            is_active: Some(true),
            created_at: None,
            monthly_budget_usd: None,
            extra: BTreeMap::new(),
        }];
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

fn request(method: Method, uri: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {BEARER}"))
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

async fn response_json(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn node(id: &str, name: &str, node_type: &str) -> ProviderNode {
    ProviderNode {
        id: id.into(),
        r#type: node_type.into(),
        name: name.into(),
        prefix: Some("p".into()),
        api_type: Some("chat".into()),
        base_url: Some("http://a".into()),
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

fn connection(id: &str, provider: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        provider: provider.into(),
        api_key: Some("sk-test".into()),
        ..ProviderConnection::default()
    }
}

/// The `providerSpecificData` map a connection carries in memory.
fn in_memory_data(state: &AppState, conn_id: &str) -> BTreeMap<String, Value> {
    let snapshot = state.db.snapshot();
    snapshot
        .provider_connections
        .iter()
        .find(|c| c.id == conn_id)
        .unwrap_or_else(|| panic!("connection {conn_id} is missing"))
        .provider_specific_data
        .clone()
}

/// The same map as SQLite holds it. `providerSpecificData` is not part of the
/// encrypted secret set, so the fan-out can be asserted straight off disk —
/// an in-memory-only update that never reached the diff would pass every
/// snapshot assertion.
fn stored_data(state: &AppState, conn_id: &str) -> Value {
    let raw: String = state
        .db
        .sqlite
        .with_conn(|c| {
            c.query_row(
                "SELECT data FROM providerConnections WHERE id = ?1",
                [conn_id],
                |r| r.get(0),
            )
        })
        .unwrap_or_else(|e| panic!("read stored connection {conn_id}: {e}"));
    serde_json::from_str::<Value>(&raw).expect("data blob is json")["providerSpecificData"].clone()
}

#[tokio::test]
async fn put_provider_node_fans_out_to_member_connections() {
    let state = app_state().await;
    state
        .db
        .update(|db| {
            db.provider_nodes
                .push(node("node-a", "Node A", "openai-compatible"));
            db.provider_nodes
                .push(node("node-b", "Node B", "openai-compatible"));
            db.provider_connections.push(connection("conn-a", "node-a"));
            db.provider_connections
                .push(connection("conn-a2", "node-a"));
            db.provider_connections.push(connection("conn-b", "node-b"));
            // A key the PUT does not carry must survive the merge.
            db.provider_connections[0]
                .provider_specific_data
                .insert("region".into(), json!("eu-west-1"));
        })
        .await
        .unwrap();

    let app = app(&state);
    let response = app
        .oneshot(request(
            Method::PUT,
            "/api/provider-nodes/node-a",
            Body::from(
                json!({
                    "name": "Node A2",
                    "prefix": "  p2  ",
                    "baseUrl": "http://b",
                    "apiType": "responses",
                })
                .to_string(),
            ),
        ))
        .await
        .unwrap();

    let (status, body) = response_json(response).await;
    assert_eq!(status, StatusCode::OK, "PUT failed: {body}");
    assert_eq!(body["node"]["name"], json!("Node A2"));

    for conn_id in ["conn-a", "conn-a2"] {
        let data = in_memory_data(&state, conn_id);
        assert_eq!(data.get("prefix"), Some(&json!("p2")), "{conn_id} prefix");
        assert_eq!(
            data.get("baseUrl"),
            Some(&json!("http://b")),
            "{conn_id} baseUrl"
        );
        assert_eq!(
            data.get("apiType"),
            Some(&json!("responses")),
            "{conn_id} apiType"
        );
        assert_eq!(
            data.get("nodeName"),
            Some(&json!("Node A2")),
            "{conn_id} nodeName"
        );

        let stored = stored_data(&state, conn_id);
        assert_eq!(
            stored["baseUrl"],
            json!("http://b"),
            "{conn_id} stored baseUrl"
        );
        assert_eq!(
            stored["nodeName"],
            json!("Node A2"),
            "{conn_id} stored nodeName"
        );
    }

    assert_eq!(
        in_memory_data(&state, "conn-a").get("region"),
        Some(&json!("eu-west-1")),
        "the fan-out must merge, not replace: 'region' was not in the PUT"
    );

    // A node's edit must not reach another node's connections.
    assert!(
        in_memory_data(&state, "conn-b").is_empty(),
        "conn-b belongs to node-b and must be untouched"
    );
}

#[tokio::test]
async fn put_provider_node_normalizes_base_url_per_type() {
    let state = app_state().await;
    state
        .db
        .update(|db| {
            db.provider_nodes
                .push(node("node-anth", "Anth", "anthropic-compatible"));
            db.provider_nodes
                .push(node("node-emb", "Emb", "custom-embedding"));
            db.provider_connections
                .push(connection("conn-anth", "node-anth"));
            db.provider_connections
                .push(connection("conn-emb", "node-emb"));
            // 9router leaves the previous value in place for a node type that
            // has no apiType (`apiType: undefined` is dropped by JSON).
            db.provider_connections[0]
                .provider_specific_data
                .insert("apiType".into(), json!("messages"));
        })
        .await
        .unwrap();

    let app = app(&state);
    for (node_id, base_url) in [
        ("node-anth", "https://api.example/v1/messages"),
        ("node-emb", "https://api.example/v1/embeddings/"),
    ] {
        let response = app
            .clone()
            .oneshot(request(
                Method::PUT,
                &format!("/api/provider-nodes/{node_id}"),
                Body::from(json!({ "baseUrl": base_url, "apiType": "chat" }).to_string()),
            ))
            .await
            .unwrap();
        let (status, body) = response_json(response).await;
        assert_eq!(status, StatusCode::OK, "PUT {node_id} failed: {body}");
        assert_eq!(body["node"]["baseUrl"], json!("https://api.example/v1"));
    }

    for conn_id in ["conn-anth", "conn-emb"] {
        assert_eq!(
            stored_data(&state, conn_id)["baseUrl"],
            json!("https://api.example/v1"),
            "{conn_id} would be double-suffixed by the executor"
        );
    }

    assert_eq!(
        in_memory_data(&state, "conn-anth").get("apiType"),
        Some(&json!("messages")),
        "apiType is only fanned out for openai-compatible nodes; the existing value survives"
    );
    assert_eq!(
        in_memory_data(&state, "conn-emb").get("apiType"),
        None,
        "custom-embedding has no apiType, so the PUT must not introduce one"
    );
}

#[tokio::test]
async fn post_provider_node_normalizes_base_url_per_type() {
    let state = app_state().await;
    let app = app(&state);

    // 9router normalises on create too (provider-nodes/route.js:66-86), so a
    // node that is never edited still stores a URL the executor can suffix.
    for (node_type, base_url) in [
        ("anthropic-compatible", "https://api.example/v1/messages"),
        ("custom-embedding", "https://api.example/v1/embeddings/"),
    ] {
        let response = app
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/provider-nodes",
                Body::from(
                    json!({ "name": "Node", "prefix": "p", "baseUrl": base_url, "type": node_type })
                        .to_string(),
                ),
            ))
            .await
            .unwrap();

        let (status, body) = response_json(response).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "POST {node_type} failed: {body}"
        );
        assert_eq!(body["node"]["baseUrl"], json!("https://api.example/v1"));
    }
}
