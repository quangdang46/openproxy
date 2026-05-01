use std::collections::BTreeMap;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

pub const DEFAULT_MITM_ROUTER_BASE: &str = "http://localhost:20128";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppDb {
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub provider_connections: Vec<ProviderConnection>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub provider_nodes: Vec<ProviderNode>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub proxy_pools: Vec<ProxyPool>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub model_aliases: BTreeMap<String, ModelAliasTarget>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub custom_models: Vec<CustomModel>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub mitm_alias: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub combos: Vec<Combo>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub api_keys: Vec<ApiKey>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub settings: Settings,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub pricing: PricingTable,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl AppDb {
    pub fn normalize(&mut self) {
        self.settings.normalize();

        for api_key in &mut self.api_keys {
            if api_key.is_active.is_none() {
                api_key.is_active = Some(true);
            }
        }
    }

    pub fn from_json_value(value: Value) -> Self {
        let Value::Object(mut fields) = value else {
            return Self::default();
        };

        let mut db = Self {
            provider_connections: extract_named_field(&mut fields, "providerConnections"),
            provider_nodes: extract_named_field(&mut fields, "providerNodes"),
            proxy_pools: extract_named_field(&mut fields, "proxyPools"),
            model_aliases: extract_named_field(&mut fields, "modelAliases"),
            custom_models: extract_named_field(&mut fields, "customModels"),
            mitm_alias: extract_named_field(&mut fields, "mitmAlias"),
            combos: extract_named_field(&mut fields, "combos"),
            api_keys: extract_named_field(&mut fields, "apiKeys"),
            settings: extract_named_field(&mut fields, "settings"),
            pricing: extract_named_field(&mut fields, "pricing"),
            extra: fields.into_iter().collect(),
        };
        db.normalize();
        db
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConnection {
    pub id: String,
    pub provider: String,
    #[serde(default = "default_auth_type")]
    pub auth_type: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub priority: Option<u32>,
    #[serde(default)]
    pub is_active: Option<bool>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub global_priority: Option<u32>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub test_status: Option<String>,
    #[serde(default)]
    pub last_tested: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_error_at: Option<String>,
    #[serde(default)]
    pub rate_limited_until: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub consecutive_use_count: Option<u32>,
    #[serde(default)]
    pub backoff_level: Option<u32>,
    #[serde(default)]
    pub consecutive_errors: Option<u32>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub provider_specific_data: BTreeMap<String, Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ProviderConnection {
    pub fn is_active(&self) -> bool {
        self.is_active.unwrap_or(true)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNode {
    pub id: String,
    pub r#type: String,
    pub name: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub api_type: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProxyPool {
    pub id: String,
    pub name: String,
    pub proxy_url: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub no_proxy: String,
    #[serde(
        default = "default_proxy_type",
        deserialize_with = "deserialize_null_default"
    )]
    pub r#type: String,
    #[serde(default)]
    pub is_active: Option<bool>,
    #[serde(default)]
    pub strict_proxy: Option<bool>,
    #[serde(default)]
    pub test_status: Option<String>,
    #[serde(default)]
    pub last_tested_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CustomModel {
    pub provider_alias: String,
    pub id: String,
    #[serde(
        default = "default_model_type",
        deserialize_with = "deserialize_null_default"
    )]
    pub r#type: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Combo {
    pub id: String,
    pub name: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub models: Vec<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    pub key: String,
    #[serde(default)]
    pub machine_id: Option<String>,
    #[serde(default)]
    pub is_active: Option<bool>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ApiKey {
    pub fn is_active(&self) -> bool {
        self.is_active.unwrap_or(true)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub cloud_enabled: bool,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub cloud_url: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub tunnel_enabled: bool,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub tunnel_url: String,
    #[serde(
        default = "default_tunnel_provider",
        deserialize_with = "deserialize_null_default"
    )]
    pub tunnel_provider: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub tailscale_enabled: bool,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub tailscale_url: String,
    #[serde(
        default = "default_sticky_round_robin_limit",
        deserialize_with = "deserialize_null_default"
    )]
    pub sticky_round_robin_limit: u32,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub provider_strategies: BTreeMap<String, String>,
    #[serde(
        default = "default_combo_strategy",
        deserialize_with = "deserialize_null_default"
    )]
    pub combo_strategy: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub combo_strategies: BTreeMap<String, String>,
    #[serde(
        default = "default_true",
        deserialize_with = "deserialize_null_default"
    )]
    pub require_login: bool,
    #[serde(
        default = "default_true",
        deserialize_with = "deserialize_null_default"
    )]
    pub tunnel_dashboard_access: bool,
    #[serde(
        default = "default_true",
        deserialize_with = "deserialize_null_default"
    )]
    pub observability_enabled: bool,
    #[serde(
        default = "default_observability_max_records",
        deserialize_with = "deserialize_null_default"
    )]
    pub observability_max_records: u32,
    #[serde(
        default = "default_observability_batch_size",
        deserialize_with = "deserialize_null_default"
    )]
    pub observability_batch_size: u32,
    #[serde(
        default = "default_observability_flush_interval_ms",
        deserialize_with = "deserialize_null_default"
    )]
    pub observability_flush_interval_ms: u32,
    #[serde(
        default = "default_observability_max_json_size",
        deserialize_with = "deserialize_null_default"
    )]
    pub observability_max_json_size: u32,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub outbound_proxy_enabled: bool,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub outbound_proxy_url: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub outbound_no_proxy: String,
    #[serde(
        default = "default_mitm_router_base_url",
        deserialize_with = "deserialize_null_default"
    )]
    pub mitm_router_base_url: String,
    #[serde(
        default = "default_true",
        deserialize_with = "deserialize_null_default"
    )]
    pub rtk_enabled: bool,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub caveman_enabled: bool,
    #[serde(
        default = "default_caveman_level",
        deserialize_with = "deserialize_null_default"
    )]
    pub caveman_level: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cloud_enabled: false,
            cloud_url: String::new(),
            tunnel_enabled: false,
            tunnel_url: String::new(),
            tunnel_provider: default_tunnel_provider(),
            tailscale_enabled: false,
            tailscale_url: String::new(),
            sticky_round_robin_limit: default_sticky_round_robin_limit(),
            provider_strategies: BTreeMap::new(),
            combo_strategy: default_combo_strategy(),
            combo_strategies: BTreeMap::new(),
            require_login: true,
            tunnel_dashboard_access: true,
            observability_enabled: true,
            observability_max_records: default_observability_max_records(),
            observability_batch_size: default_observability_batch_size(),
            observability_flush_interval_ms: default_observability_flush_interval_ms(),
            observability_max_json_size: default_observability_max_json_size(),
            outbound_proxy_enabled: false,
            outbound_proxy_url: String::new(),
            outbound_no_proxy: String::new(),
            mitm_router_base_url: default_mitm_router_base_url(),
            rtk_enabled: true,
            caveman_enabled: false,
            caveman_level: default_caveman_level(),
            extra: BTreeMap::new(),
        }
    }
}

