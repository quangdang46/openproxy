use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::types::{TokenUsage, UsageDb, UsageEntry};

use super::pricing::Pricing;

pub struct UsageTracker {
    db: Arc<Db>,
    pricing: Pricing,
}

impl UsageTracker {
    pub fn new(db: Arc<Db>) -> Self {
        let snapshot = db.snapshot();
        let pricing = if snapshot.pricing.is_empty() {
            Pricing::default()
        } else {
            Pricing::from_db(&snapshot.pricing)
        };
        Self { db, pricing }
    }

    pub async fn track_request(
        &self,
        provider: &str,
        model: &str,
        tokens: Option<&TokenUsage>,
        connection_id: Option<&str>,
        api_key: Option<&str>,
        endpoint: Option<&str>,
    ) {
        let prompt_tokens = tokens
            .and_then(|t| t.prompt_tokens.or(t.input_tokens))
            .unwrap_or(0);
        let completion_tokens = tokens
            .and_then(|t| t.completion_tokens.or(t.output_tokens))
            .unwrap_or(0);

        let cost = self
            .pricing
            .calculate_cost(provider, model, prompt_tokens, completion_tokens);

        let entry = UsageEntry {
            timestamp: Some(Utc::now().to_rfc3339()),
            provider: Some(provider.to_string()),
            model: model.to_string(),
            tokens: tokens.cloned(),
            connection_id: connection_id.map(String::from),
            api_key: api_key.map(String::from),
            endpoint: endpoint.map(String::from),
            cost: Some(cost),
            status: None,
            extra: Default::default(),
        };

        let _ = self.db.update_usage(move |db| {
            db.history.push(entry);
            if db.total_requests_lifetime < db.history.len() as u64 {
                db.total_requests_lifetime = db.history.len() as u64;
            }
        }).await;
    }

    pub fn get_usage_db(&self) -> Arc<UsageDb> {
        self.db.usage_snapshot()
    }

    pub fn get_pricing(&self) -> &Pricing {
        &self.pricing
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSummary {
    pub total_requests: u64,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_cost: f64,
    pub days: Vec<DailyUsageSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyUsageSummary {
    pub date: String,
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost: f64,
    pub by_provider: Vec<ProviderUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderUsage {
    pub provider: String,
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost: f64,
}

impl UsageTracker {
    pub fn summarize(&self) -> UsageSummary {
        let usage_db = self.db.usage_snapshot();
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

        let days: Vec<_> = usage_db
            .daily_summary
            .iter()
            .map(|(date, summary)| DailyUsageSummary {
                date: date.clone(),
                requests: summary.requests,
                prompt_tokens: summary.prompt_tokens,
                completion_tokens: summary.completion_tokens,
                cost: summary.cost,
                by_provider: summary
                    .by_provider
                    .iter()
                    .map(|(provider, counter)| ProviderUsage {
                        provider: provider.clone(),
                        requests: counter.requests,
                        prompt_tokens: counter.prompt_tokens,
                        completion_tokens: counter.completion_tokens,
                        cost: counter.cost,
                    })
                    .collect(),
            })
            .collect();

        UsageSummary {
            total_requests: usage_db.total_requests_lifetime,
            total_prompt_tokens: total_prompt,
            total_completion_tokens: total_completion,
            total_cost,
            days,
        }
    }
}
