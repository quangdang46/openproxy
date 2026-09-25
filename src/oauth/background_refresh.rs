//! Background proactive OAuth token-refresh scheduler — port of 9router
//! `src/sse/services/backgroundTokenRefresh.js`.
//!
//! Independent of inbound requests. Fail-open everywhere: tick errors and
//! per-connection failures never kill the interval.
//!
//! - Tick every 5 minutes, first pass after 10 seconds.
//! - Select active OAuth connections with a refresh token whose access token
//!   expires within `max(provider lead, BACKGROUND_REFRESH_LEAD_MS)` (30 min).
//! - Dispatch through the same per-provider `dispatch_oauth_refresh` used by
//!   the request path, then persist the new tokens.

use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Refresh when expiry is within 30 minutes (or the provider on-request
/// lead, whichever is larger) — JS BACKGROUND_REFRESH_LEAD_MS.
pub const BACKGROUND_REFRESH_LEAD_MS: u64 = 30 * 60 * 1000;
const TICK_INTERVAL_SECS: u64 = 5 * 60;
const INITIAL_DELAY_SECS: u64 = 10;

/// Providers whose upstream rate-limits by OAuth client id (JS
/// SENSITIVE_PROVIDERS) — a burst of refreshes trips their abuse limits and
/// invalid_grants a share of them.
const SENSITIVE_PROVIDERS: [&str; 2] = ["antigravity", "gemini-cli"];
const SENSITIVE_DELAY_MS: u64 = 12_000;
const NORMAL_DELAY_MS: u64 = 1_500;
const SENSITIVE_JITTER_MS: u64 = 4_000;
const NORMAL_JITTER_MS: u64 = 200;

static TICK_RUNNING: AtomicBool = AtomicBool::new(false);

/// Per-provider on-request refresh lead (JS getRefreshLeadMs). Providers not
/// listed fall back to BACKGROUND_REFRESH_LEAD_MS alone.
fn provider_lead_ms(provider: &str) -> Option<u64> {
    use crate::oauth::token_refresh as tr;
    let lead = match provider {
        "codex" | "opencode" | "cx" => tr::REFRESH_LEAD_CODEX_MS,
        "openai" => tr::REFRESH_LEAD_OPENAI_MS,
        "claude" | "anthropic" => tr::REFRESH_LEAD_CLAUDE_MS,
        "iflow" => tr::REFRESH_LEAD_IFLOW_MS,
        "qwen" => tr::REFRESH_LEAD_QWEN_MS,
        "kimi-coding" | "kimi" => tr::REFRESH_LEAD_KIMI_CODING_MS,
        "antigravity" | "gemini-cli" | "gemini" => tr::REFRESH_LEAD_ANTIGRAVITY_MS,
        "xai" | "grok-cli" | "gcli" | "gb" => tr::REFRESH_LEAD_XAI_MS,
        _ => return None,
    };
    Some(lead)
}

/// Pure selection: OAuth connections with a refreshToken whose access token
/// expires within max(provider lead, BACKGROUND_REFRESH_LEAD_MS).
/// Mirrors JS selectConnectionsNeedingRefresh.
pub fn select_connections_needing_refresh(
    connections: &[crate::types::ProviderConnection],
    now_ms: i64,
) -> Vec<crate::types::ProviderConnection> {
    connections
        .iter()
        .filter(|conn| {
            if !conn.is_active() {
                return false;
            }
            let auth_type = conn.auth_type.to_ascii_lowercase().replace('_', "");
            if auth_type != "oauth" {
                return false;
            }
            let Some(refresh_token) = conn.refresh_token.as_deref().filter(|r| !r.is_empty())
            else {
                return false;
            };
            let _ = refresh_token;
            let Some(expires_at) = conn.expires_at.as_deref() else {
                return false;
            };
            let Ok(expires_at) = chrono::DateTime::parse_from_rfc3339(expires_at) else {
                return false;
            };
            let expires_at_ms = expires_at.timestamp_millis();
            let lead = provider_lead_ms(&conn.provider)
                .unwrap_or(0)
                .max(BACKGROUND_REFRESH_LEAD_MS);
            expires_at_ms - now_ms < lead as i64
        })
        .cloned()
        .collect()
}

/// Pace to insert *after* refreshing one account (JS
/// `backgroundTokenRefresh.js:126-135`). The base delay is operator-tunable per
/// class via env var; a zero or unparseable value falls back to the default, the
/// same `Number(env) || default` fallback the JS relies on.
fn inter_account_delay_ms(provider: &str) -> u64 {
    let sensitive = SENSITIVE_PROVIDERS.contains(&provider);
    let (env_key, default_base, default_jitter) = if sensitive {
        (
            "BG_REFRESH_GOOGLE_DELAY_MS",
            SENSITIVE_DELAY_MS,
            SENSITIVE_JITTER_MS,
        )
    } else {
        ("BG_REFRESH_DELAY_MS", NORMAL_DELAY_MS, NORMAL_JITTER_MS)
    };
    let base = std::env::var(env_key)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default_base);
    base + if sensitive {
        rand::random::<u64>() % SENSITIVE_JITTER_MS
    } else {
        default_jitter
    }
}

