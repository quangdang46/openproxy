use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::PricingTable;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CostModel {
    #[serde(rename = "per_token")]
    PerToken,
    #[serde(rename = "flat_monthly")]
    FlatMonthly,
    #[serde(rename = "free")]
    Free,
    #[serde(rename = "credits")]
    Credits,
}

impl CostModel {
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "per_token" | "per-token" => Self::PerToken,
            "flat_monthly" | "flat-monthly" => Self::FlatMonthly,
            "free" => Self::Free,
            "credits" => Self::Credits,
            _ => Self::PerToken,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    pub model: String,
    pub provider: String,
    pub cost_model: CostModel,
    #[serde(default)]
    pub input_price_per_million: f64,
    #[serde(default)]
    pub output_price_per_million: f64,
    #[serde(default)]
    pub cache_creation_price_per_million: f64,
    #[serde(default)]
    pub cache_read_price_per_million: f64,
    /// Reasoning tokens bill at their own rate on some models (Anthropic bills
    /// them above `output`). `None` means "same as output", which is 9router's
    /// `pricing.reasoning || pricing.output` default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_price_per_million: Option<f64>,
    #[serde(default)]
    pub flat_monthly_price: f64,
    #[serde(default)]
    pub credits: f64,
}

impl ModelPricing {
    pub fn new(model: &str, provider: &str, cost_model: CostModel) -> Self {
        Self {
            model: model.to_string(),
            provider: provider.to_string(),
            cost_model,
            input_price_per_million: 0.0,
            output_price_per_million: 0.0,
            cache_creation_price_per_million: 0.0,
            cache_read_price_per_million: 0.0,
            reasoning_price_per_million: None,
            flat_monthly_price: 0.0,
            credits: 0.0,
        }
    }

    pub fn per_token(model: &str, provider: &str, price_per_million: f64) -> Self {
        Self {
            model: model.to_string(),
            provider: provider.to_string(),
            cost_model: CostModel::PerToken,
            input_price_per_million: price_per_million,
            output_price_per_million: price_per_million,
            cache_creation_price_per_million: 0.0,
            cache_read_price_per_million: 0.0,
            reasoning_price_per_million: None,
            flat_monthly_price: 0.0,
            credits: 0.0,
        }
    }

    pub fn flat_monthly(model: &str, provider: &str, price: f64) -> Self {
        Self {
            model: model.to_string(),
            provider: provider.to_string(),
            cost_model: CostModel::FlatMonthly,
            input_price_per_million: 0.0,
            output_price_per_million: 0.0,
            cache_creation_price_per_million: 0.0,
            cache_read_price_per_million: 0.0,
            reasoning_price_per_million: None,
            flat_monthly_price: price,
            credits: 0.0,
        }
    }

    pub fn free(model: &str, provider: &str) -> Self {
        Self {
            model: model.to_string(),
            provider: provider.to_string(),
            cost_model: CostModel::Free,
            input_price_per_million: 0.0,
            output_price_per_million: 0.0,
            cache_creation_price_per_million: 0.0,
            cache_read_price_per_million: 0.0,
            reasoning_price_per_million: None,
            flat_monthly_price: 0.0,
            credits: 0.0,
        }
    }

    pub fn credits(model: &str, provider: &str, amount: f64) -> Self {
        Self {
            model: model.to_string(),
            provider: provider.to_string(),
            cost_model: CostModel::Credits,
            input_price_per_million: 0.0,
            output_price_per_million: 0.0,
            cache_creation_price_per_million: 0.0,
            cache_read_price_per_million: 0.0,
            reasoning_price_per_million: None,
            flat_monthly_price: 0.0,
            credits: amount,
        }
    }

