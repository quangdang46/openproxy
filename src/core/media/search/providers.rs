//! Concrete `SearchProvider` impls for the 10 supported providers.
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
        "brave-search" => &BRAVE,
        "perplexity" => &PERPLEXITY,
        "exa" => &EXA,
        "tavily" => &TAVILY,
        "google-pse" => &GOOGLE_PSE,
        "linkup" => &LINKUP,
        "searchapi" => &SEARCH_API,
        "youcom" => &YOUCOM,
        "searxng" => &SEARXNG,
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
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        let endpoint = if request.search_type == SearchType::News {
            "/news"
        } else {
            "/search"
        };
        Ok(format!(
            "{}{endpoint}",
            resolve_base_url("https://google.serper.dev", request)
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

// ─── brave-search ────────────────────────────────────────────────────────

pub struct BraveProvider;
pub static BRAVE: BraveProvider = BraveProvider;
impl SearchProvider for BraveProvider {
    fn id(&self) -> &'static str {
        "brave-search"
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
            resolve_base_url("https://api.search.brave.com/res/v1", request),
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
        Ok(resolve_base_url(
            "https://api.perplexity.ai/search",
            request,
        ))
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
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        Ok(resolve_base_url("https://api.exa.ai/search", request))
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
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        Ok(resolve_base_url("https://api.tavily.com/search", request))
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
            ),
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
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        Ok(resolve_base_url("https://api.linkup.so/v1/search", request))
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
            resolve_base_url("https://www.searchapi.io/api/v1/search", request),
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
            resolve_base_url("https://api.you.com/search", request),
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
    fn build_url(&self, request: &SearchRequest<'_>) -> Result<String, String> {
        let base = resolve_base_url("http://localhost:8080", request);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::media::search::base::request_from_body;
    use serde_json::json;

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
        let r = req("hi", 5);
        assert!(SEARXNG.no_auth());
        let url = SEARXNG.build_url(&r).unwrap();
        assert!(url.starts_with("http://localhost:8080/search"));
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
}
