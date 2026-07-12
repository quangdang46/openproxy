//! Quota auto-ping (9router `quotaAutoPing` parity).
//!
//! Keeps the settings contract (`claudeAutoPing` / `codexAutoPing` in settings
//! extra) and runs a 60s tick (dashboard POST + background spawn from `main`).
//!
//! On each tick, for enabled OAuth Claude/Codex connections:
//! 1. Optionally refresh credentials when a refresh token is present
//! 2. Fetch live quota and decide whether a warm ping is due
//! 3. Send a minimal synthetic request (Claude messages / Codex responses)
//! 4. Persist `lastPingedResetAt` / `lastPingedResetKey` / `lastPingAt`
//!
//! # Cooldowns (from 9router `QUOTA_AUTOPING_CONFIG`)
//! - `pingLeadMs` 5s — Claude fires once reset is within lead
//! - `refreshAheadMs` 5min — skip usage refetch far from reset (Claude only)
//! - `failureCooldownMs` 15min — avoid spam after refresh/ping failure
//! - Codex uses `pingWhenResetAtSlides` + `resetAtDriftMs` + `minPingIntervalMs`
//!
//! # Remaining limits
//! - Proxy is resolved via `resolve_proxy_target` (connection / pool / outbound).
//!   Per-connection vercel relay is not modeled (same as most OP executors).
//! - Claude spoof headers are a static subset of 9r `CLAUDE_CLI_SPOOF_HEADERS`
//!   (version/beta/UA/x-app); stainless arch/os are fixed, not host-mapped.
//! - No per-connection concurrent ping mutex beyond the global tick lock.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{routing::post, Json, Router};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::core::executor::{CodexExecutionRequest, CodexExecutor, UpstreamResponse};
use crate::core::proxy::resolve_proxy_target;
use crate::oauth::token_refresh::dispatch_oauth_refresh;
use crate::server::api::usage::fetch_oauth_quota;
use crate::server::state::AppState;
use crate::types::{ProviderConnection, Settings};

const TICK_INTERVAL: Duration = Duration::from_secs(60);
const PING_LEAD_MS: i64 = 5_000;
const REFRESH_AHEAD_MS: i64 = 300_000;
const FAILURE_COOLDOWN_MS: u64 = 900_000;
const CODEX_RESET_DRIFT_MS: i64 = 30_000;
const CODEX_MIN_PING_INTERVAL_MS: i64 = 600_000;

const CLAUDE_PING_URL: &str = "https://api.anthropic.com/v1/messages?beta=true";
const CLAUDE_PING_MODEL: &str = "claude-haiku-4-5-20251001";
const CLAUDE_PING_TEXT: &str = "hi";
const CLAUDE_PING_MAX_TOKENS: u32 = 1;
const CLAUDE_ANTHROPIC_VERSION: &str = "2023-06-01";
const CLAUDE_ANTHROPIC_BETA: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05,advanced-tool-use-2025-11-20,effort-2025-11-24,structured-outputs-2025-12-15,fast-mode-2026-02-01,redact-thinking-2026-02-12,token-efficient-tools-2026-03-28";

const CODEX_PING_MODEL: &str = "gpt-5.5";
const CODEX_PING_TEXT: &str = "hi";
const CODEX_PING_INSTRUCTIONS: &str = "Reply with OK.";
const CODEX_PING_REASONING_EFFORT: &str = "none";

static TICK_RUNNING: AtomicBool = AtomicBool::new(false);

/// Process-local caches matching 9r `global.__quotaAutoPing`.
struct AutoPingState {
    /// Last observed session resetAt per `provider:connectionId`.
    reset_cache: BTreeMap<String, String>,
    /// Failure timestamps for cooldown.
    failure_cache: BTreeMap<String, Instant>,
}

static AUTO_PING_STATE: Lazy<Mutex<AutoPingState>> = Lazy::new(|| {
    Mutex::new(AutoPingState {
        reset_cache: BTreeMap::new(),
        failure_cache: BTreeMap::new(),
    })
});

#[derive(Clone, Copy)]
struct ProviderPingConfig {
    settings_key: &'static str,
    quota_key: &'static str,
    /// Claude: fire when `now >= resetAt - pingLeadMs`.
    /// Codex: fire when resetAt slides forward by >= drift.
    ping_when_reset_at_slides: bool,
    reset_at_drift_ms: i64,
    min_ping_interval_ms: Option<i64>,
    skip_when_blocking_quota_exhausted: bool,
}

