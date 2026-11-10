//! Smart Router combo strategy — port of 9router
//! `open-sse/services/systemoneRouter.js` (pure helpers + sticky session
//! store) and the `comboStrategy === "smart"` ordering block of
//! `open-sse/services/combo.js:372-448`.
//!
//! The strategy asks a System One decision model (default
//! `oc/jev-1.13-free`, 8s ceiling) which configured combo member best fits
//! the request, then orders the member list accordingly. Session stickiness
//! keeps the chosen model warm for prompt-cache retention under the
//! "smart"/"strict"/"custom" policies; "per-turn" (or `sessionSticky:
//! false`) re-evaluates every request.
//!
//! Everything here fails open: a missing router credential, timeout, or
//! unparsable answer yields `None` and the caller keeps the configured
//! order — upstream `resolveSmartChoice` never throws for the same reason.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use serde_json::Value;

pub const DEFAULT_SYSTEMONE_ROUTER_MODEL: &str = "oc/jev-1.13-free";
pub const DEFAULT_SYSTEMONE_TIMEOUT_MS: u64 = 8000;
pub const MAX_STATE_CHARS: usize = 2000;
pub const MAX_STICKY_SESSIONS: usize = 5000;
pub const STICKY_SESSION_TTL: Duration = Duration::from_secs(2 * 60 * 60);

// ── Prompt state extraction ────────────────────────────────────────────

/// `extractPromptState` — last user turn (or `prompt`/`input` string),
/// truncated to `max_chars`.
pub fn extract_prompt_state(body: &Value, max_chars: usize) -> String {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .or_else(|| body.get("input").and_then(Value::as_array));

    if let Some(messages) = messages {
        if !messages.is_empty() {
            // Latest user message first.
            for msg in messages.iter().rev() {
                let is_user = msg.get("role").and_then(Value::as_str) == Some("user")
                    || msg.get("type").and_then(Value::as_str) == Some("message")
                    || (msg.get("role").is_none() && msg.get("content").is_some());
                if is_user {
                    let text = extract_text_from_content(
                        msg.get("content")
                            .or_else(|| msg.get("text"))
                            .unwrap_or(&Value::Null),
                    );
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        return truncate_chars(trimmed, max_chars);
                    }
                }
            }
            // Fallback: latest message of any role.
            if let Some(last) = messages.last() {
                let text = extract_text_from_content(
                    last.get("content")
                        .or_else(|| last.get("text"))
                        .unwrap_or(&Value::Null),
                );
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return truncate_chars(trimmed, max_chars);
                }
            }
        }
    }

    for key in ["prompt", "input"] {
        if let Some(s) = body.get(key).and_then(Value::as_str) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return truncate_chars(trimmed, max_chars);
            }
        }
    }
    String::new()
}

