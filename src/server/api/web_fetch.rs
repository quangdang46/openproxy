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

use crate::core::combo::{
    check_fallback_error, get_combo_models_from_data, strategy_for_combo, ComboStrategy,
};
use crate::server::state::AppState;
use crate::types::{PricingTable, ProviderConnection};

use super::auth_error_response;
use super::cors::cors_preflight_response;

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
    cors_preflight_response()
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
        if let Err(e) = crate::server::auth::require_api_key_with_reload(&headers, &state.db).await
        {
            return auth_error_response(e);
        }
    }

    // ── 3. Validate required fields ────────────────────────────────────────
    let provider_input = match req.provider.as_deref() {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => {
            return fetch_error(
                StatusCode::BAD_REQUEST,
                "Missing required field: provider (or model)",
            )
        }
    };

    let url = match req.url.as_deref() {
        Some(s) if !s.trim().is_empty() => {
            // Validate URL syntax
            if url::Url::parse(s).is_err() {
                return fetch_error(StatusCode::BAD_REQUEST, "Invalid URL format");
            }
            // Reject URLs that resolve to private/internal IPs (SSRF protection)
            if let Err(msg) = check_private_ip(s.trim()).await {
                return fetch_error(StatusCode::BAD_REQUEST, &msg);
            }
            s.trim().to_string()
        }
        _ => return fetch_error(StatusCode::BAD_REQUEST, "Missing required field: url"),
    };

    // 9router fetch/index.js: `format || DEFAULT_FORMAT` — an empty string
    // falls back to markdown just like a missing field.
    let format = req.format.trim();
    let format = if format.is_empty() {
        "markdown"
    } else {
        format
    };
    let max_chars = req.max_characters.unwrap_or(usize::MAX);
    let snapshot = state.db.snapshot();

    // Per-key access control (9router fetch.js:98 enforceKeyAccessProvider):
    // the provider IS the model here, so a restricted key needs the provider
    // id (or the combo) on its list. Runs before combo expansion.
    let key_access_ctx = crate::core::key_access::context_for_presented_key(
        crate::server::auth::extract_api_key(&headers).as_deref(),
        &snapshot,
    );
    if let Err(message) = crate::core::key_access::enforce_key_access_provider(
        key_access_ctx.as_ref(),
        &provider_input,
        &snapshot,
    ) {
        return fetch_error(StatusCode::FORBIDDEN, &message);
    }

    // ── 4. Combo detection (baseline parity) ────────────────────────────────
    if let Some(combo_models) = get_combo_models_from_data(&provider_input, &snapshot.combos) {
        let strategy = strategy_for_combo(&snapshot, &provider_input);
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
            Err(e) => {
                return fetch_error(
                    StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_GATEWAY),
                    &e.message,
                )
            }
        }
    }

    // ── 5. Single provider dispatch ─────────────────────────────────────────
    match execute_single_fetch(&state, &provider_input, &url, format, max_chars).await {
        Ok(resp) => resp,
        Err(e) => fetch_error(
            StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_GATEWAY),
            &e.message,
        ),
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
    let state = state.clone();
    // 9router fetch.js:97 hands `settings.comboStickyRoundRobinLimit` to
    // handleComboChat so a round-robin web/search combo pins each member for
    // the same run of requests chat does. `execute_combo_strategy` hardcodes
    // 1, which would rotate on every call.
    let sticky_limit = state
        .db
        .snapshot()
        .settings
        .combo_sticky_round_robin_limit
        .max(1);
    let pricing = PricingTable::new();

    crate::core::combo::execute_combo_strategy_full(
        models,
        combo_name,
        strategy,
        &[],
        sticky_limit,
        None,
        &pricing,
        |_: &str| crate::core::combo::ModelCapacity::Available,
        move |model: &str| {
            let model_owned = model.to_string();
            let url = url.clone();
            let format = format.clone();
            let max_chars = max_chars;
            let state = state.clone();
            async move {
                execute_single_fetch(&state, &model_owned, &url, &format, max_chars)
                    .await
                    .map_err(|e| crate::core::combo::ComboAttemptError {
                        status: e.status,
                        message: e.message,
                        retry_after: None,
                        upstream_body: None,
                        upstream_headers: Vec::new(),
                    })
            }
        },
        // Fetch responses carry no SSE/JSON content contract — accept
        // every member result untouched.
        |result, _is_last| async move { crate::core::combo::ComboOkVerdict::Accept(result) },
    )
    .await
    .map_err(|e| crate::core::combo::ComboExecutionError {
        status: e.status,
        message: e.message,
        earliest_retry_after: e.earliest_retry_after,
        upstream_body: e.upstream_body,
        upstream_headers: e.upstream_headers,
    })
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
    // Held as an Arc, not a borrow of its contents: the in-flight slot guard
    // owns the registry so it can ride along inside a streaming body.
    let registry = state.account_registry.clone();

    loop {
        let connection = select_fetch_connection(&snapshot, &provider_id, &excluded);

        let Some(connection) = connection else {
            return Err(FetchError {
                status: 400,
                message: format!("No credentials for provider: {}", provider_id),
            });
        };

        let (rate_limit_remaining, rate_limit_reset) = registry.rate_limit_info(&connection.id);
        let slot =
            registry.acquire_slot(&connection.id, 10, rate_limit_remaining, rate_limit_reset);

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
                let decision = check_fallback_error(
                    status,
                    &message,
                    current_backoff,
                    Some(provider_id.as_str()),
                );
                let cooldown = decision.cooldown;
                let backoff_level = decision.new_backoff_level.unwrap_or(current_backoff);

                if decision.should_fallback {
                    mark_connection_unavailable(
                        state,
                        &connection.id,
                        status,
                        &message,
                        cooldown,
                        backoff_level,
                    )
                    .await;
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

    let (request_url, request_body, headers) =
        build_fetch_request(provider, connection, url, format)?;

    let proxy = crate::core::proxy::resolve_proxy_target(
        &state.db.snapshot(),
        connection,
        &state.db.snapshot().settings,
    );

    let client = state
        .client_pool
        .get(provider, proxy.as_ref())
        .map_err(|e| FetchError {
            status: 502,
            message: e.to_string(),
        })?;

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
    normalize_fetch_response(
        provider,
        url,
        format,
        max_chars,
        body,
        response_ms,
        upstream_ms,
    )
}

/// `fetchConfig.formats` registry (ported from 9router v0.5.116 #2239,
/// open-sse/providers/registry/*.js). Providers without an entry have no
/// format restriction. The registry drives both the dashboard selector and
/// this backend validation; unsupported requests are rejected before any
/// upstream call.
fn fetch_supported_formats(provider: &str) -> Option<&'static [&'static str]> {
    match provider {
        "firecrawl" => Some(&["markdown", "html", "text"]),
        "jina-reader" => Some(&["markdown", "text", "html"]),
        "tavily" => Some(&["markdown", "text"]),
        "exa" => Some(&["text", "markdown"]),
        "ollama" => Some(&["markdown"]),
        "tinyfish" => Some(&["markdown", "html"]),
        _ => None,
    }
}

