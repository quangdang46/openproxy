//! Payload rules — per-request transformations applied to the chat
//! completions body before it is forwarded upstream.
//!
//! Modeled after OmniRoute's `payloadRules` (MIT) but rewritten in Rust.
//! Four rule kinds, all selected by a wildcard match against the request's
//! `model` field (and optionally a `protocol` tag):
//!
//! * `default`  — set params when the caller did not supply them
//! * `override` — force params, replacing any caller-supplied value
//! * `filter`   — strip JSON pointer-style paths from the outgoing payload
//! * `default_raw` — `default`-style fill but applied to the raw upstream
//!   payload after format conversion. In openproxy today most providers
//!   accept the body verbatim, so this currently behaves identically to
//!   `default`. Kept as a distinct slot so we can wire it after the
//!   per-provider transform layer lands without another schema bump.
//!
//! The rule set is persisted under `Settings::payload_rules` in `db.json`
//! and edited live via `PUT /api/settings/payload-rules`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One entry in a rule's `models` array.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PayloadRuleModelSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
}

/// A rule that injects (default) or forces (override / default_raw) params.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PayloadMutationRule {
    pub models: Vec<PayloadRuleModelSpec>,
    /// Object whose keys (dot-separated) describe where to set values.
    /// Example: `{ "temperature": 0.2, "metadata.user_id": "redacted" }`
    /// becomes `body.temperature = 0.2` and `body.metadata.user_id =
    /// "redacted"`.
    #[serde(default)]
    pub params: Map<String, Value>,
}

/// A rule that strips dot-paths from the outgoing payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PayloadFilterRule {
    pub models: Vec<PayloadRuleModelSpec>,
    #[serde(default)]
    pub params: Vec<String>,
}

/// Persisted top-level config. All four lists default to empty so an
/// upgraded `db.json` from a previous release deserialises cleanly.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PayloadRulesConfig {
    #[serde(default)]
    pub default: Vec<PayloadMutationRule>,
    #[serde(default)]
    pub r#override: Vec<PayloadMutationRule>,
    #[serde(default)]
    pub filter: Vec<PayloadFilterRule>,
    #[serde(default)]
    pub default_raw: Vec<PayloadMutationRule>,
}

impl PayloadRulesConfig {
    pub fn is_empty(&self) -> bool {
        self.default.is_empty()
            && self.r#override.is_empty()
            && self.filter.is_empty()
            && self.default_raw.is_empty()
    }

    /// Trim, drop empty entries, and normalize order so the persisted form
    /// is stable across saves.
    pub fn normalize(&mut self) {
        normalize_mutation_rules(&mut self.default);
        normalize_mutation_rules(&mut self.r#override);
        normalize_filter_rules(&mut self.filter);
        normalize_mutation_rules(&mut self.default_raw);
    }

    pub fn summary(&self) -> PayloadRulesSummary {
        PayloadRulesSummary {
            default: self.default.len(),
            r#override: self.r#override.len(),
            filter: self.filter.len(),
            default_raw: self.default_raw.len(),
        }
    }
}

/// Counts surfaced in the UI summary card.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PayloadRulesSummary {
    pub default: usize,
    pub r#override: usize,
    pub filter: usize,
    pub default_raw: usize,
}

fn normalize_mutation_rules(rules: &mut Vec<PayloadMutationRule>) {
    rules.retain(|rule| !rule.models.is_empty() && !rule.params.is_empty());
    for rule in rules.iter_mut() {
        rule.models.retain(|m| !m.name.trim().is_empty());
        for spec in rule.models.iter_mut() {
            spec.name = spec.name.trim().to_string();
            spec.protocol = spec.protocol.as_ref().and_then(|p| {
                let t = p.trim();
                if t.is_empty() {
                    None
                } else {
                    Some(t.to_string())
                }
            });
        }
    }
}

