//! Web-search provider adapters ported from
//! `open-sse/handlers/search/`.
//!
//! Each provider implements [`SearchProvider`]: build the upstream
//! request from a [`SearchRequest`], then normalise the response into
//! the unified [`SearchResultSet`] shape (matches OmniRoute's schema).
//!
//! Supported providers:
//!   serper, serpingapi, brave-search, perplexity, exa, tavily, google-pse, linkup,
//!   searchapi, youcom, searxng, xquik, ollama-search, glm.
//!
//! Chat-based LLM search (`searchViaChat` in the 9router registry) is
//! covered in [`chat_search`]: gemini, antigravity, openai, xai, kimi,
//! minimax, perplexity.

mod base;
mod chat_search;
pub mod handler;
mod providers;

pub use base::{
    assert_public_url, assert_public_url_resolved, fetch_public, ChatSearchResult, SearchProvider,
    SearchRequest, SearchResult, SearchResultSet,
};
pub use chat_search::{handle_chat_search, has_chat_search};
pub use handler::{handle_search, handle_search_value, SearchHandlerError};

/// Run the search pipeline for `provider` if a matching adapter exists.
/// Returns `None` to fall through to a generic flow.
///
/// Port of `open-sse/handlers/search/index.js handleSearchCore` routing:
/// a dedicated `searchConfig` adapter wins; otherwise a `searchViaChat`
/// provider falls back to a chat-completions grounding call (chatSearch.js).
pub async fn dispatch(
    client: &reqwest::Client,
    credentials: &crate::types::ProviderConnection,
    provider: &str,
    body: &serde_json::Value,
) -> Option<Result<serde_json::Value, super::MediaError>> {
    let Some(provider_impl) = get_search_provider(provider) else {
        return dispatch_chat_search(client, credentials, provider, body).await;
    };
    let mut request = match base::request_from_body(body, Some(credentials)) {
        Ok(r) => r,
        Err(msg) => return Some(Err(super::MediaError::Validation(msg))),
    };
    // 9router parity (registry maxMaxResults): clamp to the provider cap
    // (searxng 50, youcom/default 100) after the default is resolved.
    request.max_results = request.max_results.min(provider_impl.max_max_results());
    Some(
        handle_search_value(client, provider_impl, &request)
            .await
            .map_err(Into::into),
    )
}

/// Dedicated search API lookup miss: try the `searchViaChat` chat-based LLM
/// search (port of `handleSearchCore` steps 2-3 in index.js:165-183).
///
/// Returns `None` when the provider has no chat-search config so the caller
/// keeps its "unsupported provider" behavior.
async fn dispatch_chat_search(
    client: &reqwest::Client,
    credentials: &crate::types::ProviderConnection,
    provider: &str,
    body: &serde_json::Value,
) -> Option<Result<serde_json::Value, super::MediaError>> {
    if !has_chat_search(provider) {
        return None;
    }
    let request = match base::request_from_body(body, Some(credentials)) {
        Ok(r) => r,
        Err(msg) => return Some(Err(super::MediaError::Validation(msg))),
    };
    let Some(chat) = handle_chat_search(client, provider, &request).await else {
        return Some(Err(super::MediaError::Validation(format!(
            "Provider {provider} does not support web search"
        ))));
    };
    // Port of `handleChatSearch` success payload (chatSearch.js:534-549):
    // data: { provider, query, results, answer, usage, metrics, errors }.
    let mut out = serde_json::to_value(&chat.set).unwrap_or(serde_json::Value::Null);
    if let Some(obj) = out.as_object_mut() {
        obj.insert(
            "provider".to_string(),
            serde_json::Value::String(provider.to_string()),
        );
        obj.insert(
            "query".to_string(),
            serde_json::Value::String(request.query.clone()),
        );
        obj.insert(
            "answer".to_string(),
            serde_json::json!({
                "source": provider,
                "text": chat.answer_text,
                "model": chat.model,
            }),
        );
        obj.insert(
            "usage".to_string(),
            serde_json::json!({
                "queries_used": 1,
                "search_cost_usd": 0,
                "llm_tokens": chat.llm_tokens,
            }),
        );
        obj.insert(
            "metrics".to_string(),
            serde_json::json!({
                "response_time_ms": serde_json::Value::Null,
                "upstream_latency_ms": serde_json::Value::Null,
                "total_results_available": serde_json::Value::Null,
            }),
        );
        obj.insert("errors".to_string(), serde_json::json!([]));
    }
    Some(Ok(out))
}

/// Look up the search adapter for a provider id.
pub fn get_search_provider(provider: &str) -> Option<&'static dyn SearchProvider> {
    providers::lookup(provider)
}

pub fn is_search_provider(provider: &str) -> bool {
    get_search_provider(provider).is_some()
}
