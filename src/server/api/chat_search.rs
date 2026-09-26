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

use crate::core::combo::{
    execute_combo_strategy_full, get_combo_models_from_data, strategy_for_combo, ComboAttemptError,
    ComboExecutionError, ComboStrategy, ModelCapacity,
};
use crate::core::media::search::{dispatch as search_dispatch, is_search_provider};
use crate::core::proxy::resolve_proxy_target;
use crate::server::auth::require_api_key_with_reload;
use crate::server::state::AppState;
use crate::types::PricingTable;

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

    // The explicit `provider` field is the other half of 9router's
    // `body.provider || body.model` (search.js:33) — the dashboard sends a
    // web-search model under `model` because for webSearch the provider IS the
    // model. Resolution below still prefers `model`, as it always has.
    let provider_field = body
        .get("provider")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let provider = model_str
        .and_then(resolve_search_provider)
        .or_else(|| provider_field.and_then(resolve_search_provider))
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

    // 9router search.js:73-86 tests the provider/model string against the
    // combo table BEFORE resolving a provider id, and hands a hit to
    // handleComboChat with the same strategy + sticky settings chat uses. A
    // combo configured over search providers therefore fans out here; without
    // the branch it is read as a literal provider name and falls through to
    // the `.unwrap_or("serper")` default.
    if let Some(combo_name) = model_str.or(provider_field) {
        if let Some(combo_models) = get_combo_models_from_data(combo_name, &snapshot.combos) {
            let strategy = strategy_for_combo(&snapshot, combo_name);
            let sticky_limit = snapshot.settings.combo_sticky_round_robin_limit.max(1);
            let pricing = PricingTable::new();
            let combo_state = state.clone();
            let combo_search_body = search_body.clone();
            let combo_snapshot = snapshot.clone();
            let result = execute_combo_strategy_full(
                &combo_models,
                Some(combo_name),
                strategy,
                &[],
                sticky_limit,
                None,
                &pricing,
                |_: &str| ModelCapacity::Available,
                move |member: &str| {
                    let state = combo_state.clone();
                    let search_body = combo_search_body.clone();
                    let snapshot = combo_snapshot.clone();
                    let member = member.to_string();
                    async move {
                        // 9router handleSingleProviderSearch answers 400
                        // `Unknown provider: X` for a member that is not a
                        // search provider, which handleComboChat treats as a
                        // member failure and rotates past.
                        let Some(member_provider) = resolve_search_provider(&member) else {
                            return Err(ComboAttemptError {
                                status: 400,
                                message: format!("Unknown provider: {}", member),
                                retry_after: None,
                                upstream_body: None,
                            });
                        };
                        let chain =
                            run_search_chain(&state, &snapshot, member_provider, &search_body)
                                .await;
                        match chain.success {
                            Some(success) => Ok(success),
                            None => Err(chain.into_attempt_error(member_provider)),
                        }
                    }
                },
            )
            .await;

            return match result {
                Ok((results_value, usage_tokens, effective_provider)) => build_search_response(
                    &body,
                    provider,
                    &query,
                    search_type,
                    &results_value,
                    usage_tokens,
                    &effective_provider,
                ),
                Err(ComboExecutionError {
                    status,
                    message,
                    earliest_retry_after: _,
                    upstream_body: _,
                }) => with_cors_response(
                    (
                        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                        Json(json!({
                            "error": {
                                "message": format!("Search failed: {}", message),
                                "type": "server_error",
                                "code": null
                            }
                        })),
                    )
                        .into_response(),
                ),
            };
        }
    }

    let chain = run_search_chain(&state, &snapshot, provider, &search_body).await;
    if !chain.attempted {
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

    let Some((results_value, usage_tokens, effective_provider)) = chain.success else {
        return with_cors_response(
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": {
                        "message": format!("Search failed: {}", chain.last_err_msg.unwrap_or_else(|| "all search providers failed".to_string())),
                        "type": "server_error",
                        "code": chain.last_err_code
                    }
                })),
            )
                .into_response(),
        );
    };

    build_search_response(
        &body,
        provider,
        &query,
        search_type,
        &results_value,
        usage_tokens,
        &effective_provider,
    )
}

/// One provider's search attempt plus every cross-provider failover candidate
/// behind it. Shared by the single-provider path and each combo member, so a
/// member fails (and the combo rotates) with exactly the diagnostics the
/// single-provider path would have produced.
struct SearchChain {
    /// The first provider that answered: raw payload, result count, provider id.
    success: Option<(Value, u64, String)>,
    /// Whether any candidate had a usable connection — `false` means the
    /// provider is simply not configured.
    attempted: bool,
    last_err_msg: Option<String>,
    last_err_code: Option<String>,
}

impl SearchChain {
    /// The failure as a combo member error. "No credentials" stays a 400 so
    /// the combo's own error is the actionable one rather than a blanket 502.
    fn into_attempt_error(self, provider: &str) -> ComboAttemptError {
        if !self.attempted {
            return ComboAttemptError {
                status: 400,
                message: format!(
                    "No active credentials found for search provider: {}",
                    provider
                ),
                retry_after: None,
                upstream_body: None,
            };
        }
        ComboAttemptError {
            status: 502,
            message: self
                .last_err_msg
                .unwrap_or_else(|| "all search providers failed".to_string()),
            retry_after: None,
            upstream_body: None,
        }
    }
}

/// Search one provider, then the remaining registry providers that have an
/// active connection. Returns on the first answer.
async fn run_search_chain(
    state: &AppState,
    snapshot: &crate::types::AppDb,
    provider: &str,
    search_body: &Value,
) -> SearchChain {
    let mut chain = SearchChain {
        success: None,
        attempted: false,
        last_err_msg: None,
        last_err_code: None,
    };

    for candidate in failover_order(provider) {
        let connection = match select_search_connection(snapshot, candidate) {
            Some(c) => c,
            None => continue,
        };
        chain.attempted = true;
        let proxy = resolve_proxy_target(snapshot, &connection, &snapshot.settings);
        let client = match state.client_pool.get(candidate, proxy.as_ref()) {
            Ok(c) => c,
            Err(e) => {
                chain.last_err_msg = Some(format!("Failed to create HTTP client: {}", e));
                chain.last_err_code = Some("server_error".to_string());
                continue;
            }
        };
        match search_dispatch(&client, &connection, candidate, search_body).await {
            Some(Ok(raw_value)) => {
                let results_arr = raw_value
                    .get("results")
                    .and_then(Value::as_array)
                    .map(|a| a.len())
                    .unwrap_or(0);
                chain.success = Some((raw_value, results_arr as u64, candidate.to_string()));
                break;
            }
            Some(Err(err)) => {
                chain.last_err_msg = Some(err.message().to_string());
                chain.last_err_code = Some(format!("search_{}", err.status()));
                continue;
            }
            None => continue,
        }
    }

    chain
}

/// Wrap a provider's results in the chat-completion envelope this endpoint
/// answers with.
fn build_search_response(
    body: &Value,
    fallback_model: &str,
    query: &str,
    search_type: &str,
    results_value: &Value,
    usage_tokens: u64,
    effective_provider: &str,
) -> Response {
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
        "model": body.get("model").and_then(Value::as_str).unwrap_or(fallback_model),
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

    with_cors_response((StatusCode::OK, Json(response)).into_response())
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
