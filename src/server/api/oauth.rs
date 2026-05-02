use axum::{
    extract::{Path, Query, State, rejection::JsonRejection},
    http::StatusCode,
    response::IntoResponse,
    response::Response,
    routing::{get, post},
    Json, Router,
};
use std::str;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::oauth::pending::PendingOAuthFlow;
use crate::oauth::providers;
use crate::oauth::device_code;
use crate::oauth::{OAuthProviderConfig, TokenResponse};
use crate::server::auth::require_api_key;
use crate::server::state::AppState;
use crate::types::ProviderConnection;

const PKCE_FLOW_TTL_SECS: i64 = 600;
const DEVICE_FLOW_TTL_SECS: i64 = 900;

#[derive(Debug, Deserialize)]
pub struct StartQuery {
    pub redirect_uri: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeviceCodeBody {
    pub redirect_uri: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RefreshBody {
    pub refresh_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StartResponse {
    pub auth_url: String,
    pub state: String,
    pub provider: String,
    pub expires_in: u64,
}

#[derive(Debug, Serialize)]
pub struct CallbackResponse {
    pub success: bool,
    pub provider: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
    pub expires_in: u64,
}

#[derive(Debug, Serialize)]
pub struct PollResponse {
    pub success: bool,
    pub provider: String,
    pub expires_in: Option<u64>,
    pub pending: Option<bool>,
    pub retry_after: Option<u64>,
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RefreshResponse {
    pub success: bool,
    pub access_token: String,
    pub expires_in: u64,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub provider: String,
    pub connected: bool,
    pub auth_type: String,
    pub expires_at: Option<String>,
    pub needs_refresh: Option<bool>,
    pub scope: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OAuthError {
    pub error: OAuthErrorDetail,
}

#[derive(Debug, Serialize)]
pub struct OAuthErrorDetail {
    pub message: String,
    pub code: String,
    pub provider: String,
}

fn make_error(message: &str, code: &str, provider: &str) -> Json<OAuthError> {
    Json(OAuthError {
        error: OAuthErrorDetail {
            message: message.to_string(),
            code: code.to_string(),
            provider: provider.to_string(),
        },
    })
}

fn make_error_response(status: StatusCode, message: &str, code: &str, provider: &str) -> Response {
    (status, make_error(message, code, provider)).into_response()
}

fn generate_code_verifier() -> String {
    let mut random_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut random_bytes);
    let charset = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    random_bytes
        .iter()
        .map(|&b| charset[(b as usize) % charset.len()] as char)
        .collect()
}

fn generate_code_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let hash = hasher.finalize();
    URL_SAFE_NO_PAD.encode(hash)
}

fn generate_state() -> String {
    let mut random_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut random_bytes);
    URL_SAFE_NO_PAD.encode(random_bytes)
}

fn get_provider_config(provider: &str) -> Option<OAuthProviderConfig> {
    providers::get_config(provider)
}

fn is_pkce_provider(provider: &str) -> bool {
    matches!(provider, "claude" | "codex" | "gitlab")
}

fn is_device_code_provider(provider: &str) -> bool {
    matches!(provider, "github" | "kiro" | "kimi-coding" | "kilocode" | "codebuddy")
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

async fn store_connection(
    db: &crate::db::Db,
    account_id: &str,
    provider: &str,
    token_response: &TokenResponse,
    redirect_uri: Option<&str>,
) -> anyhow::Result<()> {
    let provider_config = get_provider_config(provider);
    let client_id = provider_config
        .as_ref()
        .and_then(|c| c.extra_params.get("client_id"))
        .map(|v| v.as_str())
        .unwrap_or("openproxy")
        .to_string();

    let now = now_secs();
    let expires_at = token_response.expires_in.map(|secs| {
        let expires = chrono::Utc::now() + chrono::Duration::seconds(secs);
        expires.to_rfc3339()
    });

    let redirect_uri = redirect_uri.map(|s| s.to_string()).or_else(|| {
        provider_config.as_ref().and_then(|c| c.extra_params.get("redirect_uri"))
            .map(|v| v.as_str())
            .map(|s| s.to_string())
    }).unwrap_or_else(|| "http://localhost:20128/oauth/callback".to_string());

    db.update(|db| {
        let snapshot = db;
        if let Some(conn_idx) = snapshot.provider_connections.iter().position(|conn| {
            conn.provider == provider && conn.id.contains(account_id)
        }) {
            snapshot.provider_connections[conn_idx].access_token = Some(token_response.access_token.clone());
            snapshot.provider_connections[conn_idx].refresh_token = token_response.refresh_token.clone();
            snapshot.provider_connections[conn_idx].expires_at = expires_at;
            snapshot.provider_connections[conn_idx].scope = token_response.scope.clone();
            snapshot.provider_connections[conn_idx].updated_at = Some(chrono::Utc::now().to_rfc3339());
        } else {
            let connection_id = format!("{}-{}", account_id, Uuid::new_v4());
            let connection = ProviderConnection {
                id: connection_id,
                provider: provider.to_string(),
                auth_type: "oauth".to_string(),
                name: None,
                priority: Some(100),
                is_active: Some(true),
                created_at: Some(chrono::Utc::now().to_rfc3339()),
                updated_at: Some(chrono::Utc::now().to_rfc3339()),
                display_name: None,
                email: None,
                global_priority: None,
                default_model: None,
                access_token: Some(token_response.access_token.clone()),
                refresh_token: token_response.refresh_token.clone(),
                expires_at,
                token_type: token_response.token_type.clone(),
                scope: token_response.scope.clone(),
                id_token: token_response.id_token.clone(),
                project_id: None,
                api_key: None,
                test_status: None,
                last_tested: None,
                last_error: None,
                last_error_at: None,
                rate_limited_until: None,
                expires_in: token_response.expires_in,
                error_code: None,
                consecutive_use_count: None,
                backoff_level: None,
                consecutive_errors: None,
                proxy_url: None,
                proxy_label: None,
                use_connection_proxy: None,
                provider_specific_data: std::collections::BTreeMap::new(),
                extra: std::collections::BTreeMap::new(),
            };
            snapshot.provider_connections.push(connection);
        }
    }).await?;
    Ok(())
}

// GET /api/oauth/:provider/start
pub async fn start_oauth_flow(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<StartQuery>,
    headers: axum::http::HeaderMap,
) -> Response {
    let api_key = match require_api_key(&headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };
    let account_id = &api_key.id;

    let provider_config = match get_provider_config(&provider) {
        Some(config) => config,
        None => return make_error_response(
            StatusCode::BAD_REQUEST,
            "Unknown provider",
            "unknown_provider",
            &provider,
        ),
    };

    let code_verifier = generate_code_verifier();
    let code_challenge = generate_code_challenge(&code_verifier);
    let state_value = generate_state();

    let redirect_uri = query.redirect_uri.as_deref().unwrap_or("http://localhost:20128/oauth/callback");

    let auth_url = provider_config.build_auth_url(
        "openproxy",
        redirect_uri,
        &state_value,
        &code_challenge,
    );

    let now = now_secs();
    let flow = PendingOAuthFlow {
        state: state_value.clone(),
        code_verifier: code_verifier.clone(),
        provider: provider.clone(),
        account_id: account_id.clone(),
        redirect_uri: Some(redirect_uri.to_string()),
        device_code: None,
        user_code: None,
        created_at: now,
        expires_at: now + PKCE_FLOW_TTL_SECS,
    };

    if let Err(_) = state.pending_flows.insert(flow) {
        return make_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to store flow",
            "internal_error",
            &provider,
        );
    }

    Json(StartResponse {
        auth_url,
        state: state_value,
        provider: provider.clone(),
        expires_in: PKCE_FLOW_TTL_SECS as u64,
    })
    .into_response()
}

// GET /api/oauth/:provider/callback
pub async fn oauth_callback(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    if let Some(error) = &query.error {
        let desc = query.error_description.as_deref().unwrap_or(error);
        return make_error_response(
            StatusCode::BAD_REQUEST,
            desc,
            error,
            &provider,
        );
    }

    let state_param = match &query.state {
        Some(s) => s,
        None => return make_error_response(
            StatusCode::BAD_REQUEST,
            "Missing state parameter",
            "missing_state",
            &provider,
        ),
    };

    let code = match &query.code {
        Some(c) => c,
        None => return make_error_response(
            StatusCode::BAD_REQUEST,
            "Missing code parameter",
            "missing_code",
            &provider,
        ),
    };

    let flow = match state.pending_flows.remove(state_param) {
        Some(f) => f,
        None => return make_error_response(
            StatusCode::NOT_FOUND,
            "Flow not found or expired",
            "flow_not_found",
            &provider,
        ),
    };

    let provider_config = match get_provider_config(&provider) {
        Some(config) => config,
        None => return make_error_response(
            StatusCode::BAD_REQUEST,
            "Unknown provider",
            "unknown_provider",
            &provider,
        ),
    };

    let redirect_uri = flow.redirect_uri.as_deref().unwrap_or("http://localhost:20128/oauth/callback");

    let token_response = match device_code::exchange_code_for_token(
        &provider_config,
        code,
        &flow.code_verifier,
        redirect_uri,
        "openproxy",
    )
    .await
    {
        Ok(resp) => resp,
        Err(e) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                &e.error_description.unwrap_or_else(|| e.error.clone()),
                &e.error,
                &provider,
            );
        }
    };

