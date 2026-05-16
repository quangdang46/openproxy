//! Port of `open-sse/config/appConstants.js`. Provider-specific User-Agent
//! values, IDE/platform enums (used by Antigravity Cloud Code), OAuth
//! endpoints, and the static system-prompt strings + default-tool-name
//! decoy sets for Claude Code and Antigravity.

use once_cell::sync::Lazy;
use serde_json::json;
use std::collections::BTreeSet;
use std::time::Duration;

// ─── Gemini CLI ────────────────────────────────────────────────────────────

pub const GEMINI_CLI_VERSION: &str = "0.31.0";
pub const GEMINI_CLI_API_CLIENT: &str = "google-genai-sdk/1.41.0 gl-node/v22.19.0";

/// Build the User-Agent string Gemini CLI advertises for `model`.
pub fn gemini_cli_user_agent(model: &str) -> String {
    let model = if model.is_empty() { "unknown" } else { model };
    let os = match std::env::consts::OS {
        "windows" => "windows",
        other => other,
    };
    format!(
        "GeminiCLI/{}/{} ({}; {})",
        GEMINI_CLI_VERSION,
        model,
        os,
        std::env::consts::ARCH
    )
}

// ─── GitHub Copilot ────────────────────────────────────────────────────────

pub mod github_copilot {
    pub const VSCODE_VERSION: &str = "1.110.0";
    pub const COPILOT_CHAT_VERSION: &str = "0.38.0";
    pub const USER_AGENT: &str = "GitHubCopilotChat/0.38.0";
    pub const API_VERSION: &str = "2025-04-01";
}

// ─── Antigravity Cloud Code enums ─────────────────────────────────────────

#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IdeType {
    Unspecified = 0,
    Jetski = 10,
    Antigravity = 9,
    Plugins = 7,
}

#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AgPlatform {
    Unspecified = 0,
    DarwinAmd64 = 1,
    DarwinArm64 = 2,
    LinuxAmd64 = 3,
    LinuxArm64 = 4,
    WindowsAmd64 = 5,
}

#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AgPluginType {
    Unspecified = 0,
    CloudCode = 1,
    Gemini = 2,
}

/// Best-effort detection of the current host's `Platform` enum value.
/// Returns [`AgPlatform::Unspecified`] on unknown OS/arch combinations.
pub fn current_platform() -> AgPlatform {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => AgPlatform::DarwinArm64,
        ("macos", _) => AgPlatform::DarwinAmd64,
        ("linux", "aarch64") => AgPlatform::LinuxArm64,
        ("linux", _) => AgPlatform::LinuxAmd64,
        ("windows", _) => AgPlatform::WindowsAmd64,
        _ => AgPlatform::Unspecified,
    }
}

/// Antigravity advertised User-Agent for chat / stream requests.
pub fn ag_chat_user_agent() -> String {
    format!(
        "antigravity/1.107.0 {}/{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// Antigravity advertised User-Agent for the platform handshake.
pub fn ag_platform_user_agent() -> String {
    format!(
        "antigravity/1.104.0 {}/{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

// ─── Anti-loop / cloaking ────────────────────────────────────────────────

/// Internal anti-loop header: tagged on every outbound request so an
/// openproxy fronted by another openproxy can short-circuit the second hop.
pub const INTERNAL_REQUEST_HEADER_NAME: &str = "x-request-source";
pub const INTERNAL_REQUEST_HEADER_VALUE: &str = "local";

/// Suffix appended to client tools when forwarding to Antigravity (anti-ban).
pub const AG_TOOL_SUFFIX: &str = "_ide";
/// Suffix appended to client tools when forwarding to Claude (anti-ban).
pub const CLAUDE_TOOL_SUFFIX: &str = "_ide";

/// Claude Code's own tool names. Requests carrying these names bypass the
/// `_ide` suffix to keep the upstream tool registry intact.
pub static CC_DEFAULT_TOOLS: Lazy<BTreeSet<&'static str>> = Lazy::new(|| {
    [
        "Task",
        "TaskOutput",
        "TaskStop",
        "TaskCreate",
        "TaskGet",
        "TaskUpdate",
        "TaskList",
        "Bash",
        "Glob",
        "Grep",
        "Read",
        "Edit",
        "Write",
        "NotebookEdit",
        "WebFetch",
        "WebSearch",
        "AskUserQuestion",
        "Skill",
        "EnterPlanMode",
        "ExitPlanMode",
    ]
    .into_iter()
    .collect()
});

/// Antigravity's own tool names — used as decoys with neutral
/// description/properties.
pub static AG_DEFAULT_TOOLS: Lazy<BTreeSet<&'static str>> = Lazy::new(|| {
    [
        "browser_subagent",
        "command_status",
        "find_by_name",
        "generate_image",
        "grep_search",
        "list_dir",
        "list_resources",
        "multi_replace_file_content",
        "notify_user",
        "read_resource",
        "read_terminal",
        "read_url_content",
        "replace_file_content",
        "run_command",
        "search_web",
        "send_command_input",
        "task_boundary",
        "view_content_chunk",
        "view_file",
        "write_to_file",
    ]
    .into_iter()
    .collect()
});

// ─── Cloud Code Assist API ────────────────────────────────────────────────

pub mod cloud_code_api {
    pub const LOAD_CODE_ASSIST: &str =
        "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
    pub const ONBOARD_USER: &str = "https://cloudcode-pa.googleapis.com/v1internal:onboardUser";
}

