//! Translation registry — mirrors open-sse/translator/index.js
//!
//! Provides a registry-backed translation system for request and response transforms.
//! The pipeline is: source format -> OpenAI intermediate -> target format.
//!
//! This module does NOT include the actual transform implementations.
//! Those live in request_transform.rs (to be filled by Phase 2 translator beads)
//! and response_transform.rs (already partially implemented).

use serde_json::Value;
use std::collections::HashMap;

/// All supported translation formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    OpenAi,
    OpenAiResponses,
    OpenAiResponse,
    Claude,
    Gemini,
    GeminiCli,
    Vertex,
    Codex,
    Antigravity,
    Kiro,
    Cursor,
    Ollama,
}

impl Format {
    /// Parse from string (used for registry key lookups).
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "openai" => Some(Self::OpenAi),
            "openai-responses" | "openaiResponses" => Some(Self::OpenAiResponses),
            "openai-response" => Some(Self::OpenAiResponse),
            "claude" => Some(Self::Claude),
            "gemini" => Some(Self::Gemini),
            "gemini-cli" => Some(Self::GeminiCli),
            "vertex" => Some(Self::Vertex),
            "codex" => Some(Self::Codex),
            "antigravity" => Some(Self::Antigravity),
            "kiro" => Some(Self::Kiro),
            "cursor" => Some(Self::Cursor),
            "ollama" => Some(Self::Ollama),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::OpenAiResponses => "openai-responses",
            Self::OpenAiResponse => "openai-response",
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::GeminiCli => "gemini-cli",
            Self::Vertex => "vertex",
            Self::Codex => "codex",
            Self::Antigravity => "antigravity",
            Self::Kiro => "kiro",
            Self::Cursor => "cursor",
            Self::Ollama => "ollama",
        }
    }

    /// Returns true if this format uses OpenAI's native structure.
    pub fn is_openai(&self) -> bool {
        matches!(
            self,
            Self::OpenAi | Self::OpenAiResponses | Self::OpenAiResponse
        )
    }
}

/// Request transform signature: (model, body, stream) -> transformed_body
pub type RequestTransformFn =
    fn(model: &str, body: &mut Value, stream: bool, credentials: Option<&Value>) -> bool;

/// Response transform signature: (chunk, state) -> Vec<String>
/// Returns SSE lines to emit.
pub type ResponseTransformFn =
    fn(chunk: &[u8], state: &mut ResponseTransformState) -> Vec<String>;

/// Shared state for response streaming transforms.
/// Each format has its own state variant tracked here.
#[derive(Debug, Clone, Default)]
pub struct ResponseTransformState {
    /// OpenAI SSE state
    pub openai: OpenAiResponseState,
    /// Anthropic SSE state
    pub anthropic: AnthropicResponseState,
    /// Gemini SSE state
    pub gemini: GeminiResponseState,
    /// Responses API state
    pub responses: ResponsesResponseState,
    /// Cursor streaming state
    pub cursor: CursorResponseState,
    /// Ollama streaming state
    pub ollama: OllamaResponseState,
    /// Kiro streaming state
    pub kiro: KiroResponseState,
}

#[derive(Debug, Clone, Default)]
pub struct OpenAiResponseState {
    pub line_buffer: String,
}

