//! MITM per-provider handler helpers (9router `src/mitm/server.js` + `config.js`).
//!
//! This module ports the pure mapping helpers used by the MITM dispatch:
//! - `get_tool_for_host` — map an upstream host to a MITM tool/provider.
//! - `resolve_router_path` — map an intercepted copilot path to the local
//!   router endpoint.
//!
//! The full per-provider interceptors (antigravity model rewrite + SSE error
//! framing, kiro OpenAI→AWS EventStream conversion, copilot URL remap) build
//! on these helpers; cursor is a not-implemented stub in both JS and here.

/// URL path substrings that mark a request as a chat turn for each tool
/// (9router config.js URL_PATTERNS:26-31).
pub const URL_PATTERNS: &[(&str, &[&str])] = &[
    ("antigravity", &[":generateContent", ":streamGenerateContent"]),
    ("copilot", &["/chat/completions", "/v1/messages", "/responses"]),
    ("kiro", &["/generateAssistantResponse"]),
    ("cursor", &["/BidiAppend", "/RunSSE", "/RunPoll", "/Run"]),
];

/// Upstream host → tool mapping (9router config.js getToolForHost).
/// Hosts are matched case-insensitively by substring.
const TOOL_HOSTS: &[(&str, &str)] = &[
    ("api.individual.githubcopilot.com", "copilot"),
    ("daily-cloudcode-pa.googleapis.com", "antigravity"),
    ("cloudcode-pa.googleapis.com", "antigravity"),
    ("q.us-east-1.amazonaws.com", "kiro"),
    ("codewhisperer.runtime.us-east-1.kiro.dev", "kiro"),
    ("api2.cursor.sh", "cursor"),
];

/// Map an intercepted upstream host to its MITM tool/provider. Returns `None`
/// for hosts not handled by the MITM proxy.
pub fn get_tool_for_host(host: &str) -> Option<&'static str> {
    let host = host.trim().to_ascii_lowercase();
    // Strip an optional port suffix for host matching.
    let host = host.split(':').next().unwrap_or(&host);
    TOOL_HOSTS
        .iter()
        .find(|(h, _)| host.ends_with(*h))
        .map(|(_, tool)| *tool)
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
        assert_eq!(resolve_router_path("/chat/completions"), "/v1/chat/completions");
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
        assert_eq!(
            get_tool_for_host("q.us-east-1.amazonaws.com"),
            Some("kiro")
        );
        assert_eq!(get_tool_for_host("api2.cursor.sh"), Some("cursor"));
        assert_eq!(get_tool_for_host("example.com"), None);
    }

    #[test]
    fn mitm_get_tool_for_host_matches_with_port() {
        assert_eq!(
            get_tool_for_host("api2.cursor.sh:443"),
            Some("cursor")
        );
    }

    #[test]
    fn mitm_get_tool_for_host_is_case_insensitive() {
        assert_eq!(
            get_tool_for_host("API.INDIVIDUAL.GITHUBCOPILOT.COM"),
            Some("copilot")
        );
    }
}
