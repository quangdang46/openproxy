use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{routing, Json, Router};
use bytes::Bytes;
use chrono::{Duration as ChronoDuration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tokio::time::{self, Duration};

use crate::core::usage::{DailyUsageSummary, Pricing, ProviderUsage, UsageTracker};
use crate::server::auth::require_dashboard_session;
use crate::server::state::AppState;
use crate::server::usage_live::UsageEvent;
use crate::server::usage_stream::{build_usage_stats, UsagePeriod, UsageStatsPayload};
use crate::types::{TokenUsage, UsageDb, UsageEntry};

fn require_usage_access(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    super::require_dashboard_or_management_api_key(headers, state)
}

fn is_usage_apikey_provider(provider: &str) -> bool {
    matches!(provider, "glm" | "glm-cn" | "minimax" | "minimax-cn")
}

fn usage_message_for_provider(provider: &str) -> String {
    match provider {
        "github" => "GitHub Copilot connected. Usage tracked per request.".to_string(),
        "gemini-cli" => {
            "Gemini CLI uses Google Cloud quotas. Check Google Cloud Console for details."
                .to_string()
        }
        "antigravity" => "Antigravity connected. Usage tracked per request.".to_string(),
        "claude" => "Claude connected. Usage tracked per request.".to_string(),
        "codex" => "Codex connected. Check OpenAI dashboard for usage.".to_string(),
        "kiro" => "Kiro connected. Usage tracked per request.".to_string(),
        "qwen" => "Qwen connected. Usage tracked per request.".to_string(),
        "iflow" => "iFlow connected. Usage tracked per request.".to_string(),
        "ollama" => "Ollama Cloud uses a free tier with light usage limits (resets every 5h & 7d). For detailed usage tracking, visit ollama.com/settings/keys.".to_string(),
        other => format!("Usage API not implemented for {other}"),
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        // v1 routes
        .route("/v1/usage", routing::get(get_usage))
        .route("/v1/usage/summary", routing::get(get_usage_summary))
        .route("/v1/usage/history", routing::get(get_usage_history))
        .route("/v1/usage/daily", routing::get(get_usage_daily))
        .route("/v1/usage/pricing", routing::get(get_pricing))
        // api/usage routes (mirror v1 for dashboard compatibility)
        .route("/api/usage", routing::get(get_usage))
        .route("/api/usage/stats", routing::get(get_usage_stats))
        .route("/api/usage/summary", routing::get(get_usage_summary))
        .route("/api/usage/history", routing::get(get_usage_history))
        .route("/api/usage/daily", routing::get(get_usage_daily))
        .route("/api/usage/pricing", routing::get(get_pricing))
        .route("/api/usage/stream", routing::get(stream_usage_stats))
        // Additional dashboard endpoints
        .route(
            "/api/usage/{connection_id}",
            routing::get(get_connection_usage),
        )
        .route("/api/usage/chart", routing::get(get_usage_chart))
        .route("/api/usage/providers", routing::get(get_usage_by_provider))
        .route(
            "/api/usage/request-details",
            routing::get(get_request_details),
        )
        .route("/api/usage/logs", routing::get(get_usage_logs))
        .route("/api/usage/request-logs", routing::get(get_usage_logs))
}

async fn get_usage(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_usage_access(&headers, &state) {
        return response;
    }

    let tracker = UsageTracker::new(state.db.clone());
    let summary = tracker.summarize();
    Json(summary).into_response()
}

#[derive(Debug, Deserialize)]
struct StatsQuery {
    period: Option<String>,
}

async fn get_usage_stats(
    State(state): State<AppState>,
    Query(query): Query<StatsQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = require_dashboard_session(&headers, &state.db) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": error.message() })),
        )
            .into_response();
    }

    let period = match query.period.as_deref().unwrap_or("7d") {
        value @ ("24h" | "7d" | "30d" | "60d" | "all") => {
            UsagePeriod::parse(value).expect("validated usage period must parse")
        }
        _ => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Invalid period. Use one of: 24h, 7d, 30d, 60d, all"
                })),
            )
                .into_response()
        }
    };

    let payload = build_dashboard_usage_stats(&state, period).await;
    Json(payload).into_response()
}

