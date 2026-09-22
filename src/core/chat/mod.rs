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
    /// Thinking level stripped from `model(level)` / `model-level` (re-applied post-translate).
    pub thinking_level: Option<String>,
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

        let (model_target, mut upstream_model_id, strip_list) =
            resolve_model_metadata(provider, model);

        // Global model(level) / model-level strip (9router thinkingUnified.stripThinkingSuffix)
        // Keep level for post-translate re-apply (applyThinking parity).
        let (stripped, thinking_level) =
            crate::core::utils::thinking_suffix::strip_thinking_suffix_owned(&upstream_model_id);
        if stripped != upstream_model_id {
            upstream_model_id = stripped;
        }

        // 9router chatCore `useTransport` guard (modelTargetFormat is checked
        // first in Rust, transport second): only use the sourceFormat-matched
        // transport when the model declares support for that sourceFormat —
        // opencode-go models differ in endpoint support (kimi/glm only do
        // /chat/completions). Undeclared models keep the upstream default
        // (use the transport).
        let transport = resolve_transport(provider, source_format)
            .filter(|_| model_supports_source_format(provider, &upstream_model_id, source_format));
        let mut target_format = model_target
            .or_else(|| transport.as_ref().map(|t| t.format))
            .unwrap_or_else(|| registry::get_target_format_for_provider(provider));

        // GitHub Copilot Claude models use the Anthropic-native /v1/messages
        // shim (9router v0.5.35). Force Claude as target so chat translates
        // OpenAI→Claude before dispatch; response path then Claude→client.
        // Name-pattern check (not registry targetFormat) so live catalog
        // claude-* variants are covered without static list lag.
        if provider == "github"
            && crate::core::executor::GithubExecutor::is_claude_model(&upstream_model_id)
        {
            target_format = Format::Claude;
        }

        let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(true);

        Self {
            provider: provider.to_string(),
            model: model.to_string(),
            upstream_model_id,
            thinking_level,
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
fn resolve_model_metadata(provider: &str, model: &str) -> (Option<Format>, String, Vec<String>) {
    let catalog = provider_catalog();
    if let Some(entry) = catalog.find_model(provider, model) {
        let target = entry.target_format.as_deref().and_then(Format::from_str);
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
    // 9router acb5c34c (`getModelTargetFormat` + `isMuseSparkModel`): all
    // Muse Spark models on opencode-family providers route to
    // /zen/v1/responses, even ones the static catalog hasn't registered yet
    // (e.g. future 1.4/2.0 versions — the JS test pins exactly this).
    // Scoped to opencode providers only (incl. short aliases oc/ocg);
    // other providers keep Chat Completions routing.
    if matches!(provider, "opencode" | "opencode-go" | "oc" | "ocg") && is_muse_spark_model(model) {
        return (Some(Format::OpenAiResponses), model.to_string(), Vec::new());
    }
    // 9router opencode-go.js registry: grok-4.6 + gpt-5.6-luna are
    // responses-only (`targetFormat: openai-responses`,
    // `supportedFormats: [openai-responses]`) — route them to
    // /zen/go/v1/responses like Muse Spark. Scoped to opencode-go only
    // (registry entry lives on that provider); opencode/oc keep Chat.
    if matches!(provider, "opencode-go" | "ocg") && is_opencode_go_responses_only_model(model) {
        return (Some(Format::OpenAiResponses), model.to_string(), Vec::new());
    }
    (None, model.to_string(), Vec::new())
}

/// Whether a model id is a Muse Spark model (served by /zen/v1/responses).
/// 9router `isMuseSparkModel` (helpers.js): strip a trailing thinking suffix
/// `model(level)`, take the vendor-prefix base, match `muse[-_]?spark`
/// followed by end-of-string or a `-_:.\s` separator (case-insensitive).
fn is_muse_spark_model(model_id: &str) -> bool {
    // Strip trailing thinking suffix "model(level)".
    let mut clean = model_id.trim();
    if let Some(open) = clean.rfind('(') {
        if clean.ends_with(')') && !clean[open + 1..clean.len() - 1].contains(['(', ')']) {
            clean = clean[..open].trim_end();
        }
    }
    let base = clean.rsplit('/').next().unwrap_or(clean);
    let lower = base.to_lowercase();
    // Start-anchored like the JS `/^muse…/`: a mid-string "muse" (e.g.
    // `amuse-spark-x`) must not match.
    let Some(after_muse) = lower.strip_prefix("muse") else {
        return false;
    };
    // Optional single `-`/`_` separator, then literal "spark".
    let after_sep = after_muse.strip_prefix(['-', '_']).unwrap_or(after_muse);
    let Some(after_spark) = after_sep.strip_prefix("spark") else {
        return false;
    };
    // Followed by end-of-string or one of `-_:.\s`.
    after_spark.is_empty() || after_spark.starts_with(['-', '_', ':', '.', ' ', '\t'])
}

/// Responses-only models on opencode-go (9router registry
/// `open-sse/providers/registry/opencode-go.js`): grok-4.6 + gpt-5.6-luna
/// carry `targetFormat: openai-responses` with no Chat transport.
fn is_opencode_go_responses_only_model(model_id: &str) -> bool {
    let mut clean = model_id.trim();
    if let Some(open) = clean.rfind('(') {
        if clean.ends_with(')') && !clean[open + 1..clean.len() - 1].contains(['(', ')']) {
            clean = clean[..open].trim_end();
        }
    }
    let base = clean.rsplit('/').next().unwrap_or(clean);
    let lower = base.to_lowercase();
    lower == "grok-4.6" || lower == "gpt-5.6-luna"
}

/// Per-model endpoint support on opencode-go (9router registry opencode-go.js
/// `supportedFormats`, following https://opencode.ai/docs/go/). `None` means
/// the model is not in the static table — keep the upstream default of using
/// the transport (JS: `!modelSupportedFormats` → use transport).
fn opencode_go_supported_formats(model_id: &str) -> Option<&'static [Format]> {
    let mut clean = model_id.trim();
    if let Some(open) = clean.rfind('(') {
        if clean.ends_with(')') && !clean[open + 1..clean.len() - 1].contains(['(', ')']) {
            clean = clean[..open].trim_end();
        }
    }
    let base = clean.rsplit('/').next().unwrap_or(clean);
    let lower = base.to_lowercase();
    // Strip a trailing "-free" subscription-tier suffix (catalog ids carry it).
    let core = lower.strip_suffix("-free").unwrap_or(&lower);
    // Responses-only + Muse Spark: /zen/go/v1/responses only.
    if core == "grok-4.6" || core == "gpt-5.6-luna" || is_muse_spark_model(model_id) {
        return Some(&[Format::OpenAiResponses]);
    }
    // Full 3-leg models: openai + claude + openai-responses.
    if matches!(
        core,
        "deepseek-v4-pro" | "deepseek-v4-flash" | "deepseek-v4-flash-vision-exp"
    ) {
        return Some(&[Format::OpenAi, Format::Claude, Format::OpenAiResponses]);
    }
    // Dual-leg models: openai + claude.
    if matches!(
        core,
        "minimax-m3"
            | "minimax-m2.7"
            | "minimax-m2.5"
            | "qwen3.8-max"
            | "qwen3.8-flash"
            | "qwen3.7-max"
            | "qwen3.7-plus"
            | "qwen3.6-plus"
    ) {
        return Some(&[Format::OpenAi, Format::Claude]);
    }
    // Single-leg models: openai (/chat/completions) only.
    if matches!(
        core,
        "deepseek-flash"
            | "glm-5.3-flash"
            | "glm-5.3"
            | "glm-5.2"
            | "glm-5.1"
            | "kimi-k2.7-code"
            | "kimi-k2.6"
            | "kimi-k3"
            | "longcat-2.0"
            | "mimo-v2.5"
            | "mimo-v2.5-pro"
            | "hy4-preview"
            | "hy3"
    ) {
        return Some(&[Format::OpenAi]);
    }
    None
}

/// 9router chatCore `useTransport` guard: when a model declares
/// supportedFormats, only use the sourceFormat-matched transport if that
/// format is declared. Scoped to opencode-go/ocg (the only provider whose
/// registry models declare supportedFormats); every other provider — and
/// undeclared opencode-go models — keeps the upstream default (true).
fn model_supports_source_format(provider: &str, model: &str, source: Format) -> bool {
    if !matches!(provider, "opencode-go" | "ocg") {
        return true;
    }
    match opencode_go_supported_formats(model) {
        Some(formats) => formats.contains(&source),
        None => true,
    }
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
    entries.into_iter().find(|t| t.format == source_format)
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
                // Coding plan OpenAI leg (9r registry); moonshot.cn is web-search only.
                base_url: "https://api.kimi.com/coding/v1/chat/completions".into(),
            },
            TransportMatch {
                format: Format::Claude,
                // beta baked in: runtime_transport already_endpoint would otherwise drop urlSuffix.
                base_url: "https://api.kimi.com/coding/v1/messages?beta=true".into(),
            },
        ],
        "kimi-coding" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://api.kimi.com/coding/v1/chat/completions".into(),
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://api.kimi.com/coding/v1/messages?beta=true".into(),
            },
        ],
        "glm" => vec![
            TransportMatch {
                format: Format::OpenAi,
                // GLM Coding plan (api.z.ai); open.bigmodel.cn is glm-cn.
                base_url: "https://api.z.ai/api/coding/paas/v4/chat/completions".into(),
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://api.z.ai/api/anthropic/v1/messages?beta=true".into(),
            },
        ],
        "minimax" => vec![
            TransportMatch {
                format: Format::OpenAi,
                // Modern OpenAI chat; chatcompletion_v2 is legacy/search-only in 9r.
                base_url: "https://api.minimax.io/v1/chat/completions".into(),
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://api.minimax.io/anthropic/v1/messages?beta=true".into(),
            },
        ],
        "minimax-cn" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://api.minimaxi.com/v1/chat/completions".into(),
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://api.minimaxi.com/anthropic/v1/messages?beta=true".into(),
            },
        ],
        "xiaomi-mimo" | "mimo" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://api.xiaomimimo.com/v1/chat/completions".into(),
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://api.xiaomimimo.com/anthropic/v1/messages".into(),
            },
        ],
        // Region-specific bases are applied in DefaultExecutor; here we only
        // signal format preference so plan.target_format / runtime_transport path work.
        // Full regional URL is rebuilt in default.rs using PSD.region.
        "xiaomi-tokenplan" | "xmtp" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://token-plan-sgp.xiaomimimo.com/v1/chat/completions".into(),
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://token-plan-sgp.xiaomimimo.com/anthropic/v1/messages".into(),
            },
        ],
        // Alibaba Token Plan — Singapore-only, OpenAI-compatible only.
        // Anthropic surface is NOT authorized for this plan.
        "alitp-intl" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1/chat/completions".into(),
            },
        ],
        // opencode-go 3-leg transports (9router registry opencode-go.js
        // `transports[]`): openai → /zen/go/v1/chat/completions,
        // claude → /zen/go/v1/messages, openai-responses → /zen/go/v1/responses.
        // Guarded per-model by supportedFormats in RequestPlan::new.
        "opencode-go" | "ocg" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://opencode.ai/zen/go/v1/chat/completions".into(),
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://opencode.ai/zen/go/v1/messages".into(),
            },
            TransportMatch {
                format: Format::OpenAiResponses,
                base_url: "https://opencode.ai/zen/go/v1/responses".into(),
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
        Some(p) => {
            // Dispatch by body shape (9router injectSystemPrompt format switch).
            let format = if body.get("system").is_some() {
                "claude"
            } else if body.get("systemInstruction").is_some()
                || body.get("system_instruction").is_some()
            {
                "gemini"
            } else {
                "openai"
            };
            inject_system_prompt(body, format, &p)
        }
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
    fn xiaomi_mimo_claude_transport() {
        let t = resolve_transport("xiaomi-mimo", Format::Claude).expect("claude transport");
        assert!(t.base_url.contains("anthropic/v1/messages"));
        let t = resolve_transport("mimo", Format::OpenAi).expect("openai transport");
        assert!(t.base_url.contains("chat/completions"));
    }

    #[test]
    fn multi_endpoint_transports_match_9r_registry_hosts() {
        // kimi / kimi-coding — coding plan hosts (not moonshot.cn)
        let t = resolve_transport("kimi", Format::OpenAi).expect("kimi openai");
        assert_eq!(
            t.base_url,
            "https://api.kimi.com/coding/v1/chat/completions"
        );
        let t = resolve_transport("kimi", Format::Claude).expect("kimi claude");
        assert_eq!(
            t.base_url,
            "https://api.kimi.com/coding/v1/messages?beta=true"
        );
        let t = resolve_transport("kimi-coding", Format::Claude).expect("kimi-coding claude");
        assert_eq!(
            t.base_url,
            "https://api.kimi.com/coding/v1/messages?beta=true"
        );

        // glm coding plan (api.z.ai), not open.bigmodel.cn paas
        let t = resolve_transport("glm", Format::OpenAi).expect("glm openai");
        assert_eq!(
            t.base_url,
            "https://api.z.ai/api/coding/paas/v4/chat/completions"
        );
        let t = resolve_transport("glm", Format::Claude).expect("glm claude");
        assert_eq!(
            t.base_url,
            "https://api.z.ai/api/anthropic/v1/messages?beta=true"
        );

        // minimax modern hosts/paths (not chatcompletion_v2 / minimax.chat)
        let t = resolve_transport("minimax", Format::OpenAi).expect("minimax openai");
        assert_eq!(t.base_url, "https://api.minimax.io/v1/chat/completions");
        let t = resolve_transport("minimax", Format::Claude).expect("minimax claude");
        assert_eq!(
            t.base_url,
            "https://api.minimax.io/anthropic/v1/messages?beta=true"
        );
        let t = resolve_transport("minimax-cn", Format::OpenAi).expect("minimax-cn openai");
        assert_eq!(t.base_url, "https://api.minimaxi.com/v1/chat/completions");
        let t = resolve_transport("minimax-cn", Format::Claude).expect("minimax-cn claude");
        assert_eq!(
            t.base_url,
            "https://api.minimaxi.com/anthropic/v1/messages?beta=true"
        );
    }

    #[test]
    fn xiaomi_tokenplan_dual_transport() {
        let t = resolve_transport("xiaomi-tokenplan", Format::Claude).expect("claude");
        assert!(t.base_url.contains("anthropic"));
        let t = resolve_transport("xmtp", Format::OpenAi).expect("openai");
        assert!(t.base_url.contains("chat/completions"));
    }

    #[test]
    fn alitp_intl_openai_only_transport() {
        // Alibaba Token Plan supports OpenAI-compatible only, no Claude surface.
        let t = resolve_transport("alitp-intl", Format::OpenAi).expect("alitp-intl openai");
        assert_eq!(
            t.base_url,
            "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1/chat/completions"
        );
        // Claude transport should NOT exist for alitp-intl.
        assert!(resolve_transport("alitp-intl", Format::Claude).is_none());
    }

    #[test]
    fn plan_strips_model_thinking_paren_suffix() {
        let body =
            json!({"model": "gpt-4o(high)", "messages": [{"role": "user", "content": "hi"}]});
        let plan = RequestPlan::new(
            Some("/v1/chat/completions"),
            &body,
            "openai",
            "gpt-4o(high)",
        );
        assert_eq!(plan.dispatch_model(), "gpt-4o");
        // Level kept for post-translate re-apply (thinking-suffix-reapply)
        assert_eq!(plan.thinking_level.as_deref(), Some("high"));
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
    // 9router acb5c34c (isMuseSparkModel + executor-const-guard test):
    // routing matrix incl thinking suffixes and future versions.
    #[test]
    fn muse_spark_models_match_responses_routing() {
        for id in [
            "muse-spark-1.2-contributor-free",
            "muse-spark-1.3-contributor-free",
            "muse-spark-1.4-contributor-free",
            "muse-spark-2.0-contributor-free(xhigh)",
            "muse-spark-1.2-contributor-free(xhigh)",
            "MUSE-SPARK-1.5",
            "muse_spark-1.2",
            "prefix/muse-spark-1.2",
        ] {
            assert!(is_muse_spark_model(id), "{id} should match");
        }
        for id in [
            "big-pickle",
            "hy3-free",
            "",
            "muse",
            "spark",
            "musical-sparkler",
            // Start-anchored: mid-string "muse" must not match (JS /^muse/).
            "amuse-spark-x",
            "prefix-amuse-spark-1.2",
        ] {
            assert!(!is_muse_spark_model(id), "{id} should not match");
        }
    }

    #[test]
    fn muse_spark_resolves_responses_target_on_opencode_only() {
        use crate::core::translator::registry::Format;
        // Registered catalog entries (targetFormat openai-responses).
        let (t, _, _) = resolve_model_metadata("opencode", "muse-spark-1.2-contributor-free");
        assert_eq!(t, Some(Format::OpenAiResponses));
        // Unregistered future version → isMuseSparkModel fallback, opencode
        // family only (incl. short aliases oc/ocg).
        let (t, _, _) = resolve_model_metadata("opencode", "muse-spark-1.4-contributor-free");
        assert_eq!(t, Some(Format::OpenAiResponses));
        let (t, _, _) = resolve_model_metadata("opencode-go", "muse-spark-9.9-foo");
        assert_eq!(t, Some(Format::OpenAiResponses));
        let (t, _, _) = resolve_model_metadata("oc", "muse-spark-9.9-foo");
        assert_eq!(t, Some(Format::OpenAiResponses));
        let (t, _, _) = resolve_model_metadata("ocg", "muse-spark-9.9-foo");
        assert_eq!(t, Some(Format::OpenAiResponses));
        // Other providers keep Chat Completions routing.
        let (t, _, _) = resolve_model_metadata("openai", "muse-spark-1.4-contributor-free");
        assert_eq!(t, None);
        // Non-spark models unaffected.
        let (t, _, _) = resolve_model_metadata("opencode", "big-pickle");
        assert_eq!(t, None);
    }

    #[test]
    fn opencode_go_responses_only_models_route_to_responses() {
        use crate::core::translator::registry::Format;
        for id in [
            "grok-4.6",
            "GROK-4.6",
            "gpt-5.6-luna",
            "ocg/grok-4.6",
            "grok-4.6(high)",
        ] {
            assert!(is_opencode_go_responses_only_model(id), "{id} should match");
            let (t, _, _) = resolve_model_metadata("opencode-go", id);
            assert_eq!(t, Some(Format::OpenAiResponses), "{id} on opencode-go");
            let (t, _, _) = resolve_model_metadata("ocg", id);
            assert_eq!(t, Some(Format::OpenAiResponses), "{id} on ocg");
        }
        // Scoped to opencode-go: opencode/oc keep Chat routing.
        let (t, _, _) = resolve_model_metadata("opencode", "grok-4.6");
        assert_eq!(t, None);
        let (t, _, _) = resolve_model_metadata("oc", "gpt-5.6-luna");
        assert_eq!(t, None);
        // Non-listed models unaffected.
        assert!(!is_opencode_go_responses_only_model("grok-4.5"));
        assert!(!is_opencode_go_responses_only_model("gpt-5.6-terra"));
        assert!(!is_opencode_go_responses_only_model("big-pickle"));
    }

    #[test]
    fn opencode_go_three_leg_transports() {
        // 9router registry opencode-go.js `transports[]`.
        let t = resolve_transport("opencode-go", Format::OpenAi).expect("openai leg");
        assert_eq!(t.base_url, "https://opencode.ai/zen/go/v1/chat/completions");
        let t = resolve_transport("opencode-go", Format::Claude).expect("claude leg");
        assert_eq!(t.base_url, "https://opencode.ai/zen/go/v1/messages");
        let t = resolve_transport("opencode-go", Format::OpenAiResponses).expect("responses leg");
        assert_eq!(t.base_url, "https://opencode.ai/zen/go/v1/responses");
        // Short alias ocg shares the table.
        let t = resolve_transport("ocg", Format::Claude).expect("ocg claude leg");
        assert_eq!(t.base_url, "https://opencode.ai/zen/go/v1/messages");
        // opencode/oc are single-endpoint — no transports.
        assert!(resolve_transport("opencode", Format::OpenAi).is_none());
        assert!(resolve_transport("oc", Format::Claude).is_none());
    }

    #[test]
    fn opencode_go_supported_formats_guard() {
        // Single-leg kimi: claude-format request must not use the claude transport.
        let body = json!({
            "model": "kimi-k2.6",
            "system": [{"type": "text", "text": "sys"}],
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 64
        });
        let plan = RequestPlan::new(Some("/v1/messages"), &body, "opencode-go", "kimi-k2.6");
        assert_eq!(plan.source_format, Format::Claude);
        assert!(plan.transport_base_url.is_none());
        assert!(plan.needs_translation());

        // Same request on the openai leg keeps the transport (lossless).
        let body = json!({"model": "kimi-k2.6", "messages": [{"role": "user", "content": "hi"}]});
        let plan = RequestPlan::new(
            Some("/v1/chat/completions"),
            &body,
            "opencode-go",
            "kimi-k2.6",
        );
        assert_eq!(plan.target_format, Format::OpenAi);
        assert_eq!(
            plan.transport_base_url.as_deref(),
            Some("https://opencode.ai/zen/go/v1/chat/completions")
        );
        assert!(!plan.needs_translation());
    }

    #[test]
    fn opencode_go_dual_and_full_leg_models() {
        // Dual-leg minimax-m3: claude transport applies, responses does not.
        assert!(model_supports_source_format(
            "opencode-go",
            "minimax-m3",
            Format::Claude
        ));
        assert!(!model_supports_source_format(
            "opencode-go",
            "minimax-m3",
            Format::OpenAiResponses
        ));
        // Full 3-leg deepseek-v4-pro supports all legs.
        for fmt in [Format::OpenAi, Format::Claude, Format::OpenAiResponses] {
            assert!(model_supports_source_format(
                "opencode-go",
                "deepseek-v4-pro",
                fmt
            ));
        }
        // Responses-only models accept only the responses leg.
        assert!(model_supports_source_format(
            "opencode-go",
            "grok-4.6",
            Format::OpenAiResponses
        ));
        assert!(!model_supports_source_format(
            "opencode-go",
            "grok-4.6",
            Format::OpenAi
        ));
        // Case, vendor prefix, -free tier suffix, and thinking suffix tolerated.
        assert!(model_supports_source_format(
            "opencode-go",
            "OCG/Kimi-K2.6-free(high)",
            Format::OpenAi
        ));
        // Undeclared models keep the upstream default (use the transport).
        assert!(model_supports_source_format(
            "opencode-go",
            "some-future-model",
            Format::Claude
        ));
        // Guard scoped to opencode-go/ocg: other providers always use transport.
        assert!(model_supports_source_format(
            "deepseek",
            "deepseek-chat",
            Format::Claude
        ));
        assert!(model_supports_source_format(
            "opencode",
            "kimi-k2.6",
            Format::Claude
        ));
    }
}
