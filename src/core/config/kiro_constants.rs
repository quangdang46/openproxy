//! Port of `open-sse/config/kiroConstants.js`.
//!
//! Kiro-specific constants, suffix detection (`-agentic`, `-thinking`),
//! and the chunked-write system prompt that turns the agentic variant on.
//! Behavioural ports of `isThinkingEnabled`, `resolveKiroModel`, and
//! `buildThinkingSystemPrefix`.

use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

pub const KIRO_AGENTIC_SUFFIX: &str = "-agentic";
pub const KIRO_THINKING_SUFFIX: &str = "-thinking";
pub const KIRO_THINKING_BUDGET_DEFAULT: u32 = 16_000;
pub const KIRO_THINKING_BUDGET_MAX: u32 = 32_000;

/// Long-form chunked-write protocol prompt prepended to agentic-variant
/// requests. Verbatim from upstream — server timeouts depend on the
/// LLM honouring the 350-line cap.
pub const KIRO_AGENTIC_SYSTEM_PROMPT: &str = "# CRITICAL: CHUNKED WRITE PROTOCOL (MANDATORY)

You MUST follow these rules for ALL file operations. Violation causes server timeouts and task failure.

## ABSOLUTE LIMITS
- **MAXIMUM 350 LINES** per single write/edit operation - NO EXCEPTIONS
- **RECOMMENDED 300 LINES** or less for optimal performance
- **NEVER** write entire files in one operation if >300 lines

## MANDATORY CHUNKED WRITE STRATEGY

### For NEW FILES (>300 lines total):
1. FIRST: Write initial chunk (first 250-300 lines) using write_to_file/fsWrite
2. THEN: Append remaining content in 250-300 line chunks using file append operations
3. REPEAT: Continue appending until complete

### For EDITING EXISTING FILES:
1. Use surgical edits (apply_diff/targeted edits) - change ONLY what's needed
2. NEVER rewrite entire files - use incremental modifications
3. Split large refactors into multiple small, focused edits

### For LARGE CODE GENERATION:
1. Generate in logical sections (imports, types, functions separately)
2. Write each section as a separate operation
3. Use append operations for subsequent sections

## EXAMPLES OF CORRECT BEHAVIOR

CORRECT: Writing a 600-line file
- Operation 1: Write lines 1-300 (initial file creation)
- Operation 2: Append lines 301-600

CORRECT: Editing multiple functions
- Operation 1: Edit function A
- Operation 2: Edit function B
- Operation 3: Edit function C

WRONG: Writing 500 lines in single operation -> TIMEOUT
WRONG: Rewriting entire file to change 5 lines -> TIMEOUT
WRONG: Generating massive code blocks without chunking -> TIMEOUT

## WHY THIS MATTERS
- Server has 2-3 minute timeout for operations
- Large writes exceed timeout and FAIL completely
- Chunked writes are FASTER and more RELIABLE
- Failed writes waste time and require retry

REMEMBER: When in doubt, write LESS per operation. Multiple small operations > one large operation.";

/// Result of parsing a possibly-suffixed Kiro model id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedKiroModel {
    /// The real upstream model id, with all 9router-synthetic suffixes stripped.
    pub upstream: String,
    /// Whether `-agentic` was present.
    pub agentic: bool,
    /// Whether `-thinking` was present.
    pub thinking: bool,
}

/// Returns true iff `model` ends with the agentic suffix.
pub fn is_agentic_model(model: &str) -> bool {
    model.ends_with(KIRO_AGENTIC_SUFFIX)
}

/// Returns true iff `model` ends with the thinking suffix.
pub fn is_thinking_model(model: &str) -> bool {
    model.ends_with(KIRO_THINKING_SUFFIX)
}

/// Strip the `-agentic` suffix if present.
pub fn strip_agentic_suffix(model: &str) -> &str {
    model.strip_suffix(KIRO_AGENTIC_SUFFIX).unwrap_or(model)
}

/// Strip the `-thinking` suffix if present.
pub fn strip_thinking_suffix(model: &str) -> &str {
    model.strip_suffix(KIRO_THINKING_SUFFIX).unwrap_or(model)
}

/// Strip a trailing Anthropic snapshot date (`-YYYYMMDD`) — port of the
/// `ANTHROPIC_SNAPSHOT_SUFFIX` regex in upstream `normalizeKiroUpstreamModelId`.
fn strip_anthropic_snapshot_suffix(id: &str) -> &str {
    let bytes = id.as_bytes();
    let n = bytes.len();
    // "-" + exactly 8 ASCII digits at end of string.
    if n >= 9 && bytes[n - 9] == b'-' && bytes[n - 8..].iter().all(|b| b.is_ascii_digit()) {
        &id[..n - 9]
    } else {
        id
    }
}