const CLAUDE_CFG: ProviderPingConfig = ProviderPingConfig {
    settings_key: "claudeAutoPing",
    quota_key: "session (5h)",
    ping_when_reset_at_slides: false,
    reset_at_drift_ms: 0,
    min_ping_interval_ms: None,
    skip_when_blocking_quota_exhausted: false,
};

const CODEX_CFG: ProviderPingConfig = ProviderPingConfig {
    settings_key: "codexAutoPing",
    quota_key: "session",
    ping_when_reset_at_slides: true,
    reset_at_drift_ms: CODEX_RESET_DRIFT_MS,
    min_ping_interval_ms: Some(CODEX_MIN_PING_INTERVAL_MS),
    skip_when_blocking_quota_exhausted: true,
};

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/quota/auto-ping/tick", post(tick_handler))
}

async fn tick_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let result = run_quota_auto_ping_tick(&state).await;
    Json(result).into_response()
}

/// Spawn a background interval that ticks auto-ping every 60s.
/// Best-effort — safe to call once at process boot.
pub fn spawn_quota_auto_ping(state: AppState) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            let _ = run_quota_auto_ping_tick(&state).await;
            tokio::time::sleep(TICK_INTERVAL).await;
        }
    });
}

pub async fn run_quota_auto_ping_tick(state: &AppState) -> Value {
    if TICK_RUNNING.swap(true, Ordering::SeqCst) {
        return json!({
            "ok": true,
            "skipped": true,
            "reason": "tick already running",
        });
    }

    let result = run_tick_inner(state).await;
    TICK_RUNNING.store(false, Ordering::SeqCst);
    result
}

async fn run_tick_inner(state: &AppState) -> Value {
    let snapshot = state.db.snapshot();
    let settings = &snapshot.settings;

    let mut results: Vec<Value> = Vec::new();
    let mut target_count = 0u32;
    let mut ping_attempts = 0u32;
    let mut ping_successes = 0u32;

    for (provider, cfg) in [("claude", CLAUDE_CFG), ("codex", CODEX_CFG)] {
        let enabled_map = auto_ping_connections(settings, cfg.settings_key);
        if enabled_map.is_empty() {
            continue;
        }

        for conn in snapshot
            .provider_connections
            .iter()
            .filter(|c| c.provider == provider && c.is_active() && c.auth_type == "oauth")
        {
            if enabled_map.get(&conn.id) != Some(&true) {
                continue;
            }
            target_count += 1;

            match process_connection(state, conn, provider, cfg).await {
                TickOutcome::Observe {
                    reset_at,
                    near_reset,
                } => {
                    results.push(json!({
                        "provider": provider,
                        "connectionId": conn.id,
                        "resetAt": reset_at,
                        "nearReset": near_reset,
                        "quotaKey": cfg.quota_key,
                        "action": "observe",
                    }));
                }
                TickOutcome::Skip { reason, reset_at } => {
                    results.push(json!({
                        "provider": provider,
                        "connectionId": conn.id,
                        "resetAt": reset_at,
                        "nearReset": false,
                        "quotaKey": cfg.quota_key,
                        "action": "skip",
                        "reason": reason,
                    }));
                }
                TickOutcome::Ping {
                    ok,
                    reset_at,
                    error,
                } => {
                    ping_attempts += 1;
                    if ok {
                        ping_successes += 1;
                    }
                    let mut entry = json!({
                        "provider": provider,
                        "connectionId": conn.id,
                        "resetAt": reset_at,
                        "nearReset": true,
                        "quotaKey": cfg.quota_key,
                        "action": if ok { "ping_success" } else { "ping_failed" },
                    });
                    if let Some(err) = error {
                        entry["error"] = json!(err);
                    }
                    results.push(entry);
                }
            }
        }
    }

    if target_count == 0 {
        return json!({
            "ok": true,
            "targets": 0,
            "results": [],
            "note": "No claudeAutoPing/codexAutoPing connections enabled",
        });
    }

    json!({
        "ok": true,
        "targets": target_count,
        "pingAttempts": ping_attempts,
        "pingSuccesses": ping_successes,
        "results": results,
        "warmPing": "active",
    })
}