async fn stream_usage_stats(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_dashboard_session(&headers, &state.db) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": error.message() })),
        )
            .into_response();
    }

    let encoder = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    let mut receiver = state.usage_live.subscribe();
    let stream_state = state.clone();

    let body = Body::from_stream(async_stream::stream! {
        let _encode_guard = encoder.lock().await;
        let period = UsagePeriod::Last7Days;
        let mut cached_stats = Some(build_dashboard_usage_stats(&stream_state, period).await);
        if let Some(initial) = &cached_stats {
            yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("data: {}\n\n", serde_json::to_string(initial).unwrap_or_else(|_| "{}".to_string()))));
        }
        let mut keepalive = time::interval(Duration::from_secs(25));

        loop {
            tokio::select! {
                _ = keepalive.tick() => {
                    yield Ok(Bytes::from_static(b": ping\n\n"));
                }
                event = receiver.recv() => {
                    match event {
                        Ok(UsageEvent::Update) => {
                            let fresh = build_dashboard_usage_stats(&stream_state, period).await;
                            let payload = serde_json::to_string(&fresh).unwrap_or_else(|_| "{}".to_string());
                            cached_stats = Some(fresh);
                            yield Ok(Bytes::from(format!("data: {}\n\n", payload)));
                        }
                        Ok(UsageEvent::Pending) => {
                            let pending = stream_state.usage_live.pending_snapshot().await;
                            let active_requests = build_active_requests(&stream_state).await;
                            let error_provider = stream_state.usage_live.error_provider().await;
                            if let Some(mut stats) = cached_stats.clone() {
                                stats.pending = pending;
                                stats.active_requests = active_requests;
                                stats.recent_requests = crate::server::usage_stream::build_recent_requests(&stream_state.usage_tracker().get_usage_db().history);
                                stats.error_provider = error_provider;
                                let payload = serde_json::to_string(&stats).unwrap_or_else(|_| "{}".to_string());
                                cached_stats = Some(stats);
                                yield Ok(Bytes::from(format!("data: {}\n\n", payload)));
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let fresh = build_dashboard_usage_stats(&stream_state, period).await;
                            let payload = serde_json::to_string(&fresh).unwrap_or_else(|_| "{}".to_string());
                            cached_stats = Some(fresh);
                            yield Ok(Bytes::from(format!("data: {}\n\n", payload)));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    });

    (
        [
            (axum::http::header::CONTENT_TYPE, "text/event-stream"),
            (axum::http::header::CACHE_CONTROL, "no-cache"),
            (axum::http::header::CONNECTION, "keep-alive"),
        ],
        body,
    )
        .into_response()
}

async fn build_dashboard_usage_stats(state: &AppState, period: UsagePeriod) -> UsageStatsPayload {
    let snapshot = state.db.snapshot();
    let usage_db = state.usage_tracker().get_usage_db();
    let pending = state.usage_live.pending_snapshot().await;
    let active_requests = build_active_requests(state).await;
    let error_provider = state.usage_live.error_provider().await;

    build_usage_stats(
        period,
        &usage_db,
        &snapshot.provider_connections,
        &snapshot.provider_nodes,
        &snapshot.api_keys,
        pending,
        active_requests,
        error_provider,
    )
}

async fn build_active_requests(state: &AppState) -> Vec<crate::server::usage_live::ActiveRequest> {
    let snapshot = state.db.snapshot();
    let connection_names = snapshot
        .provider_connections
        .iter()
        .map(|connection| {
            let name = connection
                .name
                .clone()
                .or_else(|| connection.email.clone())
                .unwrap_or_else(|| connection.id.clone());
            (connection.id.clone(), name)
        })
        .collect();
    state.usage_live.active_requests(&connection_names).await
}

async fn get_usage_summary(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_usage_access(&headers, &state) {
        return response;
    }

    let tracker = UsageTracker::new(state.db.clone());
    let usage_db = tracker.get_usage_db();

    let mut total_prompt = 0u64;
    let mut total_completion = 0u64;
    let mut total_cost = 0.0;

    for entry in &usage_db.history {
        if let Some(tokens) = &entry.tokens {
            total_prompt += tokens.prompt_tokens.or(tokens.input_tokens).unwrap_or(0);
            total_completion += tokens
                .completion_tokens
                .or(tokens.output_tokens)
                .unwrap_or(0);
        }
        total_cost += entry.cost.unwrap_or(0.0);
    }

    let summary = UsageSummaryCompact {
        total_requests: usage_db.total_requests_lifetime,
        total_prompt_tokens: total_prompt,
        total_completion_tokens: total_completion,
        total_cost,
    };

    Json(summary).into_response()
}

async fn get_usage_history(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_usage_access(&headers, &state) {
        return response;
    }

    let tracker = UsageTracker::new(state.db.clone());
    let usage_db = tracker.get_usage_db();

    #[derive(Serialize)]
    struct HistoryResponse {
        total_requests: u64,
        history: Vec<UsageEntryDto>,
    }

    #[derive(Serialize)]
    struct UsageEntryDto {
        timestamp: Option<String>,
        provider: Option<String>,
        model: String,
        prompt_tokens: u64,
        completion_tokens: u64,
        cost: f64,
    }

    let history: Vec<_> = usage_db
        .history
        .iter()
        .map(|e| UsageEntryDto {
            timestamp: e.timestamp.clone(),
            provider: e.provider.clone(),
            model: e.model.clone(),
            prompt_tokens: e
                .tokens
                .as_ref()
                .and_then(|t| t.prompt_tokens.or(t.input_tokens))
                .unwrap_or(0),
            completion_tokens: e
                .tokens
                .as_ref()
                .and_then(|t| t.completion_tokens.or(t.output_tokens))
                .unwrap_or(0),
            cost: e.cost.unwrap_or(0.0),
        })
        .collect();

    Json(HistoryResponse {
        total_requests: usage_db.total_requests_lifetime,
        history,
    })
    .into_response()
}

async fn get_usage_daily(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_usage_access(&headers, &state) {
        return response;
    }

    let tracker = UsageTracker::new(state.db.clone());
    let usage_db = tracker.get_usage_db();

    let daily: Vec<_> = usage_db
        .daily_summary
        .iter()
        .map(|(date, summary)| DailyUsageSummary {
            date: date.clone(),
            requests: summary.requests,
            prompt_tokens: summary.prompt_tokens,
            completion_tokens: summary.completion_tokens,
            cost: summary.cost,
            by_provider: summary
                .by_provider
                .iter()
                .map(|(provider, counter)| ProviderUsage {
                    provider: provider.clone(),
                    requests: counter.requests,
                    prompt_tokens: counter.prompt_tokens,
                    completion_tokens: counter.completion_tokens,
                    cost: counter.cost,
                })
                .collect(),
        })
        .collect();

    Json(daily).into_response()
}

async fn get_pricing(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_usage_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let pricing = if snapshot.pricing.is_empty() {
        Pricing::default()
    } else {
        Pricing::from_db(&snapshot.pricing)
    };

    Json(pricing).into_response()
}

#[derive(Serialize)]
struct UsageSummaryCompact {
    total_requests: u64,
    total_prompt_tokens: u64,
    total_completion_tokens: u64,
    total_cost: f64,
}

// Handler for GET /api/usage/:connection_id
async fn get_connection_usage(
    State(state): State<AppState>,
    axum::extract::Path(connection_id): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_usage_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let Some(connection) = snapshot
        .provider_connections
        .iter()
        .find(|entry| entry.id == connection_id)
    else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "Connection not found" })),
        )
            .into_response();
    };

    let is_oauth = connection.auth_type == "oauth";
    let is_apikey_eligible =
        connection.auth_type == "apikey" && is_usage_apikey_provider(&connection.provider);
    if !is_oauth && !is_apikey_eligible {
        return Json(serde_json::json!({
            "message": "Usage not available for this connection"
        }))
        .into_response();
    }

    let tracker = UsageTracker::new(state.db.clone());
    let usage_db = tracker.get_usage_db();

    let mut prompt = 0u64;
    let mut completion = 0u64;
    let mut cost = 0.0;
    let mut request_count = 0u64;

    for entry in &usage_db.history {
        if entry.connection_id.as_deref() == Some(&connection_id) {
            request_count += 1;
            if let Some(tokens) = &entry.tokens {
                prompt += tokens.prompt_tokens.or(tokens.input_tokens).unwrap_or(0);
                completion += tokens
                    .completion_tokens
                    .or(tokens.output_tokens)
                    .unwrap_or(0);
            }
            cost += entry.cost.unwrap_or(0.0);
        }
    }

    Json(ConnectionUsageResponse {
        connection_id,
        total_requests: request_count,
        total_prompt_tokens: prompt,
        total_completion_tokens: completion,
        total_cost: cost,
        message: usage_message_for_provider(&connection.provider),
        quotas: serde_json::json!({}),
    })
    .into_response()
}

