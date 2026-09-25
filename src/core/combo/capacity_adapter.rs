//! Capacity adapter — 9router `open-sse/services/capacityAdapter.js` parity.
//!
//! When a request needs capabilities (vision/pdf/audioInput/videoInput) that
//! none of the combo members support, the adapter prepends models from the
//! per-capability pools configured in `settings.capacityAdapter`. Models that
//! were *added* by the adapter get their history stripped to fit the model's
//! context window before dispatch (they are typically small free models).

use std::collections::HashSet;

use serde_json::Value;

/// Hard capabilities that a model must support for a request; the adapter
/// only ever considers these when deciding whether to augment.
pub const CAPABILITY_KEYS: [&str; 4] = ["vision", "pdf", "audioInput", "videoInput"];
/// 9router default model used when an enabled pool has no explicit models.
pub const DEFAULT_FALLBACK_MODEL: &str = "oc/mimo-v2.5-free";

/// Context-window default when the target model has no known window
/// (9router: `contextWindow || 200000`).
const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;
/// 9router keeps the first 6 older messages verbatim (`HEAD_KEEP = 6`).
const HEAD_KEEP: usize = 6;
/// 9router converts the budget to characters at 4 chars/token.
const CHARS_PER_TOKEN: u64 = 4;
/// 9router only uses 80% of the context window for history.
const HEADROOM_FACTOR: f64 = 0.8;

/// Normalized view of one capability pool entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapEntry {
    pub enabled: bool,
    pub round_robin: bool,
    pub models: Vec<String>,
}

impl CapEntry {
    fn disabled() -> Self {
        Self {
            enabled: false,
            round_robin: false,
            models: Vec::new(),
        }
    }
}

/// Normalize one `capacityAdapter` capability entry (9router
/// `normalizeCapEntry`). Accepts either the array-form
/// (`[{ model }]` or `["model"]` — enabled with no round-robin) or the
/// object-form (`{ enabled, roundRobin, models }`); anything else is
/// treated as disabled.
fn normalize_cap_entry(entry: &Value) -> CapEntry {
    match entry {
        Value::Array(items) => {
            let models = items
                .iter()
                .filter_map(|item| {
                    item.get("model")
                        .and_then(Value::as_str)
                        .or_else(|| item.as_str())
                        .map(str::to_string)
                })
                .collect();
            CapEntry {
                enabled: true,
                round_robin: false,
                models,
            }
        }
        Value::Object(obj) => {
            let enabled = obj.get("enabled").and_then(Value::as_bool).unwrap_or(true);
            let round_robin = obj
                .get("roundRobin")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let models = obj
                .get("models")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| m.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            CapEntry {
                enabled,
                round_robin,
                models,
            }
        }
        _ => CapEntry::disabled(),
    }
}

/// Resolve a capability pool from settings, applying the 9router fallback:
/// an enabled pool with no explicit models gets `[DEFAULT_FALLBACK_MODEL]`.
fn get_capacity_adapter_config(cap: &str, settings: &Value) -> CapEntry {
    let entry = settings
        .get(cap)
        .map(normalize_cap_entry)
        .unwrap_or_else(CapEntry::disabled);

    if entry.enabled && entry.models.is_empty() {
        return CapEntry {
            enabled: true,
            round_robin: entry.round_robin,
            models: vec![DEFAULT_FALLBACK_MODEL.to_string()],
        };
    }
    entry
}

/// Flatten all enabled pools in `CAPABILITY_KEYS` order, deduped
/// order-preserving (9router `getCapacityAdapterModels`).
pub fn get_capacity_adapter_models(settings: &Value) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for cap in CAPABILITY_KEYS {
        let cfg = get_capacity_adapter_config(cap, settings);
        if !cfg.enabled {
            continue;
        }
        for model in cfg.models {
            if seen.insert(model.clone()) {
                out.push(model);
            }
        }
    }
    out
}

/// Strategy for one capability pool: `enabled && roundRobin` →
/// `"round-robin"`, otherwise `"fallback"` (9router
/// `getCapacityAdapterStrategy`).
pub fn get_capacity_adapter_strategy(cap: &str, settings: &Value) -> &'static str {
    let entry = get_capacity_adapter_config(cap, settings);
    if entry.enabled && entry.round_robin {
        "round-robin"
    } else {
        "fallback"
    }
}

