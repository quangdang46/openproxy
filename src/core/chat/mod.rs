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
use crate::core::translator::caveman::inject_caveman;
use crate::core::translator::ponytail::{inject_ponytail_prompt, PonytailLevel};
use crate::core::translator::registry::{self, Format};
use crate::core::utils::thinking_suffix::ThinkingNative;
use crate::types::Settings;

/// Multi-endpoint transport entry (9router `PROVIDERS[p].transports[]`).
#[derive(Debug, Clone, PartialEq)]
pub struct TransportMatch {
    pub format: Format,
    pub base_url: String,
    /// Per-transport thinking wire format (9router `transports[].thinkingFormat`,
    /// thinkingUnified.js:130-148). Strict `/responses` legs declare
    /// `openai-responses` so `resolve_thinking_native` writes
    /// `reasoning:{effort,summary}` instead of a stray top-level
    /// `reasoning_effort` those upstreams reject.
    pub thinking_format: Option<ThinkingNative>,
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
            resolve_model_metadata_with_client(provider, model, Some(body));

        // Global model(level) / model-level strip (9router thinkingUnified.stripThinkingSuffix)
        // Keep level for post-translate re-apply (applyThinking parity).
        let (stripped, thinking_level) =
            crate::core::utils::thinking_suffix::strip_thinking_suffix_owned(&upstream_model_id);
        if stripped != upstream_model_id {
            upstream_model_id = stripped;
        }

        // 9router chatCore.js:91-104 `useTransport` guard + modelTargetTransport
        // fallback: when the model declares supportedFormats that exclude the
        // client's sourceFormat, fall back to the transport for the model's
        // declared targetFormat so the URL always matches the translated body
        // (a Responses-only model must not get an OpenAI body POSTed to
        // /chat/completions). When the guard passes, the sourceFormat-matched
        // transport wins even if absent (upstream `runtimeTransport` may be
        // null → provider default URL).
        let transport = if model_supports_source_format(provider, &upstream_model_id, source_format)
        {
            resolve_transport(provider, source_format)
        } else {
            model_target.and_then(|fmt| resolve_transport(provider, fmt))
        };
        // Transport first, then the model target, then the provider default —
        // 9router's order (chatCore.js: `useTransport?.format || modelTargetFormat
        // || getTargetFormat(...)`).
        //
        // A source-format-matched endpoint keeps the request lossless, so it
        // outranks a model-level targetFormat, which is only the fallback for a
        // client whose wire format has no supported transport. Checking the
        // model target first sent a Claude-format request for a multi-endpoint
        // provider to /messages where the provider only serves
        // /chat/completions (kimi, glm) — a transport error rather than a
        // translation.
        let mut target_format = transport
            .as_ref()
            .map(|t| t.format)
            .or(model_target)
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
    resolve_model_metadata_with_client(provider, model, None)
}

/// Split a trailing `"(level)"` thinking suffix off `model`, returning
/// `(base, suffix_with_parens)`. Mirrors the JS
/// `/\([^()]+\)\s*$/` match in `getModelUpstreamId` — any non-empty,
/// paren-free parenthesized tail counts (e.g. `(high)`, `(8192)`).
fn split_paren_suffix(model: &str) -> (&str, &str) {
    let trimmed = model.trim_end();
    if trimmed.ends_with(')') {
        if let Some(open) = trimmed.rfind('(') {
            let inner = &trimmed[open + 1..trimmed.len() - 1];
            if !inner.is_empty() && !inner.contains(['(', ')']) {
                return (trimmed[..open].trim_end(), &trimmed[open..]);
            }
        }
    }
    (trimmed, "")
}

/// 9router providerModels.js `getModelUpstreamId` tier normalization:
/// `xhigh`/`max` collapse to `high`, `minimal` to `low`.
fn normalize_tier(tier: &str) -> String {
    let t = tier.trim().to_lowercase();
    match t.as_str() {
        "xhigh" | "max" => "high".to_string(),
        "minimal" => "low".to_string(),
        _ => t,
    }
}

/// JS truthiness for the `a || b || c` effort chain (null/false/0/"" are falsy).
fn js_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// JS `Number(x)` coercion for `thinking.budget_tokens`.
fn js_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                Some(0.0) // Number("") === 0
            } else {
                t.parse::<f64>().ok()
            }
        }
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Null => Some(0.0),
        _ => None,
    }
}

/// Requested thinking tier from the client request body
/// (9router `getModelUpstreamId` clientBody branch):
/// `reasoning_effort || output_config.effort || reasoning.effort`, else
/// `thinking.budget_tokens` thresholds (>16384 high, <=4096 low, else medium).
fn client_requested_tier(client_body: &Value) -> Option<String> {
    let effort = [
        client_body.get("reasoning_effort"),
        client_body.pointer("/output_config/effort"),
        client_body
            .get("reasoning")
            .filter(|r| r.is_object())
            .and_then(|r| r.get("effort")),
    ]
    .into_iter()
    .flatten()
    .find(|v| js_truthy(v));
    if let Some(v) = effort {
        if let Some(s) = v.as_str() {
            if !s.is_empty() {
                return Some(normalize_tier(s));
            }
        }
        // Truthy non-string effort: JS falls through to the thinking branch.
    }
    let thinking = client_body.get("thinking").filter(|t| t.is_object())?;
    let budget = js_number(thinking.get("budget_tokens")?)?;
    if budget > 0.0 {
        Some(
            if budget > 16384.0 {
                "high"
            } else if budget <= 4096.0 {
                "low"
            } else {
                "medium"
            }
            .to_string(),
        )
    } else {
        None
    }
}

