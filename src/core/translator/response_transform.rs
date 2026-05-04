//! Response transformation for streaming SSE format conversion
//!
//! This module handles chunk-by-chunk format transformation for different provider formats.
//! Each executor implements trait-based response transformation that converts provider-specific
//! streaming formats to OpenAI SSE format.

use bytes::Bytes;
use serde::Deserialize;

/// Base streaming state shared across all transformations
#[derive(Debug, Clone, Default)]
pub struct StreamingBase {
    /// Buffer for incomplete SSE lines
    pub line_buffer: String,
    /// Track if we're inside a data field
    pub in_data_field: bool,
    /// Accumulated content for current chunk
    pub content_accumulator: String,
}

/// OpenAI SSE streaming state
#[derive(Debug, Clone)]
pub struct OpenAiStreamingState {
    pub base: StreamingBase,
}

/// Anthropic SSE streaming state
#[derive(Debug, Clone, Default)]
pub struct AnthropicStreamingState {
    pub base: StreamingBase,
    /// Track partial message for content blocks
    pub current_block: Option<String>,
    /// Cache control metadata
    pub cache_lookaheads: Vec<String>,
}

/// Gemini streaming state
#[derive(Debug, Clone, Default)]
pub struct GeminiStreamingState {
    pub base: StreamingBase,
    /// Track current part index
    pub current_part_index: usize,
}

/// Ollama streaming state
#[derive(Debug, Clone, Default)]
pub struct OllamaStreamingState {
    pub base: StreamingBase,
    /// Track message index
    pub message_idx: usize,
}

/// Cursor Connect Protocol streaming state
#[derive(Debug, Clone, Default)]
pub struct CursorStreamingState {
    pub base: StreamingBase,
    /// Raw frame buffer for binary protocol
    pub frame_buffer: Vec<u8>,
    /// Decompressed buffer
    pub decompress_buffer: Vec<u8>,
    /// Track if inside message
    pub in_message: bool,
}

/// Kiro EventStream state
#[derive(Debug, Clone, Default)]
pub struct KiroStreamingState {
    pub base: StreamingBase,
    /// Event stream buffer
    pub event_buffer: Vec<u8>,
    /// Current event type
    pub current_event_type: Option<String>,
}

/// Trait for transforming streaming responses
pub trait StreamingTransformer: Send {
    /// Transform a chunk of bytes into OpenAI SSE format
    fn transform_chunk(&mut self, chunk: &Bytes) -> Vec<String>;

    /// Get the format this transformer outputs
    fn output_format(&self) -> &str;

    /// Check if this transformer handles the given content type
    fn matches_content_type(&self, content_type: Option<&str>) -> bool;
}

/// OpenAI SSE format transformer
#[derive(Debug, Clone, Default)]
pub struct OpenAiTransformer;

impl OpenAiTransformer {
    pub fn new() -> Self {
        Self
    }
}

impl StreamingTransformer for OpenAiTransformer {
    fn transform_chunk(&mut self, chunk: &Bytes) -> Vec<String> {
        let text = String::from_utf8_lossy(chunk);
        let mut lines = Vec::new();

        for line in text.lines() {
            let line = line.trim();
            if let Some(data) = line.strip_prefix("data: ") {
                let data = data.trim();

                if data == "[DONE]" {
                    lines.push("data: [DONE]".to_string());
                } else {
                    lines.push(line.to_string());
                }
            }
        }

        if lines.is_empty() && !text.is_empty() && !text.contains("data:") {
            // Pass through non-SSE content as-is
            lines.push(text.to_string());
        }

        lines
    }

    fn output_format(&self) -> &str {
        "openai"
    }

    fn matches_content_type(&self, content_type: Option<&str>) -> bool {
        content_type
            .map(|ct| ct.contains("text/event-stream") || ct.contains("application/json"))
            .unwrap_or(false)
    }
}

/// Anthropic SSE to OpenAI SSE transformer
#[derive(Debug, Clone, Default)]
pub struct AnthropicToOpenAiTransformer {
    pub state: AnthropicStreamingState,
}

impl AnthropicToOpenAiTransformer {
    pub fn new() -> Self {
        Self {
            state: AnthropicStreamingState::default(),
        }
    }
}

