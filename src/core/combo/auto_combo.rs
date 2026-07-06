//! Auto Combo routing strategy
//!
//! Automatically forms model combinations based on model name patterns and
//! provider catalog matching. Instead of requiring the operator to enumerate
//! every member, auto-combo uses a pattern template (e.g. `"gpt-4o-mini"`,
//! `"claude-sonnet-4"`) and dynamically resolves which provider-backed models
//! match that name at dispatch time.
//!
//! The resolved members are then dispatched using a standard Fallback strategy
//! (sequential attempt in a stable order), which matches the "try each provider
//! that serves this model" expectation.

use std::collections::HashSet;

use serde_json::Value;

use crate::types::AppDb;

/// Resolved auto-combo member entry.
#[derive(Debug, Clone)]
pub struct AutoComboMember {
    /// The fully qualified model identifier (e.g. `"openai/gpt-4o-mini"`).
    pub model: String,
    /// How the match was made: "exact" when a `model` field matched, or
    /// "pattern" when a provider catalog model pattern matched the base.
    pub match_kind: String,
}

/// Configuration stored in a combo's `extra.autoComboConfig` map.
#[derive(Debug, Clone, Default)]
pub struct AutoComboConfig {
    /// Base model name used for matching (e.g. `"gpt-4o-mini"`). When
    /// empty, the combo itself acts as the base and we resolve from the
    /// full provider catalog.
    pub base_model: Option<String>,
    /// Maximum number of members to resolve. 0 = unlimited.
    pub max_members: usize,
    /// If true, only include one member per provider group (avoid
    /// multiple accounts of the same provider).
    pub deduplicate_providers: bool,
    /// Comma-separated list of provider names to prefer (tried first).
    pub preferred_providers: Vec<String>,
    /// Comma-separated list of provider names to exclude entirely.
    pub excluded_providers: Vec<String>,
}

