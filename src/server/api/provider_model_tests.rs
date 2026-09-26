use std::time::{Duration, Instant};

use axum::{
    body::to_bytes,
    extract::{Path, State},
    http::{header::AUTHORIZATION, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
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

fn provider_alias(provider: &str) -> &str {
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
    // openproxy-s2oc: only a success status means the model answered. A 400
    // here is openproxy's OWN in-process rejection ("No credentials for
    // provider: X") — the unconfigured-provider / disabled-model case — and
    // the sibling endpoint already treats it as failure
    // (providers.rs `response.status().is_success()`).
    let ok = status.is_success();
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ComboTestModelRequest {
    /// `<provider-prefix>/<model-id>` exactly as it would appear in a
    /// combo's `models` list. Tested via a real
    /// `chat::chat_completions` call with `max_tokens=1`, mirroring the
    /// per-connection `test_provider_models` behaviour so the same
    /// status/latency semantics apply.
    pub(super) model: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ComboTestModelResponse {
    pub(super) model: String,
    pub(super) ok: bool,
    pub(super) latency_ms: u64,
    pub(super) error: Option<String>,
}

/// `POST /api/combos/test-model` — quick health check for a single
/// `<prefix>/<model-id>` combo member. Used by the combo edit modal to
/// give the operator a per-row test icon without having to know which
/// connection backs each combo entry.
///
/// The actual request shape (`max_tokens=1`, single `"hi"` message,
/// non-streaming, 15s timeout, only `200 OK` counts as "model responded")
/// deliberately matches [`ping_model`] so the two surfaces produce
/// comparable results. A `400` is openproxy's own in-process rejection
/// ("No credentials for provider: X"), not an upstream model answer, so it
/// is reported as a failure with the dispatcher's reason (openproxy-s2oc).
pub(super) async fn test_combo_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ComboTestModelRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let model = req.model.trim();
    if model.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "`model` is required" })),
        )
            .into_response();
    }

    let api_key = internal_api_key(&state);
    let start = Instant::now();
    let mut ping_headers = HeaderMap::new();
    if let Some(api_key) = api_key.as_deref() {
        if let Ok(value) = HeaderValue::from_str(&format!("Bearer {api_key}")) {
            ping_headers.insert(AUTHORIZATION, value);
        }
    }

    let body = json!({
        "model": model,
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
            return Json(ComboTestModelResponse {
                model: model.to_string(),
                ok: false,
                latency_ms: start.elapsed().as_millis() as u64,
                error: Some("Request timed out".to_string()),
            })
            .into_response();
        }
    };

    let latency_ms = start.elapsed().as_millis() as u64;
    let status = response.status();
    // openproxy-s2oc: see `ping_model` — a 400 is openproxy's own rejection,
    // not a model answer, so it must surface as `ok: false` with the
    // dispatcher's reason instead of a green tick in the combo modal.
    let ok = status.is_success();
    let error = if ok {
        None
    } else {
        Some(read_error_text(response, status).await)
    };

    Json(ComboTestModelResponse {
        model: model.to_string(),
        ok,
        latency_ms,
        error,
    })
    .into_response()
}

async fn read_error_text(response: Response, status: StatusCode) -> String {
    let text = match to_bytes(response.into_body(), usize::MAX).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => return error.to_string(),
    };

    // ping.js:131-133 hands the operator `error.message` — never the raw
    // envelope — and caps the detail at 240 chars. 120 chars of raw body
    // showed `{"error":{"code":…` and cut the provider's own sentence off
    // mid-word, which is the only thing the test icon has to say.
    let detail = crate::core::utils::error::parse_upstream_message(&text);
    let detail: String = detail.chars().take(240).collect();

    if detail.is_empty() {
        format!("HTTP {}", status.as_u16())
    } else {
        format!("HTTP {}: {detail}", status.as_u16())
    }
}

#[cfg(test)]
mod tests {
    use super::read_error_text;
    use axum::{body::Body, http::StatusCode, response::Response};

    fn error_response(status: StatusCode, body: &str) -> Response {
        Response::builder()
            .status(status)
            .body(Body::from(body.to_string()))
            .expect("response")
    }

    /// The probe surfaces the provider's message, not the JSON envelope
    /// openproxy's own error body wrapped it in.
    #[tokio::test]
    async fn read_error_text_unwraps_the_error_message() {
        let text = read_error_text(
            error_response(
                StatusCode::BAD_REQUEST,
                r#"{"error":{"code":"bad_request","message":"unsupported for chat","type":"invalid_request_error"}}"#,
            ),
            StatusCode::BAD_REQUEST,
        )
        .await;
        assert_eq!(text, "HTTP 400: unsupported for chat");
    }

    /// ping.js:131-133 caps the detail at 240 chars. A message longer than
    /// that is cut there — not at 120, which landed mid-sentence.
    #[tokio::test]
    async fn read_error_text_caps_the_detail_at_240_chars() {
        let long = "x".repeat(400);
        let body = format!(r#"{{"error":{{"message":"{long}"}}}}"#);
        let text = read_error_text(
            error_response(StatusCode::BAD_GATEWAY, &body),
            StatusCode::BAD_GATEWAY,
        )
        .await;
        assert_eq!(text, format!("HTTP 502: {}", "x".repeat(240)));
    }

    /// A body that is not JSON (a bare gateway page) falls through verbatim.
    #[tokio::test]
    async fn read_error_text_keeps_a_non_json_body_verbatim() {
        let text = read_error_text(
            error_response(StatusCode::BAD_GATEWAY, "502 Bad Gateway"),
            StatusCode::BAD_GATEWAY,
        )
        .await;
        assert_eq!(text, "HTTP 502: 502 Bad Gateway");
    }

    /// ping.js:131-133 omits the `: detail` half entirely when there is no
    /// detail — an empty body yields the bare status.
    #[tokio::test]
    async fn read_error_text_without_a_body_is_the_bare_status() {
        let text = read_error_text(
            error_response(StatusCode::SERVICE_UNAVAILABLE, ""),
            StatusCode::SERVICE_UNAVAILABLE,
        )
        .await;
        assert_eq!(text, "HTTP 503");
    }
}
