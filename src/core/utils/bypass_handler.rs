//! Port of `open-sse/utils/bypassHandler.js`.
//!
//! Pattern matcher that decides whether an inbound Claude CLI request can
//! be answered with a fake response without round-tripping to a provider.
//! The actual fake-response generation lives in the chat handler since it
//! needs the translator pipeline; this module is the pure detector.

use serde_json::Value;

use crate::core::config::runtime_config::SKIP_PATTERNS;

/// What the detector decided to do with the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BypassDecision {
    /// No bypass applies; continue with normal routing.
    Pass,
    /// Bypass with the default fake CLI response.
    Bypass,
    /// Bypass with a synthetic "naming" response: the body claims this is
    /// a new topic and gives a 3-word title taken from the first user
    /// message. Used by Claude Code's `isNewTopic` topic-extraction path.
    Naming { title: String },
}

/// Concatenate every text part of a content value into a single string.
/// Mirrors `getText` in 9router.
fn extract_text(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        return arr
            .iter()
            .filter(|c| c.get("type").and_then(|v| v.as_str()) == Some("text"))
            .filter_map(|c| c.get("text").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join(" ");
    }
    String::new()
}

/// Decide whether to bypass `body`.
///
/// `user_agent` should be the lower-cased `User-Agent` header. `cc_filter_naming`
/// enables Pattern #5 (Claude Code's `isNewTopic` detector) — the caller
/// usually wires this to a setting.
pub fn detect_bypass(body: &Value, user_agent: &str, cc_filter_naming: bool) -> BypassDecision {
    if !user_agent.contains("claude-cli") {
        return BypassDecision::Pass;
    }
    let Some(messages) = body.get("messages").and_then(|v| v.as_array()) else {
        return BypassDecision::Pass;
    };
    if messages.is_empty() {
        return BypassDecision::Pass;
    }

    // Pattern 1: Title extraction (assistant message with content[0].text == "{").
    if let Some(last) = messages.last() {
        if last.get("role").and_then(|v| v.as_str()) == Some("assistant") {
            let first_text = last
                .pointer("/content/0/text")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if first_text == "{" {
                return BypassDecision::Bypass;
            }
        }
    }

    // Pattern 2: Warmup.
    let first_text = extract_text(messages[0].get("content"));
    if first_text == "Warmup" {
        return BypassDecision::Bypass;
    }

    // Pattern 3: single-message "count" probe.
    if messages.len() == 1
        && messages[0].get("role").and_then(|v| v.as_str()) == Some("user")
        && extract_text(messages[0].get("content")) == "count"
    {
        return BypassDecision::Bypass;
    }

    // Pattern 4: SKIP_PATTERNS substring match across all user messages.
    let user_text = messages
        .iter()
        .filter(|m| m.get("role").and_then(|v| v.as_str()) == Some("user"))
        .map(|m| extract_text(m.get("content")))
        .collect::<Vec<_>>()
        .join(" ");
    if SKIP_PATTERNS.iter().any(|p| user_text.contains(p)) {
        return BypassDecision::Bypass;
    }

    // Pattern 5: CC `isNewTopic` (only when caller opted in).
    if cc_filter_naming {
        let system_from_messages = messages
            .iter()
            .find(|m| m.get("role").and_then(|v| v.as_str()) == Some("system"))
            .map(|m| extract_text(m.get("content")))
            .unwrap_or_default();

        let system_from_body = match body.get("system") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(arr)) => arr
                .iter()
                .filter(|s| s.get("type").and_then(|v| v.as_str()) == Some("text"))
                .filter_map(|s| s.get("text").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        };

        let system_text = if !system_from_messages.is_empty() {
            system_from_messages
        } else {
            system_from_body
        };
        if system_text.contains("isNewTopic") {
            let user_msg = messages
                .iter()
                .find(|m| m.get("role").and_then(|v| v.as_str()) == Some("user"));
            let user_text = extract_text(user_msg.and_then(|m| m.get("content")));
            let title = user_text
                .split_whitespace()
                .take(3)
                .collect::<Vec<_>>()
                .join(" ");
            return BypassDecision::Naming { title };
        }
    }

    BypassDecision::Pass
}

/// Default text used by the bypass handler when generating a fake CLI
/// command response.
pub const DEFAULT_BYPASS_TEXT: &str = "CLI Command Execution: Clear Terminal";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn non_claude_cli_passes_through() {
        let body = json!({"messages": [{"role": "user", "content": "Warmup"}]});
        assert_eq!(
            detect_bypass(&body, "curl/8.0", false),
            BypassDecision::Pass
        );
    }

    #[test]
    fn warmup_request_bypasses() {
        let body = json!({"messages": [{"role": "user", "content": "Warmup"}]});
        assert_eq!(
            detect_bypass(&body, "claude-cli/2.1", false),
            BypassDecision::Bypass
        );
    }

    #[test]
    fn count_request_bypasses() {
        let body = json!({"messages": [{"role": "user", "content": "count"}]});
        assert_eq!(
            detect_bypass(&body, "claude-cli/2.1", false),
            BypassDecision::Bypass
        );
    }

    #[test]
    fn title_extraction_pattern_bypasses() {
        let body = json!({"messages": [
            {"role": "user", "content": "real message"},
            {"role": "assistant", "content": [{"type": "text", "text": "{"}]}
        ]});
        assert_eq!(
            detect_bypass(&body, "claude-cli/2.1", false),
            BypassDecision::Bypass
        );
    }

    #[test]
    fn skip_pattern_matches_filler_titles() {
        let body = json!({"messages": [
            {"role": "user", "content": "Please write a 5-10 word title for the following conversation: x"}
        ]});
        assert_eq!(
            detect_bypass(&body, "claude-cli/2.1", false),
            BypassDecision::Bypass
        );
    }

    #[test]
    fn naming_pattern_returns_first_three_words() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "Build me a todo list app"}
            ],
            "system": [{"type": "text", "text": "isNewTopic detection: pls"}]
        });
        let decision = detect_bypass(&body, "claude-cli/2.1", true);
        assert_eq!(
            decision,
            BypassDecision::Naming {
                title: "Build me a".to_string()
            }
        );
    }

    #[test]
    fn naming_disabled_falls_through() {
        let body = json!({
            "messages": [{"role": "user", "content": "hello"}],
            "system": "isNewTopic"
        });
        assert_eq!(
            detect_bypass(&body, "claude-cli/2.1", false),
            BypassDecision::Pass
        );
    }
}
