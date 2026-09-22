//! MITM per-provider handler helpers (9router `src/mitm/server.js` + `config.js`,
//! OmniRoute `src/mitm/handlers/` + `targets/` + `detection/`).
//!
//! This module ports the pure mapping helpers used by the MITM dispatch:
//! - `get_tool_for_host` — map an upstream host to a MITM tool/provider.
//! - `resolve_router_path` — map an intercepted copilot path to the local
//!   router endpoint.
//! - `MITM_AGENT_TARGETS` — per-agent intercept descriptors (OmniRoute
//!   `src/mitm/targets/*.ts`): hosts, endpoint patterns, router paths.
//! - `strip_trailing_assistant_prefill` — Claude Code handler parity
//!   (OmniRoute `handlers/claudeCode.ts`): drop trailing assistant turns.
//!
//! The full per-provider interceptors (antigravity model rewrite + SSE error
//! framing, kiro OpenAI→AWS EventStream conversion, copilot URL remap) build
//! on these helpers; cursor and trae are not-implemented stubs in both JS and
//! here (trae: `trae.invalid` placeholder, viability investigating).

/// URL path substrings that mark a request as a chat turn for each tool
/// (9router config.js URL_PATTERNS:26-31; OmniRoute targets/*/endpointPatterns).
pub const URL_PATTERNS: &[(&str, &[&str])] = &[
    (
        "antigravity",
        &[":generateContent", ":streamGenerateContent"],
    ),
    (
        "copilot",
        &["/chat/completions", "/v1/messages", "/responses"],
    ),
    (
        "ghe-copilot",
        &["/chat/completions", "/v1/chat/completions", "/responses"],
    ),
    ("kiro", &["/generateAssistantResponse"]),
    ("cursor", &["/BidiAppend", "/RunSSE", "/RunPoll", "/Run"]),
    (
        "codex",
        &[
            "/backend-api/codex/chat/completions",
            "/v1/chat/completions",
        ],
    ),
    ("claude-code", &["/v1/messages"]),
    ("open-code", &["/v1/chat/completions"]),
    ("zed", &["/v1/chat/completions"]),
    // trae: viability investigating — no endpoint patterns (matches nothing).
    ("trae", &[]),
];

/// A MITM agent intercept target (OmniRoute `src/mitm/targets/*.ts`).
pub struct MitmAgentTarget {
    /// Agent id (`agentId` / target `id`).
    pub id: &'static str,
    /// Upstream hosts steered to the proxy (`hosts`).
    pub hosts: &'static [&'static str],
    /// Router path intercepted bodies are forwarded to.
    pub router_path: &'static str,
    /// Whether the agent is usable (`viability: "investigating"` → false).
    pub viable: bool,
}

/// Per-agent intercept targets (OmniRoute `src/mitm/targets/`).
pub const MITM_AGENT_TARGETS: &[MitmAgentTarget] = &[
    MitmAgentTarget {
        id: "antigravity",
        hosts: &[
            "daily-cloudcode-pa.googleapis.com",
            "cloudcode-pa.googleapis.com",
        ],
        router_path: "/v1/chat/completions",
        viable: true,
    },
    MitmAgentTarget {
        id: "copilot",
        hosts: &[
            "api.githubcopilot.com",
            "copilot-proxy.githubusercontent.com",
        ],
        router_path: "/v1/chat/completions",
        viable: true,
    },
    // GHE Copilot matches via configured gheUrl (hosts empty upstream).
    MitmAgentTarget {
        id: "ghe-copilot",
        hosts: &[],
        router_path: "/v1/chat/completions",
        viable: true,
    },
    MitmAgentTarget {
        id: "kiro",
        hosts: &[
            "runtime.us-east-1.kiro.dev",
            "q.us-east-1.amazonaws.com",
            "codewhisperer.us-east-1.amazonaws.com",
        ],
        router_path: "/v1/messages",
        viable: true,
    },
    MitmAgentTarget {
        id: "cursor",
        hosts: &["api2.cursor.sh"],
        router_path: "/v1/chat/completions",
        viable: false, // stub in both JS and here
    },
    MitmAgentTarget {
        id: "codex",
        hosts: &["chatgpt.com"],
        router_path: "/v1/chat/completions",
        viable: true,
    },
    // Opt-in: only fires when the user configures DNS routing for it.
    MitmAgentTarget {
        id: "claude-code",
        hosts: &["api.anthropic.com"],
        router_path: "/v1/messages",
        viable: true,
    },
    MitmAgentTarget {
        id: "open-code",
        hosts: &["opencode.ai"],
        router_path: "/v1/chat/completions",
        viable: true,
    },
    MitmAgentTarget {
        id: "zed",
        hosts: &["api.zed.dev"],
        router_path: "/v1/chat/completions",
        viable: true,
    },
    // Placeholder host: registered for UI listing, never matches traffic.
    MitmAgentTarget {
        id: "trae",
        hosts: &["trae.invalid"],
        router_path: "/v1/chat/completions",
        viable: false,
    },
];

