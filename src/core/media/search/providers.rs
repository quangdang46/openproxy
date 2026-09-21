//! Concrete `SearchProvider` impls for the 14 supported providers.
//!
//! Builder + normalizer pairs from `open-sse/handlers/search/{callers,normalizers}.js`.

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Method;
use serde_json::{json, Value};

use super::base::{
    get_provider_setting, make_result, now_iso, parse_domain_filter, resolve_base_url,
    SearchProvider, SearchRequest, SearchResult, SearchResultSet, SearchType,
};

pub fn lookup(id: &str) -> Option<&'static dyn SearchProvider> {
    Some(match id {
        "serper" => &SERPER,
        "serpingapi" => &SERPINGAPI,
        "brave-search" => &BRAVE,
        "perplexity" => &PERPLEXITY,
        "exa" => &EXA,
        "tavily" => &TAVILY,
        "google-pse" => &GOOGLE_PSE,
        "linkup" => &LINKUP,
        "searchapi" => &SEARCH_API,
        "youcom" => &YOUCOM,
        "searxng" => &SEARXNG,
        "xquik" => &XQUIK,
        "ollama-search" => &OLLAMA_SEARCH,
        "glm" => &GLM,
        _ => return None,
    })
}

fn json_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    h
}

fn accept_json() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(ACCEPT, HeaderValue::from_static("application/json"));
    h
}

fn require_token<'a>(request: &SearchRequest<'a>, provider: &str) -> Result<&'a str, String> {
    request
        .token
        .ok_or_else(|| format!("{provider} requires an API key"))
}

fn page_number(offset: Option<u32>, max_results: u32) -> Option<u32> {
    let offset = offset.filter(|&o| o > 0)?;
    if max_results == 0 {
        return None;
    }
    Some(offset / max_results + 1)
}

// ─── serper ──────────────────────────────────────────────────────────────

pub struct SerperProvider;
pub static SERPER: SerperProvider = SerperProvider;
impl SearchProvider for SerperProvider {
    fn id(&self) -> &'static str {
        "serper"
    }
    fn timeout_ms(&self) -> Option<u64> {
        // 9router registry serper.js timeoutMs = 10000.
        Some(10_000)
    }
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        let endpoint = if request.search_type == SearchType::News {
            "/news"
        } else {
            "/search"
        };
        Ok(format!(
            "{}{endpoint}",
            resolve_base_url("https://google.serper.dev", request)?
        ))
    }
    fn build_headers(&self, request: &SearchRequest<'_>) -> Result<HeaderMap, String> {
        let token = require_token(request, "serper")?;
        let mut h = json_headers();
        h.insert(
            "X-API-Key",
            HeaderValue::from_str(token).map_err(|e| e.to_string())?,
        );
        Ok(h)
    }
    fn method(&self) -> Method {
        Method::POST
    }
    fn build_body(&self, request: &SearchRequest<'_>) -> Option<Value> {
        let mut body = json!({"q": request.query, "num": request.max_results});
        if let Some(c) = &request.country {
            body["gl"] = json!(c.to_lowercase());
        }
        if let Some(l) = &request.language {
            body["hl"] = json!(l);
        }
        Some(body)
    }
    fn normalize(&self, body: &Value, request: &SearchRequest<'_>) -> SearchResultSet {
        let now = now_iso();
        let key = if request.search_type == SearchType::News {
            "news"
        } else {
            "organic"
        };
        let items = body
            .get(key)
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let results: Vec<SearchResult> = items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                make_result(
                    "serper",
                    item.get("title").and_then(|v| v.as_str()),
                    item.get("link").and_then(|v| v.as_str()),
                    item.get("snippet")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("description").and_then(|v| v.as_str())),
                    None,
                    item.get("date").and_then(|v| v.as_str()),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    idx as u32,
                    &now,
                )
            })
            .collect();
        let total = body
            .pointer("/searchParameters/totalResults")
            .and_then(|v| v.as_u64());
        SearchResultSet {
            results,
            total_results: total,
        }
    }
}

// ─── serpingapi ──────────────────────────────────────────────────────────

pub struct SerpingApiProvider;
pub static SERPINGAPI: SerpingApiProvider = SerpingApiProvider;
impl SearchProvider for SerpingApiProvider {
    fn id(&self) -> &'static str {
        "serpingapi"
    }
    fn timeout_ms(&self) -> Option<u64> {
        Some(10_000)
    }
    fn max_max_results(&self) -> u32 {
        100
    }
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        if request.search_type == SearchType::News {
            return Err("serpingapi does not support news search".to_string());
        }
        Ok(format!(
            "{}/v1/search",
            resolve_base_url("https://api.serpingapi.com", request)?
        ))
    }
    fn build_headers(&self, request: &SearchRequest<'_>) -> Result<HeaderMap, String> {
        let token = require_token(request, "serpingapi")?;
        let mut h = json_headers();
        h.insert(
            "X-API-Key",
            HeaderValue::from_str(token).map_err(|e| e.to_string())?,
        );
        Ok(h)
    }
    fn method(&self) -> Method {
        Method::POST
    }
    fn build_body(&self, request: &SearchRequest<'_>) -> Option<Value> {
        let mut body = json!({"q": request.query, "num": request.max_results});
        if let Some(c) = &request.country {
            body["gl"] = json!(c.to_lowercase());
        }
        if let Some(l) = &request.language {
            body["hl"] = json!(l);
        }
        if let Some(t) = request.time_range.as_deref() {
            let tbs = match t {
                "day" => Some("qdr:d"),
                "week" => Some("qdr:w"),
                "month" => Some("qdr:m"),
                "year" => Some("qdr:y"),
                _ => None,
            };
            if let Some(tbs) = tbs {
                body["tbs"] = json!(tbs);
            }
        }
        if let Some(page) = page_number(request.offset, request.max_results) {
            body["page"] = json!(page);
        }
        if let Some(location) = get_provider_setting(request, "location") {
            body["location"] = json!(location);
        }
        Some(body)
    }
    fn normalize(&self, body: &Value, _request: &SearchRequest<'_>) -> SearchResultSet {
        let now = now_iso();
        let items = body
            .get("organic")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let results: Vec<SearchResult> = items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                make_result(
                    "serpingapi",
                    item.get("title").and_then(|v| v.as_str()),
                    item.get("link").and_then(|v| v.as_str()),
                    item.get("snippet").and_then(|v| v.as_str()),
                    None,
                    item.get("date").and_then(|v| v.as_str()),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    idx as u32,
                    &now,
                )
            })
            .collect();
        SearchResultSet {
            results,
            total_results: None,
        }
    }
}

// ─── brave-search ────────────────────────────────────────────────────────

