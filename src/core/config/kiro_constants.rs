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
}
