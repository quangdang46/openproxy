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

/// Look up the search adapter for a provider id.
pub fn get_search_provider(provider: &str) -> Option<&'static dyn SearchProvider> {
    providers::lookup(provider)
}

pub fn is_search_provider(provider: &str) -> bool {
    get_search_provider(provider).is_some()
}