/// Delays to sleep after each due account, in order. The final account gets no
/// trailing delay — pacing spaces out a burst, it does not pad the tail.
fn paced_refresh_delays(providers: &[&str]) -> Vec<Duration> {
    providers
        .iter()
        .take(providers.len().saturating_sub(1))
        .map(|provider| Duration::from_millis(inter_account_delay_ms(provider)))
        .collect()
}

/// One scheduler tick. Fail-open at top level and per connection.
async fn run_tick(state: &crate::server::state::AppState) {
    if TICK_RUNNING.swap(true, Ordering::SeqCst) {
        tracing::debug!(target: "openproxy::bg_token_refresh", "tick already running, skip");
        return;
    }
    let _guard = TickGuard;

    let snapshot = state.db.snapshot();
    let due = select_connections_needing_refresh(&snapshot.provider_connections, now_ms());
    if due.is_empty() {
        return;
    }
    tracing::info!(target: "openproxy::bg_token_refresh", "refreshing {} due OAuth connection(s)", due.len());

    let pacing: Vec<Duration> = {
        let providers: Vec<&str> = due.iter().map(|conn| conn.provider.as_str()).collect();
        paced_refresh_delays(&providers)
    };

    for (index, conn) in due.iter().enumerate() {
        let Some(refresh_token) = conn.refresh_token.clone() else {
            continue;
        };
        match crate::oauth::token_refresh::dispatch_oauth_refresh(
            &conn.provider,
            &refresh_token,
            &conn.provider_specific_data,
        )
        .await
        {
            Ok(result) => {
                persist_refresh(state, &conn.id, &result).await;
                tracing::info!(target: "openproxy::bg_token_refresh",
                    "connection {} ({}) refreshed", conn.id, conn.provider);
            }
            Err(e) => {
                // Fail-open: log and move on.
                tracing::warn!(target: "openproxy::bg_token_refresh",
                    "connection {} ({}) refresh failed: {e}", conn.id, conn.provider);
            }
        }

        if let Some(delay) = pacing.get(index) {
            tokio::time::sleep(*delay).await;
        }
    }
}

struct TickGuard;
impl Drop for TickGuard {
    fn drop(&mut self) {
        TICK_RUNNING.store(false, Ordering::SeqCst);
    }
}

