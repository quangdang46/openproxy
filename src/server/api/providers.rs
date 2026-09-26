use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Instant;
use tokio::time::{timeout, Duration};

use crate::server::state::AppState;

fn require_management_access(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    super::require_dashboard_or_management_api_key(headers, state)
}

#[derive(Debug, Deserialize)]
struct SuggestedModelsQuery {
    url: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

fn value_as_u64(value: Option<&Value>) -> Option<u64> {
    match value {
        Some(Value::Number(number)) => number.as_u64(),
        Some(Value::String(text)) => text.parse().ok(),
        _ => None,
    }
}

fn filter_suggested_models(kind: &str, values: &[Value]) -> Result<Vec<Value>, String> {
    match kind {
        "openrouter-free" => {
            let mut filtered = values
                .iter()
                .filter_map(|value| {
                    let pricing = value.get("pricing")?;
                    let prompt = pricing.get("prompt")?.as_str()?;
                    let completion = pricing.get("completion")?.as_str()?;
                    let context_length = value_as_u64(value.get("context_length"))?;
                    if prompt != "0" || completion != "0" || context_length < 200_000 {
                        return None;
                    }

                    Some(json!({
                        "id": value.get("id").and_then(Value::as_str).unwrap_or_default(),
                        "name": value.get("name").and_then(Value::as_str),
                        "contextLength": context_length,
                    }))
                })
                .collect::<Vec<_>>();

            filtered.sort_by(|a, b| {
                value_as_u64(b.get("contextLength")).cmp(&value_as_u64(a.get("contextLength")))
            });
            Ok(filtered)
        }
        "opencode-free" => Ok(values
            .iter()
            .filter_map(|value| {
                let id = value.get("id").and_then(Value::as_str)?;
                id.ends_with("-free").then(|| {
                    json!({
                        "id": id,
                        "name": id,
                    })
                })
            })
            .collect()),
        _ => Err("Unknown filter type".to_string()),
    }
}

async fn get_suggested_models(Query(query): Query<SuggestedModelsQuery>) -> Response {
    // Public route: matches 9router's UX. The endpoint is a pure passthrough
    // proxy that fetches a public provider models URL (e.g. opencode.ai,
    // openrouter.ai) and filters the response. No secrets are returned.
    let Some(url) = query
        .url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing url or type" })),
        )
            .into_response();
    };
    let Some(kind) = query
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing url or type" })),
        )
            .into_response();
    };

    if filter_suggested_models(kind, &[]).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Unknown filter type" })),
        )
            .into_response();
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(client) => client,
        Err(_) => return Json(json!({ "data": [] })).into_response(),
    };

    let response = match client.get(url).send().await {
        Ok(response) if response.status().is_success() => response,
        Ok(_) | Err(_) => return Json(json!({ "data": [] })).into_response(),
    };

    let payload = match response.json::<Value>().await {
        Ok(payload) => payload,
        Err(_) => return Json(json!({ "data": [] })).into_response(),
    };

    let raw = payload
        .get("data")
        .or_else(|| payload.get("models"))
        .unwrap_or(&payload);
    let items = raw.as_array().cloned().unwrap_or_default();
    let data = filter_suggested_models(kind, &items).unwrap_or_default();

    Json(json!({ "data": data })).into_response()
}

// ============================================================
// Provider Validate API - /api/providers/validate
// ============================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateProviderRequest {
    provider: String,
    api_key: Option<String>,
    provider_specific_data: Option<Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateProviderResponse {
    pub valid: bool,
    pub error: Option<String>,
}

async fn validate_provider_credentials(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ValidateProviderRequest>,
) -> impl IntoResponse {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let api_key = req.api_key.as_deref();
    let base_url = req
        .provider_specific_data
        .as_ref()
        .and_then(|v| v.get("baseUrl"))
        .and_then(Value::as_str)
        .map(String::from);

    let provider = req.provider.as_str();
    let (valid, error, _) = test_provider_api(provider, api_key, base_url.as_deref()).await;

    Json(ValidateProviderResponse { valid, error }).into_response()
}

// ============================================================
// Provider-Node Validate API - /api/provider-nodes/validate
// ============================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateNodeRequest {
    base_url: String,
    api_key: String,
    r#type: Option<String>,
    model_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateNodeResponse {
    pub valid: bool,
    pub error: Option<String>,
    pub method: Option<String>,
    pub dimensions: Option<u32>,
}

