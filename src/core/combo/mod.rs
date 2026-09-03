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
use crate::types::{AppDb, Combo, PricingTable};

pub mod auto_combo;
pub mod capabilities;
pub mod capacity_adapter;
pub mod fusion;
pub mod hedging;
pub mod ordering;
pub mod shadow;

pub use ordering::{sort_models_by_cost, sort_models_by_latency};

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
    AutoCombo,
    Hedging,
    Shadow,
    /// Cheapest-first: sort members by pricing-table cost ascending (free $0 first).
    Cheapest,
    /// Fastest-first: sort members by latency hint ascending (missing → original order).
    Fastest,
    /// Quality-first: keep capability-tier ordering (vision/pdf/audio/video aware).
    Quality,
}

/// Map a strategy string (from `settings.combo_strategies`, `combo.extra["strategy"]`,
/// or the global `settings.combo_strategy`) to a [`ComboStrategy`]. Unknown / empty
/// values fall back to `Fallback`.
pub fn parse_combo_strategy(value: &str) -> ComboStrategy {
    let value = value.trim();
    if value.eq_ignore_ascii_case("round-robin") {
        ComboStrategy::RoundRobin
    } else if value.eq_ignore_ascii_case("fusion") {
        ComboStrategy::Fusion
    } else if value.eq_ignore_ascii_case("auto-combo") || value.eq_ignore_ascii_case("autocombo") {
        ComboStrategy::AutoCombo
    } else if value.eq_ignore_ascii_case("hedging") {
        ComboStrategy::Hedging
    } else if value.eq_ignore_ascii_case("shadow") {
        ComboStrategy::Shadow
    } else if value.eq_ignore_ascii_case("cheapest") {
        ComboStrategy::Cheapest
    } else if value.eq_ignore_ascii_case("fastest") {
        ComboStrategy::Fastest
    } else if value.eq_ignore_ascii_case("quality") {
        ComboStrategy::Quality
    } else {
        ComboStrategy::Fallback
    }
}

/// Resolve the effective strategy for a combo, in priority order:
/// 1. `settings.combo_strategies[name]` (per-combo override)
/// 2. `combo.extra["strategy"]` (set when the combo was created)
/// 3. `settings.combo_strategy` (global default)
///
/// Single source of truth for the chat, web-fetch, and CLI dispatch paths.
pub fn strategy_for_combo(snapshot: &AppDb, combo_name: &str) -> ComboStrategy {
    let value: String = if let Some(entry) = snapshot.settings.combo_strategies.get(combo_name) {
        entry.strategy_name().to_string()
    } else if let Some(combo) = snapshot.combos.iter().find(|c| c.name == combo_name) {
        combo
            .extra
            .get("strategy")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| snapshot.settings.combo_strategy.clone())
    } else {
        snapshot.settings.combo_strategy.clone()
    };

    parse_combo_strategy(&value)
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

/// Parse a `retryAfter` value from an upstream error body into a concrete
/// timestamp. 9router `handleComboChat` reads `errorBody.retryAfter` and
/// normalizes via `new Date(retryAfter)`, which accepts both an ISO-8601 date
/// string and a numeric seconds count. Returns `None` when absent/unparseable.
pub fn parse_retry_after_from_body(body: &[u8]) -> Option<DateTime<Utc>> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let retry_after = value
        .get("error")
        .and_then(|e| e.get("retryAfter"))
        .or_else(|| value.get("retryAfter"))?;
    if let Some(iso) = retry_after.as_str() {
        // ISO-8601 date string.
        if let Ok(dt) = DateTime::parse_from_rfc3339(iso) {
            return Some(dt.with_timezone(&Utc));
        }
        // Numeric string of seconds.
        if let Ok(secs) = iso.parse::<i64>() {
            return Utc::now().checked_add_signed(chrono::Duration::seconds(secs));
        }
        return None;
    }
    if let Some(secs) = retry_after.as_i64() {
        return Utc::now().checked_add_signed(chrono::Duration::seconds(secs));
    }
    if let Some(secs) = retry_after.as_f64() {
        return Utc::now().checked_add_signed(chrono::Duration::seconds(secs as i64));
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComboExecutionError {
    pub status: u16,
    pub message: String,
    pub earliest_retry_after: Option<DateTime<Utc>>,
    /// Preserved upstream error body from the last failing member (H23).
    /// When set, the error response should return this body verbatim.
    pub upstream_body: Option<Vec<u8>>,
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
const HARD_CAPS: &[&str] = &["vision", "pdf", "audioInput", "videoInput"];

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

    // 9router currently has search detection disabled; keep empty for parity.
    let mut required = HashSet::new();

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
        return required;
    }

    for last_user in &trailing_users {
        // Full message scan (JS scanMessage): images[], attachments,
        // message-level media keys, array blocks, data-URI strings.
        scan_message_capabilities(last_user, &mut required);
    }

    required
}