pub struct BraveProvider;
pub static BRAVE: BraveProvider = BraveProvider;
impl SearchProvider for BraveProvider {
    fn id(&self) -> &'static str {
        "brave-search"
    }
    fn timeout_ms(&self) -> Option<u64> {
        // 9router registry brave-search.js timeoutMs = 10000.
        Some(10_000)
    }
    fn max_max_results(&self) -> u32 {
        // 9router registry brave-search.js maxMaxResults = 20.
        20
    }
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        let endpoint = if request.search_type == SearchType::News {
            "/news/search"
        } else {
            "/web/search"
        };
        let mut qp = vec![
            ("q", request.query.clone()),
            ("count", request.max_results.to_string()),
        ];
        if let Some(c) = &request.country {
            qp.push(("country", c.clone()));
        }
        if let Some(l) = &request.language {
            qp.push(("search_lang", l.clone()));
        }
        Ok(format!(
            "{}{endpoint}?{}",
            resolve_base_url("https://api.search.brave.com/res/v1", request)?,
            serde_urlencoded::to_string(&qp).unwrap_or_default()
        ))
    }
    fn build_headers(&self, request: &SearchRequest<'_>) -> Result<HeaderMap, String> {
        let token = require_token(request, "brave-search")?;
        let mut h = accept_json();
        h.insert(
            "X-Subscription-Token",
            HeaderValue::from_str(token).map_err(|e| e.to_string())?,
        );
        Ok(h)
    }
    fn normalize(&self, body: &Value, request: &SearchRequest<'_>) -> SearchResultSet {
        let now = now_iso();
        let container = if request.search_type == SearchType::News {
            body.get("news").or(Some(body))
        } else {
            body.get("web")
        };
        let items = container
            .and_then(|c| c.get("results"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let results: Vec<SearchResult> = items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                let favicon = item
                    .pointer("/meta_url/favicon")
                    .and_then(|v| v.as_str())
                    .or_else(|| item.get("favicon").and_then(|v| v.as_str()));
                make_result(
                    "brave-search",
                    item.get("title").and_then(|v| v.as_str()),
                    item.get("url").and_then(|v| v.as_str()),
                    item.get("description").and_then(|v| v.as_str()),
                    None,
                    item.get("page_age")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("age").and_then(|v| v.as_str())),
                    favicon,
                    None,
                    None,
                    None,
                    None,
                    None,
                    idx as u32,
                    &now,
                )
            })
            .collect();
        let total = container
            .and_then(|c| c.get("totalCount"))
            .and_then(|v| v.as_u64());
        SearchResultSet {
            results,
            total_results: total,
        }
    }
}

// ─── perplexity ──────────────────────────────────────────────────────────

pub struct PerplexityProvider;
pub static PERPLEXITY: PerplexityProvider = PerplexityProvider;
impl SearchProvider for PerplexityProvider {
    fn id(&self) -> &'static str {
        "perplexity"
    }
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        resolve_base_url("https://api.perplexity.ai/search", request)
    }
    fn build_headers(&self, request: &SearchRequest<'_>) -> Result<HeaderMap, String> {
        let token = require_token(request, "perplexity")?;
        let mut h = json_headers();
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|e| e.to_string())?,
        );
        Ok(h)
    }
    fn method(&self) -> Method {
        Method::POST
    }
    fn build_body(&self, request: &SearchRequest<'_>) -> Option<Value> {
        let mut body = json!({"query": request.query, "max_results": request.max_results});
        if let Some(c) = &request.country {
            body["country"] = json!(c);
        }
        if let Some(l) = &request.language {
            body["search_language_filter"] = json!([l]);
        }
        if !request.domain_filter.is_empty() {
            body["search_domain_filter"] = json!(request.domain_filter.clone());
        }
        Some(body)
    }
    fn normalize(&self, body: &Value, _request: &SearchRequest<'_>) -> SearchResultSet {
        let now = now_iso();
        let items = body
            .get("results")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let results: Vec<SearchResult> = items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                make_result(
                    "perplexity",
                    item.get("title").and_then(|v| v.as_str()),
                    item.get("url").and_then(|v| v.as_str()),
                    item.get("snippet").and_then(|v| v.as_str()),
                    None,
                    item.get("date")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("last_updated").and_then(|v| v.as_str())),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    idx as u32,
                    &now,
                )
            })
            .collect();
        let total = results.len() as u64;
        SearchResultSet {
            results,
            total_results: Some(total),
        }
    }
}

// ─── exa ─────────────────────────────────────────────────────────────────

pub struct ExaProvider;
pub static EXA: ExaProvider = ExaProvider;
impl SearchProvider for ExaProvider {
    fn id(&self) -> &'static str {
        "exa"
    }
    fn timeout_ms(&self) -> Option<u64> {
        // 9router registry exa.js searchConfig timeoutMs = 10000.
        Some(10_000)
    }
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        resolve_base_url("https://api.exa.ai/search", request)
    }
    fn build_headers(&self, request: &SearchRequest<'_>) -> Result<HeaderMap, String> {
        let token = require_token(request, "exa")?;
        let mut h = json_headers();
        h.insert(
            "x-api-key",
            HeaderValue::from_str(token).map_err(|e| e.to_string())?,
        );
        Ok(h)
    }
    fn method(&self) -> Method {
        Method::POST
    }
    fn build_body(&self, request: &SearchRequest<'_>) -> Option<Value> {
        let (includes, excludes) = parse_domain_filter(&request.domain_filter);
        let mut body = json!({
            "query": request.query,
            "numResults": request.max_results,
            "type": "auto",
            "text": true,
            "highlights": true,
        });
        if !includes.is_empty() {
            body["includeDomains"] = json!(includes);
        }
        if !excludes.is_empty() {
            body["excludeDomains"] = json!(excludes);
        }
        if request.search_type == SearchType::News {
            body["category"] = json!("news");
        }
        Some(body)
    }
    fn normalize(&self, body: &Value, _request: &SearchRequest<'_>) -> SearchResultSet {
        let now = now_iso();
        let items = body
            .get("results")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let results: Vec<SearchResult> = items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                let snippet_owned = item
                    .pointer("/highlights/0")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| {
                        item.get("text")
                            .and_then(|v| v.as_str())
                            .map(|t| t.chars().take(300).collect())
                    });
                make_result(
                    "exa",
                    item.get("title").and_then(|v| v.as_str()),
                    item.get("url").and_then(|v| v.as_str()),
                    snippet_owned.as_deref(),
                    item.get("score").and_then(|v| v.as_f64()),
                    item.get("publishedDate").and_then(|v| v.as_str()),
                    item.get("favicon").and_then(|v| v.as_str()),
                    item.get("text").and_then(|v| v.as_str()),
                    Some("text"),
                    item.get("image").and_then(|v| v.as_str()),
                    item.get("author").and_then(|v| v.as_str()),
                    None,
                    idx as u32,
                    &now,
                )
            })
            .collect();
        let total = results.len() as u64;
        SearchResultSet {
            results,
            total_results: Some(total),
        }
    }
}

// ─── tavily ──────────────────────────────────────────────────────────────