/// Filesystem paths proving an agent is installed (OmniRoute
/// `src/mitm/detection/*.ts`, `~`-prefixed entries expand to `$HOME`).
pub const MITM_AGENT_DETECTION_PATHS: &[(&str, &[&str])] = &[
    (
        "codex",
        &[
            "/usr/local/bin/codex",
            "/usr/bin/codex",
            "~/.local/bin/codex",
            "~/.npm-global/bin/codex",
        ],
    ),
    (
        "claude-code",
        &[
            "/usr/local/bin/claude",
            "/usr/bin/claude",
            "~/.local/bin/claude",
            "~/.claude",
        ],
    ),
    (
        "open-code",
        &[
            "/usr/bin/opencode",
            "/usr/local/bin/opencode",
            "~/.local/bin/opencode",
            "~/.opencode",
            "~/.config/opencode",
        ],
    ),
    (
        "zed",
        &[
            "/usr/bin/zed",
            "/usr/local/bin/zed",
            "~/.local/bin/zed",
            "~/.config/zed",
        ],
    ),
    ("kiro", &["~/.kiro"]),
    ("cursor", &["~/.cursor"]),
    ("copilot", &["~/.copilot"]),
];

/// Detect whether an agent is installed (filesystem probes only, OmniRoute
/// detection parity — never spawns shells).
pub fn detect_agent_installed(agent_id: &str) -> Option<String> {
    let (_, paths) = MITM_AGENT_DETECTION_PATHS
        .iter()
        .find(|(id, _)| *id == agent_id)?;
    let home = std::env::var("HOME").unwrap_or_default();
    for p in *paths {
        let expanded = if let Some(rest) = p.strip_prefix("~/") {
            format!("{home}/{rest}")
        } else {
            p.to_string()
        };
        if std::path::Path::new(&expanded).exists() {
            return Some(expanded);
        }
    }
    None
}

/// Strip trailing assistant prefill turns (OmniRoute `handlers/claudeCode.ts`):
/// loop over ALL consecutive trailing assistant turns, but never strip the
/// array to empty (an empty messages array is itself invalid).
pub fn strip_trailing_assistant_prefill(messages: &mut Vec<serde_json::Value>) {
    while messages.len() > 1
        && messages
            .last()
            .and_then(|m| m.get("role"))
            .and_then(|r| r.as_str())
            == Some("assistant")
    {
        messages.pop();
    }
}

/// 9router config.js isChatRequest(): URL pattern match first; for kiro also
/// accept IDE ≥1.0.228 which POSTs to `/` with
/// `x-amz-target: KiroRuntimeService.GenerateAssistantResponse`.
pub fn is_chat_request(tool: &str, url: &str, x_amz_target: Option<&str>) -> bool {
    let patterns = URL_PATTERNS
        .iter()
        .find(|(t, _)| *t == tool)
        .map(|(_, p)| *p)
        .unwrap_or(&[]);
    if patterns.iter().any(|p| url.contains(p)) {
        return true;
    }
    if tool == "kiro" {
        return x_amz_target
            .unwrap_or("")
            .contains("GenerateAssistantResponse");
    }
    false
}

