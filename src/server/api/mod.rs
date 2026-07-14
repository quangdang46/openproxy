pub mod a2a;
pub mod admin_items;
mod auth;
pub mod chat;
pub mod chat_search;
pub mod cli_tools;
pub mod cloud_credentials;
pub mod cloud_sync;
pub mod compat;
pub mod cors;
pub mod db_backups;
pub mod guard;
pub mod headroom;
pub mod locale;
pub mod mcp;
pub mod mcp_server;
pub mod media;
pub mod media_providers;
pub mod mitm_config;
pub mod models_alias;
pub mod models_availability;
pub mod models_custom;
pub mod models_disabled;
pub mod oauth;
pub mod observability;
pub mod pricing;
mod provider_connection_test;
mod provider_model_tests;
mod provider_models;
pub mod provider_nodes;
mod provider_validate;
pub mod providers;
pub mod quota_auto_ping;
pub mod settings_payload_rules;
pub mod shutdown;
pub mod stt;
pub mod tags;
pub mod translator;
pub mod tunnel;
pub mod usage;
pub mod v1_api_chat;
pub mod v1_models;
pub mod v1beta;
pub mod web_fetch;

use std::collections::BTreeMap;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{
    extract::Path,
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::core::auth::CLI_TOKEN_HEADER;
use crate::server::auth::{extract_api_key, require_api_key, require_dashboard_session, AuthError};
use crate::server::state::AppState;
use crate::types::{AppDb, HealthResponse, ProviderConnection};

/// Header carrying the dashboard password for sensitive re-auth (export/import).
/// Accepts the OpenProxy name and the legacy 9router name for compatibility.
const DB_PASSWORD_HEADERS: &[&str] = &["x-op-password", "x-9r-password"];

pub fn routes(state: AppState) -> Router<AppState> {
    use axum::middleware;

    // ── PUBLIC: no auth required ──
    let public = Router::new()
        .route("/health", get(health))
        .route("/api/health", get(api_health))
        .route("/api/catalog", get(api_catalog))
        .route("/v1", get(v1_root))
        .route("/v1/health", get(health))
        .route("/v1/v1/health", get(health));

    // ── PROTECTED: valid API key required ──
    let protected = Router::new()
        .merge(v1_api_chat::routes())
        .merge(v1_models::routes())
        .nest(
            "/v1/v1/models",
            Router::new()
                .route(
                    "/",
                    get(v1_models::list_default_models).options(v1_models::cors_options),
                )
                .route(
                    "/info",
                    get(v1_models::models_info).options(v1_models::cors_options),
                )
                .route(
                    "/{kind}",
                    get(v1_models::list_models_by_kind).options(v1_models::cors_options),
                ),
        )
        .merge(v1beta::routes())
        .merge(chat_search::routes())
        .merge(web_fetch::routes())
        .route(
            "/v1/chat/completions",
            post(chat::chat_completions).options(chat::cors_options),
        )
        .route(
            "/chat/completions",
            post(chat::chat_completions).options(chat::cors_options),
        )
        .route(
            "/v1/v1/chat/completions",
            post(chat::chat_completions).options(chat::cors_options),
        )
        .route(
            "/v1/messages",
            post(compat::messages).options(compat::cors_options),
        )
        .route(
            "/messages",
            post(compat::messages).options(compat::cors_options),
        )
        .route(
            "/v1/v1/messages",
            post(compat::messages).options(compat::cors_options),
        )
        .route(
            "/v1/messages/count_tokens",
            post(compat::count_tokens).options(compat::cors_options),
        )
        .route(
            "/messages/count_tokens",
            post(compat::count_tokens).options(compat::cors_options),
        )
        .route(
            "/v1/v1/messages/count_tokens",
            post(compat::count_tokens).options(compat::cors_options),
        )
        .route(
            "/v1/responses",
            post(compat::responses).options(compat::cors_options),
        )
        .route(
            "/v1/v1/responses",
            post(compat::responses).options(compat::cors_options),
        )
        .route(
            "/v1/responses/compact",
            post(compat::responses_compact).options(compat::cors_options),
        )
        .route(
            "/v1/v1/responses/compact",
            post(compat::responses_compact).options(compat::cors_options),
        )
        .route(
            "/v1/audio/transcriptions",
            post(stt::audio_transcriptions).options(stt::cors_options),
        )
        .route(
            "/v1/v1/audio/transcriptions",
            post(stt::audio_transcriptions).options(stt::cors_options),
        )
        .route(
            "/v1/audio/speech",
            post(media::audio_speech).options(media::cors_options),
        )
        .route(
            "/v1/v1/audio/speech",
            post(media::audio_speech).options(media::cors_options),
        )
        .route(
            "/v1/embeddings",
            post(media::embeddings).options(media::cors_options),
        )
        .route(
            "/v1/v1/embeddings",
            post(media::embeddings).options(media::cors_options),
        )
        .route(
            "/v1/images/generations",
            post(media::images_generations).options(media::cors_options),
        )
        .route(
            "/v1/v1/images/generations",
            post(media::images_generations).options(media::cors_options),
        )
        .route(
            "/v1/images/edits",
            post(media::images_edits).options(media::cors_options),
        )
        .route(
            "/v1/v1/images/edits",
            post(media::images_edits).options(media::cors_options),
        )
        .route(
            "/v1/video/generations",
            post(media::video_generations).options(media::cors_options),
        )
        .route(
            "/v1/v1/video/generations",
            post(media::video_generations).options(media::cors_options),
        )
        .route(
            "/v1/audio/music",
            post(media::audio_music).options(media::cors_options),
        )
        .route(
            "/v1/v1/audio/music",
            post(media::audio_music).options(media::cors_options),
        )
        .route(
            "/v1/rerank",
            post(media::rerank).options(media::cors_options),
        )
        .route(
            "/v1/v1/rerank",
            post(media::rerank).options(media::cors_options),
        )
        .route(
            "/v1/moderations",
            post(media::moderations).options(media::cors_options),
        )
        .route(
            "/v1/v1/moderations",
            post(media::moderations).options(media::cors_options),
        )
        .route(
            "/v1/search",
            post(media::search).options(media::cors_options),
        )
        .route(
            "/v1/v1/search",
            post(media::search).options(media::cors_options),
        )
        .route(
            "/v1/audio/voices",
            get(media::audio_voices).options(media::cors_options),
        )
        .route(
            "/v1/v1/audio/voices",
            get(media::audio_voices).options(media::cors_options),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            guard::require_protected,
        ));

    // ── ADMIN: dashboard session or management API key required ──
    let admin_local_only = Router::new()
        // Headroom proxy management (local-only)
        .route("/api/headroom/status", get(headroom::status))
        .route("/api/headroom/start", post(headroom::start))
        .route("/api/headroom/stop", post(headroom::stop))
        .route("/api/headroom/restart", post(headroom::restart))
        .route(
            "/api/headroom/extras",
            get(headroom::extras_get)
                .post(headroom::extras_post)
                .delete(headroom::extras_delete),
        )
        .route(
            "/api/headroom/proxy/{*path}",
            get(headroom::proxy_handler)
                .post(headroom::proxy_handler)
                .put(headroom::proxy_handler)
                .patch(headroom::proxy_handler)
                .delete(headroom::proxy_handler)
                .head(headroom::proxy_handler)
                .options(headroom::proxy_handler),
        )
        .route_layer(middleware::from_fn(guard::require_local_only));

    let admin = Router::new()
        // Credential management (admin-tier — dashboard or API key)
        .route("/api/keys", get(list_keys_api))
        .route("/api/keys", post(create_key_api))
        .route("/api/keys/{id}", delete(delete_key_api))
        .route("/api/keys/{id}", put(update_key_api))
        .merge(cli_tools::routes())
        .merge(quota_auto_ping::routes())
        .merge(db_backups::routes())
        .merge(locale::routes())
        .merge(models_disabled::routes())
        .merge(models_alias::routes())
        .merge(models_availability::routes())
        .merge(models_custom::routes())
        .merge(provider_nodes::routes())
        .merge(providers::routes())
        .merge(settings_payload_rules::routes())
        .merge(tunnel::routes())
        .merge(usage::routes())
        .merge(cloud_sync::routes())
        .merge(cloud_credentials::routes())
        .merge(admin_items::routes())
        .merge(pricing::routes())
        .merge(tags::routes())
        .merge(translator::routes())
        .merge(shutdown::routes())
        .merge(oauth::routes())
        .merge(admin_local_only)
        .route(
            "/api/dashboard/chat/completions",
            post(chat::dashboard_chat_completions),
        )
        .route("/api/providers", get(list_providers_api))
        .route("/api/providers", post(create_provider_api))
        .route("/api/nodes", get(list_nodes_api))
        .route("/api/nodes", post(create_node_api))
        .route("/api/combos", get(list_combos_api))
        .route("/api/combos", post(create_combo_api))
        .route(
            "/api/combos/test-model",
            post(provider_model_tests::test_combo_model),
        )
        .route("/api/proxy-pools", get(list_pools_api))
        .route("/api/proxy-pools", post(create_pool_api))
        .route("/api/proxy-pools/{id}", put(update_pool_api))
        .route("/api/proxy-pools/{id}", delete(delete_pool_api))
        .route(
            "/api/settings",
            get(get_settings_api)
                .put(update_settings_api)
                .patch(update_settings_api),
        )
        .route("/api/settings/proxy-test", post(proxy_test_api))
        .route("/api/version", get(get_version_api))
        .route("/api/version/update", post(version_update_api))
        .route(
            "/api/settings/database",
            get(settings_database_export_api).post(settings_database_import_api),
        )
        .route("/api/settings/require-login", get(get_require_login_api))
        .route("/api/db/export", get(export_db_api))
        .route_layer(middleware::from_fn_with_state(state, guard::require_admin));

    // ── Remaining modules: complex/mixed auth managed per-handler ──
    let remaining = Router::new()
        .merge(media_providers::routes())
        .merge(observability::routes())
        .merge(mitm_config::routes())
        .merge(mcp::routes())
        .merge(mcp_server::routes())
        .merge(auth::routes())
        .merge(a2a::routes())
        .merge(provider_validate::routes());

    // ── Assemble ──
    public.merge(protected).merge(admin).merge(remaining)
}

