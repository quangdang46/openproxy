//! Antigravity live quota cache + strike-based circuit breaker.
//!
//! Port of 9router `src/sse/services/antigravityQuota.js` (bead
//! openproxy-v5116-sync-mozz.23 — gap noted when mozza.14 closed).
//!
//! Upstream keeps an in-memory `connectionId → quotas` map populated lazily:
//! the auth pre-filter (`auth.js`) skips accounts whose cached quota for the
//! requested model is exhausted, and the chat error path
//! (`handleAntigravityQuotaError`) refreshes the cache on 409/429, then
//! either cache-blocks the pair until the upstream `resetAt` or — when the
//! quota API keeps reporting remaining quota while generation endpoints keep
//! 429ing (sprint/weekly dual-pool mismatch, #3681) — trips a strike breaker
//! that blocks the pair for 15 minutes.
//!
//! # Concurrency
//!
//! All state lives behind one `RwLock`; refreshes are deduplicated per
//! connection with a per-connection `tokio::sync::Mutex` (upstream
//! `inflightRefresh` map) and throttled to one upstream call per 30s per
//! connection (`MIN_REFRESH_INTERVAL_MS`).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex as StdMutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use super::quota_fetcher::fetch_antigravity_quota;

const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const STRIKE_WINDOW_MS: u64 = 60_000;
const STRIKE_THRESHOLD: u32 = 3;
const STRIKE_BLOCK_MS: u64 = 15 * 60_000;

#[derive(Debug, Default)]
struct StrikeState {
    count: u32,
    /// Anchored at the FIRST qualifying strike (fixed window).
    window_start_ms: u64,
}

#[derive(Debug, Default)]
struct AntigravityQuotaState {
    /// connection_id → quotas object (`{modelId: entry, claude_gpt_weekly: …}`).
    quotas: HashMap<String, Value>,
    last_refresh: HashMap<String, Instant>,
    /// "connectionId|model" → strike counter.
    strikes: HashMap<String, StrikeState>,
    /// "connectionId|model" → blockedUntil epoch ms.
    blocks: HashMap<String, u64>,
}

static STATE: LazyLock<RwLock<AntigravityQuotaState>> =
    LazyLock::new(|| RwLock::new(AntigravityQuotaState::default()));

/// Per-connection refresh locks (upstream `inflightRefresh` dedup map).
static REFRESH_LOCKS: LazyLock<StdMutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn iso_from_ms(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let nanos = ((ms % 1000) * 1_000_000) as u32;
    chrono::DateTime::from_timestamp(secs, nanos)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

/// Parse a quota `resetAt` (RFC 3339 string or epoch number) to epoch ms.
fn parse_reset_ms(value: &Value) -> Option<u64> {
    match value {
        Value::String(s) => {
            if let Ok(f) = s.parse::<f64>() {
                let ms = if f >= 1_000_000_000_000.0 {
                    f
                } else {
                    f * 1000.0
                };
                return Some(ms as u64);
            }
            chrono::DateTime::parse_from_rfc3339(s)
                .or_else(|_| chrono::DateTime::parse_from_rfc3339(&format!("{s}Z")))
                .ok()
                .map(|dt| dt.timestamp_millis().max(0) as u64)
        }
        Value::Number(n) => n.as_f64().map(|f| {
            let ms = if f >= 1_000_000_000_000.0 {
                f
            } else {
                f * 1000.0
            };
            ms as u64
        }),
        _ => None,
    }
}

fn remaining_percentage(entry: &Value) -> Option<f64> {
    entry.get("remainingPercentage")?.as_f64()
}

/// Strip a trailing `(…)` suffix (upstream `replace(/\([^()]+\)\s*$/, "")`).
fn strip_paren_suffix(model: &str) -> &str {
    let t = model.trim_end();
    if t.ends_with(')') {
        if let Some(open) = t.rfind('(') {
            let inner = &t[open + 1..t.len() - 1];
            if !inner.trim().is_empty() && !inner.contains(['(', ')']) {
                return t[..open].trim_end();
            }
        }
    }
    t
}

/// Model family for Antigravity quota buckets ("claude_gpt", "gemini_argon",
/// "gemini") — port of `getAntigravityModelFamily`.
pub fn antigravity_model_family(model: &str) -> Option<&'static str> {
    let mut clean = model;
    if let Some(idx) = clean.find('/') {
        clean = &clean[idx + 1..];
    }
    let clean = clean.to_ascii_lowercase();
    if clean.starts_with("claude-") || clean.starts_with("gpt-") || clean == "claude" {
        return Some("claude_gpt");
    }
    if (clean.starts_with("gemini-") && clean.contains("argon")) || clean == "gemini_argon" {
        return Some("gemini_argon");
    }
    if (clean.starts_with("gemini-") && !clean.contains("image")) || clean == "gemini" {
        return Some("gemini");
    }
    None
}