// Handler for GET /api/usage/chart?period=X
#[derive(Debug, Deserialize)]
struct ChartQuery {
    period: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageChartBucket {
    label: String,
    tokens: u64,
    cost: f64,
}

async fn get_usage_chart(
    State(state): State<AppState>,
    Query(params): Query<ChartQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_usage_access(&headers, &state) {
        return response;
    }

    let period = params.period.as_deref().unwrap_or("7d");
    if !matches!(period, "24h" | "7d" | "30d" | "60d") {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Invalid period" })),
        )
            .into_response();
    }

    let tracker = UsageTracker::new(state.db.clone());
    let usage_db = tracker.get_usage_db();
    Json(build_usage_chart(&usage_db, period)).into_response()
}

fn build_usage_chart(usage_db: &UsageDb, period: &str) -> Vec<UsageChartBucket> {
    if period == "24h" {
        let now = Utc::now();
        let bucket_count = 24usize;
        let bucket_ms = 60 * 60 * 1000_i64;
        let start = now - ChronoDuration::hours(bucket_count as i64);

        let mut buckets = (0..bucket_count)
            .map(|index| {
                let ts = start + ChronoDuration::hours(index as i64);
                UsageChartBucket {
                    label: ts.format("%H:%M").to_string(),
                    tokens: 0,
                    cost: 0.0,
                }
            })
            .collect::<Vec<_>>();

        for entry in &usage_db.history {
            let Some(timestamp) = entry.timestamp.as_deref().and_then(parse_usage_timestamp) else {
                continue;
            };
            if timestamp < start || timestamp > now {
                continue;
            }

            let delta_ms = timestamp.timestamp_millis() - start.timestamp_millis();
            let index = (delta_ms / bucket_ms).clamp(0, (bucket_count - 1) as i64) as usize;
            buckets[index].tokens += usage_prompt_tokens(entry) + usage_completion_tokens(entry);
            buckets[index].cost += entry.cost.unwrap_or(0.0);
        }

        return buckets;
    }

    let bucket_count = match period {
        "7d" => 7,
        "30d" => 30,
        "60d" => 60,
        _ => 7,
    };
    let today = Utc::now().date_naive();

    (0..bucket_count)
        .map(|index| {
            let date = today - ChronoDuration::days((bucket_count - 1 - index) as i64);
            let date_key = date.format("%Y-%m-%d").to_string();
            let summary = usage_db.daily_summary.get(&date_key);

            UsageChartBucket {
                label: format_daily_chart_label(date),
                tokens: summary
                    .map(|day| day.prompt_tokens + day.completion_tokens)
                    .unwrap_or(0),
                cost: summary.map(|day| day.cost).unwrap_or(0.0),
            }
        })
        .collect()
}