pub struct TavilyProvider;
pub static TAVILY: TavilyProvider = TavilyProvider;
impl SearchProvider for TavilyProvider {
    fn id(&self) -> &'static str {
        "tavily"
    }
    fn timeout_ms(&self) -> Option<u64> {
        // 9router registry tavily.js searchConfig timeoutMs = 10000.
        Some(10_000)
    }
    fn max_max_results(&self) -> u32 {
        // 9router registry tavily.js searchConfig maxMaxResults = 20.
        20
    }
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        resolve_base_url("https://api.tavily.com/search", request)
    }
    fn build_headers(&self, request: &SearchRequest<'_>) -> Result<HeaderMap, String> {
        let token = require_token(request, "tavily")?;
        let mut h = json_headers();
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|e| e.to_string())?,
        );
        Ok(h)
    }
    fn method(&self) -> Method {
        Method::POST
    }
    fn build_body(&self, request: &SearchRequest<'_>) -> Option<Value> {
        let (includes, excludes) = parse_domain_filter(&request.domain_filter);
        let mut body = json!({
            "query": request.query,
            "max_results": request.max_results,
            "topic": if request.search_type == SearchType::News { "news" } else { "general" },
        });
        if !includes.is_empty() {
            body["include_domains"] = json!(includes);
        }
        if !excludes.is_empty() {
            body["exclude_domains"] = json!(excludes);
        }
        if let Some(c) = &request.country {
            body["country"] = json!(c);
        }
        Some(body)
    }
    fn normalize(&self, body: &Value, _request: &SearchRequest<'_>) -> SearchResultSet {
        let now = now_iso();
        let items = body
            .get("results")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let results: Vec<SearchResult> = items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                make_result(
                    "tavily",
                    item.get("title").and_then(|v| v.as_str()),
                    item.get("url").and_then(|v| v.as_str()),
                    item.get("content").and_then(|v| v.as_str()),
                    item.get("score").and_then(|v| v.as_f64()),
                    item.get("published_date").and_then(|v| v.as_str()),
                    None,
                    item.get("raw_content").and_then(|v| v.as_str()),
                    Some("text"),
                    None,
                    None,
                    None,
                    idx as u32,
                    &now,
                )
            })
            .collect();
        let total = results.len() as u64;
        SearchResultSet {
            results,
            total_results: Some(total),
        }
    }
}

// ─── google-pse ──────────────────────────────────────────────────────────

pub struct GooglePseProvider;
pub static GOOGLE_PSE: GooglePseProvider = GooglePseProvider;
impl SearchProvider for GooglePseProvider {
    fn id(&self) -> &'static str {
        "google-pse"
    }
    fn timeout_ms(&self) -> Option<u64> {
        // 9router registry google-pse.js timeoutMs = 10000.
        Some(10_000)
    }
    fn max_max_results(&self) -> u32 {
        // 9router registry google-pse.js maxMaxResults = 10.
        10
    }
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        let api_key = request
            .token
            .ok_or_else(|| "Google Programmable Search requires an API key".to_string())?;
        let cx = get_provider_setting(request, "cx")
            .ok_or_else(|| "Google Programmable Search requires both apiKey and cx".to_string())?;
        let mut qp = vec![
            ("key", api_key.to_string()),
            ("cx", cx),
            ("q", request.query.clone()),
            ("num", request.max_results.min(10).to_string()),
        ];
        if let Some(c) = &request.country {
            qp.push(("gl", c.to_lowercase()));
        }
        if let Some(l) = &request.language {
            qp.push(("hl", l.clone()));
        }
        if let Some(t) = request.time_range.as_deref().filter(|t| *t != "any") {
            let v = match t {
                "day" => "d1",
                "week" => "w1",
                "month" => "m1",
                "year" => "y1",
                _ => "",
            };
            if !v.is_empty() {
                qp.push(("dateRestrict", v.to_string()));
            }
        }
        if let Some(o) = request.offset.filter(|&o| o > 0) {
            qp.push(("start", (o + 1).min(91).to_string()));
        }
        Ok(format!(
            "{}?{}",
            resolve_base_url(
                "https://customsearch.googleapis.com/customsearch/v1",
                request
            )?,
            serde_urlencoded::to_string(&qp).unwrap_or_default()
        ))
    }
    fn build_headers(&self, _request: &SearchRequest<'_>) -> Result<HeaderMap, String> {
        Ok(accept_json())
    }
    fn normalize(&self, body: &Value, _request: &SearchRequest<'_>) -> SearchResultSet {
        let now = now_iso();
        let items = body
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let results: Vec<SearchResult> = items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                let img = item
                    .pointer("/pagemap/cse_image/0/src")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        item.pointer("/pagemap/cse_thumbnail/0/src")
                            .and_then(|v| v.as_str())
                    })
                    .or_else(|| {
                        item.pointer("/pagemap/metatags/0/og:image")
                            .and_then(|v| v.as_str())
                    });
                make_result(
                    "google-pse",
                    item.get("title").and_then(|v| v.as_str()),
                    item.get("link").and_then(|v| v.as_str()),
                    item.get("snippet").and_then(|v| v.as_str()),
                    None,
                    None,
                    None,
                    None,
                    None,
                    img,
                    None,
                    None,
                    idx as u32,
                    &now,
                )
            })
            .collect();
        let total = body
            .pointer("/searchInformation/totalResults")
            .or_else(|| body.pointer("/queries/request/0/totalResults"))
            .and_then(|v| {
                v.as_str()
                    .and_then(|s| s.parse::<u64>().ok())
                    .or_else(|| v.as_u64())
            });
        SearchResultSet {
            results,
            total_results: total,
        }
    }
}

// ─── linkup ──────────────────────────────────────────────────────────────

pub struct LinkupProvider;
pub static LINKUP: LinkupProvider = LinkupProvider;
impl SearchProvider for LinkupProvider {
    fn id(&self) -> &'static str {
        "linkup"
    }
    fn timeout_ms(&self) -> Option<u64> {
        // 9router registry linkup.js timeoutMs = 10000.
        Some(10_000)
    }
    fn max_max_results(&self) -> u32 {
        // 9router registry linkup.js maxMaxResults = 50.
        50
    }
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        resolve_base_url("https://api.linkup.so/v1/search", request)
    }
    fn build_headers(&self, request: &SearchRequest<'_>) -> Result<HeaderMap, String> {
        let token = require_token(request, "linkup")?;
        let mut h = json_headers();
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|e| e.to_string())?,
        );
        Ok(h)
    }
    fn method(&self) -> Method {
        Method::POST
    }
    fn build_body(&self, request: &SearchRequest<'_>) -> Option<Value> {
        let (includes, excludes) = parse_domain_filter(&request.domain_filter);
        let depth = get_provider_setting(request, "depth")
            .filter(|d| ["fast", "standard", "deep"].contains(&d.as_str()))
            .unwrap_or_else(|| "standard".to_string());
        let mut body = json!({
            "q": request.query,
            "depth": depth,
            "outputType": "searchResults",
            "maxResults": request.max_results,
        });
        if !includes.is_empty() {
            body["includeDomains"] = json!(includes);
        }
        if !excludes.is_empty() {
            body["excludeDomains"] = json!(excludes);
        }
        if let Some(t) = request.time_range.as_deref().filter(|t| *t != "any") {
            let now = chrono::Utc::now();
            let from = match t {
                "day" => now - chrono::Duration::days(1),
                "week" => now - chrono::Duration::weeks(1),
                "month" => now - chrono::Duration::days(30),
                "year" => now - chrono::Duration::days(365),
                _ => now,
            };
            body["fromDate"] = json!(from.format("%Y-%m-%d").to_string());
            body["toDate"] = json!(now.format("%Y-%m-%d").to_string());
        }
        Some(body)
    }
    fn normalize(&self, body: &Value, _request: &SearchRequest<'_>) -> SearchResultSet {
        let now = now_iso();
        let items = body
            .get("results")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let results: Vec<SearchResult> = items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                make_result(
                    "linkup",
                    item.get("name")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("title").and_then(|v| v.as_str())),
                    item.get("url").and_then(|v| v.as_str()),
                    item.get("content")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("snippet").and_then(|v| v.as_str())),
                    None,
                    None,
                    None,
                    item.get("content").and_then(|v| v.as_str()),
                    Some("text"),
                    item.get("image_url")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("imageUrl").and_then(|v| v.as_str())),
                    None,
                    item.get("type").and_then(|v| v.as_str()).or(Some("web")),
                    idx as u32,
                    &now,
                )
            })
            .collect();
        let total = results.len() as u64;
        SearchResultSet {
            results,
            total_results: Some(total),
        }
    }
}