/// Resolve quota info for a model from a connection's quotas map — port of
/// `getAntigravityModelQuota`. Checks the direct entry, tier aliases, and the
/// shared family summary buckets (5h session + weekly). Returns the winning
/// bucket: the most-constrained candidate, or the exhausted bucket with the
/// latest resetAt when any candidate reads 0%.
pub fn antigravity_model_quota(quotas: &Value, model: &str) -> Option<Value> {
    let obj = quotas.as_object()?;
    if model.is_empty() {
        return None;
    }

    // 1. Normalise: strip `provider/` prefix and a trailing `(tier)` suffix.
    let mut clean = model;
    if let Some(idx) = clean.find('/') {
        clean = &clean[idx + 1..];
    }
    let clean = strip_paren_suffix(clean.trim()).trim();
    if clean.is_empty() {
        return None;
    }
    let clean_lower = clean.to_ascii_lowercase();

    let family = antigravity_model_family(&clean_lower);
    let mut candidates: Vec<&Value> = Vec::new();

    // 2. Direct entry.
    if let Some(q) = obj.get(clean) {
        candidates.push(q);
    }
    // The map keys are the raw upstream ids (lowercase); retry case-insensitively
    // is unnecessary — upstream ids are already lowercase.
    if candidates.is_empty() {
        if let Some(q) = obj.get(&clean_lower) {
            candidates.push(q);
        }
    }

    // 3. Tier aliases for unadorned base ids.
    const TIER_ALIASES: &[(&str, &str)] = &[
        ("gemini-4-argon", "gemini-4-argon-high"),
        ("claude-sonnet-5-5", "claude-sonnet-5-5-high"),
        ("claude-opus-5-5", "claude-opus-5-5-high"),
        ("gemini-3.8-flash", "gemini-3.8-flash-medium"),
        ("gemini-3.7-flash", "gemini-3.7-flash-medium"),
        ("gemini-3.6-flash", "gemini-3.6-flash-medium"),
    ];
    for &(base, alias) in TIER_ALIASES {
        if clean_lower == base {
            if let Some(q) = obj.get(alias) {
                candidates.push(q);
            }
        }
    }

    // 4. Shared family summary buckets.
    fn push_key<'a>(
        obj: &'a serde_json::Map<String, Value>,
        key: &str,
        candidates: &mut Vec<&'a Value>,
    ) {
        if let Some(q) = obj.get(key) {
            candidates.push(q);
        }
    }
    match family {
        Some("claude_gpt") => {
            push_key(obj, "claude_gpt_session", &mut candidates);
            push_key(obj, "claude_gpt_weekly", &mut candidates);
        }
        Some("gemini_argon") => {
            push_key(obj, "gemini_argon_session", &mut candidates);
            push_key(obj, "gemini_argon_weekly", &mut candidates);
            // Ultra plans share the main Gemini bucket — no separate argon
            // summary, so fall back to the shared Gemini buckets.
            if !obj.contains_key("gemini_argon_session") && !obj.contains_key("gemini_argon_weekly")
            {
                push_key(obj, "gemini_session", &mut candidates);
                push_key(obj, "gemini_weekly", &mut candidates);
            }
        }
        Some("gemini") => {
            push_key(obj, "gemini_session", &mut candidates);
            push_key(obj, "gemini_weekly", &mut candidates);
        }
        _ => {}
    }

    if candidates.is_empty() {
        return None;
    }

    // 5. Any exhausted (0%) bucket with a future resetAt wins; use the latest
    // resetAt so we do not unblock prematurely.
    let now = now_ms();
    let mut exhausted: Vec<(u64, &Value)> = Vec::new();
    for c in &candidates {
        let Some(pct) = remaining_percentage(c) else {
            continue;
        };
        if pct > 0.0 {
            continue;
        }
        match c.get("resetAt").and_then(parse_reset_ms) {
            Some(reset_ms) if reset_ms > now => exhausted.push((reset_ms, c)),
            None => exhausted.push((0, c)),
            _ => {}
        }
    }
    if !exhausted.is_empty() {
        exhausted.sort_by(|a, b| b.0.cmp(&a.0));
        let primary = exhausted[0].1;
        let mut out = primary.clone();
        if let Some(obj) = out.as_object_mut() {
            obj.insert("remainingPercentage".into(), json!(0));
            if !obj.contains_key("displayName") {
                obj.insert("displayName".into(), json!(clean));
            }
        }
        return Some(out);
    }

    // 6. Otherwise the most constrained (lowest remainingPercentage) bucket.
    let mut valid: Vec<&Value> = candidates
        .iter()
        .copied()
        .filter(|c| remaining_percentage(c).is_some())
        .collect();
    if valid.is_empty() {
        return None;
    }
    valid.sort_by(|a, b| {
        remaining_percentage(a)
            .partial_cmp(&remaining_percentage(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Some(valid[0].clone())
}

/// auth.js pre-filter check: `getAntigravityModelQuota(quotas, model) ||
/// quotas[model]` blocks when remainingPercentage <= 0 and resetAt is in the
/// future. Returns the blocking entry (for resetAt extraction) or None.
pub fn antigravity_blocking_quota(quotas: &Value, model: &str) -> Option<Value> {
    let quota = antigravity_model_quota(quotas, model).or_else(|| quotas.get(model).cloned())?;
    let pct = remaining_percentage(&quota)?;
    if pct > 0.0 {
        return None;
    }
    let reset_ms = quota.get("resetAt").and_then(parse_reset_ms)?;
    if reset_ms <= now_ms() {
        return None;
    }
    Some(quota)
}

/// Cached quotas object for a connection (clone), if any.
pub fn antigravity_quota_cache_get(connection_id: &str) -> Option<Value> {
    STATE
        .read()
        .ok()
        .and_then(|st| st.quotas.get(connection_id).cloned())
}

/// auth.js pre-filter: should this connection be skipped for `model` because
/// its cached Antigravity quota is exhausted? False when nothing is cached.
pub fn antigravity_account_quota_blocked(connection_id: &str, model: &str) -> bool {
    let Some(quotas) = antigravity_quota_cache_get(connection_id) else {
        return false;
    };
    antigravity_blocking_quota(&quotas, model).is_some()
}

/// auth.js "all accounts locked" path: the quota-cache resetAt that should
/// feed the retry-after calculation for this connection+model, when the
/// cached reading is exhausted with a future reset.
pub fn antigravity_quota_reset_after(
    connection_id: &str,
    model: &str,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let quotas = antigravity_quota_cache_get(connection_id)?;
    let quota = antigravity_model_quota(&quotas, model).or_else(|| quotas.get(model).cloned())?;
    let reset_ms = quota.get("resetAt").and_then(parse_reset_ms)?;
    if reset_ms <= now_ms() {
        return None;
    }
    chrono::DateTime::from_timestamp_millis(reset_ms as i64)
}

/// Re-apply active strike blocks onto a fresh quotas snapshot so the
/// pre-filter keeps skipping the blocked pair until the block expires —
/// port of `applyActiveStrikeBlocks`.
fn apply_active_strike_blocks(
    state: &mut AntigravityQuotaState,
    connection_id: &str,
    quotas: &mut Value,
) {
    let now = now_ms();
    let prefix = format!("{connection_id}|");
    let expired: Vec<String> = state
        .blocks
        .iter()
        .filter(|(key, until)| key.starts_with(&prefix) && **until <= now)
        .map(|(key, _)| key.clone())
        .collect();
    for key in expired {
        state.blocks.remove(&key);
    }
    let Some(obj) = quotas.as_object_mut() else {
        return;
    };
    let blocked: Vec<(String, u64)> = state
        .blocks
        .iter()
        .filter(|(key, _)| key.starts_with(&prefix))
        .map(|(key, until)| (key[prefix.len()..].to_string(), *until))
        .collect();
    for (model, until) in blocked {
        obj.insert(
            model,
            json!({
                "remainingPercentage": 0,
                "resetAt": iso_from_ms(until),
            }),
        );
    }
}

/// Refresh quota for a single Antigravity connection from the upstream API.
/// Updates the in-memory cache only; cache expiry is the upstream model
/// resetAt. Returns the quotas map, or None on failure/throttle (the caller
/// keeps the previous cache — upstream preserves known cache on 401/403).
pub async fn refresh_antigravity_quota(connection_id: &str, access_token: &str) -> Option<Value> {
    // Coalesce concurrent refreshes before applying the interval gate
    // (upstream `inflightRefresh`).
    let lock = {
        let mut locks = REFRESH_LOCKS.lock().ok()?;
        locks
            .entry(connection_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let _guard = lock.lock().await;

    {
        let st = STATE.read().ok()?;
        if let Some(last) = st.last_refresh.get(connection_id) {
            if last.elapsed() < MIN_REFRESH_INTERVAL {
                return st.quotas.get(connection_id).cloned();
            }
        }
    }

    // Record every attempt so failed quota calls cannot amplify an upstream
    // 429 burst (upstream sets lastRefreshAt before the fetch).
    if let Ok(mut st) = STATE.write() {
        st.last_refresh
            .insert(connection_id.to_string(), Instant::now());
    }

    let usage = fetch_antigravity_quota(access_token, "antigravity").await;
    // 401/403 usage responses can contain an empty quotas object plus
    // `message` — preserve known cache instead of replacing it.
    if usage.get("message").is_some() {
        return None;
    }
    let mut quotas = usage.get("quotas")?.clone();
    if !quotas.is_object() {
        return None;
    }

    if let Ok(mut st) = STATE.write() {
        apply_active_strike_blocks(&mut st, connection_id, &mut quotas);
        st.quotas.insert(connection_id.to_string(), quotas.clone());
    }
    Some(quotas)
}

/// Handle an Antigravity 409/429 — refresh the live quota cache and return
/// the block deadline (epoch ms) when the pair must be skipped, or None to
/// fall through to the normal account-unavailable path.
///
/// Port of `handleAntigravityQuotaError`:
/// - quota reading exhausted with a future resetAt → cache-block until resetAt;
/// - quota reading optimistic/unavailable → strike counter; 3 strikes inside
///   60s (fixed window anchored at the first strike) → synthesize a 0% cache
///   entry and block the pair for 15m.
pub async fn handle_antigravity_quota_error(
    connection_id: &str,
    status: u16,
    model: &str,
    access_token: &str,
) -> Option<u64> {
    let _ = status; // 409 and 429 count identically upstream (see below)
    let quotas = refresh_antigravity_quota(connection_id, access_token).await;
    let quota = quotas
        .as_ref()
        .and_then(|q| antigravity_model_quota(q, model));

    // Strike breaker: count every 429/409 whose quota reading is either
    // optimistic (remaining > 0) or unavailable (quota API error). Upstream
    // counts both statuses by design — Antigravity signals pool exhaustion
    // with 409 as well (#3561).
    let optimistic = quota
        .as_ref()
        .and_then(remaining_percentage)
        .map(|pct| pct > 0.0)
        .unwrap_or(true);
    if optimistic {
        let key = format!("{connection_id}|{model}");
        let now = now_ms();
        let count = {
            let mut st = STATE.write().ok()?;
            let strike = st.strikes.get(&key);
            let window_start = match strike {
                Some(s) if now.saturating_sub(s.window_start_ms) <= STRIKE_WINDOW_MS => {
                    s.window_start_ms
                }
                _ => now,
            };
            let count = match strike {
                Some(s) if s.window_start_ms == window_start => s.count + 1,
                _ => 1,
            };
            st.strikes.insert(
                key.clone(),
                StrikeState {
                    count,
                    window_start_ms: window_start,
                },
            );
            count
        };
        if count >= STRIKE_THRESHOLD {
            let blocked_until = now + STRIKE_BLOCK_MS;
            if let Ok(mut st) = STATE.write() {
                st.strikes.remove(&key);
                // Synthesize a 0% entry in the shared cache so the
                // pre-filter skips this pair on subsequent requests too.
                let mut cached = st
                    .quotas
                    .get(connection_id)
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if let Some(obj) = cached.as_object_mut() {
                    obj.insert(
                        model.to_string(),
                        json!({
                            "remainingPercentage": 0,
                            "resetAt": iso_from_ms(blocked_until),
                        }),
                    );
                }
                st.quotas.insert(connection_id.to_string(), cached);
                st.blocks.insert(key, blocked_until);
            }
            return Some(blocked_until);
        }
        return None;
    }

    // Healthy-but-exhausted reading: clear strikes and use the exact resetAt.
    let key = format!("{connection_id}|{model}");
    if let Ok(mut st) = STATE.write() {
        st.strikes.remove(&key);
    }
    let quota = quota?;
    let reset_value = quota.get("resetAt")?.clone();
    let reset_ms = parse_reset_ms(&reset_value)?;
    if reset_ms <= now_ms() {
        return None;
    }

    // Cache-block the requested model under its model name (RAM only — the
    // caller must NOT persist a modelLock_* for this path, upstream parity).
    if let Ok(mut st) = STATE.write() {
        let mut cached = st
            .quotas
            .get(connection_id)
            .cloned()
            .unwrap_or_else(|| json!({}));
        if let Some(obj) = cached.as_object_mut() {
            obj.insert(
                model.to_string(),
                json!({
                    "remainingPercentage": 0,
                    "resetAt": reset_value,
                }),
            );
        }
        st.quotas.insert(connection_id.to_string(), cached);
    }
    Some(reset_ms)
}

/// Clear strike state for a connection|model after a successful request, so
/// "consecutive" strikes means consecutive — port of `clearAntigravityStrikes`.
/// Only removes a synthesized cache entry (resetAt == our block deadline); a
/// real upstream 0% reading stays.
pub fn clear_antigravity_strikes(connection_id: &str, model: &str) {
    let key = format!("{connection_id}|{model}");
    let until = {
        let Ok(mut st) = STATE.write() else {
            return;
        };
        st.strikes.remove(&key);
        st.blocks.remove(&key)
    };
    let Some(until) = until else {
        return;
    };
    let iso = iso_from_ms(until);
    let Ok(mut st) = STATE.write() else {
        return;
    };
    let Some(cached) = st.quotas.get_mut(connection_id) else {
        return;
    };
    let reset_matches = |entry: Option<&Value>| {
        entry.and_then(|e| e.get("resetAt")).and_then(Value::as_str) == Some(iso.as_str())
    };
    if let Some(obj) = cached.as_object_mut() {
        if reset_matches(obj.get(model)) {
            obj.remove(model);
        }
        let family = antigravity_model_family(model);
        let session_key = match family {
            Some("claude_gpt") => Some("claude_gpt_session"),
            Some("gemini_argon") => Some("gemini_argon_session"),
            Some("gemini") => Some("gemini_session"),
            _ => None,
        };
        if let Some(sk) = session_key {
            if reset_matches(obj.get(sk)) {
                obj.remove(sk);
            }
        }
    }
}

/// Test hook: clear all antigravity quota state.
#[cfg(test)]
pub(crate) fn reset_antigravity_quota_state_for_tests() {
    if let Ok(mut st) = STATE.write() {
        *st = AntigravityQuotaState::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn quotas_fixture() -> Value {
        json!({
            "gemini-3.8-flash-medium": { "used": 900, "total": 1000, "remainingPercentage": 10.0, "resetAt": "2999-01-01T00:00:00Z" },
            "gemini-3.8-flash-high": { "used": 1000, "total": 1000, "remainingPercentage": 0.0, "resetAt": "2999-01-01T00:00:00Z" },
            "claude-sonnet-5-5-high": { "used": 500, "total": 1000, "remainingPercentage": 50.0, "resetAt": "2999-01-01T00:00:00Z" },
            "gemini_argon_weekly": { "used": 1000, "total": 1000, "remainingPercentage": 0.0, "resetAt": "2999-06-01T00:00:00Z" },
            "gemini_weekly": { "used": 100, "total": 1000, "remainingPercentage": 90.0, "resetAt": "2999-06-01T00:00:00Z" },
        })
    }

    #[test]
    fn model_family_classification() {
        assert_eq!(
            antigravity_model_family("claude-sonnet-5-5-high"),
            Some("claude_gpt")
        );
        assert_eq!(
            antigravity_model_family("gpt-oss-120b-medium"),
            Some("claude_gpt")
        );
        assert_eq!(
            antigravity_model_family("gemini-4-argon-high"),
            Some("gemini_argon")
        );
        assert_eq!(
            antigravity_model_family("gemini-3.8-flash-high"),
            Some("gemini")
        );
        assert_eq!(antigravity_model_family("gemini-3.1-flash-image"), None);
        assert_eq!(
            antigravity_model_family("ag/gemini-3.8-flash"),
            Some("gemini")
        );
    }

    #[test]
    fn direct_entry_and_tier_alias() {
        let q = quotas_fixture();
        let m = antigravity_model_quota(&q, "gemini-3.8-flash-medium").unwrap();
        assert_eq!(m["remainingPercentage"], 10.0);
        // Unadorned base id resolves via the tier alias.
        let m = antigravity_model_quota(&q, "gemini-3.8-flash").unwrap();
        assert_eq!(m["remainingPercentage"], 10.0);
        // provider/ prefix + (tier) suffix stripped.
        let m = antigravity_model_quota(&q, "ag/gemini-3.8-flash-medium(high)").unwrap();
        assert_eq!(m["remainingPercentage"], 10.0);
    }

    #[test]
    fn exhausted_bucket_wins_with_latest_reset() {
        let q = quotas_fixture();
        // gemini-3.8-flash-high reads 0% with a future reset → exhausted.
        let m = antigravity_model_quota(&q, "gemini-3.8-flash-high").unwrap();
        assert_eq!(m["remainingPercentage"], 0);
        // Argon family falls back to shared gemini buckets only when no argon
        // summary exists; here gemini_argon_weekly is 0% → exhausted.
        let m = antigravity_model_quota(&q, "gemini-4-argon-high").unwrap();
        assert_eq!(m["remainingPercentage"], 0);
        // claude family has no exhausted bucket → most constrained wins.
        let m = antigravity_model_quota(&q, "claude-sonnet-5-5").unwrap();
        assert!(m["remainingPercentage"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn strike_block_synthesizes_zero_entry_and_clears_on_success() {
        reset_antigravity_quota_state_for_tests();
        let cid = "conn-test";
        let model = "gemini-3.8-flash-high";
        // Seed an optimistic cache entry, then synthesize exactly what the
        // strike path writes after STRIKE_THRESHOLD 429s: a 0% entry whose
        // resetAt is our own block deadline, plus the blocks-map entry.
        let until = now_ms() + STRIKE_BLOCK_MS;
        {
            let mut st = STATE.write().unwrap();
            let entry = json!({ "remainingPercentage": 50.0, "resetAt": "2999-01-01T00:00:00Z" });
            let mut quotas_obj = serde_json::Map::new();
            quotas_obj.insert(model.to_string(), entry);
            st.quotas.insert(cid.to_string(), Value::Object(quotas_obj));
            let mut cached = st.quotas.get(cid).cloned().unwrap();
            cached[model] = json!({ "remainingPercentage": 0, "resetAt": iso_from_ms(until) });
            st.quotas.insert(cid.to_string(), cached);
            st.blocks.insert(format!("{cid}|{model}"), until);
        }
        // auth.js pre-filter now skips the pair.
        assert!(antigravity_account_quota_blocked(cid, model));
        assert_eq!(
            antigravity_quota_reset_after(cid, model).map(|dt| dt.timestamp_millis() as u64),
            Some(until)
        );
        // A real upstream 0% reading (different resetAt) is NOT cleared.
        clear_antigravity_strikes(cid, model);
        assert!(!antigravity_account_quota_blocked(cid, model));
        reset_antigravity_quota_state_for_tests();
    }

    #[test]
    fn blocking_quota_requires_future_reset() {
        let now = now_ms();
        // Expired reset → not blocking.
        let q = json!({ "m": { "remainingPercentage": 0.0, "resetAt": iso_from_ms(now - 1000) } });
        assert!(antigravity_blocking_quota(&q, "m").is_none());
        // Future reset → blocking.
        let q =
            json!({ "m": { "remainingPercentage": 0.0, "resetAt": iso_from_ms(now + 60_000) } });
        assert!(antigravity_blocking_quota(&q, "m").is_some());
        // Positive remaining → not blocking.
        let q =
            json!({ "m": { "remainingPercentage": 5.0, "resetAt": iso_from_ms(now + 60_000) } });
        assert!(antigravity_blocking_quota(&q, "m").is_none());
    }
}