impl Settings {
    pub fn normalize(&mut self) {
        if !self.outbound_proxy_enabled && !self.outbound_proxy_url.trim().is_empty() {
            self.outbound_proxy_enabled = true;
        }

        self.caveman_level = normalize_caveman_level_value(&self.caveman_level);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ModelAliasTarget {
    Path(String),
    Mapping(ProviderModelRef),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelRef {
    pub provider: String,
    pub model: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

pub type PricingTable = BTreeMap<String, BTreeMap<String, Value>>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct UsageDb {
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub history: Vec<UsageEntry>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub total_requests_lifetime: u64,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub daily_summary: BTreeMap<String, DailySummary>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl UsageDb {
    pub fn normalize(&mut self) {
        if self.total_requests_lifetime < self.history.len() as u64 {
            self.total_requests_lifetime = self.history.len() as u64;
        }

        self.daily_summary.clear();
        for entry in &self.history {
            aggregate_usage_entry(&mut self.daily_summary, entry);
        }
    }

    pub fn from_json_value(value: Value) -> Self {
        let Value::Object(mut fields) = value else {
            return Self::default();
        };

        let mut usage = Self {
            history: extract_named_field(&mut fields, "history"),
            total_requests_lifetime: extract_named_field(&mut fields, "totalRequestsLifetime"),
            daily_summary: extract_named_field(&mut fields, "dailySummary"),
            extra: fields.into_iter().collect(),
        };
        usage.normalize();
        usage
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageEntry {
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    pub model: String,
    #[serde(default)]
    pub tokens: Option<TokenUsage>,
    #[serde(default)]
    pub connection_id: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub cost: Option<f64>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct TokenUsage {
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub completion_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DailySummary {
    #[serde(default)]
    pub requests: u64,
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub cost: f64,
    #[serde(default)]
    pub by_provider: BTreeMap<String, SummaryCounter>,
    #[serde(default)]
    pub by_model: BTreeMap<String, SummaryCounter>,
    #[serde(default)]
    pub by_account: BTreeMap<String, SummaryCounter>,
    #[serde(default)]
    pub by_api_key: BTreeMap<String, SummaryCounter>,
    #[serde(default)]
    pub by_endpoint: BTreeMap<String, SummaryCounter>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SummaryCounter {
    #[serde(default)]
    pub requests: u64,
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub cost: f64,
    #[serde(default)]
    pub raw_model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HealthResponse {
    pub status: &'static str,
    pub component: &'static str,
}

impl HealthResponse {
    pub fn new(component: &'static str) -> Self {
        Self {
            status: "ok",
            component,
        }
    }
}

fn default_auth_type() -> String {
    "oauth".into()
}

fn default_proxy_type() -> String {
    "http".into()
}

fn default_model_type() -> String {
    "llm".into()
}

fn default_tunnel_provider() -> String {
    "cloudflare".into()
}

fn default_sticky_round_robin_limit() -> u32 {
    3
}

fn default_combo_strategy() -> String {
    "fallback".into()
}

fn default_observability_max_records() -> u32 {
    1000
}

fn default_observability_batch_size() -> u32 {
    20
}

fn default_observability_flush_interval_ms() -> u32 {
    5000
}

fn default_observability_max_json_size() -> u32 {
    1024
}

fn default_mitm_router_base_url() -> String {
    DEFAULT_MITM_ROUTER_BASE.into()
}

fn default_caveman_level() -> String {
    "full".into()
}

fn normalize_caveman_level_value(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "lite" => "lite".into(),
        "full" => "full".into(),
        "ultra" => "ultra".into(),
        _ => default_caveman_level(),
    }
}

fn default_true() -> bool {
    true
}

fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(|value| value.unwrap_or_default())
}

fn aggregate_usage_entry(daily_summary: &mut BTreeMap<String, DailySummary>, entry: &UsageEntry) {
    let date_key = entry
        .timestamp
        .as_deref()
        .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
        .map(|timestamp| timestamp.date_naive().to_string())
        .or_else(|| {
            entry
                .timestamp
                .as_ref()
                .map(|timestamp| timestamp.chars().take(10).collect())
        })
        .unwrap_or_else(|| "unknown".into());

    let summary = daily_summary
        .entry(date_key)
        .or_insert_with(|| DailySummary {
            requests: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            cost: 0.0,
            by_provider: BTreeMap::new(),
            by_model: BTreeMap::new(),
            by_account: BTreeMap::new(),
            by_api_key: BTreeMap::new(),
            by_endpoint: BTreeMap::new(),
            extra: BTreeMap::new(),
        });

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

    summary.requests += 1;
    summary.prompt_tokens += prompt_tokens;
    summary.completion_tokens += completion_tokens;
    summary.cost += cost;

    if let Some(provider) = entry.provider.as_deref() {
        add_to_counter(
            &mut summary.by_provider,
            provider,
            prompt_tokens,
            completion_tokens,
            cost,
            None,
        );
    }

    let model_key = match entry.provider.as_deref() {
        Some(provider) => format!("{}|{}", entry.model, provider),
        None => entry.model.clone(),
    };
    add_to_counter(
        &mut summary.by_model,
        &model_key,
        prompt_tokens,
        completion_tokens,
        cost,
        Some((
            Some(entry.model.clone()),
            entry.provider.clone(),
            None,
            None,
        )),
    );

    if let Some(connection_id) = entry.connection_id.as_deref() {
        add_to_counter(
            &mut summary.by_account,
            connection_id,
            prompt_tokens,
            completion_tokens,
            cost,
            Some((
                Some(entry.model.clone()),
                entry.provider.clone(),
                None,
                None,
            )),
        );
    }

    let api_key = entry
        .api_key
        .clone()
        .unwrap_or_else(|| "local-no-key".into());
    let api_key_key = format!(
        "{}|{}|{}",
        api_key,
        entry.model,
        entry.provider.clone().unwrap_or_else(|| "unknown".into())
    );
    add_to_counter(
        &mut summary.by_api_key,
        &api_key_key,
        prompt_tokens,
        completion_tokens,
        cost,
        Some((
            Some(entry.model.clone()),
            entry.provider.clone(),
            Some(entry.api_key.clone().unwrap_or_default()),
            None,
        )),
    );

    let endpoint = entry.endpoint.clone().unwrap_or_else(|| "Unknown".into());
    let endpoint_key = format!(
        "{}|{}|{}",
        endpoint,
        entry.model,
        entry.provider.clone().unwrap_or_else(|| "unknown".into())
    );
    add_to_counter(
        &mut summary.by_endpoint,
        &endpoint_key,
        prompt_tokens,
        completion_tokens,
        cost,
        Some((
            Some(entry.model.clone()),
            entry.provider.clone(),
            None,
            Some(endpoint),
        )),
    );
}

type SummaryMetadata = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn add_to_counter(
    target: &mut BTreeMap<String, SummaryCounter>,
    key: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    cost: f64,
    metadata: Option<SummaryMetadata>,
) {
    let counter = target
        .entry(key.to_string())
        .or_insert_with(|| SummaryCounter {
            requests: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            cost: 0.0,
            raw_model: None,
            provider: None,
            api_key: None,
            endpoint: None,
            extra: BTreeMap::new(),
        });

    counter.requests += 1;
    counter.prompt_tokens += prompt_tokens;
    counter.completion_tokens += completion_tokens;
    counter.cost += cost;

    if let Some((raw_model, provider, api_key, endpoint)) = metadata {
        if raw_model.is_some() {
            counter.raw_model = raw_model;
        }
        if provider.is_some() {
            counter.provider = provider;
        }
        if api_key.is_some() {
            counter.api_key = api_key;
        }
        if endpoint.is_some() {
            counter.endpoint = endpoint;
        }
    }
}

fn extract_named_field<T>(fields: &mut serde_json::Map<String, Value>, key: &str) -> T
where
    T: Default + DeserializeOwned,
{
    fields
        .remove(key)
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}