async fn validate_provider_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ValidateNodeRequest>,
) -> impl IntoResponse {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let base_url = req.base_url.trim().trim_end_matches('/');
    let api_key = req.api_key.as_str();
    let node_type = req.r#type.as_deref().unwrap_or("openai-compatible");
    let model_id = req.model_id.as_deref();

    // Custom embedding validation
    if node_type == "custom-embedding" {
        if model_id.is_none() || model_id.unwrap().trim().is_empty() {
            return Json(ValidateNodeResponse {
                valid: false,
                error: Some("Model ID required for embedding validation".to_string()),
                method: None,
                dimensions: None,
            })
            .into_response();
        }

        let embed_url = format!("{}/embeddings", base_url);
        let model_id = model_id.unwrap_or_default();

        match test_embedding_url(&embed_url, api_key, model_id).await {
            Ok(dims) => Json(ValidateNodeResponse {
                valid: true,
                error: None,
                method: Some("embeddings".to_string()),
                dimensions: dims,
            })
            .into_response(),
            Err(e) => Json(ValidateNodeResponse {
                valid: false,
                error: Some(e),
                method: Some("embeddings".to_string()),
                dimensions: None,
            })
            .into_response(),
        }
    } else {
        // OpenAI compatible or Anthropic compatible
        let is_anthropic = node_type == "anthropic-compatible";

        let models_url = if is_anthropic {
            // Strip /messages suffix if present
            let base = base_url.trim_end_matches("/messages");
            format!("{}/models", base)
        } else {
            format!("{}/models", base_url)
        };

        match test_url(
            &models_url,
            api_key,
            if is_anthropic {
                Some("anthropic")
            } else {
                None
            },
            model_id,
        )
        .await
        {
            Ok(_) => Json(ValidateNodeResponse {
                valid: true,
                error: None,
                method: Some("models".to_string()),
                dimensions: None,
            })
            .into_response(),
            Err(_) => {
                // Fallback to chat endpoint if model_id provided
                if model_id.is_some() {
                    let chat_url = format!("{}/chat/completions", base_url);
                    match test_chat_url(&chat_url, api_key, model_id, is_anthropic).await {
                        Ok(_) => Json(ValidateNodeResponse {
                            valid: true,
                            error: None,
                            method: Some("chat".to_string()),
                            dimensions: None,
                        })
                        .into_response(),
                        Err(e) => Json(ValidateNodeResponse {
                            valid: false,
                            error: Some(e),
                            method: Some("chat".to_string()),
                            dimensions: None,
                        })
                        .into_response(),
                    }
                } else {
                    Json(ValidateNodeResponse {
                        valid: false,
                        error: Some("Models endpoint not available".to_string()),
                        method: None,
                        dimensions: None,
                    })
                    .into_response()
                }
            }
        }
    }
}

// ============================================================
// Models Test API - /api/models/test
// ============================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestModelRequest {
    model: Option<String>,
    kind: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestModelResponse {
    pub ok: bool,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
    pub status: Option<u16>,
    /// Set on a soft pass — the round-trip worked, but the response is not the
    /// plain completion the probe usually gets.
    pub note: Option<String>,
}

/// 9router spends 1024 tokens on the probe, not one (ping.js:117-120).
/// Reasoning models (ClinePass/kimi-k3, deepseek-v4-pro) burn the budget on
/// chain-of-thought before emitting an answer, so a tiny probe starves the
/// answer and reports "no completion choices" for a model that works.
fn chat_probe_body(model: &str) -> Value {
    json!({
        "model": model,
        "max_tokens": 1024,
        "stream": false,
        "messages": [{ "role": "user", "content": "hi" }]
    })
}

/// A length-limited response that carries reasoning but no visible content is a
/// successful round-trip, not a failure (ping.js:196-205, issue #3010). Returns
/// the note to surface so the caller can say why the answer looked empty.
fn reasoning_only_note(parsed: Option<&Value>) -> Option<&'static str> {
    let choices = parsed?.get("choices")?.as_array()?;
    let first = choices.first()?;
    if first.get("finish_reason")?.as_str()? != "length" {
        return None;
    }
    let message = first.get("message")?;
    let content_empty = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .is_empty();
    if !content_empty {
        return None;
    }
    let has_reasoning = [
        "reasoning",
        "reasoning_content",
        "thinking",
        "thinking_content",
    ]
    .iter()
    .any(|key| {
        message
            .get(*key)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    });
    has_reasoning.then_some("reasoning-only response (length-limited)")
}

fn truncate_test_model_detail(detail: &str) -> String {
    detail.chars().take(240).collect()
}

fn format_test_model_http_error(status: u16, detail: Option<&str>) -> String {
    match detail {
        Some(detail) if !detail.is_empty() => {
            format!("HTTP {status}: {}", truncate_test_model_detail(detail))
        }
        _ => format!("HTTP {status}"),
    }
}

