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
use crate::server::auth::require_api_key_with_reload;
use crate::server::state::AppState;

use super::auth_error_response;
use super::cors::{cors_preflight_response, with_cors_response};

/// Supported shorthand aliases that map to a search provider id.
fn resolve_search_provider(alias: &str) -> Option<&'static str> {
    let lowered = alias.trim().to_lowercase();
    // Direct provider ids
    let static_id = match lowered.as_str() {
        "serper" => "serper",
        "serpingapi" | "sping" => "serpingapi",
        "brave-search" | "brave" | "bs" => "brave-search",
        "perplexity" => "perplexity",
        "exa" => "exa",
        "tavily" | "tv" => "tavily",
        "google-pse" | "gps" => "google-pse",
        "linkup" | "lu" => "linkup",
        "searchapi" | "sa" => "searchapi",
        "youcom" | "you" => "youcom",
        "searxng" | "searx" => "searxng",
        "xquik" => "xquik",
        // Only explicit search aliases map here — bare "ollama" is the chat
        // provider and has no JS mapping (credentialFallback goes the other
        // direction: ollama-search.js:21 reuses the ollama chat key).
        "ollama-search" | "ollama_search" => "ollama-search",
        "glm" => "glm",
        "antigravity" | "ag" => "antigravity",
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

/// Credential fallback owner for search providers that reuse a related
/// chat provider's key (port of `credentialFallback` in the 9router
/// registry: ollama-search → ollama).
fn credential_fallback_provider(provider: &str) -> Option<&'static str> {
    match provider {
        "ollama-search" => Some("ollama"),
        _ => None,
    }
}

fn connection_has_key(c: &crate::types::ProviderConnection) -> bool {
    c.api_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .is_some()
        || c.access_token
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .is_some()
}

/// Select the best active provider connection for a given search provider.
///
/// Port of `src/sse/handlers/search.js` credential loop: when the search
/// provider has no own connection, fall back to the linked provider's
/// credentials (`credentialFallback`, e.g. ollama-search → ollama).
fn select_search_connection(
    snapshot: &crate::types::AppDb,
    provider: &str,
) -> Option<crate::types::ProviderConnection> {
    let own = snapshot
        .provider_connections
        .iter()
        .filter(|c| c.provider == provider && c.is_active() && connection_has_key(c))
        .min_by_key(|c| c.priority.unwrap_or(999))
        .cloned();
    if own.is_some() {
        return own;
    }
    credential_fallback_provider(provider).and_then(|fallback| {
        snapshot
            .provider_connections
            .iter()
            .filter(|c| c.provider == fallback && c.is_active() && connection_has_key(c))
            .min_by_key(|c| c.priority.unwrap_or(999))
            .cloned()
    })
}

/// Canonical search-provider order used for cross-provider failover
/// (mirrors the 15-provider registry in [`resolve_search_provider`]).
const SEARCH_FAILOVER_ORDER: &[&str] = &[
    "serper",
    "serpingapi",
    "brave-search",
    "perplexity",
    "exa",
    "tavily",
    "google-pse",
    "linkup",
    "searchapi",
    "youcom",
    "searxng",
    "xquik",
    "ollama-search",
    "glm",
    "antigravity",
];

/// Pure failover ordering: primary first, then the remaining providers in
/// canonical order. Unit-testable decision fn; the handler filters this
/// down to providers with an active connection.
fn failover_order(primary: &str) -> Vec<&'static str> {
    let mut order = Vec::with_capacity(SEARCH_FAILOVER_ORDER.len());
    if let Some(hit) = SEARCH_FAILOVER_ORDER.iter().find(|id| **id == primary) {
        order.push(*hit);
    } else {
        return order;
    }
    order.extend(
        SEARCH_FAILOVER_ORDER
            .iter()
            .filter(|id| **id != primary)
            .copied(),
    );
    order
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
    // 9router `search.js:48-60` gates on `settings.requireApiKey`, so a
    // deployment that turns the key requirement off accepts anonymous search
    // callers. Keying this on anything else — or requiring a key outright —
    // would make this the one /v1 surface that setting cannot open.
    if state.db.snapshot().settings.require_api_key() {
        if let Err(e) = require_api_key_with_reload(&headers, &state.db).await {
            return auth_error_response(e);
        }
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

    // -- Execute search with cross-provider failover: primary first, then
    // the remaining registry providers that have an active connection --
    let snapshot = state.db.snapshot();
    let mut attempted: Vec<&'static str> = Vec::new();
    let mut last_err_msg: Option<String> = None;
    let mut last_err_code: Option<String> = None;
    let mut success: Option<(Value, u64, &'static str)> = None;

    for candidate in failover_order(provider) {
        let connection = match select_search_connection(&snapshot, candidate) {
            Some(c) => c,
            None => continue,
        };
        attempted.push(candidate);
        let proxy = resolve_proxy_target(&snapshot, &connection, &snapshot.settings);
        let client = match state.client_pool.get(candidate, proxy.as_ref()) {
            Ok(c) => c,
            Err(e) => {
                last_err_msg = Some(format!("Failed to create HTTP client: {}", e));
                last_err_code = Some("server_error".to_string());
                continue;
            }
        };
        match search_dispatch(&client, &connection, candidate, &search_body).await {
            Some(Ok(raw_value)) => {
                let results_arr = raw_value
                    .get("results")
                    .and_then(Value::as_array)
                    .map(|a| a.len())
                    .unwrap_or(0);
                success = Some((raw_value, results_arr as u64, candidate));
                break;
            }
            Some(Err(err)) => {
                last_err_msg = Some(err.message().to_string());
                last_err_code = Some(format!("search_{}", err.status()));
                continue;
            }
            None => continue,
        }
    }

    if attempted.is_empty() {
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

    let Some((results_value, usage_tokens, effective_provider)) = success else {
        return with_cors_response(
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": {
                        "message": format!("Search failed: {}", last_err_msg.unwrap_or_else(|| "all search providers failed".to_string())),
                        "type": "server_error",
                        "code": last_err_code
                    }
                })),
            )
                .into_response(),
        );
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
            "provider": effective_provider,
            "query": query,
            "total_results": total_results,
            "search_type": search_type,
        }
    });

    let resp = (StatusCode::OK, Json(response)).into_response();
    with_cors_response(resp)
}

#[cfg(test)]
mod tests {
    use super::{extract_query, failover_order, resolve_search_provider, SEARCH_FAILOVER_ORDER};

    #[test]
    fn chat_search_failover_order_primary_first() {
        let order = failover_order("exa");
        assert_eq!(order.first(), Some(&"exa"));
        assert_eq!(order.len(), SEARCH_FAILOVER_ORDER.len());
        let mut seen = std::collections::HashSet::new();
        for id in &order {
            assert!(seen.insert(*id), "duplicate failover entry: {id}");
        }
    }

    #[test]
    fn chat_search_failover_order_unknown_primary_is_empty() {
        assert!(failover_order("not-a-provider").is_empty());
    }

    #[test]
    fn chat_search_failover_order_covers_registry_aliases() {
        // Every alias target in resolve_search_provider must appear in the
        // failover order so failover can actually reach it.
        for alias in [
            "serper",
            "brave",
            "exa",
            "tavily",
            "ollama-search",
            "glm",
            "antigravity",
        ] {
            let id = resolve_search_provider(alias).unwrap();
            assert!(
                SEARCH_FAILOVER_ORDER.contains(&id),
                "failover order missing provider id: {id}"
            );
        }
        assert!(extract_query(&serde_json::json!({"query": "q"})).is_some());
    }
}
