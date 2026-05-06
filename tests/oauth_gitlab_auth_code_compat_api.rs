use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use tower::util::ServiceExt;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    AppState::new(db)
}

fn get_request(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn post_request(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
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
async fn gitlab_authorize_matches_9router_query_shape() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(get_request(
            "/api/oauth/gitlab/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback&baseUrl=https%3A%2F%2Fgitlab.example.com&clientId=gitlab-client",
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["flowType"], "authorization_code_pkce");
    assert_eq!(json["redirectUri"], "http://localhost:4624/callback");
    assert_eq!(json["callbackPath"], "/callback");
    assert!(json.get("fixedPort").is_none());

    let state = json["state"].as_str().expect("state");
    let code_verifier = json["codeVerifier"].as_str().expect("code verifier");
    let code_challenge = json["codeChallenge"].as_str().expect("code challenge");
    let expected_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
    assert_eq!(code_challenge, expected_challenge);
    assert_eq!(
        json["authUrl"],
        format!(
            "https://gitlab.example.com/oauth/authorize?client_id=gitlab-client&redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback&response_type=code&state={state}&scope=api+read_user&code_challenge={code_challenge}&code_challenge_method=S256"
        )
    );
}

#[tokio::test]
async fn gitlab_exchange_matches_9router_meta_and_saves_connection() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("client_id=gitlab-client"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=auth-code"))
        .and(body_string_contains(
            "redirect_uri=http%3A%2F%2Flocalhost%3A4624%2Fcallback",
        ))
        .and(body_string_contains("code_verifier=pkce-verifier"))
        .and(body_string_contains("client_secret=gitlab-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "gitlab-access",
            "refresh_token": "gitlab-refresh",
            "expires_in": 7200,
            "scope": "api read_user"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v4/user"))
        .and(header("authorization", "Bearer gitlab-access"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "username": "gitlab-user",
            "email": "me@example.com",
            "name": "GitLab User"
        })))
        .mount(&server)
        .await;

    let state = app_state().await;
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(post_request(
            "/api/oauth/gitlab/exchange",
            json!({
                "code": "auth-code",
                "redirectUri": "http://localhost:4624/callback",
                "codeVerifier": "pkce-verifier",
                "meta": {
                    "baseUrl": server.uri(),
                    "clientId": "gitlab-client",
                    "clientSecret": "gitlab-secret"
                }
            }),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["success"], true);
    assert_eq!(json["connection"]["provider"], "gitlab");
    assert!(json["connection"].get("email").is_none());
    assert!(json["connection"].get("displayName").is_none());

    let snapshot = state.db.snapshot();
    assert_eq!(snapshot.provider_connections.len(), 1);
    let connection = &snapshot.provider_connections[0];
    assert_eq!(connection.provider, "gitlab");
    assert_eq!(connection.auth_type, "oauth");
    assert_eq!(connection.name.as_deref(), Some("Account 1"));
    assert_eq!(connection.access_token.as_deref(), Some("gitlab-access"));
    assert_eq!(connection.refresh_token.as_deref(), Some("gitlab-refresh"));
    assert_eq!(connection.scope.as_deref(), Some("api read_user"));
    assert_eq!(connection.test_status.as_deref(), Some("active"));
    assert_eq!(
        connection.provider_specific_data.get("username"),
        Some(&json!("gitlab-user"))
    );
    assert_eq!(
        connection.provider_specific_data.get("email"),
        Some(&json!("me@example.com"))
    );
    assert_eq!(
        connection.provider_specific_data.get("name"),
        Some(&json!("GitLab User"))
    );
    assert_eq!(
        connection.provider_specific_data.get("baseUrl"),
        Some(&json!(server.uri()))
    );
    assert_eq!(
        connection.provider_specific_data.get("clientId"),
        Some(&json!("gitlab-client"))
    );
    assert_eq!(
        connection.provider_specific_data.get("authKind"),
        Some(&json!("oauth"))
    );
}