enum TickOutcome {
    Observe {
        reset_at: Option<String>,
        near_reset: bool,
    },
    Skip {
        reason: String,
        reset_at: Option<String>,
    },
    Ping {
        ok: bool,
        reset_at: Option<String>,
        error: Option<String>,
    },
}

async fn process_connection(
    state: &AppState,
    conn: &ProviderConnection,
    provider: &str,
    cfg: ProviderPingConfig,
) -> TickOutcome {
    let key = cache_key(provider, &conn.id);

    // Failure cooldown
    {
        let st = AUTO_PING_STATE.lock();
        if let Some(failed_at) = st.failure_cache.get(&key) {
            if failed_at.elapsed() < Duration::from_millis(FAILURE_COOLDOWN_MS) {
                return TickOutcome::Skip {
                    reason: "failure_cooldown".into(),
                    reset_at: st.reset_cache.get(&key).cloned(),
                };
            }
        }
    }

    // Claude: skip far from reset using cached resetAt (refreshAheadMs)
    if !cfg.ping_when_reset_at_slides {
        let st = AUTO_PING_STATE.lock();
        if let Some(cached) = st.reset_cache.get(&key) {
            if let Some(reset_ms) = parse_reset_ms(cached) {
                let now = chrono::Utc::now().timestamp_millis();
                if now < reset_ms - REFRESH_AHEAD_MS {
                    return TickOutcome::Observe {
                        reset_at: Some(cached.clone()),
                        near_reset: false,
                    };
                }
            }
        }
    }

    // Refresh credentials (best-effort; 9r always attempts)
    let mut connection = conn.clone();
    if let Some(rt) = connection
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        match dispatch_oauth_refresh(provider, rt, &connection.provider_specific_data).await {
            Ok(result) => {
                let conn_id = connection.id.clone();
                let new_access = result.access_token.clone();
                let new_refresh = result.refresh_token.clone();
                let expires_at = result.expires_in.map(|secs| {
                    (chrono::Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339()
                });
                let last_refresh_at = chrono::Utc::now().to_rfc3339();
                let _ = state
                    .db
                    .update({
                        let conn_id = conn_id.clone();
                        let new_access = new_access.clone();
                        let new_refresh = new_refresh.clone();
                        let expires_at = expires_at.clone();
                        let last_refresh_at = last_refresh_at.clone();
                        move |db| {
                            if let Some(c) =
                                db.provider_connections.iter_mut().find(|c| c.id == conn_id)
                            {
                                c.access_token = Some(new_access);
                                if let Some(rt) = new_refresh {
                                    c.refresh_token = Some(rt);
                                }
                                if let Some(exp) = expires_at {
                                    c.expires_at = Some(exp);
                                }
                                c.provider_specific_data
                                    .insert("lastRefreshAt".into(), Value::String(last_refresh_at));
                            }
                        }
                    })
                    .await;
                connection.access_token = Some(result.access_token);
                if let Some(rt) = result.refresh_token {
                    connection.refresh_token = Some(rt);
                }
                if let Some(exp) = expires_at {
                    connection.expires_at = Some(exp);
                }
            }
            Err(e) => {
                mark_failure(&key);
                warn!(
                    target: "openproxy::auto_ping",
                    provider = provider,
                    connection_id = %conn.id,
                    error = %e,
                    "quota auto-ping: credential refresh failed"
                );
                return TickOutcome::Skip {
                    reason: format!("refresh_failed: {e}"),
                    reset_at: None,
                };
            }
        }
    }

    let usage = fetch_oauth_quota(&connection).await;
    let quotas = usage.get("quotas").cloned().unwrap_or_else(|| json!({}));
    let quota = quotas.get(cfg.quota_key).cloned().unwrap_or(Value::Null);
    let reset_at = quota
        .get("resetAt")
        .or_else(|| quota.get("reset_at"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let Some(reset_at) = reset_at else {
        return TickOutcome::Skip {
            reason: "no_reset_at".into(),
            reset_at: None,
        };
    };

    let previous_cached = {
        let mut st = AUTO_PING_STATE.lock();
        let prev = st.reset_cache.get(&key).cloned();
        st.reset_cache.insert(key.clone(), reset_at.clone());
        prev
    };

    if cfg.skip_when_blocking_quota_exhausted
        && has_exhausted_blocking_quota(&quotas, cfg.quota_key)
    {
        return TickOutcome::Skip {
            reason: "blocking_quota_exhausted".into(),
            reset_at: Some(reset_at),
        };
    }

    if is_quota_exhausted(&quota) {
        return TickOutcome::Skip {
            reason: "session_quota_exhausted".into(),
            reset_at: Some(reset_at),
        };
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    let should_ping = should_ping_for_reset(cfg, previous_cached.as_deref(), &reset_at, now_ms);
    if !should_ping {
        let near = !cfg.ping_when_reset_at_slides
            && parse_reset_ms(&reset_at).is_some_and(|ms| now_ms >= ms - PING_LEAD_MS);
        return TickOutcome::Observe {
            reset_at: Some(reset_at),
            near_reset: near,
        };
    }

    if let Some(interval_ms) = cfg.min_ping_interval_ms {
        if was_pinged_recently(&connection, interval_ms, now_ms) {
            return TickOutcome::Skip {
                reason: "min_ping_interval".into(),
                reset_at: Some(reset_at),
            };
        }
    }

    let reset_key = normalize_reset_key(&reset_at);
    let last_pinged_key = connection
        .extra
        .get("lastPingedResetKey")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            connection
                .extra
                .get("lastPingedResetAt")
                .and_then(|v| v.as_str())
                .map(normalize_reset_key)
        });
    if last_pinged_key.as_deref() == Some(reset_key.as_str()) {
        return TickOutcome::Skip {
            reason: "already_pinged_this_reset".into(),
            reset_at: Some(reset_at),
        };
    }

    let ping_result = match provider {
        "claude" => send_claude_ping(state, &connection).await,
        "codex" => send_codex_ping(state, &connection).await,
        _ => Err("unsupported provider".into()),
    };

    match ping_result {
        Ok(()) => {
            clear_failure(&key);
            let pinged_at = chrono::Utc::now().to_rfc3339();
            let conn_id = connection.id.clone();
            let reset_at_store = reset_at.clone();
            let reset_key_store = reset_key.clone();
            let _ = state
                .db
                .update(move |db| {
                    if let Some(c) = db.provider_connections.iter_mut().find(|c| c.id == conn_id) {
                        c.extra
                            .insert("lastPingedResetAt".into(), json!(reset_at_store));
                        c.extra
                            .insert("lastPingedResetKey".into(), json!(reset_key_store));
                        c.extra.insert("lastPingAt".into(), json!(pinged_at));
                        c.updated_at = Some(chrono::Utc::now().to_rfc3339());
                    }
                })
                .await;
            info!(
                target: "openproxy::auto_ping",
                provider = provider,
                connection_id = %connection.id,
                reset_at = %reset_at,
                "quota auto-ping: warm ping sent"
            );
            TickOutcome::Ping {
                ok: true,
                reset_at: Some(reset_at),
                error: None,
            }
        }
        Err(e) => {
            mark_failure(&key);
            warn!(
                target: "openproxy::auto_ping",
                provider = provider,
                connection_id = %connection.id,
                reset_at = %reset_at,
                error = %e,
                "quota auto-ping: warm ping failed"
            );
            TickOutcome::Ping {
                ok: false,
                reset_at: Some(reset_at),
                error: Some(e),
            }
        }
    }
}

async fn send_claude_ping(state: &AppState, connection: &ProviderConnection) -> Result<(), String> {
    let token = connection
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "missing access token".to_string())?;

    let snapshot = state.db.snapshot();
    let proxy = resolve_proxy_target(&snapshot, connection, &snapshot.settings);
    let client = state
        .client_pool
        .get("claude-auto-ping", proxy.as_ref())
        .map_err(|e| format!("client pool: {e}"))?;

    let body = json!({
        "model": CLAUDE_PING_MODEL,
        "max_tokens": CLAUDE_PING_MAX_TOKENS,
        "messages": [{ "role": "user", "content": CLAUDE_PING_TEXT }],
    });

    let response = client
        .post(CLAUDE_PING_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .header("anthropic-version", CLAUDE_ANTHROPIC_VERSION)
        .header("anthropic-beta", CLAUDE_ANTHROPIC_BETA)
        .header("anthropic-dangerous-direct-browser-access", "true")
        .header("user-agent", "claude-cli/2.1.92 (external, sdk-cli)")
        .header("x-app", "cli")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("claude ping request failed: {e}"))?;

    let status = response.status();
    // Drain body so the connection is reusable.
    let _ = response.bytes().await;
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("claude ping HTTP {}", status.as_u16()))
    }
}

