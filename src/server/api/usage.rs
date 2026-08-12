use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{routing, Json, Router};
use bytes::Bytes;
use chrono::{DateTime, Datelike, Duration as ChronoDuration, Local, NaiveDate, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tokio::time::{self, Duration};

use crate::core::usage::quota_fetcher::{
    codex_account_id, consume_codex_rate_limit_reset_credit, fetch_antigravity_quota,
    fetch_claude_quota, fetch_codex_quota, fetch_deepseek_usage, fetch_gemini_cli_quota,
    fetch_kimi_oauth_usage,
    fetch_github_quota, fetch_glm_quota, fetch_grok_cli_quota, fetch_kimi_usage,
    fetch_kiro_quota, fetch_minimax_quota, fetch_qoder_quota, fetch_vercel_ai_gateway_quota,
    fetch_codebuddy_quota, get_codex_rate_limit_reset_credits,
};
use crate::core::usage::{DailyUsageSummary, Pricing, ProviderUsage, UsageTracker};
use crate::oauth::token_refresh::{dispatch_oauth_refresh, refresh_codex_token};
use crate::server::state::AppState;
use crate::server::usage_live::UsageEvent;
use crate::server::usage_stream::{build_usage_stats, UsagePeriod, UsageStatsPayload};
use crate::types::{ProviderConnection, TokenUsage, UsageDb, UsageEntry};

fn require_usage_access(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    super::require_dashboard_or_management_api_key(headers, state)
}

/// 9router `USAGE_APIKEY_PROVIDERS` parity (providers.js:163-165 — 12 registry
/// entries with `features.usageApikey`). Providers without a live-quota fetcher
/// fall back to a static message / per-request history (never 500).
fn is_usage_apikey_provider(provider: &str) -> bool {
    matches!(
        provider,
        "glm"
            | "glm-cn"
            | "minimax"
            | "minimax-cn"
            | "kimi"
            | "deepseek"
            | "kiro"
            | "ollama"
            | "qoder"
            | "vercel-ai-gateway"
            | "codebuddy-cn"
            | "codebuddy-intl"
    )
}

/// Dispatch to the correct OAuth quota fetcher for `connection`. Returns
/// `{}` for providers that don't expose a live quota endpoint.
pub async fn fetch_oauth_quota(connection: &ProviderConnection) -> Value {
    let token = match connection
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(t) => t,
        None => return serde_json::json!({}),
    };
    let provider = connection.provider.as_str();
    let psd = &connection.provider_specific_data;
    match provider {
        "github" | "github-copilot" => fetch_github_quota(token, provider).await,
        "claude" => fetch_claude_quota(token, provider).await,
        "codex" => fetch_codex_quota(token, provider).await,
        "kiro" => fetch_kiro_quota(token, provider, psd).await,
        "gemini-cli" => fetch_gemini_cli_quota(token, provider, psd).await,
        "antigravity" => fetch_antigravity_quota(token, provider).await,
        "qoder" => fetch_qoder_quota(token, provider).await,
        "grok-cli" => fetch_grok_cli_quota(token).await,
        // Kimi OAuth connections hit /v1/usages with Bearer + X-Msh-* headers.
        "kimi" | "kimi-coding" => fetch_kimi_oauth_usage(token, psd).await,
        _ => serde_json::json!({}),
    }
}

fn usage_message_for_provider(provider: &str) -> String {
    match provider {
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
        .route(
            "/api/usage/{connection_id}/codex-reset-credits",
            routing::get(get_connection_codex_reset_credits).post(reset_connection_credits),
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
    if let Err(response) = require_usage_access(&headers, &state) {
        return response;
    }

    let period = match query.period.as_deref().unwrap_or("today") {
        value @ ("today" | "24h" | "7d" | "30d" | "60d" | "all") => {
            UsagePeriod::parse(value).expect("validated usage period must parse")
        }
        _ => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Invalid period. Use one of: today, 24h, 7d, 30d, 60d, all"
                })),
            )
                .into_response()
        }
    };

    let payload = build_dashboard_usage_stats(&state, period).await;
    Json(payload).into_response()
}

