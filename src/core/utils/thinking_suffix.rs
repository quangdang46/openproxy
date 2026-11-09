//! Global `model(level)` / `model-level` thinking suffix helpers.
//!
//! Ports 9router `thinkingUnified` strip + apply:
//! - `stripThinkingSuffix` / `parseSuffix` level extraction
//! - `applyThinking` post-translate re-apply onto provider-native fields
//!
//! Levels: none|minimal|low|medium|high|xhigh|max.

use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::LazyLock;

use crate::core::translator::registry::Format;

/// Effort / thinking levels recognized in model suffixes.
pub const THINKING_LEVELS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Hyphen-suffix levels: discrete levels plus `auto`/`ultra` (parens parity).
const HYPHEN_SUFFIX_LEVELS: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "auto", "ultra",
];

/// 9router LEVEL_TO_BUDGET for Claude-style budget_tokens.
pub fn level_to_budget(level: &str) -> Option<u32> {
    match level.to_ascii_lowercase().as_str() {
        "none" => Some(0),
        "minimal" => Some(512),
        "low" => Some(1024),
        "medium" => Some(8192),
        "high" => Some(24576),
        "xhigh" => Some(32768),
        "max" => Some(128_000),
        _ => None,
    }
}

/// Numeric budget → nearest discrete level (9router `budgetToLevel`).
pub fn budget_to_level(budget: u32) -> Option<&'static str> {
    if budget == 0 {
        return None;
    }
    if budget <= 768 {
        Some("minimal")
    } else if budget <= 4096 {
        Some("low")
    } else if budget <= 16384 {
        Some("medium")
    } else if budget <= 28672 {
        Some("high")
    } else {
        Some("xhigh")
    }
}

/// Parse trailing thinking level from a model id.
///
/// Ports 9router `parseSuffix` (`thinkingUnified.js:34-46`):
/// - `foo(high)` / `foo (high)` — discrete level
/// - `foo(8192)` — numeric budget_tokens
/// - `foo(auto)` — auto intent
/// - `foo(ultra)` — ultra level
/// - `foo(none)` / `foo(off)` — disable (normalized to `"none"`)
/// - `foo-high` / `foo-medium` / … — hyphen suffix (levels + auto/ultra)
///
/// Returns `(upstream_model, Some(value))` when a suffix was stripped.
/// Numeric budgets are returned verbatim (e.g. `Some("8192")`); `"off"`
/// is normalized to `"none"`. Unknown parentheticals are left unstripped.
pub fn strip_thinking_suffix(model: &str) -> (&str, Option<&str>) {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return (model, None);
    }

    // Parenthetical: model(value) or model (value)
    if let Some((base, raw)) = split_paren_suffix(trimmed) {
        if base.is_empty() {
            return (trimmed, None);
        }
        let lower = raw.to_ascii_lowercase();
        // Numeric budget: model(8192) — return digits borrowed from input.
        if !lower.is_empty() && lower.bytes().all(|b| b.is_ascii_digit()) {
            return (base, Some(raw));
        }
        let canonical: Option<&'static str> = match lower.as_str() {
            "none" | "off" => Some("none"),
            "auto" => Some("auto"),
            "ultra" => Some("ultra"),
            _ => THINKING_LEVELS.iter().find(|l| **l == lower).copied(),
        };
        if let Some(level) = canonical {
            return (base, Some(level));
        }
        return (trimmed, None);
    }

    // Hyphen suffix: model-high (+ auto/ultra for consistency with parens)
    for level in HYPHEN_SUFFIX_LEVELS {
        let suffix = format!("-{level}");
        if let Some(base) = trimmed.strip_suffix(&suffix) {
            if !base.is_empty() {
                return (base, Some(*level));
            }
        }
    }

    (trimmed, None)
}

/// Split a trailing `(value)` suffix → `(base, raw_value)`.
///
/// Returns `None` when the model does not end with a well-formed
/// parenthetical (9router `stripThinkingSuffix` no-op case).
fn split_paren_suffix(model: &str) -> Option<(&str, &str)> {
    if !model.ends_with(')') {
        return None;
    }
    let open = model.rfind('(')?;
    let raw = model.get(open + 1..model.len() - 1)?;
    if raw.trim().is_empty() || raw.contains('(') || raw.contains(')') {
        return None;
    }
    Some((model[..open].trim_end(), raw.trim()))
}

/// Apply strip to an owned model string; returns (upstream, optional level).
pub fn strip_thinking_suffix_owned(model: &str) -> (String, Option<String>) {
    let (base, level) = strip_thinking_suffix(model);
    (base.to_string(), level.map(str::to_string))
}

/// Wire-format native thinking style (9router `thinkingFormat` / FORMAT_TO_NATIVE).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingNative {
    OpenAi,
    ClaudeBudget,
    ClaudeAdaptive,
    GeminiBudget,
    GeminiLevel,
    Zai,
    Qwen,
    DeepSeek,
    Kimi,
    MiniMax,
    /// No wire rewrite (Kiro handles thinking via system prefix / model suffix).
    Noop,
    /// Strict OpenAI Responses wire: `reasoning:{effort,summary:"auto"}` and
    /// NO top-level `reasoning_effort` (9router `applyFormat` case
    /// "openai-responses", thinkingUnified.js:275-289). Used by
    /// `transports[].thinkingFormat` on strict `/responses` legs
    /// (opencode-go, muse) that reject Chat-shaped thinking fields.
    OpenAiResponses,
}

/// Resolve native thinking wire format from target format + provider + model.
///
/// Mirrors 9router `resolveFormat` + capability heuristics without the full
/// capabilities matrix (provider/model exact overrides for high-traffic families).
pub fn resolve_thinking_native(
    target_format: Format,
    provider: &str,
    model: &str,
) -> ThinkingNative {
    // transports[].thinkingFormat wins over the provider/model defaults
    // (9router thinkingUnified.js:130-148 `resolveFormat`: the transport entry
    // matching the TARGET format declares the thinking shape that endpoint
    // accepts — strict /responses upstreams need reasoning:{effort,summary}).
    if let Some(native) = crate::core::chat::provider_transports(provider)
        .into_iter()
        .find(|t| t.format == target_format)
        .and_then(|t| t.thinking_format)
    {
        return native;
    }

    let p = provider.to_ascii_lowercase();
    let m = model.to_ascii_lowercase();

    // Provider-level thinkingFormat overrides (registry thinkingFormat)
    match p.as_str() {
        "glm" | "glm-cn" | "zai" | "zhipu" => return ThinkingNative::Zai,
        "qwen" | "qwen-code" | "dashscope" => return ThinkingNative::Qwen,
        "deepseek" | "ds" => return ThinkingNative::DeepSeek,
        "kimi" | "kimi-coding" | "moonshot" => return ThinkingNative::Kimi,
        "minimax" | "minimax-cn" => return ThinkingNative::MiniMax,
        "kiro" => return ThinkingNative::Noop,
        _ => {}
    }

    match target_format {
        Format::OpenAi | Format::OpenAiResponses | Format::OpenAiResponse | Format::Codex => {
            ThinkingNative::OpenAi
        }
        Format::Claude => {
            if is_claude_adaptive_model(&m) {
                ThinkingNative::ClaudeAdaptive
            } else {
                ThinkingNative::ClaudeBudget
            }
        }
        Format::Gemini | Format::Vertex => {
            if is_gemini_level_model(&m) {
                ThinkingNative::GeminiLevel
            } else {
                ThinkingNative::GeminiBudget
            }
        }
        Format::GeminiCli | Format::Antigravity => {
            if is_gemini_level_model(&m) {
                ThinkingNative::GeminiLevel
            } else {
                ThinkingNative::GeminiBudget
            }
        }
        Format::Kiro => ThinkingNative::Noop,
        // Cursor / Ollama / CommandCode: leave body alone (executors normalize).
        Format::Cursor | Format::Ollama | Format::CommandCode => ThinkingNative::Noop,
    }
}

