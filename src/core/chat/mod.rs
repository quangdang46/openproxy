//! Shared chat core — extracted from src/server/api/chat.rs
//!
//! This module provides the shared orchestration for chat request handling.
//! Route handlers in src/server/api/ become thin wrappers around this core.
//!
//! The pipeline is:
//!   1. Detect source format (from endpoint path + body)
//!   2. Resolve model (provider, model, alias, combo)
//!   3. Select credentials (with account fallback)
//!   4. Run guardrails (pre_call — injection scan, PII masking)
//!   5. Translate request (source -> OpenAI intermediate -> target)
//!   6. Apply preprocessing (RTK, caveman)
//!   7. Dispatch to executor
//!   8. Run guardrails (post_call — PII masking on response)
//!   9. Translate response (target -> OpenAI intermediate -> source)
//!   10. Stream or return JSON

use serde_json::Value;

use crate::core::guardrails::global_guardrail_registry;
use crate::core::model::catalog::provider_catalog;
use crate::core::rtk::system_inject::inject_system_prompt;
use crate::core::translator::caveman::inject_caveman;
use crate::core::translator::ponytail::{inject_ponytail_prompt, PonytailLevel};
use crate::core::translator::registry::{self, Format};
use crate::types::Settings;

/// Multi-endpoint transport entry (9router `PROVIDERS[p].transports[]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportMatch {
    pub format: Format,
    pub base_url: String,
}

/// Result of planning a request before dispatch.
///
/// Mirrors 9router chatCore resolution order:
/// `modelTargetFormat || resolveTransport(provider, sourceFormat)?.format || getTargetFormat(provider)`
#[derive(Debug, Clone)]
pub struct RequestPlan {
    /// The provider name (e.g. "openai", "claude", "cursor")
    pub provider: String,
    /// The resolved model name (alias / client-facing id)
    pub model: String,
    /// Upstream model id sent to the provider (catalog `upstreamModelId` or same as model)
    pub upstream_model_id: String,
    /// Source format detected from the request
    pub source_format: Format,
    /// Target format for the provider
    pub target_format: Format,
    /// Content-type strip list from catalog (`image`, `audio`, …)
    pub strip_list: Vec<String>,
    /// Optional multi-endpoint base URL when transport matched source format
    pub transport_base_url: Option<String>,
    /// Whether this is a streaming request (upstream)
    pub stream: bool,
    /// Whether this is a passthrough (client tool matches provider ecosystem)
    pub passthrough: bool,
    /// Whether bypass applies (warmup, skip, cc naming)
    pub bypass: bool,
    /// Provider forceStream + client non-stream → aggregate SSE to JSON
    pub sse_to_json: bool,
}

impl RequestPlan {
    /// Create a request plan from the request body and resolved provider/model.
    pub fn new(endpoint_path: Option<&str>, body: &Value, provider: &str, model: &str) -> Self {
        let source_format = if let Some(path) = endpoint_path {
            // Body-aware endpoint detection (Cursor CLI chat/completions + input[])
            registry::detect_source_format_by_endpoint_with_body(path, Some(body))
                .unwrap_or_else(|| registry::detect_source_format(body))
        } else {
            registry::detect_source_format(body)
        };

        let (model_target, upstream_model_id, strip_list) =
            resolve_model_metadata(provider, model);

        // 9router: modelTargetFormat || resolveTransport?.format || getTargetFormat
        let transport = resolve_transport(provider, source_format);
        let target_format = model_target
            .or_else(|| transport.as_ref().map(|t| t.format))
            .unwrap_or_else(|| registry::get_target_format_for_provider(provider));

        let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(true);

        Self {
            provider: provider.to_string(),
            model: model.to_string(),
            upstream_model_id,
            source_format,
            target_format,
            strip_list,
            transport_base_url: transport.map(|t| t.base_url),
            stream,
            passthrough: false,
            bypass: false,
            sse_to_json: false,
        }
    }

    /// Model id to send upstream (after catalog remapping).
    pub fn dispatch_model(&self) -> &str {
        &self.upstream_model_id
    }

    /// Returns true if request needs translation (source != target).
    pub fn needs_translation(&self) -> bool {
        self.source_format != self.target_format && !self.passthrough
    }
}

