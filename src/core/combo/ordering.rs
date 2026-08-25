//! Cost- and latency-aware combo member ordering (`Cheapest` / `Fastest`).
//!
//! Both orderings read the same pricing table the usage/cost engine uses
//! (`AppDb::pricing`, exposed to the dashboard as `/api/pricing`), so a model
//! priced in the UI immediately affects routing order.

use serde_json::Value;

use crate::core::model::resolve_provider_alias;
use crate::core::usage::{parse_model_pricing, CostModel};
use crate::types::PricingTable;

/// Resolve the pricing row for a combo entry (`prefix/model`).
///
/// The pricing table may be keyed by provider alias (`cc`) or canonical id
/// (`claude`), and free tiers register a single provider-wide `all` row
/// (`kiro`, `opencode`, `vertex`), so all four combinations are tried.
fn pricing_entry<'a>(model: &'a str, pricing: &'a PricingTable) -> Option<(&'a Value, &'a str)> {
    let (prefix, name) = model.split_once('/')?;
    let resolved = resolve_provider_alias(prefix);
    for provider in [prefix, resolved.as_str()] {
        if let Some(models) = pricing.get(provider) {
            if let Some(entry) = models.get(name).or_else(|| models.get("all")) {
                return Some((entry, name));
            }
        }
    }
    None
}

/// Representative per-request cost of a combo member (USD per 1M in + out).
///
/// A missing provider/model row, an unpriced row, or a non-per-token cost
/// model (free / flat-monthly subscription / prepaid credits) all yield `0.0`,
/// so free-tier members sort ahead of metered ones under `Cheapest`.
fn model_cost(model: &str, pricing: &PricingTable) -> f64 {
    let Some((entry, name)) = pricing_entry(model, pricing) else {
        return 0.0;
    };
    let provider = model.split('/').next().unwrap_or_default();
    let parsed = parse_model_pricing(provider, name, entry);
    match parsed.cost_model {
        CostModel::PerToken => parsed.input_price_per_million + parsed.output_price_per_million,
        CostModel::Free | CostModel::FlatMonthly | CostModel::Credits => 0.0,
    }
}

/// Sort combo members ascending by cost (free `$0` first).
///
/// Stable: equal-cost members keep their configured relative order, so the
/// operator's declared priority still decides ties.
pub fn sort_models_by_cost(models: &[String], pricing: &PricingTable) -> Vec<String> {
    sort_by_key(models, |model| Some(model_cost(model, pricing)))
}

/// Latency hint (ms) for a combo member, when the pricing row carries one.
fn model_latency(model: &str, pricing: &PricingTable) -> Option<f64> {
    let (entry, _) = pricing_entry(model, pricing)?;
    let obj = entry.as_object()?;
    ["latency", "latencyMs", "latency_ms"]
        .iter()
        .find_map(|field| obj.get(*field).and_then(Value::as_f64))
}

/// Sort combo members ascending by latency hint (fastest first).
///
/// Members without a hint keep their configured relative order and are placed
/// after every hinted member, so `Fastest` degrades to the declared priority
/// when no latency data exists.
pub fn sort_models_by_latency(models: &[String], pricing: &PricingTable) -> Vec<String> {
    sort_by_key(models, |model| model_latency(model, pricing))
}