/// Build the headers Cloud Code Assist expects on `loadCodeAssist`.
pub fn load_code_assist_headers() -> serde_json::Value {
    let metadata = json!({
        "ideType": IdeType::Antigravity as u8,
        "platform": current_platform() as u8,
        "pluginType": AgPluginType::Gemini as u8,
    });
    json!({
        "Content-Type": "application/json",
        "User-Agent": "google-api-nodejs-client/9.15.1",
        "X-Goog-Api-Client": "google-cloud-sdk vscode_cloudshelleditor/0.1",
        "Client-Metadata": serde_json::to_string(&metadata).unwrap_or_default(),
    })
}

/// Same metadata Cloud Code Assist reads from the headers, but as a JSON
/// object the caller can embed in a request body.
pub fn load_code_assist_metadata() -> serde_json::Value {
    json!({
        "ideType": IdeType::Antigravity as u8,
        "platform": current_platform() as u8,
        "pluginType": AgPluginType::Gemini as u8,
    })
}

// ─── System prompts ──────────────────────────────────────────────────────

pub const CLAUDE_SYSTEM_PROMPT: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";

pub const ANTIGRAVITY_DEFAULT_SYSTEM: &str = "You are Antigravity, a powerful agentic AI coding assistant designed by the Google Deepmind team working on Advanced Agentic Coding.You are pair programming with a USER to solve their coding task. The task may require creating a new codebase, modifying or debugging an existing codebase, or simply answering a question.**Absolute paths only****Proactiveness**";

// ─── Token refresh lead times (proactive renewal) ────────────────────────

/// Returns how far in advance of expiry we should proactively refresh
/// the OAuth token for a given provider id. Returns `None` if the
/// provider has no proactive refresh policy.
pub fn refresh_lead(provider: &str) -> Option<Duration> {
    match provider {
        "codex" => Some(Duration::from_secs(5 * 24 * 60 * 60)),
        "claude" => Some(Duration::from_secs(4 * 60 * 60)),
        "iflow" => Some(Duration::from_secs(24 * 60 * 60)),
        "qwen" => Some(Duration::from_secs(20 * 60)),
        "kimi-coding" => Some(Duration::from_secs(5 * 60)),
        "antigravity" => Some(Duration::from_secs(5 * 60)),
        _ => None,
    }
}

// ─── OAuth endpoints ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct OAuthEndpoint {
    pub token: &'static str,
    pub auth: &'static str,
    pub device_code: Option<&'static str>,
}

/// Lookup canonical OAuth endpoints for a known provider.
pub fn oauth_endpoint(provider: &str) -> Option<OAuthEndpoint> {
    Some(match provider {
        "google" => OAuthEndpoint {
            token: "https://oauth2.googleapis.com/token",
            auth: "https://accounts.google.com/o/oauth2/auth",
            device_code: None,
        },
        "openai" => OAuthEndpoint {
            token: "https://auth.openai.com/oauth/token",
            auth: "https://auth.openai.com/oauth/authorize",
            device_code: None,
        },
        "anthropic" => OAuthEndpoint {
            token: "https://api.anthropic.com/v1/oauth/token",
            auth: "https://api.anthropic.com/v1/oauth/authorize",
            device_code: None,
        },
        "qwen" => OAuthEndpoint {
            token: "https://qwen.ai/api/v1/oauth2/token",
            auth: "https://qwen.ai/api/v1/oauth2/device/code",
            device_code: None,
        },
        "iflow" => OAuthEndpoint {
            token: "https://iflow.cn/oauth/token",
            auth: "https://iflow.cn/oauth",
            device_code: None,
        },
        "github" => OAuthEndpoint {
            token: "https://github.com/login/oauth/access_token",
            auth: "https://github.com/login/oauth/authorize",
            device_code: Some("https://github.com/login/device/code"),
        },
        _ => return None,
    })
}

// ─── Kimi OAuth headers ──────────────────────────────────────────────────

/// Build the custom Kimi OAuth headers identifying this client to the
/// Kimi server. Mirrors `buildKimiHeaders()` in 9router.
pub fn build_kimi_headers() -> serde_json::Value {
    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    json!({
        "X-Msh-Platform": "openproxy",
        "X-Msh-Version": "2.1.2",
        "X-Msh-Device-Model": format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        "X-Msh-Device-Id": format!("kimi-{timestamp_ms}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cc_default_tools_includes_bash() {
        assert!(CC_DEFAULT_TOOLS.contains("Bash"));
        assert!(CC_DEFAULT_TOOLS.contains("Read"));
    }

    #[test]
    fn refresh_lead_returns_known_providers() {
        assert!(refresh_lead("codex").is_some());
        assert!(refresh_lead("nonexistent").is_none());
    }

    #[test]
    fn oauth_endpoint_github_has_device_code() {
        let gh = oauth_endpoint("github").unwrap();
        assert!(gh.device_code.is_some());
        assert!(oauth_endpoint("google").unwrap().device_code.is_none());
    }

    #[test]
    fn user_agent_contains_version_and_model() {
        let ua = gemini_cli_user_agent("gemini-3-pro");
        assert!(ua.contains("GeminiCLI/0.31.0"));
        assert!(ua.contains("gemini-3-pro"));
    }
}