    /// 9router `calculateCostFromTokens` (open-sse/providers/pricing.js:418-449).
    ///
    /// `input_tokens` is cache-INCLUSIVE — cached and cache-created tokens are
    /// subsets of it — so charging all three independently bills the same tokens
    /// twice, once at the full input rate. Subtract the subsets first, then
    /// charge each at its own rate.
    ///
    /// The `0.0 → input` fallbacks on the cache rates reproduce 9router's
    /// `pricing.cached || pricing.input`: a single-rate table leaves those keys
    /// unset, and JS `||` treats the missing rate as "charge the input rate"
    /// rather than "charge nothing".
    pub fn calculate_cost(
        &self,
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_tokens: u64,
        cache_read_tokens: u64,
        reasoning_tokens: u64,
    ) -> f64 {
        if !matches!(self.cost_model, CostModel::PerToken) {
            return 0.0;
        }

        let non_cached_input =
            input_tokens.saturating_sub(cache_read_tokens + cache_creation_tokens);
        let cache_read_price = self.rate_or(
            self.cache_read_price_per_million,
            self.input_price_per_million,
        );
        let cache_creation_price = self.rate_or(
            self.cache_creation_price_per_million,
            self.input_price_per_million,
        );
        let reasoning_price = self
            .reasoning_price_per_million
            .unwrap_or(self.output_price_per_million);

        (non_cached_input as f64 / 1_000_000.0) * self.input_price_per_million
            + (cache_read_tokens as f64 / 1_000_000.0) * cache_read_price
            + (output_tokens as f64 / 1_000_000.0) * self.output_price_per_million
            + (reasoning_tokens as f64 / 1_000_000.0) * reasoning_price
            + (cache_creation_tokens as f64 / 1_000_000.0) * cache_creation_price
    }

