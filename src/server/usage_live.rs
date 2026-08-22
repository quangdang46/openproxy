use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{Duration as ChronoDuration, Utc};
use serde::Serialize;
use tokio::sync::{broadcast, RwLock};
use tokio::time::{sleep, Duration};

/// 9router usageRepo.js:12 — a pending request that never completes is
/// force-zeroed after this many ms.
const PENDING_TIMEOUT_MS: u64 = 60 * 1000;

#[derive(Debug, Clone, Copy)]
pub enum UsageEvent {
    Pending,
    Update,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingSnapshot {
    pub by_model: BTreeMap<String, u64>,
    pub by_account: BTreeMap<String, BTreeMap<String, u64>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveRequest {
    pub model: String,
    pub provider: String,
    pub account: String,
    pub count: u64,
}

#[derive(Debug, Clone)]
struct ErrorProviderState {
    provider: String,
    recorded_at: chrono::DateTime<Utc>,
}

pub struct UsageLiveState {
    pending: RwLock<PendingSnapshot>,
    last_error_provider: RwLock<Option<ErrorProviderState>>,
    sender: broadcast::Sender<UsageEvent>,
    /// Per (connection_id, model_key) 60s force-zero timers (9router
    /// `pendingTimers`). Cancelled on finish.
    timers: Mutex<std::collections::HashMap<String, tokio::task::JoinHandle<()>>>,
}

impl Default for UsageLiveState {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageLiveState {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(128);
        Self {
            pending: RwLock::new(PendingSnapshot::default()),
            last_error_provider: RwLock::new(None),
            sender,
            timers: Mutex::new(std::collections::HashMap::new()),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<UsageEvent> {
        self.sender.subscribe()
    }

    pub fn notify_update(&self) {
        let _ = self.sender.send(UsageEvent::Update);
    }

    /// 9router usageRepo.js trackPendingRequest parity — increment counters and
    /// arm a per-(connection, model) 60s force-zero timer.
    pub async fn start_request(
        self: &Arc<Self>,
        model: &str,
        provider: &str,
        connection_id: Option<&str>,
    ) {
        let model_key = model_key(model, provider);
        {
            let mut pending = self.pending.write().await;
            *pending.by_model.entry(model_key.clone()).or_insert(0) += 1;
            if let Some(connection_id) = connection_id {
                let account = pending
                    .by_account
                    .entry(connection_id.to_string())
                    .or_default();
                *account.entry(model_key.clone()).or_insert(0) += 1;
            }
        }
        let _ = self.sender.send(UsageEvent::Pending);

        self.arm_pending_timer(model_key, connection_id);
    }

    /// Cancel the pending force-zero timer for this request's key
    /// (JS `clearTimeout` on non-started).
    fn cancel_pending_timer(&self, model_key: &str, connection_id: Option<&str>) {
        let key = pending_key(connection_id, model_key);
        if let Ok(mut timers) = self.timers.lock() {
            if let Some(handle) = timers.remove(&key) {
                handle.abort();
            }
        }
    }

    /// Arm a 60s force-zero timer for `(connection_id, model_key)`.
    fn arm_pending_timer(self: &Arc<Self>, model_key: String, connection_id: Option<&str>) {
        let key = pending_key(connection_id, &model_key);
        self.cancel_pending_timer(&model_key, connection_id);
        let state = self.clone();
        let key_for_task = key.clone();
        let handle = tokio::spawn(async move {
            sleep(Duration::from_millis(PENDING_TIMEOUT_MS)).await;
            state.force_zero(&key_for_task).await;
        });
        if let Ok(mut timers) = self.timers.lock() {
            timers.insert(key, handle);
        }
    }

    /// Force-zero the by_model/by_account counters for a stuck request after
    /// its 60s timeout (JS setTimeout → `= 0` assignment, not decrement).
    async fn force_zero(&self, key: &str) {
        let (connection_id, model_key) = split_pending_key(key);
        {
            let mut pending = self.pending.write().await;
            if pending.by_model.get(&model_key).is_some_and(|c| *c > 0) {
                pending.by_model.insert(model_key.clone(), 0);
            }
            if !connection_id.is_empty() {
                if let Some(account) = pending.by_account.get_mut(&connection_id) {
                    if account.get(&model_key).is_some_and(|c| *c > 0) {
                        account.insert(model_key.clone(), 0);
                    }
                }
            }
        }
        let _ = self.sender.send(UsageEvent::Pending);
    }

    /// 9router usageRepo.js trackPendingRequest parity — decrement counters and
    /// cancel the pending force-zero timer.
    pub async fn finish_request(
        self: &Arc<Self>,
        model: &str,
        provider: &str,
        connection_id: Option<&str>,
        error: bool,
    ) {
        let model_key = model_key(model, provider);
        {
            let mut pending = self.pending.write().await;
            decrement_map(&mut pending.by_model, &model_key);
            if let Some(connection_id) = connection_id {
                if let Some(account) = pending.by_account.get_mut(connection_id) {
                    decrement_map(account, &model_key);
                    if account.is_empty() {
                        pending.by_account.remove(connection_id);
                    }
                }
            }
        }
        self.cancel_pending_timer(&model_key, connection_id);

        if error {
            let mut last_error_provider = self.last_error_provider.write().await;
            *last_error_provider = Some(ErrorProviderState {
                provider: provider.to_ascii_lowercase(),
                recorded_at: Utc::now(),
            });
        }

        let _ = self.sender.send(UsageEvent::Pending);
    }

    pub async fn pending_snapshot(&self) -> PendingSnapshot {
        self.pending.read().await.clone()
    }

    pub async fn active_requests(
        &self,
        connection_names: &BTreeMap<String, String>,
    ) -> Vec<ActiveRequest> {
        let pending = self.pending.read().await;
        let mut active = Vec::new();
        for (connection_id, models) in &pending.by_account {
            for (model_key, count) in models {
                if *count == 0 {
                    continue;
                }
                let account = connection_names
                    .get(connection_id)
                    .cloned()
                    .unwrap_or_else(|| {
                        format!(
                            "Account {}...",
                            connection_id.chars().take(8).collect::<String>()
                        )
                    });
                let (model, provider) = split_model_key(model_key);
                active.push(ActiveRequest {
                    model,
                    provider,
                    account,
                    count: *count,
                });
            }
        }
        active
    }

    pub async fn error_provider(&self) -> String {
        let mut last_error_provider = self.last_error_provider.write().await;
        match last_error_provider.as_ref() {
            Some(state) if Utc::now() - state.recorded_at < ChronoDuration::seconds(10) => {
                state.provider.clone()
            }
            Some(_) => {
                *last_error_provider = None;
                String::new()
            }
            None => String::new(),
        }
    }
}

/// Timer key = `${connectionId}|${modelKey}` (JS usageRepo.js:165).
fn pending_key(connection_id: Option<&str>, model_key: &str) -> String {
    format!("{}|{model_key}", connection_id.unwrap_or(""))
}

/// Split a timer key back into (connection_id, model_key).
fn split_pending_key(key: &str) -> (String, String) {
    match key.split_once('|') {
        Some((cid, m)) => (cid.to_string(), m.to_string()),
        None => (String::new(), key.to_string()),
    }
}

fn decrement_map(map: &mut BTreeMap<String, u64>, key: &str) {
    if let Some(count) = map.get_mut(key) {
        if *count > 1 {
            *count -= 1;
        } else {
            map.remove(key);
        }
    }
}

fn model_key(model: &str, provider: &str) -> String {
    if provider.trim().is_empty() {
        model.to_string()
    } else {
        format!("{model} ({provider})")
    }
}

fn split_model_key(model_key: &str) -> (String, String) {
    if let Some((model, provider)) = model_key.rsplit_once(" (") {
        if let Some(provider) = provider.strip_suffix(')') {
            return (model.to_string(), provider.to_string());
        }
    }
    (model_key.to_string(), "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast;

    fn state() -> Arc<UsageLiveState> {
        Arc::new(UsageLiveState::new())
    }

    #[tokio::test]
    async fn force_zero_clears_stuck_pending_counts() {
        let st = state();
        let mut rx = st.subscribe();
        st.start_request("gpt-4", "openai", Some("c1")).await;
        st.start_request("gpt-4", "openai", Some("c1")).await;
        // Both increments land in by_model/by_account.
        let snap = st.pending_snapshot().await;
        assert_eq!(snap.by_model.get("gpt-4 (openai)"), Some(&2));
        assert_eq!(snap.by_account["c1"].get("gpt-4 (openai)"), Some(&2));
        // Simulate the 60s timeout firing → force zero (no finish_request).
        st.force_zero(&pending_key(Some("c1"), "gpt-4 (openai)"))
            .await;
        let snap = st.pending_snapshot().await;
        assert_eq!(
            snap.by_model.get("gpt-4 (openai)"),
            Some(&0),
            "by_model zeroed"
        );
        assert_eq!(
            snap.by_account["c1"].get("gpt-4 (openai)"),
            Some(&0),
            "by_account zeroed"
        );
        // A Pending event was broadcast after force-zero.
        assert!(rx.recv().await.is_ok(), "Pending event after force-zero");
    }

    #[tokio::test]
    async fn force_zero_does_not_go_negative_or_touch_other_keys() {
        let st = state();
        st.start_request("gpt-4", "openai", Some("c1")).await;
        st.start_request("gpt-5", "openai", Some("c1")).await;
        st.force_zero(&pending_key(Some("c1"), "gpt-4 (openai)"))
            .await;
        let snap = st.pending_snapshot().await;
        // gpt-4 zeroed, gpt-5 untouched.
        assert_eq!(snap.by_model.get("gpt-4 (openai)"), Some(&0));
        assert_eq!(snap.by_model.get("gpt-5 (openai)"), Some(&1));
    }

    #[tokio::test]
    async fn finish_request_cancels_timer() {
        let st = state();
        let key = pending_key(Some("c1"), "gpt-4 (openai)");
        st.start_request("gpt-4", "openai", Some("c1")).await;
        // Timer armed.
        assert!(st.timers.lock().unwrap().contains_key(&key));
        st.finish_request("gpt-4", "openai", Some("c1"), false)
            .await;
        // Timer cancelled + removed on finish.
        assert!(!st.timers.lock().unwrap().contains_key(&key));
        let snap = st.pending_snapshot().await;
        assert!(
            snap.by_model.get("gpt-4 (openai)").is_none(),
            "count removed on finish"
        );
    }
}