// ─── searchapi ───────────────────────────────────────────────────────────

pub struct SearchApiProvider;
pub static SEARCH_API: SearchApiProvider = SearchApiProvider;
impl SearchProvider for SearchApiProvider {
    fn id(&self) -> &'static str {
        "searchapi"
    }
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        let api_key = require_token(request, "searchapi")?;
        let mut qp = vec![
            (
                "engine",
                if request.search_type == SearchType::News {
                    "google_news".to_string()
                } else {
                    "google".to_string()
                },
            ),
            ("q", request.query.clone()),
            ("api_key", api_key.to_string()),
        ];
        if let Some(c) = &request.country {
            qp.push(("gl", c.to_lowercase()));
        }
        if let Some(l) = &request.language {
            qp.push(("hl", l.clone()));
        }
        if let Some(p) = page_number(request.offset, request.max_results) {
            qp.push(("page", p.to_string()));
        }
        Ok(format!(
            "{}?{}",
            resolve_base_url("https://www.searchapi.io/api/v1/search", request)?,
            serde_urlencoded::to_string(&qp).unwrap_or_default()
        ))
    }
    fn build_headers(&self, _request: &SearchRequest<'_>) -> Result<HeaderMap, String> {
        Ok(accept_json())
    }
    fn normalize(&self, body: &Value, _request: &SearchRequest<'_>) -> SearchResultSet {
        let now = now_iso();
        let items = body
            .get("organic_results")
            .and_then(|v| v.as_array())
            .cloned()
            .or_else(|| body.get("top_stories").and_then(|v| v.as_array()).cloned())
            .unwrap_or_default();
        let results: Vec<SearchResult> = items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                make_result(
                    "searchapi",
                    item.get("title").and_then(|v| v.as_str()),
                    item.get("link").and_then(|v| v.as_str()),
                    item.get("snippet")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("description").and_then(|v| v.as_str())),
                    None,
                    item.get("date")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("published_at").and_then(|v| v.as_str())),
                    item.get("favicon").and_then(|v| v.as_str()),
                    None,
                    None,
                    item.get("thumbnail").and_then(|v| v.as_str()),
                    item.get("source").and_then(|v| v.as_str()),
                    None,
                    idx as u32,
                    &now,
                )
            })
            .collect();
        let total = body
            .pointer("/search_information/total_results")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
            });
        let len = results.len() as u64;
        SearchResultSet {
            results,
            total_results: total.or(Some(len)),
        }
    }
}

// ─── youcom ──────────────────────────────────────────────────────────────

pub struct YouComProvider;
pub static YOUCOM: YouComProvider = YouComProvider;
impl SearchProvider for YouComProvider {
    fn id(&self) -> &'static str {
        "youcom"
    }
    fn timeout_ms(&self) -> Option<u64> {
        // 9router registry timeoutMs for youcom.
        Some(10_000)
    }
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        let _ = require_token(request, "youcom")?;
        let (includes, excludes) = parse_domain_filter(&request.domain_filter);
        let mut qp = vec![
            ("query", request.query.clone()),
            ("count", request.max_results.min(100).to_string()),
        ];
        if let Some(t) = request.time_range.as_deref().filter(|t| *t != "any") {
            qp.push(("freshness", t.to_string()));
        }
        if let (Some(o), m) = (request.offset, request.max_results) {
            if o > 0 && m > 0 {
                qp.push(("offset", ((o / m).min(9)).to_string()));
            }
        }
        if let Some(c) = &request.country {
            qp.push(("country", c.clone()));
        }
        if let Some(l) = &request.language {
            qp.push(("language", l.clone()));
        }
        if !includes.is_empty() {
            qp.push(("include_domains", includes.join(",")));
        }
        if !excludes.is_empty() {
            qp.push(("exclude_domains", excludes.join(",")));
        }
        if let Some(co) = request.content_options.as_ref() {
            if co
                .get("full_page")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                qp.push((
                    "livecrawl",
                    if request.search_type == SearchType::News {
                        "news".to_string()
                    } else {
                        "web".to_string()
                    },
                ));
                let fmt = if co.get("format").and_then(|v| v.as_str()) == Some("markdown") {
                    "markdown"
                } else {
                    "html"
                };
                qp.push(("livecrawl_formats", fmt.to_string()));
            }
        }
        Ok(format!(
            "{}?{}",
            resolve_base_url("https://ydc-index.io/v1/search", request)?,
            serde_urlencoded::to_string(&qp).unwrap_or_default()
        ))
    }
    fn build_headers(&self, request: &SearchRequest<'_>) -> Result<HeaderMap, String> {
        let token = require_token(request, "youcom")?;
        let mut h = accept_json();
        h.insert(
            "X-API-Key",
            HeaderValue::from_str(token).map_err(|e| e.to_string())?,
        );
        Ok(h)
    }
    fn normalize(&self, body: &Value, request: &SearchRequest<'_>) -> SearchResultSet {
        let now = now_iso();
        let container = body.get("results");
        let key = if request.search_type == SearchType::News {
            "news"
        } else {
            "web"
        };
        let items = container
            .and_then(|c| c.get(key))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let results: Vec<SearchResult> = items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                let snippet_owned = item
                    .get("snippets")
                    .and_then(|v| v.as_array())
                    .and_then(|arr| arr.iter().find_map(|v| v.as_str()))
                    .map(str::to_string)
                    .or_else(|| {
                        item.get("description")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    });
                let livecrawl_text = item
                    .get("markdown")
                    .and_then(|v| v.as_str())
                    .or_else(|| item.get("html").and_then(|v| v.as_str()));
                let livecrawl_format = if item.get("markdown").and_then(|v| v.as_str()).is_some() {
                    "markdown"
                } else {
                    "html"
                };
                make_result(
                    "youcom",
                    item.get("title").and_then(|v| v.as_str()),
                    item.get("url").and_then(|v| v.as_str()),
                    snippet_owned.as_deref(),
                    None,
                    item.get("page_age").and_then(|v| v.as_str()),
                    item.get("favicon_url").and_then(|v| v.as_str()),
                    livecrawl_text,
                    if livecrawl_text.is_some() {
                        Some(livecrawl_format)
                    } else {
                        None
                    },
                    item.get("thumbnail_url").and_then(|v| v.as_str()),
                    None,
                    Some(request.search_type.as_str()),
                    idx as u32,
                    &now,
                )
            })
            .collect();
        let total = results.len() as u64;
        SearchResultSet {
            results,
            total_results: Some(total),
        }
    }
}

// ─── searxng ─────────────────────────────────────────────────────────────