/// Port of `normalizeKiroUpstreamModelId` (`open-sse/config/kiroConstants.js`,
/// bead openproxy-4d5h.46): Kiro's upstream only knows a small canonical set
/// of model ids, so dated snapshots (`claude-sonnet-4-5-20250929`),
/// dotted/dashed Claude spellings and third-party aliases all fold onto it.
///
/// Returns the canonical upstream id. Unknown ids — and `gpt-*` ids that are
/// not the 5.6 family — are returned UNCHANGED (original case preserved),
/// exactly like the JS `return model` fallthroughs.
pub fn normalize_kiro_upstream_model_id(model: &str) -> String {
    if model.is_empty() {
        return model.to_string();
    }
    // JS: model.toLowerCase().replace(/-\d{8}$/, "")
    let lowered = model.to_lowercase();
    let id = strip_anthropic_snapshot_suffix(&lowered);

    if id.contains("sonnet") {
        if id.contains("5.5") || id.contains("5-5") {
            return "claude-sonnet-5.5".to_string();
        }
        if id.contains("4.6") || id.contains("4-6") {
            return "claude-sonnet-4.6".to_string();
        }
        if id.contains("sonnet-5") || id.contains("sonnet.5") {
            return "claude-sonnet-5".to_string();
        }
        if id.contains("4.5") || id.contains("4-5") {
            return "claude-sonnet-4.5".to_string();
        }
        if id.contains("sonnet-4") || id.contains("sonnet.4") {
            return "claude-sonnet-4".to_string();
        }
        // Legacy ids like claude-3-5-sonnet-20241022 land here.
        return "claude-sonnet-4.5".to_string();
    }
    if id.contains("fable") {
        return "claude-fable-5".to_string();
    }
    if id.contains("opus") {
        if id.contains("5.5") || id.contains("5-5") {
            return "claude-opus-5.5".to_string();
        }
        if id.contains("opus-5") || id.contains("opus.5") || id.contains("opus 5") {
            return "claude-opus-5".to_string();
        }
        if id.contains("4.8") || id.contains("4-8") {
            return "claude-opus-4.8".to_string();
        }
        if id.contains("4.7") || id.contains("4-7") {
            return "claude-opus-4.7".to_string();
        }
        if id.contains("4.5") || id.contains("4-5") {
            return "claude-opus-4.5".to_string();
        }
        return "claude-opus-4.6".to_string();
    }
    if id.contains("haiku") {
        return if id.contains("5.5") || id.contains("5-5") {
            "claude-haiku-5.5"
        } else {
            "claude-haiku-4.5"
        }
        .to_string();
    }
    if id == "auto" {
        return "auto".to_string();
    }
    if id.contains("deepseek") {
        return "deepseek-3.2".to_string();
    }
    if id.contains("glm") {
        return "glm-5".to_string();
    }
    if id.contains("minimax") {
        return if id.contains("2.5") || id.contains("2-5") {
            "minimax-m2.5"
        } else {
            "minimax-m2.1"
        }
        .to_string();
    }
    if id.contains("qwen") {
        return "qwen3-coder-next".to_string();
    }
    if id.contains("kimi") {
        return "kimi-k3".to_string();
    }
    if id.contains("gpt") {
        if id.contains("terra") {
            return "gpt-5.6-terra".to_string();
        }
        if id.contains("luna") {
            return "gpt-5.6-luna".to_string();
        }
        if id.contains("sol") || id.contains("5.6") || id.contains("5-6") {
            return "gpt-5.6-sol".to_string();
        }
        // Non-5.6 GPT ids pass through untouched (JS `return model`).
        return model.to_string();
    }
    model.to_string()
}

/// Resolve a 9router model id to the real upstream id + behavioural flags.
/// `upstream` is normalized via [`normalize_kiro_upstream_model_id`] so
/// dated snapshots and dotted/dashed spellings reach Kiro canonically.
pub fn resolve_kiro_model(model: &str) -> ResolvedKiroModel {
    let mut upstream = model.to_string();
    let mut agentic = false;
    let mut thinking = false;
    if is_agentic_model(&upstream) {
        agentic = true;
        upstream = strip_agentic_suffix(&upstream).to_string();
    }
    if is_thinking_model(&upstream) {
        thinking = true;
        upstream = strip_thinking_suffix(&upstream).to_string();
    }
    ResolvedKiroModel {
        upstream: normalize_kiro_upstream_model_id(&upstream),
        agentic,
        thinking,
    }
}

/// Port of `applyKiroThinkingOverride` (`kiroConstants.js`): apply a parsed
/// `model(level)` suffix override onto the request body BEFORE translation so
/// [`resolve_kiro_thinking_budget`] sees it downstream.
///
/// - budget (`model(8192)`) → `thinking: {type:"enabled", budget_tokens}` and
///   drops the competing effort fields.
/// - level/auto/none → merges `output_config.effort` (`auto`/`none` pass
///   through verbatim, like the JS `override.mode` fallthrough).
pub fn apply_kiro_thinking_override(body: &mut Value, level: &str) {
    use crate::core::utils::thinking_suffix::{suffix_to_intent, ThinkingIntent};
    let Some(intent) = suffix_to_intent(level) else {
        return;
    };
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    match intent {
        ThinkingIntent::Budget(budget) => {
            obj.remove("output_config");
            obj.remove("reasoning_effort");
            obj.remove("reasoning");
            obj.insert(
                "thinking".to_string(),
                serde_json::json!({ "type": "enabled", "budget_tokens": budget }),
            );
        }
        other => {
            let effort = match &other {
                ThinkingIntent::None_ => "none",
                ThinkingIntent::Auto => "auto",
                ThinkingIntent::Level(l) => l.as_str(),
                ThinkingIntent::Budget(_) => unreachable!(),
            };
            let mut output_config = obj
                .get("output_config")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            if let Some(oc) = output_config.as_object_mut() {
                oc.insert("effort".to_string(), Value::String(effort.to_string()));
            } else {
                output_config = serde_json::json!({ "effort": effort });
            }
            obj.insert("output_config".to_string(), output_config);
        }
    }
}

/// Build the magic system-prompt prefix that turns Kiro reasoning on.
pub fn build_thinking_system_prefix(budget: Option<u32>) -> String {
    let raw = budget.unwrap_or(KIRO_THINKING_BUDGET_DEFAULT);
    let safe = raw.clamp(1, KIRO_THINKING_BUDGET_MAX);
    format!(
        "<thinking_mode>enabled</thinking_mode>\n<max_thinking_length>{}</max_thinking_length>",
        safe
    )
}

