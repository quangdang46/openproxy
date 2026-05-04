use std::collections::BTreeMap;

use chrono::{Duration as ChronoDuration, Utc};
use serde::Serialize;
use tokio::sync::{broadcast, RwLock};

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
}

impl UsageLiveState {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(128);
        Self {
            pending: RwLock::new(PendingSnapshot::default()),
            last_error_provider: RwLock::new(None),
            sender,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<UsageEvent> {
        self.sender.subscribe()
    }

    pub fn notify_update(&self) {
        let _ = self.sender.send(UsageEvent::Update);
    }

    pub async fn start_request(&self, model: &str, provider: &str, connection_id: Option<&str>) {
        let model_key = model_key(model, provider);
        {
            let mut pending = self.pending.write().await;
            *pending.by_model.entry(model_key.clone()).or_insert(0) += 1;
            if let Some(connection_id) = connection_id {
                let account = pending
                    .by_account
                    .entry(connection_id.to_string())
                    .or_default();
                *account.entry(model_key).or_insert(0) += 1;
            }
        }
        let _ = self.sender.send(UsageEvent::Pending);
    }

    pub async fn finish_request(
        &self,
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
                let account = connection_names.get(connection_id).cloned().unwrap_or_else(|| {
                    format!("Account {}...", connection_id.chars().take(8).collect::<String>())
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