// Handler for GET /api/usage/providers
async fn get_usage_by_provider(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_usage_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let usage_db = state.usage_tracker().get_usage_db();
    let providers = usage_provider_options(&usage_db, &snapshot.provider_nodes);

    Json(UsageProvidersPayload { providers }).into_response()
}

// Handler for GET /api/usage/request-details
async fn get_request_details(
    State(state): State<AppState>,
    Query(query): Query<RequestDetailsQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_usage_access(&headers, &state) {
        return response;
    }

    let page = query.page.unwrap_or(1);
    if page == 0 {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Page must be >= 1" })),
        )
            .into_response();
    }

    let page_size = query.page_size.unwrap_or(20);
    if !(1..=100).contains(&page_size) {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "PageSize must be between 1 and 100" })),
        )
            .into_response();
    }

    let usage_db = state.usage_tracker().get_usage_db();
    let mut details = build_request_detail_records(&usage_db);

    if let Some(provider) = query
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        details.retain(|detail| detail.provider == provider);
    }
    if let Some(model) = query
        .model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        details.retain(|detail| detail.model == model);
    }
    if let Some(connection_id) = query
        .connection_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        details.retain(|detail| detail.connection_id.as_deref() == Some(connection_id));
    }
    if let Some(status) = query
        .status
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        details.retain(|detail| detail.status == status);
    }
    if let Some(start_date) = query.start_date.as_deref().and_then(parse_usage_timestamp) {
        details.retain(|detail| {
            parse_usage_timestamp(&detail.timestamp)
                .is_some_and(|timestamp| timestamp >= start_date)
        });
    }
    if let Some(end_date) = query.end_date.as_deref().and_then(parse_usage_timestamp) {
        details.retain(|detail| {
            parse_usage_timestamp(&detail.timestamp).is_some_and(|timestamp| timestamp <= end_date)
        });
    }

    let total_items = details.len();
    let total_pages = if total_items == 0 {
        0
    } else {
        total_items.div_ceil(page_size)
    };
    let start_index = (page - 1) * page_size;
    let paged = if start_index >= total_items {
        Vec::new()
    } else {
        details
            .into_iter()
            .skip(start_index)
            .take(page_size)
            .collect::<Vec<_>>()
    };

    Json(RequestDetailsPayload {
        details: paged,
        pagination: RequestDetailsPagination {
            page,
            page_size,
            total_items,
            total_pages,
            has_next: page < total_pages,
            has_prev: page > 1 && total_pages > 0,
        },
    })
    .into_response()
}