/// Adaptive-thinking support predicate for Claude Haiku, ported from 9router
/// `isAdaptiveThinkingUnsupported` (`open-sse/translator/formats/claude.js:175`,
/// upstream v0.5.111 #5183): Haiku 3.x/4.x reject `thinking.type "adaptive"` +
/// `output_config.effort`; Haiku 5+ accepts ONLY adaptive. The version may
/// follow "haiku" (`haiku-4-5`, `haiku-5.5`, dated snapshots like
/// `claude-haiku-4-5-20251001`) or precede it (`claude-4.5-haiku`,
/// `claude-3-5-haiku`). Unversioned "haiku" keeps the old, safe downgrade.
///
/// Rust `regex` has no look-around, so the JS `(?<![\d.])` guard becomes a
/// consuming `(?:^|[^\d.])` prefix — identical leftmost-match semantics.
pub(crate) fn is_adaptive_thinking_unsupported(model: &str) -> bool {
    if !model.to_ascii_lowercase().contains("haiku") {
        return false;
    }
    static HAIKU_VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)haiku[-._]?(\d+)|(?:^|[^\d.])(\d+)(?:[-._]\d+)?[-._]?haiku")
            .expect("haiku version regex")
    });
    let Some(caps) = HAIKU_VERSION_RE.captures(model) else {
        // Unversioned haiku → keep the old, safe downgrade.
        return true;
    };
    let major = caps
        .get(1)
        .or_else(|| caps.get(2))
        .and_then(|m| m.as_str().parse::<u64>().ok());
    match major {
        Some(v) => v < 5,
        None => true,
    }
}

fn is_claude_adaptive_model(model: &str) -> bool {
    // 9router MODEL_CAPABILITIES: opus/sonnet 4.6+, opus-5 and sonnet-5 use
    // claude-adaptive. Haiku is version-gated: 5+ is adaptive-only (#5183),
    // older/unversioned budget.
    let m = model.to_ascii_lowercase();
    if m.contains("haiku") {
        return !is_adaptive_thinking_unsupported(model);
    }
    m.contains("opus-4.6")
        || m.contains("opus-4-6")
        || m.contains("opus-4.7")
        || m.contains("opus-4-7")
        || m.contains("opus-4.8")
        || m.contains("opus-4-8")
        || m.contains("opus-5")
        || m.contains("sonnet-4.6")
        || m.contains("sonnet-4-6")
        || m.contains("sonnet-5")
}

/// Whether a claude-adaptive model advertises the `xhigh` effort tier.
///
/// Ports the relevant `PATTERN_THINKING` entries in 9router
/// `thinkingLevels.js`: `*claude*4.6*` / `*claude*4-6*` resolve to
/// `CLAUDE_NO_XHIGH` (`["none","low","medium","high","max"]`); every other
/// claude-adaptive model falls through to the `claude-adaptive` format
/// default `budgetX` (`["none","low","medium","high","xhigh","max"]`).
/// `applyFormat` clamps `xhigh` → `high` only when the advertised set
/// lacks it.
fn claude_adaptive_supports_xhigh(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    !(m.contains("claude") && (m.contains("4.6") || m.contains("4-6")))
}

fn is_gemini_level_model(model: &str) -> bool {
    // Gemini 3.x/4.x (Argon) use thinkingLevel; 2.5 uses thinkingBudget.
    // 9router thinkingLevels.js `*gemini-4*` pattern + capabilities.js
    // `thinkingFormat: "gemini-level"` (commit 29fc880).
    let m = model.to_ascii_lowercase();
    m.contains("gemini-3")
        || m.contains("gemini3")
        || crate::core::translator::request::openai_to_gemini::is_gemini4_model(model)
}

/// Selectable `thinkingLevel` values per 9router `PATTERN_THINKING` /
/// `FORMAT_LEVELS["gemini-level"]` (thinkingLevels.js): Gemini 4 (Argon)
/// rejects `"minimal"` with 400 — LOW is the floor and thinking cannot be
/// disabled; every other gemini-level model keeps the minimal|low|medium|high
/// enum.
fn gemini_level_supported_levels(model: &str) -> &'static [&'static str] {
    if crate::core::translator::request::openai_to_gemini::is_gemini4_model(model) {
        &["low", "medium", "high"]
    } else {
        &["minimal", "low", "medium", "high"]
    }
}

/// True when the body already carries a client- or settings-provided thinking intent.
///
/// Used to avoid double-applying when `providerThinking` already set fields and
/// there is no model-suffix override.
pub fn body_has_thinking_intent(body: &Value) -> bool {
    if body
        .get("thinking")
        .is_some_and(|v| !v.is_null() && v != &Value::Bool(false))
    {
        return true;
    }
    if body
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
    {
        return true;
    }
    if body
        .pointer("/reasoning/effort")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
    {
        return true;
    }
    if body
        .pointer("/output_config/effort")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
    {
        return true;
    }
    if body
        .get("enable_thinking")
        .and_then(Value::as_bool)
        .is_some()
    {
        return true;
    }
    if body.get("thinkingConfig").is_some()
        || body.pointer("/generationConfig/thinkingConfig").is_some()
        || body
            .pointer("/request/generationConfig/thinkingConfig")
            .is_some()
    {
        return true;
    }
    false
}

/// Strip all known thinking wire fields (9router `stripAll`).
fn strip_all_thinking_fields(body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    obj.remove("thinking");
    obj.remove("reasoning_effort");
    obj.remove("reasoning");
    obj.remove("thinkingConfig");
    obj.remove("enable_thinking");
    obj.remove("thinking_budget");
    obj.remove("output_config");
    if let Some(gc) = obj
        .get_mut("generationConfig")
        .and_then(Value::as_object_mut)
    {
        gc.remove("thinkingConfig");
    }
    if let Some(req) = obj.get_mut("request").and_then(Value::as_object_mut) {
        if let Some(gc) = req
            .get_mut("generationConfig")
            .and_then(Value::as_object_mut)
        {
            gc.remove("thinkingConfig");
        }
    }
}

fn ensure_object<'a>(map: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    if !map.get(key).map(Value::is_object).unwrap_or(false) {
        map.insert(key.to_string(), Value::Object(Map::new()));
    }
    map.get_mut(key)
        .and_then(Value::as_object_mut)
        .expect("just inserted object")
}

fn get_gemini_generation_config(body: &mut Value) -> Option<&mut Map<String, Value>> {
    let obj = body.as_object_mut()?;
    if obj.get("request").map(Value::is_object).unwrap_or(false) {
        let req = ensure_object(obj, "request");
        return Some(ensure_object(req, "generationConfig"));
    }
    Some(ensure_object(obj, "generationConfig"))
}

fn set_gemini_thinking(body: &mut Value, tc: Value) {
    if let Some(gc) = get_gemini_generation_config(body) {
        gc.insert("thinkingConfig".into(), tc);
    }
}

fn ensure_gemini_output_floor(body: &mut Value, floor: u32) {
    if let Some(gc) = get_gemini_generation_config(body) {
        let current = gc
            .get("maxOutputTokens")
            .and_then(Value::as_u64)
            .map(|v| v as u32);
        if current.map(|c| c < floor).unwrap_or(true) {
            gc.insert("maxOutputTokens".into(), json!(floor));
        }
    }
}

fn gemini_budget_output_floor(budget: i64) -> u32 {
    if budget < 0 {
        return 32768;
    }
    if budget <= 1024 {
        8192
    } else if budget <= 8192 {
        16384
    } else if budget <= 24576 {
        32768
    } else {
        65535
    }
}

fn gemini_level_output_floor(level: &str) -> u32 {
    match level {
        "minimal" => 4096,
        "low" => 8192,
        "medium" => 16384,
        "high" => 65535,
        _ => 65535,
    }
}

fn effort_to_gemini_thinking_level(level: &str) -> &str {
    // Gemini 3 enum: minimal|low|medium|high — clamp max/xhigh/none/auto
    match level {
        "none" | "off" => "minimal",
        "xhigh" | "max" | "auto" => "high",
        other => other,
    }
}