/// Strategy for the adapter on a request: the first hard capability present
/// in `required` that has an enabled non-empty pool decides (9router
/// `getActiveAdapterStrategy`). Empty pools (which fall back to the default
/// model) still count as non-empty — that is the 9router behavior.
pub fn get_active_adapter_strategy(required: &HashSet<String>, settings: &Value) -> &'static str {
    for cap in CAPABILITY_KEYS {
        if !required.contains(cap) {
            continue;
        }
        let entry = get_capacity_adapter_config(cap, settings);
        if entry.enabled && !entry.models.is_empty() {
            return if entry.round_robin {
                "round-robin"
            } else {
                "fallback"
            };
        }
    }
    "fallback"
}

/// Does `model_str` (e.g. `"openai/gpt-4o"`) satisfy all hard caps?
fn model_satisfies(model_str: &str, required_hard: &HashSet<String>) -> bool {
    if required_hard.is_empty() {
        return true;
    }
    required_hard
        .iter()
        .all(|cap| model_has_capability(model_str, cap))
}

/// Augment a model list with capacity-adapter models when none of the
/// original models satisfies every hard requirement (9router
/// `augmentModelsWithCapacityAdapter`).
///
/// Returns the original list unchanged when there is nothing hard to
/// satisfy, the list is empty, or any original model already satisfies the
/// caps. Otherwise prepends (in `CAPABILITY_KEYS` order) every pool model
/// that satisfies the caps and is not already in the list.
pub fn augment_models_with_capacity_adapter(
    models: &[String],
    required: &HashSet<String>,
    settings: &Value,
) -> Vec<String> {
    if required.is_empty() || models.is_empty() {
        return models.to_vec();
    }
    if models.iter().any(|m| model_satisfies(m, required)) {
        return models.to_vec();
    }

    let mut adapter_models: Vec<String> = Vec::new();
    for cap in CAPABILITY_KEYS {
        if !required.contains(cap) {
            continue;
        }
        for model in get_capacity_adapter_config(cap, settings).models {
            if !models.iter().any(|m| m == &model)
                && model_satisfies(&model, required)
                && !adapter_models.iter().any(|m| m == &model)
            {
                adapter_models.push(model);
            }
        }
    }

    let mut out = adapter_models;
    out.extend(models.iter().cloned());
    out
}

/// Estimate the character length of one message for budget purposes
/// (9router `blockLength`): string content counts its length; array content
/// sums `text` lengths with a fallback of 50 chars per block.
fn block_length(msg: &Value) -> usize {
    let content = msg.get("content");
    let text = msg
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| content.and_then(Value::as_str));
    if let Some(text) = text {
        return text.chars().count();
    }
    if let Some(arr) = content.and_then(Value::as_array) {
        let mut total = 0;
        for block in arr {
            total += block
                .get("text")
                .and_then(Value::as_str)
                .map(|s| s.chars().count())
                .unwrap_or(50);
        }
        return total;
    }
    0
}

/// Character budget from a context window: `(window || 200000) * 0.8 * 4`.
fn budget_chars(context_window: Option<u64>) -> usize {
    let window = context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW);
    (window as f64 * HEADROOM_FACTOR * CHARS_PER_TOKEN as f64) as usize
}

