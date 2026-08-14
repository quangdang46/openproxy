//! Search handler — orchestrate one /v1/search call.

use reqwest::Client;
use std::time::Duration;
use thiserror::Error;

use super::base::{SearchProvider, SearchRequest, SearchResultSet};

/// 9router search.js: global timeout is 15s (was 30s in Rust).
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

impl SearchHandlerError {
    pub fn status(&self) -> u16 {
        match self {
            SearchHandlerError::Http(code, _) => *code,
            SearchHandlerError::Validation(_) => 400,
            SearchHandlerError::UnsupportedProvider(_) => 400,
            SearchHandlerError::Upstream(_) => 502,
        }
    }
}

pub async fn handle_search(
    client: &Client,
    provider: &dyn SearchProvider,
    request: &SearchRequest<'_>,
) -> Result<SearchResultSet, SearchHandlerError> {
    let started = std::time::Instant::now();
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

    // 9router: per-provider timeoutMs (default 10000) wins over the 15s
    // global timeout when set.
    let effective_timeout = provider
        .timeout_ms()
        .map(Duration::from_millis)
        .unwrap_or(GLOBAL_TIMEOUT);
    let mut builder = client
        .request(provider.method(), &url)
        .headers(headers)
        .timeout(effective_timeout);
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
        // 9router parity (search/index.js:181-198): on a retriable error
        // (not 400/401/403/404) within the global budget, fall back to a
        // chat-completions LLM search when the provider is configured for it.
        const NON_RETRIABLE: [u16; 4] = [400, 401, 403, 404];
        let retriable = !NON_RETRIABLE.contains(&status);
        if retriable
            && started.elapsed() < GLOBAL_TIMEOUT
            && provider.chat_fallback().is_some()
            && request.token.is_some()
        {
            if let Some(fallback_set) =
                super::chat_search::handle_chat_search(client, provider.id(), request).await
            {
                return Ok(fallback_set);
            }
        }
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
    use crate::core::media::search::base::{
        request_from_body, ChatSearchFallback, SearchRequest, SearchResultSet, SearchType,
    };
    use crate::core::media::search::get_search_provider;

    #[test]
    fn validation_rejects_empty_query() {
        let provider = get_search_provider("serper").unwrap();
        let body = serde_json::json!({"query": ""});
        let res = request_from_body(&body, None);
        assert!(res.is_err()); // request_from_body rejects empty query
        let _ = provider;
    }

    #[test]
    fn timeout_precedence_global_15s_and_provider_10s() {
        // Global default: 15s.
        assert_eq!(GLOBAL_TIMEOUT, Duration::from_secs(15));

        // Providers with an explicit timeoutMs override it.
        let searxng = get_search_provider("searxng").unwrap();
        assert_eq!(searxng.timeout_ms(), Some(10_000));
        let youcom = get_search_provider("youcom").unwrap();
        assert_eq!(youcom.timeout_ms(), Some(10_000));

        // Providers without one fall back to the 15s global.
        let serper = get_search_provider("serper").unwrap();
        assert_eq!(serper.timeout_ms(), None);
        let effective = serper
            .timeout_ms()
            .map(Duration::from_millis)
            .unwrap_or(GLOBAL_TIMEOUT);
        assert_eq!(effective, Duration::from_secs(15));
    }

    /// Test provider that fails the dedicated endpoint but advertises a
    /// chat fallback. Its URL points at a wiremock server.
    struct FailingWithChatFallback {
        base_url: String,
    }

    impl SearchProvider for FailingWithChatFallback {
        fn id(&self) -> &'static str {
            "gemini"
        }
        fn chat_fallback(&self) -> Option<ChatSearchFallback> {
            Some(ChatSearchFallback {
                model: "gemini-2.5-flash",
            })
        }
        fn build_url(&self, _request: &SearchRequest<'_>) -> Result<String, String> {
            Ok(format!("{}/fail", self.base_url))
        }
        fn build_headers(
            &self,
            _request: &SearchRequest<'_>,
        ) -> Result<reqwest::header::HeaderMap, String> {
            Ok(reqwest::header::HeaderMap::new())
        }
        fn normalize(
            &self,
            _body: &serde_json::Value,
            _request: &SearchRequest<'_>,
        ) -> SearchResultSet {
            SearchResultSet {
                results: vec![],
                total_results: Some(0),
            }
        }
        fn method(&self) -> reqwest::Method {
            reqwest::Method::POST
        }
    }

    fn req_with_token(
        query: &'static str,
        token: &'static str,
        base_url: Option<&'static str>,
    ) -> SearchRequest<'static> {
        let mut provider_options = std::collections::BTreeMap::new();
        if let Some(b) = base_url {
            provider_options.insert("baseUrl".to_string(), serde_json::json!(b));
        }
        SearchRequest {
            query: query.to_string(),
            search_type: SearchType::Web,
            max_results: 5,
            token: Some(token),
            country: None,
            language: None,
            time_range: None,
            offset: None,
            domain_filter: vec![],
            content_options: None,
            provider_options,
            provider_specific_data: Default::default(),
        }
    }

    #[tokio::test]
    async fn search_fails_over_to_chat_on_retriable_error() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // Dedicated endpoint → 502 (retriable). The chat fallback endpoint
        // (Gemini generateContent) → 200 with a grounding chunk.
        Mock::given(method("POST"))
            .and(path("/fail"))
            .respond_with(ResponseTemplate::new(502))
            .mount(&server)
            .await;
        // Broad mock first: any POST to the mock root with the grounding
        // payload shape → 200. (Colons in paths are legal; keep this simple.)
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "candidates": [{
                    "content": { "parts": [{ "text": "answer" }] },
                    "groundingMetadata": {
                        "groundingChunks": [
                            { "web": { "uri": "https://example.com", "title": "Example" } }
                        ]
                    }
                }],
                "usageMetadata": { "totalTokenCount": 5 }
            })))
            .mount(&server)
            .await;

        let provider = FailingWithChatFallback {
            base_url: server.uri(),
        };
        let client = reqwest::Client::new();
        let request = req_with_token(
            "hello",
            "tok-123",
            Some(Box::leak(server.uri().into_boxed_str())),
        );

        let result = handle_search(&client, &provider, &request).await;
        assert!(
            result.is_ok(),
            "retriable 502 should fall back to chat search: {result:?}"
        );
        let set = result.unwrap();
        assert_eq!(set.results.len(), 1);
        assert_eq!(set.results[0].url, "https://example.com");
    }

    #[tokio::test]
    async fn search_does_not_failover_on_404() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // Dedicated endpoint → 404 (non-retriable). No chat fallback mock
        // mounted — if the fallback were invoked it would hit the real
        // generativelanguage API and fail the test.
        Mock::given(method("POST"))
            .and(path("/fail"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let provider = FailingWithChatFallback {
            base_url: server.uri(),
        };
        let client = reqwest::Client::new();
        let request = req_with_token("hello", "tok-123", None);

        let result = handle_search(&client, &provider, &request).await;
        let err = result.unwrap_err();
        assert_eq!(err.status(), 404, "404 must not trigger the chat fallback");
    }
}
