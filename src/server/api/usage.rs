use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{routing, Json, Router};
use serde::Serialize;

use crate::core::usage::{DailyUsageSummary, Pricing, ProviderUsage, UsageTracker};
use crate::server::auth::require_api_key;
use crate::server::state::AppState;

use super::auth_error_response;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/usage", routing::get(get_usage))
        .route("/v1/usage/summary", routing::get(get_usage_summary))
        .route("/v1/usage/history", routing::get(get_usage_history))
        .route("/v1/usage/daily", routing::get(get_usage_daily))
        .route("/v1/usage/pricing", routing::get(get_pricing))
}

async fn get_usage(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(error) = require_api_key(&headers, &state.db) {
        return auth_error_response(error);
    }

    let tracker = UsageTracker::new(state.db.clone());
    let summary = tracker.summarize();
    Json(summary).into_response()
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
            total_completion += tokens.completion_tokens.or(tokens.output_tokens).unwrap_or(0);
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
            prompt_tokens: e.tokens.as_ref().and_then(|t| t.prompt_tokens.or(t.input_tokens)).unwrap_or(0),
            completion_tokens: e.tokens.as_ref().and_then(|t| t.completion_tokens.or(t.output_tokens)).unwrap_or(0),
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