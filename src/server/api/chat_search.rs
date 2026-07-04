//! `/v1/search` — Chat + Search hybrid endpoint.
//!
//! Accepts chat-completion-style input (`messages[]`), derives a search
//! query from the last user message, routes through the existing web
//! search module, and returns an OpenAI-compatible chat completion
//! response that embeds search results in both a human-readable message
//! and a structured `search_results` field.

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::core::media::search::{dispatch as search_dispatch, is_search_provider};
use crate::core::proxy::resolve_proxy_target;
use crate::server::auth::require_api_key;
use crate::server::state::AppState;

use super::auth_error_response;
use super::cors::{cors_preflight_response, with_cors_response};

/// Supported shorthand aliases that map to a search provider id.
fn resolve_search_provider(alias: &str) -> Option<&'static str> {
    let lowered = alias.trim().to_lowercase();
    // Direct provider ids
    let static_id = match lowered.as_str() {
        "serper" => "serper",
        "brave-search" | "brave" | "bs" => "brave-search",
        "perplexity" => "perplexity",
        "exa" => "exa",
        "tavily" | "tv" => "tavily",
        "google-pse" | "gps" => "google-pse",
        "linkup" | "lu" => "linkup",
        "searchapi" | "sa" => "searchapi",
        "youcom" | "you" => "youcom",
        "searxng" | "searx" => "searxng",
        _ => return None,
    };
    Some(static_id)
}

