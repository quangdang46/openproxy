//! Shared chat core — extracted from src/server/api/chat.rs
//!
//! This module provides the shared orchestration for chat request handling.
//! Route handlers in src/server/api/ become thin wrappers around this core.
//!
//! The pipeline is:
//!   1. Detect source format (from endpoint path + body)
//!   2. Resolve model (provider, model, alias, combo)
//!   3. Select credentials (with account fallback)
//!   4. Translate request (source -> OpenAI intermediate -> target)
//!   5. Apply preprocessing (RTK, caveman)
//!   6. Dispatch to executor
//!   7. Translate response (target -> OpenAI intermediate -> source)
//!   8. Stream or return JSON

use serde_json::Value;

use crate::core::translator::registry::{self, Format};

/// Result of planning a request before dispatch.
#[derive(Debug, Clone)]
pub struct RequestPlan {
    /// The provider name (e.g. "openai", "claude", "cursor")
    pub provider: String,
    /// The resolved model name
    pub model: String,
    /// Source format detected from the request
    pub source_format: Format,
    /// Target format for the provider
    pub target_format: Format,
    /// Whether this is a streaming request
    pub stream: bool,
    /// Whether this is a passthrough (client tool matches provider ecosystem)
    pub passthrough: bool,
    /// Whether bypass applies (warmup, skip, cc naming)
    pub bypass: bool,
}

impl RequestPlan {
    /// Create a request plan from the request body and resolved provider/model.
    pub fn new(
        endpoint_path: Option<&str>,
        body: &Value,
        provider: &str,
        model: &str,
    ) -> Self {
        let source_format = if let Some(path) = endpoint_path {
            registry::detect_source_format_by_endpoint(path)
                .unwrap_or_else(|| registry::detect_source_format(body))
        } else {
            registry::detect_source_format(body)
        };

        let target_format = registry::get_target_format_for_provider(provider);

        let stream = body
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        Self {
            provider: provider.to_string(),
            model: model.to_string(),
            source_format,
            target_format,
            stream,
            passthrough: false,
            bypass: false,
        }
    }

    /// Returns true if request needs translation (source != target).
    pub fn needs_translation(&self) -> bool {
        self.source_format != self.target_format && !self.passthrough
    }
}

/// Placeholder for the chat-core dispatch function.
/// Will be expanded as the Phase 2 bead progresses.
pub fn plan_request(
    endpoint_path: Option<&str>,
    body: &Value,
    provider: &str,
    model: &str,
) -> RequestPlan {
    let mut plan = RequestPlan::new(endpoint_path, body, provider, model);

    // TODO: detect passthrough (client tool matches provider ecosystem)
    // TODO: detect bypass patterns

    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_detect_source_format_openai() {
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        });
        assert_eq!(registry::detect_source_format(&body), Format::OpenAi);
    }

    #[test]
    fn test_detect_source_format_responses() {
        let body = json!({
            "model": "gpt-4",
            "input": "hello",
            "stream": true
        });
        assert_eq!(registry::detect_source_format(&body), Format::OpenAiResponses);
    }

    #[test]
    fn test_detect_source_format_claude() {
        // Claude-specific indicators: system array at body level
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "system": [{"type": "text", "text": "You are Claude."}],
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 1024
        });
        assert_eq!(registry::detect_source_format(&body), Format::Claude);
    }

    #[test]
    fn test_detect_source_format_gemini() {
        let body = json!({
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}]
        });
        assert_eq!(registry::detect_source_format(&body), Format::Gemini);
    }

    #[test]
    fn test_detect_source_format_by_endpoint() {
        assert_eq!(
            registry::detect_source_format_by_endpoint("/v1/messages"),
            Some(Format::Claude)
        );
        assert_eq!(
            registry::detect_source_format_by_endpoint("/v1/responses"),
            Some(Format::OpenAiResponses)
        );
        assert_eq!(
            registry::detect_source_format_by_endpoint("/v1/responses/compact"),
            Some(Format::OpenAiResponses)
        );
        assert_eq!(
            registry::detect_source_format_by_endpoint("/v1/chat/completions"),
            None
        );
    }

    #[test]
    fn test_get_target_format_for_provider() {
        assert_eq!(registry::get_target_format_for_provider("openai"), Format::OpenAi);
        assert_eq!(registry::get_target_format_for_provider("claude"), Format::Claude);
        assert_eq!(registry::get_target_format_for_provider("gemini"), Format::Gemini);
        assert_eq!(registry::get_target_format_for_provider("cursor"), Format::Cursor);
        assert_eq!(registry::get_target_format_for_provider("kiro"), Format::Kiro);
        assert_eq!(registry::get_target_format_for_provider("codex"), Format::OpenAiResponses);
        assert_eq!(registry::get_target_format_for_provider("ollama"), Format::Ollama);
        assert_eq!(registry::get_target_format_for_provider("deepseek"), Format::OpenAi);
    }

    #[test]
    fn test_request_plan_needs_translation() {
        let body = json!({"model": "gpt-4", "messages": [], "stream": true});
        let plan = RequestPlan::new(
            Some("/v1/chat/completions"),
            &body,
            "openai",
            "gpt-4",
        );
        // OpenAI body to OpenAI provider — no translation needed
        assert!(!plan.needs_translation());

        let plan = RequestPlan::new(
            Some("/v1/chat/completions"),
            &body,
            "claude",
            "claude-sonnet-4",
        );
        // OpenAI body to Claude provider — needs translation
        assert!(plan.needs_translation());
    }

    #[test]
    fn test_ensure_tool_call_ids() {
        let mut body = json!({
            "messages": [{
                "role": "assistant",
                "tool_calls": [{
                    "function": {"name": "read_file", "arguments": "{}"}
                }]
            }]
        });
        registry::ensure_tool_call_ids(&mut body);
        // Should have added an id
        let tc = &body["messages"][0]["tool_calls"][0];
        assert!(tc.get("id").is_some());
        assert!(tc["id"].as_str().unwrap().starts_with("call_read_file_"));
    }
}