/// Map an effort level string to a thinking budget, mirroring the
/// `effortToBudget` LEVEL_TO_BUDGET table in 9router's thinking.js. Unknown
/// levels fall back to the default budget (JS: `?? KIRO_THINKING_BUDGET_DEFAULT`).
pub fn effort_to_kiro_budget(level: &str) -> Option<u32> {
    match level.to_lowercase().as_str() {
        "minimal" => Some(512),
        "low" => Some(1024),
        "medium" => Some(8192),
        "high" => Some(24576),
        "xhigh" => Some(32768),
        "max" => Some(128_000),
        // "auto" and any other unknown level: JS treats it as a fallback to
        // KIRO_THINKING_BUDGET_DEFAULT (16000).
        _ => Some(KIRO_THINKING_BUDGET_DEFAULT),
    }
}

/// Resolve the Kiro thinking budget for an inbound request, mirroring
/// `resolveKiroThinkingBudget` in 9router's kiroConstants.js.
///
/// Resolution order (first match wins):
///   1. `body.output_config.effort` (Claude-flavored native effort field)
///   2. `body.thinking` block — `disabled` → None; `enabled`/`adaptive` with
///      `budget_tokens` → that budget
///   3. `body.reasoning_effort` / `body.reasoning.effort` — `none`/`off`/
///      `disabled` → None; otherwise effortToBudget(level)
///   4. `anthropic-beta` header containing `interleaved-thinking` → 16000
///   5. messages/system containing a `<thinking_mode>` tag → 16000
///   6. model id containing `thinking` or `-reason` → 16000
///   7. else None
pub fn resolve_kiro_thinking_budget(
    body: &Value,
    headers: Option<&dyn HeaderLookup>,
    model: &str,
) -> Option<u32> {
    // 1. output_config.effort (native Claude effort field).
    if let Some(effort) = body
        .get("output_config")
        .and_then(|o| o.get("effort"))
        .and_then(|v| v.as_str())
    {
        return effort_to_kiro_budget(effort);
    }

    // 2. thinking block.
    if let Some(thinking) = body.get("thinking") {
        let t = thinking.get("type").and_then(|v| v.as_str());
        match t {
            Some("disabled") => return None,
            Some("enabled" | "adaptive") => {
                let budget = thinking
                    .get("budget_tokens")
                    .and_then(|v| v.as_f64())
                    .filter(|b| b.is_finite() && *b > 0.0);
                if let Some(b) = budget {
                    return Some(b as u32);
                }
                // enabled without a positive budget still counts as thinking;
                // JS falls through to the default-budget fallbacks below.
            }
            _ => {}
        }
    }

    // 3. reasoning effort fields.
    let effort = body
        .get("reasoning_effort")
        .and_then(|v| v.as_str())
        .or_else(|| {
            body.get("reasoning")
                .and_then(|r| r.get("effort"))
                .and_then(|v| v.as_str())
        });
    if let Some(level) = effort {
        let lowered = level.to_lowercase();
        if matches!(lowered.as_str(), "none" | "off" | "disabled") {
            return None;
        }
        return effort_to_kiro_budget(&lowered);
    }

    // 4. anthropic-beta header.
    if let Some(h) = headers {
        if let Some(beta) = h.get("anthropic-beta") {
            if beta.to_lowercase().contains("interleaved-thinking") {
                return Some(KIRO_THINKING_BUDGET_DEFAULT);
            }
        }
    }

    // 5. thinking-mode tag in messages/system.
    if contains_thinking_mode_tag(body) {
        return Some(KIRO_THINKING_BUDGET_DEFAULT);
    }

    // 6. model id hint.
    let m = model.to_lowercase();
    if m.contains("thinking") || m.contains("-reason") {
        return Some(KIRO_THINKING_BUDGET_DEFAULT);
    }

    None
}

/// Decide which native-effort request field a model speaks, mirroring
/// `resolveKiroEffortPath` in 9router's kiroConstants.js:
///   - `"reasoning"` for GPT-5.6-family models (id contains `gpt`, `5` and
///     `6` tokens)
///   - `"output_config"` for Claude models newer than 4.5 (major > 4, or
///     major 4 with minor > 5)
///   - `None` otherwise
pub fn resolve_kiro_effort_path(model: &str) -> Option<&'static str> {
    // Upstream matches on the dash/dot-normalized id:
    // `/(?:^|[/.])gpt[/.]5[/.]6(?:[/.]|$)/` — the 5 and 6 must be adjacent
    // version tokens of the gpt id, not merely present anywhere.
    let normalized = model.to_lowercase().replace('-', ".");
    static GPT_56_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:^|[/.])gpt[/.]5[/.]6(?:[/.]|$)").expect("gpt-5.6 effort regex")
    });
    if GPT_56_RE.is_match(&normalized) {
        return Some("reasoning");
    }

    // Claude: parse the first numeric version run (`N` or `N.M`) after the
    // `claude` token — e.g. `4.6` in `claude-sonnet-4.6`, `4` in
    // `claude-opus-4-20250514`.
    if normalized.contains("claude") {
        let v = parse_claude_version(model)?;
        let (major, minor) = v;
        // Upstream: null for major < 4, or major 4 with minor null / <= 5 /
        // date-suffix (>= 1000); output_config otherwise (major > 4, or
        // major 4 with minor 6..=999).
        let is_newer = major > 4 || (major == 4 && minor.is_some_and(|m| m > 5 && m < 1000));
        if is_newer {
            return Some("output_config");
        }
    }

    None
}