async fn test_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TestModelRequest>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let Some(model) = req.model.filter(|model| !model.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Model required" })),
        )
            .into_response();
    };
    let kind = req.kind.as_deref().unwrap_or("chat");

    // Route to appropriate internal endpoint
    let internal_path = if kind == "embedding" {
        "/v1/embeddings"
    } else {
        "/v1/chat/completions"
    };

    let body = if kind == "embedding" {
        serde_json::json!({
            "model": model,
            "input": "test"
        })
    } else {
        chat_probe_body(&model)
    };

    let base_url = internal_base_url(&headers);
    let client = reqwest::Client::new();
    let url = format!("{}{}", base_url, internal_path);

    let start = Instant::now();

    // Use API key auth if available
    let snapshot = state.db.snapshot();
    let api_key = snapshot
        .api_keys
        .iter()
        .find(|k| k.is_active.unwrap_or(true))
        .map(|k| k.key.clone());

    let mut request = client.post(&url).json(&body);

    if let Some(key) = api_key {
        request = request.header("Authorization", format!("Bearer {}", key));
    }

    match timeout(Duration::from_secs(15), request.send()).await {
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "ok": false,
                "error": "Request timed out",
            })),
        )
            .into_response(),
        Ok(Err(error)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "ok": false,
                "error": error.to_string(),
            })),
        )
            .into_response(),
        Ok(Ok(response)) => {
            let latency_ms = start.elapsed().as_millis() as u64;
            let status = response.status().as_u16();
            let ok_status = response.status().is_success();
            let raw_text = response.text().await.unwrap_or_default();
            let parsed: Option<Value> = serde_json::from_str(&raw_text).ok();

            if !ok_status {
                let detail = if kind == "embedding" {
                    parsed
                        .as_ref()
                        .and_then(|value| value.get("error"))
                        .and_then(|value| {
                            value
                                .get("message")
                                .and_then(Value::as_str)
                                .or_else(|| value.as_str())
                        })
                        .or_else(|| (!raw_text.is_empty()).then_some(raw_text.as_str()))
                } else {
                    parsed
                        .as_ref()
                        .and_then(|value| value.get("error"))
                        .and_then(|value| {
                            value
                                .get("message")
                                .and_then(Value::as_str)
                                .or_else(|| value.as_str())
                        })
                        .or_else(|| {
                            parsed
                                .as_ref()
                                .and_then(|value| value.get("msg"))
                                .and_then(Value::as_str)
                        })
                        .or_else(|| {
                            parsed
                                .as_ref()
                                .and_then(|value| value.get("message"))
                                .and_then(Value::as_str)
                        })
                        .or_else(|| (!raw_text.is_empty()).then_some(raw_text.as_str()))
                };

                return Json(TestModelResponse {
                    ok: false,
                    latency_ms: Some(latency_ms),
                    error: Some(format_test_model_http_error(status, detail)),
                    status: Some(status),
                    note: None,
                })
                .into_response();
            }

            if kind == "embedding" {
                let has_embedding = parsed
                    .as_ref()
                    .and_then(|value| value.get("data"))
                    .and_then(Value::as_array)
                    .and_then(|data| data.first())
                    .and_then(|item| item.get("embedding"))
                    .and_then(Value::as_array)
                    .is_some();

                return Json(TestModelResponse {
                    ok: has_embedding,
                    latency_ms: Some(latency_ms),
                    error: (!has_embedding)
                        .then(|| "Provider returned no embedding data".to_string()),
                    status: Some(status),
                    note: None,
                })
                .into_response();
            }

            let provider_status = parsed
                .as_ref()
                .and_then(|value| value.get("status"))
                .and_then(|value| {
                    value
                        .as_u64()
                        .map(|status| status.to_string())
                        .or_else(|| value.as_str().map(str::to_string))
                });
            let provider_msg = parsed
                .as_ref()
                .and_then(|value| value.get("msg"))
                .and_then(Value::as_str)
                .or_else(|| {
                    parsed
                        .as_ref()
                        .and_then(|value| value.get("message"))
                        .and_then(Value::as_str)
                });

            if let Some(provider_status) = provider_status {
                if provider_status != "200" && provider_status != "0" {
                    return Json(TestModelResponse {
                        ok: false,
                        latency_ms: Some(latency_ms),
                        error: provider_msg
                            .map(|msg| format!("Provider status {}: {}", provider_status, msg))
                            .or_else(|| Some(format!("Provider status {}", provider_status))),
                        status: Some(status),
                        note: None,
                    })
                    .into_response();
                }
            }

            if let Some(provider_error) = parsed
                .as_ref()
                .and_then(|value| value.get("error"))
                .and_then(|value| {
                    value
                        .get("message")
                        .and_then(Value::as_str)
                        .or_else(|| value.as_str())
                })
            {
                return Json(TestModelResponse {
                    ok: false,
                    latency_ms: Some(latency_ms),
                    error: Some(truncate_test_model_detail(provider_error)),
                    status: Some(status),
                    note: None,
                })
                .into_response();
            }

            // OpenAI-style response: { choices: [...] }
            let has_choices = parsed
                .as_ref()
                .and_then(|value| value.get("choices"))
                .and_then(Value::as_array)
                .map(|choices| !choices.is_empty())
                .unwrap_or(false);

            // Claude / Anthropic-style response: { content: [...], stop_reason }
            // forwarded through the proxy as-is. Treat a non-empty `content`
            // array as a successful round-trip too (Bug 8).
            let has_anthropic_content = parsed
                .as_ref()
                .and_then(|value| value.get("content"))
                .and_then(Value::as_array)
                .map(|content| !content.is_empty())
                .unwrap_or(false);

            // Gemini-style response: { candidates: [...] }
            let has_gemini_candidates = parsed
                .as_ref()
                .and_then(|value| value.get("candidates"))
                .and_then(Value::as_array)
                .map(|c| !c.is_empty())
                .unwrap_or(false);

            let ok_completion = has_choices || has_anthropic_content || has_gemini_candidates;
            // A length-limited, reasoning-only response is a real round-trip
            // that simply has no visible answer to show, so it passes and the
            // note says why (ping.js:196-205).
            let note = reasoning_only_note(parsed.as_ref());

            Json(TestModelResponse {
                ok: ok_completion,
                latency_ms: Some(latency_ms),
                error: (!ok_completion)
                    .then(|| "Provider returned no completion choices for this model".to_string()),
                status: Some(status),
                note: note.map(str::to_string),
            })
            .into_response()
        }
    }
}

