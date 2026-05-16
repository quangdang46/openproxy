//! Port of `open-sse/translator/helpers/maxTokensHelper.js`.
//!
//! Decide the `max_tokens` value to forward upstream:
//!   - Default from [`DEFAULT_MAX_TOKENS`] when the caller did not set one.
//!   - Auto-bump to [`DEFAULT_MIN_TOKENS`] when tool calls are present so
//!     argument-streaming has room to finish.
//!   - Always strictly greater than `thinking.budget_tokens` (Claude API
//!     hard requirement; equality is rejected).

use serde_json::Value;

use crate::core::config::runtime_config::{DEFAULT_MAX_TOKENS, DEFAULT_MIN_TOKENS};

/// Compute the adjusted `max_tokens` for `body`. Does NOT mutate.
pub fn adjust_max_tokens(body: &Value) -> u32 {
    let mut max_tokens = body
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(DEFAULT_MAX_TOKENS);

    let has_tools = body
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if has_tools && max_tokens < DEFAULT_MIN_TOKENS {
        max_tokens = DEFAULT_MIN_TOKENS;
    }

    if let Some(budget) = body
        .pointer("/thinking/budget_tokens")
        .and_then(|v| v.as_u64())
    {
        let budget = budget as u32;
        if max_tokens <= budget {
            max_tokens = budget.saturating_add(1024);
        }
    }

    max_tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn returns_default_when_unset() {
        let body = json!({});
        assert_eq!(adjust_max_tokens(&body), DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn honours_explicit_value() {
        let body = json!({"max_tokens": 1234});
        assert_eq!(adjust_max_tokens(&body), 1234);
    }

    #[test]
    fn bumps_to_min_when_tools_present_and_value_low() {
        let body = json!({"max_tokens": 100, "tools": [{}]});
        assert_eq!(adjust_max_tokens(&body), DEFAULT_MIN_TOKENS);
    }

    #[test]
    fn skips_min_bump_when_no_tools() {
        let body = json!({"max_tokens": 100});
        assert_eq!(adjust_max_tokens(&body), 100);
    }

    #[test]
    fn enforces_strictly_greater_than_budget() {
        let body = json!({
            "max_tokens": 16000,
            "thinking": {"budget_tokens": 16000}
        });
        // 16000 == budget → bump to budget + 1024
        assert_eq!(adjust_max_tokens(&body), 16000 + 1024);
    }

    #[test]
    fn no_bump_when_max_already_above_budget() {
        let body = json!({
            "max_tokens": 32000,
            "thinking": {"budget_tokens": 16000}
        });
        assert_eq!(adjust_max_tokens(&body), 32000);
    }
}