/// Upstream host → tool mapping (9router config.js getToolForHost +
/// OmniRoute `src/mitm/targets/*.ts` hosts). Hosts are matched
/// case-insensitively by substring suffix.
const TOOL_HOSTS: &[(&str, &str)] = &[
    ("api.individual.githubcopilot.com", "copilot"),
    ("api.githubcopilot.com", "copilot"),
    ("copilot-proxy.githubusercontent.com", "copilot"),
    ("daily-cloudcode-pa.googleapis.com", "antigravity"),
    ("cloudcode-pa.googleapis.com", "antigravity"),
    ("q.us-east-1.amazonaws.com", "kiro"),
    ("runtime.us-east-1.kiro.dev", "kiro"),
    ("codewhisperer.runtime.us-east-1.kiro.dev", "kiro"),
    ("api2.cursor.sh", "cursor"),
    ("chatgpt.com", "codex"),
    ("api.anthropic.com", "claude-code"),
    ("opencode.ai", "open-code"),
    ("api.zed.dev", "zed"),
];

/// Map an intercepted upstream host to its MITM tool/provider. Returns `None`
/// for hosts not handled by the MITM proxy. Non-viable agents (cursor stub,
/// trae investigating) never match — trae's `trae.invalid` placeholder is
/// deliberately absent here so it can never match real traffic.
pub fn get_tool_for_host(host: &str) -> Option<&'static str> {
    let target = MITM_AGENT_TARGETS
        .iter()
        .find(|t| t.viable && t.hosts.iter().any(|h| host_match(host, h)))
        .map(|t| t.id);
    if target.is_some() {
        return target;
    }
    // Legacy fallback (9router hosts not covered by a target entry).
    let host = host.trim().to_ascii_lowercase();
    let host = host.split(':').next().unwrap_or(&host);
    TOOL_HOSTS
        .iter()
        .find(|(h, _)| host.ends_with(*h))
        .map(|(_, tool)| *tool)
}

fn host_match(host: &str, pattern: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    let host = host.split(':').next().unwrap_or(&host);
    host.ends_with(&pattern.to_ascii_lowercase())
}

/// Render a sanitized MITM error JSON body (OmniRoute
/// `handlers/base.ts writeError` parity: `{error: {message, type:
/// "mitm_error"}}`). Secrets (bearer tokens, api keys) are redacted so they
/// never leak into traffic-inspector transcripts.
pub fn mitm_error_body(message: &str) -> String {
    serde_json::json!({
        "error": { "message": sanitize_mitm_error(message), "type": "mitm_error" }
    })
    .to_string()
}

fn sanitize_mitm_error(message: &str) -> String {
    let mut current = message.to_string();
    for marker in ["Bearer ", "bearer ", "api-key ", "apikey "] {
        let mut merged = String::with_capacity(current.len());
        let mut remaining = current.as_str();
        while let Some(pos) = remaining.find(marker) {
            let start = pos + marker.len();
            merged.push_str(&remaining[..start]);
            let tail = &remaining[start..];
            let end = tail
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',' || c == '}')
                .unwrap_or(tail.len());
            let secret = &tail[..end];
            if secret.len() > 8 {
                merged.push_str(&format!("{}…<redacted>", &secret[..4]));
            } else {
                merged.push_str("<redacted>");
            }
            remaining = &tail[end..];
        }
        merged.push_str(remaining);
        current = merged;
    }
    current
}

