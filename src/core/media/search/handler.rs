//! Search handler — orchestrate one /v1/search call.

use reqwest::Client;
use std::time::Duration;
use thiserror::Error;

use super::base::{SearchProvider, SearchRequest, SearchResultSet};

const GLOBAL_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Error)]
pub enum SearchHandlerError {
    #[error("HTTP {0}: {1}")]
    Http(u16, String),
    #[error("validation: {0}")]
    Validation(String),
    #[error("provider {0} not supported for search")]
    UnsupportedProvider(String),
    #[error("upstream: {0}")]
    Upstream(String),
}

pub async fn handle_search(
    client: &Client,
    provider: &dyn SearchProvider,
    request: &SearchRequest<'_>,
) -> Result<SearchResultSet, SearchHandlerError> {
    if request.query.is_empty() {
        return Err(SearchHandlerError::Validation("Query is empty".into()));
    }
    if !provider.no_auth() && request.token.is_none() {
        return Err(SearchHandlerError::Validation(format!(
            "{} requires an API key",
            provider.id()
        )));
    }
    let url = provider
        .build_url(request)
        .map_err(SearchHandlerError::Validation)?;
    let headers = provider
        .build_headers(request)
        .map_err(SearchHandlerError::Validation)?;

    let mut builder = client
        .request(provider.method(), &url)
        .headers(headers)
        .timeout(GLOBAL_TIMEOUT);
    if let Some(body) = provider.build_body(request) {
        builder = builder.json(&body);
    }

    let res = builder
        .send()
        .await
        .map_err(|e| SearchHandlerError::Upstream(e.to_string()))?;
    if !res.status().is_success() {
        let status = res.status().as_u16();
        let text = res.text().await.unwrap_or_default();
        return Err(SearchHandlerError::Http(status, text));
    }
    let body: serde_json::Value = res
        .json()
        .await
        .map_err(|e| SearchHandlerError::Upstream(format!("parse json: {e}")))?;
    Ok(provider.normalize(&body, request))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::media::search::base::request_from_body;
    use crate::core::media::search::get_search_provider;

    #[test]
    fn validation_rejects_empty_query() {
        let provider = get_search_provider("serper").unwrap();
        let body = serde_json::json!({"query": ""});
        let res = request_from_body(&body, None);
        assert!(res.is_err()); // request_from_body rejects empty query
        let _ = provider;
    }
}