pub struct SearxngProvider;
pub static SEARXNG: SearxngProvider = SearxngProvider;
impl SearchProvider for SearxngProvider {
    fn id(&self) -> &'static str {
        "searxng"
    }
    fn no_auth(&self) -> bool {
        true
    }
    fn timeout_ms(&self) -> Option<u64> {
        // 9router registry timeoutMs for searxng.
        Some(10_000)
    }
    fn max_max_results(&self) -> u32 {
        // 9router registry searxng.js maxMaxResults = 50.
        50
    }
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        // 9router: default URL comes from SEARXNG_URL env (default
        // http://localhost:8888/search).
        let default = std::env::var("SEARXNG_URL")
            .unwrap_or_else(|_| "http://localhost:8888/search".to_string());
        let base = resolve_base_url(&default, request)?;
        let url = if base.ends_with("/search") {
            base
        } else {
            format!("{base}/search")
        };
        let mut qp = vec![
            ("q", request.query.clone()),
            ("format", "json".to_string()),
            (
                "categories",
                if request.search_type == SearchType::News {
                    "news".to_string()
                } else {
                    "general".to_string()
                },
            ),
        ];
        if let Some(l) = &request.language {
            qp.push(("language", l.clone()));
        }
        if let Some(t) = request.time_range.as_deref().filter(|t| *t != "any") {
            qp.push(("time_range", t.to_string()));
        }
        if let Some(p) = page_number(request.offset, request.max_results) {
            qp.push(("pageno", p.to_string()));
        }
        Ok(format!(
            "{url}?{}",
            serde_urlencoded::to_string(&qp).unwrap_or_default()
        ))
    }
    fn build_headers(&self, _: &SearchRequest<'_>) -> Result<HeaderMap, String> {
        Ok(accept_json())
    }
    fn normalize(&self, body: &Value, _request: &SearchRequest<'_>) -> SearchResultSet {
        let now = now_iso();
        let items = body
            .get("results")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let results: Vec<SearchResult> = items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                let source = item
                    .get("engines")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .or_else(|| {
                        item.get("engine")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                    .or_else(|| {
                        item.get("category")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    });
                make_result(
                    "searxng",
                    item.get("title").and_then(|v| v.as_str()),
                    item.get("url").and_then(|v| v.as_str()),
                    item.get("content")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("snippet").and_then(|v| v.as_str())),
                    None,
                    item.get("publishedDate")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("published_date").and_then(|v| v.as_str())),
                    None,
                    None,
                    None,
                    item.get("thumbnail")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("img_src").and_then(|v| v.as_str())),
                    None,
                    source.as_deref(),
                    idx as u32,
                    &now,
                )
            })
            .collect();
        let total = results.len() as u64;
        SearchResultSet {
            results,
            total_results: Some(total),
        }
    }
}

// ─── xquik ───────────────────────────────────────────────────────────────
// Port of `open-sse/handlers/search/callers.js buildXquikRequest` (350-375)
// + `normalizers.js normalizeXquik` (202-241).

/// Extra fields in the Xquik unified shape that don't fit `SearchResult`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct XquikSearchResultSet {
    pub results: Vec<SearchResult>,
    pub total_results: Option<u64>,
    pub pagination: Value,
}

pub struct XquikProvider;
pub static XQUIK: XquikProvider = XquikProvider;
impl SearchProvider for XquikProvider {
    fn id(&self) -> &'static str {
        "xquik"
    }
    fn timeout_ms(&self) -> Option<u64> {
        // 9router registry xquik.js searchConfig timeoutMs = 10000.
        Some(10_000)
    }
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        let _ = require_token(request, "Xquik")?;
        let query_type = get_provider_setting(request, "queryType");
        if let Some(ref qt) = query_type {
            if qt != "Latest" && qt != "Top" {
                return Err("Xquik queryType must be Latest or Top".to_string());
            }
        }
        let mut qp = vec![
            ("q", request.query.clone()),
            ("limit", request.max_results.to_string()),
        ];
        if let Some(cursor) = get_provider_setting(request, "cursor") {
            qp.push(("cursor", cursor));
        }
        if let Some(qt) = query_type {
            qp.push(("queryType", qt));
        }
        if let Some(l) = &request.language {
            qp.push(("language", l.clone()));
        }
        Ok(format!(
            "{}?{}",
            resolve_base_url("https://xquik.com/api/v1/x/tweets/search", request)?,
            serde_urlencoded::to_string(&qp).unwrap_or_default()
        ))
    }
    fn build_headers(&self, request: &SearchRequest<'_>) -> Result<HeaderMap, String> {
        let token = require_token(request, "Xquik")?;
        let mut h = accept_json();
        h.insert(
            "x-api-key",
            HeaderValue::from_str(token).map_err(|e| e.to_string())?,
        );
        Ok(h)
    }
    fn normalize(&self, body: &Value, _request: &SearchRequest<'_>) -> SearchResultSet {
        normalize_xquik(body)
    }
    fn extra_envelope(&self, body: &Value) -> Option<Vec<(String, Value)>> {
        Some(vec![(
            "pagination".to_string(),
            normalize_xquik_with_pagination(body).pagination,
        )])
    }
}

