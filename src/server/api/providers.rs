use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Instant;

use crate::server::state::AppState;

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
    Path(id): Path<String>,
) -> Json<ProviderModelsResponse> {
    let snapshot = state.db.snapshot();

    // Find the provider connection
    let connection = snapshot
        .provider_connections
        .iter()
        .find(|c| c.id == id);

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

    Json(ProviderModelsResponse { models })
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
    Path(id): Path<String>,
) -> impl IntoResponse {
    let snapshot = state.db.snapshot();

    let connection = match snapshot
        .provider_connections
        .iter()
        .find(|c| c.id == id)
    {
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
    let (valid, error, latency_ms) = test_provider_api(provider, api_key, base_url.as_deref()).await;

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
    State(_state): State<AppState>,
    Json(req): Json<ValidateProviderRequest>,
) -> impl IntoResponse {
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
    State(_state): State<AppState>,
    Json(req): Json<ValidateNodeRequest>,
) -> impl IntoResponse {
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

        match test_url(&models_url, api_key, if is_anthropic { Some("anthropic") } else { None }, model_id).await
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
    Json(req): Json<TestModelRequest>,
) -> impl IntoResponse {
    let model = req.model;
    let kind = req.kind.as_deref().unwrap_or("chat");

    // Route to appropriate internal endpoint
    let internal_path = if kind == "embedding" {
        "/api/v1/embeddings"
    } else {
        "/api/v1/chat/completions"
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

    // Build request to internal endpoint
    let client = reqwest::Client::new();
    let base_url = "http://localhost";
    let url = format!("{}{}", base_url, internal_path);

    let _start = Instant::now();

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

    match request.send().await {
        Ok(response) => {
            let latency_ms = _start.elapsed().as_millis() as u64;
            let status = response.status().as_u16();

            if response.status().is_success() || response.status().as_u16() == 400 {
                // 400 may mean auth passed but model invalid - that's ok for test
                Json(TestModelResponse {
                    ok: true,
                    latency_ms: Some(latency_ms),
                    error: None,
                    status: Some(status),
                })
                .into_response()
            } else {
                let error_text = response.text().await.unwrap_or_default();
                Json(TestModelResponse {
                    ok: false,
                    latency_ms: Some(latency_ms),
                    error: Some(format!("HTTP {}: {}", status, error_text)),
                    status: Some(status),
                })
                .into_response()
            }
        }
        Err(e) => Json(TestModelResponse {
            ok: false,
            latency_ms: None,
            error: Some(e.to_string()),
            status: None,
        })
        .into_response(),
    }
}

// ============================================================
// Helper Functions
// ============================================================

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
            return (false, Some("Failed to create HTTP client".to_string()), None);
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
                Err(format!("Request failed with status {}", resp.status().as_u16()))
            }
        }
        Err(e) => Err(e.to_string()),
    }
}

async fn test_chat_url(url: &str, api_key: &str, model_id: Option<&str>, is_anthropic: bool) -> Result<(), String> {
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
                Err(format!("Request failed with status {}", resp.status().as_u16()))
            }
        }
        Err(e) => Err(e.to_string()),
    }
}

// ============================================================
// Route Registration
// ============================================================

pub fn routes() -> Router<AppState> {
    Router::new()
        // Provider models - GET /api/providers/{id}/models
        .route("/api/providers/{id}/models", get(list_provider_models))
        // Provider test - POST /api/providers/{id}/test
        .route("/api/providers/{id}/test", post(test_provider_connection))
        // Provider validate - POST /api/providers/validate
        .route("/api/providers/validate", post(validate_provider_credentials))
        // Provider-node validate - POST /api/provider-nodes/validate
        .route("/api/provider-nodes/validate", post(validate_provider_node))
        // Model test - POST /api/models/test
        .route("/api/models/test", post(test_model))
}