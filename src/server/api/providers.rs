use axum::{
    extract::{Path, State},
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

// ============================================================
// Provider Models API - /api/providers/:id/models
// ============================================================

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModel {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct ProviderModelsResponse {
    pub models: Vec<ProviderModel>,
}

async fn list_provider_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();

    // Find the provider connection
    let connection = snapshot.provider_connections.iter().find(|c| c.id == id);

    let models = match connection {
        Some(conn) => {
            // Get enabled models from provider_specific_data
            if let Some(models_array) = conn
                .provider_specific_data
                .get("enabledModels")
                .and_then(Value::as_array)
            {
                models_array
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|s| ProviderModel {
                        id: s.to_string(),
                        name: s.to_string(),
                    })
                    .collect()
            } else if let Some(default_model) = conn.default_model.as_deref() {
                vec![ProviderModel {
                    id: default_model.to_string(),
                    name: default_model.to_string(),
                }]
            } else {
                vec![]
            }
        }
        None => vec![],
    };

    Json(ProviderModelsResponse { models }).into_response()
}

// ============================================================
// Provider Test API - /api/providers/:id/test
// ============================================================

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTestResponse {
    pub valid: bool,
    pub error: Option<String>,
    pub refreshed: bool,
    pub latency_ms: Option<u64>,
}

async fn test_provider_connection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();

    let connection = match snapshot.provider_connections.iter().find(|c| c.id == id) {
        Some(c) => c,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": "Connection not found",
                    "valid": false
                })),
            )
                .into_response();
        }
    };

    let provider = connection.provider.as_str();
    let api_key = connection.api_key.as_deref();
    let base_url = connection
        .provider_specific_data
        .get("baseUrl")
        .and_then(Value::as_str)
        .map(String::from);

    // Test based on provider type
    let (valid, error, latency_ms) =
        test_provider_api(provider, api_key, base_url.as_deref()).await;

    Json(ProviderTestResponse {
        valid,
        error,
        refreshed: false,
        latency_ms,
    })
    .into_response()
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

        match test_url(&embed_url, api_key, Some("embedding"), model_id).await {
            Ok(_) => {
                // Try to get dimensions
                let dims = None; // Would need to parse response body
                Json(ValidateNodeResponse {
                    valid: true,
                    error: None,
                    method: Some("embeddings".to_string()),
                    dimensions: dims,
                })
                .into_response()
            }
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
    model: String,
    kind: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestModelResponse {
    pub ok: bool,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
    pub status: Option<u16>,
}

async fn test_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TestModelRequest>,
) -> impl IntoResponse {
    if let Err(response) = require_management_access(&headers, &state) {
        return response;
    }

    let model = req.model;
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
        serde_json::json!({
            "model": model,
            "max_tokens": 1,
            "stream": false,
            "messages": [{ "role": "user", "content": "hi" }]
        })
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
        Err(_) => Json(TestModelResponse {
            ok: false,
            latency_ms: Some(start.elapsed().as_millis() as u64),
            error: Some("Request timed out".to_string()),
            status: None,
        })
        .into_response(),
        Ok(Err(error)) => Json(TestModelResponse {
            ok: false,
            latency_ms: Some(start.elapsed().as_millis() as u64),
            error: Some(error.to_string()),
            status: None,
        })
        .into_response(),
        Ok(Ok(response)) => {
            let latency_ms = start.elapsed().as_millis() as u64;
            let status = response.status().as_u16();
            let ok_status = response.status().is_success();
            let raw_text = response.text().await.unwrap_or_default();
            let parsed: Option<Value> = serde_json::from_str(&raw_text).ok();

            if !ok_status {
                let detail = parsed
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
                    .unwrap_or(raw_text.as_str());

                return Json(TestModelResponse {
                    ok: false,
                    latency_ms: Some(latency_ms),
                    error: Some(format!("HTTP {}: {}", status, detail)),
                    status: Some(status),
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
                    })
                    .into_response();
                }
            }

            let has_choices = parsed
                .as_ref()
                .and_then(|value| value.get("choices"))
                .and_then(Value::as_array)
                .map(|choices| !choices.is_empty())
                .unwrap_or(false);

            Json(TestModelResponse {
                ok: has_choices,
                latency_ms: Some(latency_ms),
                error: (!has_choices)
                    .then(|| "Provider returned no completion choices for this model".to_string()),
                status: Some(status),
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
        return Err("URLs pointing to private/internal networks are not allowed for provider validation".to_string());
    }

    Ok(())
}

async fn test_provider_api(
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
                    (resp.status().is_success(), None, Some(latency_ms))
                }
                Err(e) => (false, Some(e.to_string()), None),
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
                    (resp.status().is_success(), None, Some(latency_ms))
                }
                Err(e) => (false, Some(e.to_string()), None),
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
                        (resp.status().is_success(), None, Some(latency_ms))
                    }
                    Err(e) => (false, Some(e.to_string()), None),
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
                    (resp.status().is_success(), None, Some(latency_ms))
                }
                Err(e) => (false, Some(e.to_string()), None),
            }
        }
        // Custom/OpenAI compatible providers with base_url
        _ => {
            if let Some(url) = base_url {
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
                            (resp.status().is_success(), None, Some(latency_ms))
                        }
                    }
                    Err(e) => (false, Some(e.to_string()), None),
                }
            } else {
                (false, Some("Base URL required".to_string()), None)
            }
        }
    }
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
    let client_id = whoami::hostname();
    let client_name = whoami::username();

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
        // Test batch - POST /api/providers/test-batch
        .route("/api/providers/test-batch", post(test_provider_batch))
        // Client info - GET /api/providers/client
        .route("/api/providers/client", get(get_client_info))
        // Provider models - GET /api/providers/{id}/models
        .route("/api/providers/{id}/models", get(list_provider_models))
        // Provider test - POST /api/providers/{id}/test
        .route("/api/providers/{id}/test", post(test_provider_connection))
        // Provider validate - POST /api/providers/validate
        .route(
            "/api/providers/validate",
            post(validate_provider_credentials),
        )
        // Provider-node validate - POST /api/provider-nodes/validate
        .route("/api/provider-nodes/validate", post(validate_provider_node))
        // Model test - POST /api/models/test
        .route("/api/models/test", post(test_model))
}