/// Shared Xquik normalizer (also feeds the pagination-carrying variant).
fn normalize_xquik(body: &Value) -> SearchResultSet {
    let now = now_iso();
    let items = body
        .get("tweets")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let results: Vec<SearchResult> = items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let username = item
                .get("author")
                .and_then(|a| a.get("username"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let author_name = item
                .get("author")
                .and_then(|a| a.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let tweet_id = item
                .get("id")
                .and_then(|v| v.as_str().map(str::to_string))
                .or_else(|| {
                    item.get("id")
                        .and_then(|v| v.as_u64())
                        .map(|n| n.to_string())
                })
                .unwrap_or_default();
            let url = if !username.is_empty() && !tweet_id.is_empty() {
                format!(
                    "https://x.com/{}/status/{}",
                    urlencoding::encode(username),
                    urlencoding::encode(&tweet_id)
                )
            } else if !tweet_id.is_empty() {
                format!(
                    "https://x.com/i/web/status/{}",
                    urlencoding::encode(&tweet_id)
                )
            } else {
                String::new()
            };
            let author = if !username.is_empty() {
                Some(format!("@{username}"))
            } else if !author_name.is_empty() {
                Some(author_name.to_string())
            } else {
                None
            };
            let title = match &author {
                Some(a) => format!("{a} on X"),
                None => "X post".to_string(),
            };
            let image_url = item
                .get("media")
                .and_then(|v| v.as_array())
                .and_then(|arr| {
                    arr.iter().find_map(|m| {
                        m.get("mediaUrl")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                });
            let text = item.get("text").and_then(|v| v.as_str());
            make_result(
                "xquik",
                Some(&title),
                Some(&url),
                text,
                None,
                item.get("createdAt").and_then(|v| v.as_str()),
                None,
                text,
                Some("text"),
                image_url.as_deref(),
                author.as_deref(),
                Some("x_post"),
                idx as u32,
                &now,
            )
        })
        .collect();
    SearchResultSet {
        results,
        total_results: None,
    }
}

/// Xquik normalizer including `has_next_page` cursor pagination
/// (normalizers.js:232-240). The unified `SearchResultSet` has no
/// pagination slot, so this companion returns it alongside.
pub fn normalize_xquik_with_pagination(body: &Value) -> XquikSearchResultSet {
    let set = normalize_xquik(body);
    let next_cursor = body
        .get("next_cursor")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    XquikSearchResultSet {
        results: set.results,
        total_results: None,
        pagination: json!({
            "has_more": body.get("has_next_page").and_then(|v| v.as_bool()).unwrap_or(false),
            "next_cursor": next_cursor,
        }),
    }
}

// ─── ollama-search ───────────────────────────────────────────────────────
// Port of `open-sse/handlers/search/callers.js buildOllamaSearchRequest`
// (380-395) + `normalizers.js normalizeOllamaSearch` (243-258).

pub struct OllamaSearchProvider;
pub static OLLAMA_SEARCH: OllamaSearchProvider = OllamaSearchProvider;
impl SearchProvider for OllamaSearchProvider {
    fn id(&self) -> &'static str {
        "ollama-search"
    }
    fn timeout_ms(&self) -> Option<u64> {
        // 9router registry ollama-search.js searchConfig timeoutMs = 10000.
        Some(10_000)
    }
    fn max_max_results(&self) -> u32 {
        // 9router registry ollama-search.js maxMaxResults = 10.
        10
    }
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        resolve_base_url("https://ollama.com/api/web_search", request)
    }
    fn build_headers(&self, request: &SearchRequest<'_>) -> Result<HeaderMap, String> {
        let mut h = json_headers();
        if let Some(token) = request.token {
            h.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}")).map_err(|e| e.to_string())?,
            );
        }
        Ok(h)
    }
    fn method(&self) -> Method {
        Method::POST
    }
    fn build_body(&self, request: &SearchRequest<'_>) -> Option<Value> {
        let mut body = json!({"query": request.query, "max_results": request.max_results});
        if let Some(c) = &request.country {
            body["country"] = json!(c);
        }
        if let Some(l) = &request.language {
            body["language"] = json!(l);
        }
        Some(body)
    }
    fn normalize(&self, body: &Value, _request: &SearchRequest<'_>) -> SearchResultSet {
        let now = now_iso();
        let items: Vec<Value> = if let Some(arr) = body.get("results").and_then(|v| v.as_array()) {
            arr.clone()
        } else if let Some(arr) = body.as_array() {
            arr.clone()
        } else {
            Vec::new()
        };
        let results: Vec<SearchResult> = items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                let content = item.get("content").and_then(|v| v.as_str());
                make_result(
                    "ollama-search",
                    item.get("title").and_then(|v| v.as_str()),
                    item.get("url").and_then(|v| v.as_str()),
                    content.or_else(|| item.get("snippet").and_then(|v| v.as_str())),
                    None,
                    item.get("published_at").and_then(|v| v.as_str()),
                    None,
                    content,
                    Some("text"),
                    None,
                    None,
                    item.get("source").and_then(|v| v.as_str()),
                    idx as u32,
                    &now,
                )
            })
            .collect();
        let total = results.len() as u64;
        SearchResultSet {
            results,
            total_results: Some(total),
        }
    }
}

// ─── glm ─────────────────────────────────────────────────────────────────
// Port of `open-sse/handlers/search/callers.js buildGlmSearchRequest`
// (402-423) + `normalizers.js normalizeGlmSearch` (260-283).