async fn get_usage_logs(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let usage_db = state.usage_tracker().get_usage_db();
    let logs: Vec<_> = usage_db
        .history
        .iter()
        .rev()
        .take(200)
        .map(|entry| format_usage_log(entry, &snapshot.provider_connections))
        .collect();

    Json(logs).into_response()
}

fn format_usage_log(
    entry: &crate::types::UsageEntry,
    connections: &[crate::types::ProviderConnection],
) -> String {
    let timestamp = entry.timestamp.as_deref().unwrap_or("-");
    let model = if entry.model.is_empty() {
        "-"
    } else {
        entry.model.as_str()
    };
    let provider = entry.provider.as_deref().unwrap_or("-");
    let account = entry
        .connection_id
        .as_deref()
        .and_then(|id| {
            connections
                .iter()
                .find(|connection| connection.id == id)
                .map(|connection| {
                    connection
                        .name
                        .clone()
                        .or_else(|| connection.email.clone())
                        .unwrap_or_else(|| id.chars().take(8).collect())
                })
        })
        .unwrap_or_else(|| "-".to_string());
    let sent = entry
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.prompt_tokens.or(tokens.input_tokens))
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let received = entry
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.completion_tokens.or(tokens.output_tokens))
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let status = match entry.status.as_deref() {
        Some("success") => "OK".to_string(),
        Some(value) if value.eq_ignore_ascii_case("ok") => "OK".to_string(),
        Some(value) => value.to_string(),
        None => "OK".to_string(),
    };

    format!("{timestamp} | {model} | {provider} | {account} | {sent} | {received} | {status}")
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionUsageResponse {
    connection_id: String,
    total_requests: u64,
    total_prompt_tokens: u64,
    total_completion_tokens: u64,
    total_cost: f64,
    message: String,
    quotas: Value,
}

