use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use parking_lot::Mutex;

use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::core::account_fallback::{BACKOFF_BASE_MS, BACKOFF_MAX_MS, MAX_BACKOFF_LEVEL};
use crate::types::Combo;

pub mod fusion;

const LONG_COOLDOWN: Duration = Duration::from_secs(120);
const SHORT_COOLDOWN: Duration = Duration::from_secs(5);
const TRANSIENT_COOLDOWN: Duration = Duration::from_secs(30);

// Fusion tuning defaults — equivalent to FUSION_DEFAULTS in 9router.
const FUSION_DEFAULT_MIN_PANEL: usize = 2;
const FUSION_DEFAULT_STRAGGLER_GRACE_MS: u64 = 8000;
const FUSION_DEFAULT_PANEL_HARD_TIMEOUT_MS: u64 = 90000;

/// Tunable knobs for fusion strategy, parsed from `combo.extra.fusionConfig`
/// or the per-combo strategy overrides in settings.
#[derive(Debug, Clone)]
pub struct FusionConfig {
    /// Minimum successful panel answers before we start the straggler grace timer.
    /// Clamped to `[2, panel.len()]`.
    pub min_panel: usize,
    /// Milliseconds to wait for laggard panel models once quorum is reached.
    pub straggler_grace_ms: u64,
    /// Absolute per-panel-call timeout (one hung model cannot stall the whole fusion).
    pub panel_hard_timeout_ms: u64,
    /// Optional judge model string; falls back to the first panel model.
    pub judge_model: Option<String>,
}

impl Default for FusionConfig {
    fn default() -> Self {
        Self {
            min_panel: FUSION_DEFAULT_MIN_PANEL,
            straggler_grace_ms: FUSION_DEFAULT_STRAGGLER_GRACE_MS,
            panel_hard_timeout_ms: FUSION_DEFAULT_PANEL_HARD_TIMEOUT_MS,
            judge_model: None,
        }
    }
}

impl FusionConfig {
    /// Parse a `FusionConfig` from the `extra.fusionConfig` map (serde_json::Value).
    /// Missing fields get the defaults; `min_panel` is clamped to `[2, panel_len]`.
    pub fn from_extra(extra: &serde_json::Map<String, Value>, panel_len: usize) -> Self {
        let cfg = extra.get("fusionConfig").and_then(|v| v.as_object());
        let mut s = Self::default();
        if let Some(cfg) = cfg {
            if let Some(v) = cfg.get("minPanel").and_then(Value::as_u64) {
                s.min_panel = (v as usize).max(2).min(panel_len);
            }
            if let Some(v) = cfg.get("stragglerGraceMs").and_then(Value::as_u64) {
                s.straggler_grace_ms = v;
            }
            if let Some(v) = cfg.get("panelHardTimeoutMs").and_then(Value::as_u64) {
                s.panel_hard_timeout_ms = v;
            }
            if let Some(v) = cfg.get("judgeModel").and_then(Value::as_str) {
                let v = v.trim();
                if !v.is_empty() {
                    s.judge_model = Some(v.to_string());
                }
            }
        }
        s.min_panel = s.min_panel.max(2).min(panel_len);
        s
    }
}

/// Result from one panel model in a fusion execution.
#[derive(Debug, Clone)]
pub struct FusionPanelResult {
    /// The model name that produced this answer.
    pub model: String,
    /// Extracted text content from the panel response.
    pub text: String,
}