pub struct GlmSearchProvider;
pub static GLM: GlmSearchProvider = GlmSearchProvider;
impl SearchProvider for GlmSearchProvider {
    fn id(&self) -> &'static str {
        "glm"
    }
    fn timeout_ms(&self) -> Option<u64> {
        // 9router registry glm.js searchConfig timeoutMs = 10000.
        Some(10_000)
    }
    fn max_max_results(&self) -> u32 {
        // 9router registry glm.js searchConfig maxMaxResults = 50.
        50
    }
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        resolve_base_url("https://api.z.ai/api/mcp/web_search_prime/mcp", request)
    }
    fn build_headers(&self, request: &SearchRequest<'_>) -> Result<HeaderMap, String> {
        let mut h = json_headers();
        if let Some(token) = request.token {
            h.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}")).map_err(|e| e.to_string())?,
            );
        }
        Ok(h)
    }
    fn method(&self) -> Method {
        Method::POST
    }
    fn build_body(&self, request: &SearchRequest<'_>) -> Option<Value> {
        Some(json!({
            "jsonrpc": "2.0",
            "id": format!("9r-{}", chrono::Utc::now().timestamp_millis()),
            "method": "tools/call",
            "params": {
                "name": "web_search_prime",
                "arguments": {
                    "search_query": request.query,
                    "count": request.max_results,
                },
            },
        }))
    }
    fn normalize(&self, body: &Value, _request: &SearchRequest<'_>) -> SearchResultSet {
        let now = now_iso();
        // MCP envelope: { result: { content: [{ type: "text", text: "<json>" }] } }.
        // The nested text is itself stringified JSON carrying results/news/array.
        let mut payload: Value = body.clone();
        if let Some(text) = body
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
        {
            if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                payload = parsed;
            } else {
                payload = json!({});
            }
        }
        let items: Vec<Value> = if let Some(arr) = payload.get("results").and_then(|v| v.as_array())
        {
            arr.clone()
        } else if let Some(arr) = payload.get("news").and_then(|v| v.as_array()) {
            arr.clone()
        } else if let Some(arr) = payload.as_array() {
            arr.clone()
        } else {
            Vec::new()
        };
        let results: Vec<SearchResult> = items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                make_result(
                    "glm",
                    item.get("title").and_then(|v| v.as_str()),
                    item.get("link")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("url").and_then(|v| v.as_str())),
                    item.get("content").and_then(|v| v.as_str()),
                    None,
                    item.get("publish_date")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("published_at").and_then(|v| v.as_str())),
                    item.get("icon").and_then(|v| v.as_str()),
                    None,
                    None,
                    None,
                    None,
                    item.get("media").and_then(|v| v.as_str()),
                    idx as u32,
                    &now,
                )
            })
            .collect();
        let total = results.len() as u64;
        SearchResultSet {
            results,
            total_results: Some(total),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::media::search::base::request_from_body;
    use serde_json::json;

    /// Serializes tests that touch the SEARXNG_URL env var.
    static SEARXNG_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn req(query: &str, max: u32) -> SearchRequest<'static> {
        SearchRequest {
            query: query.into(),
            search_type: SearchType::Web,
            max_results: max,
            token: None,
            country: None,
            language: None,
            time_range: None,
            offset: None,
            domain_filter: vec![],
            content_options: None,
            provider_options: Default::default(),
            provider_specific_data: Default::default(),
        }
    }

    #[test]
    fn registry_finds_known() {
        for id in [
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
        ] {
            assert!(lookup(id).is_some(), "missing provider {id}");
        }
        assert!(lookup("nope").is_none());
    }

    #[test]
    fn serper_news_endpoint() {
        let mut r = req("hi", 5);
        r.search_type = SearchType::News;
        r.token = Some("k");
        let url = SERPER.build_url(&r).unwrap();
        assert!(url.ends_with("/news"));
    }

    #[test]
    fn google_pse_requires_cx() {
        let mut r = req("hi", 5);
        r.token = Some("k");
        let err = GOOGLE_PSE.build_url(&r).unwrap_err();
        assert!(err.contains("cx"));
    }

    #[test]
    fn google_pse_includes_cx_when_provided() {
        let body = json!({"query": "hi", "max_results": 5, "provider_options": {"cx": "abc"}});
        let mut r = request_from_body(&body, None).unwrap();
        r.token = Some("k");
        let url = GOOGLE_PSE.build_url(&r).unwrap();
        assert!(url.contains("cx=abc"));
        assert!(url.contains("key=k"));
    }

    #[test]
    fn searxng_no_auth_uses_localhost_default() {
        // Env vars are process-global; serialize with the guard test below.
        let _guard = SEARXNG_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var("SEARXNG_URL");
        }
        let r = req("hi", 5);
        assert!(SEARXNG.no_auth());
        let url = SEARXNG.build_url(&r).unwrap();
        assert!(url.starts_with("http://localhost:8888/search"));
    }

    #[test]
    fn searxng_default_from_env() {
        let _guard = SEARXNG_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Unset → 9router default localhost:8888/search.
        unsafe {
            std::env::remove_var("SEARXNG_URL");
        }
        let url = SEARXNG.build_url(&req("hi", 5)).unwrap();
        assert!(
            url.starts_with("http://localhost:8888/search?"),
            "got: {url}"
        );

        // Set → env value used.
        std::env::set_var("SEARXNG_URL", "https://x.example");
        let url = SEARXNG.build_url(&req("hi", 5)).unwrap();
        assert!(url.starts_with("https://x.example/search?"), "got: {url}");

        unsafe {
            std::env::remove_var("SEARXNG_URL");
        }
    }

    #[test]
    fn exa_normalises_full_text_to_content() {
        let body = json!({
            "results": [{
                "title": "T", "url": "https://x.com",
                "highlights": ["snip"], "text": "full body",
                "score": 0.9
            }]
        });
        let r = req("hi", 5);
        let set = EXA.normalize(&body, &r);
        assert_eq!(set.results.len(), 1);
        assert_eq!(set.results[0].score, Some(0.9));
        assert!(set.results[0].content.is_some());
    }

    #[test]
    fn brave_news_uses_news_container() {
        let body = json!({
            "news": {
                "results": [{"title": "n", "url": "https://x", "description": "d"}],
                "totalCount": 42
            }
        });
        let mut r = req("hi", 5);
        r.search_type = SearchType::News;
        let set = BRAVE.normalize(&body, &r);
        assert_eq!(set.total_results, Some(42));
        assert_eq!(set.results.len(), 1);
    }

    #[test]
    fn youcom_url_uses_ydc_index() {
        // 9router registry/youcom.js:20 baseUrl — NOT api.you.com.
        let mut r = req("test", 5);
        r.token = Some("tok");
        let url = YOUCOM.build_url(&r).unwrap();
        assert!(
            url.starts_with("https://ydc-index.io/v1/search?"),
            "youcom must hit ydc-index.io, got: {url}"
        );
        assert!(url.contains("query=test"));
        // X-API-Key header (JS authHeader x-api-key).
        let headers = YOUCOM.build_headers(&r).unwrap();
        assert_eq!(
            headers.get("X-API-Key").and_then(|v| v.to_str().ok()),
            Some("tok")
        );
    }

    #[test]
    fn searxng_caps_max_results_at_50() {
        // 9router registry searxng.js maxMaxResults = 50; youcom = 100.
        assert_eq!(SEARXNG.max_max_results(), 50);
        assert_eq!(YOUCOM.max_max_results(), 100);
        assert_eq!(SERPER.max_max_results(), 100); // default

        // A request with max_results 100 clamped to searxng's 50.
        let mut r = req("query", 100);
        r.max_results = r.max_results.min(SEARXNG.max_max_results());
        assert_eq!(r.max_results, 50);

        // youcom stays 100.
        let mut y = req("query", 100);
        y.max_results = y.max_results.min(YOUCOM.max_max_results());
        assert_eq!(y.max_results, 100);
    }

    #[test]
    fn youcom_livecrawl_markdown_format() {
        // 9router callers.js buildYouComRequest (289-295): full_page →
        // livecrawl=web|news + livecrawl_formats=markdown|html.
        let mut r = req("test", 5);
        r.token = Some("tok");
        r.content_options = Some(serde_json::json!({
            "full_page": true,
            "format": "markdown",
        }));
        let url = YOUCOM.build_url(&r).unwrap();
        assert!(
            url.contains("livecrawl=web"),
            "news→web for web search: {url}"
        );
        assert!(
            url.contains("livecrawl_formats=markdown"),
            "markdown format must be passed: {url}"
        );

        // Non-markdown format defaults to html.
        let mut r2 = req("test", 5);
        r2.token = Some("tok");
        r2.content_options = Some(serde_json::json!({
            "full_page": true,
            "format": "json",
        }));
        let url2 = YOUCOM.build_url(&r2).unwrap();
        assert!(
            url2.contains("livecrawl_formats=html"),
            "non-markdown → html: {url2}"
        );

        // full_page false → no livecrawl.
        let mut r3 = req("test", 5);
        r3.token = Some("tok");
        r3.content_options = Some(serde_json::json!({
            "full_page": false,
            "format": "markdown",
        }));
        let url3 = YOUCOM.build_url(&r3).unwrap();
        assert!(
            !url3.contains("livecrawl"),
            "no livecrawl when full_page false: {url3}"
        );
    }

    #[test]
    fn registry_finds_new_search_providers() {
        // P0 #15: xquik/ollama-search/glm must be in lookup so
        // POST /v1/search no longer returns 400 for them.
        for id in ["xquik", "ollama-search", "glm"] {
            assert!(lookup(id).is_some(), "missing provider {id}");
        }
    }

    #[test]
    fn xquik_builds_documented_get_request() {
        // 9router tests/unit/xquik-search-provider.test.js: builds the
        // documented GET request without putting the key in the URL.
        let mut r = req("from:github release notes", 10);
        r.token = Some("xq_test_key");
        r.language = Some("en".to_string());
        r.provider_options
            .insert("queryType".to_string(), serde_json::json!("Latest"));
        r.provider_options
            .insert("cursor".to_string(), serde_json::json!("next page"));
        let url = XQUIK.build_url(&r).unwrap();
        assert!(
            url.starts_with("https://xquik.com/api/v1/x/tweets/search?"),
            "got: {url}"
        );
        assert!(url.contains("q=from%3Agithub+release+notes") || url.contains("q=from"));
        assert!(url.contains("limit=10"));
        assert!(url.contains("queryType=Latest"));
        assert!(url.contains("language=en"));
        assert!(!url.contains("xq_test_key"), "key must not be in URL");
        let headers = XQUIK.build_headers(&r).unwrap();
        assert_eq!(
            headers.get("x-api-key").and_then(|v| v.to_str().ok()),
            Some("xq_test_key")
        );
    }

    #[test]
    fn xquik_rejects_bad_query_type() {
        let mut r = req("hi", 5);
        r.token = Some("k");
        r.provider_options
            .insert("queryType".to_string(), serde_json::json!("Popular"));
        let err = XQUIK.build_url(&r).unwrap_err();
        assert!(err.contains("Xquik queryType must be Latest or Top"));
    }

    #[test]
    fn xquik_normalizes_posts_and_pagination() {
        // Mirrors tests/unit/xquik-search-provider.test.js "normalizes posts
        // and preserves cursor pagination".
        let body = json!({
            "tweets": [{
                "id": "1234567890",
                "text": "Release notes are live.",
                "createdAt": "2026-08-25T12:00:00Z",
                "author": {"username": "github", "name": "GitHub"},
                "media": [{"mediaUrl": "https://pbs.twimg.com/media/example.jpg", "type": "photo"}],
            }],
            "has_next_page": true,
            "next_cursor": "cursor-2",
        });
        let r = req("hi", 5);
        let set = XQUIK.normalize(&body, &r);
        assert_eq!(set.results.len(), 1);
        assert_eq!(set.results[0].title, "@github on X");
        assert_eq!(set.results[0].url, "https://x.com/github/status/1234567890");
        assert_eq!(set.results[0].snippet, "Release notes are live.");
        assert!(set.total_results.is_none());
        let paged = normalize_xquik_with_pagination(&body);
        assert_eq!(
            paged.pagination,
            json!({"has_more": true, "next_cursor": "cursor-2"})
        );
    }

    #[test]
    fn xquik_stable_url_without_author() {
        let body = json!({
            "tweets": [{"id": "9876543210", "text": "Author data is unavailable."}],
            "has_next_page": false,
        });
        let r = req("hi", 5);
        let set = XQUIK.normalize(&body, &r);
        assert_eq!(set.results[0].url, "https://x.com/i/web/status/9876543210");
    }

    #[test]
    fn ollama_search_posts_query_body() {
        // 9router callers.js buildOllamaSearchRequest: POST
        // https://ollama.com/api/web_search { query, max_results }.
        let mut r = req("hello", 5);
        r.token = Some("ollama-key");
        let url = OLLAMA_SEARCH.build_url(&r).unwrap();
        assert_eq!(url, "https://ollama.com/api/web_search");
        assert_eq!(OLLAMA_SEARCH.method(), reqwest::Method::POST);
        let body = OLLAMA_SEARCH.build_body(&r).unwrap();
        assert_eq!(body["query"], json!("hello"));
        assert_eq!(body["max_results"], json!(5));
        let headers = OLLAMA_SEARCH.build_headers(&r).unwrap();
        assert_eq!(
            headers
                .get(reqwest::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
            Some("Bearer ollama-key")
        );
        // Token optional — no-auth header set still builds.
        let r2 = req("hello", 5);
        assert!(OLLAMA_SEARCH.build_headers(&r2).is_ok());
    }

    #[test]
    fn ollama_search_normalizer_falls_back_to_bare_array() {
        // normalizers.js normalizeOllamaSearch: data.results first,
        // fall back to a bare array.
        let r = req("hi", 5);
        let wrapped = json!({"results": [
            {"title": "T", "url": "https://x.com", "content": "body text"}
        ]});
        let set = OLLAMA_SEARCH.normalize(&wrapped, &r);
        assert_eq!(set.results.len(), 1);
        assert_eq!(set.results[0].snippet, "body text");
        let bare = json!([
            {"title": "T2", "url": "https://y.com", "snippet": "snip"}
        ]);
        let set2 = OLLAMA_SEARCH.normalize(&bare, &r);
        assert_eq!(set2.results.len(), 1);
        assert_eq!(set2.results[0].title, "T2");
    }

    #[test]
    fn glm_posts_jsonrpc_envelope() {
        // 9router callers.js buildGlmSearchRequest: POST
        // https://api.z.ai/api/mcp/web_search_prime/mcp with the
        // { jsonrpc, method: "tools/call", params: { name:
        // "web_search_prime", arguments: { search_query, count } } }
        // MCP envelope.
        let mut r = req("hello", 7);
        r.token = Some("glm-key");
        let url = GLM.build_url(&r).unwrap();
        assert_eq!(url, "https://api.z.ai/api/mcp/web_search_prime/mcp");
        assert_eq!(GLM.method(), reqwest::Method::POST);
        let body = GLM.build_body(&r).unwrap();
        assert_eq!(body["jsonrpc"], json!("2.0"));
        assert_eq!(body["method"], json!("tools/call"));
        assert_eq!(body["params"]["name"], json!("web_search_prime"));
        assert_eq!(body["params"]["arguments"]["search_query"], json!("hello"));
        assert_eq!(body["params"]["arguments"]["count"], json!(7));
        assert_eq!(GLM.max_max_results(), 50);
    }

    #[test]
    fn serpingapi_requires_token() {
        let r = req("hi", 5);
        assert!(SERPINGAPI.build_headers(&r).is_err());
        let mut ok = req("hi", 5);
        ok.token = Some("key");
        let headers = SERPINGAPI.build_headers(&ok).unwrap();
        assert_eq!(
            headers.get("X-API-Key").and_then(|v| v.to_str().ok()),
            Some("key")
        );
    }

    #[test]
    fn serpingapi_rejects_news() {
        let mut r = req("hi", 5);
        r.search_type = SearchType::News;
        assert!(SERPINGAPI.build_url(&r).is_err());
        let web = req("hi", 5);
        let url = SERPINGAPI.build_url(&web).unwrap();
        assert_eq!(url, "https://api.serpingapi.com/v1/search");
        assert_eq!(SERPINGAPI.max_max_results(), 100);
    }

    #[test]
    fn serpingapi_body_maps_country_language_time_range_and_page() {
        let mut r = req("openproxy", 10);
        r.country = Some("US".into());
        r.language = Some("en".into());
        r.time_range = Some("week".into());
        r.offset = Some(20);
        r.provider_options
            .insert("location".to_string(), serde_json::json!("New York,US"));
        let body = SERPINGAPI.build_body(&r).unwrap();
        assert_eq!(body["q"], json!("openproxy"));
        assert_eq!(body["num"], json!(10));
        assert_eq!(body["gl"], json!("us"));
        assert_eq!(body["hl"], json!("en"));
        assert_eq!(body["tbs"], json!("qdr:w"));
        assert_eq!(body["page"], json!(3));
        assert_eq!(body["location"], json!("New York,US"));
    }

    #[test]
    fn serpingapi_normalize_maps_organic() {
        let body = json!({
            "organic": [
                {"title": "T", "link": "https://x.com", "snippet": "s", "date": "2026-09-01"},
                {"title": "T2", "link": "https://y.com", "snippet": "s2"}
            ]
        });
        let r = req("hi", 5);
        let set = SERPINGAPI.normalize(&body, &r);
        assert_eq!(set.results.len(), 2);
        assert_eq!(set.results[0].citation["provider"], json!("serpingapi"));
        assert_eq!(set.results[0].url, "https://x.com");
        assert_eq!(set.total_results, None);
    }

    #[test]
    fn glm_normalizer_unwraps_mcp_text_envelope() {
        // normalizers.js normalizeGlmSearch: unwrap
        // data.result.content[0].text, then parse nested stringified JSON.
        let inner = serde_json::to_string(&json!({
            "results": [{
                "title": "T", "link": "https://x.com",
                "content": "body", "publish_date": "2026-01-01",
            }]
        }))
        .unwrap();
        let body = json!({"result": {"content": [{"type": "text", "text": inner}]}});
        let r = req("hi", 5);
        let set = GLM.normalize(&body, &r);
        assert_eq!(set.results.len(), 1);
        assert_eq!(set.results[0].url, "https://x.com");
        assert_eq!(set.results[0].snippet, "body");
    }
}