/// Strip message history to fit a model's context window (9router
/// `stripHistoryForContext`). Operates on the `messages`, `input`, or
/// `contents` key. System/developer messages are always kept; the trailing
/// user turn (everything after the last assistant/model message) is kept in
/// full; from the older middle, the first `HEAD_KEEP` messages are kept
/// verbatim and the rest are dropped from the oldest end until the budget
/// fits. Returns `true` when `body` was mutated.
pub fn strip_history_for_context(body: &mut Value, context_window: Option<u64>) -> bool {
    let key = ["messages", "input", "contents"]
        .into_iter()
        .find(|k| body.get(*k).and_then(Value::as_array).is_some());

    let Some(key) = key else {
        return false;
    };
    let Some(messages) = body.get_mut(key).and_then(|v| v.as_array_mut()) else {
        return false;
    };

    if messages.len() <= 1 {
        return false;
    }

    // System/developer messages are pinned to the front.
    let split_at = messages
        .iter()
        .position(|msg| {
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
            role != "system" && role != "developer"
        })
        .unwrap_or(messages.len());

    // Everything after the last assistant/model turn is the trailing tail —
    // the current user turn (with any attachments) must never be stripped.
    let tail_start = messages
        .iter()
        .rposition(|msg| {
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
            role == "assistant" || role == "model"
        })
        .map(|pos| pos + 1)
        .unwrap_or(split_at);

    if tail_start <= split_at {
        return false;
    }

    let system_msgs: Vec<Value> = messages.drain(..split_at).collect();
    let tail: Vec<Value> = messages.drain(tail_start - split_at..).collect();

    // Budget covers everything below the system/developer block.
    let budget = budget_chars(context_window);
    let tail_len: usize = tail.iter().map(block_length).sum();

    let keep = budget.saturating_sub(tail_len);

    // Keep the first HEAD_KEEP older messages verbatim, then fill from the
    // end of the head (oldest dropped first) until the budget is exhausted.
    let head_keep = messages.len().min(HEAD_KEEP);
    let mut head: Vec<Value> = messages.drain(..head_keep).collect();

    let mut head_len: usize = head.iter().map(block_length).sum();
    while head_len > keep && !head.is_empty() {
        head_len = head_len.saturating_sub(block_length(head.first().unwrap()));
        head.remove(0);
    }

    *messages = Vec::new();
    messages.extend(system_msgs);
    messages.extend(head);
    messages.extend(tail);
    true
}

/// True when `entry` supports `capability`.
///
/// Answers from the real resolver — `get_capabilities_for_model` — the way
/// 9router's `reorderByCapabilities` does, splitting the entry into provider
/// and model. The old code matched provider prefixes and substrings of the
/// model name, which cannot see a synced-catalog override and cannot see a
/// Gemini-native body at all: a capability carried by a
/// `contents[].parts[].inlineData` block is invisible to a name pattern.
///
/// The prefix heuristic survives as a fallback for entries the catalog does
/// not know, so an unlisted model is not silently demoted to "has nothing"
/// and dropped out of a rotation it used to qualify for.
fn model_has_capability(entry: &str, capability: &str) -> bool {
    let (provider, model) = match entry.split_once('/') {
        Some((provider, model)) => (provider, model),
        None => ("", entry),
    };
    let caps = crate::core::combo::capabilities::get_capabilities_for_model(provider, model);

    let from_catalog = match capability {
        "vision" => caps.vision,
        "pdf" => caps.pdf,
        "audioInput" => caps.audio_input,
        "videoInput" => caps.video_input,
        "imageOutput" => caps.image_output,
        "audioOutput" => caps.audio_output,
        "search" => caps.search,
        "tools" => caps.tools,
        "reasoning" => caps.reasoning,
        _ => false,
    };
    if from_catalog {
        return true;
    }

    prefix_heuristic(entry, capability)
}