async fn v1_root() -> Response {
    Json(json!({
        "version": "v1",
        "endpoints": [
            "/v1/chat/completions",
            "/v1/messages",
            "/v1/messages/count_tokens",
            "/v1/responses",
            "/v1/responses/compact",
            "/v1/embeddings",
            "/v1/images/generations",
            "/v1/audio/speech",
            "/v1/audio/transcriptions",
            "/v1/search",
            "/v1/web/fetch",
            "/v1/models",
            "/v1/usage",
        ]
    }))
    .into_response()
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse::new("api"))
}

async fn api_health() -> Response {
    Json(json!({ "ok": true })).into_response()
}

async fn api_catalog() -> Response {
    static CATALOG_JSON: &str = include_str!("../../core/model/provider_catalog.json");
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        CATALOG_JSON,
    )
        .into_response()
}

async fn get_version_api() -> Response {
    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let latest_version = fetch_latest_release_version().await;
    let has_update = latest_version
        .as_deref()
        .map(|latest| compare_semver_like(latest, &current_version) > 0)
        .unwrap_or(false);

    Json(json!({
        "currentVersion": current_version,
        "latestVersion": latest_version,
        "hasUpdate": has_update,
        "dashboardVersion": dashboard_package_version(),
    }))
    .into_response()
}

async fn version_update_api() -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "success": false,
            "message": "Self-update is handled by the Rust binary. Use cargo install or download the latest release."
        })),
    )
        .into_response()
}

fn dashboard_package_version() -> &'static str {
    static PACKAGE_JSON: &str = include_str!("../../../web/package.json");
    serde_json::from_str::<Value>(PACKAGE_JSON)
        .ok()
        .and_then(|value| {
            value
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .map(|version| Box::leak(version.into_boxed_str()) as &'static str)
        .unwrap_or(env!("CARGO_PKG_VERSION"))
}

async fn fetch_latest_dashboard_version() -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(4))
        .build()
        .ok()?;

    client
        .get("https://registry.npmjs.org/openproxy/latest")
        .send()
        .await
        .ok()?
        .json::<Value>()
        .await
        .ok()?
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_string)
}

async fn fetch_latest_release_version() -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(4))
        .user_agent(concat!("openproxy/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;

    let body: Value = client
        .get("https://api.github.com/repos/quangdang46/openproxy/releases/latest")
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let tag = body.get("tag_name").and_then(Value::as_str)?;
    Some(tag.trim_start_matches('v').to_string())
}

fn compare_semver_like(a: &str, b: &str) -> i32 {
    let parse = |input: &str| {
        input
            .split('.')
            .take(3)
            .map(|part| part.parse::<u32>().unwrap_or(0))
            .collect::<Vec<_>>()
    };

    let mut a_parts = parse(a);
    let mut b_parts = parse(b);
    while a_parts.len() < 3 {
        a_parts.push(0);
    }
    while b_parts.len() < 3 {
        b_parts.push(0);
    }

    for (left, right) in a_parts.iter().zip(b_parts.iter()) {
        if left > right {
            return 1;
        }
        if left < right {
            return -1;
        }
    }

    0
}

pub(super) fn require_management_api_key(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<(), Response> {
    require_api_key(headers, &state.db)
        .map(|_| ())
        .map_err(auth_error_response)
}

pub(super) fn require_dashboard_or_management_api_key(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<(), Response> {
    if extract_api_key(headers).is_some() {
        return require_management_api_key(headers, state);
    }

    match require_dashboard_session(headers, &state.db) {
        Ok(_) => Ok(()),
        Err(error) => Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": error.message() })),
        )
            .into_response()),
    }
}

/// Extra password re-auth for database export/import (9router parity).
///
/// Skipped when:
/// - the request carries a CLI token (`x-9r-cli-token`) — local CLI is trusted
/// - a valid management API key is present — automation/CLI via Bearer
/// - `requireLogin` is off or no dashboard password is configured
///
/// Otherwise the caller must supply the current dashboard password via
/// `x-op-password` / `x-9r-password` header or the `password` JSON field.
pub(super) fn require_database_password_reauth(
    headers: &HeaderMap,
    state: &AppState,
    body_password: Option<&str>,
) -> Result<(), Response> {
    if headers.get(CLI_TOKEN_HEADER).is_some() {
        return Ok(());
    }

    if extract_api_key(headers).is_some() && require_api_key(headers, &state.db).is_ok() {
        return Ok(());
    }

    let snapshot = state.db.snapshot();
    let settings = &snapshot.settings;
    let has_password = auth::settings_password_hash(settings).is_some();
    if !settings.require_login || !has_password {
        return Ok(());
    }

    let header_password = DB_PASSWORD_HEADERS.iter().find_map(|name| {
        headers
            .get(*name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    });
    let password = body_password
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or(header_password);

    if auth::verify_dashboard_password(password.as_deref(), settings) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "Invalid password" })),
        )
            .into_response())
    }
}