fn to_kimi_reasoning_effort(level: &str) -> Option<&'static str> {
    match level {
        "auto" => Some("high"),
        "minimal" => Some("low"),
        "xhigh" => Some("max"),
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "max" => Some("max"),
        "none" => None,
        _ => None,
    }
}

/// Unified thinking intent: 9router `extractThinking` / `captureThinking`
/// result (`{ mode, budget?, level? }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThinkingIntent {
    /// Thinking disabled (`none` / `off` / `thinking.disabled` / budget 0).
    None_,
    /// Provider-default reasoning (`auto` / adaptive-enabled / budget -1).
    Auto,
    /// Numeric token budget (`model(8192)` / `budget_tokens` / `thinkingBudget`).
    Budget(u32),
    /// Discrete effort level (`minimal|low|medium|high|xhigh|max|ultra`).
    Level(String),
}

/// Convert a suffix / effort string to a unified intent.
///
/// Ports 9router `parseSuffix` override (numeric → budget, `auto` → auto,
/// `none`/`off` → disable, otherwise discrete level). Returns `None` for
/// empty input.
pub fn suffix_to_intent(value: &str) -> Option<ThinkingIntent> {
    let raw = value.trim().to_ascii_lowercase();
    if raw.is_empty() {
        return None;
    }
    if raw == "none" || raw == "off" {
        return Some(ThinkingIntent::None_);
    }
    if raw == "auto" {
        return Some(ThinkingIntent::Auto);
    }
    if raw.bytes().all(|b| b.is_ascii_digit()) {
        return raw.parse::<u32>().ok().map(ThinkingIntent::Budget);
    }
    Some(ThinkingIntent::Level(raw))
}

/// Extract unified thinking intent from a request body (mixed shapes).
///
/// Port of 9router `extractThinking` (`thinkingUnified.js:50-103`): checks
/// `output_config.effort` → `reasoning_effort` / `reasoning.effort` →
/// Claude `thinking` → Gemini `thinkingConfig` (top-level, `generationConfig`,
/// or `request.generationConfig`) → Qwen `enable_thinking`. Returns `None`
/// when no thinking intent is present.
pub fn extract_thinking_intent(body: &Value) -> Option<ThinkingIntent> {
    // Claude output_config.effort (explicit) — priority over adaptive thinking.
    if let Some(e) = body
        .pointer("/output_config/effort")
        .and_then(Value::as_str)
    {
        if !e.trim().is_empty() {
            return suffix_to_intent(e);
        }
    }

    // OpenAI chat / Responses shape — effort first (zai sends both a thinking
    // object and reasoning.effort).
    let effort = body
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .or_else(|| {
            body.get("reasoning").and_then(|r| {
                if r.is_object() {
                    r.get("effort").and_then(Value::as_str)
                } else {
                    None
                }
            })
        });
    if let Some(e) = effort {
        if !e.trim().is_empty() {
            return suffix_to_intent(e);
        }
    }

    // Claude shape.
    if let Some(t) = body.get("thinking") {
        if t.is_object() {
            let ttype = t.get("type").and_then(Value::as_str).unwrap_or("");
            if ttype == "disabled" {
                return Some(ThinkingIntent::None_);
            }
            if ttype == "adaptive" || ttype == "enabled" {
                if let Some(b) = t.get("budget_tokens").and_then(Value::as_u64) {
                    if b > 0 && b <= u32::MAX as u64 {
                        return Some(ThinkingIntent::Budget(b as u32));
                    }
                }
                return Some(ThinkingIntent::Auto);
            }
        }
    }

    // Gemini shape (top-level, generationConfig, or request envelope).
    let tc = body.get("thinkingConfig").or_else(|| {
        body.pointer("/generationConfig/thinkingConfig")
            .or_else(|| body.pointer("/request/generationConfig/thinkingConfig"))
    });
    if let Some(tc) = tc {
        if tc.is_object() {
            if let Some(lvl) = tc.get("thinkingLevel").and_then(Value::as_str) {
                return suffix_to_intent(lvl);
            }
            if let Some(tb) = tc.get("thinkingBudget").and_then(Value::as_i64) {
                if tb == 0 {
                    return Some(ThinkingIntent::None_);
                }
                if tb < 0 {
                    return Some(ThinkingIntent::Auto);
                }
                return Some(ThinkingIntent::Budget(tb as u32));
            }
        }
    }

    // Qwen shape.
    if let Some(enabled) = body.get("enable_thinking").and_then(Value::as_bool) {
        if !enabled {
            return Some(ThinkingIntent::None_);
        }
        if let Some(tb) = body.get("thinking_budget").and_then(Value::as_u64) {
            if tb > 0 && tb <= u32::MAX as u64 {
                return Some(ThinkingIntent::Budget(tb as u32));
            }
        }
        return Some(ThinkingIntent::Auto);
    }

    None
}

/// Capture the OpenAI-shaped client's ask for thinking *text* — port of
/// 9router `openAIThinkingDisplay` (thinkingUnified.js:116-126). Claude only
/// returns thinking content with `thinking.display: "summarized"`, a field
/// OpenAI has no equivalent for:
/// - Responses API: `reasoning.summary` (non-empty, non-"none") → summarized.
///   When `reasoning` is an object its `summary` is authoritative — the
///   function returns early and never falls through to `reasoning_effort`.
/// - Chat Completions: any string `reasoning_effort` → summarized.
fn openai_thinking_display(body: &Value) -> Option<String> {
    let obj = body.as_object()?;
    if let Some(reasoning) = obj.get("reasoning").filter(|v| v.is_object()) {
        return reasoning
            .get("summary")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && *s != "none")
            .map(|_| "summarized".to_string());
    }
    obj.get("reasoning_effort")
        .and_then(Value::as_str)
        .map(|_| "summarized".to_string())
}

/// Alias of [`extract_thinking_intent`], named for clarity at the call-site
/// where intent is snapshotted before format translation (JS `captureThinking`).
pub fn capture_thinking(body: &Value) -> Option<ThinkingIntent> {
    extract_thinking_intent(body)
}

/// OpenAI wire level sets (9router `thinkingLevels.js`). The default
/// `openai` format list has no `max`; codex-flavoured rows add it.
const OPENAI_WIRE_LEVELS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh"];
const GPT56_MAX_LEVELS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];
const GPT56_MAX_ULTRA_LEVELS: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];
/// `*codex*` pattern — codex cannot disable thinking.
const CODEX_PATTERN_LEVELS: &[&str] = &["low", "medium", "high", "xhigh"];

/// Upstream #5186: OpenAI documents `reasoning.effort: "max"` for GPT-5.6 and
/// GPT-6, so an `openai-compatible-*` node serving those ids gets the codex
/// level set instead of the `openai` format list that clamps `max`→`xhigh`.
static OPENAI_MAX_EFFORT_MODEL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|/)gpt-(?:5\.6|6)(?:[.-]|$)").expect("gpt max-effort regex")
});

/// `getThinkingLevels` reduced to the branches that can change
/// `max`/`ultra` normalization on the OpenAI wire:
/// - `codex` provider rows (`*gpt-6*`, `*gpt-5.6-sol|terra|luna*`, `*codex*`)
/// - `openai-compatible-*` nodes serving `gpt-5.6`/`gpt-6` (#5186)
fn openai_supported_levels(provider: &str, model: &str) -> &'static [&'static str] {
    let base = strip_trailing_paren(model);
    let m = base.to_ascii_lowercase();
    let p = provider.to_ascii_lowercase();
    if p == "codex" || p == "cx" {
        if m.contains("gpt-6") {
            return GPT56_MAX_LEVELS;
        }
        if m.contains("gpt-5.6-sol") || m.contains("gpt-5.6-terra") {
            return GPT56_MAX_ULTRA_LEVELS;
        }
        if m.contains("gpt-5.6-luna") {
            return GPT56_MAX_LEVELS;
        }
        if m.contains("codex") {
            return CODEX_PATTERN_LEVELS;
        }
        return OPENAI_WIRE_LEVELS;
    }
    if p.starts_with("openai-compatible-") && OPENAI_MAX_EFFORT_MODEL_RE.is_match(base) {
        return GPT56_MAX_LEVELS;
    }
    OPENAI_WIRE_LEVELS
}

