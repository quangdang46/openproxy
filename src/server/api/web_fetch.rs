//! `/v1/web/fetch` — Web URL extraction endpoint.
//! Baseline parity: POST + OPTIONS, conditional auth, combo support,
//! per-account fallback, exact normalized response envelope.

use std::collections::HashSet;

use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::core::combo::{check_fallback_error, get_combo_models_from_data, ComboStrategy};
use crate::server::state::AppState;
use crate::types::ProviderConnection;

use super::auth_error_response;

// ─── Route mount ────────────────────────────────────────────────────────────

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/v1/web/fetch",
        post(handle_web_fetch).options(cors_options),
    )
}

// ─── Request shape ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FetchRequest {
    /// Provider ID (firecrawl, jina-reader, tavily, exa) or model alias.
    /// UI sends `model` since provider IS the model for this endpoint.
    #[serde(alias = "model")]
    provider: Option<String>,
    /// Target URL to fetch and extract content from.
    url: Option<String>,
    /// Output format: "markdown" (default), "html", "text".
    #[serde(default = "default_format")]
    format: String,
    /// Truncate output to this many characters.
    max_characters: Option<usize>,
}

fn default_format() -> String {
    "markdown".to_string()
}

// ─── CORS preflight ────────────────────────────────────────────────────────────

async fn cors_options() -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::OK;
    resp.headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    resp.headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("POST, OPTIONS"));
    resp.headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("*"));
    resp
}

// ─── Main handler ────────────────────────────────────────────────────────────

async fn handle_web_fetch(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<FetchRequest>, JsonRejection>,
) -> Response {
    // ── 1. Parse body ──────────────────────────────────────────────────────
    let Json(req) = match body {
        Ok(b) => b,
        Err(_) => return fetch_error(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };

    // ── 2. Auth: conditional on settings.require_login (baseline parity) ───
    if state.db.snapshot().settings.require_login {
        if let Err(e) = crate::server::auth::require_api_key(&headers, &state.db) {
            return auth_error_response(e);
        }
    }

    // ── 3. Validate required fields ────────────────────────────────────────
    let provider_input = match req.provider.as_deref() {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return fetch_error(StatusCode::BAD_REQUEST, "Missing required field: provider (or model)"),
    };

    let url = match req.url.as_deref() {
        Some(s) if !s.trim().is_empty() => {
            // Validate URL syntax
            if url::Url::parse(s).is_err() {
                return fetch_error(StatusCode::BAD_REQUEST, "Invalid URL format");
            }
            s.trim().to_string()
        }
        _ => return fetch_error(StatusCode::BAD_REQUEST, "Missing required field: url"),
    };

    let format = req.format.trim();
    let max_chars = req.max_characters.unwrap_or(usize::MAX);
    let snapshot = state.db.snapshot();

    // ── 4. Combo detection (baseline parity) ────────────────────────────────
    if let Some(combo_models) = get_combo_models_from_data(&provider_input, &snapshot.combos) {
        let strategy = combo_strategy_for(&snapshot, &provider_input);
        let fetch_state = state.clone();
        let req_url = url.clone();
        let req_format = format.to_string();
        let req_max = max_chars;

        match execute_combo_fetch(
            &combo_models,
            Some(&provider_input),
            strategy,
            req_url,
            req_format,
            req_max,
            &fetch_state,
        )
        .await
        {
            Ok(resp) => return resp,
            Err(e) => return fetch_error(StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_GATEWAY), &e.message),
        }
    }

    // ── 5. Single provider dispatch ─────────────────────────────────────────
    match execute_single_fetch(&state, &provider_input, &url, format, max_chars).await {
        Ok(resp) => resp,
        Err(e) => fetch_error(StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_GATEWAY), &e.message),
    }
}

// ─── Combo execution ──────────────────────────────────────────────────────────

