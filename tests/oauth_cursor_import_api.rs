use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    AppState::new(db)
}

fn request(method: Method, uri: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

async fn response_json(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap();
    (status, json)
}

fn make_cursor_jwt(email: &str, user_id: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(
        json!({
            "email": email,
            "sub": user_id
        })
        .to_string(),
    );
    format!("{header}.{payload}.cursor-signature-padding-for-length-check")
}

#[tokio::test]
async fn cursor_import_get_matches_9router_instructions() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(request(
            Method::GET,
            "/api/oauth/cursor/import",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "provider": "cursor",
            "method": "import_token",
            "instructions": {
                "title": "How to get your Cursor token",
                "steps": [
                    "1. Open Cursor IDE and make sure you're logged in",
                    "2. Find the state.vscdb file:",
                    "   - Linux: ~/.config/Cursor/User/globalStorage/state.vscdb",
                    "   - macOS: /Users/<user>/Library/Application Support/Cursor/User/globalStorage/state.vscdb",
                    "   - Windows: %APPDATA%\\Cursor\\User\\globalStorage\\state.vscdb",
                    "3. Open the database with SQLite browser or CLI:",
                    "   sqlite3 state.vscdb \"SELECT value FROM itemTable WHERE key='cursorAuth/accessToken'\"",
                    "4. Also get the machine ID:",
                    "   sqlite3 state.vscdb \"SELECT value FROM itemTable WHERE key='storage.serviceMachineId'\"",
                    "5. Paste both values in the form below"
                ],
                "alternativeMethod": [
                    "Or use this one-liner to get both values:",
                    "sqlite3 state.vscdb \"SELECT key, value FROM itemTable WHERE key IN ('cursorAuth/accessToken', 'storage.serviceMachineId')\""
                ]
            },
            "requiredFields": [
                {
                    "name": "accessToken",
                    "label": "Access Token",
                    "description": "From cursorAuth/accessToken in state.vscdb",
                    "type": "textarea"
                },
                {
                    "name": "machineId",
                    "label": "Machine ID",
                    "description": "From storage.serviceMachineId in state.vscdb",
                    "type": "text"
                }
            ]
        })
    );
}

#[tokio::test]
async fn cursor_import_post_matches_9router_success_flow() {
    let access_token = make_cursor_jwt("me@example.com", "user-123");
    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(request(
            Method::POST,
            "/api/oauth/cursor/import",
            Body::from(
                json!({
                    "accessToken": format!("  {access_token}  "),
                    "machineId": " 550e8400-e29b-41d4-a716-446655440000 "
                })
                .to_string(),
            ),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["connection"]["provider"], "cursor");
    assert_eq!(json["connection"]["email"], "me@example.com");
    assert!(json["connection"]["id"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.provider_connections.len(), 1);
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.provider, "cursor");
    assert_eq!(connection.auth_type, "oauth");
    assert_eq!(connection.name.as_deref(), Some("me@example.com"));
    assert_eq!(connection.email.as_deref(), Some("me@example.com"));
    assert_eq!(
        connection.access_token.as_deref(),
        Some(access_token.as_str())
    );
    assert_eq!(connection.refresh_token, None);
    assert!(connection.expires_at.is_some());
    assert_eq!(connection.test_status.as_deref(), Some("active"));
    assert_eq!(
        connection.provider_specific_data.get("machineId"),
        Some(&json!("550e8400-e29b-41d4-a716-446655440000"))
    );
    assert_eq!(
        connection.provider_specific_data.get("authMethod"),
        Some(&json!("imported"))
    );
    assert_eq!(
        connection.provider_specific_data.get("provider"),
        Some(&json!("Imported"))
    );
    assert_eq!(
        connection.provider_specific_data.get("userId"),
        Some(&json!("user-123"))
    );
}

#[tokio::test]
async fn cursor_import_post_validates_inputs_like_9router() {
    let valid_token = make_cursor_jwt("me@example.com", "user-123");
    let app = openproxy::build_app(app_state().await);

    let missing_access_token = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/oauth/cursor/import",
            Body::from(json!({ "machineId": "550e8400-e29b-41d4-a716-446655440000" }).to_string()),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(missing_access_token).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json, json!({ "error": "Access token is required" }));

    let missing_machine_id = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/oauth/cursor/import",
            Body::from(json!({ "accessToken": valid_token }).to_string()),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(missing_machine_id).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json, json!({ "error": "Machine ID is required" }));

    let short_token = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/oauth/cursor/import",
            Body::from(
                json!({
                    "accessToken": "short-token",
                    "machineId": "550e8400-e29b-41d4-a716-446655440000"
                })
                .to_string(),
            ),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(short_token).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        json,
        json!({ "error": "Invalid token format. Token appears too short." })
    );

    let invalid_machine_id = app
        .oneshot(request(
            Method::POST,
            "/api/oauth/cursor/import",
            Body::from(
                json!({
                    "accessToken": make_cursor_jwt("me@example.com", "user-123"),
                    "machineId": "not-a-uuid"
                })
                .to_string(),
            ),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(invalid_machine_id).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        json,
        json!({ "error": "Invalid machine ID format. Expected UUID format." })
    );
}