/// Port of `parseClaudeVersion` (`open-sse/config/kiroConstants.js`):
/// lowercase, `-` → `.`, then match the first `claude` token followed by a
/// `N` or `N.M` version run. Returns `(major, minor)` — `minor` is `None`
/// for unversioned-minor ids like `claude-sonnet-5`.
fn parse_claude_version(model: &str) -> Option<(u32, Option<u32>)> {
    let normalized = model.to_lowercase().replace('-', ".");
    static CLAUDE_VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:^|[/.])claude(?:[/.][a-z]+)*[/.](\d+)(?:[/.](\d+))?(?:[/.]|$)")
            .expect("claude version regex")
    });
    let caps = CLAUDE_VERSION_RE.captures(&normalized)?;
    let major = caps.get(1)?.as_str().parse::<u32>().ok()?;
    let minor = caps.get(2).and_then(|m| m.as_str().parse::<u32>().ok());
    Some((major, minor))
}

/// Port of `kiroModelLacksXhigh` (kiroConstants.js): 4.6 Claude models cap
/// at `low|medium|high|max`; 4.7+ add `xhigh`. Unknown models stay
/// conservative (treated as lacking `xhigh`).
fn kiro_model_lacks_xhigh(model: &str) -> bool {
    match parse_claude_version(model) {
        None => true,
        Some((major, minor)) => major == 4 && minor.is_some_and(|m| m <= 6),
    }
}

