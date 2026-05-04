use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{routing, Json, Router};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::time::{self, Duration};

use crate::core::usage::{DailyUsageSummary, Pricing, ProviderUsage, UsageTracker};
use crate::server::auth::{require_api_key, require_dashboard_session};
use crate::server::state::AppState;
use crate::server::usage_live::UsageEvent;
use crate::server::usage_stream::{build_usage_stats, UsagePeriod, UsageStatsPayload};

use super::auth_error_response;

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
}

async fn get_usage(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
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
        value @ ("24h" | "7d" | "30d" | "60d" | "all") => UsagePeriod::parse(value)
            .expect("validated usage period must parse"),
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

async fn build_dashboard_usage_stats(
    state: &AppState,
    period: UsagePeriod,
) -> UsageStatsPayload {
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
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
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
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
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
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
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
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
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
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
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
    })
    .into_response()
}

#[derive(Serialize)]
struct ConnectionUsageResponse {
    connection_id: String,
    total_requests: u64,
    total_prompt_tokens: u64,
    total_completion_tokens: u64,
    total_cost: f64,
}

// Handler for GET /api/usage/chart?period=X
#[derive(Debug, Deserialize)]
struct ChartQuery {
    period: Option<String>,
}

async fn get_usage_chart(
    State(state): State<AppState>,
    Query(params): Query<ChartQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
    }

    let tracker = UsageTracker::new(state.db.clone());
    let usage_db = tracker.get_usage_db();

    let period = params.period.as_deref().unwrap_or("7d");
    let days_to_show = match period {
        "24h" => 1,
        "7d" => 7,
        "30d" => 30,
        "90d" => 90,
        _ => 7,
    };

    // Build chart data from daily summary
    let chart_data: Vec<ChartDataPoint> = usage_db
        .daily_summary
        .iter()
        .rev()
        .take(days_to_show)
        .map(|(date, summary)| ChartDataPoint {
            date: date.clone(),
            requests: summary.requests,
            prompt_tokens: summary.prompt_tokens,
            completion_tokens: summary.completion_tokens,
            cost: summary.cost,
        })
        .collect();

    Json(ChartResponse { data: chart_data }).into_response()
}

#[derive(Serialize)]
struct ChartResponse {
    data: Vec<ChartDataPoint>,
}

#[derive(Serialize)]
struct ChartDataPoint {
    date: String,
    requests: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    cost: f64,
}

// Handler for GET /api/usage/providers
async fn get_usage_by_provider(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
    }

    let tracker = UsageTracker::new(state.db.clone());
    let usage_db = tracker.get_usage_db();

    let mut provider_stats: HashMap<String, ProviderStats> = HashMap::new();

    for entry in &usage_db.history {
        let provider = entry
            .provider
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let stats = provider_stats.entry(provider).or_insert(ProviderStats {
            provider: entry
                .provider
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            total_requests: 0,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            total_cost: 0.0,
        });
        stats.total_requests += 1;
        if let Some(tokens) = &entry.tokens {
            stats.total_prompt_tokens += tokens.prompt_tokens.or(tokens.input_tokens).unwrap_or(0);
            stats.total_completion_tokens += tokens
                .completion_tokens
                .or(tokens.output_tokens)
                .unwrap_or(0);
        }
        stats.total_cost += entry.cost.unwrap_or(0.0);
    }

    let providers: Vec<_> = provider_stats.into_values().collect();
    Json(ProvidersUsageResponse { providers }).into_response()
}

#[derive(Serialize)]
struct ProvidersUsageResponse {
    providers: Vec<ProviderStats>,
}

#[derive(Serialize)]
struct ProviderStats {
    provider: String,
    total_requests: u64,
    total_prompt_tokens: u64,
    total_completion_tokens: u64,
    total_cost: f64,
}

// Handler for GET /api/usage/request-details
async fn get_request_details(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
    }

    let tracker = UsageTracker::new(state.db.clone());
    let usage_db = tracker.get_usage_db();

    let details: Vec<_> = usage_db
        .history
        .iter()
        .rev()
        .take(100)
        .map(|e| RequestDetail {
            timestamp: e.timestamp.clone(),
            provider: e.provider.clone(),
            model: e.model.clone(),
            connection_id: e.connection_id.clone(),
            endpoint: e.endpoint.clone(),
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
            status: e.status.clone(),
        })
        .collect();

    Json(RequestDetailsResponse { requests: details }).into_response()
}

#[derive(Serialize)]
struct RequestDetailsResponse {
    requests: Vec<RequestDetail>,
}

#[derive(Serialize)]
struct RequestDetail {
    timestamp: Option<String>,
    provider: Option<String>,
    model: String,
    connection_id: Option<String>,
    endpoint: Option<String>,
    prompt_tokens: u64,
    completion_tokens: u64,
    cost: f64,
    status: Option<String>,
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
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("test-conn-123"));
        assert!(json.contains("42"));
    }

    #[test]
    fn test_chart_data_point_serialization() {
        let point = ChartDataPoint {
            date: "2024-01-15".to_string(),
            requests: 100,
            prompt_tokens: 5000,
            completion_tokens: 2500,
            cost: 1.50,
        };
        let json = serde_json::to_string(&point).unwrap();
        assert!(json.contains("2024-01-15"));
        assert!(json.contains("100"));
    }

    #[test]
    fn test_provider_stats_serialization() {
        let stats = ProviderStats {
            provider: "openai".to_string(),
            total_requests: 50,
            total_prompt_tokens: 2000,
            total_completion_tokens: 1000,
            total_cost: 0.50,
        };
        let json = serde_json::to_string(&stats).unwrap();
        assert!(json.contains("openai"));
        assert!(json.contains("50"));
    }

    #[test]
    fn test_request_detail_serialization() {
        let detail = RequestDetail {
            timestamp: Some("2024-01-15T10:30:00Z".to_string()),
            provider: Some("openai".to_string()),
            model: "gpt-4".to_string(),
            connection_id: Some("conn-456".to_string()),
            endpoint: Some("/v1/chat/completions".to_string()),
            prompt_tokens: 100,
            completion_tokens: 50,
            cost: 0.02,
            status: Some("success".to_string()),
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
}