    if let Err(e) = store_connection(&state.db, &flow.account_id, &provider, &token_response, Some(redirect_uri)).await {
        return make_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to store connection: {}", e),
            "storage_error",
            &provider,
        );
    }

    Json(CallbackResponse {
        success: true,
        provider: provider.clone(),
        message: "OAuth flow completed successfully".to_string(),
    })
    .into_response()
}

// POST /api/oauth/:provider/device_code
pub async fn start_device_code(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(_query): Query<DeviceCodeBody>,
    headers: axum::http::HeaderMap,
) -> Response {
    let api_key = match require_api_key(&headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };
    let account_id = api_key.id;

    if !is_device_code_provider(&provider) {
        return make_error_response(
            StatusCode::BAD_REQUEST,
            "Provider does not support device code flow",
            "unsupported_flow",
            &provider,
        );
    }

    let provider_config = match get_provider_config(&provider) {
        Some(config) => config,
        None => return make_error_response(
            StatusCode::BAD_REQUEST,
            "Unknown provider",
            "unknown_provider",
            &provider,
        ),
    };

    let client_id = provider_config
        .extra_params.get("client_id")
        .map(|v| v.as_str())
        .unwrap_or("openproxy")
        .to_string();

    let device_resp = match device_code::start_device_flow(&provider_config, &client_id).await {
        Ok(resp) => resp,
        Err(e) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                &e.error_description.unwrap_or_else(|| e.error.clone()),
                &e.error,
                &provider,
            );
        }
    };

    let now = now_secs();
    let flow = PendingOAuthFlow {
        state: device_resp.device_code.clone(),
        code_verifier: String::new(),
        provider: provider.clone(),
        account_id: account_id.clone(),
        redirect_uri: None,
        device_code: Some(device_resp.device_code.clone()),
        user_code: Some(device_resp.user_code.clone()),
        created_at: now,
        expires_at: now + DEVICE_FLOW_TTL_SECS,
    };

    if let Err(_) = state.pending_flows.insert(flow) {
        return make_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to store flow",
            "internal_error",
            &provider,
        );
    }

    Json(DeviceCodeResponse {
        device_code: device_resp.device_code,
        user_code: device_resp.user_code,
        verification_uri: device_resp.verification_uri,
        interval: device_resp.interval,
        expires_in: device_resp.expires_in.unwrap_or(DEVICE_FLOW_TTL_SECS) as u64,
    })
    .into_response()
}