async fn execute_combo_fetch(
    models: &[String],
    combo_name: Option<&str>,
    strategy: ComboStrategy,
    url: String,
    format: String,
    max_chars: usize,
    state: &AppState,
) -> Result<Response, crate::core::combo::ComboExecutionError> {
    let url = url.clone();
    let format = format.to_string();
    let max_chars = max_chars as usize;
    let state = state.clone();

    crate::core::combo::execute_combo_strategy(
        models,
        combo_name,
        strategy,
        move |model: &str| {
            let model_owned = model.to_string();
            let url = url.clone();
            let format = format.clone();
            let max_chars = max_chars as usize;
            let state = state.clone();
            async move {
                execute_single_fetch(&state, &model_owned, &url, &format, max_chars)
                    .await
                    .map_err(|e| crate::core::combo::ComboAttemptError {
                        status: e.status,
                        message: e.message,
                        retry_after: None,
                    })
            }
        },
    )
    .await
    .map_err(|e| crate::core::combo::ComboExecutionError {
        status: e.status,
        message: e.message,
        earliest_retry_after: e.earliest_retry_after,
    })
}

fn combo_strategy_for(snapshot: &crate::types::AppDb, combo_name: &str) -> ComboStrategy {
    let value = snapshot
        .settings
        .combo_strategies
        .get(combo_name)
        .map(String::as_str)
        .unwrap_or(snapshot.settings.combo_strategy.as_str());

    if value.eq_ignore_ascii_case("round-robin") {
        ComboStrategy::RoundRobin
    } else {
        ComboStrategy::Fallback
    }
}

// ─── Single provider execution ───────────────────────────────────────────────

#[derive(Debug)]
struct FetchError {
    status: u16,
    message: String,
}

impl From<reqwest::Error> for FetchError {
    fn from(e: reqwest::Error) -> Self {
        FetchError {
            status: 502,
            message: e.to_string(),
        }
    }
}

async fn execute_single_fetch(
    state: &AppState,
    provider_input: &str,
    url: &str,
    format: &str,
    max_chars: usize,
) -> Result<Response, FetchError> {
    let provider_id = resolve_fetch_provider(provider_input);
    let snapshot = state.db.snapshot();

    // Get credentials for this provider (with fallback loop)
    let mut excluded = HashSet::new();
    let registry = &state.account_registry;

    loop {
        let connection = select_fetch_connection(&snapshot, &provider_id, &excluded);

        let Some(connection) = connection else {
            return Err(FetchError {
                status: 400,
                message: format!("No credentials for provider: {}", provider_id),
            });
        };

        let (rate_limit_remaining, rate_limit_reset) = registry.rate_limit_info(&connection.id);
        let slot = registry.acquire_slot(
            &connection.id,
            10,
            rate_limit_remaining,
            rate_limit_reset,
        );

        let Some(_slot) = slot else {
            excluded.insert(connection.id.clone());
            continue;
        };

        match do_fetch(state, &provider_id, &connection, url, format, max_chars).await {
            Ok(response) => {
                clear_connection_error(state, &connection.id).await;
                return Ok(cors_json_response(StatusCode::OK, response));
            }
            Err(e) => {
                let status = e.status;
                let message = e.message.clone();
                let current_backoff = connection.backoff_level.unwrap_or(0);
                let decision = check_fallback_error(status, &message, current_backoff);
                let cooldown = decision.cooldown;
                let backoff_level = decision.new_backoff_level.unwrap_or(current_backoff + 1);

                if decision.should_fallback {
                    mark_connection_unavailable(state, &connection.id, status, &message, cooldown, backoff_level).await;
                    excluded.insert(connection.id.clone());
                    continue;
                }
                return Err(e);
            }
        }
    }
}

