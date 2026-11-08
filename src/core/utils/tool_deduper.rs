//! Port of `open-sse/utils/toolDeduper.js`.
//!
//! Two normalizations before dispatch:
//! - MCP-equivalent built-in tool dedup (Claude clients only, reduces token
//!   bloat).
//! - Exact same-name tool dedup for DeepSeek models — the DeepSeek upstream
//!   rejects duplicate tool names with 400 "Tool names must be unique" on
//!   every endpoint. First definition wins; `tool_choice` and
//!   message-history references are by name/id so nothing breaks.

use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;

use crate::core::utils::client_detector::ClientTool;

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

/// Port of `isDeepSeekModel` (`open-sse/providers/models/helpers.js`).
///
/// Accepts both bare ids (`deepseek-v4-pro(max)`) and provider-prefixed
/// ones (`openrouter/deepseek-chat`): strips a trailing `(level)` suffix,
/// takes the last `/` segment, and requires a `deepseek-` prefix
/// (case-insensitive).
fn is_deepseek_model(model_id: &str) -> bool {
    let mut clean = model_id.trim();
    // `model(level)` thinking suffix — upstream strips `/\([^()]+\)\s*$/`.
    if clean.ends_with(')') {
        if let Some(open) = clean.rfind('(') {
            let inner = &clean[open + 1..clean.len() - 1];
            if !inner.trim().is_empty() && !inner.contains('(') && !inner.contains(')') {
                clean = clean[..open].trim_end();
            }
        }
    }
    let base = clean.rsplit('/').next().unwrap_or(clean);
    base.len() >= "deepseek-".len() && base[.."deepseek-".len()].eq_ignore_ascii_case("deepseek-")
}

/// Result of [`dedupe_tools`].
#[derive(Debug, Clone)]
pub struct DedupeResult {
    /// Tools array after stripping. Same shape as input.
    pub tools: Vec<Value>,
    /// Names of any tools that were removed.
    pub stripped: Vec<String>,
}

