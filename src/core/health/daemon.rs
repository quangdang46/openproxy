//! Background health-check daemon.
//!
//! Ticks every [`HEALTH_TICK_INTERVAL`] (3 min — free-tier providers flap far
//! more often than paid ones), probes every active API-key connection, and
//! records the observed status in [`crate::core::health::HealthRegistry`].
//!
//! OAuth connections are **not** actively probed: their liveness check would
//! consume subscription quota and can trigger token-refresh side effects. They
//! are still covered by the existing rate-limit / model-lock cooldowns.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tracing::{debug, warn};

use super::probe::probe_connection;
use super::{HealthRecord, DEGRADED_UNTIL_KEY, HEALTH_CHECKED_AT_KEY, HEALTH_STATUS_KEY};
use crate::core::circuit_breaker::CircuitBreakerRegistry;
use crate::core::proxy::resolve_proxy_target;
use crate::server::state::AppState;
use crate::types::ProviderConnection;

/// Probe interval. 3 min keeps a 10 min `503` degrade window observable while
/// costing at most one cheap GET per connection per interval.
pub const HEALTH_TICK_INTERVAL: Duration = Duration::from_secs(180);
/// Delay before the first tick so boot-time work (migrations, OAuth refresh)
/// finishes first.
const BOOT_DELAY: Duration = Duration::from_secs(10);
/// Upper bound on probes per tick — protects against pathological configs with
/// hundreds of connections.
const MAX_PROBES_PER_TICK: usize = 48;
/// Only re-persist a degrade window when it moved by at least this much.
const PERSIST_DRIFT_SECS: i64 = 60;
/// Settings flag (in `settings.extra`) to disable the daemon.
const SETTINGS_ENABLED_KEY: &str = "healthCheckEnabled";
/// Circuit-breaker endpoint label for health-driven transitions.
const BREAKER_ENDPOINT: &str = "/chat/completions";

static TICK_RUNNING: AtomicBool = AtomicBool::new(false);

/// Spawn the health daemon. Best-effort; safe to call once at process boot
/// alongside `spawn_quota_auto_ping`.
pub fn spawn_health_daemon(state: AppState) {
    tokio::spawn(async move {
        tokio::time::sleep(BOOT_DELAY).await;
        loop {
            let _ = run_health_tick(&state).await;
            tokio::time::sleep(HEALTH_TICK_INTERVAL).await;
        }
    });
}

/// Run one health tick. Returns a JSON summary (same shape convention as
/// `quota_auto_ping::run_quota_auto_ping_tick`).
pub async fn run_health_tick(state: &AppState) -> Value {
    if TICK_RUNNING.swap(true, Ordering::SeqCst) {
        return json!({ "ok": true, "skipped": true, "reason": "tick already running" });
    }
    let result = tick_inner(state).await;
    TICK_RUNNING.store(false, Ordering::SeqCst);
    result
}

async fn tick_inner(state: &AppState) -> Value {
    let snapshot = state.db.snapshot();
    if snapshot
        .settings
        .extra
        .get(SETTINGS_ENABLED_KEY)
        .and_then(Value::as_bool)
        == Some(false)
    {
        return json!({ "ok": true, "skipped": true, "reason": "healthCheckEnabled=false" });
    }

    let candidates: Vec<ProviderConnection> = snapshot
        .provider_connections
        .iter()
        .filter(|conn| conn.is_active() && is_api_key_auth(&conn.auth_type))
        .take(MAX_PROBES_PER_TICK)
        .cloned()
        .collect();

    let mut results: Vec<Value> = Vec::new();
    let mut pending: Vec<(String, HealthRecord)> = Vec::new();
    let mut probed = 0u32;
    let mut degraded = 0u32;

    for conn in &candidates {
        let proxy = resolve_proxy_target(&snapshot, conn, &snapshot.settings);
        let client = match state.client_pool.get("health-probe", proxy.as_ref()) {
            Ok(client) => client,
            Err(error) => {
                warn!(
                    target: "openproxy::health",
                    connection_id = %conn.id,
                    error = %error,
                    "health probe: client pool unavailable"
                );
                continue;
            }
        };

        let Some(outcome) = probe_connection(&client, conn).await else {
            // No API key or unknown base URL — nothing cheap to probe.
            continue;
        };
        probed += 1;

        let record = state.health.record_probe(
            &conn.id,
            &conn.provider,
            outcome.http_status,
            outcome.error.clone(),
        );
        apply_to_breaker(&state.circuit_breaker, conn, &record);

        if record.degraded_until.is_some() {
            degraded += 1;
        }
        if needs_persist(conn, &record) {
            pending.push((conn.id.clone(), record.clone()));
        }

        results.push(json!({
            "connectionId": conn.id,
            "provider": conn.provider,
            "status": record.status.as_str(),
            "httpStatus": record.http_status,
            "degradedUntil": record.degraded_until.map(|value| value.to_rfc3339()),
            "consecutiveFailures": record.consecutive_failures,
            "url": outcome.url,
        }));
    }

    if !pending.is_empty() {
        persist_records(state, pending).await;
    }

    debug!(
        target: "openproxy::health",
        probed, degraded, "health tick complete"
    );

    json!({
        "ok": true,
        "candidates": candidates.len(),
        "probed": probed,
        "degraded": degraded,
        "results": results,
    })
}

