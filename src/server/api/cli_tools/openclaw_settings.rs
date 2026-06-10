//! GET /api/cli-tools/openclaw-config
//!
//! Read-only aggregation of current OpenProxy config for OpenClaw.
//! OpenClaw reads from this endpoint to auto-configure itself.
//! No file write happens here.

use std::collections::BTreeSet;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use serde_json::{json, Value};

use crate::server::state::AppState;
use crate::types::ProviderConnection;

const DEFAULT_BASE_URL: &str = "http://localhost:4623";

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/cli-tools/openclaw-config",
        get(get_openclaw_config),
    )
}

fn require_management_access(
    headers: &HeaderMap,
    state: &AppState,
) -> std::result::Result<(), Response> {
    super::super::require_dashboard_or_management_api_key(headers, state)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenClawConfigResponse {
    base_url: String,
    api_key: String,
    default_model: String,
    available_models: Vec<String>,
    providers: Vec<Value>,
}

async fn get_openclaw_config(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();

    // Base URL: use the configured mitm_router_base_url, or the default.
    let base_url = if snapshot.settings.mitm_router_base_url.is_empty() {
        DEFAULT_BASE_URL.to_string()
    } else {
        snapshot.settings.mitm_router_base_url.trim_end_matches('/').to_string()
    };

    // API key: first active key from the database.
    let api_key = snapshot
        .api_keys
        .first()
        .map(|k| k.key.clone())
        .unwrap_or_default();

    // Collect available models from multiple sources.
    let mut model_set: BTreeSet<String> = BTreeSet::new();

    // 1. Default models from active provider connections.
    for conn in &snapshot.provider_connections {
        if conn.is_active() {
            if let Some(ref model) = conn.default_model {
                if !model.is_empty() {
                    model_set.insert(model.clone());
                }
            }
        }
    }

    // 2. Custom models (user-defined).
    for cm in &snapshot.custom_models {
        if !cm.id.is_empty() {
            model_set.insert(cm.id.clone());
        }
    }

    // 3. Model aliases (keys).
    for alias in snapshot.model_aliases.keys() {
        model_set.insert(alias.clone());
    }

    let available_models: Vec<String> = model_set.into_iter().collect();
    let default_model = available_models.first().cloned().unwrap_or_default();

    // Providers: active connections with secrets redacted.
    let providers: Vec<Value> = snapshot
        .provider_connections
        .iter()
        .filter(|c| c.is_active())
        .map(redact_connection)
        .collect();

    Json(OpenClawConfigResponse {
        base_url,
        api_key,
        default_model,
        available_models,
        providers,
    })
    .into_response()
}

/// Strip secrets from a ProviderConnection before sending it to OpenClaw.
fn redact_connection(conn: &ProviderConnection) -> Value {
    let mut v = serde_json::to_value(conn).unwrap_or_else(|_| json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.remove("accessToken");
        obj.remove("refreshToken");
        obj.remove("idToken");
        obj.remove("apiKey");
        if let Some(specific) = obj.get_mut("providerSpecificData") {
            if let Some(map) = specific.as_object_mut() {
                for secret in ["accessToken", "refreshToken", "idToken", "apiKey", "cookie", "password"] {
                    map.remove(secret);
                }
            }
        }
    }
    v
}
