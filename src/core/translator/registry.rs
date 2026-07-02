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

/// Valid OpenAI content block types (mirrors VALID_OPENAI_CONTENT_TYPES in schema/blocks.js).
const VALID_OPENAI_CONTENT_TYPES: &[&str] = &[
    "text",
    "image_url",
    "image",
    "input_audio",
    "audio_url",
    "refusal",
];

/// Valid OpenAI message-level roles (mirrors VALID_OPENAI_MESSAGE_TYPES).
const VALID_OPENAI_MESSAGE_TYPES: &[&str] = &["system", "user", "assistant", "tool"];

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
    CommandCode,
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
            "commandcode" | "command-code" => Some(Self::CommandCode),
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
            Self::CommandCode => "commandcode",
        }
    }

    /// Returns true if this format supports remote image URLs natively.
    pub fn is_openai(&self) -> bool {
        matches!(
            self,
            Self::OpenAi | Self::OpenAiResponses | Self::OpenAiResponse
        )
    }

    /// Returns true if this format needs images as inline base64
    /// rather than supporting remote HTTP URLs (9router TARGETS_NEED_BASE64 parity).
    pub fn needs_image_prefetch(&self) -> bool {
        matches!(
            self,
            Self::Gemini
                | Self::GeminiCli
                | Self::Vertex
                | Self::Ollama
                | Self::CommandCode
                | Self::Antigravity
                | Self::Kiro
        )
    }
}

/// Request transform signature: (model, body, stream) -> transformed_body
pub type RequestTransformFn =
    fn(model: &str, body: &mut Value, stream: bool, credentials: Option<&Value>) -> bool;

/// Response transform signature: (chunk, state) -> Vec<String>
/// Returns SSE lines to emit.
pub type ResponseTransformFn = fn(chunk: &[u8], state: &mut ResponseTransformState) -> Vec<String>;

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
    /// CommandCode streaming state
    pub commandcode: CommandCodeResponseState,
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
    /// Dynamic state used by claude_to_openai_response streaming transform
    pub claude_state: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Default)]
pub struct GeminiResponseState {
    pub line_buffer: String,
    pub current_part_index: usize,
    /// Accumulated tool call data: tool-call-index -> {id, name, arguments_buf}
    pub tool_calls_accum: serde_json::Map<String, Value>,
    /// Response ID extracted from the first OpenAI SSE chunk
    pub response_id: String,
    /// Model name extracted from the first OpenAI SSE chunk
    pub model: String,
    /// Whether we have already emitted a finish chunk (guard against duplicates)
    pub finish_emitted: bool,
    /// Dynamic state used by gemini_to_openai_response streaming transform
    pub gemini_state: std::collections::HashMap<String, Value>,
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
    /// Generic state used by responses_to_chat_response (OpenAiResponses -> OpenAi).
    pub state: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Default)]
pub struct CursorResponseState {
    pub frame_buffer: Vec<u8>,
    pub decompress_buffer: Vec<u8>,
    pub in_message: bool,
    /// Generic state used by cursor_to_openai_response.
    pub state: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Default)]
pub struct OllamaResponseState {
    pub line_buffer: String,
    pub message_idx: usize,
    /// Generic state used by ollama_to_openai_response.
    pub state: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Default)]
pub struct KiroResponseState {
    pub event_buffer: Vec<u8>,
    pub current_event_type: Option<String>,
    /// Generic state used by kiro_to_openai_response.
    pub state: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Default)]
pub struct CommandCodeResponseState {
    pub response_id: Option<String>,
    pub created: Option<i64>,
    pub model: Option<String>,
    pub chunk_index: u64,
    pub tool_index: u64,
    pub tool_index_by_id: serde_json::Map<String, Value>,
    pub finish_reason: Option<String>,
    pub usage: Option<Value>,
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
        && body
            .get("userAgent")
            .and_then(Value::as_str)
            .is_some_and(|ua| ua == "antigravity")
    {
        return Format::Antigravity;
    }

    // 3. CommandCode: has `threadId` and `params.messages` instead of top-level `messages`
    if body.get("threadId").is_some()
        && body.get("params").and_then(|p| p.get("messages")).is_some()
    {
        return Format::CommandCode;
    }

