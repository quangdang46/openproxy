//! Port of `open-sse/utils/toolDeduper.js`.
//!
//! Strip built-in / duplicate tool definitions when an equivalent MCP
//! tool is also present, to reduce token bloat for Claude clients.

use regex::Regex;
use serde_json::Value;

/// One entry of the dedup ruleset: if any of `triggers` is present in the
/// inbound tool list, then any tool whose name matches any of `strip` is
/// removed.
struct DedupRule {
    triggers: &'static [Pattern],
    strip: &'static [Pattern],
}

#[allow(clippy::enum_variant_names)]
enum Pattern {
    Exact(&'static str),
    Regex(&'static str),
}

impl Pattern {
    fn matches(&self, name: &str) -> bool {
        match self {
            Pattern::Exact(s) => *s == name,
            // Compiling once per call is wasteful but the rules are small
            // and the pattern set is only used on tool definitions (which
            // aren't a hot path). If profiling shows up, switch to a
            // `Lazy<Regex>` per pattern.
            Pattern::Regex(re) => Regex::new(re).map(|r| r.is_match(name)).unwrap_or(false),
        }
    }
}

const RULES: &[DedupRule] = &[
    DedupRule {
        triggers: &[
            Pattern::Exact("mcp__exa__web_search_exa"),
            Pattern::Exact("mcp__exa__web_fetch_exa"),
        ],
        strip: &[
            Pattern::Exact("WebSearch"),
            Pattern::Exact("WebFetch"),
            Pattern::Exact("mcp__workspace__web_fetch"),
        ],
    },
    DedupRule {
        triggers: &[
            Pattern::Exact("mcp__tavily__tavily_search"),
            Pattern::Exact("mcp__tavily__tavily_extract"),
        ],
        strip: &[
            Pattern::Exact("WebSearch"),
            Pattern::Exact("WebFetch"),
            Pattern::Exact("mcp__workspace__web_fetch"),
        ],
    },
    DedupRule {
        triggers: &[Pattern::Regex(r"^mcp__browsermcp__")],
        strip: &[Pattern::Regex(r"^mcp__Claude_in_Chrome__")],
    },
];

fn tool_name(t: &Value) -> &str {
    t.get("name")
        .and_then(|v| v.as_str())
        .or_else(|| {
            t.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("")
}

/// Result of [`dedupe_tools`].
#[derive(Debug, Clone)]
pub struct DedupeResult {
    /// Tools array after stripping. Same shape as input.
    pub tools: Vec<Value>,
    /// Names of any tools that were removed.
    pub stripped: Vec<String>,
}

/// Dedupe an OpenAI / Claude tools array by applying [`RULES`].
pub fn dedupe_tools(tools: &[Value]) -> DedupeResult {
    if tools.is_empty() {
        return DedupeResult {
            tools: tools.to_vec(),
            stripped: Vec::new(),
        };
    }
    let names: Vec<&str> = tools.iter().map(tool_name).collect();
    let mut to_strip: Vec<String> = Vec::new();
    for rule in RULES {
        let has_trigger = names
            .iter()
            .any(|n| rule.triggers.iter().any(|p| p.matches(n)));
        if !has_trigger {
            continue;
        }
        for n in &names {
            if rule.strip.iter().any(|p| p.matches(n))
                && !to_strip.iter().any(|x| x == n)
            {
                to_strip.push((*n).to_string());
            }
        }
    }
    if to_strip.is_empty() {
        return DedupeResult {
            tools: tools.to_vec(),
            stripped: Vec::new(),
        };
    }
    let strip_set: std::collections::HashSet<&str> =
        to_strip.iter().map(String::as_str).collect();
    let kept = tools
        .iter()
        .filter(|t| !strip_set.contains(tool_name(t)))
        .cloned()
        .collect();
    DedupeResult {
        tools: kept,
        stripped: to_strip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exa_mcp_strips_builtin_web_tools() {
        let tools = vec![
            json!({"name": "WebSearch"}),
            json!({"name": "WebFetch"}),
            json!({"name": "mcp__exa__web_search_exa"}),
        ];
        let res = dedupe_tools(&tools);
        assert_eq!(res.tools.len(), 1);
        assert_eq!(res.stripped.len(), 2);
        assert!(res.stripped.contains(&"WebSearch".to_string()));
    }

    #[test]
    fn no_trigger_returns_input_unchanged() {
        let tools = vec![json!({"name": "WebSearch"})];
        let res = dedupe_tools(&tools);
        assert_eq!(res.tools.len(), 1);
        assert!(res.stripped.is_empty());
    }

    #[test]
    fn browsermcp_regex_strips_claude_in_chrome() {
        let tools = vec![
            json!({"name": "mcp__browsermcp__do_thing"}),
            json!({"name": "mcp__Claude_in_Chrome__navigate"}),
        ];
        let res = dedupe_tools(&tools);
        assert_eq!(res.tools.len(), 1);
        assert_eq!(res.stripped, vec!["mcp__Claude_in_Chrome__navigate"]);
    }

    #[test]
    fn supports_function_wrapped_tools() {
        let tools = vec![
            json!({"function": {"name": "WebSearch"}}),
            json!({"function": {"name": "mcp__exa__web_search_exa"}}),
        ];
        let res = dedupe_tools(&tools);
        assert_eq!(res.tools.len(), 1);
    }
}
