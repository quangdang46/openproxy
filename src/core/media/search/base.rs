//! Common types and helpers for search providers.

use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::types::ProviderConnection;

/// Request body shared across providers. Maps directly to OmniRoute's
/// `SearchRequestParams`.
#[derive(Debug, Clone)]
pub struct SearchRequest<'a> {
    pub query: String,
    pub search_type: SearchType,
    pub max_results: u32,
    pub token: Option<&'a str>,
    pub country: Option<String>,
    pub language: Option<String>,
    /// `"day" | "week" | "month" | "year" | "any"`.
    pub time_range: Option<String>,
    pub offset: Option<u32>,
    /// Optionally prefixed with `-` to indicate exclusion.
    pub domain_filter: Vec<String>,
    pub content_options: Option<Value>,
    /// Free-form per-provider knobs (`baseUrl`, `cx`, `depth`, …).
    pub provider_options: BTreeMap<String, Value>,
    pub provider_specific_data: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchType {
    Web,
    News,
}

impl SearchType {
    pub fn as_str(self) -> &'static str {
        match self {
            SearchType::Web => "web",
            SearchType::News => "news",
        }
    }
    pub fn parse(s: Option<&str>) -> Self {
        match s {
            Some("news") => SearchType::News,
            _ => SearchType::Web,
        }
    }
}

/// One unified search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_url: Option<String>,
    pub snippet: String,
    pub position: u32,
    pub score: Option<f64>,
    pub published_at: Option<String>,
    pub favicon_url: Option<String>,
    pub content: Option<Value>,
    pub metadata: Value,
    pub citation: Value,
    pub provider_raw: Option<Value>,
}

/// Response envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultSet {
    pub results: Vec<SearchResult>,
    pub total_results: Option<u64>,
}

/// Trait implemented by every search provider. Builds the upstream
/// request and normalises the response.
pub trait SearchProvider: Send + Sync {
    fn id(&self) -> &'static str;

    /// Whether the upstream is no-auth (searxng with public instance, etc.).
    fn no_auth(&self) -> bool {
        false
    }

    /// Build the upstream URL.
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String>;