    // 4. Gemini format: has `contents[]` or `systemInstruction[]`
    if body.get("contents").is_some() || body.get("systemInstruction").is_some() {
        return Format::Gemini;
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

    // 6. OpenAI-specific indicators (fields that never appear in Claude format)
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

    // 7. Default to OpenAI
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
        "commandcode" | "command-code" => Format::CommandCode,
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
    pub fn register_request(&mut self, from: Format, to: Format, f: RequestTransformFn) {
        self.request_transforms.insert((from, to), f);
    }

    /// Register a response transform.
    pub fn register_response(&mut self, from: Format, to: Format, f: ResponseTransformFn) {
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
                // Use the same persistent state so the transform can accumulate
                // tool calls and other per-stream data across chunks.
                let converted = transform(chunk, state);
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
    // normalizeDeveloperRole: rewrite role "developer" -> "system" so
    // OAI-compat providers (DeepSeek, Groq, Ollama, …) that pre-date the
    // Codex CLI role split don't 400 on the request.
    crate::core::translator::helpers::openai_helper::normalize_developer_role(body);
    // ensureToolCallIds: ensure tool_calls have ids (full impl from tool_call_helper)
    crate::core::translator::helpers::tool_call_helper::ensure_tool_call_ids(body);
    // fixMissingToolResponses: insert empty tool_result if needed (full impl from tool_call_helper)
    crate::core::translator::helpers::tool_call_helper::fix_missing_tool_responses(body);
    true
}

/// Strip specific content types from messages (opt-in via stripList).
/// Mirrors stripContentTypes in open-sse/translator/index.js.
pub fn strip_content_types(body: &mut Value, strip_list: &[&str]) {
    if strip_list.is_empty() {
        return;
    }
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };

    let image_types: std::collections::HashSet<&str> = ["image_url", "image"].into_iter().collect();
    let audio_types: std::collections::HashSet<&str> =
        ["audio_url", "input_audio"].into_iter().collect();

    let strip_image = strip_list.contains(&"image");
    let strip_audio = strip_list.contains(&"audio");

    for msg in messages.iter_mut() {
        let Some(content) = msg.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        content.retain(|part| {
            let t = match part.get("type").and_then(Value::as_str) {
                Some(t) => t,
                None => return true,
            };
            if image_types.contains(t) && strip_image {
                return false;
            }
            if audio_types.contains(t) && strip_audio {
                return false;
            }
            true
        });
        if content.is_empty() {
            if let Some(obj) = msg.as_object_mut() {
                obj.insert("content".to_string(), Value::String(String::new()));
            }
        }
    }
}

/// Filter messages to OpenAI standard format.
/// Mirrors filterToOpenAIFormat in open-sse/translator/formats/openai.js.
pub fn filter_to_openai_format(body: &mut Value, preserve_cache_control: bool) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };

    // Process each message
    for msg in messages.iter_mut() {
        // Normalize developer role to system (many providers don't support developer)
        if let Some(obj) = msg.as_object_mut() {
            if obj.get("role").and_then(Value::as_str) == Some("developer") {
                obj.insert("role".to_string(), Value::String("system".to_string()));
            }
        }

        // Keep tool messages as-is
        if msg.get("role").and_then(Value::as_str) == Some("tool") {
            continue;
        }

        // Keep assistant messages with tool_calls as-is
        if msg.get("role").and_then(Value::as_str) == Some("assistant")
            && msg.get("tool_calls").is_some()
        {
            continue;
        }

        // Handle string content — keep as-is
        if msg.get("content").and_then(Value::as_str).is_some() {
            continue;
        }

        // Handle array content — strip Claude-specific blocks
        if let Some(arr) = msg.get_mut("content").and_then(Value::as_array_mut) {
            let mut filtered: Vec<Value> = Vec::new();
            for block in arr.drain(..) {
                let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
                // Skip thinking blocks
                if block_type == "thinking"
                    || block_type == "redacted_thinking"
                    || block_type == "signature"
                {
                    continue;
                }
                // Only keep valid OpenAI content types
                if VALID_OPENAI_CONTENT_TYPES.contains(&block_type) {
                    let mut cleaned = block;
                    if let Some(obj) = cleaned.as_object_mut() {
                        obj.remove("signature");
                        if !preserve_cache_control {
                            obj.remove("cache_control");
                        }
                    }
                    filtered.push(cleaned);
                } else if block_type == "tool_use" || block_type == "tool_result" {
                    // Keep tool blocks as-is (they'll be handled separately)
                    filtered.push(block);
                }
            }

            // If all content was filtered, add empty text
            if filtered.is_empty() {
                filtered.push(serde_json::json!({"type": "text", "text": ""}));
            }

            if let Some(obj) = msg.as_object_mut() {
                obj.insert("content".to_string(), Value::Array(filtered));
            }
        }
    }

    // Filter out messages with only empty text (but NEVER filter tool messages)
    messages.retain(|msg| {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        // Always keep tool messages
        if role == "tool" {
            return true;
        }
        // Always keep assistant messages with tool_calls
        if role == "assistant" && msg.get("tool_calls").is_some() {
            return true;
        }
        // Check content
        match msg.get("content") {
            Some(Value::String(s)) => !s.trim().is_empty(),
            Some(Value::Array(arr)) => arr.iter().any(|b| {
                let t = b.get("type").and_then(Value::as_str).unwrap_or("");
                if t == "text" {
                    b.get("text")
                        .and_then(Value::as_str)
                        .map(|s| !s.trim().is_empty())
                        .unwrap_or(false)
                } else {
                    true
                }
            }),
            _ => true,
        }
    });

    // Remove empty tools array
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        if tools.is_empty() {
            if let Some(obj) = body.as_object_mut() {
                obj.remove("tools");
            }
        }
    }

    // Normalize tools to OpenAI format (from Claude, Gemini, etc.)
    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
        let mut normalized: Vec<Value> = Vec::new();
        for tool in tools.drain(..) {
            // Already OpenAI format
            if tool.get("type").and_then(Value::as_str) == Some("function")
                && tool.get("function").is_some()
            {
                normalized.push(tool);
                continue;
            }
            // Claude format: {name, description, input_schema}
            if tool.get("name").is_some()
                && (tool.get("input_schema").is_some() || tool.get("description").is_some())
            {
                normalized.push(serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": tool.get("name").and_then(Value::as_str).unwrap_or(""),
                        "description": tool.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                        "parameters": tool.get("input_schema").cloned().unwrap_or(serde_json::json!({"type": "object", "properties": {}}))
                    }
                }));
                continue;
            }
            // Gemini format: {functionDeclarations: [{name, description, parameters}]}
            if let Some(decls) = tool.get("functionDeclarations").and_then(Value::as_array) {
                for fn_decl in decls {
                    normalized.push(serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": fn_decl.get("name").and_then(Value::as_str).unwrap_or(""),
                            "description": fn_decl.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                            "parameters": fn_decl.get("parameters").cloned().unwrap_or(serde_json::json!({"type": "object", "properties": {}}))
                        }
                    }));
                }
                continue;
            }
            normalized.push(tool);
        }
        *tools = normalized;
    }

    // Normalize tool_choice to OpenAI format
    if let Some(choice) = body.get("tool_choice").cloned() {
        if let Some(choice_obj) = choice.as_object() {
            let choice_type = choice_obj.get("type").and_then(Value::as_str).unwrap_or("");
            match choice_type {
                "auto" => {
                    if let Some(obj) = body.as_object_mut() {
                        obj.insert("tool_choice".to_string(), Value::String("auto".to_string()));
                    }
                }
                "any" => {
                    if let Some(obj) = body.as_object_mut() {
                        obj.insert(
                            "tool_choice".to_string(),
                            Value::String("required".to_string()),
                        );
                    }
                }
                "tool" => {
                    if let Some(name) = choice_obj.get("name").and_then(Value::as_str) {
                        if let Some(obj) = body.as_object_mut() {
                            obj.insert(
                                "tool_choice".to_string(),
                                serde_json::json!({
                                    "type": "function",
                                    "function": {"name": name}
                                }),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// Normalize thinking config: remove if last message is not user.
/// Stub — mirrors normalizeThinkingConfig in 9router.
pub fn normalize_thinking_config(_body: &mut Value) {
    // Full implementation would check if last message is user
    // and remove/reduce thinking budget accordingly.
    // For now, this is a no-op.
}

/// Global registry instance — lazily initialized.
use std::sync::OnceLock;
static REGISTRY: OnceLock<TranslationRegistry> = OnceLock::new();

/// Get the global translation registry.
/// Initializes with all registered transforms on first call.
pub fn global_registry() -> &'static TranslationRegistry {
    use crate::core::translator::request::antigravity_to_openai::antigravity_to_openai_request;
    use crate::core::translator::request::claude_to_openai::claude_to_openai_request;
    use crate::core::translator::request::gemini_to_openai::gemini_to_openai_request;
    use crate::core::translator::request::openai_responses::{
        chat_to_openai_responses_request, openai_responses_to_chat_request,
    };
    use crate::core::translator::request::openai_to_claude::openai_to_claude_request;
    use crate::core::translator::request::openai_to_commandcode::openai_to_commandcode_request;
    use crate::core::translator::request::openai_to_cursor::openai_to_cursor_request;
    use crate::core::translator::request::openai_to_gemini::openai_to_gemini_cli_request;
    use crate::core::translator::request::openai_to_gemini::openai_to_gemini_request;
    use crate::core::translator::request::openai_to_kiro::openai_to_kiro_request;
    use crate::core::translator::request::openai_to_ollama::openai_to_ollama_request;
    use crate::core::translator::request::openai_to_vertex::openai_to_vertex_request;
    use crate::core::translator::response::claude_to_openai::claude_to_openai_streaming;
    use crate::core::translator::response::commandcode_to_openai::commandcode_to_openai_response;
    use crate::core::translator::response::cursor_to_openai::cursor_to_openai_streaming;
    use crate::core::translator::response::gemini_to_openai::gemini_to_openai_streaming;
    use crate::core::translator::response::kiro_to_openai::kiro_to_openai_streaming;
    use crate::core::translator::response::ollama_to_openai::ollama_to_openai_streaming;
    use crate::core::translator::response::openai_responses::responses_to_chat_streaming;
    use crate::core::translator::response::openai_to_gemini::openai_to_gemini_response;

    REGISTRY.get_or_init(|| {
        let mut reg = TranslationRegistry::new();

        // Request transforms
        reg.register_request(
            Format::OpenAi,
            Format::Claude,
            openai_to_claude_request as RequestTransformFn,
        );
        reg.register_request(
            Format::Claude,
            Format::OpenAi,
            claude_to_openai_request as RequestTransformFn,
        );
        reg.register_request(
            Format::Gemini,
            Format::OpenAi,
            gemini_to_openai_request as RequestTransformFn,
        );
        reg.register_request(
            Format::GeminiCli,
            Format::OpenAi,
            gemini_to_openai_request as RequestTransformFn,
        );
        reg.register_request(
            Format::OpenAi,
            Format::Ollama,
            openai_to_ollama_request as RequestTransformFn,
        );
        reg.register_request(
            Format::OpenAi,
            Format::Gemini,
            openai_to_gemini_request as RequestTransformFn,
        );
        reg.register_request(
            Format::OpenAi,
            Format::GeminiCli,
            openai_to_gemini_cli_request as RequestTransformFn,
        );
        reg.register_request(
            Format::OpenAi,
            Format::Vertex,
            openai_to_vertex_request as RequestTransformFn,
        );
        reg.register_request(
            Format::OpenAi,
            Format::Kiro,
            openai_to_kiro_request as RequestTransformFn,
        );
        reg.register_request(
            Format::OpenAi,
            Format::Cursor,
            openai_to_cursor_request as RequestTransformFn,
        );
        reg.register_request(
            Format::OpenAi,
            Format::Antigravity,
            openai_to_gemini_cli_request as RequestTransformFn,
        );
        reg.register_request(
            Format::Antigravity,
            Format::OpenAi,
            antigravity_to_openai_request as RequestTransformFn,
        );
        reg.register_request(
            Format::OpenAi,
            Format::OpenAiResponses,
            chat_to_openai_responses_request as RequestTransformFn,
        );
        reg.register_request(
            Format::OpenAiResponses,
            Format::OpenAi,
            openai_responses_to_chat_request as RequestTransformFn,
        );
        reg.register_request(
            Format::OpenAi,
            Format::Codex,
            chat_to_openai_responses_request as RequestTransformFn,
        );
        reg.register_request(
            Format::OpenAi,
            Format::CommandCode,
            openai_to_commandcode_request as RequestTransformFn,
        );
        reg.register_response(
            Format::CommandCode,
            Format::OpenAi,
            commandcode_to_openai_response as ResponseTransformFn,
        );
        reg.register_response(
            Format::OpenAi,
            Format::Gemini,
            openai_to_gemini_response as ResponseTransformFn,
        );
        reg.register_response(
            Format::Claude,
            Format::OpenAi,
            claude_to_openai_streaming as ResponseTransformFn,
        );
        reg.register_response(
            Format::Gemini,
            Format::OpenAi,
            gemini_to_openai_streaming as ResponseTransformFn,
        );
        reg.register_response(
            Format::Ollama,
            Format::OpenAi,
            ollama_to_openai_streaming as ResponseTransformFn,
        );
        reg.register_response(
            Format::Cursor,
            Format::OpenAi,
            cursor_to_openai_streaming as ResponseTransformFn,
        );
        reg.register_response(
            Format::Kiro,
            Format::OpenAi,
            kiro_to_openai_streaming as ResponseTransformFn,
        );
        reg.register_response(
            Format::OpenAiResponses,
            Format::OpenAi,
            responses_to_chat_streaming as ResponseTransformFn,
        );

        reg
    })
}