/// The previous prefix/substring table, kept for entries the catalog does not
/// resolve.
fn prefix_heuristic(entry: &str, capability: &str) -> bool {
    let entry_lower = entry.to_lowercase();

    match capability {
        "vision" => {
            if entry_lower.starts_with("openai/o1")
                || entry_lower.starts_with("openai/o3")
                || entry_lower.starts_with("anthropic/claude")
                || entry_lower.starts_with("google/gemini")
                || entry_lower.starts_with("vertex/claude")
                || entry_lower.starts_with("vertex/gemini")
                || entry_lower.starts_with("aws/claude")
                || entry_lower.starts_with("gcp/gemini")
                || entry_lower.starts_with("custom/node-openai")
            {
                return true;
            }
            entry_lower.contains("vision")
                || entry_lower.contains("-4o")
                || entry_lower.contains("gemini")
                || entry_lower.starts_with("oc/mimo")
        }
        "pdf" => {
            entry_lower.starts_with("anthropic/claude")
                || entry_lower.starts_with("vertex/claude")
                || entry_lower.starts_with("aws/claude")
                || entry_lower.starts_with("google/gemini")
                || entry_lower.starts_with("vertex/gemini")
                || entry_lower.starts_with("gcp/gemini")
        }
        "audioInput" | "videoInput" => entry_lower.starts_with("oc/mimo"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn capacity_adapter_augments_only_when_original_lacks_cap() {
        let settings = json!({
            "vision": {"enabled": true, "roundRobin": false, "models": ["oc/mimo-v2.5-free"]}
        });

        // openai/gpt-4 does NOT satisfy vision (gpt-4 < 4o), so the pool
        // model is prepended.
        let augmented = augment_models_with_capacity_adapter(
            &["openai/gpt-4".to_string()],
            &HashSet::from(["vision".to_string()]),
            &settings,
        );
        assert_eq!(augmented, vec!["oc/mimo-v2.5-free", "openai/gpt-4"]);

        // anthropic/claude satisfies vision, so nothing is added.
        let unchanged = augment_models_with_capacity_adapter(
            &["anthropic/claude".to_string()],
            &HashSet::from(["vision".to_string()]),
            &settings,
        );
        assert_eq!(unchanged, vec!["anthropic/claude"]);
    }

    #[test]
    fn capacity_adapter_strip_history_keeps_system_and_tail() {
        let mut body = json!({
            "model": "test",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "old user 1"},
                {"role": "assistant", "content": "old assistant 1"},
                {"role": "user", "content": "old user 2"},
                {"role": "assistant", "content": "old assistant 2"},
                {"role": "user", "content": "old user 3"},
                {"role": "assistant", "content": "old assistant 3"},
                {"role": "user", "content": "old user 4"},
                {"role": "assistant", "content": "old assistant 4"},
                {"role": "user", "content": "current turn with image", "image_url": {"url": "https://example.com/img.png"}}
            ]
        });

        assert!(strip_history_for_context(&mut body, Some(200_000)));

        let messages = body["messages"].as_array().unwrap();
        // System message retained at the front.
        assert_eq!(messages[0]["role"], "system");
        // Trailing user turn (with the image) retained in full.
        let last = messages.last().unwrap();
        assert_eq!(last["role"], "user");
        assert!(last.get("image_url").is_some());
        assert!(last["content"].as_str().unwrap().contains("current turn"));
        // History is reduced: system + first HEAD_KEEP older messages + the
        // trailing turn (9router keeps HEAD_KEEP=6 older messages verbatim).
        assert_eq!(messages.len(), 8);
        assert_eq!(messages[1]["content"], "old user 1");
    }

    #[test]
    fn capacity_adapter_strip_history_respects_small_budget() {
        let mut body = json!({
            "messages": [
                {"role": "system", "content": "s"},
                {"role": "user", "content": "aaaaaaaaaa"},
                {"role": "assistant", "content": "bbbbbbbbbb"},
                {"role": "user", "content": "cccccccccc"},
                {"role": "assistant", "content": "dddddddddd"},
                {"role": "user", "content": "trailing"}
            ]
        });

        // Window of 1 token -> budget of 3 chars: head must shrink hard.
        assert!(strip_history_for_context(&mut body, Some(1)));

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert!(messages
            .iter()
            .any(|m| m["role"] == "user" && m["content"] == "trailing"));
        // The trailing user turn survives; older turns are trimmed.
        assert!(messages.len() < 6);
    }

    #[test]
    fn normalize_accepts_array_and_object_forms() {
        let arr = json!([{"model": "oc/mimo-v2.5-free"}, "openai/gpt-4o"]);
        let entry = normalize_cap_entry(&arr);
        assert!(entry.enabled);
        assert!(!entry.round_robin);
        assert_eq!(
            entry.models,
            vec!["oc/mimo-v2.5-free".to_string(), "openai/gpt-4o".to_string()]
        );

        let obj = json!({"enabled": false, "roundRobin": true, "models": ["x"]});
        let entry = normalize_cap_entry(&obj);
        assert!(!entry.enabled);
        assert!(entry.round_robin);
        assert_eq!(entry.models, vec!["x".to_string()]);

        assert_eq!(normalize_cap_entry(&Value::Null), CapEntry::disabled());
    }

    #[test]
    fn enabled_empty_pool_falls_back_to_default_model() {
        let settings = json!({"vision": {"enabled": true, "roundRobin": true, "models": []}});
        let cfg = get_capacity_adapter_config("vision", &settings);
        assert!(cfg.enabled);
        assert!(cfg.round_robin);
        assert_eq!(cfg.models, vec![DEFAULT_FALLBACK_MODEL]);

        // Flattened pool carries the fallback model too.
        assert_eq!(
            get_capacity_adapter_models(&settings),
            vec![DEFAULT_FALLBACK_MODEL]
        );
    }

    #[test]
    fn adapter_models_dedup_order_preserving() {
        let settings = json!({
            "vision": {"enabled": true, "models": ["oc/mimo-v2.5-free", "openai/gpt-4o"]},
            "pdf": {"enabled": true, "models": ["openai/gpt-4o"]},
            "audioInput": {"enabled": true, "models": ["oc/mimo-v2.5-free"]},
            "videoInput": {"enabled": false, "models": ["nope"]}
        });
        assert_eq!(
            get_capacity_adapter_models(&settings),
            vec!["oc/mimo-v2.5-free", "openai/gpt-4o"]
        );
    }

    #[test]
    fn active_strategy_uses_first_satisfying_cap() {
        // vision is round-robin; pdf is fallback.
        let settings = json!({
            "vision": {"enabled": true, "roundRobin": true, "models": ["oc/mimo-v2.5-free"]},
            "pdf": {"enabled": true, "roundRobin": false, "models": ["openai/gpt-4o"]}
        });

        // Required {vision} -> round-robin (first satisfying cap).
        let required_vision = HashSet::from(["vision".to_string()]);
        assert_eq!(
            get_active_adapter_strategy(&required_vision, &settings),
            "round-robin"
        );

        // Required {pdf} -> fallback.
        let required_pdf = HashSet::from(["pdf".to_string()]);
        assert_eq!(
            get_active_adapter_strategy(&required_pdf, &settings),
            "fallback"
        );

        // Required {videoInput} with no pool -> fallback.
        let required_video = HashSet::from(["videoInput".to_string()]);
        assert_eq!(
            get_active_adapter_strategy(&required_video, &settings),
            "fallback"
        );

        // No requirements -> fallback.
        assert_eq!(
            get_active_adapter_strategy(&HashSet::new(), &settings),
            "fallback"
        );
    }
}