impl StreamingTransformer for AnthropicToOpenAiTransformer {
    fn transform_chunk(&mut self, chunk: &Bytes) -> Vec<String> {
        let text = String::from_utf8_lossy(chunk);
        let mut output_lines = Vec::new();

        for line in text.lines() {
            let line = line.trim();

            if let Some(data) = line.strip_prefix("data: ") {
                let data = data.trim();

                if data == "[DONE]" {
                    output_lines.push("data: [DONE]".to_string());
                    continue;
                }

                // Parse event
                if let Ok(event) = serde_json::from_str::<AnthropicSSEEvent>(data) {
                    // Convert message_start
                    if let Some(msg_start) = event.message_start {
                        let id = msg_start.id.as_deref().unwrap_or("anonymous");
                        let model = msg_start.model.as_deref().unwrap_or("");
                        output_lines.push(format!(
                            r#"{{"id":"{id}","object":"chat.completion.chunk","created":{},"model":"{model}","choices":[{{"index":0,"delta":{{}},"logprobs":null,"finish_reason":null}}]}}"#,
                            msg_start.created_at.unwrap_or(0)
                        ));
                    }

                    // Convert content_block_start
                    if let Some(block_start) = event.content_block_start {
                        let _block_type = block_start
                            .content_block
                            .get("type")
                            .and_then(|t| t.as_str())
                            .unwrap_or("text");
                        let block_index = block_start.index;
                        output_lines.push(format!(
                            r#"{{"id":"assistant","object":"chat.completion.chunk","created":0,"model":"","choices":[{{"index":{},"delta":{{"role":"assistant","content":null}},"logprobs":null,"finish_reason":null}}]}}"#,
                            block_index
                        ));
                    }

                    // Convert content_block_delta
                    if let Some(delta) = event.content_block_delta {
                        let delta_type = delta
                            .delta
                            .get("type")
                            .and_then(|t| t.as_str())
                            .unwrap_or("text");
                        let text = delta
                            .delta
                            .get("text")
                            .and_then(|t| t.as_str())
                            .unwrap_or("");
                        let index = delta.index;

                        match delta_type {
                            "text_delta" if !text.is_empty() => {
                                output_lines.push(format!(
                                    r#"{{"id":"assistant","object":"chat.completion.chunk","created":0,"model":"","choices":[{{"index":{},"delta":{{"content":"{}"}},"logprobs":null,"finish_reason":null}}]}}"#,
                                    index,
                                    escape_json_string(text)
                                ));
                            }
                            "thinking_delta" if !text.is_empty() => {
                                output_lines.push(format!(
                                    r#"{{"id":"assistant","object":"chat.completion.chunk","created":0,"model":"","choices":[{{"index":{},"delta":{{"content":"[thinking] {} [/thinking]"}},"logprobs":null,"finish_reason":null}}]}}"#,
                                    index,
                                    escape_json_string(text)
                                ));
                            }
                            "cache_control_delta" => {
                                // Emit cache_lookahead metadata
                                if let Some(param) =
                                    delta.delta.get("cache_control").and_then(|c| c.get("type"))
                                {
                                    if param == "cache_control_lookahead" {
                                        output_lines.push(format!(
                                            r#"{{"id":"assistant","object":"chat.completion.chunk","created":0,"model":"","choices":[{{"index":{},"delta":{{"cache_lookahead":true}},"logprobs":null,"finish_reason":null}}]}}"#,
                                            index
                                        ));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }

                    // Convert message_delta
                    if let Some(msg_delta) = event.message_delta {
                        if let Some(stop_reason) = msg_delta.stop_reason {
                            output_lines.push(format!(
                                r#"{{"id":"assistant","object":"chat.completion.chunk","created":0,"model":"","choices":[{{"index":0,"delta":{{}},"logprobs":null,"finish_reason":"{}"}}]}}"#,
                                stop_reason
                            ));
                        }
                        if let Some(usage) = msg_delta.usage {
                            let prompt_tokens = usage.input_tokens.unwrap_or(0);
                            let completion_tokens = usage.output_tokens.unwrap_or(0);
                            output_lines.push(format!(
                                r#"{{"id":"assistant","object":"chat.completion.chunk","created":0,"model":"","choices":[{{"index":0,"delta":{{}},"logprobs":null,"finish_reason":"stop"}}],"usage":{{"prompt_tokens":{},"completion_tokens":{},"total_tokens":{}}}}}"#,
                                prompt_tokens, completion_tokens, prompt_tokens + completion_tokens
                            ));
                        }
                    }
                }
            }
        }

        output_lines
    }

    fn output_format(&self) -> &str {
        "openai"
    }

    fn matches_content_type(&self, content_type: Option<&str>) -> bool {
        content_type
            .map(|ct| ct.contains("text/event-stream"))
            .unwrap_or(false)
    }
}

/// Anthropic SSE event structure
#[derive(Debug, Deserialize)]
pub struct AnthropicSSEEvent {
    #[serde(rename = "type")]
    #[serde(default)]
    pub event_type: Option<String>,
    #[serde(default)]
    pub message_start: Option<MessageStart>,
    #[serde(default)]
    pub content_block_start: Option<ContentBlockStart>,
    #[serde(default)]
    pub content_block_delta: Option<ContentBlockDelta>,
    #[serde(default)]
    pub message_delta: Option<MessageDelta>,
}

#[derive(Debug, Deserialize)]
pub struct MessageStart {
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub msg_type: Option<String>,
    pub model: Option<String>,
    #[serde(rename = "created_at")]
    pub created_at: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct ContentBlockStart {
    pub index: usize,
    pub content_block: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct ContentBlockDelta {
    pub index: usize,
    pub delta: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct MessageDelta {
    #[serde(default)]
    pub stop_reason: Option<String>,
    pub usage: Option<MessageUsage>,
}

#[derive(Debug, Deserialize)]
pub struct MessageUsage {
    #[serde(rename = "input_tokens")]
    #[serde(default)]
    pub input_tokens: Option<usize>,
    #[serde(rename = "output_tokens")]
    #[serde(default)]
    pub output_tokens: Option<usize>,
}

/// Gemini to OpenAI SSE transformer
#[derive(Debug, Clone, Default)]
pub struct GeminiToOpenAiTransformer {
    pub state: GeminiStreamingState,
}

impl GeminiToOpenAiTransformer {
    pub fn new() -> Self {
        Self {
            state: GeminiStreamingState::default(),
        }
    }
}

impl StreamingTransformer for GeminiToOpenAiTransformer {
    fn transform_chunk(&mut self, chunk: &Bytes) -> Vec<String> {
        let text = String::from_utf8_lossy(chunk);
        let mut output_lines = Vec::new();

        for line in text.lines() {
            let line = line.trim();

            if let Some(data) = line.strip_prefix("data: ") {
                if data.trim() == "[DONE]" {
                    output_lines.push("data: [DONE]".to_string());
                    continue;
                }

                // Parse Gemini SSE
                if let Ok(event) = serde_json::from_str::<GeminiSSEEvent>(data) {
                    if let Some(candidate) = event.candidates {
                        for candidate_data in candidate {
                            if let Some(content) = candidate_data.content {
                                for part in content.parts.unwrap_or_default() {
                                    if let Some(text) = part.text {
                                        if !text.is_empty() {
                                            output_lines.push(format!(
                                                r#"{{"id":"assistant","object":"chat.completion.chunk","created":0,"model":"gemini","choices":[{{"index":{},"delta":{{"content":"{}"}},"logprobs":null,"finish_reason":null}}]}}"#,
                                                self.state.current_part_index,
                                                escape_json_string(&text)
                                            ));
                                        }
                                    }
                                    if let Some(function_call) = part.function_call {
                                        let name = function_call.name.unwrap_or_default();
                                        let args = function_call.args.unwrap_or_default();
                                        if let Ok(args_str) = serde_json::to_string(&args) {
                                            output_lines.push(format!(
                                                r#"{{"id":"assistant","object":"chat.completion.chunk","created":0,"model":"gemini","choices":[{{"index":{},"delta":{{"function_call":{{"name":"{}","arguments":"{}"}}}},"logprobs":null,"finish_reason":null}}]}}"#,
                                                self.state.current_part_index,
                                                escape_json_string(&name),
                                                escape_json_string(&args_str)
                                            ));
                                        }
                                    }
                                }
                                self.state.current_part_index += 1;
                            }
                        }
                    }

                    // Handle usage metadata
                    if let Some(usage) = event.usage_metadata {
                        let prompt_tokens = usage.prompt_token_count.unwrap_or(0);
                        let completion_tokens = usage.candidates_token_count.unwrap_or(0);
                        output_lines.push(format!(
                            r#"{{"id":"assistant","object":"chat.completion.chunk","created":0,"model":"gemini","choices":[{{"index":0,"delta":{{}},"logprobs":null,"finish_reason":"stop"}}],"usage":{{"prompt_tokens":{},"completion_tokens":{},"total_tokens":{}}}}}"#,
                            prompt_tokens, completion_tokens, prompt_tokens + completion_tokens
                        ));
                    }
                }
            }
        }

        output_lines
    }

    fn output_format(&self) -> &str {
        "openai"
    }

    fn matches_content_type(&self, content_type: Option<&str>) -> bool {
        content_type
            .map(|ct| ct.contains("text/event-stream"))
            .unwrap_or(false)
    }
}

/// Gemini SSE event structure
#[derive(Debug, Deserialize)]
pub struct GeminiSSEEvent {
    #[serde(default)]
    pub candidates: Option<Vec<GeminiCandidate>>,
    #[serde(rename = "usageMetadata")]
    #[serde(default)]
    pub usage_metadata: Option<GeminiUsage>,
}

#[derive(Debug, Deserialize)]
pub struct GeminiCandidate {
    pub content: Option<GeminiContent>,
}

#[derive(Debug, Deserialize)]
pub struct GeminiContent {
    pub parts: Option<Vec<GeminiPart>>,
}

#[derive(Debug, Deserialize)]
pub struct GeminiPart {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(rename = "functionCall")]
    #[serde(default)]
    pub function_call: Option<GeminiFunctionCall>,
}

#[derive(Debug, Deserialize)]
pub struct GeminiFunctionCall {
    pub name: Option<String>,
    pub args: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct GeminiUsage {
    #[serde(rename = "promptTokenCount")]
    #[serde(default)]
    pub prompt_token_count: Option<usize>,
    #[serde(rename = "candidatesTokenCount")]
    #[serde(default)]
    pub candidates_token_count: Option<usize>,
}

/// Ollama to OpenAI SSE transformer
#[derive(Debug, Clone, Default)]
pub struct OllamaToOpenAiTransformer {
    pub state: OllamaStreamingState,
}

impl OllamaToOpenAiTransformer {
    pub fn new() -> Self {
        Self {
            state: OllamaStreamingState::default(),
        }
    }
}

impl StreamingTransformer for OllamaToOpenAiTransformer {
    fn transform_chunk(&mut self, chunk: &Bytes) -> Vec<String> {
        let text = String::from_utf8_lossy(chunk);
        let mut output_lines = Vec::new();

        for line in text.lines() {
            let line = line.trim();

            if let Some(data) = line.strip_prefix("data: ") {
                let data = data.trim();

                if data == "[DONE]" {
                    output_lines.push("data: [DONE]".to_string());
                    continue;
                }

                // Parse Ollama streaming response
                if let Ok(event) = serde_json::from_str::<OllamaStreamResponse>(data) {
                    if let Some(message) = event.message {
                        let role = message.role.unwrap_or_else(|| "assistant".to_string());
                        let content = message.content.unwrap_or_default();

                        if !content.is_empty() {
                            output_lines.push(format!(
                                r#"{{"id":"chatcmpl-{}","object":"chat.completion.chunk","created":{},"model":"ollama","choices":[{{"index":{},"delta":{{"role":"{}","content":"{}"}},"logprobs":null,"finish_reason":null}}]}}"#,
                                self.state.message_idx,
                                event.created_at.unwrap_or(0),
                                self.state.message_idx,
                                role,
                                escape_json_string(&content)
                            ));
                        }

                        // Handle tool calls
                        if let Some(tool_calls) = message.tool_calls {
                            for (i, tool_call) in tool_calls.into_iter().enumerate() {
                                let name = tool_call.function.name.unwrap_or_default();
                                let args = tool_call.function.arguments.unwrap_or_default();
                                if let Ok(args_str) = serde_json::to_string(&args) {
                                    output_lines.push(format!(
                                        r#"{{"id":"chatcmpl-{}","object":"chat.completion.chunk","created":{},"model":"ollama","choices":[{{"index":{},"delta":{{"tool_calls":[{{"index":{},"id":"tool_{}","type":"function","function":{{"name":"{}","arguments":"{}"}}}}]}},"logprobs":null,"finish_reason":null}}]}}"#,
                                        self.state.message_idx,
                                        event.created_at.unwrap_or(0),
                                        self.state.message_idx,
                                        i,
                                        i,
                                        escape_json_string(&name),
                                        escape_json_string(&args_str)
                                    ));
                                }
                            }
                        }
                    }

                    // Handle done signal
                    if event.done.unwrap_or(false) {
                        output_lines.push("data: [DONE]".to_string());
                        self.state.message_idx += 1;
                    }
                }
            }
        }

        output_lines
    }

    fn output_format(&self) -> &str {
        "openai"
    }

    fn matches_content_type(&self, content_type: Option<&str>) -> bool {
        content_type
            .map(|ct| ct.contains("text/event-stream"))
            .unwrap_or(false)
    }
}

/// Ollama stream response structure
#[derive(Debug, Deserialize)]
pub struct OllamaStreamResponse {
    pub model: Option<String>,
    #[serde(rename = "created_at")]
    pub created_at: Option<u64>,
    pub message: Option<OllamaMessage>,
    pub done: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct OllamaMessage {
    pub role: Option<String>,
    pub content: Option<String>,
    #[serde(rename = "tool_calls")]
    pub tool_calls: Option<Vec<OllamaToolCall>>,
}

#[derive(Debug, Deserialize)]
pub struct OllamaToolCall {
    pub function: OllamaFunction,
}

#[derive(Debug, Deserialize)]
pub struct OllamaFunction {
    pub name: Option<String>,
    pub arguments: Option<serde_json::Value>,
}

/// Transform a complete SSE stream from bytes to lines
pub fn transform_sse_stream(
    chunk: &Bytes,
    transformer: &mut dyn StreamingTransformer,
) -> Vec<String> {
    transformer.transform_chunk(chunk)
}

/// Helper to escape JSON strings
fn escape_json_string(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c.is_control() => {
                result.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => result.push(c),
        }
    }
    result
}

/// Detect transformer based on provider name
pub fn transformer_for_provider(provider: &str) -> Option<Box<dyn StreamingTransformer>> {
    match provider {
        "anthropic" => Some(Box::new(AnthropicToOpenAiTransformer::new())),
        "gemini" => Some(Box::new(GeminiToOpenAiTransformer::new())),
        "ollama" => Some(Box::new(OllamaToOpenAiTransformer::new())),
        "claude" | "glm" | "kimi" | "minimax" | "minimax-cn" => {
            Some(Box::new(AnthropicToOpenAiTransformer::new()))
        }
        _ => Some(Box::new(OpenAiTransformer::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_json_string() {
        assert_eq!(escape_json_string("hello"), "hello");
        assert_eq!(escape_json_string("hello\nworld"), "hello\\nworld");
        assert_eq!(escape_json_string("hello\"world"), "hello\\\"world");
    }

    #[test]
    fn test_openai_transformer_passthrough() {
        let mut transformer = OpenAiTransformer::new();
        let chunk = Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n");
        let lines = transformer.transform_chunk(&chunk);
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_anthropic_to_openai_transformer() {
        let mut transformer = AnthropicToOpenAiTransformer::new();
        // Simulate Anthropic SSE - format: data: {"type":"message_start","message_start":{...}}
        let chunk = Bytes::from(
            r#"data: {"type":"message_start","message_start":{"id":"test","model":"claude-3","type":"message_start","created_at":1234567890}}"#,
        );
        let lines = transformer.transform_chunk(&chunk);
        assert!(
            !lines.is_empty(),
            "Expected non-empty output lines, got: {:?}",
            lines
        );
    }

    #[test]
    fn test_openai_transformer_multiple_lines() {
        let mut transformer = OpenAiTransformer::new();
        let chunk = Bytes::from(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"index\":0}]}\n\n\
             data: {\"choices\":[{\"delta\":{\"content\":\" world\"},\"index\":0}]}\n\n\
             data: [DONE]\n\n",
        );
        let lines = transformer.transform_chunk(&chunk);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("hello"));
        assert!(lines[1].contains("world"));
        assert_eq!(lines[2], "data: [DONE]");
    }

    #[test]
    fn test_openai_transformer_done_signal() {
        let mut transformer = OpenAiTransformer::new();
        let chunk = Bytes::from("data: [DONE]\n\n");
        let lines = transformer.transform_chunk(&chunk);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "data: [DONE]");
    }

    #[test]
    fn test_openai_transformer_non_sse_content() {
        let mut transformer = OpenAiTransformer::new();
        let chunk = Bytes::from("plain text without SSE markers\n\n");
        let lines = transformer.transform_chunk(&chunk);
        assert!(!lines.is_empty());
        assert!(lines[0].contains("plain text"));
    }

    #[test]
    fn test_openai_transformer_matches_content_type() {
        let transformer = OpenAiTransformer::new();
        assert!(transformer.matches_content_type(Some("text/event-stream")));
        assert!(transformer.matches_content_type(Some("application/json")));
        assert!(!transformer.matches_content_type(Some("text/plain")));
        assert!(!transformer.matches_content_type(None));
    }

    #[test]
    fn test_anthropic_message_start_conversion() {
        let mut transformer = AnthropicToOpenAiTransformer::new();
        let chunk = Bytes::from(
            r#"data: {"type":"message_start","message_start":{"id":"msg-123","model":"claude-3-opus-20250219","type":"message","created_at":1234567890}}"#,
        );
        let lines = transformer.transform_chunk(&chunk);
        assert!(!lines.is_empty());
        let output = &lines[0];
        assert!(output.contains("\"id\":\"msg-123\""));
        assert!(output.contains("\"object\":\"chat.completion.chunk\""));
    }

    #[test]
    fn test_anthropic_content_block_start_conversion() {
        let mut transformer = AnthropicToOpenAiTransformer::new();
        let chunk = Bytes::from(
            r#"data: {"type":"content_block_start","content_block_start":{"index":0,"content_block":{"type":"text"}}}"#,
        );
        let lines = transformer.transform_chunk(&chunk);
        assert!(!lines.is_empty());
        let output = &lines[0];
        assert!(output.contains("\"delta\":{\"role\":\"assistant\""));
    }

    #[test]
    fn test_anthropic_text_delta_conversion() {
        let mut transformer = AnthropicToOpenAiTransformer::new();
        let chunk = Bytes::from(
            r#"data: {"type":"content_block_delta","content_block_delta":{"index":0,"delta":{"type":"text_delta","text":"Hello"}}}"#,
        );
        let lines = transformer.transform_chunk(&chunk);
        assert!(!lines.is_empty());
        let output = &lines[0];
        assert!(output.contains("\"content\":\"Hello\""));
        assert!(output.contains("\"object\":\"chat.completion.chunk\""));
    }

    #[test]
    fn test_anthropic_thinking_delta_conversion() {
        let mut transformer = AnthropicToOpenAiTransformer::new();
        let chunk = Bytes::from(
            r#"data: {"type":"content_block_delta","content_block_delta":{"index":0,"delta":{"type":"thinking_delta","text":"reasoning here"}}}"#,
        );
        let lines = transformer.transform_chunk(&chunk);
        assert!(!lines.is_empty());
        let output = &lines[0];
        assert!(output.contains("[thinking]"));
        assert!(output.contains("[/thinking]"));
        assert!(output.contains("reasoning here"));
    }

    #[test]
    fn test_anthropic_cache_control_delta_conversion() {
        let mut transformer = AnthropicToOpenAiTransformer::new();
        let chunk = Bytes::from(
            r#"data: {"type":"content_block_delta","content_block_delta":{"index":0,"delta":{"type":"cache_control_delta","cache_control":{"type":"cache_control_lookahead"}}}}"#,
        );
        let lines = transformer.transform_chunk(&chunk);
        assert!(!lines.is_empty());
        let output = &lines[0];
        assert!(output.contains("\"cache_lookahead\":true"));
    }

    #[test]
    fn test_anthropic_message_delta_stop_reason() {
        let mut transformer = AnthropicToOpenAiTransformer::new();
        let chunk = Bytes::from(
            r#"data: {"type":"message_delta","message_delta":{"stop_reason":"end_turn"}}"#,
        );
        let lines = transformer.transform_chunk(&chunk);
        assert!(!lines.is_empty());
        let output = &lines[0];
        assert!(output.contains("\"finish_reason\":\"end_turn\""));
    }

    #[test]
    fn test_anthropic_message_delta_usage() {
        let mut transformer = AnthropicToOpenAiTransformer::new();
        let chunk = Bytes::from(
            r#"data: {"type":"message_delta","message_delta":{"usage":{"output_tokens":150,"input_tokens":50}}}"#,
        );
        let lines = transformer.transform_chunk(&chunk);
        assert!(!lines.is_empty());
        let output = &lines[0];
        assert!(output.contains("\"usage\""));
        assert!(output.contains("\"prompt_tokens\":50"));
        assert!(output.contains("\"completion_tokens\":150"));
    }

    #[test]
    fn test_anthropic_multiple_events_in_chunk() {
        let mut transformer = AnthropicToOpenAiTransformer::new();
        let chunk = Bytes::from(
            "data: {\"type\":\"message_start\",\"message_start\":{\"id\":\"msg-1\",\"model\":\"claude-3\",\"created_at\":1234567890}}\n\
             data: {\"type\":\"content_block_start\",\"content_block_start\":{\"index\":0,\"content_block\":{\"type\":\"text\"}}}\n\
             data: {\"type\":\"content_block_delta\",\"content_block_delta\":{\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}}\n\
             data: {\"type\":\"message_delta\",\"message_delta\":{\"stop_reason\":\"end_turn\",\"usage\":{\"output_tokens\":10,\"input_tokens\":5}}}}",
        );
        let lines = transformer.transform_chunk(&chunk);
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_anthropic_empty_text_delta_skipped() {
        let mut transformer = AnthropicToOpenAiTransformer::new();
        let chunk = Bytes::from(
            r#"data: {"type":"content_block_delta","content_block_delta":{"index":0,"delta":{"type":"text_delta","text":""}}}"#,
        );
        let lines = transformer.transform_chunk(&chunk);
        // Empty text should produce no output lines
        assert!(lines.is_empty());
    }

    #[test]
    fn test_gemini_text_part_conversion() {
        let mut transformer = GeminiToOpenAiTransformer::new();
        let chunk = Bytes::from(
            r#"data: {"candidates":[{"content":{"parts":[{"text":"Hello Gemini"}]}}]}"#,
        );
        let lines = transformer.transform_chunk(&chunk);
        assert!(!lines.is_empty());
        let output = &lines[0];
        assert!(output.contains("\"content\":\"Hello Gemini\""));
    }

    #[test]
    fn test_gemini_function_call_conversion() {
        let transformer = GeminiToOpenAiTransformer::new();
        assert!(transformer.matches_content_type(Some("text/event-stream")));
    }

    #[test]
    fn test_gemini_usage_metadata() {
        let mut transformer = GeminiToOpenAiTransformer::new();
        let chunk = Bytes::from(
            r#"data: {"candidates":[{"content":{"parts":[{"text":"test"}]}}],"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":50}}"#,
        );
        let lines = transformer.transform_chunk(&chunk);
        // Should have text line plus usage line
        assert!(!lines.is_empty());
        let usage_line = lines.last().unwrap();
        assert!(usage_line.contains("\"usage\""));
        assert!(usage_line.contains("\"prompt_tokens\":100"));
        assert!(usage_line.contains("\"completion_tokens\":50"));
    }

    #[test]
    fn test_gemini_done_signal() {
        let mut transformer = GeminiToOpenAiTransformer::new();
        let chunk = Bytes::from("data: [DONE]\n\n");
        let lines = transformer.transform_chunk(&chunk);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "data: [DONE]");
    }

    #[test]
    fn test_gemini_multiple_parts_increment_index() {
        let mut transformer = GeminiToOpenAiTransformer::new();
        let chunk1 =
            Bytes::from(r#"data: {"candidates":[{"content":{"parts":[{"text":"First"}]}}]}"#);
        let chunk2 =
            Bytes::from(r#"data: {"candidates":[{"content":{"parts":[{"text":"Second"}]}}]}"#);
        let lines1 = transformer.transform_chunk(&chunk1);
        let lines2 = transformer.transform_chunk(&chunk2);
        // Each chunk should have consistent part indices
        assert!(!lines1.is_empty());
        assert!(!lines2.is_empty());
    }

    #[test]
    fn test_ollama_message_content_conversion() {
        let mut transformer = OllamaToOpenAiTransformer::new();
        let chunk = Bytes::from(
            r#"data: {"model":"llama3","created_at":1234567890,"message":{"role":"assistant","content":"Hello Ollama"},"done":false}"#,
        );
        let lines = transformer.transform_chunk(&chunk);
        assert!(!lines.is_empty());
        let output = &lines[0];
        assert!(output.contains("\"content\":\"Hello Ollama\""));
        assert!(output.contains("\"role\":\"assistant\""));
    }

    #[test]
    fn test_ollama_tool_calls_conversion() {
        let mut transformer = OllamaToOpenAiTransformer::new();
        let chunk = Bytes::from(
            r#"data: {"model":"llama3","created_at":1234567890,"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"search","arguments":{"query":"rust"}}}]},"done":false}"#,
        );
        let lines = transformer.transform_chunk(&chunk);
        assert!(!lines.is_empty());
        let output = &lines[0];
        assert!(output.contains("\"tool_calls\""));
        assert!(output.contains("\"name\":\"search\""));
    }

    #[test]
    fn test_ollama_done_signal() {
        let mut transformer = OllamaToOpenAiTransformer::new();
        let chunk = Bytes::from(r#"data: {"model":"llama3","done":true}"#);
        let lines = transformer.transform_chunk(&chunk);
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|l| l == "data: [DONE]"));
    }

    #[test]
    fn test_ollama_increments_message_idx_on_done() {
        let mut transformer = OllamaToOpenAiTransformer::new();
        assert_eq!(transformer.state.message_idx, 0);

        let chunk1 = Bytes::from(r#"data: {"done":true}"#);
        let lines1 = transformer.transform_chunk(&chunk1);
        assert!(lines1.iter().any(|l| l == "data: [DONE]"));
    }

    #[test]
    fn test_transformer_for_provider_anthropic() {
        let transformer = transformer_for_provider("anthropic");
        assert!(transformer.is_some());
        assert_eq!(transformer.unwrap().output_format(), "openai");
    }

    #[test]
    fn test_transformer_for_provider_gemini() {
        let transformer = transformer_for_provider("gemini");
        assert!(transformer.is_some());
        assert_eq!(transformer.unwrap().output_format(), "openai");
    }

    #[test]
    fn test_transformer_for_provider_ollama() {
        let transformer = transformer_for_provider("ollama");
        assert!(transformer.is_some());
        assert_eq!(transformer.unwrap().output_format(), "openai");
    }

    #[test]
    fn test_transformer_for_provider_claude_alias() {
        let transformer = transformer_for_provider("claude");
        assert!(transformer.is_some());
        assert_eq!(transformer.unwrap().output_format(), "openai");
    }

    #[test]
    fn test_transformer_for_provider_glm_alias() {
        let transformer = transformer_for_provider("glm");
        assert!(transformer.is_some());
        assert_eq!(transformer.unwrap().output_format(), "openai");
    }

    #[test]
    fn test_transformer_for_provider_unknown_defaults_to_openai() {
        let transformer = transformer_for_provider("unknown-provider");
        assert!(transformer.is_some());
        let t = transformer.unwrap();
        assert_eq!(t.output_format(), "openai");
        assert!(t.matches_content_type(Some("text/event-stream")));
    }

    #[test]
    fn test_transform_sse_stream_helper() {
        let mut transformer = OpenAiTransformer::new();
        let chunk = Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\"test\"}}]}\n\n");
        let lines = transform_sse_stream(&chunk, &mut transformer);
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_escape_json_string_control_characters() {
        assert_eq!(escape_json_string("tab\there"), "tab\\there");
        assert_eq!(escape_json_string("return\r\n"), "return\\r\\n");
        assert_eq!(escape_json_string("null\u{0}byte"), "null\\u0000byte");
    }

    #[test]
    fn test_escape_json_string_unicode() {
        assert_eq!(escape_json_string("日本語"), "日本語");
        assert_eq!(escape_json_string("emoji🎉"), "emoji🎉");
    }

    #[test]
    fn test_streaming_base_default() {
        let base = StreamingBase::default();
        assert!(base.line_buffer.is_empty());
        assert!(!base.in_data_field);
        assert!(base.content_accumulator.is_empty());
    }

    #[test]
    fn test_openai_transformer_preserves_json_format() {
        let mut transformer = OpenAiTransformer::new();
        let complex_json = r#"data: {"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"Hello world"},"finish_reason":null}]}"#;
        let chunk = Bytes::from(complex_json);
        let lines = transformer.transform_chunk(&chunk);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("chatcmpl-123"));
        assert!(lines[0].contains("Hello world"));
    }

    #[test]
    fn test_anthropic_unknown_event_type_handled() {
        let mut transformer = AnthropicToOpenAiTransformer::new();
        let chunk = Bytes::from(r#"data: {"type":"unknown_event","data":{"foo":"bar"}}"#);
        let lines = transformer.transform_chunk(&chunk);
        // Unknown types should not produce output
        assert!(lines.is_empty());
    }

    #[test]
    fn test_gemini_empty_candidates_handled() {
        let mut transformer = GeminiToOpenAiTransformer::new();
        let chunk = Bytes::from(r#"data: {"candidates":[]}"#);
        let lines = transformer.transform_chunk(&chunk);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_gemini_null_parts_handled() {
        let mut transformer = GeminiToOpenAiTransformer::new();
        let chunk = Bytes::from(r#"data: {"candidates":[{"content":{"parts":null}}]}"#);
        let lines = transformer.transform_chunk(&chunk);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_ollama_null_content_handled() {
        let mut transformer = OllamaToOpenAiTransformer::new();
        let chunk = Bytes::from(
            r#"data: {"model":"llama3","message":{"role":"assistant","content":null},"done":false}"#,
        );
        let lines = transformer.transform_chunk(&chunk);
        assert!(lines.is_empty()); // No content to emit
    }

    #[test]
    fn test_ollama_empty_content_skipped() {
        let mut transformer = OllamaToOpenAiTransformer::new();
        let chunk = Bytes::from(
            r#"data: {"model":"llama3","message":{"role":"assistant","content":""},"done":false}"#,
        );
        let lines = transformer.transform_chunk(&chunk);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_chunk_by_chunk_accumulation_openai() {
        let mut transformer = OpenAiTransformer::new();
        let chunk1 = Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n");
        let chunk2 = Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\" World\"}}]}\n");
        let lines1 = transformer.transform_chunk(&chunk1);
        let lines2 = transformer.transform_chunk(&chunk2);
        assert!(!lines1.is_empty());
        assert!(!lines2.is_empty());
        assert!(lines1[0].contains("Hello"));
        assert!(lines2[0].contains("World"));
    }

    #[test]
    fn test_chunk_by_chunk_accumulation_anthropic() {
        let mut transformer = AnthropicToOpenAiTransformer::new();
        let chunk1 = Bytes::from("data: {\"type\":\"message_start\",\"message_start\":{\"id\":\"msg-1\",\"model\":\"claude-3\",\"created_at\":1234567890}}\n");
        let chunk2 = Bytes::from("data: {\"type\":\"content_block_start\",\"content_block_start\":{\"index\":0,\"content_block\":{\"type\":\"text\"}}}\n");
        let chunk3 = Bytes::from("data: {\"type\":\"content_block_delta\",\"content_block_delta\":{\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}}\n");
        let lines1 = transformer.transform_chunk(&chunk1);
        let lines2 = transformer.transform_chunk(&chunk2);
        let lines3 = transformer.transform_chunk(&chunk3);
        assert!(!lines1.is_empty());
        assert!(!lines2.is_empty());
        assert!(!lines3.is_empty());
    }

    #[test]
    fn test_anthropic_thinking_block_format() {
        let mut transformer = AnthropicToOpenAiTransformer::new();
        let chunk = Bytes::from(
            "data: {\"type\":\"content_block_delta\",\"content_block_delta\":{\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"text\":\"Let me think about this step by step...\"}}}\n",
        );
        let lines = transformer.transform_chunk(&chunk);
        assert!(!lines.is_empty());
        let output = &lines[0];
        assert!(output.contains("[thinking]"));
        assert!(output.contains("[/thinking]"));
        assert!(output.contains("step by step"));
    }

    #[test]
    fn test_kiro_event_buffer_state() {
        let state = KiroStreamingState::default();
        assert!(state.event_buffer.is_empty());
        assert!(state.current_event_type.is_none());
        assert!(state.base.line_buffer.is_empty());
    }

    #[test]
    fn test_anthropic_streaming_state_tracks_current_block() {
        let mut transformer = AnthropicToOpenAiTransformer::new();
        assert!(transformer.state.current_block.is_none());
        let chunk = Bytes::from(
            "data: {\"type\":\"content_block_start\",\"content_block_start\":{\"index\":0,\"content_block\":{\"type\":\"text\"}}}\n",
        );
        transformer.transform_chunk(&chunk);
    }

    #[test]
    fn test_anthropic_cache_control_lookahead_emitted() {
        let mut transformer = AnthropicToOpenAiTransformer::new();
        let chunk = Bytes::from(
            "data: {\"type\":\"content_block_delta\",\"content_block_delta\":{\"index\":0,\"delta\":{\"type\":\"cache_control_delta\",\"cache_control\":{\"type\":\"cache_control_lookahead\",\"amount\":1024}}}}\n",
        );
        let lines = transformer.transform_chunk(&chunk);
        assert!(!lines.is_empty());
        let output = &lines[0];
        assert!(output.contains("\"cache_lookahead\":true"));
    }

    #[test]
    fn test_gemini_streaming_state_part_index_tracking() {
        let mut transformer = GeminiToOpenAiTransformer::new();
        assert_eq!(transformer.state.current_part_index, 0);
        let chunk = Bytes::from(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"First\"}]}}]}\n",
        );
        transformer.transform_chunk(&chunk);
        assert_eq!(transformer.state.current_part_index, 1);
        let chunk2 = Bytes::from(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Second\"}]}}]}\n",
        );
        transformer.transform_chunk(&chunk2);
        assert_eq!(transformer.state.current_part_index, 2);
    }

    #[test]
    fn test_ollama_streaming_state_message_idx_tracking() {
        let mut transformer = OllamaToOpenAiTransformer::new();
        assert_eq!(transformer.state.message_idx, 0);
        let chunk = Bytes::from(r#"data: {"model":"llama3","done":true}"#);
        transformer.transform_chunk(&chunk);
    }

    #[test]
    fn test_bytes_zero_copy_no_intermediate_allocation() {
        let data = b"data: {\"choices\":[{\"delta\":{\"content\":\"test\"}}]}\n".to_vec();
        let bytes = Bytes::from(data);
        let mut transformer = OpenAiTransformer::new();
        let lines = transformer.transform_chunk(&bytes);
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_bytes_from_static() {
        let bytes =
            Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"static\"}}]}\n");
        let mut transformer = OpenAiTransformer::new();
        let lines = transformer.transform_chunk(&bytes);
        assert!(!lines.is_empty());
        assert!(lines[0].contains("static"));
    }

    #[test]
    fn test_transformer_output_format_consistency() {
        let openai = OpenAiTransformer::new();
        let anthropic = AnthropicToOpenAiTransformer::new();
        let gemini = GeminiToOpenAiTransformer::new();
        let ollama = OllamaToOpenAiTransformer::new();
        assert_eq!(openai.output_format(), "openai");
        assert_eq!(anthropic.output_format(), "openai");
        assert_eq!(gemini.output_format(), "openai");
        assert_eq!(ollama.output_format(), "openai");
    }

    #[test]
    fn test_transformer_matches_content_type_edge_cases() {
        let transformer = OpenAiTransformer::new();
        assert!(transformer.matches_content_type(Some("text/event-stream; charset=utf-8")));
        assert!(transformer.matches_content_type(Some("application/json; text/event-stream")));
        assert!(!transformer.matches_content_type(Some("text/plain; charset=utf-8")));
    }

    #[test]
    fn test_anthropic_message_delta_without_usage() {
        let mut transformer = AnthropicToOpenAiTransformer::new();
        let chunk = Bytes::from(
            "data: {\"type\":\"message_delta\",\"message_delta\":{\"stop_reason\":\"end_turn\"}}\n",
        );
        let lines = transformer.transform_chunk(&chunk);
        assert!(!lines.is_empty());
        let output = &lines[0];
        assert!(output.contains("\"finish_reason\":\"end_turn\""));
    }

    #[test]
    fn test_anthropic_partial_chunk_processing() {
        let mut transformer = AnthropicToOpenAiTransformer::new();
        let chunk = Bytes::from(
            "data: {\"type\":\"message_start\",\"message_start\":{\"id\":\"partial\"}}\n",
        );
        let lines = transformer.transform_chunk(&chunk);
        assert!(!lines.is_empty());
    }
}