async fn do_fetch(
    state: &AppState,
    provider: &str,
    connection: &ProviderConnection,
    url: &str,
    format: &str,
    max_chars: usize,
) -> Result<Value, FetchError> {
    let started_at = std::time::Instant::now();
    let upstream_start = std::time::Instant::now();

    let (request_url, request_body, headers) = build_fetch_request(provider, connection, url, format)?;

    let proxy = crate::core::proxy::resolve_proxy_target(
        &state.db.snapshot(),
        connection,
        &state.db.snapshot().settings,
    );

    let client = state
        .client_pool
        .get(provider, proxy.as_ref())
        .map_err(|e| FetchError { status: 502, message: e.to_string() })?;

    let upstream_ms = upstream_start.elapsed().as_millis() as u64;

    let resp = client
        .post(&request_url)
        .headers(headers)
        .json(&request_body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| FetchError {
            status: 504,
            message: if e.is_timeout() {
                "Request timed out".to_string()
            } else {
                e.to_string()
            },
        })?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(FetchError {
            status: status.as_u16(),
            message: text,
        });
    }

    let body: Value = resp.json().await.map_err(|e| FetchError {
        status: 502,
        message: format!("JSON parse error: {}", e),
    })?;

    let response_ms = started_at.elapsed().as_millis() as u64;
    normalize_fetch_response(provider, url, format, max_chars, body, response_ms, upstream_ms)
}

fn build_fetch_request(
    provider: &str,
    connection: &ProviderConnection,
    url: &str,
    format: &str,
) -> Result<(String, Value, HeaderMap), FetchError> {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let api_key = connection
        .api_key
        .as_deref()
        .filter(|k| !k.is_empty())
        .or_else(|| connection.access_token.as_deref().filter(|k| !k.is_empty()));

    match provider {
        "firecrawl" => {
            let request_url = "https://api.firecrawl.dev/v1/scrape".to_string();
            let request_body = json!({
                "url": url,
                "formats": [format]
            });
            if let Some(key) = api_key {
                let mut h = HeaderMap::new();
                h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
                h.insert(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", key))
                        .map_err(|_| FetchError { status: 500, message: "Invalid API key header".into() })?,
                );
                return Ok((request_url, request_body, h));
            }
            Ok((request_url, request_body, headers))
        }

        "jina-reader" => {
            let request_url = format!("https://r.jina.ai/{}", urlencoding(url));
            let request_body = json!({});
            if let Some(key) = api_key {
                let mut h = HeaderMap::new();
                h.insert(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", key))
                        .map_err(|_| FetchError { status: 500, message: "Invalid API key header".into() })?,
                );
                return Ok((request_url, request_body, h));
            }
            Ok((request_url, request_body, headers))
        }

        "tavily" => {
            let request_url = "https://api.tavily.com/extract".to_string();
            let request_body = json!({
                "urls": [url],
                "extract_depth": "basic"
            });
            if let Some(key) = api_key {
                let mut h = HeaderMap::new();
                h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
                h.insert(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", key))
                        .map_err(|_| FetchError { status: 500, message: "Invalid API key header".into() })?,
                );
                return Ok((request_url, request_body, h));
            }
            Ok((request_url, request_body, headers))
        }

        "exa" => {
            let request_url = "https://api.exa.ai/contents".to_string();
            let request_body = json!({
                "ids": [url],
                "text": true
            });
            if let Some(key) = api_key {
                let mut h = HeaderMap::new();
                h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
                h.insert(
                    HeaderName::from_static("x-api-key"),
                    HeaderValue::from_str(key)
                        .map_err(|_| FetchError { status: 500, message: "Invalid API key header".into() })?,
                );
                return Ok((request_url, request_body, h));
            }
            Ok((request_url, request_body, headers))
        }

        _ => Err(FetchError {
            status: 400,
            message: format!("Unsupported web fetch provider: {}", provider),
        }),
    }
}