/// Extract a mime from a `data:…` URI (JS /^data:([^;,]+)/).
fn data_uri_mime(value: &str) -> Option<&str> {
    let rest = value.strip_prefix("data:")?;
    let end = rest.find([';', ','])?;
    Some(&rest[..end])
}

fn add_by_mime(mime: &str, required: &mut HashSet<String>) {
    if mime.starts_with("image/") {
        required.insert("vision".to_string());
    } else if mime == "application/pdf" {
        required.insert("pdf".to_string());
    } else if mime.starts_with("audio/") {
        required.insert("audioInput".to_string());
    } else if mime.starts_with("video/") {
        required.insert("videoInput".to_string());
    }
}

/// Scan one content block (JS combo.js scanBlock, lines 117-136).
fn scan_block(b: &Value, required: &mut HashSet<String>) {
    let Some(obj) = b.as_object() else { return };
    match obj.get("type").and_then(Value::as_str) {
        Some("image_url" | "image" | "input_image") => {
            required.insert("vision".to_string());
        }
        Some("input_audio" | "audio_url" | "audio") => {
            required.insert("audioInput".to_string());
        }
        Some("input_video" | "video_url" | "video") => {
            required.insert("videoInput".to_string());
        }
        Some("file" | "document" | "input_file") => {
            // Infer modality from the embedded mime when available; generic
            // files fall back to pdf.
            let mut fmime: Option<String> = None;
            if let Some(format) = obj
                .get("input_audio")
                .and_then(|ia| ia.get("format"))
                .and_then(Value::as_str)
            {
                fmime = Some(format!("audio/{format}"));
            } else if let Some(fd) = obj
                .get("file")
                .and_then(|f| f.get("file_data"))
                .and_then(Value::as_str)
            {
                fmime = data_uri_mime(fd).map(String::from);
            } else if let Some(mt) = obj
                .get("source")
                .and_then(|src| src.get("media_type"))
                .and_then(Value::as_str)
            {
                fmime = Some(mt.to_string());
            } else if let Some(sd) = obj
                .get("source")
                .and_then(|src| src.get("data"))
                .and_then(Value::as_str)
            {
                fmime = data_uri_mime(sd).map(String::from);
            }
            match fmime {
                Some(ref m) => add_by_mime(m, required),
                None => {
                    required.insert("pdf".to_string());
                }
            }
        }
        _ => {}
    }
    // Gemini parts: inlineData/fileData carry a mime.
    for key in ["inlineData", "fileData"] {
        if let Some(mime) = obj
            .get(key)
            .and_then(|d| d.get("mimeType"))
            .and_then(Value::as_str)
        {
            add_by_mime(mime, required);
        }
    }
}