async fn send_codex_ping(state: &AppState, connection: &ProviderConnection) -> Result<(), String> {
    let snapshot = state.db.snapshot();
    let proxy = resolve_proxy_target(&snapshot, connection, &snapshot.settings);

    let executor = CodexExecutor::new(state.client_pool.clone(), None)
        .map_err(|e| format!("codex executor init: {e:?}"))?;

    let body = json!({
        "model": CODEX_PING_MODEL,
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": CODEX_PING_TEXT }],
        }],
        "instructions": CODEX_PING_INSTRUCTIONS,
        "reasoning": {
            "effort": CODEX_PING_REASONING_EFFORT,
            "summary": "auto",
        },
        "store": false,
        "stream": true,
    });

    let request = CodexExecutionRequest {
        model: CODEX_PING_MODEL.to_string(),
        body,
        stream: true,
        credentials: connection.clone(),
        proxy,
    };

    let result = executor
        .execute(request)
        .await
        .map_err(|e| format!("codex ping execute: {e:?}"))?;

    let status = result.response.status();
    // Codex 5h window starts only after the stream completes — drain fully.
    match result.response {
        UpstreamResponse::Reqwest(resp) => {
            let ok = status.is_success();
            let _ = resp.bytes().await;
            if ok {
                Ok(())
            } else {
                Err(format!("codex ping HTTP {}", status.as_u16()))
            }
        }
        other => {
            // Hyper path: best-effort status check; no body drain helper.
            if status.is_success() {
                drop(other);
                Ok(())
            } else {
                Err(format!("codex ping HTTP {}", status.as_u16()))
            }
        }
    }
}