pub(super) fn redact_provider_connection(connection: &ProviderConnection) -> ProviderConnection {
    let mut redacted = connection.clone();
    redacted.access_token = None;
    redacted.refresh_token = None;
    redacted.id_token = None;
    redacted.api_key = None;

    for secret_field in [
        "accessToken",
        "refreshToken",
        "idToken",
        "apiKey",
        "cookie",
        "password",
    ] {
        redacted.provider_specific_data.remove(secret_field);
    }

    redacted
}

pub(crate) fn safe_settings_payload(settings: &crate::types::Settings) -> Value {
    safe_settings_payload_with_db_path(settings, None)
}

pub(crate) fn safe_settings_payload_with_db_path(
    settings: &crate::types::Settings,
    db_path: Option<&str>,
) -> Value {
    let mut value = serde_json::to_value(settings).unwrap_or_else(|_| json!({}));
    if let Some(fields) = value.as_object_mut() {
        fields.remove("password");
        // Never leak the OIDC client secret (also skip_serializing, belt+suspenders).
        fields.remove("oidcClientSecret");
        fields.insert(
            "enableRequestLogs".to_string(),
            Value::Bool(std::env::var("ENABLE_REQUEST_LOGS").ok().as_deref() == Some("true")),
        );
        fields.insert(
            "enableTranslator".to_string(),
            Value::Bool(std::env::var("ENABLE_TRANSLATOR").ok().as_deref() == Some("true")),
        );
        let has_password = settings.password.is_some()
            || settings
                .extra
                .get("password")
                .and_then(|value| value.as_str())
                .is_some_and(|value| !value.is_empty());
        fields.insert("hasPassword".to_string(), Value::Bool(has_password));

        // Settings-driven OIDC first; fall back to env vars for boot-time config.
        let oidc_configured = settings.is_oidc_configured()
            || (std::env::var("OIDC_ISSUER")
                .ok()
                .is_some_and(|v| !v.is_empty())
                && std::env::var("OIDC_CLIENT_ID")
                    .ok()
                    .is_some_and(|v| !v.is_empty())
                && std::env::var("OIDC_CLIENT_SECRET")
                    .ok()
                    .is_some_and(|v| !v.is_empty()));
        fields.insert("oidcConfigured".to_string(), Value::Bool(oidc_configured));

        // Prefer the first-class auth_mode field; fall back to legacy extra.authMode /
        // oidc_enabled for older DB payloads.
        let auth_mode = {
            let mode = settings.auth_mode.trim();
            if matches!(mode, "password" | "oidc" | "both") {
                mode.to_string()
            } else {
                settings
                    .extra
                    .get("authMode")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| matches!(*value, "password" | "oidc" | "both"))
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| {
                        if settings.oidc_enabled {
                            "both".to_string()
                        } else {
                            "password".to_string()
                        }
                    })
            }
        };
        fields.insert("authMode".to_string(), Value::String(auth_mode));

        let oidc_login_label = {
            let label = settings.oidc_login_label.trim();
            if !label.is_empty() {
                label.to_string()
            } else {
                settings
                    .extra
                    .get("oidcLoginLabel")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|value| value.to_string())
                    .or_else(|| {
                        std::env::var("OIDC_LOGIN_LABEL")
                            .ok()
                            .map(|value| value.trim().to_string())
                            .filter(|value| !value.is_empty())
                    })
                    .unwrap_or_else(|| "Sign in with OIDC".to_string())
            }
        };
        fields.insert(
            "oidcLoginLabel".to_string(),
            Value::String(oidc_login_label),
        );

        if let Some(path) = db_path {
            fields.insert("databasePath".to_string(), Value::String(path.to_string()));
        }
    }

    value
}

// Provider CRUD API
async fn list_providers_api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let connections: Vec<Value> = snapshot
        .provider_connections
        .iter()
        .map(|c| {
            let has_api_key = c.api_key.as_deref().is_some_and(|k| !k.is_empty())
                || c.provider_specific_data
                    .get("apiKey")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty());
            let mut value =
                serde_json::to_value(redact_provider_connection(c)).unwrap_or_else(|_| json!({}));
            if let Some(obj) = value.as_object_mut() {
                obj.insert("hasApiKey".into(), Value::Bool(has_api_key));
            }
            value
        })
        .collect();
    Json(json!({ "connections": connections })).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateProviderRequest {
    provider: String,
    name: Option<String>,
    api_key: Option<String>,
    priority: Option<u32>,
    global_priority: Option<u32>,
    default_model: Option<String>,
    test_status: Option<String>,
    provider_specific_data: Option<serde_json::Map<String, Value>>,
    connection_proxy_enabled: Option<bool>,
    connection_proxy_url: Option<String>,
    connection_no_proxy: Option<String>,
    proxy_pool_id: Option<Value>,
    base_url: Option<String>,
}