/// Extract the search query from the request body.
fn extract_query(body: &Value) -> Option<String> {
    // Explicit `query` field takes precedence.
    if let Some(query) = body
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(query.to_string());
    }

    // Fall back to the last user message content.
    let messages = body.get("messages")?.as_array()?;
    for message in messages.iter().rev() {
        let role = message.get("role").and_then(Value::as_str)?;
        if role != "user" {
            continue;
        }
        let content = message.get("content")?;
        match content {
            Value::String(text) => {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
            Value::Array(parts) => {
                for part in parts.iter() {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            return Some(trimmed.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    None
}

/// Select the best active provider connection for a given search provider.
fn select_search_connection<'a>(
    snapshot: &'a crate::types::AppDb,
    provider: &str,
) -> Option<crate::types::ProviderConnection> {
    snapshot
        .provider_connections
        .iter()
        .filter(|c| {
            c.provider == provider
                && c.is_active()
                && c.api_key
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .is_some()
        })
        .min_by_key(|c| c.priority.unwrap_or(999))
        .cloned()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/chat/search",
            post(handle_search_completions).options(cors_options),
        )
        .route(
            "/v1/v1/chat/search",
            post(handle_search_completions).options(cors_options),
        )
}

pub async fn cors_options() -> Response {
    cors_preflight_response()
}

pub async fn handle_search_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    // -- Authentication --
    if let Err(e) = require_api_key(&headers, &state.db) {
        return auth_error_response(e);
    }

    // -- Parse request body --
    let Json(body) = match body {
        Ok(b) => b,
        Err(_) => {
            return with_cors_response(
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": {
                            "message": "Invalid JSON body",
                            "type": "invalid_request_error",
                            "code": null
                        }
                    })),
                )
                    .into_response(),
            );
        }
    };

    // -- Resolve provider --
    let model_str = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let provider = model_str
        .and_then(resolve_search_provider)
        .or_else(|| {
            // Fallback: check explicit `provider` field.
            body.get("provider")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .and_then(resolve_search_provider)
        })
        .unwrap_or("serper");

    // -- Extract query --
    let query = match extract_query(&body) {
        Some(q) => q,
        None => {
            return with_cors_response(
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": {
                            "message": "Missing search query: provide a `query` field or a `messages` array with a user message",
                            "type": "invalid_request_error",
                            "code": null
                        }
                    })),
                )
                    .into_response(),
            );
        }
    };

    // -- Select credentials --
    let snapshot = state.db.snapshot();
    let connection = match select_search_connection(&snapshot, provider) {
        Some(c) => c,
        None => {
            return with_cors_response(
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": {
                            "message": format!("No active credentials found for search provider: {}", provider),
                            "type": "invalid_request_error",
                            "code": null
                        }
                    })),
                )
                    .into_response(),
            );
        }
    };

    // -- Build the search request body for the dispatch function --
    let max_results = body
        .get("max_results")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .min(100) as u32;

    let search_type = body
        .get("search_type")
        .and_then(Value::as_str)
        .unwrap_or("web");

    let mut search_body = json!({
        "query": query,
        "max_results": max_results,
        "search_type": search_type,
    });

    // Pass through optional search parameters.
    for key in &[
        "country",
        "language",
        "time_range",
        "offset",
        "domain_filter",
        "content_options",
        "provider_options",
    ] {
        if let Some(val) = body.get(*key) {
            search_body[*key] = val.clone();
        }
    }

    // -- Execute search --
    let proxy = resolve_proxy_target(&snapshot, &connection, &snapshot.settings);
    let client = match state.client_pool.get(provider, proxy.as_ref()) {
        Ok(c) => c,
        Err(e) => {
            return with_cors_response(
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "error": {
                            "message": format!("Failed to create HTTP client: {}", e),
                            "type": "server_error",
                            "code": null
                        }
                    })),
                )
                    .into_response(),
            );
        }
    };

    let result = search_dispatch(&client, &connection, provider, &search_body).await;

    let (results_value, usage_tokens) = match result {
        Some(Ok(raw_value)) => {
            // Estimate token usage based on result count.
            let results_arr = raw_value
                .get("results")
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0);
            (raw_value, results_arr as u64)
        }
        Some(Err(err)) => {
            return with_cors_response(
                (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({
                        "error": {
                            "message": format!("Search failed: {}", err.message()),
                            "type": "server_error",
                            "code": format!("search_{}", err.status())
                        }
                    })),
                )
                    .into_response(),
            );
        }
        None => {
            return with_cors_response(
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": {
                            "message": format!("Unsupported search provider: {}", provider),
                            "type": "invalid_request_error",
                            "code": null
                        }
                    })),
                )
                    .into_response(),
            );
        }
    };

    // -- Build the chat-completion-style response --
    let results = results_value
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // Build a human-readable content string from results.
    let mut content_lines = Vec::new();
    for (i, r) in results.iter().enumerate() {
        let title = r.get("title").and_then(Value::as_str).unwrap_or("");
        let url = r.get("url").and_then(Value::as_str).unwrap_or("");
        let snippet = r.get("snippet").and_then(Value::as_str).unwrap_or("");
        content_lines.push(format!("{}. {}", i + 1, title));
        content_lines.push(format!("   URL: {}", url));
        if !snippet.is_empty() {
            content_lines.push(format!("   {}", snippet));
        }
        content_lines.push(String::new());
    }
    let content = content_lines.join("\n");

    let total_results = results_value
        .get("total_results")
        .and_then(Value::as_u64)
        .unwrap_or(results.len() as u64);

    let now = chrono::Utc::now();
    let response = json!({
        "id": format!("searchcmpl-{}", uuid::Uuid::new_v4()),
        "object": "chat.completion",
        "created": now.timestamp(),
        "model": body.get("model").and_then(Value::as_str).unwrap_or(provider),
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": content,
                },
                "finish_reason": "stop"
            }
        ],
        "usage": {
            "prompt_tokens": query.len() as u64 / 4,
            "completion_tokens": usage_tokens,
            "total_tokens": (query.len() as u64 / 4) + usage_tokens,
        },
        "search_results": results,
        "search_metadata": {
            "provider": provider,
            "query": query,
            "total_results": total_results,
            "search_type": search_type,
        }
    });

    let resp = (StatusCode::OK, Json(response)).into_response();
    with_cors_response(resp)
}