/// Copilot intercept path → local router endpoint (9router copilot.js URL_MAP).
pub fn resolve_router_path(req_path: &str) -> &'static str {
    if req_path.contains("chat/completions") {
        "/v1/chat/completions"
    } else if req_path.contains("/v1/messages") {
        "/v1/messages"
    } else if req_path.contains("/responses") {
        "/v1/responses"
    } else {
        "/v1/chat/completions"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mitm_resolve_router_path_maps_endpoints() {
        assert_eq!(
            resolve_router_path("/chat/completions"),
            "/v1/chat/completions"
        );
        assert_eq!(resolve_router_path("/api/v1/messages"), "/v1/messages");
        assert_eq!(resolve_router_path("/v1/responses"), "/v1/responses");
        assert_eq!(resolve_router_path("/foo"), "/v1/chat/completions");
    }

    #[test]
    fn mitm_get_tool_for_host() {
        assert_eq!(
            get_tool_for_host("api.individual.githubcopilot.com"),
            Some("copilot")
        );
        assert_eq!(
            get_tool_for_host("daily-cloudcode-pa.googleapis.com"),
            Some("antigravity")
        );
        assert_eq!(get_tool_for_host("q.us-east-1.amazonaws.com"), Some("kiro"));
        assert_eq!(get_tool_for_host("api2.cursor.sh"), Some("cursor"));
        assert_eq!(get_tool_for_host("example.com"), None);
    }

    #[test]
    fn mitm_get_tool_for_host_new_agents() {
        // OmniRoute targets/*.ts parity.
        assert_eq!(get_tool_for_host("chatgpt.com"), Some("codex"));
        assert_eq!(get_tool_for_host("api.anthropic.com"), Some("claude-code"));
        assert_eq!(get_tool_for_host("opencode.ai"), Some("open-code"));
        assert_eq!(get_tool_for_host("api.zed.dev"), Some("zed"));
        assert_eq!(get_tool_for_host("api.githubcopilot.com"), Some("copilot"));
        assert_eq!(
            get_tool_for_host("copilot-proxy.githubusercontent.com"),
            Some("copilot")
        );
        // trae.invalid must never match real traffic.
        assert_eq!(get_tool_for_host("trae.invalid"), None);
        assert_eq!(get_tool_for_host("trae.com"), None);
    }

    #[test]
    fn mitm_is_chat_request_new_agents() {
        assert!(is_chat_request(
            "codex",
            "/backend-api/codex/chat/completions",
            None
        ));
        assert!(is_chat_request("claude-code", "/v1/messages", None));
        assert!(is_chat_request("open-code", "/v1/chat/completions", None));
        assert!(is_chat_request("zed", "/v1/chat/completions", None));
        assert!(!is_chat_request("trae", "/anything", None));
    }

    #[test]
    fn mitm_strip_trailing_assistant_prefill() {
        use serde_json::json;
        let mut messages = vec![
            json!({"role": "user", "content": "hi"}),
            json!({"role": "assistant", "content": "pre1"}),
            json!({"role": "assistant", "content": "pre2"}),
        ];
        strip_trailing_assistant_prefill(&mut messages);
        assert_eq!(messages.len(), 1);
        // Never strips to empty.
        let mut lone = vec![json!({"role": "assistant", "content": "only"})];
        strip_trailing_assistant_prefill(&mut lone);
        assert_eq!(lone.len(), 1);
        // Non-assistant tail untouched.
        let mut user_tail = vec![
            json!({"role": "assistant", "content": "a"}),
            json!({"role": "user", "content": "b"}),
        ];
        strip_trailing_assistant_prefill(&mut user_tail);
        assert_eq!(user_tail.len(), 2);
    }

    #[test]
    fn mitm_agent_targets_router_paths() {
        let by_id = |id: &str| {
            MITM_AGENT_TARGETS
                .iter()
                .find(|t| t.id == id)
                .unwrap_or_else(|| panic!("target {id}"))
        };
        assert_eq!(by_id("codex").router_path, "/v1/chat/completions");
        assert_eq!(by_id("claude-code").router_path, "/v1/messages");
        assert_eq!(by_id("kiro").router_path, "/v1/messages");
        assert_eq!(by_id("zed").router_path, "/v1/chat/completions");
        assert!(!by_id("trae").viable);
        assert!(!by_id("cursor").viable);
    }

    #[test]
    fn mitm_get_tool_for_host_matches_with_port() {
        assert_eq!(get_tool_for_host("api2.cursor.sh:443"), Some("cursor"));
    }

    #[test]
    fn mitm_get_tool_for_host_is_case_insensitive() {
        assert_eq!(
            get_tool_for_host("API.INDIVIDUAL.GITHUBCOPILOT.COM"),
            Some("copilot")
        );
    }

    #[test]
    fn mitm_error_body_sanitizes_secrets() {
        let body = mitm_error_body("OmniRoute 401: Bearer sk-secret-token-123");
        assert!(body.contains("mitm_error"));
        assert!(!body.contains("sk-secret-token-123"));
        assert!(body.contains("redacted"));
        let clean = mitm_error_body("plain failure");
        assert!(clean.contains("plain failure"));
    }
}