/// Dedupe an OpenAI / Claude tools array.
///
/// Ports `dedupeTools` (`open-sse/utils/toolDeduper.js`):
/// - `client_tool` gates the MCP-equivalent built-in rules to Claude
///   clients only (`opts.clientTool === "claude"`).
/// - `model` enables the DeepSeek same-name pass when it resolves to a
///   `deepseek-*` model (any client × provider serving one).
pub fn dedupe_tools(tools: &[Value], client_tool: Option<ClientTool>, model: &str) -> DedupeResult {
    if tools.is_empty() {
        return DedupeResult {
            tools: tools.to_vec(),
            stripped: Vec::new(),
        };
    }
    let names: Vec<&str> = tools.iter().map(tool_name).collect();
    let mut to_strip: Vec<String> = Vec::new();

    // MCP-based built-in dedup: Claude clients only (existing behavior).
    if client_tool == Some(ClientTool::Claude) {
        for rule in RULES {
            let has_trigger = names
                .iter()
                .any(|n| rule.triggers.iter().any(|p| p.matches(n)));
            if !has_trigger {
                continue;
            }
            for n in &names {
                if rule.strip.iter().any(|p| p.matches(n)) && !to_strip.iter().any(|x| x == n) {
                    to_strip.push((*n).to_string());
                }
            }
        }
    }

    // Exact-name dedup: DeepSeek upstream rejects duplicate tool names.
    // Applies to every client × provider that serves a deepseek-* model
    // (official API, Console Go, LiteLLM gateways); non-DeepSeek models
    // are untouched. First definition wins.
    let mut to_drop: HashSet<usize> = HashSet::new();
    if is_deepseek_model(model) {
        let mut seen: HashSet<&str> = HashSet::new();
        for (i, n) in names.iter().enumerate() {
            if n.is_empty() {
                continue;
            }
            if !seen.insert(n) {
                to_drop.insert(i);
            }
        }
    }

    if to_strip.is_empty() && to_drop.is_empty() {
        return DedupeResult {
            tools: tools.to_vec(),
            stripped: Vec::new(),
        };
    }
    let strip_set: HashSet<&str> = to_strip.iter().map(String::as_str).collect();
    // JS `stripped` order: dropped (same-name) names first, then strip names.
    let mut stripped: Vec<String> = (0..tools.len())
        .filter(|i| to_drop.contains(i))
        .map(|i| names[i].to_string())
        .collect();
    stripped.extend(to_strip.iter().cloned());
    let kept = tools
        .iter()
        .enumerate()
        .filter(|(i, t)| !to_drop.contains(i) && !strip_set.contains(tool_name(t)))
        .map(|(_, t)| t.clone())
        .collect();
    DedupeResult {
        tools: kept,
        stripped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exa_mcp_strips_builtin_web_tools_for_claude_client() {
        let tools = vec![
            json!({"name": "WebSearch"}),
            json!({"name": "WebFetch"}),
            json!({"name": "mcp__exa__web_search_exa"}),
        ];
        let res = dedupe_tools(&tools, Some(ClientTool::Claude), "gpt-4o");
        assert_eq!(res.tools.len(), 1);
        assert_eq!(res.stripped.len(), 2);
        assert!(res.stripped.contains(&"WebSearch".to_string()));
    }

    #[test]
    fn mcp_equivalent_dedup_skipped_for_non_claude_client() {
        // Same tool list that strips under Claude must pass through for
        // other clients (upstream gates DEDUP_RULES on clientTool === "claude").
        let tools = vec![
            json!({"name": "WebSearch"}),
            json!({"name": "mcp__exa__web_search_exa"}),
        ];
        for client in [
            Some(ClientTool::Codex),
            Some(ClientTool::GeminiCli),
            Some(ClientTool::DeepseekTui),
            None,
        ] {
            let res = dedupe_tools(&tools, client, "gpt-4o");
            assert_eq!(res.tools.len(), 2, "client={client:?} must not strip");
            assert!(res.stripped.is_empty());
        }
    }

    #[test]
    fn deepseek_same_name_tools_deduped_first_wins() {
        let tools = vec![
            json!({"name": "Read", "description": "first"}),
            json!({"name": "Read", "description": "second"}),
            json!({"name": "Write"}),
        ];
        let res = dedupe_tools(&tools, Some(ClientTool::Codex), "deepseek-v4-pro");
        assert_eq!(res.tools.len(), 2);
        assert_eq!(res.tools[0]["description"], "first");
        assert_eq!(res.stripped, vec!["Read".to_string()]);
    }

    #[test]
    fn deepseek_dedup_handles_function_wrapped_and_suffix_models() {
        let tools = vec![
            json!({"function": {"name": "Bash"}}),
            json!({"function": {"name": "Bash"}}),
        ];
        // Provider-prefixed id + (level) thinking suffix still counts.
        let res = dedupe_tools(&tools, None, "openrouter/deepseek-chat(max)");
        assert_eq!(res.tools.len(), 1);
        assert_eq!(res.stripped, vec!["Bash".to_string()]);
    }

    #[test]
    fn non_deepseek_model_keeps_duplicate_names() {
        let tools = vec![json!({"name": "Read"}), json!({"name": "Read"})];
        let res = dedupe_tools(&tools, Some(ClientTool::Codex), "gpt-4o");
        assert_eq!(res.tools.len(), 2);
        assert!(res.stripped.is_empty());
        // claude-sonnet is not deepseek-* either.
        let res = dedupe_tools(&tools, Some(ClientTool::Claude), "claude-sonnet-4.6");
        assert_eq!(res.tools.len(), 2);
    }

    #[test]
    fn deepseek_empty_tool_names_are_ignored() {
        let tools = vec![
            json!({"description": "no name"}),
            json!({"description": "no name"}),
        ];
        let res = dedupe_tools(&tools, None, "deepseek-v4-flash");
        assert_eq!(res.tools.len(), 2);
        assert!(res.stripped.is_empty());
    }

    #[test]
    fn no_trigger_returns_input_unchanged() {
        let tools = vec![json!({"name": "WebSearch"})];
        let res = dedupe_tools(&tools, Some(ClientTool::Claude), "gpt-4o");
        assert_eq!(res.tools.len(), 1);
        assert!(res.stripped.is_empty());
    }

    #[test]
    fn browsermcp_regex_strips_claude_in_chrome() {
        let tools = vec![
            json!({"name": "mcp__browsermcp__do_thing"}),
            json!({"name": "mcp__Claude_in_Chrome__navigate"}),
        ];
        let res = dedupe_tools(&tools, Some(ClientTool::Claude), "claude-sonnet-4.6");
        assert_eq!(res.tools.len(), 1);
        assert_eq!(res.stripped, vec!["mcp__Claude_in_Chrome__navigate"]);
    }

    #[test]
    fn supports_function_wrapped_tools() {
        let tools = vec![
            json!({"function": {"name": "WebSearch"}}),
            json!({"function": {"name": "mcp__exa__web_search_exa"}}),
        ];
        let res = dedupe_tools(&tools, Some(ClientTool::Claude), "claude-sonnet-4.6");
        assert_eq!(res.tools.len(), 1);
    }
}