/// Stable ascending sort on an optional numeric key; `None` sorts last.
fn sort_by_key(models: &[String], key: impl Fn(&str) -> Option<f64>) -> Vec<String> {
    let mut indexed: Vec<(usize, Option<f64>, &String)> = models
        .iter()
        .enumerate()
        .map(|(index, model)| (index, key(model), model))
        .collect();
    indexed.sort_by(|a, b| match (a.1, b.1) {
        (Some(x), Some(y)) => x
            .partial_cmp(&y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.0.cmp(&b.0),
    });
    indexed
        .into_iter()
        .map(|(_, _, model)| model.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn table(rows: &[(&str, &str, Value)]) -> PricingTable {
        let mut pricing = PricingTable::new();
        for (provider, model, value) in rows {
            pricing
                .entry((*provider).to_string())
                .or_default()
                .insert((*model).to_string(), value.clone());
        }
        pricing
    }

    #[test]
    fn cheapest_sorts_by_api_style_pricing() {
        let models = vec![
            "openai/gpt-4o".to_string(),
            "openrouter/llama-3.1-free".to_string(),
            "anthropic/claude-3-haiku".to_string(),
        ];
        let pricing = table(&[
            ("openai", "gpt-4o", json!({ "input": 2.5, "output": 10.0 })),
            (
                "openrouter",
                "llama-3.1-free",
                json!({ "input": 0.0, "output": 0.0 }),
            ),
            (
                "anthropic",
                "claude-3-haiku",
                json!({ "input": 0.25, "output": 1.25 }),
            ),
        ]);
        assert_eq!(
            sort_models_by_cost(&models, &pricing),
            vec![
                "openrouter/llama-3.1-free".to_string(),
                "anthropic/claude-3-haiku".to_string(),
                "openai/gpt-4o".to_string(),
            ]
        );
    }

    #[test]
    fn cheapest_treats_missing_pricing_as_free_and_is_stable() {
        let models = vec![
            "openai/gpt-4o".to_string(),
            "nvidia/llama-3.1".to_string(),
            "oc/grok-code".to_string(),
        ];
        let pricing = table(&[("openai", "gpt-4o", json!({ "input": 2.5, "output": 10.0 }))]);
        assert_eq!(
            sort_models_by_cost(&models, &pricing),
            vec![
                "nvidia/llama-3.1".to_string(),
                "oc/grok-code".to_string(),
                "openai/gpt-4o".to_string(),
            ],
            "unpriced members are free and keep their configured order"
        );
    }

    #[test]
    fn cheapest_reads_alias_keyed_and_provider_wide_rows() {
        let models = vec![
            "cc/claude-opus-4".to_string(),
            "kr/claude-sonnet".to_string(),
        ];
        let pricing = table(&[
            (
                "claude",
                "claude-opus-4",
                json!({ "input": 15.0, "output": 75.0 }),
            ),
            ("kiro", "all", json!({ "costModel": "free" })),
        ]);
        assert_eq!(
            sort_models_by_cost(&models, &pricing),
            vec![
                "kr/claude-sonnet".to_string(),
                "cc/claude-opus-4".to_string(),
            ],
            "alias cc→claude resolves and kiro's free `all` row wins"
        );
    }

    #[test]
    fn cheapest_ignores_non_metered_cost_models() {
        let models = vec!["kimi/kimi-k2.5".to_string(), "glm/glm-4.7".to_string()];
        let pricing = table(&[
            (
                "kimi",
                "kimi-k2.5",
                json!({ "costModel": "flat_monthly", "flatMonthlyPrice": 9.0 }),
            ),
            ("glm", "glm-4.7", json!({ "pricePerMillion": 0.6 })),
        ]);
        assert_eq!(
            sort_models_by_cost(&models, &pricing),
            vec!["kimi/kimi-k2.5".to_string(), "glm/glm-4.7".to_string()],
            "a flat-monthly subscription has no marginal per-request cost"
        );
    }

    #[test]
    fn fastest_sorts_by_latency_hint() {
        let models = vec![
            "openai/gpt-4o".to_string(),
            "nvidia/llama-3.1".to_string(),
            "anthropic/claude-3-haiku".to_string(),
        ];
        let pricing = table(&[
            ("openai", "gpt-4o", json!({ "latency": 800.0 })),
            ("nvidia", "llama-3.1", json!({ "latencyMs": 200 })),
            ("anthropic", "claude-3-haiku", json!({ "latency": 400.0 })),
        ]);
        assert_eq!(
            sort_models_by_latency(&models, &pricing),
            vec![
                "nvidia/llama-3.1".to_string(),
                "anthropic/claude-3-haiku".to_string(),
                "openai/gpt-4o".to_string(),
            ]
        );
    }

    #[test]
    fn fastest_keeps_configured_order_without_hints() {
        let models = vec![
            "openai/gpt-4o".to_string(),
            "nvidia/llama-3.1".to_string(),
            "anthropic/claude-3-haiku".to_string(),
        ];
        let pricing = table(&[("nvidia", "llama-3.1", json!({ "latency": 200.0 }))]);
        assert_eq!(
            sort_models_by_latency(&models, &pricing),
            vec![
                "nvidia/llama-3.1".to_string(),
                "openai/gpt-4o".to_string(),
                "anthropic/claude-3-haiku".to_string(),
            ],
            "hinted member first, unhinted members keep configured order"
        );
    }
}
