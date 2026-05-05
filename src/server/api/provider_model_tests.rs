use std::time::{Duration, Instant};

use axum::{
    body::to_bytes,
    extract::{Path, State},
    http::{header::AUTHORIZATION, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::future::join_all;
use serde::Serialize;
use serde_json::json;
use tokio::time::timeout;

use crate::core::model::catalog::provider_catalog;
use crate::server::state::AppState;

use super::{chat, provider_models};

const OPENAI_COMPATIBLE_PREFIX: &str = "openai-compatible-";
const ANTHROPIC_COMPATIBLE_PREFIX: &str = "anthropic-compatible-";
const MODEL_TEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
struct TestModelTarget {
    id: String,
    name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderModelTestResult {
    model_id: String,
    name: String,
    ok: bool,
    latency_ms: u64,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderModelTestResponse {
    provider: String,
    connection_id: String,
    results: Vec<ProviderModelTestResult>,
}

pub(super) async fn test_provider_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let Some(connection) = state
        .db
        .snapshot()
        .provider_connections
        .iter()
        .find(|connection| connection.id == id)
        .cloned()
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Connection not found" })),
        )
            .into_response();
    };

    let provider = connection.provider.clone();
    let alias = provider_alias(&provider).to_string();
    let mut models = static_models_for_provider(&provider);

    if models.is_empty() && is_compatible_provider(&provider) {
        models = provider_models::fetch_models_for_connection(&state, &connection)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|model| TestModelTarget {
                name: model.name.clone(),
                id: model.id,
            })
            .collect();
    }

    if models.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "No models configured for this provider" })),
        )
            .into_response();
    }

    let api_key = internal_api_key(&state);
    let (first_model, remaining_models) = models
        .split_first()
        .expect("models should contain at least one entry");

    let mut results = Vec::with_capacity(models.len());
    results.push(ping_model(&state, &alias, first_model.clone(), api_key.as_deref()).await);

    let remaining = join_all(
        remaining_models
            .iter()
            .cloned()
            .map(|model| ping_model(&state, &alias, model, api_key.as_deref())),
    )
    .await;
    results.extend(remaining);

    Json(ProviderModelTestResponse {
        provider,
        connection_id: id,
        results,
    })
    .into_response()
}

fn static_models_for_provider(provider: &str) -> Vec<TestModelTarget> {
    let catalog = provider_catalog();
    let alias = provider_alias(provider);
    catalog
        .models_for_alias(alias)
        .unwrap_or(&[])
        .iter()
        .map(|model| TestModelTarget {
            id: model.id.clone(),
            name: model.name.clone().unwrap_or_else(|| model.id.clone()),
        })
        .collect()
}

fn provider_alias<'a>(provider: &'a str) -> &'a str {
    provider_catalog()
        .static_alias_for_provider(provider)
        .unwrap_or(provider)
}

fn internal_api_key(state: &AppState) -> Option<String> {
    state
        .db
        .snapshot()
        .api_keys
        .iter()
        .find(|key| key.is_active.unwrap_or(true))
        .map(|key| key.key.clone())
}

fn is_compatible_provider(provider: &str) -> bool {
    provider.starts_with(OPENAI_COMPATIBLE_PREFIX)
        || provider.starts_with(ANTHROPIC_COMPATIBLE_PREFIX)
}

async fn ping_model(
    state: &AppState,
    alias: &str,
    model: TestModelTarget,
    api_key: Option<&str>,
) -> ProviderModelTestResult {
    let model_name = format!("{alias}/{}", model.id);
    let start = Instant::now();
    let mut ping_headers = HeaderMap::new();
    if let Some(api_key) = api_key {
        if let Ok(value) = HeaderValue::from_str(&format!("Bearer {api_key}")) {
            ping_headers.insert(AUTHORIZATION, value);
        }
    }

    let body = json!({
        "model": model_name,
        "max_tokens": 1,
        "stream": false,
        "messages": [{ "role": "user", "content": "hi" }]
    });

    let response = match timeout(
        MODEL_TEST_TIMEOUT,
        chat::chat_completions(State(state.clone()), ping_headers, Ok(Json(body))),
    )
    .await
    {
        Ok(response) => response,
        Err(_) => {
            return ProviderModelTestResult {
                model_id: model.id,
                name: model.name,
                ok: false,
                latency_ms: start.elapsed().as_millis() as u64,
                error: Some("Request timed out".to_string()),
            };
        }
    };

    let latency_ms = start.elapsed().as_millis() as u64;
    let status = response.status();
    let ok = status == StatusCode::OK || status == StatusCode::BAD_REQUEST;
    let error = if ok {
        None
    } else {
        Some(read_error_text(response, status).await)
    };

    ProviderModelTestResult {
        model_id: model.id,
        name: model.name,
        ok,
        latency_ms,
        error,
    }
}

async fn read_error_text(response: Response, status: StatusCode) -> String {
    let text = match to_bytes(response.into_body(), usize::MAX).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => return error.to_string(),
    };

    if text.is_empty() {
        format!("HTTP {}", status.as_u16())
    } else {
        let truncated: String = text.chars().take(120).collect();
        format!("HTTP {}: {truncated}", status.as_u16())
    }
}
