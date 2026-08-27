//! Port of `open-sse/config/kiroConstants.js`.
//!
//! Kiro-specific constants, suffix detection (`-agentic`, `-thinking`),
//! and the chunked-write system prompt that turns the agentic variant on.
//! Behavioural ports of `isThinkingEnabled`, `resolveKiroModel`, and
//! `buildThinkingSystemPrefix`.

use serde_json::Value;

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

/// Resolve a 9router model id to the real upstream id + behavioural flags.
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
        upstream,
        agentic,
        thinking,
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
    let lower = model.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();

    // GPT-5.6 family: "gpt" plus a "5" and a "6" version token.
    if tokens.contains(&"gpt") && tokens.contains(&"5") && tokens.contains(&"6") {
        return Some("reasoning");
    }

    // Claude: parse the first numeric version run (`N` or `N.M`) after the
    // `claude` token — e.g. `4.6` in `claude-sonnet-4.6`, `4` in
    // `claude-opus-4-20250514`.
    if lower.contains("claude") {
        let rest = &lower[lower.find("claude").unwrap() + "claude".len()..];
        let version = rest
            .split(|c: char| !c.is_ascii_digit() && c != '.')
            .find(|tok| !tok.is_empty() && tok.chars().any(|c| c.is_ascii_digit()));
        if let Some(v) = version {
            let mut parts = v.split('.');
            let major: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
            let minor: Option<u32> = parts.next().and_then(|p| p.parse().ok());
            let is_newer = major > 4 || (major == 4 && minor.is_some_and(|m| m > 5));
            if is_newer {
                return Some("output_config");
            }
        }
    }

    None
}

/// Extract the native effort level a model should receive on the wire,
/// mirroring the JS extractors in kiroConstants.js. Only known effort
/// levels are accepted (unknown ones like `auto`/`minimal`/`ultra` yield
/// `None`); for Claude (`output_config`) the `xhigh`/`max` levels fold
/// down to `high`, and for GPT (`reasoning`) `max` maps to `xhigh`.
/// Returns `None` when the body carries no usable effort value.
pub fn extract_kiro_effort_level(body: &Value, path: &str) -> Option<String> {
    let effort = body
        .get("reasoning_effort")
        .and_then(|v| v.as_str())
        .or_else(|| {
            body.get("reasoning")
                .and_then(|r| r.get("effort"))
                .and_then(|v| v.as_str())
        })
        .or_else(|| {
            body.get("output_config")
                .and_then(|o| o.get("effort"))
                .and_then(|v| v.as_str())
        })
        .map(str::to_lowercase)?;

    match path {
        "output_config" => match effort.as_str() {
            "low" | "medium" | "high" => Some(effort),
            "xhigh" | "max" => Some("high".to_string()),
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
        && extract_kiro_effort_level(body, "reasoning").is_some()
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
    let effort = extract_kiro_effort_level(body, path)?;

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
        // xhigh/max fold down to high for Claude.
        let body = json!({"reasoning_effort": "xhigh"});
        let fields =
            build_kiro_additional_model_request_fields_for_model(&body, "claude-sonnet-4.6")
                .unwrap();
        assert_eq!(fields["output_config"]["effort"], "high");
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
}