/// Model id minus a trailing `(…)` suffix (upstream `baseId` in
/// `getThinkingLevels`).
fn strip_trailing_paren(model: &str) -> &str {
    let t = model.trim_end();
    if t.ends_with(')') {
        if let Some(open) = t.rfind('(') {
            let inner = &t[open + 1..t.len() - 1];
            if !inner.trim().is_empty() && !inner.contains(['(', ')']) {
                return t[..open].trim_end();
            }
        }
    }
    t
}

/// Normalize an OpenAI wire level (9router `normalizeOpenAILevel`):
/// `max`/`ultra` survive only when the resolved level set supports them;
/// `ultra` folds to `max` when `max` is supported but `ultra` is not.
fn normalize_openai_level<'a>(level: &'a str, supported: &[&'static str]) -> &'a str {
    match level {
        "max" => {
            if supported.contains(&"max") {
                "max"
            } else {
                "xhigh"
            }
        }
        "ultra" => {
            if supported.contains(&"ultra") {
                "ultra"
            } else if supported.contains(&"max") {
                "max"
            } else {
                "xhigh"
            }
        }
        other => other,
    }
}

/// Apply a discrete thinking level onto the post-translate body in provider-native form.
///
/// Port of 9router `applyThinking` for the common case where the config is a
/// level override from `model(level)` / `model-level` suffix. Numeric
/// (`model(8192)`), `auto`, and `ultra` values are honored; unknown values
/// fall back to `medium` downstream.
pub fn apply_thinking_level(
    target_format: Format,
    provider: &str,
    model: &str,
    body: &mut Value,
    level: &str,
) {
    if !body.is_object() {
        return;
    }
    let Some(intent) = suffix_to_intent(level) else {
        return;
    };
    apply_thinking_intent(target_format, provider, model, body, &intent);
}

/// Apply a unified thinking config onto the post-translate body in the
/// resolved provider-native format (9router `applyThinking` + `applyFormat`).
///
/// Strips all known thinking fields, then writes the native representation.
/// No-op when the body is not an object or the native format is `Noop`.
pub fn apply_thinking_intent(
    target_format: Format,
    provider: &str,
    model: &str,
    body: &mut Value,
    intent: &ThinkingIntent,
) {
    if !body.is_object() {
        return;
    }
    let native = resolve_thinking_native(target_format, provider, model);
    if native == ThinkingNative::Noop {
        return;
    }

    // 9router `applyFormat` canDisable gate: models whose caps carry
    // thinkingCanDisable:false (Claude Opus 5.5, Fable 5.1, GLM-5.3, …)
    // reject an explicit "off" — a "none" intent clamps to `minimal` effort
    // instead of writing the disabled switch.
    let caps = crate::core::combo::capabilities::get_capabilities_for_model(provider, model);
    let can_disable = caps.thinking_can_disable;
    let clamped;
    let intent = if *intent == ThinkingIntent::None_ && !can_disable {
        clamped = ThinkingIntent::Level("minimal".to_string());
        &clamped
    } else {
        intent
    };

    let none = *intent == ThinkingIntent::None_;
    // 9router `toBudget` (no range clamp — no caps matrix here): numeric
    // budgets pass through, `auto` is -1, levels map via LEVEL_TO_BUDGET.
    let budget: Option<i64> = match intent {
        ThinkingIntent::Budget(b) => Some(*b as i64),
        ThinkingIntent::Auto => Some(-1),
        ThinkingIntent::Level(l) => level_to_budget(l).map(|b| b as i64),
        ThinkingIntent::None_ => None,
    };
    // 9router `toLevel`: budget → nearest discrete level (fallback `medium`).
    let level_owned: String = match intent {
        ThinkingIntent::Level(l) => l.to_ascii_lowercase(),
        ThinkingIntent::Budget(b) => budget_to_level(*b).unwrap_or("medium").to_string(),
        ThinkingIntent::Auto => "auto".to_string(),
        ThinkingIntent::None_ => "minimal".to_string(),
    };
    let level = level_owned.as_str();
    // 9router `applyThinking`: Anthropic's `display` (summarized | omitted)
    // decides whether thinking text comes back at all — keep what the client
    // asked for instead of resetting it. `body.thinking.display` wins; the
    // captured OpenAI intent (reasoning.summary / reasoning_effort) fills in
    // when the client sent no display of its own. Read BEFORE strip_all.
    let display = body
        .pointer("/thinking/display")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| openai_thinking_display(body));
    strip_all_thinking_fields(body);

    match native {
        ThinkingNative::OpenAi => {
            if none {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("reasoning_effort".into(), Value::String("none".into()));
                }
                return;
            }
            let supported = openai_supported_levels(provider, model);
            let effort = normalize_openai_level(level, supported);
            if let Some(obj) = body.as_object_mut() {
                obj.insert("reasoning_effort".into(), Value::String(effort.into()));
            }
            // Codex / Responses: also set reasoning.effort when body looks like Responses API
            if matches!(
                target_format,
                Format::Codex | Format::OpenAiResponses | Format::OpenAiResponse
            ) || body.get("input").is_some()
            {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert(
                        "reasoning".into(),
                        json!({"effort": effort, "summary": "auto"}),
                    );
                }
            }
        }
        ThinkingNative::ClaudeBudget => {
            if none {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("thinking".into(), json!({"type": "disabled"}));
                }
                return;
            }
            // 9router `toBudget(eff, caps.thinkingRange)`: numeric budgets pass
            // through verbatim (or -1 → enabled w/o budget); levels map.
            // `budget == 0` is unreachable here (`none` returned above).
            // `display` rides along on the thinking object (upstream
            // applyFormat claude-budget arm).
            let b = budget.unwrap_or(8192);
            if let Some(obj) = body.as_object_mut() {
                let mut thinking = serde_json::Map::new();
                thinking.insert("type".into(), json!("enabled"));
                if b >= 0 {
                    thinking.insert("budget_tokens".into(), json!(b));
                }
                if let Some(d) = display.as_deref() {
                    thinking.insert("display".into(), json!(d));
                }
                obj.insert("thinking".into(), Value::Object(thinking));
            }
        }
        ThinkingNative::ClaudeAdaptive => {
            if none {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("thinking".into(), json!({"type": "disabled"}));
                }
                return;
            }
            // 9router `applyFormat` claude-adaptive (thinkingUnified.js):
            // `auto` → `high`, `minimal` → `low` (Claude effort has no
            // "minimal" tier), and `xhigh` is model-gated — Opus/Sonnet
            // 4.6 reject it, so it clamps to `high` only there; 4.7+ /
            // Sonnet 5 / Opus 5.x and Haiku 5+ advertise it and get it
            // verbatim, as does `max`.
            let effort = match level {
                "auto" => "high",
                "minimal" => "low",
                "xhigh" if !claude_adaptive_supports_xhigh(model) => "high",
                other => other,
            };
            if let Some(obj) = body.as_object_mut() {
                // 9router `applyFormat` claude-adaptive (thinkingUnified.js):
                // only models that CAN disable thinking get the explicit
                // adaptive switch — permanently adaptive models (Opus 5.5,
                // Fable 5.1) take output_config.effort directly. Claude ones
                // still get the switch when it carries a requested display:
                // display defaults to "omitted" there, so dropping it hides
                // the thinking text. A thinking-off request (clamped to
                // minimal) asks for no thinking text.
                let mut thinking = serde_json::Map::new();
                thinking.insert("type".into(), json!("adaptive"));
                if let Some(d) = display.as_deref() {
                    thinking.insert("display".into(), json!(d));
                }
                if can_disable {
                    // 9router parity: output_config.effort alone does NOT turn
                    // thinking on — Anthropic requires an explicit
                    // thinking:{type:"adaptive"} on Opus/Sonnet 4.6+, and
                    // Anthropic-compatible shims default thinking off. Send both.
                    obj.insert("thinking".into(), Value::Object(thinking));
                } else if display.is_some() && level != "minimal" && model.to_ascii_lowercase().contains("claude") {
                    obj.insert("thinking".into(), Value::Object(thinking));
                }
                obj.insert("output_config".into(), json!({"effort": effort}));
            }
        }
        ThinkingNative::GeminiBudget => {
            if none {
                set_gemini_thinking(body, json!({"thinkingBudget": 0, "includeThoughts": false}));
                return;
            }
            let b = budget.unwrap_or(-1);
            set_gemini_thinking(body, json!({"thinkingBudget": b, "includeThoughts": true}));
            ensure_gemini_output_floor(body, gemini_budget_output_floor(b));
        }
        ThinkingNative::GeminiLevel => {
            let glevel_raw = if none {
                "minimal"
            } else {
                effort_to_gemini_thinking_level(&level)
            };
            // 9router thinkingUnified.js `gemini-level` arm: a level outside
            // the model's advertised set clamps to the LOWEST supported one —
            // Gemini 4 (Argon) has no "minimal" tier, so none/auto intents
            // land on "low" instead of 400ing upstream.
            let supported = gemini_level_supported_levels(model);
            let glevel = if supported.contains(&glevel_raw) {
                glevel_raw
            } else {
                supported[0]
            };
            set_gemini_thinking(
                body,
                json!({
                    "thinkingLevel": glevel,
                    "includeThoughts": glevel != "minimal",
                }),
            );
            ensure_gemini_output_floor(body, gemini_level_output_floor(glevel));
        }
        ThinkingNative::Zai => {
            if none {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("enable_thinking".into(), Value::Bool(false));
                }
                return;
            }
            if let Some(obj) = body.as_object_mut() {
                obj.insert("thinking".into(), json!({"type": "enabled"}));
            }
        }
        ThinkingNative::Qwen => {
            if none {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("enable_thinking".into(), Value::Bool(false));
                }
                return;
            }
            if let Some(obj) = body.as_object_mut() {
                obj.insert("enable_thinking".into(), Value::Bool(true));
                // 9router qwen: only finite positive budgets are written.
                if let Some(b) = budget {
                    if b > 0 {
                        obj.insert("thinking_budget".into(), json!(b));
                    }
                }
            }
        }
        ThinkingNative::DeepSeek => {
            if none {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("thinking".into(), json!({"type": "disabled"}));
                }
                return;
            }
            let effort = if level == "xhigh" || level == "max" {
                "max"
            } else {
                "high"
            };
            if let Some(obj) = body.as_object_mut() {
                obj.insert("thinking".into(), json!({"type": "enabled"}));
                obj.insert("reasoning_effort".into(), Value::String(effort.into()));
            }
        }
        ThinkingNative::Kimi => {
            if none {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("thinking".into(), json!({"type": "disabled"}));
                }
                return;
            }
            if let Some(effort) = to_kimi_reasoning_effort(&level) {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("reasoning_effort".into(), Value::String(effort.into()));
                }
            }
        }
        ThinkingNative::MiniMax => {
            let t = if none { "disabled" } else { "adaptive" };
            if let Some(obj) = body.as_object_mut() {
                obj.insert("thinking".into(), json!({"type": t}));
            }
        }
        ThinkingNative::OpenAiResponses => {
            // 9router applyFormat "openai-responses" (thinkingUnified.js:275-289):
            // effort nests under reasoning{effort,summary}; "none" omits
            // reasoning entirely; top-level reasoning_effort is ALWAYS deleted
            // — strict upstreams (Meta, OpenCode Go) reject it as an unknown
            // parameter.
            if !none {
                let supported = openai_supported_levels(provider, model);
                let effort = normalize_openai_level(level, supported);
                if let Some(obj) = body.as_object_mut() {
                    let mut reasoning = obj
                        .get("reasoning")
                        .filter(|r| r.is_object() && !r.is_array())
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    reasoning.insert("effort".into(), Value::String(effort.into()));
                    if !reasoning.contains_key("summary") {
                        reasoning.insert("summary".into(), Value::String("auto".into()));
                    }
                    obj.insert("reasoning".into(), Value::Object(reasoning));
                }
            } else if let Some(obj) = body.as_object_mut() {
                obj.remove("reasoning");
            }
            if let Some(obj) = body.as_object_mut() {
                obj.remove("reasoning_effort");
            }
        }
        ThinkingNative::Noop => {}
    }
}