impl AutoComboConfig {
    /// Parse an `AutoComboConfig` from the `extra.autoComboConfig` map.
    pub fn from_extra(extra: &serde_json::Map<String, Value>) -> Self {
        let cfg = extra.get("autoComboConfig").and_then(|v| v.as_object());
        let mut s = Self::default();
        if let Some(cfg) = cfg {
            if let Some(v) = cfg.get("baseModel").and_then(Value::as_str) {
                let v = v.trim();
                if !v.is_empty() {
                    s.base_model = Some(v.to_string());
                }
            }
            if let Some(v) = cfg.get("maxMembers").and_then(Value::as_u64) {
                s.max_members = v as usize;
            }
            if let Some(v) = cfg.get("deduplicateProviders").and_then(Value::as_bool) {
                s.deduplicate_providers = v;
            }
            if let Some(v) = cfg.get("preferredProviders").and_then(Value::as_str) {
                s.preferred_providers = v
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            if let Some(v) = cfg.get("excludedProviders").and_then(Value::as_str) {
                s.excluded_providers = v
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
        s
    }

    /// Whether any filtering or dedup is configured (used to skip work).
    pub fn has_filters(&self) -> bool {
        self.max_members > 0
            || self.deduplicate_providers
            || !self.preferred_providers.is_empty()
            || !self.excluded_providers.is_empty()
    }
}

/// Resolve auto-combo members from the combo's model list and the current
/// AppDb snapshot.
///
/// The resolution process:
/// 1. If `models` is provided explicitly (non-empty), use them as candidates.
/// 2. Otherwise, attempt to resolve `config.base_model` against the provider
///    catalog to discover all providers that serve a model matching that name.
/// 3. Apply filters (excluded providers, dedup, max members).
/// 4. Order: preferred providers first, then the rest.
///
/// Returns the resolved member list ready for dispatch.
pub fn resolve_auto_combo_members(
    models: &[String],
    config: &AutoComboConfig,
    snapshot: &AppDb,
) -> Vec<String> {
    if !models.is_empty() {
        // Explicit member list: apply filters only.
        let mut members: Vec<String> = models.to_vec();
        apply_filters(&mut members, config, snapshot);
        return members;
    }

    // Need to resolve from catalog.
    let base = match &config.base_model {
        Some(b) => b.clone(),
        None => return Vec::new(), // Nothing to resolve.
    };

    let mut candidates: Vec<String> = Vec::new();
    let mut seen_providers: HashSet<String> = HashSet::new();

    // Scan provider connections for matching models.
    for conn in &snapshot.provider_connections {
        let provider = &conn.provider;
        if config
            .excluded_providers
            .iter()
            .any(|p| p.eq_ignore_ascii_case(provider))
        {
            continue;
        }

        // Check if this provider has a default model matching the base.
        let has_match = conn
            .default_model
            .as_deref()
            .map(|m| model_matches(m, &base))
            .unwrap_or(false);

        if has_match {
            let entry = format!("{}/{}", provider, base);
            if config.deduplicate_providers {
                let provider_key = provider.to_lowercase();
                if !seen_providers.insert(provider_key) {
                    continue;
                }
            }
            candidates.push(entry);
        }

        // Also check custom models that reference this provider.
        for cm in &snapshot.custom_models {
            if cm.provider_alias.eq_ignore_ascii_case(provider) && model_matches(&cm.id, &base) {
                let entry = format!("{}/{}", provider, cm.id);
                if config.deduplicate_providers {
                    let provider_key = provider.to_lowercase();
                    if !seen_providers.insert(provider_key) {
                        continue;
                    }
                }
                if !candidates.contains(&entry) {
                    candidates.push(entry);
                }
            }
        }
    }

    // Order: preferred providers first, then the rest.
    let mut ordered: Vec<String> = Vec::with_capacity(candidates.len());
    let mut remaining: Vec<String> = Vec::new();

    for c in candidates {
        let provider_part = c.split('/').next().unwrap_or("");
        if config
            .preferred_providers
            .iter()
            .any(|p| p.eq_ignore_ascii_case(provider_part))
        {
            ordered.push(c);
        } else {
            remaining.push(c);
        }
    }

    // Preserve insertion order for remaining.
    for c in remaining {
        if !ordered.contains(&c) {
            ordered.push(c);
        }
    }

    // Apply max members cap.
    if config.max_members > 0 && ordered.len() > config.max_members {
        ordered.truncate(config.max_members);
    }

    ordered
}

/// Apply filter rules (excluded providers, dedup, max members) to an
/// already-resolved member list.
fn apply_filters(members: &mut Vec<String>, config: &AutoComboConfig, _snapshot: &AppDb) {
    // Excluded providers.
    if !config.excluded_providers.is_empty() {
        members.retain(|m| {
            let provider_part = m.split('/').next().unwrap_or("");
            !config
                .excluded_providers
                .iter()
                .any(|p| p.eq_ignore_ascii_case(provider_part))
        });
    }

    // Deduplicate providers: keep only the first member per provider.
    if config.deduplicate_providers {
        let mut seen: HashSet<String> = HashSet::new();
        members.retain(|m| {
            let provider_part = m.split('/').next().unwrap_or("").to_lowercase();
            seen.insert(provider_part)
        });
    }

    // Preferred providers: reorder.
    if !config.preferred_providers.is_empty() {
        let mut preferred: Vec<String> = Vec::new();
        let mut rest: Vec<String> = Vec::new();
        for m in members.drain(..) {
            let provider_part = m.split('/').next().unwrap_or("");
            if config
                .preferred_providers
                .iter()
                .any(|p| p.eq_ignore_ascii_case(provider_part))
            {
                preferred.push(m);
            } else {
                rest.push(m);
            }
        }
        members.extend(preferred);
        members.extend(rest);
    }

    // Max members cap.
    if config.max_members > 0 && members.len() > config.max_members {
        members.truncate(config.max_members);
    }
}

/// Check whether `model_name` matches `base_pattern`.
///
/// Matching is case-insensitive substring: the model name must contain the
/// base pattern.  For example, base pattern `"gpt-4o-mini"` matches model
/// `"gpt-4o-mini"`, `"gpt-4o-mini-2024-07-18"`, etc.
fn model_matches(model_name: &str, base_pattern: &str) -> bool {
    model_name
        .to_lowercase()
        .contains(&base_pattern.to_lowercase())
}

/// Retrieve the model identifier a provider connection serves,
/// based on its configured `default_model` field.
fn get_provider_model(conn: &crate::types::ProviderConnection) -> Option<String> {
    conn.default_model.clone().or_else(|| {
        // Fall back to the provider name itself.
        if !conn.provider.is_empty() {
            Some(conn.provider.clone())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AppDb, ProviderConnection};
    use serde_json::json;

    fn make_conn(provider: &str, default_model: &str) -> ProviderConnection {
        ProviderConnection {
            id: format!("conn-{}", provider),
            provider: provider.to_string(),
            default_model: Some(default_model.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn resolve_from_explicit_models_applies_filters() {
        let config = AutoComboConfig {
            excluded_providers: vec!["openai".into()],
            ..Default::default()
        };
        let models = vec!["openai/gpt-4o".into(), "anthropic/claude-sonnet-4".into()];
        let snapshot = AppDb::default();
        let members = resolve_auto_combo_members(&models, &config, &snapshot);
        assert_eq!(members.len(), 1);
        assert_eq!(members[0], "anthropic/claude-sonnet-4");
    }

    #[test]
    fn deduplicate_providers_keeps_first() {
        let config = AutoComboConfig {
            deduplicate_providers: true,
            ..Default::default()
        };
        let models = vec![
            "openai/gpt-4o".into(),
            "anthropic/claude-sonnet-4".into(),
            "openai/o1".into(), // same provider, should be dropped
        ];
        let snapshot = AppDb::default();
        let members = resolve_auto_combo_members(&models, &config, &snapshot);
        assert_eq!(members.len(), 2);
        assert!(members.contains(&"openai/gpt-4o".to_string()));
        assert!(members.contains(&"anthropic/claude-sonnet-4".to_string()));
    }

    #[test]
    fn max_members_truncates() {
        let config = AutoComboConfig {
            max_members: 1,
            ..Default::default()
        };
        let models = vec!["openai/gpt-4o".into(), "anthropic/claude-sonnet-4".into()];
        let snapshot = AppDb::default();
        let members = resolve_auto_combo_members(&models, &config, &snapshot);
        assert_eq!(members.len(), 1);
    }

    #[test]
    fn preferred_providers_ordered_first() {
        let config = AutoComboConfig {
            preferred_providers: vec!["anthropic".into()],
            ..Default::default()
        };
        let models = vec!["openai/gpt-4o".into(), "anthropic/claude-sonnet-4".into()];
        let snapshot = AppDb::default();
        let members = resolve_auto_combo_members(&models, &config, &snapshot);
        assert_eq!(members[0], "anthropic/claude-sonnet-4");
        assert_eq!(members[1], "openai/gpt-4o");
    }

    #[test]
    fn model_matches_substring() {
        assert!(model_matches("gpt-4o-mini-2024-07-18", "gpt-4o-mini"));
        assert!(model_matches("GPT-4O-MINI", "gpt-4o-mini"));
        assert!(!model_matches("gpt-4o", "gpt-4o-mini"));
    }

    #[test]
    fn empty_models_and_no_base_returns_empty() {
        let config = AutoComboConfig::default();
        let snapshot = AppDb::default();
        let members = resolve_auto_combo_members(&[], &config, &snapshot);
        assert!(members.is_empty());
    }

    #[test]
    fn auto_combo_config_parses_extra() {
        let extra = serde_json::from_str::<serde_json::Map<String, Value>>(
            r#"{
                "autoComboConfig": {
                    "baseModel": "gpt-4o-mini",
                    "maxMembers": 3,
                    "deduplicateProviders": true,
                    "preferredProviders": "openai,anthropic",
                    "excludedProviders": "google"
                }
            }"#,
        )
        .unwrap();

        let config = AutoComboConfig::from_extra(&extra);
        assert_eq!(config.base_model, Some("gpt-4o-mini".into()));
        assert_eq!(config.max_members, 3);
        assert!(config.deduplicate_providers);
        assert_eq!(config.preferred_providers, vec!["openai", "anthropic"]);
        assert_eq!(config.excluded_providers, vec!["google"]);
    }
}