fn should_ping_for_reset(
    cfg: ProviderPingConfig,
    cached_reset: Option<&str>,
    reset_at: &str,
    now_ms: i64,
) -> bool {
    if cfg.ping_when_reset_at_slides {
        let Some(prev) = cached_reset else {
            return false;
        };
        return get_reset_drift_ms(prev, reset_at) >= cfg.reset_at_drift_ms;
    }

    parse_reset_ms(reset_at).is_some_and(|ms| now_ms >= ms - PING_LEAD_MS)
}

fn get_reset_drift_ms(previous: &str, next: &str) -> i64 {
    match (parse_reset_ms(previous), parse_reset_ms(next)) {
        (Some(a), Some(b)) => b - a,
        _ => 0,
    }
}

fn parse_reset_ms(reset_at: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(reset_at)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn normalize_reset_key(reset_at: &str) -> String {
    match parse_reset_ms(reset_at) {
        Some(ms) => {
            let floored = (ms / 60_000) * 60_000;
            chrono::DateTime::from_timestamp_millis(floored)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| reset_at.to_string())
        }
        None => reset_at.to_string(),
    }
}

fn was_pinged_recently(connection: &ProviderConnection, interval_ms: i64, now_ms: i64) -> bool {
    let Some(last) = connection.extra.get("lastPingAt").and_then(|v| v.as_str()) else {
        return false;
    };
    match parse_reset_ms(last) {
        Some(last_ms) => now_ms - last_ms < interval_ms,
        None => false,
    }
}

fn to_finite_number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|i| i as f64))
        .or_else(|| value.as_u64().map(|u| u as f64))
        .or_else(|| {
            value
                .as_str()
                .and_then(|s| s.trim().parse::<f64>().ok())
                .filter(|n| n.is_finite())
        })
}

fn is_quota_exhausted(quota: &Value) -> bool {
    if quota.is_null() {
        return false;
    }
    if quota.get("unlimited").and_then(|v| v.as_bool()) == Some(true) {
        return false;
    }
    if let Some(remaining) = quota.get("remaining").and_then(to_finite_number) {
        return remaining <= 0.0;
    }
    let used = quota.get("used").and_then(to_finite_number);
    let total = quota.get("total").and_then(to_finite_number);
    match (used, total) {
        (Some(u), Some(t)) if t > 0.0 => u >= t,
        _ => false,
    }
}

fn is_blocking_quota_name(name: &str, session_key: &str) -> bool {
    if name == session_key {
        return false;
    }
    !name.to_ascii_lowercase().contains("session")
}

fn has_exhausted_blocking_quota(quotas: &Value, session_key: &str) -> bool {
    let Some(obj) = quotas.as_object() else {
        return false;
    };
    obj.iter()
        .any(|(name, quota)| is_blocking_quota_name(name, session_key) && is_quota_exhausted(quota))
}