fn extract_text_from_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|part| match part {
                Value::String(s) => s.clone(),
                Value::Object(_) => part
                    .get("text")
                    .or_else(|| part.get("output"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                _ => String::new(),
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

// ── Choice criteria / answer parsing ───────────────────────────────────

/// `buildChoiceCriteria` — model ids as keys, `null` values (System One
/// uses native model knowledge; no hand-written descriptions).
pub fn build_choice_criteria(models: &[String]) -> BTreeMap<String, Value> {
    let mut criteria = BTreeMap::new();
    for m in models {
        let trimmed = m.trim();
        if !trimmed.is_empty() {
            criteria.insert(trimmed.to_string(), Value::Null);
        }
    }
    criteria
}

/// Parsed System One route answer.
#[derive(Debug, Clone)]
pub struct SmartChoice {
    pub choice: String,
    pub confidence: Option<f64>,
    pub probabilities: Option<BTreeMap<String, f64>>,
}

/// `parseSystemOneRouteAnswer` — `answers.route.{choice,confidence,
/// probabilities}`.
pub fn parse_systemone_route_answer(json: &Value) -> Option<SmartChoice> {
    let route = json.get("answers")?.get("route")?;
    if !route.is_object() {
        return None;
    }
    let choice = route
        .get("choice")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let confidence = route
        .get("confidence")
        .and_then(Value::as_f64)
        .filter(|c| c.is_finite());
    let probabilities = route
        .get("probabilities")
        .filter(|p| p.is_object())
        .and_then(|p| p.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| {
                    v.as_f64()
                        .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
                        .filter(|n| n.is_finite())
                        .map(|n| (k.clone(), n))
                })
                .collect::<BTreeMap<_, _>>()
        });
    Some(SmartChoice {
        choice,
        confidence,
        probabilities,
    })
}

/// `orderModelsByChoice` — selected model first, remaining models ordered
/// by probability descending (stable for ties / missing entries).
pub fn order_models_by_choice(models: &[String], choice: &SmartChoice) -> Vec<String> {
    if models.len() <= 1 || !models.iter().any(|m| m == &choice.choice) {
        return models.to_vec();
    }
    let mut others: Vec<String> = models
        .iter()
        .filter(|m| *m != &choice.choice)
        .cloned()
        .collect();
    if let Some(probs) = &choice.probabilities {
        // Stable sort by descending probability; missing → 0.
        others.sort_by(|a, b| {
            let pa = probs.get(a).copied().unwrap_or(0.0);
            let pb = probs.get(b).copied().unwrap_or(0.0);
            pb.partial_cmp(&pa).unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    let mut out = Vec::with_capacity(models.len());
    out.push(choice.choice.clone());
    out.extend(others);
    out
}

// ── Smart policy evaluation (combo.js:372-448) ─────────────────────────

/// Resolved smart-router knobs for one combo dispatch.
#[derive(Debug, Clone)]
pub struct SmartRouterConfig {
    /// `comboStrategies[name].routerModel` (default `oc/jev-1.13-free`).
    pub router_model: String,
    /// `comboStrategies[name].smartPolicy` ("smart" | "strict" |
    /// "per-turn" | "custom" | "smart-switch-70" | "smart-switch-90" …).
    pub smart_policy: String,
    /// `comboStrategies[name].sessionSticky !== false`.
    pub session_sticky: bool,
    /// `comboStrategies[name].confidenceThreshold` (default 0.8).
    pub confidence_threshold: f64,
}

impl Default for SmartRouterConfig {
    fn default() -> Self {
        Self {
            router_model: DEFAULT_SYSTEMONE_ROUTER_MODEL.to_string(),
            smart_policy: "smart".to_string(),
            session_sticky: true,
            confidence_threshold: 0.8,
        }
    }
}

/// Switch threshold per upstream policy resolution (combo.js:377-386).
pub fn switch_threshold(cfg: &SmartRouterConfig) -> f64 {
    let policy = cfg.smart_policy.to_lowercase();
    if policy == "custom" {
        if cfg.confidence_threshold.is_finite() && cfg.confidence_threshold > 0.0 {
            return cfg.confidence_threshold;
        }
        return 0.8;
    }
    if policy == "smart-switch-70" {
        return 0.7;
    }
    if policy == "smart-switch-90" {
        return 0.9;
    }
    if policy.starts_with("smart-switch") || policy == "smart" {
        if cfg.confidence_threshold.is_finite() && cfg.confidence_threshold > 0.0 {
            return cfg.confidence_threshold;
        }
    }
    0.8
}

/// Outcome of the smart-ordering pre-pass: the reordered member list.
/// `active_sticky_model` mirrors upstream for the zero-content sticky
/// clear (combo.js:509-511).
pub struct SmartOrdering {
    pub models: Vec<String>,
    pub active_sticky_model: Option<String>,
}

/// Evaluate the smart strategy and return the reordered model list.
///
/// `resolve_choice` is the injected System One call
/// (`(body, models) -> Option<SmartChoice>`); returning `None` anywhere
/// fails open to the configured order, exactly like upstream.
///
/// This is a synchronous-ordered port of combo.js:372-448 — the async
/// router call happens inside `resolve_choice` before this runs.
pub fn apply_smart_ordering(
    models: &[String],
    combo_name: &str,
    session_id: Option<&str>,
    cfg: &SmartRouterConfig,
    choice: Option<&SmartChoice>,
) -> SmartOrdering {
    let policy = cfg.smart_policy.to_lowercase();
    let is_per_turn = policy == "per-turn" || !cfg.session_sticky;
    let is_strict = policy == "strict";
    let threshold = switch_threshold(cfg);

    let mut active_sticky_model: Option<String> = None;
    if !is_per_turn {
        if let Some(sid) = session_id {
            active_sticky_model = get_sticky_combo_model(combo_name, sid, models);
        }
    }

    let mut rotated = models.to_vec();

    if let Some(sticky) = active_sticky_model.clone() {
        if is_strict {
            // Strict Lock: re-use the sticky model without a System One call.
            rotated = reorder_sticky_first(models, &sticky);
        } else if let Some(result) = choice {
            let new_choice = &result.choice;
            let conf = result.confidence;
            let should_switch = models.iter().any(|m| m == new_choice)
                && new_choice != &sticky
                && conf.map(|c| c >= threshold).unwrap_or(false);
            if should_switch {
                rotated = order_models_by_choice(models, result);
                if let Some(sid) = session_id {
                    set_sticky_combo_model(combo_name, sid, new_choice);
                    active_sticky_model = Some(new_choice.clone());
                }
            } else {
                // Stay on the warm model to preserve prompt cache.
                rotated = reorder_sticky_first(models, &sticky);
            }
        } else {
            // Router unavailable → retain the sticky model.
            rotated = reorder_sticky_first(models, &sticky);
        }
    } else if let Some(result) = choice {
        // Turn 1 or per-turn: evaluate once for the initial pick.
        if models.iter().any(|m| m == &result.choice) {
            let below =
                threshold > 0.0 && result.confidence.map(|c| c < threshold).unwrap_or(false);
            if below {
                // Keep configured order.
            } else {
                rotated = order_models_by_choice(models, result);
                if !is_per_turn {
                    if let Some(sid) = session_id {
                        set_sticky_combo_model(combo_name, sid, &result.choice);
                        active_sticky_model = Some(result.choice.clone());
                    }
                }
            }
        }
    }

    SmartOrdering {
        models: rotated,
        active_sticky_model,
    }
}

fn reorder_sticky_first(models: &[String], sticky: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(models.len());
    out.push(sticky.to_string());
    out.extend(models.iter().filter(|m| *m != sticky).cloned());
    out
}

/// Clear the sticky mapping for `(combo, session)` — upstream
/// `clearStickyComboSession` on a zero-content sticky response
/// (combo.js:509-511) and on explicit operator reset.
pub fn clear_sticky_combo_session(combo_name: &str, session_id: Option<&str>) {
    let mut store = STICKY_STORE.lock().unwrap_or_else(|e| e.into_inner());
    match session_id {
        Some(sid) => {
            store.remove(&(combo_name.to_string(), sid.to_string()));
        }
        None => {
            if combo_name.is_empty() {
                // Upstream `clearStickyComboSession()` with no arguments
                // resets the whole store (combo.js resetComboRotation).
                store.clear();
            } else {
                // Store keys are (combo, session) tuples — match the combo
                // exactly, not a string prefix (the combo half never
                // contains the session separator).
                store.retain(|(combo, _), _| combo != combo_name);
            }
        }
    }
}

/// Test hook — upstream `resetStickyComboStore`.
#[cfg(test)]
pub fn reset_sticky_combo_store() {
    STICKY_STORE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

// ── Sticky session store ───────────────────────────────────────────────

struct StickyEntry {
    model: String,
    last_used: Instant,
}

static STICKY_STORE: Lazy<Mutex<HashMap<(String, String), StickyEntry>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// `getStickyComboModel` — TTL-checked, membership-checked, refreshes
/// `lastUsed` on hit.
pub fn get_sticky_combo_model(
    combo_name: &str,
    session_id: &str,
    models: &[String],
) -> Option<String> {
    if session_id.is_empty() || combo_name.is_empty() || models.is_empty() {
        return None;
    }
    let mut store = STICKY_STORE.lock().unwrap_or_else(|e| e.into_inner());
    let key = (combo_name.to_string(), session_id.to_string());
    let entry = store.get(&key)?;
    if entry.last_used.elapsed() > STICKY_SESSION_TTL {
        store.remove(&key);
        return None;
    }
    if !models.iter().any(|m| m == &entry.model) {
        store.remove(&key);
        return None;
    }
    let model = entry.model.clone();
    if let Some(e) = store.get_mut(&key) {
        e.last_used = Instant::now();
    }
    Some(model)
}

/// `setStickyComboModel` — records the chosen model for the session.
pub fn set_sticky_combo_model(combo_name: &str, session_id: &str, model: &str) {
    if session_id.is_empty() || combo_name.is_empty() || model.is_empty() {
        return;
    }
    let mut store = STICKY_STORE.lock().unwrap_or_else(|e| e.into_inner());
    prune_sticky_sessions_locked(&mut store);
    store.insert(
        (combo_name.to_string(), session_id.to_string()),
        StickyEntry {
            model: model.to_string(),
            last_used: Instant::now(),
        },
    );
}

/// `pruneStickyComboSessions` — drop expired entries; if still at the
/// ceiling, drop the oldest quarter.
fn prune_sticky_sessions_locked(store: &mut HashMap<(String, String), StickyEntry>) {
    if store.len() < MAX_STICKY_SESSIONS {
        return;
    }
    store.retain(|_, e| e.last_used.elapsed() <= STICKY_SESSION_TTL);
    if store.len() >= MAX_STICKY_SESSIONS {
        let mut entries: Vec<((String, String), Instant)> = store
            .iter()
            .map(|(k, e)| (k.clone(), e.last_used))
            .collect();
        entries.sort_by_key(|(_, t)| *t);
        let drop_n = MAX_STICKY_SESSIONS / 4;
        for (key, _) in entries.into_iter().take(drop_n) {
            store.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_prompt_state_prefers_last_user_turn() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "first"},
                {"role": "assistant", "content": "reply"},
                {"role": "user", "content": [{"type": "text", "text": "second question"}]}
            ]
        });
        assert_eq!(
            extract_prompt_state(&body, MAX_STATE_CHARS),
            "second question"
        );
    }

    #[test]
    fn extract_prompt_state_falls_back_to_prompt_string() {
        let body = json!({"prompt": "  hello world  "});
        assert_eq!(extract_prompt_state(&body, MAX_STATE_CHARS), "hello world");
        assert_eq!(extract_prompt_state(&json!({}), MAX_STATE_CHARS), "");
    }

    #[test]
    fn extract_prompt_state_truncates() {
        let body = json!({"prompt": "x".repeat(3000)});
        assert_eq!(
            extract_prompt_state(&body, MAX_STATE_CHARS).len(),
            MAX_STATE_CHARS
        );
    }

    #[test]
    fn parse_route_answer_reads_choice_confidence_probabilities() {
        let json = json!({
            "answers": {"route": {
                "choice": "provider-a/model-1",
                "confidence": 0.91,
                "probabilities": {"provider-a/model-1": 0.9, "provider-b/model-2": "0.1"}
            }}
        });
        let choice = parse_systemone_route_answer(&json).unwrap();
        assert_eq!(choice.choice, "provider-a/model-1");
        assert_eq!(choice.confidence, Some(0.91));
        assert_eq!(
            choice
                .probabilities
                .as_ref()
                .unwrap()
                .get("provider-b/model-2"),
            Some(&0.1)
        );
        assert!(parse_systemone_route_answer(&json!({"answers": {}})).is_none());
        assert!(
            parse_systemone_route_answer(&json!({"answers": {"route": {"choice": "  "}}}))
                .is_none()
        );
    }

    #[test]
    fn order_models_by_choice_puts_choice_first_by_probability() {
        let models = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let choice = SmartChoice {
            choice: "c".to_string(),
            confidence: Some(0.9),
            probabilities: Some(
                [("a".to_string(), 0.2), ("b".to_string(), 0.8)]
                    .into_iter()
                    .collect(),
            ),
        };
        assert_eq!(
            order_models_by_choice(&models, &choice),
            vec!["c", "b", "a"]
        );
    }

    #[test]
    fn sticky_session_roundtrip_and_ttl_membership() {
        reset_sticky_combo_store();
        let models = vec!["m1".to_string(), "m2".to_string()];
        set_sticky_combo_model("combo", "sess", "m2");
        assert_eq!(
            get_sticky_combo_model("combo", "sess", &models),
            Some("m2".to_string())
        );
        // Model no longer in combo → cleared.
        assert_eq!(
            get_sticky_combo_model("combo", "sess", &["m1".to_string()]),
            None
        );
        clear_sticky_combo_session("combo", Some("sess"));
        assert_eq!(get_sticky_combo_model("combo", "sess", &models), None);
    }

    #[test]
    fn smart_switch_below_threshold_keeps_sticky() {
        reset_sticky_combo_store();
        let models = vec!["m1".to_string(), "m2".to_string()];
        set_sticky_combo_model("combo", "sess", "m1");
        let cfg = SmartRouterConfig::default(); // threshold 0.8
        let choice = SmartChoice {
            choice: "m2".to_string(),
            confidence: Some(0.5),
            probabilities: None,
        };
        let ordering = apply_smart_ordering(&models, "combo", Some("sess"), &cfg, Some(&choice));
        assert_eq!(ordering.models, vec!["m1", "m2"]);
        assert_eq!(ordering.active_sticky_model.as_deref(), Some("m1"));
    }

    #[test]
    fn smart_switch_above_threshold_switches_and_resticks() {
        reset_sticky_combo_store();
        let models = vec!["m1".to_string(), "m2".to_string()];
        set_sticky_combo_model("combo", "sess", "m1");
        let cfg = SmartRouterConfig::default();
        let choice = SmartChoice {
            choice: "m2".to_string(),
            confidence: Some(0.95),
            probabilities: None,
        };
        let ordering = apply_smart_ordering(&models, "combo", Some("sess"), &cfg, Some(&choice));
        assert_eq!(ordering.models, vec!["m2", "m1"]);
        assert_eq!(
            get_sticky_combo_model("combo", "sess", &models).as_deref(),
            Some("m2")
        );
    }

    #[test]
    fn strict_policy_locks_without_router_call() {
        reset_sticky_combo_store();
        let models = vec!["m1".to_string(), "m2".to_string(), "m3".to_string()];
        set_sticky_combo_model("combo", "sess", "m3");
        let cfg = SmartRouterConfig {
            smart_policy: "strict".to_string(),
            ..Default::default()
        };
        let ordering = apply_smart_ordering(&models, "combo", Some("sess"), &cfg, None);
        assert_eq!(ordering.models, vec!["m3", "m1", "m2"]);
    }

    #[test]
    fn per_turn_ignores_sticky_store() {
        reset_sticky_combo_store();
        let models = vec!["m1".to_string(), "m2".to_string()];
        set_sticky_combo_model("combo", "sess", "m1");
        let cfg = SmartRouterConfig {
            smart_policy: "per-turn".to_string(),
            ..Default::default()
        };
        let choice = SmartChoice {
            choice: "m2".to_string(),
            confidence: Some(0.9),
            probabilities: None,
        };
        let ordering = apply_smart_ordering(&models, "combo", Some("sess"), &cfg, Some(&choice));
        assert_eq!(ordering.models, vec!["m2", "m1"]);
        // Per-turn never writes stickiness.
        assert_eq!(
            get_sticky_combo_model("combo", "sess", &models).as_deref(),
            Some("m1")
        );
    }

    #[test]
    fn clear_session_scopes_to_combo_or_clears_all() {
        reset_sticky_combo_store();
        let models = vec!["m1".to_string(), "m2".to_string()];
        set_sticky_combo_model("combo-a", "s1", "m1");
        set_sticky_combo_model("combo-b", "s1", "m2");
        // Named combo + no session → drops only that combo's sessions.
        clear_sticky_combo_session("combo-a", None);
        assert_eq!(get_sticky_combo_model("combo-a", "s1", &models), None);
        assert_eq!(
            get_sticky_combo_model("combo-b", "s1", &models).as_deref(),
            Some("m2")
        );
        // Empty combo + no session → upstream resetComboRotation clears all.
        clear_sticky_combo_session("", None);
        assert_eq!(get_sticky_combo_model("combo-b", "s1", &models), None);
    }
}
