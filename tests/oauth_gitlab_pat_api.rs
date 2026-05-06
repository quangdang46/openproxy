use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    AppState::new(db)
}

fn request(body: Body) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/api/oauth/gitlab/pat")
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

#[tokio::test]
async fn gitlab_pat_route_matches_9router_success_flow() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/user"))
        .and(header("private-token", "glpat-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "email": "me@example.com",
            "username": "gitlab-user",
            "name": "GitLab User"
        })))
        .mount(&server)
        .await;

    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(request(Body::from(
            json!({
                "token": " glpat-secret ",
                "baseUrl": format!("{}/", server.uri())
            })
            .to_string(),
        )))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "success": true }));

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.provider_connections.len(), 1);
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.provider, "gitlab");
    assert_eq!(connection.auth_type, "oauth");
    assert_eq!(connection.access_token.as_deref(), Some("glpat-secret"));
    assert_eq!(connection.email.as_deref(), Some("me@example.com"));
    assert_eq!(connection.display_name.as_deref(), Some("GitLab User"));
    assert_eq!(connection.test_status.as_deref(), Some("active"));
    assert_eq!(
        connection.provider_specific_data.get("baseUrl"),
        Some(&json!(server.uri()))
    );
    assert_eq!(
        connection.provider_specific_data.get("authKind"),
        Some(&json!("personal_access_token"))
    );
    assert_eq!(
        connection.provider_specific_data.get("username"),
        Some(&json!("gitlab-user"))
    );
}

#[tokio::test]
async fn gitlab_pat_route_rejects_invalid_body_and_missing_token() {
    let app = openproxy::build_app(app_state().await);

    let invalid = app.clone().oneshot(request(Body::from("{"))).await.unwrap();
    let (status, json) = response_json(invalid).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json, json!({ "error": "Invalid request body" }));

    let missing_token = app
        .oneshot(request(Body::from(json!({ "token": "   " }).to_string())))
        .await
        .unwrap();
    let (status, json) = response_json(missing_token).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        json,
        json!({ "error": "Personal Access Token is required" })
    );
}

#[tokio::test]
async fn gitlab_pat_route_returns_401_when_verification_fails() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/user"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad token"))
        .mount(&server)
        .await;

    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(request(Body::from(
            json!({
                "token": "glpat-secret",
                "baseUrl": server.uri()
            })
            .to_string(),
        )))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        json,
        json!({ "error": "GitLab token verification failed: bad token" })
    );
}