static COMBO_ROTATION_STATE: Lazy<Mutex<HashMap<String, usize>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Sticky counter for round-robin: maps combo_name -> consecutive uses on
/// the current model. Reset when sticky_limit is reached (9router parity).
static COMBO_ROTATION_STICKY_COUNT: Lazy<Mutex<HashMap<String, u32>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// In-memory quarantine map keyed by `(combo_name, model)`. Members get
/// added when [`mark_combo_member_quarantined`] is called and removed
/// either when the TTL expires or via [`clear_combo_member_quarantine`] /
/// [`clear_combo_quarantine`]. Lives alongside `COMBO_ROTATION_STATE` so
/// the dispatcher can consult it without per-request DB I/O.
static COMBO_MEMBER_QUARANTINE: Lazy<Mutex<HashMap<(String, String), Instant>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComboPlan {
    pub name: String,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ComboStrategy {
    #[default]
    Fallback,
    RoundRobin,
    Fusion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComboAttemptError {
    pub status: u16,
    pub message: String,
    pub retry_after: Option<DateTime<Utc>>,
    /// Preserved upstream error body (JSON bytes). When set, the error response
    /// should return this body verbatim instead of constructing a new one from
    /// `message`. 9router parity: preserve upstream response on error.
    pub upstream_body: Option<Vec<u8>>,
}

impl ComboAttemptError {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after: None,
            upstream_body: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComboExecutionError {
    pub status: u16,
    pub message: String,
    pub earliest_retry_after: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackDecision {
    pub should_fallback: bool,
    pub cooldown: Duration,
    pub new_backoff_level: Option<u32>,
}

/// Whether a combo member model currently has capacity to serve a new request.
///
/// `Available` means at least one underlying provider account has a free
/// in-flight slot and is not rate-limited / locked. `Busy` means every
/// matching account is currently saturated, so picking this model would
/// either fail fast (when all members are Busy) or just burn time on the
/// inner per-account fallback before bouncing to the next combo member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelCapacity {
    Available,
    Busy,
}

pub fn get_quota_cooldown(backoff_level: u32) -> Duration {
    // Aligned to 9router formula: BASE * 2^max(0, level-1).
    // This means level 0 and level 1 both produce BASE (2s) delay.
    // 9router used `Math.pow(2, Math.max(0, backoffLevel - 1))`.
    let level = backoff_level.saturating_sub(1);
    let cooldown_ms = BACKOFF_BASE_MS.saturating_mul(2u64.saturating_pow(level));
    Duration::from_millis(cooldown_ms.min(BACKOFF_MAX_MS))
}

/// Hard capabilities that models must support to handle the request — a model
/// missing any of these gets tier-2 (last-resort) placement.
const HARD_CAPS: &[&str] = &["vision", "pdf"];

/// Detect required capabilities from the request body by scanning the last
/// user turn for multimodal blocks (9router detectRequiredCapabilities parity).
pub fn detect_required_capabilities(body: &Value) -> HashSet<String> {
    // Try messages, input (Responses API), contents (Gemini), request.contents (Gemini-passthrough).
    let messages = body
        .get("messages")
        .or_else(|| body.get("input"))
        .or_else(|| body.get("contents"))
        .or_else(|| body.get("request").and_then(|r| r.get("contents")))
        .and_then(Value::as_array);

    let Some(messages) = messages else {
        return HashSet::new();
    };

    // Search capability detection runs independently of message array presence.
    let mut required = HashSet::new();
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        if tools
            .iter()
            .any(|t| t.get("type").and_then(Value::as_str) == Some("search"))
        {
            required.insert("search".to_string());
        }
    }

    // Scan trailing user messages (9router's trailingUserItems pattern):
    // find all messages after the last assistant/model turn.
    let trailing_users: Vec<&Value> = messages
        .iter()
        .rev()
        .take_while(|msg| {
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
            role == "user" || !["assistant", "model"].contains(&role)
        })
        .filter(|msg| msg.get("role").and_then(Value::as_str) == Some("user"))
        .collect();

    if trailing_users.is_empty() {
        return HashSet::new();
    }

    let mut required = HashSet::new();

    for last_user in &trailing_users {
        // Scan OpenAI-style content (messages/input arrays).
        if let Some(content) = last_user.get("content") {
            scan_content_for_capabilities(content, &mut required);
        }
        // For Gemini-style parts (contents array), scan each part for
        // inlineData/fileData MIME types.
        if let Some(parts) = last_user.get("parts").and_then(Value::as_array) {
            for part in parts {
                if part.get("text").and_then(Value::as_str).is_some() {
                    continue;
                }
                let mime = part
                    .get("inlineData")
                    .and_then(|d| d.get("mimeType"))
                    .or_else(|| part.get("fileData").and_then(|d| d.get("mimeType")))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if mime.starts_with("image/") {
                    required.insert("vision".to_string());
                } else if mime == "application/pdf" {
                    required.insert("pdf".to_string());
                }
            }
        }
    }

    required
}

/// Scan a single content value (string or array) for vision/pdf capability signals.
fn scan_content_for_capabilities(content: &Value, required: &mut HashSet<String>) {
    match content {
        Value::Array(arr) => {
            for item in arr {
                match item.get("type").and_then(Value::as_str) {
                    Some("image_url" | "image") => {
                        required.insert("vision".to_string());
                    }
                    Some("input_file" | "document") => {
                        required.insert("pdf".to_string());
                    }
                    Some("inlineData" | "fileData") => {
                        let mime = item
                            .get("mimeType")
                            .or_else(|| item.get("inlineData").and_then(|d| d.get("mimeType")))
                            .or_else(|| item.get("fileData").and_then(|d| d.get("mimeType")))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if mime.starts_with("image/") {
                            required.insert("vision".to_string());
                        } else if mime == "application/pdf" {
                            required.insert("pdf".to_string());
                        }
                    }
                    _ => {}
                }
                // Also check Claude content blocks with source
                if let Some(source) = item.get("source") {
                    if source.get("type").and_then(Value::as_str) == Some("url") {
                        required.insert("vision".to_string());
                    }
                }
            }
        }
        Value::String(text) => {
            if text.contains("media://") {
                required.insert("vision".to_string());
            }
        }
        _ => {}
    }
}

/// Heuristic check: does the combo model entry (e.g. "openai/gpt-4o") support
/// a given capability? Uses provider-prefix and model-name patterns rather
/// than an explicit capability database (9router reads PROVIDERS[].capabilities).
fn model_has_capability(entry: &str, capability: &str) -> bool {
    let entry_lower = entry.to_lowercase();

    match capability {
        "vision" => {
            // Provider-level vision signals
            if entry_lower.starts_with("openai/gpt-4")
                || entry_lower.starts_with("openai/o1")
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
            // Model-name patterns
            if entry_lower.contains("vision")
                || entry_lower.contains("-4o")
                || entry_lower.contains("gemini")
            {
                return true;
            }
            false
        }
        "pdf" => {
            // PDF support is primarily Claude + Gemini
            if entry_lower.starts_with("anthropic/claude")
                || entry_lower.starts_with("vertex/claude")
                || entry_lower.starts_with("aws/claude")
                || entry_lower.starts_with("google/gemini")
                || entry_lower.starts_with("vertex/gemini")
                || entry_lower.starts_with("gcp/gemini")
            {
                return true;
            }
            false
        }
        _ => false,
    }
}

/// Reorder combo models so that capability-matching models are tried first
/// (9router reorderByCapabilities parity).
///
/// Tier 0 — All required caps present (preferred first).
/// Tier 1 — No missing hard caps (missing a soft cap only, still fine).
/// Tier 2 — Missing one or more hard caps (last resort).
///
/// When no capabilities are required, the input order is preserved unchanged.
pub fn reorder_by_capabilities(models: &[String], required: &HashSet<String>) -> Vec<String> {
    if required.is_empty() {
        return models.to_vec();
    }

    let mut tier0: Vec<String> = Vec::new();
    let mut tier1: Vec<String> = Vec::new();
    let mut tier2: Vec<String> = Vec::new();

    for model in models {
        let has_all_required = required.iter().all(|cap| model_has_capability(model, cap));
        let missing_hard = HARD_CAPS
            .iter()
            .any(|cap| required.contains(*cap) && !model_has_capability(model, cap));

        if has_all_required {
            tier0.push(model.clone());
        } else if !missing_hard {
            tier1.push(model.clone());
        } else {
            tier2.push(model.clone());
        }
    }

    let mut result = Vec::with_capacity(models.len());
    result.extend(tier0);
    result.extend(tier1);
    result.extend(tier2);
    result
}

pub fn check_fallback_error(status: u16, error_text: &str, backoff_level: u32) -> FallbackDecision {
    let lower = error_text.to_lowercase();

    for text_rule in [
        ("no credentials", Some(LONG_COOLDOWN), false),
        ("request not allowed", Some(SHORT_COOLDOWN), false),
        ("improperly formed request", Some(LONG_COOLDOWN), false),
        ("rate limit", None, true),
        ("too many requests", None, true),
        ("quota exceeded", None, true),
        ("capacity", None, true),
        ("overloaded", None, true),
    ] {
        if lower.contains(text_rule.0) {
            return if text_rule.2 {
                let new_level = (backoff_level + 1).min(MAX_BACKOFF_LEVEL);
                FallbackDecision {
                    should_fallback: true,
                    cooldown: get_quota_cooldown(new_level),
                    new_backoff_level: Some(new_level),
                }
            } else {
                FallbackDecision {
                    should_fallback: true,
                    cooldown: text_rule.1.unwrap_or(TRANSIENT_COOLDOWN),
                    new_backoff_level: None,
                }
            };
        }
    }

    match status {
        // 9router ERROR_RULES: 401/402/403/404 all allow fallback with LONG_COOLDOWN
        // 400 is NOT in ERROR_RULES — falls through to default TRANSIENT_COOLDOWN (9router parity)
        401 | 402 | 403 | 404 => FallbackDecision {
            should_fallback: true,
            cooldown: LONG_COOLDOWN,
            new_backoff_level: None,
        },
        429 => {
            let new_level = (backoff_level + 1).min(MAX_BACKOFF_LEVEL);
            FallbackDecision {
                should_fallback: true,
                cooldown: get_quota_cooldown(new_level),
                new_backoff_level: Some(new_level),
            }
        }
        _ => FallbackDecision {
            should_fallback: true,
            cooldown: TRANSIENT_COOLDOWN,
            new_backoff_level: None,
        },
    }
}

pub fn get_rotated_models(
    models: &[String],
    combo_name: Option<&str>,
    strategy: ComboStrategy,
    sticky_limit: u32,
) -> Vec<String> {
    if models.len() <= 1 || strategy != ComboStrategy::RoundRobin {
        return models.to_vec();
    }

    let Some(combo_name) = combo_name else {
        return models.to_vec();
    };

    let mut state = COMBO_ROTATION_STATE.lock();
    let current_index = *state.get(combo_name).unwrap_or(&0);
    let mut rotated = models.to_vec();

    for _ in 0..current_index {
        if let Some(first) = rotated.first().cloned() {
            rotated.remove(0);
            rotated.push(first);
        }
    }

    if sticky_limit > 1 {
        let mut sticky_counts = COMBO_ROTATION_STICKY_COUNT.lock();
        let count = sticky_counts.entry(combo_name.to_string()).or_insert(0);
        *count += 1;
        if *count >= sticky_limit {
            *count = 0;
            state.insert(combo_name.to_string(), (current_index + 1) % models.len());
        }
    } else {
        state.insert(combo_name.to_string(), (current_index + 1) % models.len());
    }

    rotated
}

pub fn reset_combo_rotation(combo_name: Option<&str>) {
    let mut state = COMBO_ROTATION_STATE.lock();
    let mut sticky = COMBO_ROTATION_STICKY_COUNT.lock();
    if let Some(combo_name) = combo_name {
        state.remove(combo_name);
        sticky.remove(combo_name);
    } else {
        state.clear();
        sticky.clear();
    }
}

pub fn rotation_index(combo_name: &str) -> Option<usize> {
    COMBO_ROTATION_STATE.lock().get(combo_name).copied()
}

/// Mark a single `(combo_name, model)` pair as quarantined for `ttl`.
///
/// This is used by the chat dispatcher when *every* underlying account
/// for a combo member has just failed: rather than letting the next
/// request hit the same broken model immediately, we record it here so
/// [`execute_combo_strategy_with_capacity`] can skip it on subsequent
/// calls until the TTL elapses. This is in-memory only (matches the
/// existing `COMBO_ROTATION_STATE` semantics) and resets on restart.
pub fn mark_combo_member_quarantined(combo_name: &str, model: &str, ttl: Duration) {
    let until = Instant::now() + ttl;
    let mut guard = COMBO_MEMBER_QUARANTINE.lock();
    guard.insert((combo_name.to_string(), model.to_string()), until);
}

/// Clear quarantine for a specific `(combo_name, model)` pair.
pub fn clear_combo_member_quarantine(combo_name: &str, model: &str) {
    let mut guard = COMBO_MEMBER_QUARANTINE.lock();
    guard.remove(&(combo_name.to_string(), model.to_string()));
}

/// Clear all quarantined members for a combo (e.g. after the operator
/// edited the member list).
pub fn clear_combo_quarantine(combo_name: &str) {
    let mut guard = COMBO_MEMBER_QUARANTINE.lock();
    guard.retain(|(name, _), _| name != combo_name);
}

/// Returns members currently quarantined for `combo_name` together with
/// the absolute `Instant` their cooldown expires. Stale entries are
/// pruned as a side effect.
pub fn combo_quarantine_for(combo_name: &str) -> Vec<(String, Instant)> {
    let now = Instant::now();
    let mut guard = COMBO_MEMBER_QUARANTINE.lock();
    guard.retain(|_, until| *until > now);
    guard
        .iter()
        .filter_map(|((name, model), until)| {
            if name == combo_name {
                Some((model.clone(), *until))
            } else {
                None
            }
        })
        .collect()
}

fn quarantined_members(combo_name: &str) -> HashSet<String> {
    let now = Instant::now();
    let mut guard = COMBO_MEMBER_QUARANTINE.lock();
    guard.retain(|_, until| *until > now);
    guard
        .iter()
        .filter_map(|((name, model), _)| {
            if name == combo_name {
                Some(model.clone())
            } else {
                None
            }
        })
        .collect()
}

pub fn get_combo_models_from_data(model_str: &str, combos: &[Combo]) -> Option<Vec<String>> {
    if model_str.contains('/') {
        return None;
    }

    combos
        .iter()
        .find(|combo| combo.name == model_str && !combo.models.is_empty())
        .map(|combo| combo.models.clone())
}

/// Returns the set of disabled members for a combo by name, or empty if
/// the combo doesn't exist.
pub fn get_disabled_members_for_combo(combo_name: &str, combos: &[Combo]) -> Vec<String> {
    combos
        .iter()
        .find(|combo| combo.name == combo_name)
        .map(|combo| combo.disabled_models.clone())
        .unwrap_or_default()
}

pub async fn execute_combo_strategy<T, F, Fut>(
    models: &[String],
    combo_name: Option<&str>,
    strategy: ComboStrategy,
    handle_single_model: F,
) -> Result<T, ComboExecutionError>
where
    F: FnMut(&str) -> Fut,
    Fut: Future<Output = Result<T, ComboAttemptError>>,
{
    execute_combo_strategy_with_capacity(
        models,
        combo_name,
        strategy,
        &[],
        |_| ModelCapacity::Available,
        handle_single_model,
    )
    .await
}

/// Same as [`execute_combo_strategy`], but consults a capacity callback to
/// short-circuit on saturated providers in `RoundRobin` mode and applies
/// two additional pre-gates that skip combo members *before* dispatch:
///
/// 1. **`disabled_members`** — explicit operator-supplied list of combo
///    members to never dispatch to. Filtered out in both `Fallback` and
///    `RoundRobin`. This is the "manual bypass" knob exposed via the UI
///    when a member is known to be broken but the operator wants to keep
///    it in the configured list (for visibility / quick re-enable)
///    instead of removing it.
/// 2. **Auto-quarantine** — `(combo_name, model)` pairs registered via
///    [`mark_combo_member_quarantined`]. Used by the chat dispatcher to
///    park a member for the same cooldown duration `check_fallback_error`
///    already returns when every underlying account has just failed, so
///    the next request doesn't immediately retry a known-broken model
///    and make the CLI agent appear to hang.
///
/// When at least one rotated member reports `ModelCapacity::Available`, only
/// those members are tried (in rotation order). Busy members are skipped
/// entirely — otherwise a slow request against a saturated provider would
/// pin the caller while it spins through the per-account inner fallback,
/// which is the failure mode that makes multi-repo coding agents appear to
/// hang. If every member is `Busy`, we fail fast with a 503 and surface
/// the earliest known retry-after so the caller can back off instead of
/// piling more load onto already-saturated providers.
///
/// `Fallback` strategy keeps its declared priority order for capacity —
/// capacity is advisory only and we still attempt every non-disabled,
/// non-quarantined member sequentially so the configured primary/
/// secondary semantics are preserved. Disabled/quarantined members are
/// *always* skipped regardless of strategy.
pub async fn execute_combo_strategy_with_capacity<T, F, Fut, C>(
    models: &[String],
    combo_name: Option<&str>,
    strategy: ComboStrategy,
    disabled_members: &[String],
    capacity_check: C,
    mut handle_single_model: F,
) -> Result<T, ComboExecutionError>
where
    F: FnMut(&str) -> Fut,
    Fut: Future<Output = Result<T, ComboAttemptError>>,
    C: Fn(&str) -> ModelCapacity,
{
    // Manual disable + auto-quarantine pre-gate. Applied to the raw
    // member list *before* rotation so the round-robin index doesn't
    // burn turns on members that will never be dispatched to.
    let mut skip: HashSet<String> = disabled_members.iter().cloned().collect();
    if let Some(name) = combo_name {
        skip.extend(quarantined_members(name));
    }

    let active: Vec<String> = models
        .iter()
        .filter(|model| !skip.contains(model.as_str()))
        .cloned()
        .collect();

    if active.is_empty() {
        // Distinguish "operator muted everything" from "transient
        // quarantine" so the caller can decide whether to surface a
        // 4xx vs 503.
        let only_quarantine = !models.is_empty()
            && disabled_members.is_empty()
            && combo_name.is_some()
            && models.iter().all(|m| skip.contains(m));
        return Err(ComboExecutionError {
            status: if only_quarantine { 503 } else { 400 },
            message: if only_quarantine {
                "All combo members are currently quarantined after recent failures".into()
            } else {
                "All combo members are disabled".into()
            },
            earliest_retry_after: None,
        });
    }

    let rotated = get_rotated_models(&active, combo_name, strategy, 1);

    if strategy == ComboStrategy::RoundRobin && rotated.len() > 1 {
        let available: Vec<String> = rotated
            .iter()
            .filter(|model| capacity_check(model.as_str()) == ModelCapacity::Available)
            .cloned()
            .collect();

        if available.is_empty() {
            return Err(ComboExecutionError {
                status: 503,
                message: "All combo providers are at max in-flight capacity".into(),
                earliest_retry_after: None,
            });
        }

        return iterate_combo_models(&available, &mut handle_single_model).await;
    }

    iterate_combo_models(&rotated, &mut handle_single_model).await
}

async fn iterate_combo_models<T, F, Fut>(
    order: &[String],
    handle_single_model: &mut F,
) -> Result<T, ComboExecutionError>
where
    F: FnMut(&str) -> Fut,
    Fut: Future<Output = Result<T, ComboAttemptError>>,
{
    let mut last_error = None;
    let mut earliest_retry_after = None;

    for model in order {
        match handle_single_model(model).await {
            Ok(result) => return Ok(result),
            Err(error) => {
                if let Some(retry_after) = error.retry_after {
                    earliest_retry_after = match earliest_retry_after {
                        Some(current) if current <= retry_after => Some(current),
                        _ => Some(retry_after),
                    };
                }

                let decision = check_fallback_error(error.status, &error.message, 0);
                if !decision.should_fallback {
                    return Err(ComboExecutionError {
                        status: error.status,
                        message: error.message,
                        earliest_retry_after,
                    });
                }

                // 9router transient wait: on 502/503/504, wait cooldown before
                // falling through to the next combo member so the upstream
                // gets a brief recovery window instead of an immediate retry.
                // 9router caps transient wait at 5000ms to avoid 30s+ delays in the iterator
                if matches!(error.status, 502 | 503 | 504)
                    && !decision.cooldown.is_zero()
                    && decision.cooldown.as_millis() <= 5000
                {
                    tokio::time::sleep(decision.cooldown).await;
                }

                last_error = Some(error);
            }
        }
    }

    let fallback_error = last_error.unwrap_or(ComboAttemptError {
        status: 503,
        message: "All combo models unavailable".into(),
        retry_after: earliest_retry_after,
        upstream_body: None,
    });

    let status = if fallback_error
        .message
        .to_lowercase()
        .contains("no credentials")
    {
        503
    } else {
        fallback_error.status.max(500)
    };

    Err(ComboExecutionError {
        status,
        message: fallback_error.message,
        earliest_retry_after,
    })
}