/// `getModelUpstreamId` port (9router v0.5.109, commit 82aeb90): resolve
/// unadorned models to tiered catalog variants (`<base>-<tier>`) from the
/// `(level)` suffix or the client's reasoning effort, then re-apply the
/// thinking suffix the way upstream does (`resolvedBase + suffix`, or the
/// requested tier over the catalog preset suffix).
fn resolve_model_metadata_with_client(
    provider: &str,
    model: &str,
    client_body: Option<&Value>,
) -> (Option<Format>, String, Vec<String>) {
    let catalog = provider_catalog();
    // JS keys PROVIDER_MODELS by alias and accepts id-or-alias; the Rust
    // catalog keys by canonical provider id, so normalize first.
    let provider_id = crate::core::model::resolve_provider_alias(provider);
    let (base_id, suffix) = split_paren_suffix(model);

    // Tier from the "(level)" suffix first; client body only when absent.
    let suffix_inner = suffix
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let tier = if !suffix_inner.is_empty() {
        Some(normalize_tier(&suffix_inner))
    } else {
        client_body.and_then(client_requested_tier)
    };

    // Tiered variant wins over the bare base id (e.g. gemini-3.8-flash-high).
    let found = tier
        .as_deref()
        .and_then(|t| catalog.find_model(&provider_id, &format!("{base_id}-{t}")))
        .or_else(|| catalog.find_model(&provider_id, base_id));

    if let Some(entry) = found {
        // targetFormat/strip come from the base-id lookup upstream
        // (getModelTargetFormat/getModelStrip run on the un-tiered id);
        // tiered variants carry neither, so fall back to the found entry.
        let meta = catalog.find_model(&provider_id, base_id).unwrap_or(entry);
        let target = meta.target_format.as_deref().and_then(Format::from_str);
        let strip = meta
            .strip
            .as_deref()
            .map(parse_strip_list)
            .unwrap_or_default();

        let resolved = entry
            .upstream_model_id
            .clone()
            .unwrap_or_else(|| entry.id.clone());
        let (resolved_base, preset_suffix) = split_paren_suffix(&resolved);
        let upstream = if !suffix.is_empty() {
            format!("{resolved_base}{suffix}")
        } else if !preset_suffix.is_empty() {
            match &tier {
                Some(t) => format!("{resolved_base}({t})"),
                None => resolved.clone(),
            }
        } else {
            resolved_base.to_string()
        };
        return (target, upstream, strip);
    }
    // 9router acb5c34c (`getModelTargetFormat` + `isMuseSparkModel`): all
    // Muse Spark models on opencode-family providers route to
    // /zen/v1/responses, even ones the static catalog hasn't registered yet
    // (e.g. future 1.4/2.0 versions — the JS test pins exactly this).
    // Scoped to opencode providers only (incl. short aliases oc/ocg/ocz);
    // other providers keep Chat Completions routing.
    if matches!(
        provider,
        "opencode" | "opencode-go" | "oc" | "ocg" | "opencode-zen" | "ocz"
    ) && is_muse_spark_model(model)
    {
        return (Some(Format::OpenAiResponses), model.to_string(), Vec::new());
    }
    // 9router `opencodeFamilyFormats` (helpers.js:56): on opencode-zen the
    // grok/gpt family prefix also pins /responses for ids the curated
    // catalog hasn't registered (modelsFetcher / passthrough). Scoped to
    // zen — opencode-go keeps its own narrower table above and `opencode`
    // owns the free lane.
    if matches!(provider, "opencode-zen" | "ocz") {
        let base = model
            .trim()
            .rsplit('/')
            .next()
            .unwrap_or(model.trim())
            .to_lowercase();
        if base.starts_with("grok") || base.starts_with("gpt") {
            return (Some(Format::OpenAiResponses), model.to_string(), Vec::new());
        }
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
/// `open-sse/providers/registry/opencode-go.js`, v0.5.116): grok-4.5/4.6/4.7,
/// gpt-5.6-luna, gpt-6-luna and the muse-spark contributor ids carry
/// `targetFormat: openai-responses` with no Chat transport.
fn is_opencode_go_responses_only_model(model_id: &str) -> bool {
    let mut clean = model_id.trim();
    if let Some(open) = clean.rfind('(') {
        if clean.ends_with(')') && !clean[open + 1..clean.len() - 1].contains(['(', ')']) {
            clean = clean[..open].trim_end();
        }
    }
    let base = clean.rsplit('/').next().unwrap_or(clean);
    let lower = base.to_lowercase();
    matches!(
        lower.as_str(),
        "grok-4.5"
            | "grok-4.6"
            | "grok-4.7"
            | "gpt-5.6-luna"
            | "gpt-6-luna"
            | "muse-spark-1.2-contributor"
            | "muse-spark-1.3-contributor"
    ) || is_muse_spark_model(model_id)
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
    // Responses-only + Muse Spark: /inference/go/openai/v1/responses only.
    // v0.5.116 registry adds grok-4.5/4.7, gpt-6-luna and the muse-spark
    // contributor ids (the latter two families also match below).
    if matches!(
        core,
        "grok-4.5" | "grok-4.6" | "grok-4.7" | "gpt-5.6-luna" | "gpt-6-luna"
    ) || is_muse_spark_model(model_id)
    {
        return Some(&[Format::OpenAiResponses]);
    }
    // Claude-only: claude-haiku-5-5 is served by /messages exclusively
    // ("/chat/completions answers ModelProtocolUnsupported").
    if core == "claude-haiku-5-5" || core.starts_with("claude-") {
        return Some(&[Format::Claude]);
    }
    // Full 3-leg models: openai + claude + openai-responses.
    if matches!(
        core,
        "deepseek-v4-pro"
            | "deepseek-v4-flash"
            | "deepseek-v4-flash-vision-exp"
            | "deepseek-v4.1-flash"
    ) {
        return Some(&[Format::OpenAi, Format::Claude, Format::OpenAiResponses]);
    }
    // Dual-leg models: openai + claude (v0.5.116 adds space-bunny-free and
    // qwen3.5-plus).
    if matches!(
        core,
        "minimax-m3"
            | "minimax-m2.7"
            | "minimax-m2.5"
            | "space-bunny-free"
            | "qwen3.8-max"
            | "qwen3.8-flash"
            | "qwen3.7-max"
            | "qwen3.7-plus"
            | "qwen3.6-plus"
            | "qwen3.5-plus"
    ) {
        return Some(&[Format::OpenAi, Format::Claude]);
    }
    // Unknown ids fall back to the upstream family regex
    // (`opencodeFamilyFormats`, helpers.js): deepseek-v4-(pro|flash) keep
    // the triple, minimax/qwen keep [openai, claude], grok/gpt/muse-spark
    // were handled above, everything else is openai-only.
    if core.starts_with("deepseek-v4-pro") || core.starts_with("deepseek-v4-flash") {
        return Some(&[Format::OpenAi, Format::Claude, Format::OpenAiResponses]);
    }
    if core.starts_with("minimax") || core.starts_with("qwen") {
        return Some(&[Format::OpenAi, Format::Claude]);
    }
    if core.starts_with("grok") || core.starts_with("gpt") {
        return Some(&[Format::OpenAiResponses]);
    }
    // Single-leg models: openai (/chat/completions) only — the upstream
    // `[FORMATS.OPENAI]` default for undeclared ids.
    Some(&[Format::OpenAi])
}

/// Per-model endpoint support on opencode-zen (9router registry
/// opencode-zen.js `supportedFormats` + helpers.js `OPENCODE_FAMILIES`
/// fallback for modelsFetcher/passthrough ids). Registry entries win over
/// the family regex; unknown ids default to `[openai]` like upstream
/// `getModelSupportedFormats`.
fn opencode_zen_supported_formats(model: &str) -> &'static [Format] {
    use Format::*;
    let mut clean = model.trim();
    if let Some(open) = clean.rfind('(') {
        if clean.ends_with(')') && !clean[open + 1..clean.len() - 1].contains(['(', ')']) {
            clean = clean[..open].trim_end();
        }
    }
    let base = clean.rsplit('/').next().unwrap_or(clean).to_lowercase();

    // Registry-pinned Claude-only ids (supportedFormats: ["claude"]).
    // `claude-*` also matches the family regex; qwen/union-alpha are
    // registry overrides of the broader minimax|qwen family.
    if base.starts_with("claude-")
        || matches!(
            base.as_str(),
            "qwen3.5-plus" | "qwen3.6-plus" | "union-alpha"
        )
    {
        return &[Claude];
    }
    // Responses-only: muse-spark + the gpt/grok family (registry declares
    // supportedFormats:["openai-responses"] for every gpt-*/grok-* entry).
    if is_muse_spark_model(&base) || base.starts_with("gpt") || base.starts_with("grok") {
        return &[OpenAiResponses];
    }
    // deepseek-v4 family: registry pins the four curated ids to ["openai"];
    // unknown deepseek-v4-(pro|flash) ids keep the family triple.
    if matches!(
        base.as_str(),
        "deepseek-v4-pro"
            | "deepseek-v4-flash"
            | "deepseek-v4-flash-vision-exp"
            | "deepseek-v4-flash-free"
    ) {
        return &[OpenAi];
    }
    if base.starts_with("deepseek-v4-pro") || base.starts_with("deepseek-v4-flash") {
        return &[OpenAi, Claude, OpenAiResponses];
    }
    // minimax/qwen family declares [openai, claude]; the three curated
    // minimax registry entries are ["openai"]-only.
    if matches!(
        base.as_str(),
        "minimax-m3" | "minimax-m2.7" | "minimax-m2.5"
    ) {
        return &[OpenAi];
    }
    if base.starts_with("minimax") || base.starts_with("qwen") {
        return &[OpenAi, Claude];
    }
    // Everything else (gemini, glm, kimi, big-pickle, *-free, jev, …)
    // rides /chat/completions — the upstream `[FORMATS.OPENAI]` default.
    &[OpenAi]
}

/// 9router chatCore `useTransport` guard: when a model declares
/// supportedFormats, only use the sourceFormat-matched transport if that
/// format is declared. Scoped to opencode-go/ocg and opencode-zen/ocz (the
/// providers whose registry models declare supportedFormats); every other
/// provider — and undeclared opencode-go models — keeps the upstream
/// default (true).
fn model_supports_source_format(provider: &str, model: &str, source: Format) -> bool {
    if matches!(provider, "opencode-go" | "ocg") {
        return match opencode_go_supported_formats(model) {
            Some(formats) => formats.contains(&source),
            None => true,
        };
    }
    if matches!(provider, "opencode-zen" | "ocz") {
        return opencode_zen_supported_formats(model).contains(&source);
    }
    // Muse registry models declare `supportedFormats: ["openai-responses"]`
    // (9router registry muse.js) — the chat/completions transport must not
    // win for them even though the provider exposes one.
    if matches!(
        provider,
        "muse" | "muse-ai" | "meta-model-api" | "muse-code" | "muse-subscription"
    ) && is_muse_spark_model(model)
    {
        return source == Format::OpenAiResponses;
    }
    true
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
/// `pub(crate)` so `resolve_thinking_native` can honour `transports[].thinkingFormat`.
pub(crate) fn provider_transports(provider: &str) -> Vec<TransportMatch> {
    match provider {
        "deepseek" | "ds" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://api.deepseek.com/chat/completions".into(),
                thinking_format: None,
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://api.deepseek.com/anthropic/v1/messages".into(),
                thinking_format: None,
            },
        ],
        "kimi" => vec![
            TransportMatch {
                format: Format::OpenAi,
                // Coding plan OpenAI leg (9r registry); moonshot.cn is web-search only.
                base_url: "https://api.kimi.com/coding/v1/chat/completions".into(),
                thinking_format: None,
            },
            TransportMatch {
                format: Format::Claude,
                // beta baked in: runtime_transport already_endpoint would otherwise drop urlSuffix.
                base_url: "https://api.kimi.com/coding/v1/messages?beta=true".into(),
                thinking_format: None,
            },
            // Kimi Code natively serves the OpenAI Responses API
            // (9r kimi.js transports[2]; Codex wire_api = "responses").
            TransportMatch {
                format: Format::OpenAiResponses,
                base_url: "https://api.kimi.com/coding/v1/responses".into(),
                thinking_format: None,
            },
        ],
        "kimi-coding" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://api.kimi.com/coding/v1/chat/completions".into(),
                thinking_format: None,
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://api.kimi.com/coding/v1/messages?beta=true".into(),
                thinking_format: None,
            },
            TransportMatch {
                format: Format::OpenAiResponses,
                base_url: "https://api.kimi.com/coding/v1/responses".into(),
                thinking_format: None,
            },
        ],
        "glm" => vec![
            TransportMatch {
                format: Format::OpenAi,
                // GLM Coding plan (api.z.ai); open.bigmodel.cn is glm-cn.
                base_url: "https://api.z.ai/api/coding/paas/v4/chat/completions".into(),
                thinking_format: None,
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://api.z.ai/api/anthropic/v1/messages?beta=true".into(),
                thinking_format: None,
            },
        ],
        "minimax" => vec![
            TransportMatch {
                format: Format::OpenAi,
                // Modern OpenAI chat; chatcompletion_v2 is legacy/search-only in 9r.
                base_url: "https://api.minimax.io/v1/chat/completions".into(),
                thinking_format: None,
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://api.minimax.io/anthropic/v1/messages?beta=true".into(),
                thinking_format: None,
            },
        ],
        "minimax-cn" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://api.minimaxi.com/v1/chat/completions".into(),
                thinking_format: None,
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://api.minimaxi.com/anthropic/v1/messages?beta=true".into(),
                thinking_format: None,
            },
        ],
        // 9router registry/minimax-code{,-global}.js transport: Claude format on
        // the mavis agent gateway (single transport, no OpenAI leg upstream).
        "minimax-code" => vec![TransportMatch {
            format: Format::Claude,
            base_url: "https://agent.minimax.cn/mavis/api/v1/llm/v1/messages".into(),
            thinking_format: None,
        }],
        "minimax-code-global" => vec![TransportMatch {
            format: Format::Claude,
            base_url: "https://agent.minimax.io/mavis/api/v1/llm/v1/messages".into(),
            thinking_format: None,
        }],
        "xiaomi-mimo" | "mimo" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://api.xiaomimimo.com/v1/chat/completions".into(),
                thinking_format: None,
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://api.xiaomimimo.com/anthropic/v1/messages".into(),
                thinking_format: None,
            },
        ],
        // Region-specific bases are applied in DefaultExecutor; here we only
        // signal format preference so plan.target_format / runtime_transport path work.
        // Full regional URL is rebuilt in default.rs using PSD.region.
        "xiaomi-tokenplan" | "xmtp" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://token-plan-sgp.xiaomimimo.com/v1/chat/completions".into(),
                thinking_format: None,
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://token-plan-sgp.xiaomimimo.com/anthropic/v1/messages".into(),
                thinking_format: None,
            },
        ],
        // Alibaba Token Plan — Singapore-only, OpenAI-compatible only.
        // Anthropic surface is NOT authorized for this plan.
        "alitp-intl" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1/chat/completions".into(),
                thinking_format: None,
            },
        ],
        // opencode-go 3-leg transports (9router registry opencode-go.js
        // `transports[]`): openai → /zen/go/v1/chat/completions,
        // claude → /zen/go/v1/messages, openai-responses → /zen/go/v1/responses.
        // Guarded per-model by supportedFormats in RequestPlan::new.
        "opencode-go" | "ocg" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://opencode.ai/inference/go/openai/v1/chat/completions".into(),
                thinking_format: None,
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://opencode.ai/inference/go/anthropic/v1/messages".into(),
                // 9router 633c82f5: the /messages leg uses adaptive thinking
                // and output_config.effort for DeepSeek — its
                // reasoning_effort extension requires a native Anthropic
                // target (registry transports[1] thinkingFormat).
                thinking_format: Some(ThinkingNative::ClaudeAdaptive),
            },
            // /responses rejects the Chat-shaped thinking fields
            // (reasoning_effort, thinking:{type}) — upstream declares
            // `thinkingFormat: "openai-responses"` on this leg
            // (9router registry opencode-go.js transports[2]).
            TransportMatch {
                format: Format::OpenAiResponses,
                base_url: "https://opencode.ai/inference/go/openai/v1/responses".into(),
                thinking_format: Some(ThinkingNative::OpenAiResponses),
            },
        ],
        // opencode-zen 3-leg transports (9router registry opencode-zen.js
        // `transports[]`): openai → /zen/v1/chat/completions,
        // claude → /zen/v1/messages (x-api-key + anthropic-version),
        // openai-responses → /zen/v1/responses. The registry declares no
        // thinkingFormat on the responses leg.
        "opencode-zen" | "ocz" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://opencode.ai/zen/v1/chat/completions".into(),
                thinking_format: None,
            },
            TransportMatch {
                format: Format::Claude,
                base_url: "https://opencode.ai/zen/v1/messages".into(),
                thinking_format: None,
            },
            TransportMatch {
                format: Format::OpenAiResponses,
                base_url: "https://opencode.ai/zen/v1/responses".into(),
                thinking_format: None,
            },
        ],
        // Muse (Meta Model API) dual transport (9router registry muse.js):
        // chat/completions + strict /responses whose thinkingFormat is
        // "openai-responses" (rejects top-level reasoning_effort).
        "muse" | "muse-ai" | "meta-model-api" | "muse-code" | "muse-subscription" => vec![
            TransportMatch {
                format: Format::OpenAi,
                base_url: "https://api.meta.ai/v1/chat/completions".into(),
                thinking_format: None,
            },
            TransportMatch {
                format: Format::OpenAiResponses,
                base_url: "https://api.meta.ai/v1/responses".into(),
                thinking_format: Some(ThinkingNative::OpenAiResponses),
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
    // Custom system prompt injection — delegated to the shared RTK-layer
    // dispatch so this path and `apply_request_preprocessing` behave
    // identically (Kiro / Claude / Gemini-request / OpenAI shape switch,
    // `customPromptEnabled`/`customPrompt` keys with the legacy
    // `extra["systemInject"]`/`extra["systemPrompt"]` fallback).
    modified |= crate::core::rtk::apply_system_prompt_injection(body, settings);
    modified
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
        // Kimi Code natively serves the OpenAI Responses API
        // (9r kimi.js transports[2] — Codex wire_api = "responses").
        let t = resolve_transport("kimi", Format::OpenAiResponses).expect("kimi responses");
        assert_eq!(t.base_url, "https://api.kimi.com/coding/v1/responses");
        let t = resolve_transport("kimi-coding", Format::OpenAiResponses)
            .expect("kimi-coding responses");
        assert_eq!(t.base_url, "https://api.kimi.com/coding/v1/responses");

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
    fn kimi_coding_responses_request_uses_responses_transport() {
        // 9router kimi.js transports[2]: Responses-wire clients (Codex
        // wire_api = "responses") reach https://api.kimi.com/coding/v1/responses.
        let body = json!({
            "model": "k3",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}]
        });
        let plan = RequestPlan::new(Some("/v1/responses"), &body, "kimi-coding", "k3");
        assert_eq!(plan.source_format, Format::OpenAiResponses);
        assert_eq!(plan.target_format, Format::OpenAiResponses);
        assert_eq!(
            plan.transport_base_url.as_deref(),
            Some("https://api.kimi.com/coding/v1/responses")
        );
        assert!(!plan.needs_translation());
        // The merged `kimi` provider shares the same transport table.
        let plan = RequestPlan::new(Some("/v1/responses"), &body, "kimi", "k3");
        assert_eq!(
            plan.transport_base_url.as_deref(),
            Some("https://api.kimi.com/coding/v1/responses")
        );
    }

    #[test]
    fn opencode_go_three_leg_transports() {
        // 9router registry opencode-go.js `transports[]`.
        let t = resolve_transport("opencode-go", Format::OpenAi).expect("openai leg");
        assert_eq!(
            t.base_url,
            "https://opencode.ai/inference/go/openai/v1/chat/completions"
        );
        let t = resolve_transport("opencode-go", Format::Claude).expect("claude leg");
        assert_eq!(
            t.base_url,
            "https://opencode.ai/inference/go/anthropic/v1/messages"
        );
        let t = resolve_transport("opencode-go", Format::OpenAiResponses).expect("responses leg");
        assert_eq!(
            t.base_url,
            "https://opencode.ai/inference/go/openai/v1/responses"
        );
        // Short alias ocg shares the table.
        let t = resolve_transport("ocg", Format::Claude).expect("ocg claude leg");
        assert_eq!(
            t.base_url,
            "https://opencode.ai/inference/go/anthropic/v1/messages"
        );
        // opencode/oc are single-endpoint — no transports.
        assert!(resolve_transport("opencode", Format::OpenAi).is_none());
        assert!(resolve_transport("oc", Format::Claude).is_none());
    }

    #[test]
    fn responses_legs_declare_thinking_format() {
        // openproxy-4d5h.23: upstream transports[].thinkingFormat.
        let t = resolve_transport("opencode-go", Format::OpenAiResponses).unwrap();
        assert_eq!(t.thinking_format, Some(ThinkingNative::OpenAiResponses));
        let t = resolve_transport("ocg", Format::OpenAiResponses).unwrap();
        assert_eq!(t.thinking_format, Some(ThinkingNative::OpenAiResponses));
        // Chat/claude legs declare none.
        assert_eq!(
            resolve_transport("opencode-go", Format::OpenAi)
                .unwrap()
                .thinking_format,
            None
        );
        // muse dual transport (9router registry muse.js).
        let t = resolve_transport("muse", Format::OpenAiResponses).unwrap();
        assert_eq!(t.base_url, "https://api.meta.ai/v1/responses");
        assert_eq!(t.thinking_format, Some(ThinkingNative::OpenAiResponses));
        let t = resolve_transport("muse", Format::OpenAi).unwrap();
        assert_eq!(t.base_url, "https://api.meta.ai/v1/chat/completions");
        assert_eq!(t.thinking_format, None);
    }

    #[test]
    fn muse_spark_models_skip_chat_transport() {
        // Muse Spark models are responses-only: an OpenAI-wire client must NOT
        // match the chat/completions transport (upstream supportedFormats guard).
        let body = json!({
            "model": "muse-spark-1.3",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let plan = RequestPlan::new(None, &body, "muse", "muse-spark-1.3");
        assert!(plan.transport_base_url.is_none());
        assert_eq!(plan.target_format, Format::OpenAiResponses);

        // A Responses-wire client DOES match the responses transport.
        let body = json!({
            "model": "muse-spark-1.3",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}]
        });
        let plan = RequestPlan::new(Some("/v1/responses"), &body, "muse", "muse-spark-1.3");
        assert_eq!(plan.source_format, Format::OpenAiResponses);
        assert_eq!(plan.target_format, Format::OpenAiResponses);
        assert_eq!(
            plan.transport_base_url.as_deref(),
            Some("https://api.meta.ai/v1/responses")
        );
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
            Some("https://opencode.ai/inference/go/openai/v1/chat/completions")
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

#[cfg(test)]
mod target_format_precedence {
    use super::*;
    use serde_json::json;

    /// openproxy-e43o: 9router resolves `useTransport?.format ||
    /// modelTargetFormat || getTargetFormat(...)` — transport FIRST, because a
    /// source-format-matched endpoint keeps the request lossless. We checked
    /// the model target first, so a Claude-format request for a
    /// multi-endpoint provider went to /messages where kimi and glm only serve
    /// /chat/completions: a transport error where 9router translated nothing.
    ///
    /// The plan-level test needs a live endpoint, so this pins the two halves
    /// that are reachable here: the transport is still gated on the model
    /// declaring support for the source format, and the fallback order holds
    /// when there is no transport at all.
    #[test]
    fn transport_gate_is_a_passthrough_outside_the_multi_transport_namespace() {
        // model_supports_source_format returns true for every provider that is
        // not opencode-go/ocg, so only that namespace can suppress a
        // transport. This is the property the precedence change depends on:
        // flipping the order must not change routing for anyone else.
        for (provider, model) in [
            ("kimi", "kimi-k2-turbo"),
            ("glm", "glm-5"),
            ("deepseek", "deepseek-chat"),
            ("openai", "gpt-4o"),
        ] {
            assert!(
                model_supports_source_format(provider, model, Format::Claude),
                "{provider} must keep its transport"
            );
        }
    }

    #[test]
    fn provider_default_is_the_last_resort() {
        // With no transport and no model target, the provider default decides.
        assert_eq!(
            registry::get_target_format_for_provider("anthropic"),
            Format::Claude
        );
        assert_eq!(
            registry::get_target_format_for_provider("gemini"),
            Format::Gemini
        );
    }

    // ── opencode-zen (9router registry/opencode-zen.js + executors/
    // opencode-zen.js, bead openproxy-9router-parity-v5104-sync-eidj.51) ──

    #[test]
    fn opencode_zen_three_legs_match_registry_urls() {
        let t = resolve_transport("opencode-zen", Format::OpenAi).expect("zen openai");
        assert_eq!(t.base_url, "https://opencode.ai/zen/v1/chat/completions");
        let t = resolve_transport("opencode-zen", Format::Claude).expect("zen claude");
        assert_eq!(t.base_url, "https://opencode.ai/zen/v1/messages");
        let t = resolve_transport("opencode-zen", Format::OpenAiResponses).expect("zen responses");
        assert_eq!(t.base_url, "https://opencode.ai/zen/v1/responses");
        // Short alias resolves the same table.
        assert!(resolve_transport("ocz", Format::Claude).is_some());
    }

    #[test]
    fn opencode_zen_supported_formats_follow_registry_and_family() {
        use Format::*;
        // Registry claude-only ids.
        for m in ["claude-opus-5", "qwen3.6-plus", "union-alpha"] {
            assert_eq!(
                opencode_zen_supported_formats(m),
                &[Claude],
                "{m} must be claude-only"
            );
        }
        // Responses-only gpt/grok/muse family.
        for m in ["gpt-5.5", "grok-4.6", "muse-spark-1.3"] {
            assert_eq!(
                opencode_zen_supported_formats(m),
                &[OpenAiResponses],
                "{m} must be responses-only"
            );
        }
        // Registry deepseek-v4 pins to openai; unknown variants keep family.
        assert_eq!(opencode_zen_supported_formats("deepseek-v4-pro"), &[OpenAi]);
        assert_eq!(
            opencode_zen_supported_formats("deepseek-v4-flash-free"),
            &[OpenAi]
        );
        assert_eq!(
            opencode_zen_supported_formats("deepseek-v4-flash-2027"),
            &[OpenAi, Claude, OpenAiResponses]
        );
        // Curated minimax is openai-only; other minimax/qwen keep the family pair.
        assert_eq!(opencode_zen_supported_formats("minimax-m3"), &[OpenAi]);
        assert_eq!(
            opencode_zen_supported_formats("minimax-m9"),
            &[OpenAi, Claude]
        );
        // Default lane.
        for m in [
            "big-pickle",
            "glm-5.3",
            "kimi-k2.6",
            "mimo-v2.6-flash-free",
            "gemini-3.5-flash",
        ] {
            assert_eq!(
                opencode_zen_supported_formats(m),
                &[OpenAi],
                "{m} must default to openai"
            );
        }
    }

    #[test]
    fn opencode_zen_transport_guard_blocks_unsupported_source_format() {
        // A Claude-source request for a responses-only model must NOT take
        // the /messages transport; it falls back to the model's responses
        // target transport.
        assert!(!model_supports_source_format(
            "opencode-zen",
            "gpt-5.5",
            Format::Claude
        ));
        assert!(model_supports_source_format(
            "opencode-zen",
            "claude-opus-5",
            Format::Claude
        ));
        assert!(model_supports_source_format(
            "opencode-zen",
            "big-pickle",
            Format::OpenAi
        ));
        // Alias behaves identically.
        assert!(!model_supports_source_format(
            "ocz",
            "gpt-5.5",
            Format::OpenAi
        ));
    }

    #[test]
    fn opencode_zen_model_target_routes_gpt_family_to_responses() {
        use crate::core::translator::registry::Format;
        // Catalog-registered gpt model carries targetFormat openai-responses.
        let (t, _, _) = resolve_model_metadata("opencode-zen", "gpt-5.5");
        assert_eq!(t, Some(Format::OpenAiResponses));
        // Unregistered passthrough id → family fallback.
        let (t, _, _) = resolve_model_metadata("opencode-zen", "gpt-9.9-future");
        assert_eq!(t, Some(Format::OpenAiResponses));
        let (t, _, _) = resolve_model_metadata("ocz", "grok-9.9-future");
        assert_eq!(t, Some(Format::OpenAiResponses));
        // muse-spark guard covers zen too.
        let (t, _, _) = resolve_model_metadata("opencode-zen", "muse-spark-9.9-foo");
        assert_eq!(t, Some(Format::OpenAiResponses));
        // Chat-lane models keep no model target.
        let (t, _, _) = resolve_model_metadata("opencode-zen", "big-pickle");
        assert_eq!(t, None);
    }

    // ---- 9router v0.5.109 (82aeb90): antigravity tiered model resolution ----

    #[test]
    fn antigravity_unadorned_model_resolves_tier_from_reasoning_effort() {
        // reasoning_effort picks the tiered catalog variant; the preset
        // "(tier)" suffix is re-applied like upstream getModelUpstreamId.
        let body = json!({"model": "gemini-3.8-flash", "reasoning_effort": "high"});
        let (_, upstream, _) =
            resolve_model_metadata_with_client("antigravity", "gemini-3.8-flash", Some(&body));
        assert_eq!(upstream, "gemini-3.8-flash-high(high)");

        // Alias provider id resolves the same catalog entry.
        let (_, upstream, _) =
            resolve_model_metadata_with_client("ag", "gemini-3.8-flash", Some(&body));
        assert_eq!(upstream, "gemini-3.8-flash-high(high)");
    }

    #[test]
    fn antigravity_unadorned_argon_resolves_to_tiered_variant() {
        // gemini-4-argon has no bare catalog entry — only the tiered ids.
        for (effort, expected) in [
            ("high", "gemini-4-argon-high(high)"),
            ("medium", "gemini-4-argon-medium(medium)"),
            ("low", "gemini-4-argon-low(low)"),
        ] {
            let body = json!({"model": "gemini-4-argon", "reasoning_effort": effort});
            let (_, upstream, _) =
                resolve_model_metadata_with_client("antigravity", "gemini-4-argon", Some(&body));
            assert_eq!(upstream, expected, "effort={effort}");
        }
    }

    #[test]
    fn antigravity_tier_normalization_and_budget_fallback() {
        // xhigh/max collapse to high; minimal collapses to low.
        let body = json!({"reasoning_effort": "xhigh"});
        let (_, upstream, _) =
            resolve_model_metadata_with_client("antigravity", "gemini-3.8-flash", Some(&body));
        assert_eq!(upstream, "gemini-3.8-flash-high(high)");

        let body = json!({"reasoning_effort": "minimal"});
        let (_, upstream, _) =
            resolve_model_metadata_with_client("antigravity", "gemini-3.8-flash", Some(&body));
        assert_eq!(upstream, "gemini-3.8-flash-low(low)");

        // thinking.budget_tokens thresholds: >16384 high, <=4096 low, else medium.
        for (budget, expected) in [
            (20000, "gemini-3.8-flash-high(high)"),
            (8192, "gemini-3.8-flash-medium(medium)"),
            (2048, "gemini-3.8-flash-low(low)"),
        ] {
            let body = json!({"thinking": {"budget_tokens": budget}});
            let (_, upstream, _) =
                resolve_model_metadata_with_client("antigravity", "gemini-3.8-flash", Some(&body));
            assert_eq!(upstream, expected, "budget={budget}");
        }

        // output_config.effort and reasoning.effort are alternate effort keys.
        let body = json!({"output_config": {"effort": "low"}});
        let (_, upstream, _) =
            resolve_model_metadata_with_client("antigravity", "gemini-3.8-flash", Some(&body));
        assert_eq!(upstream, "gemini-3.8-flash-low(low)");
        let body = json!({"reasoning": {"effort": "medium"}});
        let (_, upstream, _) =
            resolve_model_metadata_with_client("antigravity", "gemini-3.8-flash", Some(&body));
        assert_eq!(upstream, "gemini-3.8-flash-medium(medium)");
    }

    #[test]
    fn antigravity_model_suffix_and_default_preset() {
        // "(level)" suffix on the model id resolves the tiered variant too.
        let (_, upstream, _) =
            resolve_model_metadata_with_client("antigravity", "gemini-3.8-flash(low)", None);
        assert_eq!(upstream, "gemini-3.8-flash-low(low)");

        // No tier requested → bare base entry keeps its catalog preset.
        let (_, upstream, _) =
            resolve_model_metadata_with_client("antigravity", "gemini-3.8-flash", None);
        assert_eq!(upstream, "gemini-3.8-flash-medium(medium)");

        // Unknown tier falls back to the base entry and re-stamps the tier.
        let body = json!({"reasoning_effort": "ultra"});
        let (_, upstream, _) =
            resolve_model_metadata_with_client("antigravity", "gemini-3.8-flash", Some(&body));
        assert_eq!(upstream, "gemini-3.8-flash-medium(ultra)");
    }

    #[test]
    fn antigravity_plan_dispatch_uses_resolved_tier() {
        let body = json!({
            "model": "ag/gemini-4-argon",
            "reasoning_effort": "low",
            "messages": [{"role": "user", "content": "hi"}],
        });
        let plan = RequestPlan::new(
            Some("/v1/chat/completions"),
            &body,
            "antigravity",
            "gemini-4-argon",
        );
        // upstream "gemini-4-argon-low(low)" → thinking suffix stripped for
        // dispatch, level kept for post-translate re-apply.
        assert_eq!(plan.dispatch_model(), "gemini-4-argon-low");
        assert_eq!(plan.thinking_level.as_deref(), Some("low"));
    }
}