fn internal_base_url(_headers: &HeaderMap) -> String {
    // Always use localhost to prevent SSRF via caller-controlled Host headers.
    let port = std::env::var("PORT")
        .ok()
        .unwrap_or_else(|| "4623".to_string());
    format!("http://127.0.0.1:{port}")
}

// ============================================================
// Helper Functions
// ============================================================

/// Reject URLs that point to private/internal networks or non-HTTPS schemes
/// to prevent SSRF attacks from caller-controlled base_url fields.
fn is_safe_outbound_url(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("Invalid URL: {e}"))?;

    // Only allow http(s) schemes
    match parsed.scheme() {
        "http" | "https" => {}
        _ => return Err("Only http/https URLs allowed".to_string()),
    }

    // Reject URLs without a host
    let host_str = parsed.host_str().unwrap_or("").to_lowercase();

    // Block private/internal networks
    if host_str.is_empty()
        || host_str == "localhost"
        || host_str == "127.0.0.1"
        || host_str == "::1"
        || host_str.starts_with("10.")
        || host_str.starts_with("192.168.")
        || host_str.starts_with("172.16.")
        || host_str.starts_with("172.17.")
        || host_str.starts_with("172.18.")
        || host_str.starts_with("172.19.")
        || host_str.starts_with("172.2")
        || host_str.starts_with("172.3")
        || host_str.starts_with("0.")
        || host_str.ends_with(".local")
        || host_str.ends_with(".internal")
        || host_str.ends_with(".localhost")
    {
        return Err(
            "URLs pointing to private/internal networks are not allowed for provider validation"
                .to_string(),
        );
    }

    Ok(())
}

/// Test connectivity to a provider's `/v1/models` (or equivalent) endpoint.
/// Returns `(success, optional error string, optional latency in ms)`.
///
/// Exposed at `pub(crate)` so the CLI can run the same validation logic
/// directly without going through the HTTP API.
pub(crate) async fn test_provider_api(
    provider: &str,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> (bool, Option<String>, Option<u64>) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build();

    let client = match client {
        Ok(c) => c,
        Err(_) => {
            return (
                false,
                Some("Failed to create HTTP client".to_string()),
                None,
            );
        }
    };

    let _start = Instant::now();

    // Build test URL and request based on provider
    match provider {
        "openai" => {
            let url = "https://api.openai.com/v1/models";
            let mut request = client.get(url);
            if let Some(key) = api_key {
                request = request.header("Authorization", format!("Bearer {}", key));
            }

            match request.send().await {
                Ok(resp) => {
                    let latency_ms = _start.elapsed().as_millis() as u64;
                    let success = resp.status().is_success();
                    let error = if success {
                        None
                    } else {
                        Some(describe_http_failure("openai", resp).await)
                    };
                    (success, error, Some(latency_ms))
                }
                Err(e) => (false, Some(format!("network: {e}")), None),
            }
        }
        "anthropic" => {
            let url = "https://api.anthropic.com/v1/models";
            let mut request = client.get(url);
            if let Some(key) = api_key {
                request = request
                    .header("x-api-key", key)
                    .header("Anthropic-Version", "2023-06-01");
            }

            match request.send().await {
                Ok(resp) => {
                    let latency_ms = _start.elapsed().as_millis() as u64;
                    let success = resp.status().is_success();
                    let error = if success {
                        None
                    } else {
                        Some(describe_http_failure("anthropic", resp).await)
                    };
                    (success, error, Some(latency_ms))
                }
                Err(e) => (false, Some(format!("network: {e}")), None),
            }
        }
        "gemini" => {
            if let Some(key) = api_key {
                let url = format!(
                    "https://generativelanguage.googleapis.com/v1beta/models?key={}",
                    key
                );
                match client.get(&url).send().await {
                    Ok(resp) => {
                        let latency_ms = _start.elapsed().as_millis() as u64;
                        let success = resp.status().is_success();
                        let error = if success {
                            None
                        } else {
                            Some(describe_http_failure("gemini", resp).await)
                        };
                        (success, error, Some(latency_ms))
                    }
                    Err(e) => (false, Some(format!("network: {e}")), None),
                }
            } else {
                (false, Some("API key required".to_string()), None)
            }
        }
        "openrouter" => {
            let url = "https://openrouter.ai/api/v1/models";
            let mut request = client.get(url);
            if let Some(key) = api_key {
                request = request.header("Authorization", format!("Bearer {}", key));
            }

            match request.send().await {
                Ok(resp) => {
                    let latency_ms = _start.elapsed().as_millis() as u64;
                    let success = resp.status().is_success();
                    let error = if success {
                        None
                    } else {
                        Some(describe_http_failure("openrouter", resp).await)
                    };
                    (success, error, Some(latency_ms))
                }
                Err(e) => (false, Some(format!("network: {e}")), None),
            }
        }
        // Custom/OpenAI compatible providers with base_url
        _ => {
            // Fall back to the built-in default URL from PROVIDER_CONFIGS
            // when the user didn't supply one (e.g. `openproxy provider test llm7`).
            let resolved_url = base_url
                .map(|s| s.to_string())
                .or_else(|| crate::core::executor::provider_config_base_url(provider));

            if let Some(url) = resolved_url {
                let test_url = format!("{}/models", url.trim_end_matches('/'));
                let mut request = client.get(&test_url);
                if let Some(key) = api_key {
                    request = request.header("Authorization", format!("Bearer {}", key));
                }

                match request.send().await {
                    Ok(resp) => {
                        let latency_ms = _start.elapsed().as_millis() as u64;
                        if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 {
                            (false, Some("Invalid API key".to_string()), Some(latency_ms))
                        } else {
                            let success = resp.status().is_success();
                            let error = if success {
                                None
                            } else {
                                Some(describe_http_failure("custom", resp).await)
                            };
                            (success, error, Some(latency_ms))
                        }
                    }
                    Err(e) => (false, Some(format!("network: {e}")), None),
                }
            } else {
                (false, Some("Base URL required".to_string()), None)
            }
        }
    }
}