async fn stream_usage_stats(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_usage_access(&headers, &state) {
        return response;
    }

    let mut receiver = state.usage_live.subscribe();
    let stream_state = state.clone();

    let body = Body::from_stream(async_stream::stream! {
        // No mutex lock — copy initial data, then stream without holding any lock.
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

    /// 9router usageRepo.js getUsageHistory parity — returns camelCase rows
    /// with connectionId, apiKeyMasked, endpoint, status, tokens in addition
    /// to the token/cost summary. The dashboard UsageHistory component reads
    /// these columns.
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct UsageEntryDto {
        timestamp: Option<String>,
        provider: Option<String>,
        model: String,
        connection_id: Option<String>,
        api_key_masked: Option<String>,
        endpoint: Option<String>,
        status: Option<String>,
        tokens: Option<Value>,
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
            connection_id: e.connection_id.clone(),
            api_key_masked: mask_api_key(e.api_key.as_deref()),
            endpoint: e.endpoint.clone(),
            status: e.status.clone(),
            tokens: e.tokens.as_ref().map(|t| {
                serde_json::to_value(t).unwrap_or(Value::Null)
            }),
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

/// 9router usageRepo.js `maskApiKey` parity:
/// - null/non-string → null
/// - length <= 8 → first char + "***"
/// - otherwise → first 8 chars + "***"
fn mask_api_key(api_key: Option<&str>) -> Option<String> {
    let key = api_key?;
    if key.is_empty() {
        return None;
    }
    if key.chars().count() <= 8 {
        let mut masked = String::new();
        masked.push(key.chars().next().unwrap_or(' '));
        masked.push_str("***");
        Some(masked)
    } else {
        Some(format!("{}***", &key[..8]))
    }
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
            cache_read_input_tokens: summary.cache_read_input_tokens,
            cache_creation_input_tokens: summary.cache_creation_input_tokens,
            cached_tokens: summary.cached_tokens,
            reasoning_tokens: summary.reasoning_tokens,
            by_provider: summary
                .by_provider
                .iter()
                .map(|(provider, counter)| ProviderUsage {
                    provider: provider.clone(),
                    requests: counter.requests,
                    prompt_tokens: counter.prompt_tokens,
                    completion_tokens: counter.completion_tokens,
                    cost: counter.cost,
                    cache_read_input_tokens: counter.cache_read_input_tokens,
                    cache_creation_input_tokens: counter.cache_creation_input_tokens,
                    cached_tokens: counter.cached_tokens,
                    reasoning_tokens: counter.reasoning_tokens,
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
    // 9router route.js:135-136: Kiro's headless api-key flow persists
    // authType "api_key" (underscore) while generic apikey providers persist
    // "apikey" — accept both spellings.
    let is_apikey_eligible = (connection.auth_type == "apikey"
        || connection.auth_type == "api_key")
        && is_usage_apikey_provider(&connection.provider);
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

    // Live quota fetch for whitelisted apikey providers (GLM, MiniMax). Falls
    // back to a static info message when the fetcher returns one. We never
    // surface upstream errors as HTTP failures — the dashboard treats
    // `quotas: {}` + `message` as "connected, but quota unavailable".
    let mut live_quotas = serde_json::json!({});
    let mut live_message: Option<String> = None;
    if is_apikey_eligible {
        if let Some(api_key) = connection
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let provider = connection.provider.clone();
            let psd = connection.provider_specific_data.clone();
            let result = match provider.as_str() {
                "glm" | "glm-cn" => fetch_glm_quota(api_key, &provider).await,
                "minimax" | "minimax-cn" => fetch_minimax_quota(api_key, &provider).await,
                "kimi" => fetch_kimi_usage(api_key).await,
                "deepseek" => fetch_deepseek_usage(api_key).await,
                "qoder" => fetch_qoder_quota(api_key, &provider).await,
                "kiro" => fetch_kiro_quota(api_key, &provider, &psd).await,
                "vercel-ai-gateway" => fetch_vercel_ai_gateway_quota(api_key).await,
                "codebuddy-cn" | "codebuddy-intl" => {
                    fetch_codebuddy_quota(api_key, &provider).await
                }
                // ollama has no live apikey quota fetcher yet — fall back to
                // `{}` + per-request history (never 500).
                _ => serde_json::json!({}),
            };
            if let Some(quotas) = result.get("quotas") {
                live_quotas = quotas.clone();
            }
            if let Some(msg) = result.get("message").and_then(|v| v.as_str()) {
                live_message = Some(msg.to_string());
            }
        }
    }

    let mut live_plan: Option<Value> = None;
    let mut live_reset_credits: Option<Value> = None;
    if is_oauth {
        // 9router route.js:158-183 — refresh credentials before the quota
        // call and force-retry once on an auth-expired message.
        let result = fetch_oauth_quota_with_refresh(connection).await;
        if let Some(quotas) = result.get("quotas") {
            live_quotas = quotas.clone();
        }
        if let Some(msg) = result.get("message").and_then(|v| v.as_str()) {
            live_message = Some(msg.to_string());
        }
        if let Some(plan) = result.get("plan") {
            live_plan = Some(plan.clone());
        }
        if let Some(reset_credits) = result.get("resetCredits") {
            live_reset_credits = Some(reset_credits.clone());
        }
    }

    // When quotas are populated, skip the generic fallback message so the
    // frontend renders the QuotaTable instead of a text-only message.
    let message = if live_quotas.as_object().is_some_and(|o| !o.is_empty()) {
        live_message.unwrap_or_default()
    } else {
        live_message.unwrap_or_else(|| usage_message_for_provider(&connection.provider))
    };

    let mut body = serde_json::to_value(ConnectionUsageResponse {
        connection_id,
        total_requests: request_count,
        total_prompt_tokens: prompt,
        total_completion_tokens: completion,
        total_cost: cost,
        message,
        quotas: live_quotas,
    })
    .unwrap_or_else(|_| json!({}));
    if let Some(obj) = body.as_object_mut() {
        if let Some(plan) = live_plan {
            obj.insert("plan".to_string(), plan);
        }
        if let Some(reset_credits) = live_reset_credits {
            obj.insert("resetCredits".to_string(), reset_credits);
        }
    }
    Json(body).into_response()
}

fn is_codex_reset_auth_type(auth_type: &str) -> bool {
    matches!(
        auth_type.trim().to_ascii_lowercase().as_str(),
        "oauth" | "access_token" | "accesstoken"
    )
}

fn is_auth_expired_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "expired",
        "authentication",
        "unauthorized",
        "401",
        "re-authorize",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

/// Refresh an OAuth connection's tokens via the provider's refresh flow and
/// return a cloned connection with the refreshed credentials. 9router
/// `refreshAndUpdateCredentials` parity (route.js:23-117). Returns the
/// original connection untouched on refresh failure (JS keeps the stale
/// accessToken when one exists).
async fn refresh_oauth_connection(
    connection: &ProviderConnection,
    force: bool,
) -> Result<ProviderConnection, String> {
    let Some(refresh_token) = connection
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(connection.clone());
    };

    // JS executor.needsRefresh(credentials): refresh when expired or missing.
    let needs_refresh = force
        || match connection.expires_at.as_deref() {
            Some(expires_at) => crate::oauth::token_refresh::needs_refresh_with_lead(
                &Some(expires_at.to_string()),
                // Refresh a bit early (2 min) to avoid a doomed fetch.
                120_000,
            ),
            None => connection.access_token.as_deref().is_none_or(|t| t.trim().is_empty()),
        };
    if !needs_refresh {
        return Ok(connection.clone());
    }

    let provider = connection.provider.clone();
    let psd = connection.provider_specific_data.clone();
    let result = dispatch_oauth_refresh(&provider, refresh_token, &psd).await?;

    let mut updated = connection.clone();
    updated.access_token = Some(result.access_token);
    if let Some(new_refresh) = result.refresh_token {
        updated.refresh_token = Some(new_refresh);
    }
    if let Some(expires_in) = result.expires_in {
        let expiry = Utc::now() + ChronoDuration::seconds(expires_in);
        updated.expires_at = Some(expiry.to_rfc3339());
    }
    Ok(updated)
}

/// Fetch the OAuth quota, refreshing credentials first if stale/expired and
/// force-retrying once when the quota response reports an auth-expired
/// message. 9router route.js:158-183 parity.
async fn fetch_oauth_quota_with_refresh(connection: &ProviderConnection) -> Value {
    // 1. Refresh before the fetch when needed.
    let connection = match refresh_oauth_connection(connection, false).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("usage oauth refresh failed for {}: {}", connection.provider, e);
            // Keep the stored token (JS returns stale accessToken on failure).
            connection.clone()
        }
    };

    // 2. First fetch.
    let result = fetch_oauth_quota(&connection).await;

    // 3. Force-retry once if the quota response signals auth-expired.
    let msg = result.get("message").and_then(|v| v.as_str()).unwrap_or("");
    if is_auth_expired_message(msg) && connection.refresh_token.is_some() {
        if let Ok(retried_conn) = refresh_oauth_connection(&connection, true).await {
            let retry = fetch_oauth_quota(&retried_conn).await;
            if retry.get("message").and_then(|v| v.as_str()).is_none_or(|m| !is_auth_expired_message(m)) {
                return retry;
            }
        }
    }

    result
}

fn is_auth_expired_consume_result(
    result: &crate::core::usage::quota_fetcher::CodexResetCreditConsumeResult,
) -> bool {
    let mut values = Vec::new();
    if let Some(m) = &result.message {
        values.push(m.clone());
    }
    if let Some(c) = &result.code {
        values.push(c.clone());
    }
    if let Some(d) = result.raw.get("detail").and_then(|v| v.as_str()) {
        values.push(d.to_string());
    }
    if let Some(e) = result.raw.get("error").and_then(|v| v.as_str()) {
        values.push(e.to_string());
    }
    if result.status == 401 {
        values.push("401".to_string());
    }
    values.iter().any(|v| is_auth_expired_message(v))
}

async fn persist_codex_tokens(
    state: &AppState,
    connection_id: &str,
    access_token: &str,
    refresh_token: Option<&str>,
    expires_in: Option<i64>,
) -> Result<(), String> {
    state
        .db
        .update(|db| {
            if let Some(conn) = db
                .provider_connections
                .iter_mut()
                .find(|entry| entry.id == connection_id)
            {
                conn.access_token = Some(access_token.to_string());
                if let Some(rt) = refresh_token.map(str::trim).filter(|s| !s.is_empty()) {
                    conn.refresh_token = Some(rt.to_string());
                }
                if let Some(secs) = expires_in {
                    conn.expires_in = Some(secs);
                    conn.expires_at = Some(
                        (chrono::Utc::now() + ChronoDuration::seconds(secs.max(0))).to_rfc3339(),
                    );
                }
                conn.updated_at = Some(chrono::Utc::now().to_rfc3339());
            }
        })
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

async fn clear_local_codex_rate_limit(state: &AppState, connection_id: &str) -> Result<(), String> {
    state
        .db
        .update(|db| {
            if let Some(conn) = db
                .provider_connections
                .iter_mut()
                .find(|entry| entry.id == connection_id)
            {
                conn.rate_limited_until = None;
                conn.consecutive_errors = Some(0);
                conn.backoff_level = Some(0);
                conn.last_error = None;
                conn.last_error_at = None;
                conn.error_code = None;
                conn.extra.insert(
                    "credits_reset_at".to_string(),
                    json!(chrono::Utc::now().to_rfc3339()),
                );
            }
        })
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn consume_result_response(
    result: &crate::core::usage::quota_fetcher::CodexResetCreditConsumeResult,
    redeem_request_id: &str,
) -> Response {
    if result.ok {
        return Json(json!({
            "code": result.code,
            "reset": true,
            "windows_reset": result.windows_reset,
            "redeemRequestId": redeem_request_id,
            "credit": result.raw.get("credit").cloned().unwrap_or(Value::Null),
        }))
        .into_response();
    }

    if result.no_credit {
        return (
            axum::http::StatusCode::CONFLICT,
            Json(json!({
                "code": "no_credit",
                "reset": false,
                "windows_reset": result.windows_reset,
                "message": "No Codex reset credits available.",
            })),
        )
            .into_response();
    }

    let status = if (400..500).contains(&result.status) {
        axum::http::StatusCode::from_u16(result.status)
            .unwrap_or(axum::http::StatusCode::BAD_GATEWAY)
    } else {
        axum::http::StatusCode::BAD_GATEWAY
    };
    (
        status,
        Json(json!({
            "code": result.code.clone().unwrap_or_else(|| "unknown_response".to_string()),
            "reset": false,
            "windows_reset": result.windows_reset,
            "message": result
                .message
                .clone()
                .unwrap_or_else(|| "Codex reset credit consume returned an unexpected response.".to_string()),
        })),
    )
        .into_response()
}

// Handler for GET /api/usage/:connection_id/codex-reset-credits
async fn get_connection_codex_reset_credits(
    State(state): State<AppState>,
    axum::extract::Path(connection_id): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_usage_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let Some(mut connection) = snapshot
        .provider_connections
        .iter()
        .find(|entry| entry.id == connection_id)
        .cloned()
    else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({ "error": "Connection not found" })),
        )
            .into_response();
    };

    if connection.provider != "codex" {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Codex reset credits are only available for Codex connections."
            })),
        )
            .into_response();
    }

    if !is_codex_reset_auth_type(&connection.auth_type) {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Codex reset credits require an OAuth or access-token connection."
            })),
        )
            .into_response();
    }

    let is_oauth = connection.auth_type.eq_ignore_ascii_case("oauth");
    if is_oauth {
        if let Some(refresh_token) = connection
            .refresh_token
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            match refresh_codex_token(refresh_token).await {
                Ok(refreshed) => {
                    if let Err(e) = persist_codex_tokens(
                        &state,
                        &connection_id,
                        &refreshed.access_token,
                        refreshed.refresh_token.as_deref(),
                        refreshed.expires_in,
                    )
                    .await
                    {
                        return (
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({ "error": e })),
                        )
                            .into_response();
                    }
                    connection.access_token = Some(refreshed.access_token);
                    if let Some(rt) = refreshed.refresh_token {
                        connection.refresh_token = Some(rt);
                    }
                }
                Err(e) => {
                    return (
                        axum::http::StatusCode::UNAUTHORIZED,
                        Json(json!({ "error": format!("Credential refresh failed: {e}") })),
                    )
                        .into_response();
                }
            }
        }
    }

    let account_id = codex_account_id(&connection.provider_specific_data);
    let access_token = connection
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(token) = access_token else {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "No Codex access token available. Please re-authorize the connection."
            })),
        )
            .into_response();
    };

    let mut result = get_codex_rate_limit_reset_credits(token, account_id.as_deref()).await;
    if let Err(err) = &result {
        if is_oauth
            && is_auth_expired_message(err)
            && connection
                .refresh_token
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| !s.is_empty())
        {
            if let Some(refresh_token) = connection.refresh_token.clone() {
                match refresh_codex_token(&refresh_token).await {
                    Ok(refreshed) => {
                        let _ = persist_codex_tokens(
                            &state,
                            &connection_id,
                            &refreshed.access_token,
                            refreshed.refresh_token.as_deref(),
                            refreshed.expires_in,
                        )
                        .await;
                        result = get_codex_rate_limit_reset_credits(
                            &refreshed.access_token,
                            account_id.as_deref(),
                        )
                        .await;
                    }
                    Err(_) => {}
                }
            }
        }
    }

    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => {
            tracing::warn!(provider = "codex", error = %e, "Codex reset credits GET failed");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e })),
            )
                .into_response()
        }
    }
}