/// Scan one message (JS combo.js scanMessage, lines 141-173): Ollama images[],
/// Vercel AI SDK attachments, direct message-level media keys, array blocks,
/// and data-URIs embedded in plain-string content.
fn scan_message_capabilities(m: &Value, required: &mut HashSet<String>) {
    let Some(obj) = m.as_object() else { return };

    // Ollama / Hermes images array.
    if obj
        .get("images")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty())
    {
        required.insert("vision".to_string());
    }

    // Vercel AI SDK / Hermes experimental_attachments / attachments.
    for key in ["experimental_attachments", "attachments"] {
        if let Some(attachments) = obj.get(key).and_then(Value::as_array) {
            for att in attachments {
                let Some(att_obj) = att.as_object() else {
                    continue;
                };
                let url_mime = att_obj
                    .get("url")
                    .and_then(Value::as_str)
                    .and_then(data_uri_mime);
                let mime = att_obj
                    .get("contentType")
                    .or_else(|| att_obj.get("mediaType"))
                    .and_then(Value::as_str)
                    .or(url_mime);
                match mime {
                    Some(m) => add_by_mime(m, required),
                    None => {
                        if att_obj.contains_key("url") || att_obj.contains_key("data") {
                            required.insert("vision".to_string());
                        }
                    }
                }
            }
        }
    }

    // Direct message-level modality properties.
    if obj.contains_key("image_url") || obj.contains_key("image") {
        required.insert("vision".to_string());
    }
    if obj.contains_key("audio_url") || obj.contains_key("audio") {
        required.insert("audioInput".to_string());
    }

    // Array / string content.
    match obj.get("content") {
        Some(Value::Array(blocks)) => {
            for b in blocks {
                scan_block(b, required);
            }
        }
        Some(Value::String(text)) => {
            if text.contains("data:image/") {
                required.insert("vision".to_string());
            } else if text.contains("data:audio/") {
                required.insert("audioInput".to_string());
            } else if text.contains("data:application/pdf") {
                required.insert("pdf".to_string());
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
            // Provider-level vision signals (gpt-4 base has no vision; only
            // 4o+ variants — matched via the `-4o` model-name pattern below).
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
            // Model-name patterns
            if entry_lower.contains("vision")
                || entry_lower.contains("-4o")
                || entry_lower.contains("gemini")
                || entry_lower.starts_with("oc/mimo")
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
        // The capacity adapter's default pool model (oc/mimo-v2.5-free) is a
        // multimodal free model (9router capabilities.js entry).
        "audioInput" => entry_lower.starts_with("oc/mimo"),
        "videoInput" => entry_lower.starts_with("oc/mimo"),
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

/// Single source of truth: delegates to `error_config::classify_error`.
pub fn check_fallback_error(status: u16, error_text: &str, backoff_level: u32) -> FallbackDecision {
    use crate::core::config::error_config::{classify_error, ErrorClassification};

    match classify_error(Some(error_text), Some(status)) {
        ErrorClassification::Backoff => {
            let new_level = (backoff_level + 1).min(MAX_BACKOFF_LEVEL);
            FallbackDecision {
                should_fallback: true,
                cooldown: get_quota_cooldown(new_level),
                new_backoff_level: Some(new_level),
            }
        }
        ErrorClassification::Cooldown(d) => FallbackDecision {
            should_fallback: true,
            cooldown: d,
            new_backoff_level: None,
        },
        ErrorClassification::NoMatch => FallbackDecision {
            should_fallback: true,
            cooldown: TRANSIENT_COOLDOWN,
            new_backoff_level: None,
        },
        ErrorClassification::Permanent => FallbackDecision {
            should_fallback: false,
            cooldown: Duration::ZERO,
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

pub fn quarantined_members(combo_name: &str) -> HashSet<String> {
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
    execute_combo_strategy_full(
        models,
        combo_name,
        strategy,
        disabled_members,
        1, // sticky default
        None,
        &PricingTable::new(),
        capacity_check,
        handle_single_model,
    )
    .await
}

/// Full combo strategy with sticky limit and capability reorder after RR
/// (9router combo.js order: rotate first, then reorderByCapabilities).
///
/// `pricing` drives the cost/latency-aware orderings for the `Cheapest` and
/// `Fastest` strategies. Pass `&PricingTable::new()` when no pricing data is
/// available — both strategies then fall back to the configured priority order.
pub async fn execute_combo_strategy_full<T, F, Fut, C>(
    models: &[String],
    combo_name: Option<&str>,
    strategy: ComboStrategy,
    disabled_members: &[String],
    sticky_limit: u32,
    required_caps: Option<&HashSet<String>>,
    pricing: &PricingTable,
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
    // Health gate: skip members whose provider has *every* connection inside a
    // degrade window (health daemon saw 429/503/5xx). Providers with no health
    // record, or with at least one healthy account, are never skipped.
    skip.extend(
        models
            .iter()
            .filter(|model| crate::core::health::is_model_degraded(model))
            .cloned(),
    );

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
            && models.iter().all(|m| skip.contains(m));
        return Err(ComboExecutionError {
            status: if only_quarantine { 503 } else { 400 },
            message: if only_quarantine {
                "All combo members are currently quarantined or degraded after recent failures"
                    .into()
            } else {
                "All combo members are disabled".into()
            },
            earliest_retry_after: None,
            upstream_body: None,
        });
    }

    // 9router: getRotatedModels first, then capability autoswitch
    let mut order = get_rotated_models(&active, combo_name, strategy, sticky_limit.max(1));

    // Strategy-specific ordering. For Cheapest/Fastest the cost/latency sort is
    // the dominant ordering; for Quality we keep the capability-tier ordering;
    // for every other strategy we preserve the existing capability autoswitch
    // (only when required caps are present) so fallback/round-robin semantics
    // are unchanged.
    match strategy {
        ComboStrategy::Cheapest => {
            order = sort_models_by_cost(&order, pricing);
        }
        ComboStrategy::Fastest => {
            order = sort_models_by_latency(&order, pricing);
        }
        ComboStrategy::Quality => {
            if let Some(caps) = required_caps {
                if !caps.is_empty() {
                    order = reorder_by_capabilities(&order, caps);
                }
            }
        }
        _ => {
            if let Some(caps) = required_caps {
                if !caps.is_empty() {
                    order = reorder_by_capabilities(&order, caps);
                    tracing::debug!(
                        target: "openproxy::combo",
                        "COMBO_ORDER after_rr+caps sticky={} order={:?}",
                        sticky_limit,
                        order
                    );
                }
            }
        }
    }

    if strategy == ComboStrategy::RoundRobin && order.len() > 1 {
        let available: Vec<String> = order
            .iter()
            .filter(|model| capacity_check(model.as_str()) == ModelCapacity::Available)
            .cloned()
            .collect();

        if available.is_empty() {
            return Err(ComboExecutionError {
                status: 503,
                message: "All combo providers are at max in-flight capacity".into(),
                earliest_retry_after: None,
                upstream_body: None,
            });
        }

        return iterate_combo_models(&available, &mut handle_single_model).await;
    }

    iterate_combo_models(&order, &mut handle_single_model).await
}

async fn iterate_combo_models<T, F, Fut>(
    order: &[String],
    handle_single_model: &mut F,
) -> Result<T, ComboExecutionError>
where
    F: FnMut(&str) -> Fut,
    Fut: Future<Output = Result<T, ComboAttemptError>>,
{
    let mut last_error: Option<ComboAttemptError> = None;
    let mut first_error: Option<ComboAttemptError> = None;
    let mut earliest_retry_after = None;
    // Track actual backoff level across consecutive failures (H21).
    let mut consecutive_backoff_level: u32 = 0;

    for model in order {
        match handle_single_model(model).await {
            Ok(result) => return Ok(result),
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(ComboAttemptError {
                        status: error.status,
                        message: error.message.clone(),
                        retry_after: error.retry_after,
                        upstream_body: error.upstream_body.clone(),
                    });
                }
                if let Some(retry_after) = error.retry_after {
                    earliest_retry_after = match earliest_retry_after {
                        Some(current) if current <= retry_after => Some(current),
                        _ => Some(retry_after),
                    };
                }

                let decision =
                    check_fallback_error(error.status, &error.message, consecutive_backoff_level);
                if !decision.should_fallback {
                    return Err(ComboExecutionError {
                        status: error.status,
                        message: error.message,
                        earliest_retry_after,
                        upstream_body: error.upstream_body.clone(),
                    });
                }

                // 9router transient wait: on 502/503/504, wait cooldown before
                // falling through to the next combo member so the upstream
                // gets a brief recovery window instead of an immediate retry.
                // 9router caps transient wait at 5000ms to avoid 30s+ delays in the iterator
                if matches!(error.status, 502..=504)
                    && !decision.cooldown.is_zero()
                    && decision.cooldown.as_millis() <= 5000
                {
                    tokio::time::sleep(decision.cooldown).await;
                }

                // Advance backoff level when the decision requested a new one (H21).
                if let Some(new_level) = decision.new_backoff_level {
                    consecutive_backoff_level = new_level;
                }

                last_error = Some(error);
            }
        }
    }

    // 9router keeps the *first* failure status for the final response.
    let first_status = first_error.as_ref().map(|e| e.status);
    let message = last_error
        .as_ref()
        .map(|e| e.message.clone())
        .or_else(|| first_error.as_ref().map(|e| e.message.clone()))
        .unwrap_or_else(|| "All combo models unavailable".into());

    let status = if message.to_lowercase().contains("no credentials") {
        503
    } else {
        match first_status.unwrap_or(503) {
            0 => 503,
            s => s,
        }
    };

    // Preserve upstream_body from the last error if available (H23).
    let final_upstream_body = last_error
        .as_ref()
        .and_then(|e: &ComboAttemptError| e.upstream_body.clone())
        .or_else(|| first_error.as_ref().and_then(|e| e.upstream_body.clone()));

    Err(ComboExecutionError {
        status,
        message,
        earliest_retry_after,
        upstream_body: final_upstream_body,
    })
}

#[cfg(test)]
mod capability_scan_tests {
    use super::*;

    #[test]
    fn audio_url_and_video_url_types_detected() {
        let body = json!({
            "messages": [{"role": "user", "content": [
                {"type": "audio_url", "audio_url": {"url": "https://x/a.wav"}},
                {"type": "video_url", "video_url": {"url": "https://x/v.mp4"}}
            ]}]
        });
        let caps = detect_required_capabilities(&body);
        assert!(caps.contains("audioInput"));
        assert!(caps.contains("videoInput"));
    }

    #[test]
    fn claude_document_media_type_infers_modality() {
        // source.media_type drives inference; not hardcoded pdf.
        let body = json!({
            "messages": [{"role": "user", "content": [
                {"type": "document", "source": {
                    "type": "base64", "media_type": "application/pdf", "data": "..."}}
            ]}, {"role": "user", "content": [
                {"type": "document", "source": {
                    "type": "base64", "media_type": "image/png", "data": "..."}}
            ]}]
        });
        let caps = detect_required_capabilities(&body);
        assert!(caps.contains("pdf"));
        assert!(caps.contains("vision"));
    }

    #[test]
    fn input_audio_format_infers_audio_mime() {
        let body = json!({
            "messages": [{"role": "user", "content": [
                {"type": "file", "input_audio": {"format": "wav"}}
            ]}]
        });
        assert!(detect_required_capabilities(&body).contains("audioInput"));
    }

    #[test]
    fn ollama_images_array_and_attachments() {
        let ollama = json!({"messages": [{"role": "user", "content": "hi", "images": ["b64"]}]});
        assert!(detect_required_capabilities(&ollama).contains("vision"));
        let vercel = json!({"messages": [{"role": "user", "content": "hi",
            "experimental_attachments": [{"url": "data:application/pdf;base64,x"}]}]});
        assert!(detect_required_capabilities(&vercel).contains("pdf"));
    }

    #[test]
    fn data_uri_in_plain_string_content() {
        let body = json!({"messages": [{"role": "user",
            "content": "look at data:image/png;base64,AAA please"}]});
        assert!(detect_required_capabilities(&body).contains("vision"));
    }
}

mod tests {
    use super::*;

    #[test]
    fn combo_retry_after_from_body_parsed() {
        // Acceptance guard test (bead .107): an error whose JSON body carries
        // retryAfter as an ISO date string (no Retry-After header needed).
        let body = br#"{"error":{"message":"rate limited","retryAfter":"2030-01-01T00:00:00Z"}}"#;
        let retry_after = parse_retry_after_from_body(body);
        assert!(retry_after.is_some(), "ISO retryAfter should parse");
        if let Some(ra) = retry_after {
            assert!(ra > Utc::now(), "parsed date should be in the future");
        }
    }

    #[test]
    fn combo_retry_after_from_body_numeric_seconds() {
        // retryAfter as a numeric seconds count → future timestamp.
        let body = br#"{"error":{"retryAfter":120}}"#;
        let retry_after = parse_retry_after_from_body(body);
        assert!(retry_after.is_some(), "numeric retryAfter should parse");
        if let Some(ra) = retry_after {
            let delta = ra.signed_duration_since(Utc::now());
            assert!(delta.num_seconds() >= 100 && delta.num_seconds() <= 140);
        }
    }

    #[test]
    fn combo_retry_after_from_body_absent() {
        // No retryAfter → None.
        let body = br#"{"error":{"message":"boom"}}"#;
        assert!(parse_retry_after_from_body(body).is_none());
        assert!(parse_retry_after_from_body(b"not json").is_none());
    }

    /// Run a combo where every member fails with a retryable 429, so the
    /// recorded attempt list is the *full* dispatch order the strategy chose.
    async fn attempt_order(
        models: &[String],
        combo_name: &str,
        strategy: ComboStrategy,
        pricing: &PricingTable,
    ) -> Vec<String> {
        let attempted = Arc::new(Mutex::new(Vec::new()));
        let recorder = attempted.clone();
        let result = execute_combo_strategy_full(
            models,
            Some(combo_name),
            strategy,
            &[],
            1,
            None,
            pricing,
            |_| ModelCapacity::Available,
            move |model: &str| {
                let recorder = recorder.clone();
                let owned = model.to_string();
                async move {
                    recorder.lock().push(owned);
                    Err::<String, _>(ComboAttemptError::new(429, "rate limited"))
                }
            },
        )
        .await;
        assert!(result.is_err(), "every member fails in this fixture");
        let order = attempted.lock().clone();
        order
    }

    fn pricing_fixture() -> PricingTable {
        let mut pricing = PricingTable::new();
        for (provider, model, entry) in [
            (
                "openai",
                "gpt-4o",
                json!({ "input": 2.5, "output": 10.0, "latency": 800 }),
            ),
            (
                "anthropic",
                "claude-3-haiku",
                json!({ "input": 0.25, "output": 1.25, "latency": 400 }),
            ),
            ("nvidia", "llama-3.1", json!({ "latencyMs": 200 })),
        ] {
            pricing
                .entry(provider.to_string())
                .or_default()
                .insert(model.to_string(), entry);
        }
        pricing
    }

    fn combo_fixture() -> Vec<String> {
        vec![
            "openai/gpt-4o".to_string(),
            "anthropic/claude-3-haiku".to_string(),
            "nvidia/llama-3.1".to_string(),
        ]
    }

    #[tokio::test]
    async fn combo_strategy_cheapest_dispatches_free_then_cheap() {
        let order = attempt_order(
            &combo_fixture(),
            "cheapest-combo",
            ComboStrategy::Cheapest,
            &pricing_fixture(),
        )
        .await;
        assert_eq!(
            order,
            vec![
                "nvidia/llama-3.1".to_string(),
                "anthropic/claude-3-haiku".to_string(),
                "openai/gpt-4o".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn combo_strategy_fastest_dispatches_lowest_latency_first() {
        let order = attempt_order(
            &combo_fixture(),
            "fastest-combo",
            ComboStrategy::Fastest,
            &pricing_fixture(),
        )
        .await;
        assert_eq!(
            order,
            vec![
                "nvidia/llama-3.1".to_string(),
                "anthropic/claude-3-haiku".to_string(),
                "openai/gpt-4o".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn combo_strategy_fallback_keeps_declared_order_despite_pricing() {
        let order = attempt_order(
            &combo_fixture(),
            "fallback-combo",
            ComboStrategy::Fallback,
            &pricing_fixture(),
        )
        .await;
        assert_eq!(
            order,
            combo_fixture(),
            "fallback must ignore cost/latency and honor configured priority"
        );
    }

    #[test]
    fn parse_combo_strategy_is_case_and_whitespace_tolerant() {
        assert_eq!(parse_combo_strategy(" Cheapest "), ComboStrategy::Cheapest);
        assert_eq!(parse_combo_strategy("FASTEST"), ComboStrategy::Fastest);
        assert_eq!(parse_combo_strategy("Quality"), ComboStrategy::Quality);
        assert_eq!(
            parse_combo_strategy("Round-Robin"),
            ComboStrategy::RoundRobin
        );
        assert_eq!(parse_combo_strategy(""), ComboStrategy::Fallback);
        assert_eq!(parse_combo_strategy("bogus"), ComboStrategy::Fallback);
    }

    #[tokio::test]
    async fn combo_strategy_quality_orders_by_capability() {
        // Quality strategy keeps capability-tier order: a vision-required request
        // must try the vision-capable model (claude-3-sonnet) before the one that
        // lacks it (gpt-4o-mini), regardless of declared combo order.
        let models = vec![
            "openai/gpt-3.5-turbo".to_string(),
            "anthropic/claude-3-sonnet".to_string(),
        ];
        let mut required = HashSet::new();
        required.insert("vision".to_string());
        let pricing = PricingTable::new();
        let attempted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let attempted_clone = attempted.clone();
        let result = execute_combo_strategy_full(
            &models,
            Some("quality-combo"),
            ComboStrategy::Quality,
            &[],
            1,
            Some(&required),
            &pricing,
            |_| ModelCapacity::Available,
            move |model: &str| {
                let attempted_clone = attempted_clone.clone();
                let owned = model.to_string();
                async move {
                    attempted_clone.lock().unwrap().push(owned.clone());
                    Ok::<_, ComboAttemptError>(owned)
                }
            },
        )
        .await;
        assert!(result.is_ok(), "quality strategy should succeed");
        let attempted = attempted.lock().unwrap();
        assert_eq!(
            attempted.first().map(|s| s.as_str()),
            Some("anthropic/claude-3-sonnet"),
            "quality strategy must try the vision-capable model first"
        );
    }
}