fn cache_key(provider: &str, connection_id: &str) -> String {
    format!("{provider}:{connection_id}")
}

fn mark_failure(key: &str) {
    AUTO_PING_STATE
        .lock()
        .failure_cache
        .insert(key.to_string(), Instant::now());
}

fn clear_failure(key: &str) {
    AUTO_PING_STATE.lock().failure_cache.remove(key);
}

fn auto_ping_connections(settings: &Settings, key: &str) -> BTreeMap<String, bool> {
    let value = settings.extra.get(key);
    let Some(obj) = value.and_then(|v| v.as_object()) else {
        return BTreeMap::new();
    };
    let Some(connections) = obj.get("connections").and_then(|v| v.as_object()) else {
        return BTreeMap::new();
    };
    connections
        .iter()
        .filter_map(|(id, v)| v.as_bool().map(|b| (id.clone(), b)))
        .collect()
}

/// Resume tunnel / tailscale from persisted settings after process boot.
pub fn spawn_boot_resume(state: AppState, port: u16) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;

        let settings = state.db.snapshot().settings.clone();
        let tunnel_mgr = state.tunnel_manager.clone();

        if settings.tunnel_enabled {
            info!(
                target: "openproxy::boot",
                "tunnel was enabled — auto-resuming cloudflared"
            );
            if let Err(err) = tunnel_mgr
                .start(crate::core::tunnel::TunnelProvider::Cloudflare, port)
                .await
            {
                warn!(
                    target: "openproxy::boot",
                    error = %err,
                    "tunnel auto-resume failed"
                );
            }
        } else if settings.tailscale_enabled {
            info!(
                target: "openproxy::boot",
                "tailscale was enabled — auto-resuming funnel"
            );
            if let Err(err) = tunnel_mgr
                .start(crate::core::tunnel::TunnelProvider::Tailscale, port)
                .await
            {
                warn!(
                    target: "openproxy::boot",
                    error = %err,
                    "tailscale auto-resume failed"
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_reset_key_floors_to_minute() {
        let key = normalize_reset_key("2026-05-12T18:30:45.123Z");
        assert!(key.starts_with("2026-05-12T18:30:00"));
    }

    #[test]
    fn should_ping_claude_near_reset() {
        let now = chrono::Utc::now().timestamp_millis();
        let reset = chrono::DateTime::from_timestamp_millis(now + 1_000)
            .unwrap()
            .to_rfc3339();
        assert!(should_ping_for_reset(CLAUDE_CFG, None, &reset, now));
        let far = chrono::DateTime::from_timestamp_millis(now + 60_000)
            .unwrap()
            .to_rfc3339();
        assert!(!should_ping_for_reset(CLAUDE_CFG, None, &far, now));
    }

    #[test]
    fn should_ping_codex_on_slide() {
        let now = chrono::Utc::now().timestamp_millis();
        let prev = chrono::DateTime::from_timestamp_millis(now)
            .unwrap()
            .to_rfc3339();
        let next = chrono::DateTime::from_timestamp_millis(now + 60_000)
            .unwrap()
            .to_rfc3339();
        assert!(!should_ping_for_reset(CODEX_CFG, None, &next, now));
        assert!(should_ping_for_reset(CODEX_CFG, Some(&prev), &next, now));
        let small = chrono::DateTime::from_timestamp_millis(now + 10_000)
            .unwrap()
            .to_rfc3339();
        assert!(!should_ping_for_reset(CODEX_CFG, Some(&prev), &small, now));
    }

    #[test]
    fn quota_exhausted_from_remaining() {
        assert!(is_quota_exhausted(&json!({ "remaining": 0 })));
        assert!(!is_quota_exhausted(&json!({ "remaining": 1 })));
        assert!(!is_quota_exhausted(
            &json!({ "unlimited": true, "remaining": 0 })
        ));
        assert!(is_quota_exhausted(&json!({ "used": 10, "total": 10 })));
    }

    #[test]
    fn blocking_quota_skips_session_key() {
        let quotas = json!({
            "session": { "remaining": 0 },
            "weekly": { "remaining": 0 },
        });
        assert!(has_exhausted_blocking_quota(&quotas, "session"));
        let only_session = json!({
            "session": { "remaining": 0 },
        });
        assert!(!has_exhausted_blocking_quota(&only_session, "session"));
    }
}