/// Extract the native effort level a model should receive on the wire,
/// mirroring `extractKiroEffortLevel`/`extractKiroGptEffortLevel` in
/// kiroConstants.js. Only known effort levels are accepted (unknown ones
/// like `auto`/`minimal`/`ultra` yield `None`). For Claude
/// (`output_config`) `xhigh` is model-gated — folded to `high` only on
/// 4.6-era models — and `max` passes through as `max`; for GPT
/// (`reasoning`) `max` maps to `xhigh`. Returns `None` when the body
/// carries no usable effort value.
pub fn extract_kiro_effort_level(body: &Value, path: &str, model: &str) -> Option<String> {
    // Upstream order: output_config.effort ?? reasoning_effort ??
    // reasoning.effort (object only).
    let effort = body
        .get("output_config")
        .and_then(|o| o.get("effort"))
        .and_then(|v| v.as_str())
        .or_else(|| body.get("reasoning_effort").and_then(|v| v.as_str()))
        .or_else(|| {
            body.get("reasoning")
                .filter(|r| r.is_object())
                .and_then(|r| r.get("effort"))
                .and_then(|v| v.as_str())
        })
        .map(str::to_lowercase)?;

    match path {
        "output_config" => match effort.as_str() {
            "low" | "medium" | "high" => Some(effort),
            "xhigh" => Some(if kiro_model_lacks_xhigh(model) {
                "high".to_string()
            } else {
                "xhigh".to_string()
            }),
            "max" => Some("max".to_string()),
            _ => None,
        },
        "reasoning" => match effort.as_str() {
            "low" | "medium" | "high" | "xhigh" => Some(effort),
            "max" => Some("xhigh".to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// Whether a model uses Kiro's native GPT effort field, in which case the
/// legacy `<thinking_mode>` prompt-tag prefix must be suppressed. Mirrors
/// `usesKiroNativeGptEffort` in 9router: only true for GPT-5.6 models
/// carrying a supported effort value (none/off/disabled/unknown are not
/// native).
pub fn uses_kiro_native_gpt_effort(model: &str, body: &Value) -> bool {
    resolve_kiro_effort_path(model) == Some("reasoning")
        && extract_kiro_effort_level(body, "reasoning", model).is_some()
}

/// Build `additionalModelRequestFields` for a Kiro request, mirroring
/// `buildKiroAdditionalModelRequestFieldsForModel` in 9router's
/// kiroConstants.js:
///   - Claude models (`output_config` path) →
///     `{ thinking: { type: "adaptive", display: "summarized" }, output_config: { effort } }`
///   - GPT-5.6 models (`reasoning` path) → `{ reasoning: { effort } }`
///   - otherwise `None`
pub fn build_kiro_additional_model_request_fields_for_model(
    body: &Value,
    model: &str,
) -> Option<Value> {
    let path = resolve_kiro_effort_path(model)?;
    let effort = extract_kiro_effort_level(body, path, model)?;

    match path {
        "output_config" => Some(serde_json::json!({
            "thinking": { "type": "adaptive", "display": "summarized" },
            "output_config": { "effort": effort }
        })),
        "reasoning" => Some(serde_json::json!({
            "reasoning": { "effort": effort }
        })),
        _ => None,
    }
}

/// Detect whether an inbound request is asking for reasoning / thinking
/// output. Mirrors `isThinkingEnabled` in 9router.
///
/// Inputs:
///   - `body`: post-translation OpenAI-shaped request body.
///   - `headers`: original inbound HTTP headers (case-insensitive lookup).
///   - `model`: resolved model id (suffix-stripped is fine).
pub fn is_thinking_enabled(
    body: Option<&Value>,
    headers: Option<&dyn HeaderLookup>,
    model: Option<&str>,
) -> bool {
    if let Some(h) = headers {
        if let Some(beta) = h.get("anthropic-beta") {
            if beta.to_lowercase().contains("interleaved-thinking") {
                return true;
            }
        }
    }

    if let Some(body) = body {
        if let Some(thinking) = body.get("thinking") {
            if thinking.get("type").and_then(|v| v.as_str()) == Some("enabled") {
                let budget = thinking.get("budget_tokens").and_then(|v| v.as_f64());
                if budget.is_none() || budget.is_some_and(|b| b.is_finite() && b > 0.0) {
                    return true;
                }
            }
        }

        let effort = body
            .get("reasoning_effort")
            .and_then(|v| v.as_str())
            .or_else(|| {
                body.get("reasoning")
                    .and_then(|r| r.get("effort"))
                    .and_then(|v| v.as_str())
            });
        if let Some(v) = effort {
            let lowered = v.to_lowercase();
            if matches!(lowered.as_str(), "low" | "medium" | "high" | "auto") {
                return true;
            }
        }

        if contains_thinking_mode_tag(body) {
            return true;
        }
    }

    if let Some(model) = model {
        let m = model.to_lowercase();
        if m.contains("thinking") || m.contains("-reason") {
            return true;
        }
    }

    false
}

/// Trait abstracting "look up a header by case-insensitive name". Allows
/// passing either a `BTreeMap<String, String>`, a reqwest `HeaderMap`, or
/// a serde_json::Value (object) without forcing one shape on callers.
pub trait HeaderLookup {
    fn get(&self, name: &str) -> Option<String>;
}

impl HeaderLookup for std::collections::BTreeMap<String, String> {
    fn get(&self, name: &str) -> Option<String> {
        let want = name.to_lowercase();
        self.iter()
            .find(|(k, _)| k.to_lowercase() == want)
            .map(|(_, v)| v.clone())
    }
}

impl HeaderLookup for std::collections::HashMap<String, String> {
    fn get(&self, name: &str) -> Option<String> {
        let want = name.to_lowercase();
        self.iter()
            .find(|(k, _)| k.to_lowercase() == want)
            .map(|(_, v)| v.clone())
    }
}

impl HeaderLookup for serde_json::Value {
    fn get(&self, name: &str) -> Option<String> {
        let obj = self.as_object()?;
        let want = name.to_lowercase();
        for (k, v) in obj {
            if k.to_lowercase() == want {
                return v.as_str().map(str::to_string);
            }
        }
        None
    }
}

fn contains_thinking_mode_tag(body: &Value) -> bool {
    let messages = body.get("messages").and_then(|v| v.as_array());
    if let Some(messages) = messages {
        for msg in messages {
            let role = msg.get("role").and_then(|v| v.as_str());
            if role != Some("system") && role != Some("user") {
                continue;
            }
            let content = msg.get("content");
            if let Some(s) = content.and_then(|v| v.as_str()) {
                if contains_tag_in_text(s) {
                    return true;
                }
            } else if let Some(arr) = content.and_then(|v| v.as_array()) {
                for part in arr {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        if contains_tag_in_text(text) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    if let Some(s) = body.get("system").and_then(|v| v.as_str()) {
        if contains_tag_in_text(s) {
            return true;
        }
    }
    false
}

fn contains_tag_in_text(text: &str) -> bool {
    if !text.contains("<thinking_mode>") {
        return false;
    }
    text.contains("<thinking_mode>enabled</thinking_mode>")
        || text.contains("<thinking_mode>interleaved</thinking_mode>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_handles_combined_suffixes() {
        assert_eq!(
            resolve_kiro_model("claude-sonnet-4.5-thinking-agentic"),
            ResolvedKiroModel {
                upstream: "claude-sonnet-4.5".to_string(),
                agentic: true,
                thinking: true,
            }
        );
        assert_eq!(
            resolve_kiro_model("claude-sonnet-4.5-thinking"),
            ResolvedKiroModel {
                upstream: "claude-sonnet-4.5".to_string(),
                agentic: false,
                thinking: true,
            }
        );
        assert_eq!(
            resolve_kiro_model("claude-sonnet-4.5-agentic"),
            ResolvedKiroModel {
                upstream: "claude-sonnet-4.5".to_string(),
                agentic: true,
                thinking: false,
            }
        );
        assert_eq!(
            resolve_kiro_model("claude-sonnet-4.5"),
            ResolvedKiroModel {
                upstream: "claude-sonnet-4.5".to_string(),
                agentic: false,
                thinking: false,
            }
        );
    }

    #[test]
    fn build_thinking_prefix_clamps_budget() {
        assert!(build_thinking_system_prefix(Some(0))
            .contains("<max_thinking_length>1</max_thinking_length>"));
        assert!(build_thinking_system_prefix(Some(99_999))
            .contains("<max_thinking_length>32000</max_thinking_length>"));
        assert!(build_thinking_system_prefix(None)
            .contains("<max_thinking_length>16000</max_thinking_length>"));
    }

    #[test]
    fn is_thinking_enabled_via_header() {
        let mut h = std::collections::BTreeMap::new();
        h.insert(
            "Anthropic-Beta".to_string(),
            "interleaved-thinking-2024".to_string(),
        );
        assert!(is_thinking_enabled(None, Some(&h), None));
    }

    #[test]
    fn is_thinking_enabled_via_thinking_block() {
        let body = json!({
            "thinking": {"type": "enabled", "budget_tokens": 8000}
        });
        assert!(is_thinking_enabled(Some(&body), None, None));
    }

    #[test]
    fn is_thinking_enabled_via_reasoning_effort() {
        let body = json!({"reasoning_effort": "high"});
        assert!(is_thinking_enabled(Some(&body), None, None));

        let body = json!({"reasoning": {"effort": "medium"}});
        assert!(is_thinking_enabled(Some(&body), None, None));

        let body = json!({"reasoning_effort": "none"});
        assert!(!is_thinking_enabled(Some(&body), None, None));
    }

    #[test]
    fn is_thinking_enabled_via_system_tag() {
        let body = json!({
            "messages": [
                {"role": "system", "content": "do stuff <thinking_mode>enabled</thinking_mode>"}
            ]
        });
        assert!(is_thinking_enabled(Some(&body), None, None));
    }

    #[test]
    fn is_thinking_enabled_via_model_name() {
        assert!(is_thinking_enabled(None, None, Some("kimi-k2-thinking")));
        assert!(is_thinking_enabled(None, None, Some("o3-reason")));
        assert!(!is_thinking_enabled(None, None, Some("gpt-4o")));
    }

    #[test]
    fn resolve_kiro_thinking_budget_effort_levels() {
        // low → 1024
        let body = json!({"reasoning_effort": "low"});
        assert_eq!(
            resolve_kiro_thinking_budget(&body, None, "gpt-4o"),
            Some(1024)
        );
        // medium → 8192
        let body = json!({"reasoning_effort": "medium"});
        assert_eq!(
            resolve_kiro_thinking_budget(&body, None, "gpt-4o"),
            Some(8192)
        );
        // high → 24576
        let body = json!({"reasoning": {"effort": "high"}});
        assert_eq!(
            resolve_kiro_thinking_budget(&body, None, "gpt-4o"),
            Some(24576)
        );
        // xhigh → 32768
        let body = json!({"reasoning_effort": "xhigh"});
        assert_eq!(
            resolve_kiro_thinking_budget(&body, None, "gpt-4o"),
            Some(32768)
        );
        // max → 128000
        let body = json!({"reasoning_effort": "max"});
        assert_eq!(
            resolve_kiro_thinking_budget(&body, None, "gpt-4o"),
            Some(128_000)
        );
        // minimal → 512
        let body = json!({"reasoning_effort": "minimal"});
        assert_eq!(
            resolve_kiro_thinking_budget(&body, None, "gpt-4o"),
            Some(512)
        );
        // auto → default 16000
        let body = json!({"reasoning_effort": "auto"});
        assert_eq!(
            resolve_kiro_thinking_budget(&body, None, "gpt-4o"),
            Some(16_000)
        );
        // unknown level → default 16000 (JS `?? KIRO_THINKING_BUDGET_DEFAULT`)
        let body = json!({"reasoning_effort": "ultra"});
        assert_eq!(
            resolve_kiro_thinking_budget(&body, None, "gpt-4o"),
            Some(16_000)
        );
    }

    #[test]
    fn resolve_kiro_thinking_budget_none_off_disabled() {
        let body = json!({"reasoning_effort": "none"});
        assert_eq!(resolve_kiro_thinking_budget(&body, None, "gpt-4o"), None);
        let body = json!({"reasoning_effort": "off"});
        assert_eq!(resolve_kiro_thinking_budget(&body, None, "gpt-4o"), None);
        let body = json!({"reasoning": {"effort": "disabled"}});
        assert_eq!(resolve_kiro_thinking_budget(&body, None, "gpt-4o"), None);
        let body = json!({"reasoning": {"effort": "none"}});
        assert_eq!(resolve_kiro_thinking_budget(&body, None, "gpt-4o"), None);
        // No effort at all → None.
        let body = json!({"messages": [{"role": "user", "content": "hi"}]});
        assert_eq!(resolve_kiro_thinking_budget(&body, None, "gpt-4o"), None);
    }

    #[test]
    fn resolve_kiro_thinking_budget_thinking_block() {
        let body = json!({"thinking": {"type": "disabled"}});
        assert_eq!(resolve_kiro_thinking_budget(&body, None, "gpt-4o"), None);
        let body = json!({"thinking": {"type": "enabled", "budget_tokens": 8000}});
        assert_eq!(
            resolve_kiro_thinking_budget(&body, None, "gpt-4o"),
            Some(8000)
        );
        let body = json!({"thinking": {"type": "adaptive", "budget_tokens": 4096}});
        assert_eq!(
            resolve_kiro_thinking_budget(&body, None, "gpt-4o"),
            Some(4096)
        );
        // enabled without a budget falls through to the default fallbacks.
        let body = json!({"thinking": {"type": "enabled"}});
        assert_eq!(resolve_kiro_thinking_budget(&body, None, "gpt-4o"), None);
    }

    #[test]
    fn resolve_kiro_thinking_budget_output_config_effort() {
        let body = json!({"output_config": {"effort": "low"}});
        assert_eq!(
            resolve_kiro_thinking_budget(&body, None, "gpt-4o"),
            Some(1024)
        );
        let body = json!({"output_config": {"effort": "disabled"}});
        assert_eq!(
            resolve_kiro_thinking_budget(&body, None, "gpt-4o"),
            Some(16_000)
        );
    }

    #[test]
    fn resolve_kiro_thinking_budget_header_tag_model() {
        // anthropic-beta header.
        let mut h = std::collections::BTreeMap::new();
        h.insert(
            "Anthropic-Beta".to_string(),
            "interleaved-thinking-2024".to_string(),
        );
        let body = json!({});
        assert_eq!(
            resolve_kiro_thinking_budget(&body, Some(&h), "gpt-4o"),
            Some(16_000)
        );

        // thinking-mode tag in messages.
        let body = json!({
            "messages": [
                {"role": "system", "content": "do stuff <thinking_mode>enabled</thinking_mode>"}
            ]
        });
        assert_eq!(
            resolve_kiro_thinking_budget(&body, None, "gpt-4o"),
            Some(16_000)
        );

        // Model-name hints.
        let body = json!({});
        assert_eq!(
            resolve_kiro_thinking_budget(&body, None, "kimi-k2-thinking"),
            Some(16_000)
        );
        assert_eq!(
            resolve_kiro_thinking_budget(&body, None, "o3-reason"),
            Some(16_000)
        );
        assert_eq!(resolve_kiro_thinking_budget(&body, None, "gpt-4o"), None);
    }

    #[test]
    fn resolve_kiro_effort_path_detects_gpt56_and_claude46() {
        assert_eq!(resolve_kiro_effort_path("gpt-5.6-sol"), Some("reasoning"));
        assert_eq!(resolve_kiro_effort_path("gpt-5.6"), Some("reasoning"));
        assert_eq!(resolve_kiro_effort_path("gpt-4o"), None);
        assert_eq!(resolve_kiro_effort_path("gpt-5"), None);

        assert_eq!(
            resolve_kiro_effort_path("claude-sonnet-4.6"),
            Some("output_config")
        );
        assert_eq!(
            resolve_kiro_effort_path("claude-opus-4.6"),
            Some("output_config")
        );
        assert_eq!(resolve_kiro_effort_path("claude-3-7-sonnet"), None);
        assert_eq!(resolve_kiro_effort_path("claude-sonnet-4.5"), None);
        assert_eq!(resolve_kiro_effort_path("claude-opus-4"), None);
        // 4.6 after a 4 token: `claude-4-4.6` edge, first version run is 4.
        assert_eq!(resolve_kiro_effort_path("claude-4-4.6"), None);
    }

    #[test]
    fn build_kiro_additional_model_request_fields_claude() {
        let body = json!({"reasoning_effort": "low"});
        assert_eq!(
            build_kiro_additional_model_request_fields_for_model(&body, "claude-sonnet-4.6"),
            Some(json!({
                "thinking": {"type": "adaptive", "display": "summarized"},
                "output_config": {"effort": "low"}
            }))
        );
        // xhigh folds to high on 4.6-era models (no xhigh tier).
        let body = json!({"reasoning_effort": "xhigh"});
        let fields =
            build_kiro_additional_model_request_fields_for_model(&body, "claude-sonnet-4.6")
                .unwrap();
        assert_eq!(fields["output_config"]["effort"], "high");
        // max stays max on the Kiro pass-through.
        let body = json!({"reasoning_effort": "max"});
        let fields =
            build_kiro_additional_model_request_fields_for_model(&body, "claude-sonnet-4.6")
                .unwrap();
        assert_eq!(fields["output_config"]["effort"], "max");
        // Unsupported effort → no fields.
        let body = json!({"reasoning_effort": "auto"});
        assert_eq!(
            build_kiro_additional_model_request_fields_for_model(&body, "claude-sonnet-4.6"),
            None
        );
        // none → no fields.
        let body = json!({"reasoning_effort": "none"});
        assert_eq!(
            build_kiro_additional_model_request_fields_for_model(&body, "claude-sonnet-4.6"),
            None
        );
        // Old claude → no fields even with effort.
        let body = json!({"reasoning_effort": "low"});
        assert_eq!(
            build_kiro_additional_model_request_fields_for_model(&body, "claude-sonnet-4.5"),
            None
        );
    }

    #[test]
    fn kiro_xhigh_is_model_gated() {
        // 4.7+/Sonnet 5 / Opus 5.x advertise xhigh — it must reach the wire.
        for model in [
            "claude-opus-4.7",
            "claude-opus-4-7",
            "claude-sonnet-5",
            "claude-opus-5",
        ] {
            let body = json!({"reasoning_effort": "xhigh"});
            let fields =
                build_kiro_additional_model_request_fields_for_model(&body, model).unwrap();
            assert_eq!(
                fields["output_config"]["effort"], "xhigh",
                "model={model} must keep xhigh"
            );
        }
        // 4.6-era models clamp xhigh → high.
        for model in [
            "claude-sonnet-4.6",
            "claude-opus-4-6",
            "claude-sonnet-4.6-20260101",
        ] {
            let body = json!({"reasoning_effort": "xhigh"});
            let fields =
                build_kiro_additional_model_request_fields_for_model(&body, model).unwrap();
            assert_eq!(
                fields["output_config"]["effort"], "high",
                "model={model} must clamp xhigh to high"
            );
        }
    }

    #[test]
    fn build_kiro_additional_model_request_fields_gpt() {
        let body = json!({"reasoning": {"effort": "high"}});
        assert_eq!(
            build_kiro_additional_model_request_fields_for_model(&body, "gpt-5.6-sol"),
            Some(json!({"reasoning": {"effort": "high"}}))
        );
        // max → xhigh for GPT.
        let body = json!({"reasoning": {"effort": "max"}});
        let fields =
            build_kiro_additional_model_request_fields_for_model(&body, "gpt-5.6-sol").unwrap();
        assert_eq!(fields["reasoning"]["effort"], "xhigh");
        // none → no fields.
        let body = json!({"reasoning": {"effort": "none"}});
        assert_eq!(
            build_kiro_additional_model_request_fields_for_model(&body, "gpt-5.6-sol"),
            None
        );
    }

    #[test]
    fn uses_kiro_native_gpt_effort_guards() {
        let body = json!({"reasoning": {"effort": "high"}});
        assert!(uses_kiro_native_gpt_effort("gpt-5.6-sol", &body));
        let body = json!({"reasoning": {"effort": "none"}});
        assert!(!uses_kiro_native_gpt_effort("gpt-5.6-sol", &body));
        let body = json!({"reasoning_effort": "auto"});
        assert!(!uses_kiro_native_gpt_effort("gpt-5.6-sol", &body));
        // Non-GPT models never native.
        let body = json!({"reasoning_effort": "high"});
        assert!(!uses_kiro_native_gpt_effort("claude-sonnet-4.6", &body));
    }

    // ── normalizeKiroUpstreamModelId (bead openproxy-4d5h.46) ──────────
    // Mirrors tests/unit/kiro-model-normalization.test.js upstream.

    #[test]
    fn normalize_kiro_upstream_model_id_snapshots_and_dotted() {
        for (input, expected) in [
            // Anthropic snapshot dates fold onto the dotted canonical id.
            ("claude-sonnet-4-5-20250929", "claude-sonnet-4.5"),
            ("claude-opus-4-5-20251101", "claude-opus-4.5"),
            ("claude-haiku-4-5-20251001", "claude-haiku-4.5"),
            // Legacy 3.5 snapshot → sonnet branch default.
            ("claude-3-5-sonnet-20241022", "claude-sonnet-4.5"),
            // Undated dashed spellings.
            ("claude-sonnet-4-5", "claude-sonnet-4.5"),
            ("claude-opus-4-5", "claude-opus-4.5"),
            ("claude-opus-4-8", "claude-opus-4.8"),
            // 5.x dotted/dashed.
            ("claude-opus-5.5", "claude-opus-5.5"),
            ("claude-opus-5-5", "claude-opus-5.5"),
            ("claude-sonnet-5.5", "claude-sonnet-5.5"),
            ("claude-haiku-5.5", "claude-haiku-5.5"),
        ] {
            assert_eq!(
                normalize_kiro_upstream_model_id(input),
                expected,
                "input={input}"
            );
        }
    }

    #[test]
    fn normalize_kiro_upstream_model_id_canonical_and_aliases() {
        for (input, expected) in [
            // Already-canonical ids pass through.
            ("claude-opus-5.5", "claude-opus-5.5"),
            ("claude-opus-5", "claude-opus-5"),
            ("claude-sonnet-5", "claude-sonnet-5"),
            ("claude-sonnet-4.5", "claude-sonnet-4.5"),
            ("claude-haiku-4.5", "claude-haiku-4.5"),
            ("auto", "auto"),
            ("deepseek-3.2", "deepseek-3.2"),
            ("glm-5", "glm-5"),
            ("qwen3-coder-next", "qwen3-coder-next"),
            ("kimi-k3", "kimi-k3"),
            ("minimax-m2.5", "minimax-m2.5"),
            ("minimax-m2.1", "minimax-m2.1"),
            ("gpt-5.6-sol", "gpt-5.6-sol"),
            ("gpt-5.6-terra", "gpt-5.6-terra"),
            ("gpt-5.6-luna", "gpt-5.6-luna"),
            // Third-party aliases fold onto canonical ids.
            ("MiniMax-M2.5", "minimax-m2.5"),
            ("deepseek-v3", "deepseek-3.2"),
            ("glm-4.6", "glm-5"),
            ("qwen3-coder", "qwen3-coder-next"),
            ("kimi-k2", "kimi-k3"),
        ] {
            assert_eq!(
                normalize_kiro_upstream_model_id(input),
                expected,
                "input={input}"
            );
        }
    }

    #[test]
    fn normalize_kiro_upstream_model_id_unknown_passthrough() {
        // Unknown ids and non-5.6 GPT ids are returned byte-for-byte.
        for input in ["", "some-unknown-model", "gpt-4o", "GPT-5"] {
            assert_eq!(normalize_kiro_upstream_model_id(input), input);
        }
    }

    #[test]
    fn resolve_kiro_model_normalizes_upstream() {
        // Suffixes strip first, then the snapshot/date spelling normalizes.
        assert_eq!(
            resolve_kiro_model("claude-sonnet-4-5-20250929-thinking-agentic"),
            ResolvedKiroModel {
                upstream: "claude-sonnet-4.5".to_string(),
                agentic: true,
                thinking: true,
            }
        );
        assert_eq!(
            resolve_kiro_model("claude-opus-4-5-20251101"),
            ResolvedKiroModel {
                upstream: "claude-opus-4.5".to_string(),
                agentic: false,
                thinking: false,
            }
        );
        assert_eq!(
            resolve_kiro_model("MiniMax-M2.5-agentic").upstream,
            "minimax-m2.5"
        );
    }

    #[test]
    fn apply_kiro_thinking_override_shapes() {
        // Budget override → thinking block, competing effort fields dropped.
        let mut body = json!({
            "output_config": {"effort": "high"},
            "reasoning_effort": "low",
            "messages": []
        });
        apply_kiro_thinking_override(&mut body, "8192");
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 8192);
        assert!(body.get("output_config").is_none());
        assert!(body.get("reasoning_effort").is_none());

        // Level override → merged into output_config.effort.
        let mut body = json!({"output_config": {"other": 1}, "messages": []});
        apply_kiro_thinking_override(&mut body, "high");
        assert_eq!(body["output_config"]["effort"], "high");
        assert_eq!(body["output_config"]["other"], 1);

        // auto/none pass through verbatim (JS override.mode fallthrough).
        let mut body = json!({});
        apply_kiro_thinking_override(&mut body, "auto");
        assert_eq!(body["output_config"]["effort"], "auto");
        let mut body = json!({});
        apply_kiro_thinking_override(&mut body, "none");
        assert_eq!(body["output_config"]["effort"], "none");
    }
}