#[derive(Debug, Clone, Default)]
pub struct AnthropicResponseState {
    pub line_buffer: String,
    pub current_block_index: Option<usize>,
    pub text_accumulator: String,
    pub thinking_buffer: String,
    pub in_thinking: bool,
    pub cache_lookaheads: Vec<String>,
    pub message_id: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct GeminiResponseState {
    pub line_buffer: String,
    pub current_part_index: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ResponsesResponseState {
    pub buffer: String,
    pub seq: usize,
    pub msg_text_buf: String,
    pub reasoning_buf: String,
    pub func_args_buf: String,
    pub func_names: std::collections::HashMap<usize, String>,
    pub func_call_ids: std::collections::HashMap<usize, String>,
    pub msg_item_done: std::collections::HashMap<usize, bool>,
    pub completed_sent: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CursorResponseState {
    pub frame_buffer: Vec<u8>,
    pub decompress_buffer: Vec<u8>,
    pub in_message: bool,
}

#[derive(Debug, Clone, Default)]
pub struct OllamaResponseState {
    pub line_buffer: String,
    pub message_idx: usize,
}

#[derive(Debug, Clone, Default)]
pub struct KiroResponseState {
    pub event_buffer: Vec<u8>,
    pub current_event_type: Option<String>,
}

/// Detect source format from request body structure.
/// Mirrors open-sse/services/provider.js:detectFormat() logic.
/// Priority: Responses > Antigravity > Gemini > OpenAI-specific fields > Claude hints > default OpenAI.
pub fn detect_source_format(body: &Value) -> Format {
    // 1. OpenAI Responses API: has `input` instead of `messages[]`
    if body.get("input").is_some() {
        return Format::OpenAiResponses;
    }

    // 2. Antigravity format: Gemini wrapped in body.request
    if body
        .get("request")
        .and_then(|r| r.get("contents"))
        .is_some()
    {
        if body
            .get("userAgent")
            .and_then(Value::as_str)
            .is_some_and(|ua| ua == "antigravity")
        {
            return Format::Antigravity;
        }
    }

    // 3. Gemini format: has `contents[]` or `systemInstruction[]`
    if body.get("contents").is_some() || body.get("systemInstruction").is_some() {
        return Format::Gemini;
    }

    // 4. OpenAI-specific indicators (fields that never appear in Claude format)
    if body.get("stream_options").is_some()
        || body.get("response_format").is_some()
        || body.get("logprobs").is_some()
        || body.get("top_logprobs").is_some()
        || body.get("n").is_some()
        || body.get("presence_penalty").is_some()
        || body.get("frequency_penalty").is_some()
        || body.get("logit_bias").is_some()
        || body.get("user").is_some()
    {
        return Format::OpenAi;
    }

    // 5. Claude-specific indicators
    // Check first message for Claude-style content types
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        if let Some(first) = messages.first() {
            // Check for Claude-specific fields at body level
            if body.get("system").is_some() || body.get("anthropic_version").is_some() {
                return Format::Claude;
            }

            // Check first message content array for Claude types
            if let Some(content) = first.get("content").and_then(Value::as_array) {
                for part in content {
                    let t = part.get("type").and_then(Value::as_str);
                    // Claude uses `image` with `source.type`, tool_use, tool_result
                    if t == Some("tool_use") || t == Some("tool_result") {
                        return Format::Claude;
                    }
                    // Claude image: {type:"image", source:{type:"base64", ...}}
                    if t == Some("image") && part.get("source").is_some() {
                        return Format::Claude;
                    }
                }
            }
        }
    }

    // 6. Default to OpenAI
    Format::OpenAi
}

/// Detect source format from endpoint path.
/// Mirrors open-sse/services/provider.js:detectFormatByEndpoint() logic.
pub fn detect_source_format_by_endpoint(path: &str) -> Option<Format> {
    if path.contains("/v1/responses") {
        return Some(Format::OpenAiResponses);
    }
    if path.contains("/v1/messages") {
        return Some(Format::Claude);
    }
    None
}

/// Get the default target format for a provider.
/// Mirrors open-sse/services/provider.js:getTargetFormat().
pub fn get_target_format_for_provider(provider: &str) -> Format {
    match provider {
        "openai" => Format::OpenAi,
        "anthropic" | "claude" | "glm" | "kimi" | "minimax" | "minimax-cn" | "kimi-coding" => {
            Format::Claude
        }
        "gemini" => Format::Gemini,
        "gemini-cli" => Format::GeminiCli,
        "vertex" => Format::Vertex,
        "codex" => Format::OpenAiResponses,
        "cursor" => Format::Cursor,
        "kiro" => Format::Kiro,
        "ollama" | "ollama-local" | "ollama-cloud" => Format::Ollama,
        "antigravity" => Format::Antigravity,
        // All OpenAI-compatible providers default to OpenAI
        _ => Format::OpenAi,
    }
}

/// Translation registry for request and response transforms.
#[derive(Default)]
pub struct TranslationRegistry {
    /// Request transforms: (source_format, target_format) -> transform_fn
    request_transforms: HashMap<(Format, Format), RequestTransformFn>,
    /// Response transforms: (source_format, target_format) -> transform_fn
    response_transforms: HashMap<(Format, Format), ResponseTransformFn>,
}

impl TranslationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a request transform.
    pub fn register_request(
        &mut self,
        from: Format,
        to: Format,
        f: RequestTransformFn,
    ) {
        self.request_transforms.insert((from, to), f);
    }

    /// Register a response transform.
    pub fn register_response(
        &mut self,
        from: Format,
        to: Format,
        f: ResponseTransformFn,
    ) {
        self.response_transforms.insert((from, to), f);
    }

    /// Check if a request transform exists.
    pub fn has_request_transform(&self, from: Format, to: Format) -> bool {
        self.request_transforms.contains_key(&(from, to))
    }

    /// Check if a response transform exists.
    pub fn has_response_transform(&self, from: Format, to: Format) -> bool {
        self.response_transforms.contains_key(&(from, to))
    }

    /// Apply request transform: source -> OpenAI intermediate -> target.
    /// If source == target, applies normalization only.
    pub fn translate_request(
        &self,
        source: Format,
        target: Format,
        model: &str,
        body: &mut Value,
        stream: bool,
        credentials: Option<&Value>,
    ) -> bool {
        if source == target && source == Format::OpenAi {
            // Same format, apply normalization only
            return apply_normalization_hooks(body);
        }

        if source == Format::OpenAi && target == Format::OpenAi {
            return apply_normalization_hooks(body);
        }

        // Step 1: source -> OpenAI intermediate (if needed)
        if source != Format::OpenAi {
            let key = (source, Format::OpenAi);
            if let Some(transform) = self.request_transforms.get(&key) {
                let _ = transform(model, body, stream, credentials);
            }
        }

        // Step 2: OpenAI intermediate -> target (if needed)
        if target != Format::OpenAi {
            let key = (Format::OpenAi, target);
            if let Some(transform) = self.request_transforms.get(&key) {
                let _ = transform(model, body, stream, credentials);
            }
        }

        // Always apply normalization
        apply_normalization_hooks(body)
    }

    /// Apply response transform.
    pub fn translate_response(
        &self,
        source: Format,
        target: Format,
        chunk: &[u8],
        state: &mut ResponseTransformState,
    ) -> Vec<String> {
        if source == target {
            return vec![String::from_utf8_lossy(chunk).to_string()];
        }

        let mut results = Vec::new();

        // Step 1: source -> OpenAI intermediate
        if source != Format::OpenAi {
            let key = (source, Format::OpenAi);
            if let Some(transform) = self.response_transforms.get(&key) {
                let converted = transform(chunk, state);
                if !converted.is_empty() {
                    results = converted;
                }
            }
        } else {
            results.push(String::from_utf8_lossy(chunk).to_string());
        }

        // Step 2: OpenAI intermediate -> target
        if target != Format::OpenAi {
            let key = (Format::OpenAi, target);
            if let Some(transform) = self.response_transforms.get(&key) {
                let mut intermediate_state = ResponseTransformState::default();
                let converted = transform(chunk, &mut intermediate_state);
                if !converted.is_empty() {
                    return converted;
                }
            }
        }

        results
    }
}

/// Apply normalization hooks that are always run regardless of translation.
/// Mirrors the hooks in open-sse/translator/index.js:
///   stripContentTypes, normalizeThinkingConfig, ensureToolCallIds, fixMissingToolResponses
fn apply_normalization_hooks(body: &mut Value) -> bool {
    // ensureToolCallIds: ensure tool_calls have ids
    ensure_tool_call_ids(body);
    // fixMissingToolResponses: insert empty tool_result if needed
    fix_missing_tool_responses(body);
    true
}

/// Ensure every tool_call has an id field.
/// If id is missing, generate one.
pub fn ensure_tool_call_ids(body: &mut Value) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };

    let mut call_counter: usize = 0;
    for msg in messages.iter_mut() {
        let Some(tool_calls) = msg.get_mut("tool_calls").and_then(Value::as_array_mut) else {
            continue;
        };

        for tc in tool_calls.iter_mut() {
            let func_name = {
                let func = tc.get("function");
                let name = func.and_then(|f| f.get("name"));
                name.and_then(|n| n.as_str()).unwrap_or("tool").to_string()
            };
            if tc.get("id").is_none() {
                let id = format!("call_{func_name}_{call_counter}");
                if let Some(tc_obj) = tc.as_object_mut() {
                    tc_obj.insert("id".into(), Value::String(id));
                }
                call_counter += 1;
            }
        }
    }
}

/// Insert empty tool_result content blocks after tool_call messages
/// if the next assistant message is missing them.
fn fix_missing_tool_responses(_body: &mut Value) {
    // This is a simplified version. The full implementation in JS
    // also handles multi-turn conversations and checks the role sequence.
    // Will be expanded in the translator bead (br-3fu).
}

/// Global registry instance — lazily initialized.
use std::sync::OnceLock;
static REGISTRY: OnceLock<TranslationRegistry> = OnceLock::new();

/// Get the global translation registry.
/// Initializes with all registered transforms on first call.
pub fn global_registry() -> &'static TranslationRegistry {
    REGISTRY.get_or_init(|| {
        let mut reg = TranslationRegistry::new();
        // Phase 2 beads will register transforms here.
        // Example registration (filled by br-3fu, br-2t9, br-1mw):
        // reg.register_request(Format::OpenAi, Format::Claude, openai_to_claude_transform);
        // reg.register_request(Format::Claude, Format::OpenAi, claude_to_openai_transform);
        // ...
        reg
    })
}