/// Catalog + custom-model fields: targetFormat, upstreamModelId, strip.
fn resolve_model_metadata(
    provider: &str,
    model: &str,
) -> (Option<Format>, String, Vec<String>) {
    let catalog = provider_catalog();
    if let Some(entry) = catalog.find_model(provider, model) {
        let target = entry
            .target_format
            .as_deref()
            .and_then(Format::from_str);
        let upstream = entry
            .upstream_model_id
            .clone()
            .unwrap_or_else(|| model.to_string());
        let strip = entry
            .strip
            .as_deref()
            .map(parse_strip_list)
            .unwrap_or_default();
        return (target, upstream, strip);
    }
    (None, model.to_string(), Vec::new())
}

fn parse_strip_list(raw: &str) -> Vec<String> {
    raw.split(|c: char| c == ',' || c == '|' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect()
}

/// Multi-endpoint providers (9router `transports[]`): pick entry matching client sourceFormat.
/// When matched, translation can be skipped (source == transport format) and base URL overrides.
pub fn resolve_transport(provider: &str, source_format: Format) -> Option<TransportMatch> {
    let entries = provider_transports(provider);
    entries
        .into_iter()
        .find(|t| t.format == source_format)
}

/// Static multi-transport table ported from 9router registry (deepseek, kimi, glm, …).
fn provider_transports(provider: &str) -> Vec<TransportMatch> {
    match provider {
        "deepseek" | "ds" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://api.deepseek.com/chat/completions".into(),
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://api.deepseek.com/anthropic/v1/messages".into(),
            },
        ],
        "kimi" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://api.moonshot.cn/v1/chat/completions".into(),
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://api.moonshot.cn/anthropic/v1/messages".into(),
            },
        ],
        "kimi-coding" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://api.kimi.com/coding/v1/chat/completions".into(),
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://api.kimi.com/coding/v1/messages".into(),
            },
        ],
        "glm" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://open.bigmodel.cn/api/paas/v4/chat/completions".into(),
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://open.bigmodel.cn/api/anthropic/v1/messages".into(),
            },
        ],
        "minimax" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://api.minimax.chat/v1/text/chatcompletion_v2".into(),
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://api.minimax.chat/anthropic/v1/messages".into(),
            },
        ],
        "minimax-cn" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://api.minimaxi.com/v1/text/chatcompletion_v2".into(),
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://api.minimaxi.com/anthropic/v1/messages".into(),
            },
        ],
        _ => Vec::new(),
    }
}

/// Plan a request (detect formats, catalog metadata, multi-endpoint transport).
pub fn plan_request(
    endpoint_path: Option<&str>,
    body: &Value,
    provider: &str,
    model: &str,
) -> RequestPlan {
    RequestPlan::new(endpoint_path, body, provider, model)
}

/// Run guardrail pre_call hooks on the request body.
///
/// This should be called **before** translation so that PII masking and
/// injection detection see the original (un-translated) request.
///
/// Returns `true` if the request was modified by any guardrail.
pub async fn apply_guardrails_pre_call(body: &mut Value) -> bool {
    let registry = global_guardrail_registry();
    match registry.run_pre_call(body).await {
        Ok(()) => false,
        Err(errors) => {
            for e in &errors {
                tracing::warn!(target: "openproxy::guardrails", "pre_call guardrail: {e}");
            }
            // Guardrails that return errors (like injection detection) do not
            // block the request in this release — they only log a warning.
            // Set `GUARDRAIL_BLOCK_ON_INJECTION` or a future settings toggle
            // to make them blocking.
            true
        }
    }
}

/// Run guardrail post_call hooks on the response body.
///
/// This should be called **after** the upstream response is received but
/// **before** response translation, so PII masking can clean the provider's
/// raw output.
pub async fn apply_guardrails_post_call(response: &mut Value) -> bool {
    let registry = global_guardrail_registry();
    match registry.run_post_call(response).await {
        Ok(()) => false,
        Err(errors) => {
            for e in &errors {
                tracing::warn!(target: "openproxy::guardrails", "post_call guardrail: {e}");
            }
            true
        }
    }
}