fn normalize_filter_rules(rules: &mut Vec<PayloadFilterRule>) {
    rules.retain(|rule| !rule.models.is_empty() && !rule.params.is_empty());
    for rule in rules.iter_mut() {
        rule.models.retain(|m| !m.name.trim().is_empty());
        rule.params.retain(|p| !p.trim().is_empty());
        for spec in rule.models.iter_mut() {
            spec.name = spec.name.trim().to_string();
            spec.protocol = spec.protocol.as_ref().and_then(|p| {
                let t = p.trim();
                if t.is_empty() {
                    None
                } else {
                    Some(t.to_string())
                }
            });
        }
    }
}

/// Apply `default`, `override`, and `filter` rules to a request body in
/// place. Each rule is checked against `(model_name, protocol)` using glob
/// matching against the rule's `models` array; matching rules are applied
/// in the order they appear in the config.
///
/// Returns `true` if any rule modified the body. The caller can use this
/// for logging / observability.
pub fn apply_request_rules(
    body: &mut Value,
    model_name: &str,
    protocol: Option<&str>,
    config: &PayloadRulesConfig,
) -> bool {
    let body_obj = match body.as_object_mut() {
        Some(obj) => obj,
        None => return false,
    };

    let mut mutated = false;

    // default — fill in missing values
    for rule in &config.default {
        if !rule_matches(&rule.models, model_name, protocol) {
            continue;
        }
        for (path, value) in &rule.params {
            if set_path_if_absent(body_obj, path, value.clone()) {
                mutated = true;
            }
        }
    }

    // override — force values
    for rule in &config.r#override {
        if !rule_matches(&rule.models, model_name, protocol) {
            continue;
        }
        for (path, value) in &rule.params {
            set_path(body_obj, path, value.clone());
            mutated = true;
        }
    }

    // filter — strip paths
    for rule in &config.filter {
        if !rule_matches(&rule.models, model_name, protocol) {
            continue;
        }
        for path in &rule.params {
            if remove_path(body_obj, path) {
                mutated = true;
            }
        }
    }

    mutated
}

/// `default_raw` is applied after format-conversion. In openproxy's
/// current pipeline the body is forwarded mostly verbatim, so we expose
/// this as a separate entry point that callers can choose to invoke after
/// they perform any provider-specific transform.
pub fn apply_raw_rules(
    body: &mut Value,
    model_name: &str,
    protocol: Option<&str>,
    config: &PayloadRulesConfig,
) -> bool {
    let body_obj = match body.as_object_mut() {
        Some(obj) => obj,
        None => return false,
    };

    let mut mutated = false;
    for rule in &config.default_raw {
        if !rule_matches(&rule.models, model_name, protocol) {
            continue;
        }
        for (path, value) in &rule.params {
            if set_path_if_absent(body_obj, path, value.clone()) {
                mutated = true;
            }
        }
    }
    mutated
}

fn rule_matches(specs: &[PayloadRuleModelSpec], model_name: &str, protocol: Option<&str>) -> bool {
    specs.iter().any(|spec| {
        if !wildcard_match(&spec.name, model_name) {
            return false;
        }
        match spec.protocol.as_deref() {
            None => true,
            Some(want) => protocol.map(|got| got == want).unwrap_or(false),
        }
    })
}

/// Glob matcher: `*` matches any run of characters (including empty),
/// `?` matches a single character, everything else is literal. Matching
/// is case-insensitive — model names like `GPT-4.1` and `gpt-4.1` are
/// treated identically.
pub fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pat: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let val: Vec<char> = value.chars().flat_map(char::to_lowercase).collect();
    let (m, n) = (pat.len(), val.len());

    // Dynamic-programming match for `*` and `?`. `dp[i][j]` is true if
    // the first `i` pattern chars match the first `j` value chars.
    let mut dp = vec![vec![false; n + 1]; m + 1];
    dp[0][0] = true;
    for i in 1..=m {
        if pat[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }
    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = match pat[i - 1] {
                '*' => dp[i - 1][j] || dp[i][j - 1],
                '?' => dp[i - 1][j - 1],
                c => dp[i - 1][j - 1] && c == val[j - 1],
            };
        }
    }
    dp[m][n]
}