#[derive(Debug, Serialize)]
struct UsageProvidersPayload {
    providers: Vec<UsageProviderOption>,
}

#[derive(Debug, Serialize)]
struct UsageProviderOption {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestDetailsQuery {
    page: Option<usize>,
    page_size: Option<usize>,
    provider: Option<String>,
    model: Option<String>,
    connection_id: Option<String>,
    status: Option<String>,
    start_date: Option<String>,
    end_date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RequestLatency {
    ttft: u64,
    total: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestDetailsPayload {
    details: Vec<RequestDetailRecord>,
    pagination: RequestDetailsPagination,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestDetailsPagination {
    page: usize,
    page_size: usize,
    total_items: usize,
    total_pages: usize,
    has_next: bool,
    has_prev: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestDetailRecord {
    id: String,
    provider: String,
    model: String,
    connection_id: Option<String>,
    timestamp: String,
    status: String,
    latency: RequestLatency,
    tokens: TokenUsage,
    request: Option<Value>,
    provider_request: Option<Value>,
    provider_response: Option<Value>,
    response: Option<Value>,
    endpoint: Option<String>,
}

fn usage_provider_options(
    usage_db: &UsageDb,
    provider_nodes: &[crate::types::ProviderNode],
) -> Vec<UsageProviderOption> {
    let provider_node_names = provider_nodes
        .iter()
        .map(|node| (node.id.as_str(), node.name.as_str()))
        .collect::<HashMap<_, _>>();
    let provider_ids = build_request_detail_records(usage_db)
        .into_iter()
        .map(|detail| detail.provider)
        .filter(|provider| !provider.is_empty())
        .collect::<BTreeSet<_>>();

    provider_ids
        .into_iter()
        .map(|provider_id| UsageProviderOption {
            name: provider_node_names
                .get(provider_id.as_str())
                .map(|name| (*name).to_string())
                .unwrap_or_else(|| provider_id.clone()),
            id: provider_id,
        })
        .collect()
}

fn build_request_detail_records(usage_db: &UsageDb) -> Vec<RequestDetailRecord> {
    let mut details = usage_db
        .history
        .iter()
        .enumerate()
        .map(|(index, entry)| RequestDetailRecord {
            id: entry
                .extra
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| fallback_request_detail_id(entry, index)),
            provider: entry.provider.clone().unwrap_or_default(),
            model: entry.model.clone(),
            connection_id: entry.connection_id.clone(),
            timestamp: entry
                .timestamp
                .clone()
                .unwrap_or_else(|| Utc::now().to_rfc3339()),
            status: entry
                .status
                .clone()
                .unwrap_or_else(|| "success".to_string()),
            latency: request_latency_from_extra(&entry.extra),
            tokens: usage_tokens(entry),
            request: entry.extra.get("request").cloned(),
            provider_request: entry.extra.get("providerRequest").cloned(),
            provider_response: entry.extra.get("providerResponse").cloned(),
            response: entry.extra.get("response").cloned(),
            endpoint: entry.endpoint.clone(),
        })
        .collect::<Vec<_>>();

    details.sort_by(|left, right| right.timestamp.cmp(&left.timestamp));
    details
}

fn usage_tokens(entry: &UsageEntry) -> TokenUsage {
    entry.tokens.clone().unwrap_or(TokenUsage {
        prompt_tokens: None,
        input_tokens: None,
        completion_tokens: None,
        output_tokens: None,
        total_tokens: None,
        extra: BTreeMap::new(),
    })
}

fn request_latency_from_extra(extra: &BTreeMap<String, Value>) -> RequestLatency {
    extra
        .get("latency")
        .cloned()
        .and_then(|value| serde_json::from_value::<RequestLatency>(value).ok())
        .unwrap_or_default()
}

fn fallback_request_detail_id(entry: &UsageEntry, index: usize) -> String {
    let timestamp = entry.timestamp.as_deref().unwrap_or("unknown");
    format!(
        "{timestamp}-{index}-{}",
        entry
            .model
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
            .collect::<String>()
    )
}

fn parse_usage_timestamp(value: &str) -> Option<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn format_daily_chart_label(date: NaiveDate) -> String {
    date.format("%b %-d").to_string()
}

fn usage_prompt_tokens(entry: &UsageEntry) -> u64 {
    entry
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.prompt_tokens.or(tokens.input_tokens))
        .unwrap_or(0)
}

fn usage_completion_tokens(entry: &UsageEntry) -> u64 {
    entry
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.completion_tokens.or(tokens.output_tokens))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usage_routes_defined() {
        let _app = routes();
    }

    #[test]
    fn test_connection_usage_response_serialization() {
        let response = ConnectionUsageResponse {
            connection_id: "test-conn-123".to_string(),
            total_requests: 42,
            total_prompt_tokens: 1000,
            total_completion_tokens: 500,
            total_cost: 0.25,
            message: "ok".to_string(),
            quotas: serde_json::json!({}),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("test-conn-123"));
        assert!(json.contains("42"));
    }

    #[test]
    fn test_chart_bucket_serialization() {
        let point = UsageChartBucket {
            label: "Jan 15".to_string(),
            tokens: 7500,
            cost: 1.50,
        };
        let json = serde_json::to_string(&point).unwrap();
        assert!(json.contains("Jan 15"));
        assert!(json.contains("7500"));
    }

    #[test]
    fn test_request_detail_record_serialization() {
        let detail = RequestDetailRecord {
            id: "detail-1".to_string(),
            timestamp: "2024-01-15T10:30:00Z".to_string(),
            provider: "openai".to_string(),
            model: "gpt-4".to_string(),
            connection_id: Some("conn-456".to_string()),
            endpoint: Some("/v1/chat/completions".to_string()),
            status: "success".to_string(),
            latency: RequestLatency {
                ttft: 120,
                total: 320,
            },
            tokens: TokenUsage {
                prompt_tokens: Some(100),
                input_tokens: None,
                completion_tokens: Some(50),
                output_tokens: None,
                total_tokens: Some(150),
                extra: BTreeMap::new(),
            },
            request: Some(serde_json::json!({ "input": "hello" })),
            provider_request: None,
            provider_response: None,
            response: Some(serde_json::json!({ "content": "world" })),
        };
        let json = serde_json::to_string(&detail).unwrap();
        assert!(json.contains("gpt-4"));
        assert!(json.contains("conn-456"));
    }

    #[test]
    fn test_chart_query_deserialization() {
        let json = r#"{"period":"30d"}"#;
        let query: ChartQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.period, Some("30d".to_string()));
    }

    #[test]
    fn test_chart_query_default_period() {
        let json = r#"{}"#;
        let query: ChartQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.period, None);
    }

    #[test]
    fn test_usage_summary_compact_serialization() {
        let summary = UsageSummaryCompact {
            total_requests: 1000,
            total_prompt_tokens: 50000,
            total_completion_tokens: 25000,
            total_cost: 10.50,
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("1000"));
        assert!(json.contains("10.5"));
    }

    #[test]
    fn test_build_usage_chart_daily_bucket_count_matches_requested_period() {
        let buckets = build_usage_chart(&UsageDb::default(), "30d");
        assert_eq!(buckets.len(), 30);
    }
}