/// Apply preprocessing steps (caveman prompt injection, system prompt injection)
/// to the request body.
///
/// This should be called after translation but before dispatch, corresponding
/// to step 5 in the pipeline: "Apply preprocessing (RTK, caveman)".
///
/// Returns `true` if any modification was made.
pub fn apply_preprocessing(
    body: &mut Value,
    settings: &Settings,
    source_format: &Format,
    plan: &RequestPlan,
) -> bool {
    let mut modified = false;
    if settings.caveman_enabled {
        modified |= inject_caveman(body, source_format, &settings.caveman_level);
    }
    if settings.ponytail_enabled {
        // Ponytail always applies if enabled (no context-pressure gate).
        modified |= inject_ponytail_prompt(
            body,
            PonytailLevel::parse_or_default(&settings.ponytail_level),
        );
    }
    // System prompt injection at RTK layer.
    // Reads `systemInject` (bool) and `systemPrompt` (string) from the settings
    // `extra` map.
    modified |= apply_chat_system_prompt_injection(body, settings);
    modified
}

/// Check the RTK-layer system injection settings and apply if enabled.
/// Reads `systemInject` (bool) and `systemPrompt` (string) from the settings
/// `extra` map.
fn apply_chat_system_prompt_injection(body: &mut Value, settings: &Settings) -> bool {
    let system_inject = settings
        .extra
        .get("systemInject")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !system_inject {
        return false;
    }
    let prompt = settings
        .extra
        .get("systemPrompt")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string());
    match prompt {
        Some(p) => inject_system_prompt(body, &p),
        None => false,
    }
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
        assert_eq!(
            registry::detect_source_format(&body),
            Format::OpenAiResponses
        );
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
        assert_eq!(
            registry::get_target_format_for_provider("openai"),
            Format::OpenAi
        );
        assert_eq!(
            registry::get_target_format_for_provider("claude"),
            Format::Claude
        );
        assert_eq!(
            registry::get_target_format_for_provider("gemini"),
            Format::Gemini
        );
        assert_eq!(
            registry::get_target_format_for_provider("cursor"),
            Format::Cursor
        );
        assert_eq!(
            registry::get_target_format_for_provider("kiro"),
            Format::Kiro
        );
        assert_eq!(
            registry::get_target_format_for_provider("codex"),
            Format::OpenAiResponses
        );
        assert_eq!(
            registry::get_target_format_for_provider("ollama"),
            Format::Ollama
        );
        assert_eq!(
            registry::get_target_format_for_provider("deepseek"),
            Format::OpenAi
        );
    }

    #[test]
    fn test_request_plan_needs_translation() {
        let body = json!({"model": "gpt-4", "messages": [], "stream": true});
        let plan = RequestPlan::new(Some("/v1/chat/completions"), &body, "openai", "gpt-4");
        // OpenAI body to OpenAI provider — no translation needed
        assert!(!plan.needs_translation());
        assert_eq!(plan.dispatch_model(), "gpt-4");

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
    fn deepseek_claude_source_selects_claude_transport() {
        let body = json!({
            "model": "deepseek-chat",
            "system": [{"type": "text", "text": "sys"}],
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 64
        });
        let plan = RequestPlan::new(Some("/v1/messages"), &body, "deepseek", "deepseek-chat");
        assert_eq!(plan.source_format, Format::Claude);
        assert_eq!(plan.target_format, Format::Claude);
        assert!(!plan.needs_translation());
        assert!(
            plan.transport_base_url
                .as_deref()
                .is_some_and(|u| u.contains("anthropic")),
            "expected deepseek anthropic transport, got {:?}",
            plan.transport_base_url
        );
    }

    #[test]
    fn anthropic_compatible_provider_targets_claude() {
        let body = json!({"model": "x", "messages": [{"role": "user", "content": "hi"}]});
        let plan = RequestPlan::new(
            Some("/v1/chat/completions"),
            &body,
            "anthropic-compatible-acme",
            "claude-3",
        );
        assert_eq!(plan.target_format, Format::Claude);
        assert!(plan.needs_translation());
    }

    #[test]
    fn resolve_transport_none_for_single_endpoint() {
        assert!(resolve_transport("openai", Format::OpenAi).is_none());
        assert!(resolve_transport("cursor", Format::Cursor).is_none());
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
        crate::core::translator::helpers::tool_call_helper::ensure_tool_call_ids(&mut body);
        // Should have added an id
        let tc = &body["messages"][0]["tool_calls"][0];
        assert!(tc.get("id").is_some());
        assert!(tc["id"].as_str().unwrap().contains("read_file"));
    }
}