    fn rate_or(&self, configured: f64, fallback: f64) -> f64 {
        if configured == 0.0 {
            fallback
        } else {
            configured
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pricing {
    rates: BTreeMap<String, BTreeMap<String, ModelPricing>>,
}

impl Pricing {
    pub fn new() -> Self {
        let mut rates = BTreeMap::new();

        rates.insert(
            "glm".to_string(),
            BTreeMap::from([
                (
                    "glm-5.1".to_string(),
                    ModelPricing::per_token("glm-5.1", "glm", 0.6),
                ),
                (
                    "glm-5".to_string(),
                    ModelPricing::per_token("glm-5", "glm", 0.6),
                ),
                (
                    "glm-4.7".to_string(),
                    ModelPricing::per_token("glm-4.7", "glm", 0.6),
                ),
            ]),
        );

        rates.insert(
            "minimax".to_string(),
            BTreeMap::from([
                (
                    "MiniMax-M2.7".to_string(),
                    ModelPricing::per_token("MiniMax-M2.7", "minimax", 0.2),
                ),
                (
                    "MiniMax-M2.5".to_string(),
                    ModelPricing::per_token("MiniMax-M2.5", "minimax", 0.2),
                ),
            ]),
        );

        rates.insert(
            "kimi".to_string(),
            BTreeMap::from([
                (
                    "kimi-k2.5".to_string(),
                    ModelPricing::flat_monthly("kimi-k2.5", "kimi", 9.0),
                ),
                (
                    "kimi-k2.5-thinking".to_string(),
                    ModelPricing::flat_monthly("kimi-k2.5-thinking", "kimi", 9.0),
                ),
            ]),
        );

        rates.insert(
            "kiro".to_string(),
            BTreeMap::from([("all".to_string(), ModelPricing::free("all", "kiro"))]),
        );

        rates.insert(
            "opencode".to_string(),
            BTreeMap::from([("all".to_string(), ModelPricing::free("all", "opencode"))]),
        );

        rates.insert(
            "vertex".to_string(),
            BTreeMap::from([(
                "all".to_string(),
                ModelPricing::credits("all", "vertex", 300.0),
            )]),
        );

        Self { rates }
    }

    pub fn from_db(db_pricing: &PricingTable) -> Self {
        let mut rates = BTreeMap::new();

        for (provider, models) in db_pricing {
            let mut model_rates = BTreeMap::new();
            for (model, value) in models {
                let pricing = parse_model_pricing(provider, model, value);
                model_rates.insert(model.clone(), pricing);
            }
            if !model_rates.is_empty() {
                rates.insert(provider.clone(), model_rates);
            }
        }

        if rates.is_empty() {
            return Self::new();
        }

        Self { rates }
    }

    pub fn get(&self, provider: &str, model: &str) -> Option<&ModelPricing> {
        if let Some(models) = self.rates.get(provider) {
            if let Some(pricing) = models.get(model) {
                return Some(pricing);
            }
            return models.get("all");
        }
        None
    }

    pub fn calculate_cost(
        &self,
        provider: &str,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_tokens: u64,
        cache_read_tokens: u64,
    ) -> f64 {
        self.calculate_cost_with_reasoning(
            provider,
            model,
            input_tokens,
            output_tokens,
            cache_creation_tokens,
            cache_read_tokens,
            0,
        )
    }

    /// The full cost model. Callers that have a reasoning-token count — a
    /// thinking model bills those above `output` — must use this rather than
    /// the four-counter convenience above, which drops the term.
    pub fn calculate_cost_with_reasoning(
        &self,
        provider: &str,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_tokens: u64,
        cache_read_tokens: u64,
        reasoning_tokens: u64,
    ) -> f64 {
        self.get(provider, model)
            .map(|p| {
                p.calculate_cost(
                    input_tokens,
                    output_tokens,
                    cache_creation_tokens,
                    cache_read_tokens,
                    reasoning_tokens,
                )
            })
            .unwrap_or(0.0)
    }
}

impl Default for Pricing {
    fn default() -> Self {
        Self::new()
    }
}

/// Accepted entry shapes: `{input,output,cached,cache_creation}`,
/// `{*PricePerMillion}`, legacy `{pricePerMillion}`, or a bare number.
/// Crate-public so `core::combo::ordering` reads pricing identically.
pub(crate) fn parse_model_pricing(provider: &str, model: &str, value: &Value) -> ModelPricing {
    if let Some(obj) = value.as_object() {
        let cost_model = obj
            .get("costModel")
            .and_then(Value::as_str)
            .map(CostModel::from_str)
            .unwrap_or(CostModel::PerToken);

        let flat_monthly_price = obj
            .get("flatMonthlyPrice")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);

        let credits = obj.get("credits").and_then(Value::as_f64).unwrap_or(0.0);

        // Try API-style flat pricing fields (input, output, cached, cache_creation)
        let api_input = obj.get("input").and_then(Value::as_f64);
        let api_output = obj.get("output").and_then(Value::as_f64);
        let api_cached = obj.get("cached").and_then(Value::as_f64);
        let api_cache_creation = obj.get("cache_creation").and_then(Value::as_f64);
        // Reasoning has no legacy flat rate: an absent key means "bill at the
        // output rate", so it stays `None` rather than defaulting to a number.
        let api_reasoning = obj
            .get("reasoning")
            .or_else(|| obj.get("reasoningPricePerMillion"))
            .and_then(Value::as_f64);

        // Try per-field per-million names
        let field_input = obj.get("inputPricePerMillion").and_then(Value::as_f64);
        let field_output = obj.get("outputPricePerMillion").and_then(Value::as_f64);
        let field_cache_read = obj.get("cacheReadPricePerMillion").and_then(Value::as_f64);
        let field_cache_creation = obj
            .get("cacheCreationPricePerMillion")
            .and_then(Value::as_f64);

        // Fallback to legacy flat pricePerMillion
        let flat = obj.get("pricePerMillion").and_then(Value::as_f64);

        let input_price_per_million = api_input.or(field_input).or(flat).unwrap_or(0.0);
        let output_price_per_million = api_output.or(field_output).or(flat).unwrap_or(0.0);
        let cache_read_price_per_million = api_cached.or(field_cache_read).or(flat).unwrap_or(0.0);
        let cache_creation_price_per_million =
            api_cache_creation.or(field_cache_creation).unwrap_or(0.0);

        return ModelPricing {
            model: model.to_string(),
            provider: provider.to_string(),
            cost_model,
            input_price_per_million,
            output_price_per_million,
            cache_creation_price_per_million,
            cache_read_price_per_million,
            reasoning_price_per_million: api_reasoning,
            flat_monthly_price,
            credits,
        };
    }

    if let Some(num) = value.as_f64() {
        return ModelPricing::per_token(model, provider, num);
    }

    ModelPricing::new(model, provider, CostModel::PerToken)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glm_pricing() {
        let pricing = Pricing::new();
        let p = pricing.get("glm", "glm-5.1").unwrap();
        assert_eq!(p.cost_model, CostModel::PerToken);
        assert_eq!(p.input_price_per_million, 0.6);
        assert_eq!(p.output_price_per_million, 0.6);
        let cost = p.calculate_cost(1_000_000, 500_000, 0, 0, 0);
        assert!((cost - 0.9).abs() < 0.001);
    }

    #[test]
    fn test_minimax_pricing() {
        let pricing = Pricing::new();
        let p = pricing.get("minimax", "MiniMax-M2.7").unwrap();
        assert_eq!(p.cost_model, CostModel::PerToken);
        assert_eq!(p.input_price_per_million, 0.2);
        assert_eq!(p.output_price_per_million, 0.2);
    }

    #[test]
    fn test_kimi_pricing() {
        let pricing = Pricing::new();
        let p = pricing.get("kimi", "kimi-k2.5").unwrap();
        assert_eq!(p.cost_model, CostModel::FlatMonthly);
        assert_eq!(p.flat_monthly_price, 9.0);
    }

    #[test]
    fn test_kiro_pricing() {
        let pricing = Pricing::new();
        let p = pricing.get("kiro", "all").unwrap();
        assert_eq!(p.cost_model, CostModel::Free);
        assert_eq!(p.calculate_cost(1_000_000, 500_000, 0, 0, 0), 0.0);
    }

    #[test]
    fn test_vertex_pricing() {
        let pricing = Pricing::new();
        let p = pricing.get("vertex", "all").unwrap();
        assert_eq!(p.cost_model, CostModel::Credits);
        assert_eq!(p.credits, 300.0);
    }

    #[test]
    fn test_calculate_cost_helper() {
        let pricing = Pricing::new();
        let cost = pricing.calculate_cost("glm", "glm-5.1", 1_000_000, 0, 0, 0);
        assert!((cost - 0.6).abs() < 0.001);
    }

    #[test]
    fn test_free_model_returns_zero() {
        let pricing = Pricing::new();
        assert_eq!(
            pricing.calculate_cost("kiro", "claude-sonnet-4.5", 100_000_000, 50_000_000, 0, 0),
            0.0
        );
    }

    #[test]
    fn test_unknown_model_returns_zero() {
        let pricing = Pricing::new();
        assert_eq!(
            pricing.calculate_cost("unknown", "unknown-model", 1_000_000, 1_000_000, 0, 0),
            0.0
        );
    }

    #[test]
    fn test_cache_pricing_separate_rates() {
        let pricing = Pricing::new();

        // Parse a pricing with separate per-field rates mimicking the API format
        let value = serde_json::json!({
            "input": 2.0,
            "output": 10.0,
            "cached": 0.5,
            "cache_creation": 3.0,
        });
        let p = parse_model_pricing("test-provider", "test-model", &value);

        assert_eq!(p.input_price_per_million, 2.0);
        assert_eq!(p.output_price_per_million, 10.0);
        assert_eq!(p.cache_read_price_per_million, 0.5);
        assert_eq!(p.cache_creation_price_per_million, 3.0);

        // The cache counters are subsets of the 1M input, so the input rate only
        // covers the remaining 300k: 300k @ 2.0 = 0.6. Then 1M output @ 10.0 =
        // 10.0, 200k cache read @ 0.5 = 0.1, 500k cache creation @ 3.0 = 1.5.
        // Total = 12.2
        let cost = p.calculate_cost(1_000_000, 1_000_000, 500_000, 200_000, 0);
        assert!((cost - 12.2).abs() < 0.001);
    }

    /// The bug this guards: the old sum billed all three counters independently,
    /// so a fully-cached turn cost the full input rate PLUS the cache rate —
    /// 10x the real charge for the same tokens.
    #[test]
    fn a_fully_cached_turn_costs_the_cache_rate_not_the_input_rate() {
        let p = parse_model_pricing(
            "test-provider",
            "test-model",
            &serde_json::json!({ "input": 3.0, "output": 15.0, "cached": 0.3 }),
        );

        let cost = p.calculate_cost(1_000_000, 0, 0, 1_000_000, 0);
        assert!(
            (cost - 0.3).abs() < 0.0001,
            "1M cached tokens @ 0.3/1M is $0.30, got {cost}"
        );
    }

    #[test]
    fn reasoning_tokens_use_the_reasoning_rate_and_fall_back_to_output() {
        let priced = parse_model_pricing(
            "test-provider",
            "test-model",
            &serde_json::json!({ "input": 3.0, "output": 15.0, "reasoning": 22.5 }),
        );
        assert_eq!(priced.reasoning_price_per_million, Some(22.5));
        let with_reasoning = priced.calculate_cost(0, 0, 0, 0, 1_000_000);
        assert!(
            (with_reasoning - 22.5).abs() < 0.0001,
            "got {with_reasoning}"
        );

        // No `reasoning` key → 9router's `pricing.reasoning || pricing.output`.
        let unpriced = parse_model_pricing(
            "test-provider",
            "test-model",
            &serde_json::json!({ "input": 3.0, "output": 15.0 }),
        );
        assert_eq!(unpriced.reasoning_price_per_million, None);
        let fallback = unpriced.calculate_cost(0, 0, 0, 0, 1_000_000);
        assert!((fallback - 15.0).abs() < 0.0001, "got {fallback}");
    }

    /// The subtraction must not shift a turn that never touched the cache.
    #[test]
    fn a_zero_cache_turn_is_unchanged() {
        let p = parse_model_pricing(
            "test-provider",
            "test-model",
            &serde_json::json!({ "input": 2.0, "output": 10.0, "cached": 0.5, "cache_creation": 3.0 }),
        );
        let cost = p.calculate_cost(1_000_000, 1_000_000, 0, 0, 0);
        assert!((cost - 12.0).abs() < 0.001, "got {cost}");
    }

    /// `Math.max(0, …)`: a provider whose input count excludes its cache count
    /// (Anthropic's `input_tokens` does) must floor at zero, not wrap.
    #[test]
    fn a_cache_count_larger_than_the_input_floors_at_zero() {
        let p = parse_model_pricing(
            "test-provider",
            "test-model",
            &serde_json::json!({ "input": 2.0, "output": 10.0, "cached": 0.5 }),
        );
        let cost = p.calculate_cost(100, 0, 0, 5_000, 0);
        assert!(
            (cost - 0.0025).abs() < 0.0001,
            "the 100 input tokens floor at zero and only the 5k cached tokens \
             are chargeable at $0.50/1M, got {cost}"
        );
    }

    #[test]
    fn test_parse_api_pricing_format() {
        // The format stored by the API pricing endpoints
        let value = serde_json::json!({
            "input": 1.75,
            "output": 14.0,
            "cached": 0.175,
            "cache_creation": 1.75,
        });
        let p = parse_model_pricing("gh", "gpt-5.3-codex", &value);

        assert_eq!(p.input_price_per_million, 1.75);
        assert_eq!(p.output_price_per_million, 14.0);
        assert_eq!(p.cache_read_price_per_million, 0.175);
        assert_eq!(p.cache_creation_price_per_million, 1.75);
        assert_eq!(p.cost_model, CostModel::PerToken);

        // 1M input tokens: 1 * 1.75 = 1.75
        let cost = p.calculate_cost(1_000_000, 0, 0, 0, 0);
        assert!((cost - 1.75).abs() < 0.001);
    }

    #[test]
    fn test_parse_legacy_flat_rate() {
        // Legacy format with single pricePerMillion
        let value = serde_json::json!({
            "pricePerMillion": 5.0,
        });
        let p = parse_model_pricing("test", "model", &value);
        assert_eq!(p.input_price_per_million, 5.0);
        assert_eq!(p.output_price_per_million, 5.0);
        assert_eq!(p.cache_read_price_per_million, 5.0);
        assert_eq!(p.cache_creation_price_per_million, 0.0);
    }
}
