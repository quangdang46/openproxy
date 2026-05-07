use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Serialize;

use crate::types::{ApiKey, ProviderConnection, ProviderNode, SummaryCounter, UsageDb, UsageEntry};

use super::usage_live::{ActiveRequest, PendingSnapshot};

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UsageStatsPayload {
    pub total_requests: u64,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_cost: f64,
    pub by_provider: BTreeMap<String, AggregateStats>,
    pub by_model: BTreeMap<String, ModelStats>,
    pub by_account: BTreeMap<String, AccountStats>,
    pub by_api_key: BTreeMap<String, ApiKeyStats>,
    pub by_endpoint: BTreeMap<String, EndpointStats>,
    pub last10_minutes: Vec<LastTenMinutesBucket>,
    pub pending: PendingSnapshot,
    pub active_requests: Vec<ActiveRequest>,
    pub recent_requests: Vec<RecentRequest>,
    pub error_provider: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UsageStreamPatch {
    pub active_requests: Vec<ActiveRequest>,
    pub recent_requests: Vec<RecentRequest>,
    pub error_provider: String,
    pub pending: PendingSnapshot,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AggregateStats {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelStats {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost: f64,
    pub raw_model: String,
    pub provider: String,
    pub last_used: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AccountStats {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost: f64,
    pub raw_model: String,
    pub provider: String,
    pub connection_id: String,
    pub account_name: String,
    pub last_used: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyStats {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost: f64,
    pub raw_model: String,
    pub provider: String,
    pub api_key: Option<String>,
    pub key_name: String,
    pub api_key_key: String,
    pub last_used: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EndpointStats {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost: f64,
    pub endpoint: String,
    pub raw_model: String,
    pub provider: String,
    pub last_used: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LastTenMinutesBucket {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RecentRequest {
    pub timestamp: String,
    pub model: String,
    pub provider: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub status: String,
}

#[derive(Debug, Clone, Copy)]
pub enum UsagePeriod {
    Last24Hours,
    Last7Days,
    Last30Days,
    Last60Days,
    All,
}

impl UsagePeriod {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "24h" => Some(Self::Last24Hours),
            "7d" => Some(Self::Last7Days),
            "30d" => Some(Self::Last30Days),
            "60d" => Some(Self::Last60Days),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

pub fn build_recent_requests(history: &[UsageEntry]) -> Vec<RecentRequest> {
    let mut items: Vec<_> = history
        .iter()
        .filter_map(|entry| {
            let timestamp = entry.timestamp.clone()?;
            let tokens = entry.tokens.as_ref()?;
            let prompt_tokens = tokens.prompt_tokens.or(tokens.input_tokens).unwrap_or(0);
            let completion_tokens = tokens
                .completion_tokens
                .or(tokens.output_tokens)
                .unwrap_or(0);
            if prompt_tokens == 0 && completion_tokens == 0 {
                return None;
            }
            Some(RecentRequest {
                timestamp,
                model: entry.model.clone(),
                provider: entry.provider.clone().unwrap_or_default(),
                prompt_tokens,
                completion_tokens,
                status: entry.status.clone().unwrap_or_else(|| "ok".to_string()),
            })
        })
        .collect();

    items.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    let mut seen = HashSet::new();
    items.retain(|entry| {
        let minute = entry.timestamp.chars().take(16).collect::<String>();
        let key = format!(
            "{}|{}|{}|{}|{}",
            entry.model, entry.provider, entry.prompt_tokens, entry.completion_tokens, minute
        );
        seen.insert(key)
    });
    items.truncate(20);
    items
}

pub fn build_usage_stats(
    period: UsagePeriod,
    usage_db: &UsageDb,
    connections: &[ProviderConnection],
    provider_nodes: &[ProviderNode],
    api_keys: &[ApiKey],
    pending: PendingSnapshot,
    active_requests: Vec<ActiveRequest>,
    error_provider: String,
) -> UsageStatsPayload {
    let connection_names = build_connection_map(connections);
    let provider_names = build_provider_map(provider_nodes);
    let api_key_names = build_api_key_map(api_keys);
    let recent_requests = build_recent_requests(&usage_db.history);

    let mut stats = UsageStatsPayload {
        total_requests: usage_db.total_requests_lifetime,
        total_prompt_tokens: 0,
        total_completion_tokens: 0,
        total_cost: 0.0,
        by_provider: BTreeMap::new(),
        by_model: BTreeMap::new(),
        by_account: BTreeMap::new(),
        by_api_key: BTreeMap::new(),
        by_endpoint: BTreeMap::new(),
        last10_minutes: build_last_ten_minutes(&usage_db.history),
        pending,
        active_requests,
        recent_requests,
        error_provider,
    };

    match period {
        UsagePeriod::Last24Hours => {
            let cutoff = Utc::now() - ChronoDuration::hours(24);
            for entry in usage_db.history.iter().filter(|entry| {
                entry
                    .timestamp
                    .as_deref()
                    .and_then(parse_timestamp)
                    .is_some_and(|ts| ts >= cutoff)
            }) {
                aggregate_live_entry(
                    &mut stats,
                    entry,
                    &connection_names,
                    &provider_names,
                    &api_key_names,
                );
            }
        }
        UsagePeriod::Last7Days
        | UsagePeriod::Last30Days
        | UsagePeriod::Last60Days
        | UsagePeriod::All => {
            let max_days = match period {
                UsagePeriod::Last7Days => Some(7),
                UsagePeriod::Last30Days => Some(30),
                UsagePeriod::Last60Days => Some(60),
                UsagePeriod::All => None,
                UsagePeriod::Last24Hours => unreachable!(),
            };
            let today = Utc::now().date_naive();
            for (date_key, day) in &usage_db.daily_summary {
                if let Some(max_days) = max_days {
                    if let Ok(date) = chrono::NaiveDate::parse_from_str(date_key, "%Y-%m-%d") {
                        let diff_days = today.signed_duration_since(date).num_days();
                        if diff_days >= max_days {
                            continue;
                        }
                    }
                }

                stats.total_prompt_tokens += day.prompt_tokens;
                stats.total_completion_tokens += day.completion_tokens;
                stats.total_cost += day.cost;

                merge_by_provider(&mut stats.by_provider, &day.by_provider);
                merge_by_model(
                    &mut stats.by_model,
                    &day.by_model,
                    date_key,
                    &provider_names,
                );
                merge_by_account(
                    &mut stats.by_account,
                    &day.by_account,
                    date_key,
                    &connection_names,
                    &provider_names,
                );
                merge_by_api_key(
                    &mut stats.by_api_key,
                    &day.by_api_key,
                    date_key,
                    &provider_names,
                    &api_key_names,
                );
                merge_by_endpoint(
                    &mut stats.by_endpoint,
                    &day.by_endpoint,
                    date_key,
                    &provider_names,
                );
            }

            overlay_precise_last_used(&mut stats, &usage_db.history, max_days, &connection_names);
        }
    }

    stats
}

fn build_connection_map(connections: &[ProviderConnection]) -> BTreeMap<String, String> {
    connections
        .iter()
        .map(|connection| {
            let name = connection
                .name
                .clone()
                .or_else(|| connection.email.clone())
                .unwrap_or_else(|| connection.id.clone());
            (connection.id.clone(), name)
        })
        .collect()
}

fn build_provider_map(provider_nodes: &[ProviderNode]) -> BTreeMap<String, String> {
    provider_nodes
        .iter()
        .map(|node| (node.id.clone(), node.name.clone()))
        .collect()
}

fn build_api_key_map(api_keys: &[ApiKey]) -> BTreeMap<String, String> {
    api_keys
        .iter()
        .map(|key| (key.key.clone(), key.name.clone()))
        .collect()
}

fn aggregate_live_entry(
    stats: &mut UsageStatsPayload,
    entry: &UsageEntry,
    connection_names: &BTreeMap<String, String>,
    provider_names: &BTreeMap<String, String>,
    api_key_names: &BTreeMap<String, String>,
) {
    let prompt_tokens = entry
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.prompt_tokens.or(tokens.input_tokens))
        .unwrap_or(0);
    let completion_tokens = entry
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.completion_tokens.or(tokens.output_tokens))
        .unwrap_or(0);
    let cost = entry.cost.unwrap_or(0.0);
    let provider = entry.provider.clone().unwrap_or_default();
    let provider_display = provider_names
        .get(&provider)
        .cloned()
        .unwrap_or_else(|| provider.clone());
    let timestamp = entry.timestamp.clone().unwrap_or_default();

    stats.total_prompt_tokens += prompt_tokens;
    stats.total_completion_tokens += completion_tokens;
    stats.total_cost += cost;

    let provider_bucket = stats.by_provider.entry(provider.clone()).or_default();
    provider_bucket.requests += 1;
    provider_bucket.prompt_tokens += prompt_tokens;
    provider_bucket.completion_tokens += completion_tokens;
    provider_bucket.cost += cost;

    let model_key = if provider.is_empty() {
        entry.model.clone()
    } else {
        format!("{} ({})", entry.model, provider)
    };
    let model_bucket = stats.by_model.entry(model_key).or_default();
    model_bucket.requests += 1;
    model_bucket.prompt_tokens += prompt_tokens;
    model_bucket.completion_tokens += completion_tokens;
    model_bucket.cost += cost;
    model_bucket.raw_model = entry.model.clone();
    model_bucket.provider = provider_display.clone();
    update_last_used(&mut model_bucket.last_used, &timestamp);

    if let Some(connection_id) = &entry.connection_id {
        let account_name = connection_names
            .get(connection_id)
            .cloned()
            .unwrap_or_else(|| {
                format!(
                    "Account {}...",
                    connection_id.chars().take(8).collect::<String>()
                )
            });
        let account_key = format!("{} ({} - {})", entry.model, provider, account_name);
        let account_bucket = stats.by_account.entry(account_key).or_default();
        account_bucket.requests += 1;
        account_bucket.prompt_tokens += prompt_tokens;
        account_bucket.completion_tokens += completion_tokens;
        account_bucket.cost += cost;
        account_bucket.raw_model = entry.model.clone();
        account_bucket.provider = provider_display.clone();
        account_bucket.connection_id = connection_id.clone();
        account_bucket.account_name = account_name;
        update_last_used(&mut account_bucket.last_used, &timestamp);
    }

    let api_key_value = entry.api_key.clone();
    let api_key_group = if let Some(api_key) = &api_key_value {
        format!(
            "{}|{}|{}",
            api_key,
            entry.model,
            if provider.is_empty() {
                "unknown"
            } else {
                &provider
            }
        )
    } else {
        "local-no-key".to_string()
    };
    let api_key_bucket = stats.by_api_key.entry(api_key_group).or_default();
    api_key_bucket.requests += 1;
    api_key_bucket.prompt_tokens += prompt_tokens;
    api_key_bucket.completion_tokens += completion_tokens;
    api_key_bucket.cost += cost;
    api_key_bucket.raw_model = entry.model.clone();
    api_key_bucket.provider = provider_display.clone();
    api_key_bucket.api_key = api_key_value.clone();
    api_key_bucket.key_name = api_key_value
        .as_ref()
        .and_then(|key| {
            api_key_names
                .get(key)
                .cloned()
                .or_else(|| Some(format!("{}...", key.chars().take(8).collect::<String>())))
        })
        .unwrap_or_else(|| "Local (No API Key)".to_string());
    api_key_bucket.api_key_key = api_key_value.unwrap_or_else(|| "local-no-key".to_string());
    update_last_used(&mut api_key_bucket.last_used, &timestamp);

    let endpoint = entry
        .endpoint
        .clone()
        .unwrap_or_else(|| "Unknown".to_string());
    let endpoint_key = format!(
        "{}|{}|{}",
        endpoint,
        entry.model,
        if provider.is_empty() {
            "unknown"
        } else {
            &provider
        }
    );
    let endpoint_bucket = stats.by_endpoint.entry(endpoint_key).or_default();
    endpoint_bucket.requests += 1;
    endpoint_bucket.prompt_tokens += prompt_tokens;
    endpoint_bucket.completion_tokens += completion_tokens;
    endpoint_bucket.cost += cost;
    endpoint_bucket.endpoint = endpoint;
    endpoint_bucket.raw_model = entry.model.clone();
    endpoint_bucket.provider = provider_display;
    update_last_used(&mut endpoint_bucket.last_used, &timestamp);
}

fn merge_by_provider(
    target: &mut BTreeMap<String, AggregateStats>,
    by_provider: &BTreeMap<String, SummaryCounter>,
) {
    for (provider, counter) in by_provider {
        let bucket = target.entry(provider.clone()).or_default();
        bucket.requests += counter.requests;
        bucket.prompt_tokens += counter.prompt_tokens;
        bucket.completion_tokens += counter.completion_tokens;
        bucket.cost += counter.cost;
    }
}

fn merge_by_model(
    target: &mut BTreeMap<String, ModelStats>,
    by_model: &BTreeMap<String, SummaryCounter>,
    last_used: &str,
    provider_names: &BTreeMap<String, String>,
) {
    for (key, counter) in by_model {
        let raw_model = counter
            .raw_model
            .clone()
            .unwrap_or_else(|| key.split('|').next().unwrap_or_default().to_string());
        let provider = counter
            .provider
            .clone()
            .unwrap_or_else(|| key.split('|').nth(1).unwrap_or_default().to_string());
        let provider_display = provider_names
            .get(&provider)
            .cloned()
            .unwrap_or(provider.clone());
        let stats_key = if provider.is_empty() {
            raw_model.clone()
        } else {
            format!("{} ({})", raw_model, provider)
        };
        let bucket = target.entry(stats_key).or_default();
        bucket.requests += counter.requests;
        bucket.prompt_tokens += counter.prompt_tokens;
        bucket.completion_tokens += counter.completion_tokens;
        bucket.cost += counter.cost;
        bucket.raw_model = raw_model;
        bucket.provider = provider_display;
        update_last_used(&mut bucket.last_used, last_used);
    }
}

fn merge_by_account(
    target: &mut BTreeMap<String, AccountStats>,
    by_account: &BTreeMap<String, SummaryCounter>,
    last_used: &str,
    connection_names: &BTreeMap<String, String>,
    provider_names: &BTreeMap<String, String>,
) {
    for (connection_id, counter) in by_account {
        let raw_model = counter.raw_model.clone().unwrap_or_default();
        let provider = counter.provider.clone().unwrap_or_default();
        let provider_display = provider_names
            .get(&provider)
            .cloned()
            .unwrap_or(provider.clone());
        let account_name = connection_names
            .get(connection_id)
            .cloned()
            .unwrap_or_else(|| {
                format!(
                    "Account {}...",
                    connection_id.chars().take(8).collect::<String>()
                )
            });
        let stats_key = format!("{} ({} - {})", raw_model, provider, account_name);
        let bucket = target.entry(stats_key).or_default();
        bucket.requests += counter.requests;
        bucket.prompt_tokens += counter.prompt_tokens;
        bucket.completion_tokens += counter.completion_tokens;
        bucket.cost += counter.cost;
        bucket.raw_model = raw_model;
        bucket.provider = provider_display;
        bucket.connection_id = connection_id.clone();
        bucket.account_name = account_name;
        update_last_used(&mut bucket.last_used, last_used);
    }
}

fn merge_by_api_key(
    target: &mut BTreeMap<String, ApiKeyStats>,
    by_api_key: &BTreeMap<String, SummaryCounter>,
    last_used: &str,
    provider_names: &BTreeMap<String, String>,
    api_key_names: &BTreeMap<String, String>,
) {
    for (api_key_group, counter) in by_api_key {
        let raw_model = counter.raw_model.clone().unwrap_or_default();
        let provider = counter.provider.clone().unwrap_or_default();
        let provider_display = provider_names
            .get(&provider)
            .cloned()
            .unwrap_or(provider.clone());
        let api_key = counter.api_key.clone();
        let key_name = api_key
            .as_ref()
            .and_then(|key| {
                api_key_names
                    .get(key)
                    .cloned()
                    .or_else(|| Some(format!("{}...", key.chars().take(8).collect::<String>())))
            })
            .unwrap_or_else(|| "Local (No API Key)".to_string());
        let bucket = target.entry(api_key_group.clone()).or_default();
        bucket.requests += counter.requests;
        bucket.prompt_tokens += counter.prompt_tokens;
        bucket.completion_tokens += counter.completion_tokens;
        bucket.cost += counter.cost;
        bucket.raw_model = raw_model;
        bucket.provider = provider_display;
        bucket.api_key = api_key.clone();
        bucket.key_name = key_name;
        bucket.api_key_key = api_key.unwrap_or_else(|| "local-no-key".to_string());
        update_last_used(&mut bucket.last_used, last_used);
    }
}

fn merge_by_endpoint(
    target: &mut BTreeMap<String, EndpointStats>,
    by_endpoint: &BTreeMap<String, SummaryCounter>,
    last_used: &str,
    provider_names: &BTreeMap<String, String>,
) {
    for (endpoint_group, counter) in by_endpoint {
        let raw_model = counter.raw_model.clone().unwrap_or_default();
        let provider = counter.provider.clone().unwrap_or_default();
        let provider_display = provider_names
            .get(&provider)
            .cloned()
            .unwrap_or(provider.clone());
        let endpoint = counter.endpoint.clone().unwrap_or_else(|| {
            endpoint_group
                .split('|')
                .next()
                .unwrap_or("Unknown")
                .to_string()
        });
        let bucket = target.entry(endpoint_group.clone()).or_default();
        bucket.requests += counter.requests;
        bucket.prompt_tokens += counter.prompt_tokens;
        bucket.completion_tokens += counter.completion_tokens;
        bucket.cost += counter.cost;
        bucket.endpoint = endpoint;
        bucket.raw_model = raw_model;
        bucket.provider = provider_display;
        update_last_used(&mut bucket.last_used, last_used);
    }
}

fn overlay_precise_last_used(
    stats: &mut UsageStatsPayload,
    history: &[UsageEntry],
    max_days: Option<i64>,
    connection_names: &BTreeMap<String, String>,
) {
    let cutoff = max_days.map(|days| Utc::now() - ChronoDuration::days(days));
    for entry in history {
        let Some(timestamp) = entry.timestamp.as_ref() else {
            continue;
        };
        if let Some(cutoff) = cutoff {
            if parse_timestamp(timestamp).is_some_and(|ts| ts < cutoff) {
                continue;
            }
        }

        let provider = entry.provider.clone().unwrap_or_default();
        let model_key = if provider.is_empty() {
            entry.model.clone()
        } else {
            format!("{} ({})", entry.model, provider)
        };
        if let Some(model_bucket) = stats.by_model.get_mut(&model_key) {
            update_last_used(&mut model_bucket.last_used, timestamp);
        }

        if let Some(connection_id) = &entry.connection_id {
            let account_name = connection_names
                .get(connection_id)
                .cloned()
                .unwrap_or_else(|| {
                    format!(
                        "Account {}...",
                        connection_id.chars().take(8).collect::<String>()
                    )
                });
            let account_key = format!("{} ({} - {})", entry.model, provider, account_name);
            if let Some(account_bucket) = stats.by_account.get_mut(&account_key) {
                update_last_used(&mut account_bucket.last_used, timestamp);
            }
        }

        let api_key_group = if let Some(api_key) = &entry.api_key {
            format!(
                "{}|{}|{}",
                api_key,
                entry.model,
                if provider.is_empty() {
                    "unknown"
                } else {
                    &provider
                }
            )
        } else {
            "local-no-key".to_string()
        };
        if let Some(api_key_bucket) = stats.by_api_key.get_mut(&api_key_group) {
            update_last_used(&mut api_key_bucket.last_used, timestamp);
        }

        let endpoint = entry
            .endpoint
            .clone()
            .unwrap_or_else(|| "Unknown".to_string());
        let endpoint_key = format!(
            "{}|{}|{}",
            endpoint,
            entry.model,
            if provider.is_empty() {
                "unknown"
            } else {
                &provider
            }
        );
        if let Some(endpoint_bucket) = stats.by_endpoint.get_mut(&endpoint_key) {
            update_last_used(&mut endpoint_bucket.last_used, timestamp);
        }
    }
}

fn build_last_ten_minutes(history: &[UsageEntry]) -> Vec<LastTenMinutesBucket> {
    let now = Utc::now();
    let current_minute_start = now - ChronoDuration::seconds(now.timestamp() % 60);
    let start = current_minute_start - ChronoDuration::minutes(9);
    let mut buckets = vec![LastTenMinutesBucket::default(); 10];

    for entry in history {
        let Some(timestamp) = entry.timestamp.as_deref().and_then(parse_timestamp) else {
            continue;
        };
        if timestamp < start || timestamp > now {
            continue;
        }
        let offset = (timestamp - start).num_minutes();
        if !(0..10).contains(&offset) {
            continue;
        }
        let bucket = &mut buckets[offset as usize];
        let prompt_tokens = entry
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.prompt_tokens.or(tokens.input_tokens))
            .unwrap_or(0);
        let completion_tokens = entry
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.completion_tokens.or(tokens.output_tokens))
            .unwrap_or(0);
        bucket.requests += 1;
        bucket.prompt_tokens += prompt_tokens;
        bucket.completion_tokens += completion_tokens;
        bucket.cost += entry.cost.unwrap_or(0.0);
    }

    buckets
}

fn update_last_used(current: &mut String, candidate: &str) {
    if current.is_empty() || candidate > current.as_str() {
        *current = candidate.to_string();
    }
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}
