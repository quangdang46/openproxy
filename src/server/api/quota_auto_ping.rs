//! Quota auto-ping foundation (9router `quotaAutoPing` parity — partial).
//!
//! 9router runs a long-lived scheduler that OAuth-warms Claude/Codex session
//! windows before they reset. OpenProxy keeps the **settings contract**
//! (`claudeAutoPing` / `codexAutoPing` in settings extra) and exposes a
//! tick endpoint the dashboard can call while open. Full warm-ping (token
//! refresh + synthetic model request) is still residual — this module:
//!
//! 1. Lists enabled OAuth connections from settings
//! 2. Fetches live quota metadata for each target
//! 3. Returns structured tick results for UI / logs
//!
//! A server-side interval is started from `main` via [`spawn_quota_auto_ping`].

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{routing::post, Json, Router};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing::{info, warn};

use crate::server::api::usage::fetch_oauth_quota;
use crate::server::state::AppState;
use crate::types::Settings;

const TICK_INTERVAL: Duration = Duration::from_secs(60);
const PING_LEAD_MS: i64 = 5_000;

static TICK_RUNNING: AtomicBool = AtomicBool::new(false);

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
/// Best-effort foundation — safe to call once at process boot.
pub fn spawn_quota_auto_ping(state: AppState) {
    tokio::spawn(async move {
        // Defer first tick so OAuth / DB settle after boot.
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

    for (provider, settings_key, quota_key) in [
        ("claude", "claudeAutoPing", "session (5h)"),
        ("codex", "codexAutoPing", "session"),
    ] {
        let enabled_map = auto_ping_connections(settings, settings_key);
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

            let usage = fetch_oauth_quota(conn).await;
            let quotas = usage.get("quotas").cloned().unwrap_or_else(|| json!({}));
            let quota = quotas.get(quota_key).cloned().unwrap_or(Value::Null);
            let reset_at = quota
                .get("resetAt")
                .or_else(|| quota.get("reset_at"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let near_reset = reset_at
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| {
                    let ms = dt.timestamp_millis();
                    let now = chrono::Utc::now().timestamp_millis();
                    now >= ms - PING_LEAD_MS
                })
                .unwrap_or(false);

            // Foundation: record observation. Full synthetic warm-ping (POST
            // a 1-token completion with refreshed OAuth) is residual — needs
            // executor plumbing that 9router had in-process.
            let entry = json!({
                "provider": provider,
                "connectionId": conn.id,
                "resetAt": reset_at,
                "nearReset": near_reset,
                "quotaKey": quota_key,
                "action": if near_reset { "would_ping" } else { "observe" },
            });

            if near_reset {
                info!(
                    target: "openproxy::auto_ping",
                    provider = provider,
                    connection_id = %conn.id,
                    reset_at = ?reset_at,
                    "quota auto-ping: window near reset (warm-ping residual)"
                );
            }

            results.push(entry);
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
        "results": results,
        "warmPing": "residual",
        "note": "Settings + observation tick active; full OAuth warm-ping is residual",
    })
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
        // Let the HTTP listener bind before spawning external processes.
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