    /// Build the headers for the upstream call.
    fn build_headers(&self, request: &SearchRequest<'_>) -> Result<HeaderMap, String>;

    /// HTTP method for the upstream call.
    fn method(&self) -> reqwest::Method {
        reqwest::Method::GET
    }

    /// Optional JSON body (POST providers).
    fn build_body(&self, _request: &SearchRequest<'_>) -> Option<Value> {
        None
    }

    /// Normalise the upstream JSON to [`SearchResultSet`].
    fn normalize(&self, body: &Value, request: &SearchRequest<'_>) -> SearchResultSet;
}

/// Split `domain_filter` into `(includes, excludes)` where excludes are
/// prefixed with `-` in the input.
pub fn parse_domain_filter(filter: &[String]) -> (Vec<String>, Vec<String>) {
    let mut includes = Vec::new();
    let mut excludes = Vec::new();
    for d in filter {
        if let Some(rest) = d.strip_prefix('-') {
            excludes.push(rest.to_string());
        } else {
            includes.push(d.clone());
        }
    }
    (includes, excludes)
}

/// Read a string setting from `provider_options` or `provider_specific_data`.
pub fn get_provider_setting(request: &SearchRequest<'_>, key: &str) -> Option<String> {
    let value = request
        .provider_options
        .get(key)
        .or_else(|| request.provider_specific_data.get(key))?;
    let s = value.as_str()?.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Build a SearchRequest from credentials + JSON body. Useful when the
/// caller has the inbound `/v1/search` request body in hand.
pub fn request_from_body<'a>(
    body: &'a Value,
    credentials: Option<&'a ProviderConnection>,
) -> Result<SearchRequest<'a>, String> {
    let query = body
        .get("query")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Missing required field: query".to_string())?
        .to_string();
    let search_type = SearchType::parse(body.get("search_type").and_then(|v| v.as_str()));
    let max_results = body
        .get("max_results")
        .and_then(|v| v.as_u64())
        .unwrap_or(5)
        .min(100) as u32;
    let token = credentials
        .and_then(|c| c.api_key.as_deref().or(c.access_token.as_deref()))
        .filter(|s| !s.is_empty());
    let country = body
        .get("country")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let language = body
        .get("language")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let time_range = body
        .get("time_range")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let offset = body
        .get("offset")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let domain_filter = body
        .get("domain_filter")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let content_options = body.get("content_options").cloned();
    let provider_options = body
        .get("provider_options")
        .and_then(|v| v.as_object())
        .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    let provider_specific_data = credentials
        .map(|c| c.provider_specific_data.clone())
        .unwrap_or_default();

    Ok(SearchRequest {
        query,
        search_type,
        max_results,
        token,
        country,
        language,
        time_range,
        offset,
        domain_filter,
        content_options,
        provider_options,
        provider_specific_data,
    })
}

/// Strip the URL scheme + `www.` prefix (used by `display_url`).
pub fn make_display_url(url: &str) -> Option<String> {
    if url.is_empty() {
        return None;
    }
    let stripped = url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_start_matches("www.");
    let cleaned = stripped.split('?').next().unwrap_or(stripped);
    Some(cleaned.to_string())
}

/// Resolve the base URL with optional `provider_options.baseUrl` override.
pub fn resolve_base_url(default: &str, request: &SearchRequest<'_>) -> String {
    get_provider_setting(request, "baseUrl")
        .unwrap_or_else(|| default.to_string())
        .trim_end_matches('/')
        .to_string()
}

/// Build a unified [`SearchResult`].
pub fn make_result(
    provider_id: &str,
    title: Option<&str>,
    url: Option<&str>,
    snippet: Option<&str>,
    score: Option<f64>,
    published_at: Option<&str>,
    favicon_url: Option<&str>,
    full_text: Option<&str>,
    text_format: Option<&str>,
    image_url: Option<&str>,
    author: Option<&str>,
    source_type: Option<&str>,
    index: u32,
    now_iso: &str,
) -> SearchResult {
    let url = url.unwrap_or("").to_string();
    let display = make_display_url(&url);
    let content = full_text.map(|t| {
        serde_json::json!({
            "format": text_format.unwrap_or("text"),
            "text": t,
            "length": t.chars().count(),
        })
    });
    SearchResult {
        title: title.unwrap_or("").to_string(),
        url,
        display_url: display,
        snippet: snippet.unwrap_or("").to_string(),
        position: index + 1,
        score: score.map(|s| s.clamp(0.0, 1.0)),
        published_at: published_at.map(str::to_string),
        favicon_url: favicon_url.map(str::to_string),
        content,
        metadata: serde_json::json!({
            "author": author,
            "language": serde_json::Value::Null,
            "source_type": source_type,
            "image_url": image_url,
        }),
        citation: serde_json::json!({
            "provider": provider_id,
            "retrieved_at": now_iso,
            "rank": index + 1,
        }),
        provider_raw: None,
    }
}

/// `chrono::Utc::now().to_rfc3339()` shortcut.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_domain_filter_splits_on_dash() {
        let (inc, exc) = parse_domain_filter(&[
            "example.com".to_string(),
            "-spam.com".to_string(),
            "good.com".to_string(),
        ]);
        assert_eq!(inc, vec!["example.com", "good.com"]);
        assert_eq!(exc, vec!["spam.com"]);
    }

    #[test]
    fn make_display_url_strips_scheme_and_www() {
        assert_eq!(
            make_display_url("https://www.example.com/path?q=x"),
            Some("example.com/path".into())
        );
    }

    #[test]
    fn request_from_body_validates_query() {
        let body = serde_json::json!({});
        assert!(request_from_body(&body, None).is_err());
        let body = serde_json::json!({"query": "  "});
        assert!(request_from_body(&body, None).is_err());
    }

    #[test]
    fn request_from_body_caps_max_results() {
        let body = serde_json::json!({"query": "x", "max_results": 1000});
        let r = request_from_body(&body, None).unwrap();
        assert_eq!(r.max_results, 100);
    }
}