async fn create_provider_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateProviderRequest>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let provider = req.provider.trim();
    if provider.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "Provider is required" })),
        )
            .into_response();
    }

    let Some(name) = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "Name is required" })),
        )
            .into_response();
    };

    let Some(api_key) = req
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "API key is required" })),
        )
            .into_response();
    };

    let (connection_proxy_enabled, connection_proxy_url, connection_no_proxy) =
        match normalize_create_provider_proxy(&req) {
            Ok(proxy) => proxy,
            Err(response) => return response,
        };
    let proxy_pool_id = match normalize_create_provider_proxy_pool(
        &state.db.snapshot().proxy_pools,
        req.proxy_pool_id.as_ref(),
    ) {
        Ok(proxy_pool_id) => proxy_pool_id,
        Err(message) => return bad_request_response(&message),
    };

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let mut default_conn = ProviderConnection::default();
    default_conn.id = id;
    default_conn.provider = provider.to_string();
    // Web-cookie providers store a browser session cookie in the api_key field
    // but must retain auth_type "cookie" (9router parity) so list filters,
    // toggles, and executor dispatch treat them correctly.
    default_conn.auth_type = if is_web_cookie_provider(provider) {
        "cookie".to_string()
    } else {
        "apikey".to_string()
    };
    default_conn.name = Some(name);
    default_conn.priority = Some(req.priority.unwrap_or(1));
    default_conn.is_active = Some(true);
    default_conn.created_at = Some(now.clone());
    default_conn.updated_at = Some(now);
    default_conn.global_priority = req.global_priority;
    default_conn.default_model = req.default_model.filter(|value| !value.trim().is_empty());
    default_conn.api_key = Some(api_key);
    default_conn.test_status = Some(
        req.test_status
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "unknown".to_string()),
    );
    if let Some(provider_specific_data) = req.provider_specific_data {
        default_conn.provider_specific_data = provider_specific_data.into_iter().collect();
    }
    if let Some(base_url) = req
        .base_url
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        default_conn
            .provider_specific_data
            .insert("baseUrl".to_string(), Value::String(base_url));
    }
    if let Some(enabled) = connection_proxy_enabled {
        default_conn
            .provider_specific_data
            .insert("connectionProxyEnabled".to_string(), Value::Bool(enabled));
    }
    if let Some(url) = connection_proxy_url {
        default_conn
            .provider_specific_data
            .insert("connectionProxyUrl".to_string(), Value::String(url));
    }
    if let Some(no_proxy) = connection_no_proxy {
        default_conn
            .provider_specific_data
            .insert("connectionNoProxy".to_string(), Value::String(no_proxy));
    }
    if let Some(proxy_pool_id) = proxy_pool_id {
        default_conn
            .provider_specific_data
            .insert("proxyPoolId".to_string(), Value::String(proxy_pool_id));
    }

    let result = state
        .db
        .update(|db| {
            db.provider_connections.push(default_conn.clone());
        })
        .await;

    match result {
        Ok(_) => (
            StatusCode::CREATED,
            Json(json!({
                "success": true,
                "connection": redact_provider_connection(&default_conn)
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

// Rest of the module (unchanged below this line)
// Provider CRUD - GET, PUT, DELETE by id
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateProviderRequest {
    name: Option<String>,
    api_key: Option<String>,
    base_url: Option<String>,
    is_active: Option<bool>,
}

async fn get_provider_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }
    let snapshot = state.db.snapshot();
    let connections = snapshot.provider_connections.clone();
    let found = connections
        .iter()
        .find(|c| c.id == id || c.name.as_deref().map(|n| n == id).unwrap_or(false))
        .cloned();
    match found {
        Some(conn) => Json(json!({ "provider": conn })).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Provider not found" })),
        )
            .into_response(),
    }
}

async fn update_provider_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdateProviderRequest>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }
    let snapshot = state.db.snapshot();
    let exists = snapshot
        .provider_connections
        .iter()
        .any(|c| c.id == id || c.name.as_deref().map(|n| n == id).unwrap_or(false));
    if !exists {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Provider not found" })),
        )
            .into_response();
    }
    let result = state
        .db
        .update(|db| {
            if let Some(conn) = db
                .provider_connections
                .iter_mut()
                .find(|c| c.id == id || c.name.as_deref().map(|n| n == id).unwrap_or(false))
            {
                if let Some(name) = req.name {
                    conn.name = Some(name);
                }
                if let Some(api_key) = req.api_key {
                    conn.api_key = Some(api_key);
                }
                if let Some(base_url) = req.base_url {
                    conn.provider_specific_data
                        .insert("baseUrl".to_string(), Value::String(base_url));
                }
                if let Some(is_active) = req.is_active {
                    conn.is_active = Some(is_active);
                }
                conn.updated_at = Some(chrono::Utc::now().to_rfc3339());
            }
        })
        .await;
    match result {
        Ok(_) => {
            let snapshot = state.db.snapshot();
            match snapshot
                .provider_connections
                .iter()
                .find(|c| c.id == id || c.name.as_deref().map(|n| n == id).unwrap_or(false))
            {
                Some(conn) => Json(json!({ "provider": conn })).into_response(),
                None => (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": "Provider not found after update" })),
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

async fn delete_provider_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }
    let snapshot = state.db.snapshot();
    let exists = snapshot
        .provider_connections
        .iter()
        .any(|c| c.id == id || c.name.as_deref().map(|n| n == id).unwrap_or(false));
    if !exists {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Provider not found" })),
        )
            .into_response();
    }
    let result = state
        .db
        .update(|db| {
            db.provider_connections
                .retain(|c| c.id != id && !c.name.as_deref().map(|n| n == id).unwrap_or(false));
        })
        .await;
    match result {
        Ok(_) => Json(json!({ "success": true, "message": "Provider deleted" })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// Node CRUD API
async fn list_nodes_api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    Json(json!({ "nodes": snapshot.provider_nodes.clone() })).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateNodeRequest {
    node_type: String,
    name: String,
    base_url: Option<String>,
    api_type: Option<String>,
}

async fn create_node_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateNodeRequest>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    use crate::types::ProviderNode;

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let node = ProviderNode {
        id,
        r#type: req.node_type,
        name: req.name,
        prefix: None,
        base_url: req.base_url,
        api_type: req.api_type,
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

// Combo CRUD API
async fn list_combos_api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    Json(json!({ "combos": snapshot.combos.clone() })).into_response()
}

#[derive(Debug, Deserialize)]
struct CreateComboRequest {
    name: Option<String>,
    #[serde(default)]
    models: Vec<String>,
    kind: Option<String>,
}

async fn create_combo_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateComboRequest>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    use crate::types::Combo;

    let Some(name) = req.name.as_deref() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Name is required" })),
        )
            .into_response();
    };

    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Name is required" })),
        )
            .into_response();
    }

    if !admin_items::valid_combo_name(name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Name can only contain letters, numbers, -, _ and ." })),
        )
            .into_response();
    }

    if state
        .db
        .snapshot()
        .combos
        .iter()
        .any(|combo| combo.name == name)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Combo name already exists" })),
        )
            .into_response();
    }

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let combo = Combo {
        id,
        name: name.to_string(),
        models: req.models,
        disabled_models: Vec::new(),
        kind: req.kind.filter(|kind| !kind.is_empty()),
        created_at: Some(now.clone()),
        updated_at: Some(now),
        extra: std::collections::BTreeMap::new(),
    };

    let result = state
        .db
        .update(|db| {
            db.combos.push(combo.clone());
        })
        .await;

    match result {
        Ok(_) => (StatusCode::CREATED, Json(combo)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

// PUT /api/combos/{name} - Update combo
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateComboRequest {
    #[serde(default)]
    models: Option<Vec<String>>,
    kind: Option<String>,
}

async fn update_combo_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(req): Json<UpdateComboRequest>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let combo_exists = snapshot.combos.iter().any(|c| c.name == name);

    if !combo_exists {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Combo not found" })),
        )
            .into_response();
    }

    let result = state
        .db
        .update(|db| {
            if let Some(combo) = db.combos.iter_mut().find(|c| c.name == name) {
                if let Some(models) = req.models {
                    combo.models = models;
                }
                if let Some(kind) = req.kind {
                    combo.kind = Some(kind);
                }
                combo.updated_at = Some(chrono::Utc::now().to_rfc3339());
            }
        })
        .await;

    match result {
        Ok(_) => {
            let snapshot = state.db.snapshot();
            match snapshot.combos.iter().find(|c| c.name == name) {
                Some(combo) => (StatusCode::OK, Json(combo.clone())).into_response(),
                None => (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": "Combo not found after update" })),
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

// DELETE /api/combos/{name} - Delete combo
async fn delete_combo_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let combo_exists = snapshot.combos.iter().any(|c| c.name == name);

    if !combo_exists {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Combo not found" })),
        )
            .into_response();
    }

    let result = state
        .db
        .update(|db| {
            db.combos.retain(|c| c.name != name);
        })
        .await;

    match result {
        Ok(_) => Json(json!({ "success": true, "message": "Combo deleted" })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// API Key CRUD API
async fn list_keys_api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    Json(json!({ "keys": snapshot.api_keys.clone() })).into_response()
}

#[derive(Debug, Deserialize)]
struct CreateKeyRequest {
    name: Option<String>,
}

async fn create_key_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateKeyRequest>,
) -> Response {
    if !state.db.snapshot().api_keys.is_empty() {
        if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
            return response;
        }
    }

    use crate::types::ApiKey;

    let Some(name) = req.name.as_deref() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Name is required" })),
        )
            .into_response();
    };

    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Name is required" })),
        )
            .into_response();
    }

    let id = Uuid::new_v4().to_string();
    let machine_id = consistent_machine_id();
    let key = crate::core::auth::generate_api_key_with_machine(&machine_id);
    let now = chrono::Utc::now().to_rfc3339();

    let api_key = ApiKey {
        id,
        name: name.to_string(),
        key,
        machine_id: Some(machine_id),
        is_active: Some(true),
        created_at: Some(now),
        extra: std::collections::BTreeMap::new(),
    };

    let result = state
        .db
        .update(|db| {
            db.api_keys.push(api_key.clone());
        })
        .await;

    match result {
        Ok(_) => (
            StatusCode::CREATED,
            Json(json!({
                "key": api_key.key,
                "name": api_key.name,
                "id": api_key.id,
                "machineId": api_key.machine_id,
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

// API Key CRUD - PUT + DELETE
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateKeyRequest {
    name: Option<String>,
    is_active: Option<bool>,
}

async fn update_key_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdateKeyRequest>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let exists = snapshot.api_keys.iter().any(|k| k.id == id);

    if !exists {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "API key not found" })),
        )
            .into_response();
    }

    let result = state
        .db
        .update(|db| {
            if let Some(key) = db.api_keys.iter_mut().find(|k| k.id == id) {
                if let Some(name) = req.name {
                    key.name = name;
                }
                if let Some(is_active) = req.is_active {
                    key.is_active = Some(is_active);
                }
            }
        })
        .await;

    match result {
        Ok(_) => {
            let snapshot = state.db.snapshot();
            match snapshot.api_keys.iter().find(|k| k.id == id) {
                Some(key) => {
                    let mut response_key = key.clone();
                    response_key.key = "***".to_string();
                    Json(json!({ "key": response_key })).into_response()
                }
                None => (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": "Key not found after update" })),
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

async fn delete_key_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let exists = snapshot.api_keys.iter().any(|k| k.id == id);

    if !exists {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "API key not found" })),
        )
            .into_response();
    }

    let result = state
        .db
        .update(|db| {
            db.api_keys.retain(|k| k.id != id);
        })
        .await;

    match result {
        Ok(_) => Json(json!({ "success": true, "message": "API key deleted" })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub fn consistent_machine_id() -> String {
    let salt =
        std::env::var("MACHINE_ID_SALT").unwrap_or_else(|_| "endpoint-proxy-salt".to_string());

    match raw_machine_id() {
        Some(raw_machine_id) => {
            use sha2::Digest;

            let mut hasher = sha2::Sha256::new();
            hasher.update(raw_machine_id.as_bytes());
            hasher.update(salt.as_bytes());
            hex::encode(hasher.finalize())[..16].to_string()
        }
        None => Uuid::new_v4().to_string(),
    }
}

fn raw_machine_id() -> Option<String> {
    ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_bool_query(value: Option<&str>) -> Option<bool> {
    match value {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    }
}

// Proxy Pool CRUD API
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListPoolsQuery {
    is_active: Option<String>,
    include_usage: Option<String>,
}

async fn list_pools_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListPoolsQuery>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let mut proxy_pools = snapshot.proxy_pools.clone();

    if let Some(is_active) = parse_bool_query(query.is_active.as_deref()) {
        proxy_pools.retain(|proxy_pool| proxy_pool.is_active == Some(is_active));
    }

    proxy_pools.sort_by(|a, b| {
        b.updated_at
            .as_deref()
            .unwrap_or("")
            .cmp(a.updated_at.as_deref().unwrap_or(""))
    });

    if parse_bool_query(query.include_usage.as_deref()) == Some(true) {
        let usage_map = snapshot.provider_connections.iter().fold(
            std::collections::BTreeMap::<String, u64>::new(),
            |mut map, connection| {
                if let Some(proxy_pool_id) = connection
                    .provider_specific_data
                    .get("proxyPoolId")
                    .and_then(Value::as_str)
                {
                    *map.entry(proxy_pool_id.to_string()).or_insert(0) += 1;
                }
                map
            },
        );

        let proxy_pools = proxy_pools
            .into_iter()
            .map(|proxy_pool| {
                let mut value = serde_json::to_value(&proxy_pool).unwrap_or_else(|_| json!({}));
                if let Some(object) = value.as_object_mut() {
                    object.insert(
                        "boundConnectionCount".to_string(),
                        json!(usage_map.get(&proxy_pool.id).copied().unwrap_or(0)),
                    );
                }
                value
            })
            .collect::<Vec<_>>();

        return Json(json!({ "proxyPools": proxy_pools })).into_response();
    }

    Json(json!({ "proxyPools": proxy_pools })).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatePoolRequest {
    name: Option<String>,
    proxy_url: Option<String>,
    no_proxy: Option<String>,
    is_active: Option<bool>,
    strict_proxy: Option<bool>,
    r#type: Option<String>,
}

async fn create_pool_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreatePoolRequest>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    use crate::types::ProxyPool;

    let Some(name) = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Name is required" })),
        )
            .into_response();
    };

    let Some(proxy_url) = req
        .proxy_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Proxy URL is required" })),
        )
            .into_response();
    };

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let mut pool = ProxyPool::default();
    pool.id = id;
    pool.name = name.to_string();
    pool.proxy_url = proxy_url.to_string();
    pool.no_proxy = req.no_proxy.unwrap_or_default().trim().to_string();
    pool.r#type = match req.r#type.as_deref() {
        Some("http" | "vercel" | "cloudflare" | "deno") => req.r#type.unwrap(),
        _ => "http".to_string(),
    };
    pool.is_active = Some(req.is_active.unwrap_or(true));
    pool.strict_proxy = Some(req.strict_proxy.unwrap_or(false));
    pool.test_status = Some("unknown".to_string());
    pool.last_tested_at = None;
    pool.last_error = None;
    pool.created_at = Some(now.clone());
    pool.updated_at = Some(now);

    let result = state
        .db
        .update(|db| {
            db.proxy_pools.push(pool.clone());
        })
        .await;

    match result {
        Ok(_) => (StatusCode::CREATED, Json(json!({ "proxyPool": pool }))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

// Proxy Pool CRUD - PUT + DELETE
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdatePoolRequest {
    name: Option<String>,
    proxy_url: Option<String>,
    no_proxy: Option<String>,
    is_active: Option<bool>,
    strict_proxy: Option<bool>,
    r#type: Option<String>,
}

async fn update_pool_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdatePoolRequest>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let exists = snapshot.proxy_pools.iter().any(|p| p.id == id);

    if !exists {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Proxy pool not found" })),
        )
            .into_response();
    }

    let result = state
        .db
        .update(|db| {
            if let Some(pool) = db.proxy_pools.iter_mut().find(|p| p.id == id) {
                if let Some(name) = req.name {
                    pool.name = name;
                }
                if let Some(proxy_url) = req.proxy_url {
                    pool.proxy_url = proxy_url;
                }
                if let Some(no_proxy) = req.no_proxy {
                    pool.no_proxy = no_proxy;
                }
                if let Some(is_active) = req.is_active {
                    pool.is_active = Some(is_active);
                }
                if let Some(strict_proxy) = req.strict_proxy {
                    pool.strict_proxy = Some(strict_proxy);
                }
                if let Some(r#type) = req.r#type {
                    pool.r#type = r#type;
                }
                pool.updated_at = Some(chrono::Utc::now().to_rfc3339());
            }
        })
        .await;

    match result {
        Ok(_) => {
            let snapshot = state.db.snapshot();
            match snapshot.proxy_pools.iter().find(|p| p.id == id) {
                Some(pool) => Json(json!({ "proxyPool": pool })).into_response(),
                None => (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": "Pool not found after update" })),
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

async fn delete_pool_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let exists = snapshot.proxy_pools.iter().any(|p| p.id == id);

    if !exists {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Proxy pool not found" })),
        )
            .into_response();
    }

    let bound_connection_count = snapshot
        .provider_connections
        .iter()
        .filter(|connection| {
            connection
                .provider_specific_data
                .get("proxyPoolId")
                .and_then(Value::as_str)
                .is_some_and(|proxy_pool_id| proxy_pool_id == id)
        })
        .count();

    if bound_connection_count > 0 {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "Proxy pool is currently in use",
                "boundConnectionCount": bound_connection_count
            })),
        )
            .into_response();
    }

    let result = state
        .db
        .update(|db| {
            db.proxy_pools.retain(|p| p.id != id);
        })
        .await;

    match result {
        Ok(_) => Json(json!({ "success": true, "message": "Proxy pool deleted" })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// Settings API
async fn get_settings_api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let db_path = state.db.data_dir.join("openproxy.sqlite");
    let db_path_str = db_path.display().to_string();
    Json(safe_settings_payload_with_db_path(
        &snapshot.settings,
        Some(&db_path_str),
    ))
    .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSettingsRequest {
    tunnel_provider: Option<String>,
    sticky_round_robin_limit: Option<u32>,
    provider_strategies: Option<BTreeMap<String, String>>,
    combo_strategy: Option<String>,
    combo_strategies: Option<BTreeMap<String, crate::types::ComboStrategyEntry>>,
    mitm_router_base_url: Option<String>,
    require_login: Option<bool>,
    rtk_enabled: Option<bool>,
    caveman_enabled: Option<bool>,
    caveman_level: Option<String>,
    observability_enabled: Option<bool>,
    cloud_enabled: Option<bool>,
    cloud_url: Option<String>,
    tunnel_enabled: Option<bool>,
    tunnel_url: Option<String>,
    tailscale_enabled: Option<bool>,
    tailscale_url: Option<String>,
    tunnel_dashboard_access: Option<bool>,
    outbound_proxy_enabled: Option<bool>,
    outbound_proxy_url: Option<String>,
    outbound_no_proxy: Option<String>,
    new_password: Option<String>,
    current_password: Option<String>,
    oidc_enabled: Option<bool>,
    fallback_strategy: Option<String>,
    combo_sticky_round_robin_limit: Option<u32>,
    auth_mode: Option<String>,
    oidc_issuer_url: Option<String>,
    oidc_client_id: Option<String>,
    oidc_client_secret: Option<String>,
    oidc_scopes: Option<String>,
    oidc_login_label: Option<String>,
    client_ping_url: Option<String>,
    client_ping_any: Option<bool>,
    headroom_enabled: Option<bool>,
    headroom_url: Option<String>,
    headroom_code_aware: Option<bool>,
    headroom_kompress: Option<bool>,
    ponytail_enabled: Option<bool>,
    ponytail_level: Option<String>,
    /// Stored in settings.extra so provider-detail UI can PATCH it.
    claude_auto_ping: Option<Value>,
    /// Stored in settings.extra so provider-detail UI can PATCH it.
    codex_auto_ping: Option<Value>,
    /// Per-provider thinking mode map stored in settings.extra.
    provider_thinking: Option<Value>,
}

async fn update_settings_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpdateSettingsRequest>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    if req.new_password.is_some() || req.current_password.is_some() {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({
                "error": "Password changes must use a dedicated endpoint, not PATCH /api/settings"
            })),
        )
            .into_response();
    }

    // Validate OIDC enablement before writing: non-password auth modes need a
    // fully configured IdP (issuer + client id + secret, either already stored
    // or supplied in this request).
    {
        let snapshot = state.db.snapshot();
        let next_mode = req
            .auth_mode
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(snapshot.settings.auth_mode.as_str());
        if matches!(next_mode, "oidc" | "both") {
            let issuer = req
                .oidc_issuer_url
                .as_deref()
                .unwrap_or(snapshot.settings.oidc_issuer_url.as_str())
                .trim();
            let client_id = req
                .oidc_client_id
                .as_deref()
                .unwrap_or(snapshot.settings.oidc_client_id.as_str())
                .trim();
            let secret = req
                .oidc_client_secret
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(snapshot.settings.oidc_client_secret.as_str());
            if issuer.is_empty() || client_id.is_empty() || secret.is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": "Issuer URL, client ID, and client secret are required to enable OIDC."
                    })),
                )
                    .into_response();
            }
        }
    }

    let oidc_touched = req.auth_mode.is_some()
        || req.oidc_issuer_url.is_some()
        || req.oidc_client_id.is_some()
        || req.oidc_client_secret.is_some()
        || req.oidc_scopes.is_some()
        || req.oidc_enabled.is_some();
    // Capture before move so the oidc_enabled legacy path can still decide.
    let auth_mode_was_set = req.auth_mode.is_some();

    let result = state
        .db
        .update(|db| {
            if let Some(v) = req.tunnel_provider {
                db.settings.tunnel_provider = v;
            }
            if let Some(v) = req.sticky_round_robin_limit {
                db.settings.sticky_round_robin_limit = v.max(1);
            }
            if let Some(v) = req.provider_strategies {
                db.settings.provider_strategies = v;
            }
            if let Some(v) = req.combo_strategy {
                db.settings.combo_strategy = v;
            }
            if let Some(v) = req.combo_strategies {
                db.settings.combo_strategies = v;
            }
            if let Some(v) = req.mitm_router_base_url {
                db.settings.mitm_router_base_url = v;
            }
            if let Some(v) = req.require_login {
                db.settings.require_login = v;
            }
            if let Some(v) = req.rtk_enabled {
                db.settings.rtk_enabled = v;
            }
            if let Some(v) = req.caveman_enabled {
                db.settings.caveman_enabled = v;
            }
            if let Some(v) = req.caveman_level {
                db.settings.caveman_level = v;
            }
            if let Some(v) = req.observability_enabled {
                db.settings.observability_enabled = v;
            }
            if let Some(v) = req.cloud_enabled {
                db.settings.cloud_enabled = v;
            }
            if let Some(v) = req.cloud_url {
                db.settings.cloud_url = v;
            }
            if let Some(v) = req.tunnel_enabled {
                db.settings.tunnel_enabled = v;
            }
            if let Some(v) = req.tunnel_url {
                db.settings.tunnel_url = v;
            }
            if let Some(v) = req.tailscale_enabled {
                db.settings.tailscale_enabled = v;
            }
            if let Some(v) = req.tailscale_url {
                db.settings.tailscale_url = v;
            }
            if let Some(v) = req.tunnel_dashboard_access {
                db.settings.tunnel_dashboard_access = v;
            }
            if let Some(v) = req.outbound_proxy_enabled {
                db.settings.outbound_proxy_enabled = v;
            }
            if let Some(v) = req.outbound_proxy_url {
                db.settings.outbound_proxy_url = v;
            }
            if let Some(v) = req.outbound_no_proxy {
                db.settings.outbound_no_proxy = v;
            }
            if let Some(v) = req.fallback_strategy {
                db.settings.fallback_strategy = v;
            }
            if let Some(v) = req.combo_sticky_round_robin_limit {
                db.settings.combo_sticky_round_robin_limit = v.max(1);
            }
            if let Some(v) = req.auth_mode {
                db.settings.auth_mode = v;
            }
            if let Some(v) = req.oidc_issuer_url {
                db.settings.oidc_issuer_url = v;
            }
            if let Some(v) = req.oidc_client_id {
                db.settings.oidc_client_id = v;
            }
            // Write-only: empty/blank secret means "keep existing".
            if let Some(v) = req.oidc_client_secret {
                let trimmed = v.trim().to_string();
                if !trimmed.is_empty() {
                    db.settings.oidc_client_secret = trimmed;
                }
            }
            if let Some(v) = req.oidc_scopes {
                db.settings.oidc_scopes = v;
            }
            if let Some(v) = req.oidc_login_label {
                db.settings.oidc_login_label = v;
            }
            if let Some(v) = req.oidc_enabled {
                // Legacy flag — map onto auth_mode when auth_mode itself was not set.
                db.settings.oidc_enabled = v;
                if !auth_mode_was_set {
                    db.settings.auth_mode = if v { "both".into() } else { "password".into() };
                }
            }
            if let Some(v) = req.client_ping_url {
                db.settings.client_ping_url = v;
            }
            if let Some(v) = req.client_ping_any {
                db.settings.client_ping_any = v;
            }
            if let Some(v) = req.headroom_enabled {
                db.settings.headroom_enabled = v;
            }
            if let Some(v) = req.headroom_url {
                db.settings.headroom_url = v;
            }
            if let Some(v) = req.headroom_code_aware {
                db.settings.headroom_code_aware = v;
            }
            if let Some(v) = req.headroom_kompress {
                db.settings.headroom_kompress = v;
            }
            if let Some(v) = req.ponytail_enabled {
                db.settings.ponytail_enabled = v;
            }
            if let Some(v) = req.ponytail_level {
                db.settings.ponytail_level = v;
            }
            // Persist auto-ping + thinking maps into settings.extra (camelCase
            // keys match the web UI PATCH body).
            if let Some(v) = req.claude_auto_ping {
                db.settings.extra.insert("claudeAutoPing".into(), v);
            }
            if let Some(v) = req.codex_auto_ping {
                db.settings.extra.insert("codexAutoPing".into(), v);
            }
            if let Some(v) = req.provider_thinking {
                db.settings.extra.insert("providerThinking".into(), v);
            }
            db.settings.normalize();
        })
        .await;

    match result {
        Ok(snapshot) => {
            if oidc_touched {
                // Best-effort reload; discovery failure leaves the previous client.
                state.reload_oidc_from_settings().await;
            }
            let db_path = state.db.data_dir.join("openproxy.sqlite");
            let db_path_str = db_path.display().to_string();
            Json(safe_settings_payload_with_db_path(
                &snapshot.settings,
                Some(&db_path_str),
            ))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn export_db_api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_management_api_key(&headers, &state) {
        return response;
    }
    if let Err(response) = require_database_password_reauth(&headers, &state, None) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let val = serde_json::to_value(snapshot.as_ref()).unwrap_or(json!({}));
    Json(val).into_response()
}

async fn settings_database_export_api(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    // Dashboard path: accept session OR management key, then password re-auth.
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }
    if let Err(response) = require_database_password_reauth(&headers, &state, None) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let val = serde_json::to_value(snapshot.as_ref()).unwrap_or(json!({}));
    Json(val).into_response()
}