fn split_path(path: &str) -> Vec<String> {
    path.split('.')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn set_path(root: &mut Map<String, Value>, path: &str, value: Value) {
    let parts = split_path(path);
    if parts.is_empty() {
        return;
    }
    set_path_segments(root, &parts, value, /*only_if_absent=*/ false);
}

fn set_path_if_absent(root: &mut Map<String, Value>, path: &str, value: Value) -> bool {
    let parts = split_path(path);
    if parts.is_empty() {
        return false;
    }
    set_path_segments(root, &parts, value, /*only_if_absent=*/ true)
}

fn set_path_segments(
    root: &mut Map<String, Value>,
    parts: &[String],
    value: Value,
    only_if_absent: bool,
) -> bool {
    if parts.is_empty() {
        return false;
    }
    if parts.len() == 1 {
        let key = &parts[0];
        if only_if_absent && root.contains_key(key) {
            return false;
        }
        root.insert(key.clone(), value);
        return true;
    }
    let key = &parts[0];
    let child = root
        .entry(key.clone())
        .or_insert_with(|| Value::Object(Map::new()));
    if !child.is_object() {
        if only_if_absent {
            return false;
        }
        *child = Value::Object(Map::new());
    }
    set_path_segments(
        child.as_object_mut().expect("child is object"),
        &parts[1..],
        value,
        only_if_absent,
    )
}

fn remove_path(root: &mut Map<String, Value>, path: &str) -> bool {
    let parts = split_path(path);
    if parts.is_empty() {
        return false;
    }
    remove_path_segments(root, &parts)
}

fn remove_path_segments(root: &mut Map<String, Value>, parts: &[String]) -> bool {
    if parts.is_empty() {
        return false;
    }
    if parts.len() == 1 {
        return root.remove(&parts[0]).is_some();
    }
    let key = &parts[0];
    let Some(child) = root.get_mut(key) else {
        return false;
    };
    let Some(child_obj) = child.as_object_mut() else {
        return false;
    };
    remove_path_segments(child_obj, &parts[1..])
}

/// System-prompt override. Modeled after OmniRoute's
/// `api/settings/system-prompt`. Two modes:
///
/// * `Off` — do nothing.
/// * `Prepend` — insert the override as the first `system` message if no
///   system message exists yet, otherwise leave the caller's prompt alone.
/// * `Override` — replace any existing system message with the override.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SystemPromptMode {
    #[default]
    Off,
    Prepend,
    Override,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SystemPromptConfig {
    #[serde(default)]
    pub mode: SystemPromptMode,
    #[serde(default)]
    pub content: String,
}

impl SystemPromptConfig {
    pub fn is_active(&self) -> bool {
        !matches!(self.mode, SystemPromptMode::Off) && !self.content.trim().is_empty()
    }

    pub fn normalize(&mut self) {
        self.content = self.content.trim().to_string();
        if self.content.is_empty() {
            self.mode = SystemPromptMode::Off;
        }
    }
}

/// Apply the system-prompt override to an OpenAI-style chat completions
/// body. Mutates `body["messages"]` in place. Returns `true` if any
/// modification was made.
pub fn apply_system_prompt(body: &mut Value, config: &SystemPromptConfig) -> bool {
    if !config.is_active() {
        return false;
    }
    let Some(obj) = body.as_object_mut() else {
        return false;
    };

    // OpenAI-style `messages` array of objects with `role` + `content`.
    if let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) {
        let has_system = messages
            .iter()
            .any(|m| m.get("role").and_then(Value::as_str) == Some("system"));

        match config.mode {
            SystemPromptMode::Off => return false,
            SystemPromptMode::Prepend => {
                if !has_system {
                    messages.insert(
                        0,
                        serde_json::json!({ "role": "system", "content": config.content.clone() }),
                    );
                    return true;
                }
                return false;
            }
            SystemPromptMode::Override => {
                let mut replaced = false;
                let mut first_system_seen = false;
                messages.retain_mut(|m| {
                    let is_system = m.get("role").and_then(Value::as_str) == Some("system");
                    if !is_system {
                        return true;
                    }
                    if first_system_seen {
                        replaced = true;
                        return false;
                    }
                    first_system_seen = true;
                    if let Some(map) = m.as_object_mut() {
                        map.insert("content".to_string(), Value::String(config.content.clone()));
                    }
                    replaced = true;
                    true
                });
                if !first_system_seen {
                    messages.insert(
                        0,
                        serde_json::json!({ "role": "system", "content": config.content.clone() }),
                    );
                    replaced = true;
                }
                return replaced;
            }
        }
    }

    // Anthropic-style top-level `system` field. (Not always present
    // because openproxy mostly sees OpenAI-format requests, but worth
    // handling for the Anthropic-compat endpoint.)
    let system_field = obj.get("system");
    match config.mode {
        SystemPromptMode::Prepend => {
            if system_field.is_none() {
                obj.insert("system".to_string(), Value::String(config.content.clone()));
                return true;
            }
            false
        }
        SystemPromptMode::Override => {
            obj.insert("system".to_string(), Value::String(config.content.clone()));
            true
        }
        SystemPromptMode::Off => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn wildcard_matches_literal_and_glob() {
        assert!(wildcard_match("gpt-4", "gpt-4"));
        assert!(wildcard_match("gpt-*", "gpt-4o"));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("claude-?", "claude-3"));
        assert!(wildcard_match("GPT-4*", "gpt-4-turbo")); // case-insensitive
        assert!(!wildcard_match("gpt-4", "gpt-3"));
        assert!(!wildcard_match("claude-?", "claude-3.5"));
    }

    #[test]
    fn default_only_fills_missing() {
        let cfg = PayloadRulesConfig {
            default: vec![PayloadMutationRule {
                models: vec![PayloadRuleModelSpec {
                    name: "gpt-*".into(),
                    protocol: None,
                }],
                params: serde_json::from_value(json!({ "temperature": 0.2 })).unwrap(),
            }],
            ..Default::default()
        };

        let mut body = json!({ "model": "gpt-4", "messages": [] });
        apply_request_rules(&mut body, "gpt-4", None, &cfg);
        assert_eq!(body["temperature"], json!(0.2));

        let mut body2 = json!({ "model": "gpt-4", "temperature": 0.9 });
        apply_request_rules(&mut body2, "gpt-4", None, &cfg);
        assert_eq!(
            body2["temperature"],
            json!(0.9),
            "default must not overwrite"
        );
    }

    #[test]
    fn override_forces_value() {
        let cfg = PayloadRulesConfig {
            r#override: vec![PayloadMutationRule {
                models: vec![PayloadRuleModelSpec {
                    name: "o1*".into(),
                    protocol: Some("openai".into()),
                }],
                params: serde_json::from_value(json!({ "reasoning_effort": "medium" })).unwrap(),
            }],
            ..Default::default()
        };

        let mut body = json!({ "model": "o1-mini", "reasoning_effort": "high" });
        let changed = apply_request_rules(&mut body, "o1-mini", Some("openai"), &cfg);
        assert!(changed);
        assert_eq!(body["reasoning_effort"], json!("medium"));
    }

    #[test]
    fn protocol_gating() {
        let cfg = PayloadRulesConfig {
            r#override: vec![PayloadMutationRule {
                models: vec![PayloadRuleModelSpec {
                    name: "*".into(),
                    protocol: Some("anthropic".into()),
                }],
                params: serde_json::from_value(json!({ "max_tokens": 1024 })).unwrap(),
            }],
            ..Default::default()
        };

        let mut body = json!({ "model": "claude-3", "max_tokens": 8 });
        apply_request_rules(&mut body, "claude-3", Some("openai"), &cfg);
        assert_eq!(
            body["max_tokens"],
            json!(8),
            "different protocol must not match"
        );

        apply_request_rules(&mut body, "claude-3", Some("anthropic"), &cfg);
        assert_eq!(body["max_tokens"], json!(1024));
    }

    #[test]
    fn filter_strips_dot_path() {
        let cfg = PayloadRulesConfig {
            filter: vec![PayloadFilterRule {
                models: vec![PayloadRuleModelSpec {
                    name: "*".into(),
                    protocol: None,
                }],
                params: vec!["metadata.user_id".into()],
            }],
            ..Default::default()
        };

        let mut body = json!({
            "model": "gpt-4",
            "metadata": { "user_id": "u123", "session": "s9" }
        });
        apply_request_rules(&mut body, "gpt-4", None, &cfg);
        assert!(body["metadata"].get("user_id").is_none());
        assert_eq!(body["metadata"]["session"], json!("s9"));
    }

    #[test]
    fn system_prompt_prepend_adds_only_when_missing() {
        let cfg = SystemPromptConfig {
            mode: SystemPromptMode::Prepend,
            content: "You are helpful.".into(),
        };

        let mut body = json!({ "messages": [{ "role": "user", "content": "hi" }] });
        apply_system_prompt(&mut body, &cfg);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "You are helpful.");

        let mut body2 = json!({
            "messages": [
                { "role": "system", "content": "Caller's prompt" },
                { "role": "user", "content": "hi" }
            ]
        });
        apply_system_prompt(&mut body2, &cfg);
        assert_eq!(body2["messages"][0]["content"], "Caller's prompt");
    }

    #[test]
    fn system_prompt_override_replaces() {
        let cfg = SystemPromptConfig {
            mode: SystemPromptMode::Override,
            content: "Forced prompt.".into(),
        };

        let mut body = json!({
            "messages": [
                { "role": "system", "content": "Old prompt" },
                { "role": "system", "content": "Second old" },
                { "role": "user", "content": "hi" }
            ]
        });
        apply_system_prompt(&mut body, &cfg);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["content"], "Forced prompt.");
        assert_eq!(messages[1]["role"], "user");
    }

    #[test]
    fn nested_set_path_creates_intermediate_objects() {
        let cfg = PayloadRulesConfig {
            r#override: vec![PayloadMutationRule {
                models: vec![PayloadRuleModelSpec {
                    name: "*".into(),
                    protocol: None,
                }],
                params: serde_json::from_value(json!({ "stream_options.include_usage": true }))
                    .unwrap(),
            }],
            ..Default::default()
        };

        let mut body = json!({ "model": "gpt-4" });
        apply_request_rules(&mut body, "gpt-4", None, &cfg);
        assert_eq!(body["stream_options"]["include_usage"], json!(true));
    }

    #[test]
    fn normalize_drops_empty_rules() {
        let mut cfg = PayloadRulesConfig {
            default: vec![PayloadMutationRule {
                models: vec![PayloadRuleModelSpec {
                    name: "  ".into(),
                    protocol: None,
                }],
                params: Default::default(),
            }],
            r#override: vec![PayloadMutationRule {
                models: vec![PayloadRuleModelSpec {
                    name: "gpt-*".into(),
                    protocol: None,
                }],
                params: serde_json::from_value(json!({ "temperature": 0.1 })).unwrap(),
            }],
            ..Default::default()
        };
        cfg.normalize();
        assert!(cfg.default.is_empty());
        assert_eq!(cfg.r#override.len(), 1);
    }

    // Belt-and-braces: deserialize / re-serialize a representative
    // payload-rules JSON and make sure the shape round-trips so future
    // edits don't silently drop fields.
    #[test]
    fn deserialize_omniroute_style_config() {
        let raw = json!({
            "default": [
                { "models": [{"name": "gpt-4*"}], "params": { "temperature": 0.2 } }
            ],
            "override": [
                { "models": [{"name": "o1*", "protocol": "openai"}],
                  "params": { "reasoning_effort": "medium", "max_tokens": 4096 } }
            ],
            "filter": [
                { "models": [{"name": "claude-*"}], "params": ["metadata.user_id"] }
            ],
            "defaultRaw": [
                { "models": [{"name": "*"}], "params": { "stream_options": { "include_usage": true } } }
            ]
        });
        let cfg: PayloadRulesConfig = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(cfg.default.len(), 1);
        assert_eq!(cfg.r#override.len(), 1);
        assert_eq!(cfg.filter.len(), 1);
        assert_eq!(cfg.default_raw.len(), 1);
        let back = serde_json::to_value(&cfg).unwrap();
        assert_eq!(back["default"][0]["params"]["temperature"], json!(0.2));
        assert_eq!(back["defaultRaw"][0]["models"][0]["name"], json!("*"));
    }
}