/// Build a human-readable description of an upstream HTTP failure. Tries to
/// surface the upstream JSON error (e.g. OpenAI's `error.message`) and falls
/// back to the canonical status reason. Used by `test_provider_api` and
/// `provider validate` so users never see a bare `error: null` (bug #14).
async fn describe_http_failure(provider: &str, resp: reqwest::Response) -> String {
    let status = resp.status();
    let canonical = status.canonical_reason().unwrap_or("");
    let body = resp.text().await.unwrap_or_default();
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return format!("{provider} responded HTTP {} {canonical}", status.as_u16());
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(msg) = value
            .pointer("/error/message")
            .or_else(|| value.pointer("/error/error/message"))
            .or_else(|| value.pointer("/message"))
            .and_then(Value::as_str)
        {
            return format!("HTTP {} — {msg}", status.as_u16());
        }
    }
    let snippet: String = trimmed.chars().take(160).collect();
    format!("HTTP {} {canonical}: {snippet}", status.as_u16())
}

/// Probe a custom-embedding node the way the model would actually be used.
///
/// 9router POSTs `{ model, input: "ping" }` to `{base}/embeddings`
/// (`validate-node` route, "Custom Embedding Validation"). A GET probe never
/// reaches an embeddings endpoint, so it answered "invalid" for a working node
/// — and the dimension count the dashboard shows came back as `null` because the
/// response body was discarded.
async fn test_embedding_url(
    url: &str,
    api_key: &str,
    model_id: &str,
) -> Result<Option<u32>, String> {
    if url::Url::parse(url).is_err() {
        return Err("Invalid test URL".to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| "Failed to create HTTP client".to_string())?;

    let response = client
        .post(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "model": model_id.trim(),
            "input": "ping",
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err("API key unauthorized".to_string());
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let snippet: String = body.chars().take(200).collect();
        return Err(if snippet.is_empty() {
            format!("Embeddings request failed ({})", status.as_u16())
        } else {
            format!("Embeddings request failed ({}): {snippet}", status.as_u16())
        });
    }

    let body: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    let dimensions = body
        .pointer("/data/0/embedding")
        .and_then(|v| v.as_array())
        .map(|embedding| embedding.len() as u32);
    Ok(dimensions)
}

async fn test_url(
    url: &str,
    api_key: &str,
    provider_type: Option<&str>,
    _model_id: Option<&str>,
) -> Result<(), String> {
    // Allow test_url for known provider endpoints during provider add flows
    // but still validate the URL is well-formed
    if url::Url::parse(url).is_err() {
        return Err("Invalid test URL".to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build();

    let client = match client {
        Ok(c) => c,
        Err(_) => return Err("Failed to create HTTP client".to_string()),
    };

    let mut request = client.get(url);

    if let Some("anthropic") = provider_type {
        request = request
            .header("x-api-key", api_key)
            .header("Anthropic-Version", "2023-06-01")
            .header("Authorization", format!("Bearer {}", api_key));
    } else {
        request = request.header("Authorization", format!("Bearer {}", api_key));
    }

    match request.send().await {
        Ok(resp) => {
            if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 {
                Err("API key unauthorized".to_string())
            } else if resp.status().is_success() {
                Ok(())
            } else {
                Err(format!(
                    "Request failed with status {}",
                    resp.status().as_u16()
                ))
            }
        }
        Err(e) => Err(e.to_string()),
    }
}

async fn test_chat_url(
    url: &str,
    api_key: &str,
    model_id: Option<&str>,
    is_anthropic: bool,
) -> Result<(), String> {
    if url::Url::parse(url).is_err() {
        return Err("Invalid test URL".to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build();

    let client = match client {
        Ok(c) => c,
        Err(_) => return Err("Failed to create HTTP client".to_string()),
    };

    let model = model_id.unwrap_or("gpt-3.5-turbo");

    let body = if is_anthropic {
        serde_json::json!({
            "model": model,
            "messages": [{ "role": "user", "content": "ping" }],
            "max_tokens": 1
        })
    } else {
        serde_json::json!({
            "model": model,
            "messages": [{ "role": "user", "content": "ping" }],
            "max_tokens": 1
        })
    };

    let mut request = client.post(url).json(&body);

    if is_anthropic {
        request = request
            .header("x-api-key", api_key)
            .header("Anthropic-Version", "2023-06-01")
            .header("Authorization", format!("Bearer {}", api_key));
    } else {
        request = request.header("Authorization", format!("Bearer {}", api_key));
    }

    match request.send().await {
        Ok(resp) => {
            if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 {
                Err("API key unauthorized".to_string())
            } else if resp.status().is_success() || resp.status().as_u16() == 400 {
                // 400 may mean auth passed but model invalid
                Ok(())
            } else {
                Err(format!(
                    "Request failed with status {}",
                    resp.status().as_u16()
                ))
            }
        }
        Err(e) => Err(e.to_string()),
    }
}

// ============================================================
// Kilo Free Models API - /api/providers/kilo/free-models
// ============================================================

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KiloFreeModelsResponse {
    pub models: Vec<KiloFreeModel>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KiloFreeModel {
    pub id: String,
    pub name: String,
    pub context_length: Option<u32>,
    pub pricing: Option<KiloModelPricing>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KiloModelPricing {
    pub prompt: Option<String>,
    pub completion: Option<String>,
}

async fn get_kilo_free_models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    // Return the known free models from Kilo provider
    // These are models that are free to use via the Kilo API
    let models = vec![
        KiloFreeModel {
            id: "kilo/gpt-4.1-mini".to_string(),
            name: "GPT-4.1 Mini".to_string(),
            context_length: Some(128000),
            pricing: Some(KiloModelPricing {
                prompt: Some("0.075".to_string()),
                completion: Some("0.15".to_string()),
            }),
        },
        KiloFreeModel {
            id: "kilo/claude-sonnet-4-20250514".to_string(),
            name: "Claude Sonnet 4 (May 2025)".to_string(),
            context_length: Some(200000),
            pricing: Some(KiloModelPricing {
                prompt: Some("3.00".to_string()),
                completion: Some("15.00".to_string()),
            }),
        },
        KiloFreeModel {
            id: "kilo/reasoner-r".to_string(),
            name: "Reasoner R".to_string(),
            context_length: Some(128000),
            pricing: Some(KiloModelPricing {
                prompt: Some("0.5".to_string()),
                completion: Some("2.00".to_string()),
            }),
        },
        KiloFreeModel {
            id: "kilo/qwen3-8b".to_string(),
            name: "Qwen3 8B".to_string(),
            context_length: Some(32000),
            pricing: Some(KiloModelPricing {
                prompt: Some("0.0".to_string()),
                completion: Some("0.0".to_string()),
            }),
        },
        KiloFreeModel {
            id: "kilo/phi-4".to_string(),
            name: "Phi-4".to_string(),
            context_length: Some(16000),
            pricing: Some(KiloModelPricing {
                prompt: Some("0.0".to_string()),
                completion: Some("0.0".to_string()),
            }),
        },
        KiloFreeModel {
            id: "kilo/gpt-4o-mini".to_string(),
            name: "GPT-4o Mini".to_string(),
            context_length: Some(128000),
            pricing: Some(KiloModelPricing {
                prompt: Some("0.15".to_string()),
                completion: Some("0.60".to_string()),
            }),
        },
    ];

    Json(KiloFreeModelsResponse { models }).into_response()
}

// ============================================================
// Test Batch API - /api/providers/test-batch
// ============================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestBatchRequest {
    pub provider_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestBatchResponse {
    pub results: Vec<TestBatchResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestBatchResult {
    pub provider_id: String,
    pub valid: bool,
    pub error: Option<String>,
    pub latency_ms: Option<u64>,
}

// Wrapper to run async test in a sync context
fn run_sync_test_provider(
    provider: &str,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> (bool, Option<String>, Option<u64>) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(test_provider_api(provider, api_key, base_url))
}

async fn test_provider_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TestBatchRequest>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();

    let mut results = Vec::with_capacity(req.provider_ids.len());

    for id in req.provider_ids {
        let connection = snapshot.provider_connections.iter().find(|c| c.id == id);

        let result = match connection {
            Some(conn) => {
                let provider = conn.provider.as_str();
                let api_key = conn.api_key.as_deref();
                let base_url = conn
                    .provider_specific_data
                    .get("baseUrl")
                    .and_then(Value::as_str)
                    .map(String::from);

                // Run test with timeout
                let test_future = test_provider_api(provider, api_key, base_url.as_deref());
                let timeout_duration = Duration::from_secs(10);

                let (valid, error, latency_ms) = match timeout(timeout_duration, test_future).await
                {
                    Ok(test_result) => test_result,
                    Err(_) => (false, Some("Request timed out".to_string()), None),
                };

                TestBatchResult {
                    provider_id: id,
                    valid,
                    error,
                    latency_ms,
                }
            }
            None => TestBatchResult {
                provider_id: id,
                valid: false,
                error: Some("Connection not found".to_string()),
                latency_ms: None,
            },
        };

        results.push(result);
    }

    Json(TestBatchResponse { results }).into_response()
}

// ============================================================
// Client API - /api/providers/client
// ============================================================

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfoResponse {
    pub client_id: String,
    pub client_name: String,
    pub version: String,
    pub provider: String,
}

async fn get_client_info(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();

    // Get settings for provider info
    let settings = &snapshot.settings;

    // Get client identity - prefer hostname, fallback to os username
    // (whoami 2.x returns Result from hostname()/username()).
    let client_id = whoami::hostname().unwrap_or_else(|_| "unknown".to_string());
    let client_name = whoami::username().unwrap_or_else(|_| "unknown".to_string());

    Json(ClientInfoResponse {
        client_id,
        client_name,
        version: env!("CARGO_PKG_VERSION").to_string(),
        provider: settings.tunnel_provider.clone(),
    })
    .into_response()
}

// ============================================================
// Route Registration
// ============================================================

pub fn routes() -> Router<AppState> {
    Router::new()
        // Kilo free models - GET /api/providers/kilo/free-models
        .route("/api/providers/kilo/free-models", get(get_kilo_free_models))
        .route("/api/providers/suggested-models", get(get_suggested_models))
        // Test batch - POST /api/providers/test-batch
        .route("/api/providers/test-batch", post(test_provider_batch))
        // Client info - GET /api/providers/client
        .route("/api/providers/client", get(get_client_info))
        // Provider models - GET /api/providers/{id}/models
        .route(
            "/api/providers/{id}/models",
            get(super::provider_models::list_provider_models),
        )
        // Provider catalog import - POST /api/providers/{id}/import-models
        .route(
            "/api/providers/{id}/import-models",
            post(super::provider_models::import_provider_models),
        )
        // Provider model tests - POST /api/providers/{id}/test-models
        .route(
            "/api/providers/{id}/test-models",
            post(super::provider_model_tests::test_provider_models),
        )
        // Provider test - POST /api/providers/{id}/test
        .route(
            "/api/providers/{id}/test",
            post(super::provider_connection_test::test_provider_connection),
        )
        // Provider-node validate - POST /api/provider-nodes/validate
        .route("/api/provider-nodes/validate", post(validate_provider_node))
        // Model test - POST /api/models/test
        .route("/api/models/test", post(test_model))
        // Simulation status - GET /api/mock/status (bead sim-19, plan §3.3)
        .route("/api/mock/status", get(get_mock_status))
}

/// Simulation status across providers (bead sim-19).
/// `{forcedAll, providers: {name: {configured, effective, reason,
/// simulationSupported}}}`. Effective is computed WITHOUT request headers
/// (status time has no single request).
async fn get_mock_status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }
    let snapshot = state.db.snapshot();
    let settings_force = snapshot.settings.dev_mock_all;
    let env_force = crate::core::simulation::env_force_all();
    let names = crate::core::executor::provider_config_names();
    let modes = state.db.sqlite.with_conn(|conn| {
        let mut map = serde_json::Map::new();
        for name in &names {
            let s = crate::core::simulation::status_for(conn, name, settings_force);
            map.insert(
                name.clone(),
                json!({
                    "configured": s.configured.to_string(),
                    "effective": s.effective.to_string(),
                    "reason": s.reason.to_string(),
                    "simulationSupported": s.simulation_supported,
                }),
            );
        }
        Ok::<_, rusqlite::Error>(Value::Object(map))
    });
    match modes {
        Ok(providers) => Json(json!({
            "forcedAll": env_force || settings_force,
            "envForce": env_force,
            "settingsForce": settings_force,
            "providers": providers,
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{chat_probe_body, reasoning_only_note, test_embedding_url};
    use serde_json::json;

    /// One canned HTTP response from a throwaway listener, plus the request
    /// text the server actually read.
    async fn upstream_reply(
        status: &'static str,
        body: String,
    ) -> (Result<Option<u32>, String>, String) {
        use std::sync::Arc;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(tokio::sync::Mutex::new(String::new()));
        let recorded = seen.clone();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 4096];
            let read = socket.read(&mut buf).await.unwrap_or(0);
            *recorded.lock().await = String::from_utf8_lossy(&buf[..read]).to_string();
            let reply = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(reply.as_bytes()).await;
        });
        let url = format!("http://{addr}/v1/embeddings");
        let result = test_embedding_url(&url, "sk-test", "text-embedding-3-small").await;
        let request = seen.lock().await.clone();
        (result, request)
    }

    /// 9router POSTs `{model, input: "ping"}` to `{base}/embeddings`. A GET
    /// probe never reaches an embeddings endpoint, so it reported "invalid"
    /// for a working node — and the dimension count the dashboard shows came
    /// back as `null` because the response body was discarded.
    #[tokio::test]
    async fn embedding_probe_posts_and_reports_the_dimension_count() {
        let embedding: Vec<f64> = (0..1536).map(|i| i as f64).collect();
        let (result, request) = upstream_reply(
            "200 OK",
            serde_json::json!({"data": [{"embedding": embedding}]}).to_string(),
        )
        .await;

        assert_eq!(result.as_ref().unwrap(), &Some(1536), "{result:?}");
        assert!(
            request.starts_with("POST /v1/embeddings"),
            "the probe must be a POST, got: {request}"
        );
        assert!(
            request.contains("\"input\":\"ping\"") && request.contains("text-embedding-3-small"),
            "the probe body must match what the model would send, got: {request}"
        );
        assert!(request.contains("Bearer sk-test"), "got: {request}");
    }

    #[tokio::test]
    async fn embedding_probe_reports_an_unauthorized_key() {
        let (result, _) = upstream_reply("401 Unauthorized", String::new()).await;
        assert_eq!(result.unwrap_err(), "API key unauthorized");
    }

    #[tokio::test]
    async fn embedding_probe_surfaces_a_failed_status_with_its_body() {
        let (result, _) = upstream_reply(
            "405 Method Not Allowed",
            r#"{"error":"use POST"}"#.to_string(),
        )
        .await;
        assert_eq!(
            result.unwrap_err(),
            r#"Embeddings request failed (405): {"error":"use POST"}"#
        );
    }

    /// A 2xx body with no embedding array reports "valid" with no dimension
    /// count rather than inventing one (9router: `data?.data?.[0]?.embedding`
    /// is not an array → `null`).
    #[tokio::test]
    async fn embedding_probe_tolerates_a_body_without_an_embedding() {
        let (result, _) = upstream_reply("200 OK", r#"{"object":"list"}"#.to_string()).await;
        assert_eq!(result.unwrap(), None);
    }

    // -----------------------------------------------------------------------
    // audit finding N22 — the chat model-test probe.
    // -----------------------------------------------------------------------

    /// A one-token probe starves any reasoning model: the budget goes on
    /// chain-of-thought and the answer never arrives, so a working model reads
    /// as broken. 9router spends 1024 (ping.js:117-120).
    #[test]
    fn the_chat_probe_budget_is_1024_tokens() {
        let body = chat_probe_body("kimi-k3");
        assert_eq!(body["max_tokens"], json!(1024));
        assert_eq!(body["model"], json!("kimi-k3"));
        assert_eq!(body["stream"], json!(false));
    }

    #[test]
    fn a_length_limited_reasoning_only_response_soft_passes() {
        for key in [
            "reasoning",
            "reasoning_content",
            "thinking",
            "thinking_content",
        ] {
            let body = json!({
                "choices": [{
                    "finish_reason": "length",
                    "message": { "content": "", key: "weighing the options..." }
                }]
            });
            assert_eq!(
                reasoning_only_note(Some(&body)),
                Some("reasoning-only response (length-limited)"),
                "a {key} channel must count as a reasoning-only response"
            );
        }

        // Whitespace is not a visible answer either.
        let blank = json!({
            "choices": [{
                "finish_reason": "length",
                "message": { "content": "  \n ", "reasoning": "..." }
            }]
        });
        assert_eq!(
            reasoning_only_note(Some(&blank)),
            Some("reasoning-only response (length-limited)")
        );
    }

    /// The mirror cases: a real answer, a length limit with nothing in it, and
    /// reasoning on a normally-finished response all leave the result
    /// unexplained.
    #[test]
    fn only_an_empty_length_limited_reasoning_response_is_noted() {
        let answered = json!({
            "choices": [{ "finish_reason": "length", "message": { "content": "partial" } }]
        });
        assert_eq!(reasoning_only_note(Some(&answered)), None);

        let neither = json!({
            "choices": [{ "finish_reason": "length", "message": { "content": "" } }]
        });
        assert_eq!(reasoning_only_note(Some(&neither)), None);

        let stopped = json!({
            "choices": [{
                "finish_reason": "stop",
                "message": { "content": "", "reasoning_content": "..." }
            }]
        });
        assert_eq!(reasoning_only_note(Some(&stopped)), None);

        assert_eq!(reasoning_only_note(None), None);
    }
}