async fn settings_database_import_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let Json(mut body) = match body {
        Ok(body) => body,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid database payload" })),
            )
                .into_response()
        }
    };

    if !body.is_object() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid database payload" })),
        )
            .into_response();
    }

    // Optional password field for re-auth (stripped before import).
    let body_password = body
        .as_object_mut()
        .and_then(|obj| obj.remove("password"))
        .and_then(|v| v.as_str().map(|s| s.to_string()));
    if let Err(response) =
        require_database_password_reauth(&headers, &state, body_password.as_deref())
    {
        return response;
    }

    let imported = AppDb::from_json_value(body);

    match state
        .db
        .update(move |db| {
            if !imported.provider_connections.is_empty() {
                db.provider_connections = imported.provider_connections.clone();
            }
            if !imported.provider_nodes.is_empty() {
                db.provider_nodes = imported.provider_nodes.clone();
            }
            if !imported.api_keys.is_empty() {
                db.api_keys = imported.api_keys.clone();
            }
            if !imported.combos.is_empty() {
                db.combos = imported.combos.clone();
            }
            if !imported.proxy_pools.is_empty() {
                db.proxy_pools = imported.proxy_pools.clone();
            }
            if !imported.custom_models.is_empty() {
                db.custom_models = imported.custom_models.clone();
            }
            if !imported.model_aliases.is_empty() {
                db.model_aliases = imported.model_aliases.clone();
            }
            if !imported.pricing.is_empty() {
                db.pricing = imported.pricing.clone();
            }
            merge_settings(&mut db.settings, &imported.settings);
        })
        .await
    {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

fn merge_settings(target: &mut crate::types::Settings, source: &crate::types::Settings) {
    if source.cloud_enabled != target.cloud_enabled {
        target.cloud_enabled = source.cloud_enabled;
    }
    if source.cloud_url != target.cloud_url {
        target.cloud_url = source.cloud_url.clone();
    }
    if source.tunnel_enabled != target.tunnel_enabled {
        target.tunnel_enabled = source.tunnel_enabled;
    }
    if source.tunnel_url != target.tunnel_url {
        target.tunnel_url = source.tunnel_url.clone();
    }
    if source.tunnel_provider != target.tunnel_provider {
        target.tunnel_provider = source.tunnel_provider.clone();
    }
    if source.tailscale_enabled != target.tailscale_enabled {
        target.tailscale_enabled = source.tailscale_enabled;
    }
    if source.tailscale_url != target.tailscale_url {
        target.tailscale_url = source.tailscale_url.clone();
    }
    if source.require_login != target.require_login {
        target.require_login = source.require_login;
    }
    if source.tunnel_dashboard_access != target.tunnel_dashboard_access {
        target.tunnel_dashboard_access = source.tunnel_dashboard_access;
    }
    if source.provider_strategies != target.provider_strategies {
        target.provider_strategies = source.provider_strategies.clone();
    }
    if source.combo_strategy != target.combo_strategy {
        target.combo_strategy = source.combo_strategy.clone();
    }
    if source.combo_strategies != target.combo_strategies {
        target.combo_strategies = source.combo_strategies.clone();
    }
    if source.observability_enabled != target.observability_enabled {
        target.observability_enabled = source.observability_enabled;
    }
    if source.outbound_proxy_enabled != target.outbound_proxy_enabled {
        target.outbound_proxy_enabled = source.outbound_proxy_enabled;
    }
    if source.outbound_proxy_url != target.outbound_proxy_url {
        target.outbound_proxy_url = source.outbound_proxy_url.clone();
    }
    if source.outbound_no_proxy != target.outbound_no_proxy {
        target.outbound_no_proxy = source.outbound_no_proxy.clone();
    }
    if source.rtk_enabled != target.rtk_enabled {
        target.rtk_enabled = source.rtk_enabled;
    }
    if source.caveman_enabled != target.caveman_enabled {
        target.caveman_enabled = source.caveman_enabled;
    }
    if source.caveman_level != target.caveman_level {
        target.caveman_level = source.caveman_level.clone();
    }
    if source.sticky_round_robin_limit != target.sticky_round_robin_limit {
        target.sticky_round_robin_limit = source.sticky_round_robin_limit;
    }
    if source.fallback_strategy != target.fallback_strategy {
        target.fallback_strategy = source.fallback_strategy.clone();
    }
    if source.combo_sticky_round_robin_limit != target.combo_sticky_round_robin_limit {
        target.combo_sticky_round_robin_limit = source.combo_sticky_round_robin_limit;
    }
    if source.auth_mode != target.auth_mode {
        target.auth_mode = source.auth_mode.clone();
    }
    if source.oidc_issuer_url != target.oidc_issuer_url {
        target.oidc_issuer_url = source.oidc_issuer_url.clone();
    }
    if source.oidc_client_id != target.oidc_client_id {
        target.oidc_client_id = source.oidc_client_id.clone();
    }
    if !source.oidc_client_secret.is_empty()
        && source.oidc_client_secret != target.oidc_client_secret
    {
        target.oidc_client_secret = source.oidc_client_secret.clone();
    }
    if source.oidc_scopes != target.oidc_scopes {
        target.oidc_scopes = source.oidc_scopes.clone();
    }
    if source.oidc_login_label != target.oidc_login_label {
        target.oidc_login_label = source.oidc_login_label.clone();
    }
    if source.oidc_enabled != target.oidc_enabled {
        target.oidc_enabled = source.oidc_enabled;
    }
    if source.client_ping_url != target.client_ping_url {
        target.client_ping_url = source.client_ping_url.clone();
    }
    if source.client_ping_any != target.client_ping_any {
        target.client_ping_any = source.client_ping_any;
    }
    if source.headroom_enabled != target.headroom_enabled {
        target.headroom_enabled = source.headroom_enabled;
    }
    if source.headroom_url != target.headroom_url {
        target.headroom_url = source.headroom_url.clone();
    }
    if source.headroom_code_aware != target.headroom_code_aware {
        target.headroom_code_aware = source.headroom_code_aware;
    }
    if source.headroom_kompress != target.headroom_kompress {
        target.headroom_kompress = source.headroom_kompress;
    }
    if source.ponytail_enabled != target.ponytail_enabled {
        target.ponytail_enabled = source.ponytail_enabled;
    }
    if source.ponytail_level != target.ponytail_level {
        target.ponytail_level = source.ponytail_level.clone();
    }
    for (key, value) in &source.extra {
        target.extra.insert(key.clone(), value.clone());
    }
    target.normalize();
}

async fn get_require_login_api(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    Json(json!({
        "requireLogin": snapshot.settings.require_login,
        "tunnelDashboardAccess": snapshot.settings.tunnel_dashboard_access,
        "tunnelUrl": snapshot.settings.tunnel_url,
        "tailscaleUrl": snapshot.settings.tailscale_url,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProxyTestRequest {
    proxy_url: Option<String>,
    test_url: Option<String>,
    timeout_ms: Option<u64>,
}

async fn proxy_test_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ProxyTestRequest>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let Some(proxy_url) = req
        .proxy_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "proxyUrl is required" })),
        )
            .into_response();
    };

    let test_url = req
        .test_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("https://google.com/");
    let timeout_ms = req.timeout_ms.unwrap_or(8_000).clamp(1, 30_000);

    let proxy = match reqwest::Proxy::all(proxy_url) {
        Ok(proxy) => proxy,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "ok": false,
                    "error": format!("Invalid proxy URL: {error}"),
                })),
            )
                .into_response()
        }
    };

    let client = match reqwest::Client::builder()
        .proxy(proxy)
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .user_agent("openproxy")
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": error.to_string() })),
            )
                .into_response()
        }
    };

    let started_at = std::time::Instant::now();
    match client.head(test_url).send().await {
        Ok(response) => Json(json!({
            "ok": response.status().is_success(),
            "status": response.status().as_u16(),
            "statusText": response.status().canonical_reason().unwrap_or(""),
            "url": test_url,
            "elapsedMs": started_at.elapsed().as_millis() as u64,
        }))
        .into_response(),
        Err(error) => {
            let message = if error.is_timeout() {
                "Proxy test timed out".to_string()
            } else {
                error.to_string()
            };
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": message })),
            )
                .into_response()
        }
    }
}