/// Post-translate re-apply entry point.
///
/// Ports 9router `translator/index.js:111-120` universal
/// `captureThinking` → `applyThinking`:
///
/// - When `suffix_level` is `Some`, it is the explicit override (9router
///   `parseSuffix` wins over captured intent) — always apply.
/// - When `suffix_level` is `None`, capture intent from the (translated) body
///   via [`extract_thinking_intent`] and normalize it into the target-native
///   format. No intent → leave the body untouched.
///
/// `stream` is accepted for call-site compatibility and intentionally ignored:
/// 9router `applyThinking` runs on both streaming and non-streaming requests,
/// so early-returning on `stream == false` would drop thinking config on
/// non-streaming calls.
pub fn reapply_thinking_after_translate(
    target_format: Format,
    provider: &str,
    model: &str,
    body: &mut Value,
    suffix_level: Option<&str>,
    stream: bool,
) {
    let _ = stream;
    if let Some(level) = suffix_level {
        // Suffix override wins (numeric / auto / ultra / level / none).
        if let Some(intent) = suffix_to_intent(level) {
            apply_thinking_intent(target_format, provider, model, body, &intent);
        }
        return;
    }
    // Universal capture → apply: normalize translated-body intent to native.
    if let Some(intent) = extract_thinking_intent(body) {
        apply_thinking_intent(target_format, provider, model, body, &intent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_paren_high() {
        assert_eq!(
            strip_thinking_suffix("gpt-4o(high)"),
            ("gpt-4o", Some("high"))
        );
        assert_eq!(
            strip_thinking_suffix("gpt-4o (medium)"),
            ("gpt-4o", Some("medium"))
        );
    }

    #[test]
    fn strips_hyphen_effort() {
        assert_eq!(
            strip_thinking_suffix("grok-4.5-high"),
            ("grok-4.5", Some("high"))
        );
        assert_eq!(strip_thinking_suffix("o3-low"), ("o3", Some("low")));
    }

    #[test]
    fn leaves_plain_models() {
        assert_eq!(strip_thinking_suffix("gpt-4o"), ("gpt-4o", None));
        assert_eq!(
            strip_thinking_suffix("claude-sonnet-4"),
            ("claude-sonnet-4", None)
        );
    }

    #[test]
    fn budget_map_matches_9router() {
        assert_eq!(level_to_budget("low"), Some(1024));
        assert_eq!(level_to_budget("high"), Some(24576));
        assert_eq!(level_to_budget("medium"), Some(8192));
        assert_eq!(budget_to_level(1024), Some("low"));
        assert_eq!(budget_to_level(24576), Some("high"));
    }

    #[test]
    fn strip_apply_roundtrip_openai() {
        let (clean, level) = strip_thinking_suffix_owned("gpt-5(high)");
        assert_eq!(clean, "gpt-5");
        assert_eq!(level.as_deref(), Some("high"));

        let mut body = json!({"messages": [], "model": clean});
        reapply_thinking_after_translate(
            Format::OpenAi,
            "openai",
            &clean,
            &mut body,
            level.as_deref(),
            true,
        );
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn strip_apply_roundtrip_claude_budget() {
        let (clean, level) = strip_thinking_suffix_owned("claude-haiku-4.5-high");
        assert_eq!(clean, "claude-haiku-4.5");
        assert_eq!(level.as_deref(), Some("high"));

        let mut body = json!({"messages": [], "model": clean});
        reapply_thinking_after_translate(
            Format::Claude,
            "claude",
            &clean,
            &mut body,
            level.as_deref(),
            true,
        );
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 24576);
    }

    #[test]
    fn strip_apply_claude_adaptive_sends_both_fields() {
        let (clean, level) = strip_thinking_suffix_owned("claude-opus-4.7(medium)");
        assert_eq!(clean, "claude-opus-4.7");
        let mut body = json!({"messages": []});
        reapply_thinking_after_translate(
            Format::Claude,
            "claude",
            &clean,
            &mut body,
            level.as_deref(),
            true,
        );
        assert_eq!(body["output_config"]["effort"], "medium");
        assert_eq!(body["thinking"]["type"], "adaptive");
    }

    #[test]
    fn claude_adaptive_xhigh_is_model_gated() {
        // 9router thinkingLevels.js PATTERN_THINKING: *claude*4.6* /
        // *claude*4-6* → CLAUDE_NO_XHIGH clamps xhigh→high; 4.7+/Sonnet 5/
        // Opus 5.x advertise xhigh and get it verbatim.
        for model in [
            "claude-opus-4.7",
            "claude-sonnet-5",
            "claude-opus-5",
            "claude-haiku-5.5",
        ] {
            let mut body = json!({"messages": []});
            apply_thinking_level(Format::Claude, "claude", model, &mut body, "xhigh");
            assert_eq!(body["output_config"]["effort"], "xhigh", "{model}");
        }
        for model in ["claude-opus-4.6", "claude-sonnet-4-6", "claude-sonnet-4.6"] {
            let mut body = json!({"messages": []});
            apply_thinking_level(Format::Claude, "claude", model, &mut body, "xhigh");
            assert_eq!(body["output_config"]["effort"], "high", "{model}");
        }
        // max passes through verbatim on claude-adaptive.
        let mut body = json!({"messages": []});
        apply_thinking_level(
            Format::Claude,
            "claude",
            "claude-opus-4.6",
            &mut body,
            "max",
        );
        assert_eq!(body["output_config"]["effort"], "max");
    }

    #[test]
    fn openai_max_clamps_to_xhigh() {
        let mut body = json!({"messages": []});
        apply_thinking_level(Format::OpenAi, "openai", "gpt-5", &mut body, "max");
        assert_eq!(body["reasoning_effort"], "xhigh");
    }

    #[test]
    fn openai_compatible_gpt56_gpt6_preserve_max() {
        // Upstream #5186: openai-compatible-* nodes serving gpt-5.6/gpt-6 keep
        // reasoning_effort "max" instead of clamping to xhigh.
        for model in [
            "gpt-5.6-sol",
            "gpt-6-sol",
            "gpt-6.1-terra",
            "vendor/gpt-6-luna",
        ] {
            let mut body = json!({"messages": []});
            apply_thinking_level(
                Format::OpenAi,
                "openai-compatible-mytier",
                model,
                &mut body,
                "max",
            );
            assert_eq!(body["reasoning_effort"], "max", "{model}");
        }
        // Same node, non-GPT-5.6/6 model still clamps.
        let mut body = json!({"messages": []});
        apply_thinking_level(
            Format::OpenAi,
            "openai-compatible-mytier",
            "gpt-5.5",
            &mut body,
            "max",
        );
        assert_eq!(body["reasoning_effort"], "xhigh");
        // ultra folds to max on the max-capable set (no ultra in CODEX_GPT_5_6_LEVELS).
        let mut body = json!({"messages": []});
        apply_thinking_level(
            Format::OpenAi,
            "openai-compatible-mytier",
            "gpt-6-sol",
            &mut body,
            "ultra",
        );
        assert_eq!(body["reasoning_effort"], "max");
    }

    #[test]
    fn codex_provider_preserves_max_and_ultra() {
        // Codex PATTERN_THINKING rows: gpt-6* → max, gpt-5.6-sol/terra → +ultra.
        let mut body = json!({"messages": []});
        apply_thinking_level(Format::OpenAi, "codex", "gpt-6-sol", &mut body, "max");
        assert_eq!(body["reasoning_effort"], "max");
        let mut body = json!({"messages": []});
        apply_thinking_level(Format::OpenAi, "codex", "gpt-5.6-sol", &mut body, "ultra");
        assert_eq!(body["reasoning_effort"], "ultra");
        // gpt-5.6-luna has no ultra → folds to max.
        let mut body = json!({"messages": []});
        apply_thinking_level(Format::OpenAi, "codex", "gpt-5.6-luna", &mut body, "ultra");
        assert_eq!(body["reasoning_effort"], "max");
    }

    #[test]
    fn none_disables_claude() {
        let mut body =
            json!({"messages": [], "thinking": {"type": "enabled", "budget_tokens": 1000}});
        apply_thinking_level(
            Format::Claude,
            "claude",
            "claude-haiku-4.5",
            &mut body,
            "none",
        );
        assert_eq!(body["thinking"]["type"], "disabled");
    }

    #[test]
    fn reapply_normalizes_body_intent_without_suffix() {
        // Universal capture → apply: numeric budget survives a normalize round-trip.
        let mut body = json!({
            "messages": [],
            "thinking": {"type": "enabled", "budget_tokens": 10000}
        });
        reapply_thinking_after_translate(
            Format::Claude,
            "claude",
            "claude-haiku-4.5",
            &mut body,
            None,
            true,
        );
        assert_eq!(body["thinking"]["budget_tokens"], 10000);
    }

    #[test]
    fn reapply_converts_cross_format_intent_without_suffix() {
        // OpenAI reasoning_effort on a Claude target normalizes to thinking.
        let mut body = json!({"messages": [], "reasoning_effort": "high"});
        reapply_thinking_after_translate(
            Format::Claude,
            "claude",
            "claude-haiku-4.5",
            &mut body,
            None,
            true,
        );
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 24576);
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn reapply_preserves_thinking_display_claude_budget() {
        // 9router applyThinking keeps `body.thinking.display` across
        // stripAll → applyFormat (claude-budget arm re-adds it). Without
        // this the OpenAI→Claude translator's display:"summarized" is lost.
        let mut body = json!({
            "messages": [],
            "thinking": {"type": "enabled", "budget_tokens": 10000, "display": "summarized"}
        });
        reapply_thinking_after_translate(
            Format::Claude,
            "claude",
            "claude-haiku-4.5",
            &mut body,
            None,
            true,
        );
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 10000);
        assert_eq!(body["thinking"]["display"], "summarized");
    }

    #[test]
    fn reapply_preserves_thinking_display_claude_adaptive() {
        // claude-adaptive arm: display rides on {type:"adaptive"}.
        let mut body = json!({
            "messages": [],
            "thinking": {"type": "adaptive", "display": "summarized"}
        });
        reapply_thinking_after_translate(
            Format::Claude,
            "claude",
            "claude-sonnet-5",
            &mut body,
            None,
            true,
        );
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["thinking"]["display"], "summarized");
        assert_eq!(body["output_config"]["effort"], "high");
    }

    #[test]
    fn reapply_display_from_openai_reasoning_effort_fallback() {
        // No thinking.display on the translated body, but a surviving
        // reasoning_effort still asks for the thinking text — upstream
        // openAIThinkingDisplay maps it to "summarized".
        let mut body = json!({"messages": [], "reasoning_effort": "high"});
        reapply_thinking_after_translate(
            Format::Claude,
            "claude",
            "claude-haiku-4.5",
            &mut body,
            None,
            true,
        );
        assert_eq!(body["thinking"]["display"], "summarized");
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn reapply_reasoning_summary_none_leaves_display_absent() {
        // reasoning.summary "none" is NOT a request for thinking text.
        let mut body = json!({
            "messages": [],
            "reasoning": {"effort": "high", "summary": "none"}
        });
        reapply_thinking_after_translate(
            Format::Claude,
            "claude",
            "claude-haiku-4.5",
            &mut body,
            None,
            true,
        );
        assert!(body["thinking"].get("display").is_none(), "{body}");
    }

    #[test]
    fn reapply_noop_without_suffix_or_intent() {
        let mut body = json!({"messages": []});
        reapply_thinking_after_translate(
            Format::Claude,
            "claude",
            "claude-haiku-4.5",
            &mut body,
            None,
            true,
        );
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn non_streaming_still_applies_thinking() {
        // 9router applyThinking runs regardless of stream; no early-return.
        let mut body = json!({"messages": []});
        reapply_thinking_after_translate(
            Format::OpenAi,
            "openai",
            "gpt-5",
            &mut body,
            Some("high"),
            false,
        );
        assert_eq!(body["reasoning_effort"], "high");
    }

    #[test]
    fn suffix_overrides_existing_thinking() {
        let mut body = json!({
            "messages": [],
            "thinking": {"type": "enabled", "budget_tokens": 10000}
        });
        reapply_thinking_after_translate(
            Format::Claude,
            "claude",
            "claude-haiku-4.5",
            &mut body,
            Some("low"),
            true,
        );
        assert_eq!(body["thinking"]["budget_tokens"], 1024);
    }

    #[test]
    fn glm_zai_format_enable_thinking() {
        let mut body = json!({"messages": []});
        apply_thinking_level(Format::OpenAi, "glm", "glm-4.6", &mut body, "high");
        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn deepseek_enabled_plus_effort() {
        let mut body = json!({"messages": []});
        apply_thinking_level(
            Format::OpenAi,
            "deepseek",
            "deepseek-v4-pro",
            &mut body,
            "low",
        );
        assert_eq!(body["thinking"]["type"], "enabled");
        // deepseek maps low → high (only high/max supported)
        assert_eq!(body["reasoning_effort"], "high");
    }

    #[test]
    fn kimi_maps_minimal_to_low() {
        let mut body = json!({"messages": []});
        apply_thinking_level(Format::OpenAi, "kimi", "kimi-k2", &mut body, "minimal");
        assert_eq!(body["reasoning_effort"], "low");
    }

    #[test]
    fn gemini_budget_sets_thinking_config() {
        let mut body = json!({"contents": []});
        apply_thinking_level(
            Format::Gemini,
            "gemini",
            "gemini-2.5-flash",
            &mut body,
            "high",
        );
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            24576
        );
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["includeThoughts"],
            true
        );
    }

    /// 9router thinkingLevels.js `*gemini-4*` → `[low, medium, high]` +
    /// capabilities `thinkingCanDisable: false` (commit 29fc880): Argon
    /// rejects `"minimal"` with 400 and cannot disable thinking, so none/
    /// minimal intents clamp to `low` while supported levels pass through.
    #[test]
    fn gemini4_argon_thinking_level_clamp() {
        for level in ["none", "minimal"] {
            let mut body = json!({"contents": []});
            apply_thinking_level(Format::Gemini, "gemini", "gemini-4-argon", &mut body, level);
            assert_eq!(
                body["generationConfig"]["thinkingConfig"]["thinkingLevel"], "low",
                "{level} must clamp to low on gemini-4"
            );
        }
        let mut body = json!({"contents": []});
        apply_thinking_level(
            Format::Gemini,
            "gemini",
            "gemini-4-argon",
            &mut body,
            "high",
        );
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "high"
        );
        // Gemini 3 keeps the minimal tier.
        let mut body = json!({"contents": []});
        apply_thinking_level(
            Format::Gemini,
            "gemini",
            "gemini-3-pro",
            &mut body,
            "minimal",
        );
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "minimal"
        );
    }

    #[test]
    fn codex_sets_reasoning_object() {
        let mut body = json!({"input": []});
        apply_thinking_level(Format::Codex, "codex", "gpt-5", &mut body, "high");
        assert_eq!(body["reasoning_effort"], "high");
        assert_eq!(body["reasoning"]["effort"], "high");
    }

    #[test]
    fn resolve_native_formats() {
        assert_eq!(
            resolve_thinking_native(Format::OpenAi, "openai", "gpt-5"),
            ThinkingNative::OpenAi
        );
        assert_eq!(
            resolve_thinking_native(Format::Claude, "claude", "claude-haiku-4.5"),
            ThinkingNative::ClaudeBudget
        );
        assert_eq!(
            resolve_thinking_native(Format::Claude, "claude", "claude-opus-4.7"),
            ThinkingNative::ClaudeAdaptive
        );
        assert_eq!(
            resolve_thinking_native(Format::OpenAi, "glm", "glm-4.6"),
            ThinkingNative::Zai
        );
        assert_eq!(
            resolve_thinking_native(Format::Kiro, "kiro", "amazon-nova"),
            ThinkingNative::Noop
        );
    }

    #[test]
    fn haiku_adaptive_gate_is_version_aware() {
        // #5183: Haiku 5+ is adaptive-only; <5 and unversioned stay budget.
        for model in [
            "claude-haiku-5.5",
            "claude-haiku-5-5",
            "claude-haiku-5.5-20260101",
        ] {
            assert_eq!(
                resolve_thinking_native(Format::Claude, "claude", model),
                ThinkingNative::ClaudeAdaptive,
                "{model}"
            );
        }
        for model in [
            "claude-haiku-4-5",
            "claude-haiku-4-5-20251001",
            "claude-3-5-haiku",
            "claude-4.5-haiku",
            "claude-haiku",
        ] {
            assert_eq!(
                resolve_thinking_native(Format::Claude, "claude", model),
                ThinkingNative::ClaudeBudget,
                "{model}"
            );
        }
    }

    #[test]
    fn adaptive_haiku_5_5_applies_adaptive_plus_effort() {
        let mut body = json!({"messages": []});
        apply_thinking_level(
            Format::Claude,
            "claude",
            "claude-haiku-5.5",
            &mut body,
            "high",
        );
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "high");
    }

    #[test]
    fn strips_numeric_budget_suffix() {
        assert_eq!(
            strip_thinking_suffix("gpt-5(8192)"),
            ("gpt-5", Some("8192"))
        );
        assert_eq!(
            strip_thinking_suffix("gpt-5 (24576)"),
            ("gpt-5", Some("24576"))
        );
    }

    #[test]
    fn strips_auto_and_ultra_suffix() {
        assert_eq!(
            strip_thinking_suffix("gpt-5(auto)"),
            ("gpt-5", Some("auto"))
        );
        assert_eq!(
            strip_thinking_suffix("gpt-5(ultra)"),
            ("gpt-5", Some("ultra"))
        );
        assert_eq!(strip_thinking_suffix("o3-high"), ("o3", Some("high")));
    }

    #[test]
    fn strips_off_as_none() {
        assert_eq!(strip_thinking_suffix("gpt-5(off)"), ("gpt-5", Some("none")));
        assert_eq!(
            strip_thinking_suffix("gpt-5(none)"),
            ("gpt-5", Some("none"))
        );
    }

    #[test]
    fn leaves_unknown_paren_suffix() {
        // Unknown parentheticals are NOT model names — keep verbatim.
        assert_eq!(strip_thinking_suffix("gpt-5(foo)"), ("gpt-5(foo)", None));
    }

    #[test]
    fn suffix_to_intent_numeric_auto_ultra() {
        assert_eq!(suffix_to_intent("8192"), Some(ThinkingIntent::Budget(8192)));
        assert_eq!(suffix_to_intent("auto"), Some(ThinkingIntent::Auto));
        assert_eq!(
            suffix_to_intent("ultra"),
            Some(ThinkingIntent::Level("ultra".into()))
        );
        assert_eq!(suffix_to_intent("off"), Some(ThinkingIntent::None_));
    }

    #[test]
    fn numeric_suffix_applies_verbatim_budget_claude() {
        let mut body = json!({"messages": []});
        apply_thinking_level(
            Format::Claude,
            "claude",
            "claude-haiku-4.5",
            &mut body,
            "8192",
        );
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 8192);
    }

    #[test]
    fn auto_suffix_applies_enabled_without_budget_claude() {
        let mut body = json!({"messages": []});
        apply_thinking_level(
            Format::Claude,
            "claude",
            "claude-haiku-4.5",
            &mut body,
            "auto",
        );
        // 9router auto → budget -1 → { type: enabled } (no budget_tokens).
        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(body["thinking"].get("budget_tokens").is_none());
    }

    #[test]
    fn ultra_suffix_folds_to_xhigh_openai() {
        let mut body = json!({"messages": []});
        apply_thinking_level(Format::OpenAi, "openai", "gpt-5", &mut body, "ultra");
        assert_eq!(body["reasoning_effort"], "xhigh");
    }

    #[test]
    fn numeric_suffix_applies_gemini_budget_verbatim() {
        let mut body = json!({"contents": []});
        apply_thinking_level(
            Format::Gemini,
            "gemini",
            "gemini-2.5-flash",
            &mut body,
            "5000",
        );
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            5000
        );
    }

    #[test]
    fn extract_intent_openai_effort() {
        let body = json!({"reasoning_effort": "high"});
        assert_eq!(
            extract_thinking_intent(&body),
            Some(ThinkingIntent::Level("high".into()))
        );
    }

    #[test]
    fn extract_intent_claude_budget() {
        let body = json!({"thinking": {"type": "enabled", "budget_tokens": 10000}});
        assert_eq!(
            extract_thinking_intent(&body),
            Some(ThinkingIntent::Budget(10000))
        );
    }

    #[test]
    fn extract_intent_claude_adaptive_is_auto() {
        let body = json!({"thinking": {"type": "adaptive"}});
        assert_eq!(extract_thinking_intent(&body), Some(ThinkingIntent::Auto));
    }

    #[test]
    fn extract_intent_gemini_budget_zero_is_none() {
        let body = json!({"generationConfig": {"thinkingConfig": {"thinkingBudget": 0}}});
        assert_eq!(extract_thinking_intent(&body), Some(ThinkingIntent::None_));
    }

    #[test]
    fn extract_intent_gemini_negative_is_auto() {
        let body = json!({"generationConfig": {"thinkingConfig": {"thinkingBudget": -1}}});
        assert_eq!(extract_thinking_intent(&body), Some(ThinkingIntent::Auto));
    }

    #[test]
    fn extract_intent_qwen_enable() {
        let body = json!({"enable_thinking": true, "thinking_budget": 4096});
        assert_eq!(
            extract_thinking_intent(&body),
            Some(ThinkingIntent::Budget(4096))
        );
        let off = json!({"enable_thinking": false});
        assert_eq!(extract_thinking_intent(&off), Some(ThinkingIntent::None_));
    }

    #[test]
    fn extract_intent_output_config_priority() {
        // output_config.effort wins over reasoning_effort (JS priority order).
        let body = json!({
            "output_config": {"effort": "low"},
            "reasoning_effort": "high"
        });
        assert_eq!(
            extract_thinking_intent(&body),
            Some(ThinkingIntent::Level("low".into()))
        );
    }

    #[test]
    fn extract_intent_absent_is_none() {
        let body = json!({"messages": []});
        assert_eq!(extract_thinking_intent(&body), None);
        assert_eq!(capture_thinking(&body), None);
    }

    // ---- openproxy-4d5h.23: transports[].thinkingFormat ----

    #[test]
    fn transport_thinking_format_resolves_for_responses_legs() {
        // opencode-go /responses leg declares thinkingFormat openai-responses.
        assert_eq!(
            resolve_thinking_native(Format::OpenAiResponses, "opencode-go", "grok-4.6"),
            ThinkingNative::OpenAiResponses
        );
        assert_eq!(
            resolve_thinking_native(Format::OpenAiResponses, "ocg", "deepseek-v4-pro"),
            ThinkingNative::OpenAiResponses
        );
        // muse /responses leg likewise.
        assert_eq!(
            resolve_thinking_native(Format::OpenAiResponses, "muse", "muse-spark-1.3"),
            ThinkingNative::OpenAiResponses
        );
        // Chat legs are unaffected — no thinkingFormat declared there.
        assert_eq!(
            resolve_thinking_native(Format::OpenAi, "opencode-go", "glm-5.3"),
            ThinkingNative::OpenAi
        );
        // Claude leg has no thinkingFormat → normal format resolution.
        assert_eq!(
            resolve_thinking_native(Format::Claude, "opencode-go", "deepseek-v4-pro"),
            ThinkingNative::ClaudeBudget
        );
    }

    #[test]
    fn openai_responses_native_writes_nested_reasoning_and_drops_effort() {
        let mut body = json!({
            "input": [{"type": "message", "role": "user", "content": "hi"}],
            "reasoning_effort": "high"
        });
        apply_thinking_level(
            Format::OpenAiResponses,
            "opencode-go",
            "grok-4.6",
            &mut body,
            "high",
        );
        assert!(body.get("reasoning_effort").is_none(), "{body}");
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["reasoning"]["summary"], "auto");
    }

    #[test]
    fn openai_responses_native_none_omits_reasoning() {
        let mut body = json!({
            "input": [],
            "reasoning": {"effort": "high", "summary": "auto"},
            "reasoning_effort": "high"
        });
        apply_thinking_level(
            Format::OpenAiResponses,
            "muse",
            "muse-spark-1.3",
            &mut body,
            "none",
        );
        assert!(body.get("reasoning").is_none(), "{body}");
        assert!(body.get("reasoning_effort").is_none(), "{body}");
    }

    #[test]
    fn claude_opus_5_5_none_clamps_to_minimal_effort() {
        // Bead openproxy-4d5h.45 / upstream applyFormat canDisable gate:
        // Opus 5.5 (thinkingCanDisable:false) cannot take
        // thinking.type "disabled" — a "none" intent clamps to minimal
        // effort (output_config.effort "low") and writes no thinking block.
        let mut body = json!({"model": "claude-opus-5.5", "messages": []});
        apply_thinking_level(
            Format::Claude,
            "github",
            "claude-opus-5.5",
            &mut body,
            "none",
        );
        assert!(body.get("thinking").is_none(), "{body}");
        assert_eq!(body["output_config"]["effort"], "low", "{body}");

        // A level intent on the same model writes effort only — no
        // adaptive switch (permanently adaptive upstream).
        let mut body = json!({"model": "claude-opus-5.5", "messages": []});
        apply_thinking_level(
            Format::Claude,
            "github",
            "claude-opus-5.5",
            &mut body,
            "high",
        );
        assert!(body.get("thinking").is_none(), "{body}");
        assert_eq!(body["output_config"]["effort"], "high", "{body}");
    }

    #[test]
    fn claude_sonnet_5_5_adaptive_keeps_switch() {
        // Sonnet 5.5 CAN disable thinking (between_tools substitution is a
        // prepare-time rewrite), so the adaptive switch is still written.
        let mut body = json!({"model": "claude-sonnet-5.5", "messages": []});
        apply_thinking_level(
            Format::Claude,
            "github",
            "claude-sonnet-5.5",
            &mut body,
            "high",
        );
        assert_eq!(body["thinking"]["type"], "adaptive", "{body}");
        assert_eq!(body["output_config"]["effort"], "high", "{body}");

        // "none" still writes the disabled switch here (canDisable:true);
        // prepare_claude_request rewrites it to between_tools downstream.
        let mut body = json!({"model": "claude-sonnet-5.5", "messages": []});
        apply_thinking_level(
            Format::Claude,
            "github",
            "claude-sonnet-5.5",
            &mut body,
            "none",
        );
        assert_eq!(body["thinking"]["type"], "disabled", "{body}");
    }
}