/// Feed the health verdict into the circuit breaker so its Open window matches
/// the degrade window for the status (2 min on 429, 10 min on 503, 5 min on
/// 500/502/504). Keyed `"{provider}:{connection_id}"` for per-account
/// granularity; the endpoint label documents which dispatch path it guards.
fn apply_to_breaker(
    breaker: &CircuitBreakerRegistry,
    conn: &ProviderConnection,
    record: &HealthRecord,
) {
    let key = breaker_key(&conn.provider, &conn.id);
    breaker.record_status(&key, record.http_status);
}

/// Circuit-breaker key used for health-driven transitions.
pub fn breaker_key(provider: &str, connection_id: &str) -> String {
    CircuitBreakerRegistry::key(&format!("{provider}:{connection_id}"), BREAKER_ENDPOINT)
}

/// Persist status transitions onto the connection's `extra` map so
/// `account_fallback::is_account_unavailable` can gate dispatch without
/// consulting in-process state, and so the dashboard can show the cause.
async fn persist_records(state: &AppState, pending: Vec<(String, HealthRecord)>) {
    let now = Utc::now().to_rfc3339();
    let _ = state
        .db
        .update(move |db| {
            for (connection_id, record) in &pending {
                let Some(conn) = db
                    .provider_connections
                    .iter_mut()
                    .find(|conn| &conn.id == connection_id)
                else {
                    continue;
                };
                conn.extra
                    .insert(HEALTH_STATUS_KEY.into(), json!(record.status.as_str()));
                conn.extra.insert(
                    HEALTH_CHECKED_AT_KEY.into(),
                    json!(record.checked_at.to_rfc3339()),
                );
                match record.degraded_until {
                    Some(until) => {
                        conn.extra
                            .insert(DEGRADED_UNTIL_KEY.into(), json!(until.to_rfc3339()));
                    }
                    None => {
                        conn.extra.remove(DEGRADED_UNTIL_KEY);
                    }
                }
                conn.updated_at = Some(now.clone());
            }
        })
        .await;
}

/// Whether the record changed enough to justify a DB write: status flipped, a
/// degrade window was opened/closed, or the window moved by ≥ 60 s.
pub(super) fn needs_persist(conn: &ProviderConnection, record: &HealthRecord) -> bool {
    let stored_status = conn
        .extra
        .get(HEALTH_STATUS_KEY)
        .and_then(Value::as_str)
        .unwrap_or_default();
    if stored_status != record.status.as_str() {
        return true;
    }

    let stored_until = conn
        .extra
        .get(DEGRADED_UNTIL_KEY)
        .and_then(Value::as_str)
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc));

    match (stored_until, record.degraded_until) {
        (None, None) => false,
        (None, Some(_)) | (Some(_), None) => true,
        (Some(stored), Some(next)) => (next - stored).num_seconds().abs() >= PERSIST_DRIFT_SECS,
    }
}

fn is_api_key_auth(auth_type: &str) -> bool {
    matches!(
        auth_type.trim().to_ascii_lowercase().as_str(),
        "apikey" | "api_key" | "api-key"
    )
}