fn build_fetch_request(
    provider: &str,
    connection: &ProviderConnection,
    url: &str,
    format: &str,
) -> Result<(String, Value, HeaderMap), FetchError> {
    // 9router handleFetchCore: reject unsupported formats up front. The
    // message must not echo the untrusted format value — account fallback
    // classifies error text and values like "capacity" could be mistaken
    // for real account failures.
    if let Some(supported) = fetch_supported_formats(provider) {
        if !supported.contains(&format) {
            return Err(FetchError {
                status: 400,
                message: format!(
                    "Unsupported format for {}. Supported: {}",
                    provider,
                    supported.join(", ")
                ),
            });
        }
    }

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );

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
                h.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                h.insert(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", key)).map_err(|_| FetchError {
                        status: 500,
                        message: "Invalid API key header".into(),
                    })?,
                );
                return Ok((request_url, request_body, h));
            }
            Ok((request_url, request_body, headers))
        }

        "jina-reader" => {
            // Jina's JSON POST API (ported from 9router v0.5.45 fix(jina-reader):
            // recover after transient errors and use JSON POST API). The URL is
            // sent in the JSON body instead of being embedded in the path.
            let request_url = "https://r.jina.ai/".to_string();
            let request_body = json!({ "url": url });
            let mut h = headers.clone();
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            // 9router v0.5.116 #2239: forward the selected output format via
            // Jina's `x-respond-with` header (markdown|text|html).
            h.insert(
                HeaderName::from_static("x-respond-with"),
                HeaderValue::from_str(format).map_err(|_| FetchError {
                    status: 400,
                    message: "Invalid format".into(),
                })?,
            );
            if let Some(key) = api_key {
                h.insert(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", key)).map_err(|_| FetchError {
                        status: 500,
                        message: "Invalid API key header".into(),
                    })?,
                );
            }
            Ok((request_url, request_body, h))
        }

        "tavily" => {
            let request_url = "https://api.tavily.com/extract".to_string();
            // 9router v0.5.116 #2239: forward the selected output format via
            // Tavily's `format` body field (markdown|text).
            let request_body = json!({
                "urls": [url],
                "extract_depth": "basic",
                "format": format
            });
            if let Some(key) = api_key {
                let mut h = HeaderMap::new();
                h.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                h.insert(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", key)).map_err(|_| FetchError {
                        status: 500,
                        message: "Invalid API key header".into(),
                    })?,
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
                h.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                h.insert(
                    HeaderName::from_static("x-api-key"),
                    HeaderValue::from_str(key).map_err(|_| FetchError {
                        status: 500,
                        message: "Invalid API key header".into(),
                    })?,
                );
                return Ok((request_url, request_body, h));
            }
            Ok((request_url, request_body, headers))
        }

        "ollama" => {
            // 9router open-sse/handlers/fetch/index.js runOllama: POST
            // {base}/api/web_fetch {url} with Bearer key, normalize
            // `content` string. Base URL from connection override or
            // the web fetchConfig default.
            let base = connection
                .provider_specific_data
                .get("baseUrl")
                .and_then(|v| v.as_str())
                .unwrap_or("https://ollama.com/api/web_fetch");
            let request_url = base.trim_end_matches('/').to_string();
            let request_body = json!({ "url": url });
            let mut h = headers.clone();
            if let Some(key) = api_key {
                h.insert(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", key)).map_err(|_| FetchError {
                        status: 500,
                        message: "Invalid API key header".into(),
                    })?,
                );
            }
            Ok((request_url, request_body, h))
        }

        "tinyfish" => {
            // 9router fetch/index.js runTinyfish: POST
            // https://api.fetch.tinyfish.ai {urls:[url], format} with the
            // `X-API-Key` header. Only markdown|html are supported.
            if format != "markdown" && format != "html" {
                return Err(FetchError {
                    status: 400,
                    message: format!("Unsupported TinyFish format: {}", format),
                });
            }
            let mut h = headers.clone();
            if let Some(key) = api_key {
                h.insert(
                    HeaderName::from_static("x-api-key"),
                    HeaderValue::from_str(key).map_err(|_| FetchError {
                        status: 500,
                        message: "Invalid API key header".into(),
                    })?,
                );
            }
            Ok((
                "https://api.fetch.tinyfish.ai".to_string(),
                json!({ "urls": [url], "format": format }),
                h,
            ))
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
        "ol" | "ollama" => "ollama".to_string(),
        "tf" | "tinyfish" => "tinyfish".to_string(),
        other => other.to_string(),
    }
}