async fn persist_refresh(
    state: &crate::server::state::AppState,
    connection_id: &str,
    result: &crate::oauth::token_refresh::RefreshResult,
) {
    let expires_at = result
        .expires_in
        .map(|secs| (chrono::Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339());
    let id = connection_id.to_string();
    let access = result.access_token.clone();
    let refresh = result.refresh_token.clone();
    let _ = state
        .db
        .update(move |db| {
            if let Some(conn) = db.provider_connections.iter_mut().find(|c| c.id == id) {
                conn.access_token = Some(access);
                if let Some(rt) = refresh {
                    conn.refresh_token = Some(rt);
                }
                conn.expires_at = expires_at.or_else(|| conn.expires_at.clone());
                conn.provider_specific_data.insert(
                    "lastRefreshAt".to_string(),
                    Value::String(chrono::Utc::now().to_rfc3339()),
                );
            }
        })
        .await;
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Operator kill switch (JS `backgroundTokenRefresh.js:150`) — a truthy
/// `DISABLE_BACKGROUND_TOKEN_REFRESH` keeps the scheduler from ever starting.
pub fn background_refresh_disabled() -> bool {
    std::env::var("DISABLE_BACKGROUND_TOKEN_REFRESH")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Spawn the scheduler loop (JS startBackgroundTokenRefresh): initial pass
/// after 10s, then every 5 minutes. Never returns.
pub fn spawn_background_token_refresh(state: std::sync::Arc<crate::server::state::AppState>) {
    if background_refresh_disabled() {
        tracing::info!(target: "openproxy::bg_token_refresh",
            "background token refresh disabled by DISABLE_BACKGROUND_TOKEN_REFRESH");
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(INITIAL_DELAY_SECS)).await;
        loop {
            run_tick(&state).await;
            tokio::time::sleep(std::time::Duration::from_secs(TICK_INTERVAL_SECS)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProviderConnection;
    use std::sync::Mutex;

    /// `std::env::set_var` is process-wide; the pacing tests read the same two
    /// keys, so they must not interleave with other threads' env access.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        old_value: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old_value = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, old_value }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = self.old_value.take() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn conn(
        provider: &str,
        auth_type: &str,
        refresh: Option<&str>,
        expires_in_secs: i64,
    ) -> ProviderConnection {
        ProviderConnection {
            provider: provider.to_string(),
            auth_type: auth_type.to_string(),
            refresh_token: refresh.map(String::from),
            expires_at: Some(
                (chrono::Utc::now() + chrono::Duration::seconds(expires_in_secs)).to_rfc3339(),
            ),
            ..Default::default()
        }
    }

    #[test]
    fn selects_oauth_conn_expiring_within_30min() {
        let conns = vec![
            conn("claude", "oauth", Some("rt"), 10 * 60), // 10 min → due
            conn("claude", "oauth", Some("rt"), 6 * 60 * 60), // 6 h → not due
            conn("claude", "apikey", Some("rt"), 10 * 60), // wrong auth type
            conn("claude", "oauth", None, 10 * 60),       // no refresh token
        ];
        let due = select_connections_needing_refresh(&conns, now_ms());
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].provider, "claude");
    }

    #[test]
    fn provider_lead_extends_window() {
        // codex lead is 5 days → a connection expiring in 4 days IS due.
        let conns = vec![conn("codex", "oauth", Some("rt"), 4 * 24 * 60 * 60)];
        assert_eq!(
            select_connections_needing_refresh(&conns, now_ms()).len(),
            1
        );
        // claude lead is 4h > the 30-min floor → max() wins; a connection
        // expiring in 2h IS due (window = max(lead, floor)).
        let conns = vec![conn("claude", "oauth", Some("rt"), 2 * 60 * 60)];
        assert_eq!(
            select_connections_needing_refresh(&conns, now_ms()).len(),
            1
        );
        // A connection expiring beyond claude's 4h lead is NOT due.
        let conns = vec![conn("claude", "oauth", Some("rt"), 6 * 60 * 60)];
        assert!(select_connections_needing_refresh(&conns, now_ms()).is_empty());
    }

    #[test]
    fn inactive_or_missing_expiry_skipped() {
        let mut inactive = conn("claude", "oauth", Some("rt"), 60);
        inactive.is_active = Some(false);
        let no_expiry = ProviderConnection {
            provider: "claude".into(),
            auth_type: "oauth".into(),
            refresh_token: Some("rt".into()),
            expires_at: None,
            ..Default::default()
        };
        let conns = vec![inactive, no_expiry];
        assert!(select_connections_needing_refresh(&conns, now_ms()).is_empty());
    }

    #[test]
    fn pacing_spaces_accounts_and_skips_the_tail() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // Three due accounts → two gaps, not three: the last refresh is not padded.
        let delays = paced_refresh_delays(&["claude", "claude", "claude"]);
        assert_eq!(delays.len(), 2);
        assert!(delays
            .iter()
            .all(|d| *d == Duration::from_millis(1_500 + 200)));

        // A single due account is never delayed.
        assert!(paced_refresh_delays(&["claude"]).is_empty());
        assert!(paced_refresh_delays(&[]).is_empty());
    }

    #[test]
    fn google_family_accounts_get_the_slow_paced_delay() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        for provider in SENSITIVE_PROVIDERS {
            // 12 s base + jitter in [0, 4000) — never a back-to-back burst.
            for _ in 0..32 {
                let delay = inter_account_delay_ms(provider);
                assert!(
                    (SENSITIVE_DELAY_MS..SENSITIVE_DELAY_MS + SENSITIVE_JITTER_MS).contains(&delay),
                    "{provider} delay {delay}ms outside the sensitive window"
                );
            }
        }
    }

    #[test]
    fn pacing_base_delay_is_operator_tunable() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        {
            let _normal = EnvVarGuard::set("BG_REFRESH_DELAY_MS", "42");
            let _google = EnvVarGuard::set("BG_REFRESH_GOOGLE_DELAY_MS", "99");
            assert_eq!(
                inter_account_delay_ms("claude"),
                42 + NORMAL_JITTER_MS,
                "normal providers read BG_REFRESH_DELAY_MS"
            );
            let sensitive = inter_account_delay_ms("gemini-cli");
            assert!(
                (99..99 + SENSITIVE_JITTER_MS).contains(&sensitive),
                "gemini-cli delay {sensitive}ms did not honour the override"
            );
        }

        // `Number(env) || default` in JS: 0 and garbage both fall back.
        for junk in ["0", "", "not-a-number"] {
            let _normal = EnvVarGuard::set("BG_REFRESH_DELAY_MS", junk);
            assert_eq!(
                inter_account_delay_ms("claude"),
                NORMAL_DELAY_MS + NORMAL_JITTER_MS,
                "junk value {junk:?} should fall back to the default base"
            );
        }
    }

    #[test]
    fn disable_kill_switch_honours_truthy_env() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        {
            let _disabled = EnvVarGuard::set("DISABLE_BACKGROUND_TOKEN_REFRESH", "1");
            assert!(background_refresh_disabled());
        }
        for truthy in ["true", "YES", " on "] {
            let _disabled = EnvVarGuard::set("DISABLE_BACKGROUND_TOKEN_REFRESH", truthy);
            assert!(
                background_refresh_disabled(),
                "{truthy:?} should disable the scheduler"
            );
        }
        for falsy in ["0", "false", ""] {
            let _enabled = EnvVarGuard::set("DISABLE_BACKGROUND_TOKEN_REFRESH", falsy);
            assert!(
                !background_refresh_disabled(),
                "{falsy:?} should leave the scheduler running"
            );
        }
    }
}