#[cfg(test)]
mod capability_classifier_tests {
    use super::*;
    use std::collections::HashSet;

    /// openproxy-xhs9: the reorder used a ~10-entry provider-prefix and
    /// model-substring table. A vision-only member has to float above one that
    /// has no vision at all, or an image-bearing request tries the wrong model
    /// first.
    #[test]
    fn vision_member_sorts_above_non_vision_member() {
        let required: HashSet<String> = ["vision".to_string()].into_iter().collect();
        let models = vec![
            "openai/gpt-3.5-turbo".to_string(),
            "openai/gpt-4o".to_string(),
        ];

        let ordered = crate::core::combo::reorder_by_capabilities(&models, &required);

        assert_eq!(
            ordered[0], "openai/gpt-4o",
            "the vision-capable member must lead: {ordered:?}"
        );
    }

    /// A member the catalog does not know must not be silently demoted to
    /// "supports nothing" — it would fall to the back of every rotation it
    /// used to qualify for.
    #[test]
    fn unknown_entry_keeps_its_prefix_heuristic_answer() {
        // Not in any capability table, but the prefix heuristic has always
        // answered true for this one.
        assert!(super::model_has_capability("oc/mimo-v2.5-free", "vision"));
    }

    /// The classifier must agree with the real resolver for a model the catalog
    /// DOES know — that is the whole point of the swap.
    #[test]
    fn classifier_agrees_with_the_catalog_resolver() {
        use crate::core::combo::capabilities::get_capabilities_for_model;
        for (provider, model) in [
            ("openai", "gpt-4o"),
            ("openai", "gpt-3.5-turbo"),
            ("anthropic", "claude-opus-4.7"),
        ] {
            let caps = get_capabilities_for_model(provider, model);
            let entry = format!("{provider}/{model}");
            assert_eq!(
                super::model_has_capability(&entry, "vision"),
                caps.vision,
                "classifier disagrees with the catalog for {entry}"
            );
        }
    }

    /// The bead also asks that a Gemini-native body be readable by the reorder.
    /// inlineData/fileData are already inspected by the detector; this pins it
    /// so a regression there is visible here too.
    #[test]
    fn gemini_native_image_body_requires_vision() {
        let body = serde_json::json!({
            "contents": [{
                "role": "user",
                "parts": [{"inlineData": {"mimeType": "image/png", "data": "AAAA"}}]
            }]
        });
        let required = crate::core::combo::detect_required_capabilities(&body);
        assert!(
            required.contains("vision"),
            "a Gemini inlineData image must register as a vision requirement: {required:?}"
        );
    }
}
