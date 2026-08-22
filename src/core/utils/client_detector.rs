//! Port of `open-sse/utils/clientDetector.js`. Detects which CLI client is
//! making the request (Claude Code, Gemini CLI, Antigravity, Codex,
//! GitHub Copilot, DeepSeek TUI) so the request can be passed through
//! losslessly when it matches the upstream provider.

use serde_json::Value;
use std::collections::HashMap;

/// Identifier for a recognised CLI client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientTool {
    Claude,
    GeminiCli,
    Antigravity,
    Codex,
    GithubCopilot,
    DeepseekTui,
}

impl ClientTool {
    pub fn as_str(self) -> &'static str {
        match self {
            ClientTool::Claude => "claude",
            ClientTool::GeminiCli => "gemini-cli",
            ClientTool::Antigravity => "antigravity",
            ClientTool::Codex => "codex",
            ClientTool::GithubCopilot => "github-copilot",
            ClientTool::DeepseekTui => "deepseek-tui",
        }
    }
}

/// Detect which CLI tool is making the request.
///
/// Headers must already be lower-cased (callers responsibility — Rust HTTP
/// stacks usually do this). Returns `None` if no recognised client.
pub fn detect_client_tool(headers: &HashMap<String, String>, body: &Value) -> Option<ClientTool> {
    let ua = headers
        .get("user-agent")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let x_app = headers
        .get("x-app")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let openai_intent = headers
        .get("openai-intent")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let initiator = headers
        .get("x-initiator")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    // 9router clientDetector.js:24 originator header (lower-cased), used by the
    // codex branch below to catch codex_work_desktop etc.
    let originator = headers
        .get("originator")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    // Antigravity: detected via body field, not header.
    if body.get("userAgent").and_then(|v| v.as_str()) == Some("antigravity") {
        return Some(ClientTool::Antigravity);
    }

    if ua.contains("githubcopilotchat")
        || openai_intent == "conversation-panel"
        || initiator == "user"
    {
        return Some(ClientTool::GithubCopilot);
    }

    if ua.contains("claude-cli") || ua.contains("claude-code") || x_app == "cli" {
        return Some(ClientTool::Claude);
    }

    if ua.contains("gemini-cli") {
        return Some(ClientTool::GeminiCli);
    }

    // 9router clientDetector.js:43-44 — match codex-tui, codex-cli,
    // codex_cli_rs, "codex desktop", or an originator starting with "codex_"
    // (catches codex_work_desktop etc.). Missing these meant Codex Desktop and
    // codex-tui were translated instead of passed through losslessly.
    if ua.contains("codex-tui")
        || ua.contains("codex-cli")
        || ua.contains("codex_cli_rs")
        || ua.contains("codex desktop")
        || originator.starts_with("codex_")
    {
        return Some(ClientTool::Codex);
    }

    if ua.contains("deepseek-tui") {
        return Some(ClientTool::DeepseekTui);
    }

    None
}

/// Native (CLI tool, provider) pairings that allow lossless passthrough.
fn native_providers(tool: ClientTool) -> &'static [&'static str] {
    match tool {
        ClientTool::Claude => &["claude", "anthropic"],
        ClientTool::GeminiCli => &["gemini-cli"],
        ClientTool::Antigravity => &["antigravity"],
        ClientTool::Codex => &["codex"],
        _ => &[],
    }
}

/// Returns true iff this CLI tool + provider combination should be passed
/// through losslessly without translation.
pub fn is_native_passthrough(client_tool: Option<ClientTool>, provider: &str) -> bool {
    let Some(tool) = client_tool else {
        return false;
    };
    let normalized = if provider.starts_with("anthropic-compatible") {
        "anthropic"
    } else {
        provider
    };
    native_providers(tool).contains(&normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn h(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn detects_claude_via_user_agent() {
        let headers = h(&[("user-agent", "claude-cli/2.1.92")]);
        assert_eq!(
            detect_client_tool(&headers, &json!({})),
            Some(ClientTool::Claude)
        );
    }

    #[test]
    fn detects_antigravity_via_body() {
        let body = json!({"userAgent": "antigravity"});
        assert_eq!(
            detect_client_tool(&HashMap::new(), &body),
            Some(ClientTool::Antigravity)
        );
    }

    #[test]
    fn detects_copilot_via_intent() {
        let headers = h(&[("openai-intent", "conversation-panel")]);
        assert_eq!(
            detect_client_tool(&headers, &json!({})),
            Some(ClientTool::GithubCopilot)
        );
    }

    #[test]
    fn passthrough_matches_anthropic_variants() {
        assert!(is_native_passthrough(
            Some(ClientTool::Claude),
            "anthropic-compatible-1"
        ));
        assert!(is_native_passthrough(Some(ClientTool::Claude), "claude"));
        assert!(!is_native_passthrough(Some(ClientTool::Claude), "openai"));
        assert!(!is_native_passthrough(None, "claude"));
    }

    #[test]
    fn detects_codex_tui_and_desktop() {
        // 9router clientDetector.js:43-44 — all four UA strings + originator.
        let cases: &[(&str, &str)] = &[
            ("user-agent", "codex-tui/0.5.0"),
            ("user-agent", "codex_cli_rs/0.2.0"),
            ("user-agent", "Codex Desktop"),
            ("user-agent", "codex-cli"),
            ("originator", "codex_work_desktop"),
        ];
        for (k, v) in cases {
            let headers = h(&[(*k, *v)]);
            assert_eq!(
                detect_client_tool(&headers, &json!({})),
                Some(ClientTool::Codex),
                "expected Codex for {k}={v}"
            );
        }
        // A non-codex originator is not misdetected.
        let headers = h(&[("originator", "claude_work_desktop")]);
        assert_ne!(
            detect_client_tool(&headers, &json!({})),
            Some(ClientTool::Codex)
        );
    }
}
