//! Global `model(level)` / `model-level` thinking suffix helpers.
//!
//! 9router `thinkingUnified.stripThinkingSuffix` + effort parse used across
//! providers (not only Kiro/Codex). Levels: none|minimal|low|medium|high|xhigh|max.

/// Effort / thinking levels recognized in model suffixes.
pub const THINKING_LEVELS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];

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

/// Parse trailing thinking level from a model id.
///
/// Supports:
/// - `foo-high` / `foo-medium` / …
/// - `foo(high)` / `foo (high)`
///
/// Returns `(upstream_model, Some(level))` when a suffix was stripped.
pub fn strip_thinking_suffix(model: &str) -> (&str, Option<&str>) {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return (model, None);
    }

    // Parenthetical: model(high) or model (high)
    for level in THINKING_LEVELS {
        let paren = format!("({level})");
        if let Some(idx) = trimmed.rfind(&paren) {
            // ensure suffix is at end (allow trailing whitespace already trimmed)
            if idx + paren.len() == trimmed.len() {
                let base = trimmed[..idx].trim_end();
                if !base.is_empty() {
                    return (base, Some(*level));
                }
            }
        }
    }

    // Hyphen suffix: model-high
    for level in THINKING_LEVELS {
        let suffix = format!("-{level}");
        if let Some(base) = trimmed.strip_suffix(&suffix) {
            if !base.is_empty() {
                return (base, Some(*level));
            }
        }
    }

    (trimmed, None)
}

/// Apply strip to an owned model string; returns (upstream, optional level).
pub fn strip_thinking_suffix_owned(model: &str) -> (String, Option<String>) {
    let (base, level) = strip_thinking_suffix(model);
    (base.to_string(), level.map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