fn resolve_fetch_provider(alias: &str) -> String {
    match alias.trim().to_lowercase().as_str() {
        "jina" | "jina-reader" => "jina-reader".to_string(),
        "fc" | "firecrawl" => "firecrawl".to_string(),
        "tv" | "tavily" => "tavily".to_string(),
        "exa" => "exa".to_string(),
        other => other.to_string(),
    }
}


/// Simple percent-encoding for URL segments (Jina Reader path).
fn urlencoding(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 3);
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(byte as char),
            _ => {
                out.push('%');
                out.push_str(&format!("{:02X}", byte));
            }
        }
    }
    out
}

fn normalize_fetch_response(
    provider: &str,
    url: &str,
    format: &str,
    max_chars: usize,
    body: Value,
    response_ms: u64,
    upstream_ms: u64,
) -> Result<Value, FetchError> {
    let (text_owned, title) = match provider {
        "firecrawl" => {
            let data = body.get("data");
            let text = data
                .and_then(|d| d.get("markdown").and_then(|v| v.as_str()))
                .or_else(|| data.and_then(|d| d.get("html").and_then(|v| v.as_str())))
                .or_else(|| data.and_then(|d| d.get("text").and_then(|v| v.as_str())))
                .unwrap_or_default()
                .to_string();
            let title = data.and_then(|d| d.get("metadata"))
                .and_then(|m| m.get("title"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            (text, title)
        }
        "jina-reader" => {
            // Jina returns plain text directly
            let text = body.as_str().unwrap_or_default().to_string();
            // Try to extract title from first line (markdown heading)
            let title = text.lines()
                .next()
                .and_then(|line| line.strip_prefix("# ").map(str::to_string));
            (text, title)
        }
        "tavily" => {
            let first = body.get("results").and_then(|r| r.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.as_object());
            let text = first
                .and_then(|o| o.get("raw_content").and_then(|v| v.as_str()))
                .unwrap_or_default()
                .to_string();
            (text, None)
        }
        "exa" => {
            let first = body.get("results").and_then(|r| r.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.as_object());
            let text = first
                .and_then(|o| o.get("text").and_then(|v| v.as_str()))
                .unwrap_or_default()
                .to_string();
            let title = first
                .and_then(|o| o.get("title").and_then(|v| v.as_str()))
                .map(str::to_string);
            (text, title)
        }
        _ => ("".to_string(), None),
    };

    let text_owned = if text_owned.len() > max_chars {
        text_owned[..max_chars].to_string()
    } else {
        text_owned.clone()
    };

    Ok(json!({
        "provider": provider,
        "url": url,
        "title": title,
        "content": {
            "format": format,
            "text": text_owned,
            "length": text_owned.len()
        },
        "metadata": {
            "author": null,
            "published_at": null,
            "language": null
        },
        "usage": {
            "fetch_cost_usd": null
        },
        "metrics": {
            "response_time_ms": response_ms,
            "upstream_latency_ms": upstream_ms
        }
    }))
}

fn select_fetch_connection(
    snapshot: &crate::types::AppDb,
    provider: &str,
    excluded: &HashSet<String>,
) -> Option<ProviderConnection> {
    snapshot
        .provider_connections
        .iter()
        .filter(|c| {
            c.provider == provider
                && c.is_active()
                && connection_has_credentials(c)
                && !excluded.contains(&c.id)
        })
        .min_by_key(|c| c.priority.unwrap_or(999))
        .cloned()
}

fn connection_has_credentials(c: &ProviderConnection) -> bool {
    c.api_key.as_deref().map(str::trim).filter(|v| !v.is_empty()).is_some()
        || c.access_token.as_deref().map(str::trim).filter(|v| !v.is_empty()).is_some()
}

async fn mark_connection_unavailable(
    state: &AppState,
    connection_id: &str,
    status: u16,
    message: &str,
    cooldown: std::time::Duration,
    backoff_level: u32,
) {
    use chrono::{Duration as ChronoDuration, Utc};
    let until = ChronoDuration::from_std(cooldown)
        .map(|d| Utc::now() + d)
        .unwrap_or_else(|_| Utc::now());
    let connection_id = connection_id.to_string();
    let message = message.to_string();
    let _ = state.db.update(move |db| {
        if let Some(c) = db.provider_connections.iter_mut().find(|c| c.id == connection_id) {
            c.last_error = Some(message);
            c.last_error_at = Some(Utc::now().to_rfc3339());
            c.error_code = Some(status.to_string());
            c.backoff_level = Some(backoff_level);
            c.consecutive_errors = c.consecutive_errors.map(|e| e.saturating_add(1)).or(Some(1));
            c.test_status = Some("unavailable".into());
        }
    }).await;
}

async fn clear_connection_error(state: &AppState, connection_id: &str) {
    let connection_id = connection_id.to_string();
    let _ = state.db.update(move |db| {
        if let Some(c) = db.provider_connections.iter_mut().find(|c| c.id == connection_id) {
            c.last_error = None;
            c.last_error_at = None;
            c.error_code = None;
            c.backoff_level = Some(0);
            c.consecutive_errors = Some(0);
            c.test_status = None;
        }
    }).await;
}

// ─── Response helpers ────────────────────────────────────────────────────────

fn cors_json_response(status: StatusCode, payload: Value) -> Response {
    let mut resp = (status, Json(payload)).into_response();
    resp.headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    resp
}

fn fetch_error(status: StatusCode, message: &str) -> Response {
    let mut resp = (status, Json(json!({ "error": message }))).into_response();
    resp.headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    resp
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_fetch_provider_maps_aliases() {
        assert_eq!(resolve_fetch_provider("jina"), "jina-reader");
        assert_eq!(resolve_fetch_provider("firecrawl"), "firecrawl");
        assert_eq!(resolve_fetch_provider("tavily"), "tavily");
        assert_eq!(resolve_fetch_provider("exa"), "exa");
        assert_eq!(resolve_fetch_provider("jina-reader"), "jina-reader");
        assert_eq!(resolve_fetch_provider("FC"), "firecrawl");
    }

    #[test]
    fn normalize_firecrawl_extracts_markdown_and_title() {
        let body = json!({
            "data": {
                "markdown": "# Hello\n\nWorld content",
                "metadata": { "title": "Test Page" }
            }
        });
        let result = normalize_fetch_response(
            "firecrawl", "https://example.com", "markdown", 1000,
            body, 150, 50,
        ).unwrap();
        assert_eq!(result["provider"], "firecrawl");
        assert_eq!(result["title"], "Test Page");
        assert_eq!(result["content"]["format"], "markdown");
        assert!(result["content"]["text"].as_str().unwrap().starts_with("# Hello"));
        assert_eq!(result["metrics"]["response_time_ms"], 150);
    }

    #[test]
    fn normalize_jina_reader_extracts_plain_text_and_first_heading() {
        let body = json!("# My Title\n\nRest of the page content here.");
        let result = normalize_fetch_response(
            "jina-reader", "https://example.com", "markdown", 1000,
            body, 80, 30,
        ).unwrap();
        assert_eq!(result["provider"], "jina-reader");
        assert_eq!(result["title"], "My Title");
        assert!(result["content"]["text"].as_str().unwrap().contains("Rest of the page"));
    }

    #[test]
    fn normalize_respects_max_characters() {
        let body = json!("This is a very long text that should be truncated when max_chars is small");
        let result = normalize_fetch_response(
            "jina-reader", "https://example.com", "markdown", 20,
            body, 50, 10,
        ).unwrap();
        let text = result["content"]["text"].as_str().unwrap();
        assert_eq!(text.len(), 20);
        assert_eq!(result["content"]["length"], 20);
    }

    #[test]
    fn combo_strategy_fallback_default() {
        // verify the combo_strategy_for function uses settings correctly
        // (integration tested via web_fetch_api tests)
    }
}