pub(super) fn auth_error_response(error: AuthError) -> Response {
    let status = StatusCode::UNAUTHORIZED;
    (
        status,
        Json(json!({
            "error": {
                "message": error.message(),
                "type": "authentication_error",
                "code": "invalid_api_key"
            }
        })),
    )
        .into_response()
}

fn bad_request_response(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "success": false, "error": message })),
    )
        .into_response()
}

/// Providers that authenticate with a browser session cookie (stored in the
/// `api_key` field). Must match `WEB_COOKIE_PROVIDERS` in the dashboard.
fn is_web_cookie_provider(provider: &str) -> bool {
    matches!(provider, "grok-web" | "perplexity-web")
}

fn normalize_create_provider_proxy(
    req: &CreateProviderRequest,
) -> Result<(Option<bool>, Option<String>, Option<String>), Response> {
    let has_proxy_fields = req.connection_proxy_enabled.is_some()
        || req.connection_proxy_url.is_some()
        || req.connection_no_proxy.is_some();

    if !has_proxy_fields {
        return Ok((None, None, None));
    }

    let enabled = req.connection_proxy_enabled.unwrap_or(false);
    let url = req
        .connection_proxy_url
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let no_proxy = req
        .connection_no_proxy
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();

    if enabled && url.is_empty() {
        return Err(bad_request_response(
            "Connection proxy URL is required when connection proxy is enabled",
        ));
    }

    Ok((Some(enabled), Some(url), Some(no_proxy)))
}

fn normalize_create_provider_proxy_pool(
    proxy_pools: &[crate::types::ProxyPool],
    proxy_pool_id_input: Option<&Value>,
) -> Result<Option<String>, String> {
    let Some(proxy_pool_id_input) = proxy_pool_id_input else {
        return Ok(None);
    };

    if proxy_pool_id_input.is_null() {
        return Ok(None);
    }

    let raw = proxy_pool_id_input
        .as_str()
        .map(str::trim)
        .unwrap_or_default();

    if raw.is_empty() || raw == "__none__" {
        return Ok(None);
    }

    if !proxy_pools.iter().any(|proxy_pool| proxy_pool.id == raw) {
        return Err("Proxy pool not found".into());
    }

    Ok(Some(raw.to_string()))
}