// POST /api/oauth/:provider/poll
pub async fn poll_device_code(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    request: axum::extract::Request,
) -> Response {
    let headers = request.headers();
    let api_key = match require_api_key(headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };
    let account_id = api_key.id;

    let body = match axum::body::to_bytes(request.into_body(), 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Invalid request body",
                "invalid_body",
                &provider,
            );
        }
    };
    let body: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Invalid JSON body",
                "invalid_body",
                &provider,
            );
        }
    };
    let device_code = match body.get("device_code").and_then(|v| v.as_str()) {
        Some(code) => code.trim().to_string(),
        None => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Missing device_code in request body",
                "missing_device_code",
                &provider,
            );
        }
    };

    let _account_id = account_id;

    let pending_flow = state.pending_flows.get(&device_code);
    let flow = match pending_flow {
        Some(f) => f,
        None => {
            return make_error_response(
                StatusCode::NOT_FOUND,
                "Device code flow not found or expired",
                "flow_not_found",
                &provider,
            );
        }
    };

    let provider_config = match get_provider_config(&provider) {
        Some(config) => config,
        None => return make_error_response(
            StatusCode::BAD_REQUEST,
            "Unknown provider",
            "unknown_provider",
            &provider,
        ),
    };

    let user_code = flow.user_code.clone().unwrap_or_default();
    let interval = provider_config
        .extra_params.get("interval")
        .map(|v| v.as_str())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5);

    match device_code::poll_for_token(&provider_config, &device_code, &user_code, interval).await {
        Ok(token_response) => {
            state.pending_flows.remove(&device_code);

            // GitHub special: exchange OAuth token for Copilot token
            let final_token_response = if provider == "github" {
                match device_code::exchange_github_copilot_token(&token_response.access_token).await {
                    Ok(copilot_token) => copilot_token,
                    Err(e) => {
                        return make_error_response(
                            StatusCode::BAD_REQUEST,
                            &format!("Copilot token exchange failed: {}", e.error_description.unwrap_or_else(|| e.error.clone())),
                            "copilot_exchange_failed",
                            &provider,
                        );
                    }
                }
            } else {
                token_response
            };

            if let Err(e) = store_connection(&state.db, &flow.account_id, &provider, &final_token_response, None).await {
                return make_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("Failed to store connection: {}", e),
                    "storage_error",
                    &provider,
                );
            }

            Json(PollResponse {
                success: true,
                provider: provider.clone(),
                expires_in: final_token_response.expires_in.map(|e| e as u64),
                pending: Some(false),
                retry_after: None,
                message: Some("Authorization successful".to_string()),
            })
            .into_response()
        }
        Err(e) => {
            if e.error == "authorization_pending" || e.error == "slow_down" {
                let retry_after = provider_config
                    .extra_params.get("interval")
                    .map(|v| v.as_str())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(5);
                return Json(PollResponse {
                    success: false,
                    provider: provider.clone(),
                    expires_in: None,
                    pending: Some(true),
                    retry_after: Some(retry_after),
                    message: Some("Pending authorization".to_string()),
                })
                .into_response();
            }

            state.pending_flows.remove(&device_code);

            return make_error_response(
                StatusCode::BAD_REQUEST,
                &e.error_description.unwrap_or_else(|| e.error.clone()),
                &e.error,
                &provider,
            );
        }
    }
}