// Handler for POST /api/usage/:connection_id/codex-reset-credits
async fn reset_connection_credits(
    State(state): State<AppState>,
    axum::extract::Path(connection_id): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_usage_access(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let Some(mut connection) = snapshot
        .provider_connections
        .iter()
        .find(|entry| entry.id == connection_id)
        .cloned()
    else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({ "error": "Connection not found" })),
        )
            .into_response();
    };

    if connection.provider != "codex" {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Codex reset credits are only available for Codex connections."
            })),
        )
            .into_response();
    }

    if !is_codex_reset_auth_type(&connection.auth_type) {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Codex reset credits require an OAuth or access-token connection."
            })),
        )
            .into_response();
    }

    let is_oauth = connection.auth_type.eq_ignore_ascii_case("oauth");
    if is_oauth {
        if let Some(refresh_token) = connection
            .refresh_token
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            match refresh_codex_token(refresh_token).await {
                Ok(refreshed) => {
                    if let Err(e) = persist_codex_tokens(
                        &state,
                        &connection_id,
                        &refreshed.access_token,
                        refreshed.refresh_token.as_deref(),
                        refreshed.expires_in,
                    )
                    .await
                    {
                        return (
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({ "error": e })),
                        )
                            .into_response();
                    }
                    connection.access_token = Some(refreshed.access_token);
                    if let Some(rt) = refreshed.refresh_token {
                        connection.refresh_token = Some(rt);
                    }
                }
                Err(e) => {
                    return (
                        axum::http::StatusCode::UNAUTHORIZED,
                        Json(json!({ "error": format!("Credential refresh failed: {e}") })),
                    )
                        .into_response();
                }
            }
        }
    }

    let access_token = connection
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // Prefer OpenAI consume when we have a token; fall back to local clear only
    // when no token is present (legacy local-only semantics).
    let Some(token) = access_token else {
        if let Err(e) = clear_local_codex_rate_limit(&state, &connection_id).await {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e })),
            )
                .into_response();
        }
        return Json(json!({
            "code": "local_clear",
            "reset": true,
            "windows_reset": 0,
            "message": "No Codex access token available; cleared local rate-limit/backoff state only.",
            "localOnly": true,
        }))
        .into_response();
    };

    let redeem_request_id = uuid::Uuid::new_v4().to_string();
    let mut consume_result =
        match consume_codex_rate_limit_reset_credit(&token, &redeem_request_id).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(provider = "codex", error = %e, "Codex reset credits POST failed");
                return (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": e })),
                )
                    .into_response();
            }
        };

    if is_oauth
        && is_auth_expired_consume_result(&consume_result)
        && connection
            .refresh_token
            .as_deref()
            .map(str::trim)
            .is_some_and(|s| !s.is_empty())
    {
        if let Some(refresh_token) = connection.refresh_token.clone() {
            match refresh_codex_token(&refresh_token).await {
                Ok(refreshed) => {
                    let _ = persist_codex_tokens(
                        &state,
                        &connection_id,
                        &refreshed.access_token,
                        refreshed.refresh_token.as_deref(),
                        refreshed.expires_in,
                    )
                    .await;
                    if let Ok(retry) = consume_codex_rate_limit_reset_credit(
                        &refreshed.access_token,
                        &redeem_request_id,
                    )
                    .await
                    {
                        consume_result = retry;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        provider = "codex",
                        error = %e,
                        "Codex reset credits force refresh failed"
                    );
                }
            }
        }
    }

    if consume_result.ok {
        // Secondary: clear local rate-limit / backoff so routing can reuse the account.
        let _ = clear_local_codex_rate_limit(&state, &connection_id).await;
    }

    consume_result_response(&consume_result, &redeem_request_id)
}