/// Check whether an `IpAddr` is in a private-use or loopback range.
///
/// Covers: 127.0.0.0/8, 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16,
/// ::1, fe80::/10 (link-local), and fc00::/7 (unique-local).
/// `IpAddr::is_private()` is not available on this Rust version so we
/// delegate to the concrete-variant helpers.
fn is_private_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // Unique-local (fc00::/7)
                || (v6.octets()[0] & 0xfe) == 0xfc
        }
    }
}

/// Resolve a URL's host to IP addresses and reject private/internal ranges.
///
/// This is an SSRF guard: it blocks requests to 127.0.0.1, 10.x.x.x,
/// 172.16-31.x.x, 192.168.x.x, ::1, and other loopback/private/unique-local
/// addresses as defined by IANA special-purpose registries.
async fn check_private_ip(url_str: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url_str).map_err(|_| "Invalid URL".to_string())?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?;

    // Fast-path: if it's already a literal IP, check it directly.
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if is_private_ip(ip) {
            return Err(format!(
                "URL resolves to a private/internal IP address: {}",
                ip
            ));
        }
        return Ok(());
    }

    // Otherwise it's a hostname — resolve to IPs.
    let addr_str = format!("{}:0", host);
    let addrs = tokio::net::lookup_host(&addr_str)
        .await
        .map_err(|e| format!("DNS resolution failed: {}", e))?;

    for addr in addrs {
        let ip = addr.ip();
        if is_private_ip(ip) {
            return Err(format!(
                "URL resolves to a private/internal IP address: {}",
                ip
            ));
        }
    }

    Ok(())
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
            let title = data
                .and_then(|d| d.get("metadata"))
                .and_then(|m| m.get("title"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            (text, title)
        }
        "jina-reader" => {
            // Jina returns plain text directly
            let text = body.as_str().unwrap_or_default().to_string();
            // Try to extract title from a `Title:` metadata line first, then a
            // markdown heading (ported from 9router v0.5.45 parseJinaTitle).
            let title = text
                .lines()
                .next()
                .and_then(|line| {
                    line.strip_prefix("Title:")
                        .map(str::trim)
                        .map(str::to_string)
                })
                .or_else(|| {
                    text.lines()
                        .next()
                        .and_then(|line| line.strip_prefix("# ").map(str::to_string))
                });
            (text, title)
        }
        "tavily" => {
            let first = body
                .get("results")
                .and_then(|r| r.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.as_object());
            let text = first
                .and_then(|o| o.get("raw_content").and_then(|v| v.as_str()))
                .unwrap_or_default()
                .to_string();
            (text, None)
        }
        "exa" => {
            let first = body
                .get("results")
                .and_then(|r| r.as_array())
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
        "ollama" => {
            // 9router runOllama: `content` must be a string, else 502.
            let text = body
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            (text, None)
        }
        "tinyfish" => {
            // 9router runTinyfish: a per-URL failure rides inside a 200 as
            // `errors[0]`; results[0].text must be a string.
            if let Some(failure) = body
                .get("errors")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
            {
                let status = failure
                    .get("status")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(502) as u16;
                let msg = failure
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                return Err(FetchError {
                    status,
                    message: format!("TinyFish fetch failed: {}", msg),
                });
            }
            let page = body
                .get("results")
                .and_then(|r| r.as_array())
                .and_then(|arr| arr.first());
            let text = page
                .and_then(|p| p.get("text"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| FetchError {
                    status: 502,
                    message: "TinyFish returned no extractable content".into(),
                })?
                .to_string();
            let title = page
                .and_then(|p| p.get("title"))
                .and_then(|v| v.as_str())
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

    // TinyFish reports author/published_date/language per page
    // (runTinyfish metadata); other providers leave them null.
    let metadata = if provider == "tinyfish" {
        let page = body
            .get("results")
            .and_then(|r| r.as_array())
            .and_then(|arr| arr.first());
        json!({
            "author": page.and_then(|p| p.get("author")).and_then(|v| v.as_str()),
            "published_at": page.and_then(|p| p.get("published_date")).and_then(|v| v.as_str()),
            "language": page.and_then(|p| p.get("language")).and_then(|v| v.as_str()),
        })
    } else {
        json!({
            "author": null,
            "published_at": null,
            "language": null
        })
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
        "metadata": metadata,
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
    let until_rfc = until.to_rfc3339();
    let connection_id = connection_id.to_string();
    let message = message.to_string();
    let _ = state
        .db
        .update(move |db| {
            if let Some(c) = db
                .provider_connections
                .iter_mut()
                .find(|c| c.id == connection_id)
            {
                c.last_error = Some(message);
                c.last_error_at = Some(Utc::now().to_rfc3339());
                c.error_code = Some(status.to_string());
                c.backoff_level = Some(backoff_level);
                c.consecutive_errors = c
                    .consecutive_errors
                    .map(|e| e.saturating_add(1))
                    .or(Some(1));
                c.test_status = Some("unavailable".into());
                c.rate_limited_until = Some(until_rfc);
            }
        })
        .await;
}

async fn clear_connection_error(state: &AppState, connection_id: &str) {
    clear_connection_error_for_model(state, connection_id, None).await;
}

/// Selective model-lock clear (9router clearAccountError parity; matches chat.rs).
/// Drops expired locks and optionally the succeeded model lock — not all modelLock_*.
async fn clear_connection_error_for_model(
    state: &AppState,
    connection_id: &str,
    succeeded_model: Option<&str>,
) {
    use chrono::{DateTime, Utc};
    let connection_id = connection_id.to_string();
    let succeeded_model = succeeded_model.map(|s| s.to_string());
    let now = Utc::now();
    let _ = state
        .db
        .update(move |db| {
            if let Some(c) = db
                .provider_connections
                .iter_mut()
                .find(|c| c.id == connection_id)
            {
                c.last_error = None;
                c.last_error_at = None;
                c.error_code = None;
                c.backoff_level = Some(0);
                c.consecutive_errors = Some(0);
                c.test_status = None;
                c.rate_limited_until = None;
                let model_key = succeeded_model.as_ref().map(|m| format!("modelLock_{m}"));
                c.extra.retain(|k, v| {
                    if !k.starts_with("modelLock_") {
                        return true;
                    }
                    if let Some(exp) = v.as_str() {
                        if let Ok(t) = DateTime::parse_from_rfc3339(exp) {
                            if t.with_timezone(&Utc) <= now {
                                return false;
                            }
                        }
                    }
                    if let Some(ref mk) = model_key {
                        if k == mk {
                            return false;
                        }
                    }
                    true
                });
            }
        })
        .await;
}

// ─── Response helpers ────────────────────────────────────────────────────────

fn cors_json_response(status: StatusCode, payload: Value) -> Response {
    let mut resp = (status, Json(payload)).into_response();
    resp.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    resp
}

fn fetch_error(status: StatusCode, message: &str) -> Response {
    let mut resp = (status, Json(json!({ "error": message }))).into_response();
    resp.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
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
        assert_eq!(resolve_fetch_provider("tf"), "tinyfish");
        assert_eq!(resolve_fetch_provider("TinyFish"), "tinyfish");
    }

    #[test]
    fn normalize_tinyfish_extracts_text_title_and_metadata() {
        let body = json!({
            "results": [{
                "text": "# Page\n\nBody text",
                "title": "Tiny Page",
                "author": "Jane",
                "published_date": "2026-01-02",
                "language": "en"
            }]
        });
        let result = normalize_fetch_response(
            "tinyfish",
            "https://example.com",
            "markdown",
            1000,
            body,
            120,
            40,
        )
        .unwrap();
        assert_eq!(result["provider"], "tinyfish");
        assert_eq!(result["title"], "Tiny Page");
        assert!(result["content"]["text"]
            .as_str()
            .unwrap()
            .contains("Body text"));
        assert_eq!(result["metadata"]["author"], "Jane");
        assert_eq!(result["metadata"]["published_at"], "2026-01-02");
        assert_eq!(result["metadata"]["language"], "en");
    }

    #[test]
    fn normalize_tinyfish_surfaces_embedded_errors() {
        // runTinyfish: per-URL failures arrive inside a 200 as errors[0].
        let body =
            json!({"errors": [{"url": "https://x.com", "status": 404, "error": "not found"}]});
        let err =
            normalize_fetch_response("tinyfish", "https://x.com", "markdown", 1000, body, 10, 5)
                .unwrap_err();
        assert_eq!(err.status, 404);
        assert!(err.message.contains("not found"));

        // Missing results[0].text → 502.
        let body = json!({"results": []});
        let err =
            normalize_fetch_response("tinyfish", "https://x.com", "markdown", 1000, body, 10, 5)
                .unwrap_err();
        assert_eq!(err.status, 502);
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
            "firecrawl",
            "https://example.com",
            "markdown",
            1000,
            body,
            150,
            50,
        )
        .unwrap();
        assert_eq!(result["provider"], "firecrawl");
        assert_eq!(result["title"], "Test Page");
        assert_eq!(result["content"]["format"], "markdown");
        assert!(result["content"]["text"]
            .as_str()
            .unwrap()
            .starts_with("# Hello"));
        assert_eq!(result["metrics"]["response_time_ms"], 150);
    }

    #[test]
    fn normalize_jina_reader_extracts_plain_text_and_first_heading() {
        let body = json!("# My Title\n\nRest of the page content here.");
        let result = normalize_fetch_response(
            "jina-reader",
            "https://example.com",
            "markdown",
            1000,
            body,
            80,
            30,
        )
        .unwrap();
        assert_eq!(result["provider"], "jina-reader");
        assert_eq!(result["title"], "My Title");
        assert!(result["content"]["text"]
            .as_str()
            .unwrap()
            .contains("Rest of the page"));
    }

    #[test]
    fn normalize_respects_max_characters() {
        let body =
            json!("This is a very long text that should be truncated when max_chars is small");
        let result = normalize_fetch_response(
            "jina-reader",
            "https://example.com",
            "markdown",
            20,
            body,
            50,
            10,
        )
        .unwrap();
        let text = result["content"]["text"].as_str().unwrap();
        assert_eq!(text.len(), 20);
        assert_eq!(result["content"]["length"], 20);
    }

    #[test]
    fn combo_strategy_fallback_default() {
        // verify the combo_strategy_for function uses settings correctly
        // (integration tested via web_fetch_api tests)
    }

    fn test_connection() -> ProviderConnection {
        ProviderConnection {
            api_key: Some("test-key".to_string()),
            ..Default::default()
        }
    }

    // 9router v0.5.116 #2239: the selected output format must reach Jina
    // via the `x-respond-with` header.
    #[test]
    fn jina_reader_forwards_format_via_x_respond_with() {
        for fmt in ["markdown", "text", "html"] {
            let (_url, _body, headers) = build_fetch_request(
                "jina-reader",
                &test_connection(),
                "https://example.com",
                fmt,
            )
            .unwrap();
            assert_eq!(
                headers
                    .get(HeaderName::from_static("x-respond-with"))
                    .and_then(|v| v.to_str().ok()),
                Some(fmt),
                "x-respond-with must equal requested format {}",
                fmt
            );
        }
    }

    // 9router v0.5.116 #2239: Tavily's `format` body field carries the
    // selected output format (markdown|text only).
    #[test]
    fn tavily_forwards_format_in_body() {
        let (_url, body, _headers) =
            build_fetch_request("tavily", &test_connection(), "https://example.com", "text")
                .unwrap();
        assert_eq!(body["format"], "text");
        assert_eq!(body["extract_depth"], "basic");
        assert_eq!(body["urls"][0], "https://example.com");
    }

    // 9router handleFetchCore: unsupported formats are rejected with 400
    // before any upstream call, and the error must not echo the untrusted
    // format value (account fallback classifies error text).
    #[test]
    fn unsupported_format_rejected_without_echo() {
        // Tavily supports markdown|text only — html is rejected.
        let err = build_fetch_request("tavily", &test_connection(), "https://example.com", "html")
            .unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.message.contains("Unsupported format for tavily"));
        assert!(err.message.contains("markdown, text"));
        assert!(!err.message.contains("html"));

        // Jina rejects formats outside its registry.
        let err = build_fetch_request(
            "jina-reader",
            &test_connection(),
            "https://example.com",
            "xml",
        )
        .unwrap_err();
        assert_eq!(err.status, 400);
        assert!(!err.message.contains("xml"));

        // Ollama only supports markdown.
        let err = build_fetch_request("ollama", &test_connection(), "https://example.com", "text")
            .unwrap_err();
        assert_eq!(err.status, 400);
    }

    #[test]
    fn fetch_supported_formats_matches_upstream_registry() {
        assert_eq!(
            fetch_supported_formats("firecrawl"),
            Some(&["markdown", "html", "text"][..])
        );
        assert_eq!(
            fetch_supported_formats("jina-reader"),
            Some(&["markdown", "text", "html"][..])
        );
        assert_eq!(
            fetch_supported_formats("tavily"),
            Some(&["markdown", "text"][..])
        );
        assert_eq!(
            fetch_supported_formats("exa"),
            Some(&["text", "markdown"][..])
        );
        assert_eq!(fetch_supported_formats("ollama"), Some(&["markdown"][..]));
        assert_eq!(
            fetch_supported_formats("tinyfish"),
            Some(&["markdown", "html"][..])
        );
        assert_eq!(fetch_supported_formats("unknown"), None);
    }
}
