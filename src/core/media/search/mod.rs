//! Web-search provider adapters ported from
//! `open-sse/handlers/search/`.
//!
//! Each provider implements [`SearchProvider`]: build the upstream
//! request from a [`SearchRequest`], then normalise the response into
//! the unified [`SearchResultSet`] shape (matches OmniRoute's schema).
//!
//! Supported providers:
//!   serper, brave-search, perplexity, exa, tavily, google-pse, linkup,
//!   searchapi, youcom, searxng.

mod base;
pub mod handler;
mod providers;

pub use base::{SearchProvider, SearchRequest, SearchResult, SearchResultSet};
pub use handler::{handle_search, SearchHandlerError};

/// Run the search pipeline for `provider` if a matching adapter exists.
/// Returns `None` to fall through to a generic flow.
pub async fn dispatch(
    client: &reqwest::Client,
    credentials: &crate::types::ProviderConnection,
    provider: &str,
    body: &serde_json::Value,
) -> Option<Result<serde_json::Value, super::MediaError>> {
    let provider_impl = get_search_provider(provider)?;
    let request = match base::request_from_body(body, Some(credentials)) {
        Ok(r) => r,
        Err(msg) => return Some(Err(super::MediaError::Validation(msg))),
    };
    Some(
        handle_search(client, provider_impl, &request)
            .await
            .map(|set| serde_json::to_value(set).unwrap_or(serde_json::Value::Null))
            .map_err(Into::into),
    )
}

/// Look up the search adapter for a provider id.
pub fn get_search_provider(provider: &str) -> Option<&'static dyn SearchProvider> {
    providers::lookup(provider)
}

pub fn is_search_provider(provider: &str) -> bool {
    get_search_provider(provider).is_some()
}
