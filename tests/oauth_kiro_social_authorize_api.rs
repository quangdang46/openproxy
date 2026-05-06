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

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    AppState::new(db)
}

fn request(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
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

fn is_base64url_no_pad(value: &str) -> bool {
    value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

#[tokio::test]
async fn kiro_social_authorize_matches_9router_google_response() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(request("/api/oauth/kiro/social-authorize?provider=google"))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["provider"], "google");

    let auth_url = json["authUrl"].as_str().expect("auth url");
    let state = json["state"].as_str().expect("state");
    let code_verifier = json["codeVerifier"].as_str().expect("code verifier");
    let code_challenge = json["codeChallenge"].as_str().expect("code challenge");

    assert_eq!(state.len(), 43);
    assert_eq!(code_verifier.len(), 43);
    assert_eq!(code_challenge.len(), 43);
    assert!(is_base64url_no_pad(state));
    assert!(is_base64url_no_pad(code_verifier));
    assert!(is_base64url_no_pad(code_challenge));

    let expected_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
    assert_eq!(code_challenge, expected_challenge);
    assert_eq!(
        auth_url,
        format!(
            "https://prod.us-east-1.auth.desktop.kiro.dev/login?idp=Google&redirect_uri=kiro%3A%2F%2Fkiro.kiroAgent%2Fauthenticate-success&code_challenge={code_challenge}&code_challenge_method=S256&state={state}&prompt=select_account"
        )
    );
}

#[tokio::test]
async fn kiro_social_authorize_uses_github_idp_name_like_9router() {
    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(request("/api/oauth/kiro/social-authorize?provider=github"))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["provider"], "github");
    assert!(json["authUrl"]
        .as_str()
        .is_some_and(|url| url.contains("idp=Github")));
}

#[tokio::test]
async fn kiro_social_authorize_rejects_missing_or_invalid_provider() {
    let app = openproxy::build_app(app_state().await);

    let missing = app
        .clone()
        .oneshot(request("/api/oauth/kiro/social-authorize"))
        .await
        .unwrap();
    let (status, json) = response_json(missing).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        json,
        json!({ "error": "Invalid provider. Use 'google' or 'github'" })
    );

    let invalid = app
        .oneshot(request("/api/oauth/kiro/social-authorize?provider=gitlab"))
        .await
        .unwrap();
    let (status, json) = response_json(invalid).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        json,
        json!({ "error": "Invalid provider. Use 'google' or 'github'" })
    );
}