// Handler for GET /api/usage/chart?period=X
#[derive(Debug, Deserialize)]
struct ChartQuery {
    period: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageChartBucket {
    date: String,
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

    let period = params.period.as_deref().unwrap_or("today");
    if !matches!(period, "today" | "24h" | "7d" | "30d" | "60d") {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Invalid period" })),
        )
            .into_response();
    }

    let tracker = UsageTracker::new(state.db.clone());
    let usage_db = tracker.get_usage_db();
    Json(json!({ "data": build_usage_chart(&usage_db, period) })).into_response()
}

fn build_usage_chart(usage_db: &UsageDb, period: &str) -> Vec<UsageChartBucket> {
    if period == "today" {
        let now = Utc::now();
        let start = now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .map(|naive| naive.and_utc())
            .unwrap_or(now);
        let bucket_count = 24usize;
        let bucket_ms = 60 * 60 * 1000_i64;

        let mut buckets = (0..bucket_count)
            .map(|index| {
                let ts = start + ChronoDuration::hours(index as i64);
                UsageChartBucket {
                    date: ts.format("%H:%M").to_string(),
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

    if period == "24h" {
        let now = Utc::now();
        let bucket_count = 24usize;
        let bucket_ms = 60 * 60 * 1000_i64;
        let start = now - ChronoDuration::hours(bucket_count as i64);

        let mut buckets = (0..bucket_count)
            .map(|index| {
                let ts = start + ChronoDuration::hours(index as i64);
                UsageChartBucket {
                    date: ts.format("%H:%M").to_string(),
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
                date: format_daily_chart_label(date),
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
    // 9router usageRepo.js formatLogDate parity: local-time
    // DD-MM-YYYY HH:MM:SS (day-first, zero-padded), falling back to the raw
    // string when the timestamp doesn't parse as RFC3339.
    let timestamp = entry
        .timestamp
        .as_deref()
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|dt| {
            let local = dt.with_timezone(&Local);
            format!(
                "{:02}-{:02}-{} {:02}:{:02}:{:02}",
                local.day(),
                local.month(),
                local.year(),
                local.hour(),
                local.minute(),
                local.second()
            )
        })
        .unwrap_or_else(|| entry.timestamp.clone().unwrap_or_else(|| "-".to_string()));
    let model = if entry.model.is_empty() {
        "-".to_string()
    } else {
        entry.model.clone()
    };
    // JS r.provider?.toUpperCase() — "-" when absent.
    let provider = entry
        .provider
        .as_deref()
        .map(|p| p.to_uppercase())
        .unwrap_or_else(|| "-".to_string());
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
    // JS r.status || "-" verbatim — do NOT map success/None to "OK".
    let status = entry
        .status
        .clone()
        .unwrap_or_else(|| "-".to_string());

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
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
        cached_tokens: None,
        reasoning_tokens: None,
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
            date: "Jan 15".to_string(),
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
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                cached_tokens: None,
                reasoning_tokens: None,
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

    #[test]
    fn test_mask_api_key_short_and_long() {
        // <= 8 chars → first char + "***"
        assert_eq!(mask_api_key(Some("sk-test")), Some("s***".to_string()));
        // > 8 chars → first 8 + "***"
        assert_eq!(
            mask_api_key(Some("0123456789abcdef")),
            Some("01234567***".to_string())
        );
        // None / empty → None
        assert_eq!(mask_api_key(None), None);
        assert_eq!(mask_api_key(Some("")), None);
    }

    #[test]
    fn test_usage_history_dto_serializes_camelcase_fields() {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Dto {
            timestamp: Option<String>,
            provider: Option<String>,
            model: String,
            connection_id: Option<String>,
            api_key_masked: Option<String>,
            endpoint: Option<String>,
            status: Option<String>,
            tokens: Option<Value>,
            prompt_tokens: u64,
            completion_tokens: u64,
            cost: f64,
        }
        let dto = Dto {
            timestamp: Some("2026-01-01T00:00:00Z".into()),
            provider: Some("openai".into()),
            model: "gpt-4".into(),
            connection_id: Some("c1".into()),
            api_key_masked: Some("01234567***".into()),
            endpoint: Some("/v1/chat/completions".into()),
            status: Some("ok".into()),
            tokens: Some(serde_json::json!({"prompt_tokens": 10, "completion_tokens": 20})),
            prompt_tokens: 10,
            completion_tokens: 20,
            cost: 0.5,
        };
        let json = serde_json::to_value(&dto).unwrap();
        let obj = json.as_object().unwrap();
        // camelCase keys (9router usageRepo.js getUsageHistory parity).
        assert_eq!(obj.get("connectionId").and_then(|v| v.as_str()), Some("c1"));
        assert_eq!(
            obj.get("apiKeyMasked").and_then(|v| v.as_str()),
            Some("01234567***")
        );
        assert_eq!(
            obj.get("endpoint").and_then(|v| v.as_str()),
            Some("/v1/chat/completions")
        );
        assert_eq!(obj.get("status").and_then(|v| v.as_str()), Some("ok"));
        assert!(obj.contains_key("tokens"));
        // No snake_case leakage.
        assert!(!obj.contains_key("connection_id"));
        assert!(!obj.contains_key("api_key_masked"));
        assert!(!obj.contains_key("prompt_tokens"), "must be promptTokens");
    }

    #[test]
    fn test_is_usage_apikey_provider_includes_all_12() {
        for p in [
            "glm",
            "glm-cn",
            "minimax",
            "minimax-cn",
            "kimi",
            "deepseek",
            "kiro",
            "ollama",
            "qoder",
            "vercel-ai-gateway",
            "codebuddy-cn",
            "codebuddy-intl",
        ] {
            assert!(
                is_usage_apikey_provider(p),
                "expected {p} to be a usage apikey provider"
            );
        }
        assert!(!is_usage_apikey_provider("openai"));
        assert!(!is_usage_apikey_provider("claude"));
    }

    #[test]
    fn test_apikey_eligible_accepts_api_key_underscore() {
        use crate::types::ProviderConnection;
        // Kiro headless flow persists auth_type "api_key" (underscore).
        let conn = ProviderConnection {
            auth_type: "api_key".into(),
            provider: "kiro".into(),
            ..Default::default()
        };
        let is_apikey_eligible = (conn.auth_type == "apikey" || conn.auth_type == "api_key")
            && is_usage_apikey_provider(&conn.provider);
        assert!(is_apikey_eligible, "api_key auth must be eligible for kiro");

        // Non-whitelisted provider stays ineligible even with api_key auth.
        let conn2 = ProviderConnection {
            auth_type: "api_key".into(),
            provider: "openai".into(),
            ..Default::default()
        };
        let eligible2 = (conn2.auth_type == "apikey" || conn2.auth_type == "api_key")
            && is_usage_apikey_provider(&conn2.provider);
        assert!(!eligible2);
    }

    #[test]
    fn test_format_usage_log_local_timestamp() {
        use crate::types::UsageEntry;
        let entry = UsageEntry {
            timestamp: Some("2026-08-12T03:04:05Z".into()),
            provider: Some("glm".into()),
            model: "glm-4.7".into(),
            connection_id: Some("abc123".into()),
            status: Some("success".into()),
            ..Default::default()
        };
        let line = format_usage_log(&entry, &[]);
        // JS formatLogDate parity: local DD-MM-YYYY HH:MM:SS (day-first).
        let re = regex::Regex::new(r"^\d{2}-\d{2}-\d{4} \d{2}:\d{2}:\d{2} \| ").unwrap();
        assert!(re.is_match(&line), "timestamp must be local DD-MM-YYYY HH:MM:SS, got: {line}");
        // Provider uppercased (JS r.provider?.toUpperCase()).
        assert!(line.contains("| GLM |"), "provider must be uppercased: {line}");
        // Status verbatim (not mapped to OK).
        assert!(line.contains("| success"), "status must be raw: {line}");
    }

    #[test]
    fn test_format_usage_log_unparseable_timestamp_falls_back_raw() {
        use crate::types::UsageEntry;
        let entry = UsageEntry {
            timestamp: Some("not-a-timestamp".into()),
            provider: Some("openai".into()),
            model: "gpt-4".into(),
            ..Default::default()
        };
        let line = format_usage_log(&entry, &[]);
        assert!(line.starts_with("not-a-timestamp | "), "raw fallback expected: {line}");
        assert!(line.contains("| OPENAI |"), "provider uppercased: {line}");
        // Missing status → "-".
        assert!(line.ends_with("| -"));
    }

    #[test]
    fn test_is_auth_expired_message_matches_js_patterns() {
        // 9router AUTH_EXPIRED_PATTERNS = [expired, authentication,
        // unauthorized, 401, re-authorize].
        assert!(is_auth_expired_message("Grok CLI authentication expired. Please re-authorize."));
        assert!(is_auth_expired_message("401 Unauthorized"));
        assert!(is_auth_expired_message("Token expired"));
        assert!(is_auth_expired_message("authentication failed"));
        assert!(!is_auth_expired_message("Kimi Coding connected. Usage tracked per request."));
        assert!(!is_auth_expired_message("ok"));
    }

    #[test]
    fn test_refresh_oauth_connection_no_refresh_token_is_noop() {
        use crate::types::ProviderConnection;
        // No refresh_token → returns the connection unchanged (JS keeps stale
        // accessToken; never 401s when no refresh is possible).
        let conn = ProviderConnection {
            auth_type: "oauth".into(),
            provider: "claude".into(),
            access_token: Some("stale-token".into()),
            refresh_token: None,
            ..Default::default()
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let out = rt.block_on(refresh_oauth_connection(&conn, false)).unwrap();
        assert_eq!(out.access_token.as_deref(), Some("stale-token"));
    }
}