// POST /api/oauth/:provider/refresh
pub async fn refresh_token(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    request: axum::extract::Request,
) -> Response {
    let headers = request.headers();
    let api_key = match require_api_key(headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };
    let account_id = api_key.id;

    let body_bytes = match axum::body::to_bytes(request.into_body(), 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Invalid request body",
                "invalid_body",
                &provider,
            );
        }
    };
    let body: RefreshBody = match serde_json::from_slice(&body_bytes) {
        Ok(b) => b,
        Err(_) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Invalid JSON body",
                "invalid_body",
                &provider,
            );
        }
    };

    let snapshot = state.db.snapshot();
    let connection = snapshot.provider_connections.iter().find(|conn| {
        conn.provider == provider && conn.id.contains(&account_id)
    });

    let refresh_token = match body.refresh_token {
        Some(ref token) => token.clone(),
        None => connection
            .and_then(|c| c.refresh_token.clone())
            .unwrap_or_default(),
    };

    if refresh_token.is_empty() {
        return make_error_response(
            StatusCode::BAD_REQUEST,
            "No refresh token available",
            "no_refresh_token",
            &provider,
        );
    }

    let provider_config = match get_provider_config(&provider) {
        Some(config) => config,
        None => return make_error_response(
            StatusCode::BAD_REQUEST,
            "Unknown provider",
            "unknown_provider",
            &provider,
        ),
    };

    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", &refresh_token),
        ("client_id", "openproxy"),
    ];

    let resp = match client
        .post(&provider_config.token_url)
        .form(&params)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                &format!("Request failed: {}", e),
                "request_failed",
                &provider,
            );
        }
    };

    let token_response: TokenResponse = match resp.json().await {
        Ok(t) => t,
        Err(_) => {
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "Failed to parse token response",
                "parse_error",
                &provider,
            );
        }
    };

    if let Err(e) = store_connection(&state.db, &account_id, &provider, &token_response, None).await {
        return make_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to store connection: {}", e),
            "storage_error",
            &provider,
        );
    }

    Json(RefreshResponse {
        success: true,
        access_token: token_response.access_token.clone(),
        expires_in: token_response.expires_in.unwrap_or(3600) as u64,
        refresh_token: token_response.refresh_token.or(Some(refresh_token)),
    })
    .into_response()
}

// GET /api/oauth/:provider/status
pub async fn oauth_status(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    headers: axum::http::HeaderMap,
) -> Response {
    let api_key = match require_api_key(&headers, &state.db) {
        Ok(key) => key,
        Err(e) => return crate::server::api::auth_error_response(e),
    };
    let account_id = api_key.id;

    let snapshot = state.db.snapshot();
    let connection = snapshot.provider_connections.iter().find(|conn| {
        conn.provider == provider && conn.id.contains(&account_id)
    });

    match connection {
        Some(conn) => {
            let needs_refresh = crate::oauth::needs_refresh(&conn.expires_at);
            Json(StatusResponse {
                provider: provider.clone(),
                connected: true,
                auth_type: conn.auth_type.clone(),
                expires_at: conn.expires_at.clone(),
                needs_refresh: Some(needs_refresh),
                scope: conn.scope.clone(),
            })
            .into_response()
        }
        None => Json(StatusResponse {
            provider: provider.clone(),
            connected: false,
            auth_type: "oauth".to_string(),
            expires_at: None,
            needs_refresh: None,
            scope: None,
        })
        .into_response(),
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/oauth/{provider}/start", get(start_oauth_flow))
        .route("/api/oauth/{provider}/callback", get(oauth_callback))
        .route("/api/oauth/{provider}/device_code", post(start_device_code))
        .route("/api/oauth/{provider}/poll", post(poll_device_code))
        .route("/api/oauth/{provider}/refresh", post(refresh_token))
        .route("/api/oauth/{provider}/status", get(oauth_status))
}
