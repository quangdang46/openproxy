//! Stream decision helpers — 9router `chatCore.js` stream flag parity.
//!
//! Single pure function used by chat so DeepSeek-TUI, Accept preference,
//! forceStream, and imageGen rules cannot drift from each other.

use crate::core::utils::client_detector::ClientTool;
use crate::core::translator::registry::Format;

/// Providers that force upstream streaming (9router registry forceStream: true).
/// Client may still receive JSON via SSE→JSON aggregation.
pub fn provider_requires_streaming(provider: &str) -> bool {
    matches!(
        provider,
        "codex"
            | "openai"
            | "commandcode"
            | "command-code"
            | "codebuddy-cn"
            | "grok-cli"
            | "gcli"
            | "gb"
    )
}

/// Resolved stream plan for a single request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamPlan {
    /// Whether to request stream=true from the provider.
    pub stream: bool,
    /// Client explicitly asked for streaming (body.stream===true or format).
    pub client_requested_streaming: bool,
    /// Provider registry forces stream.
    pub provider_forced: bool,
    /// Aggregate SSE to JSON for non-streaming clients on forceStream providers.
    pub sse_to_json: bool,
}

/// Resolve stream flags matching 9router chatCore.js order:
/// 1. forceStream provider → stream true
/// 2. else body.stream !== false (default true)
/// 3. imageGen + antigravity|gemini-cli → stream false
/// 4. deepseek-tui && stream !== true → stream false
/// 5. Accept json && !sse && stream !== true && !forceStream → stream false
pub fn resolve_stream_flags(
    body_stream: Option<bool>,
    accept: Option<&str>,
    provider: &str,
    model: &str,
    source_format: Format,
    client_tool: Option<ClientTool>,
    model_type: Option<&str>,
) -> StreamPlan {
    let provider_forced = provider_requires_streaming(provider);

    let client_requested_streaming = body_stream == Some(true)
        || matches!(
            source_format,
            Format::Antigravity | Format::Gemini | Format::GeminiCli
        );

    // Base: forceStream ? true : (stream !== false)
    let mut stream = if provider_forced {
        true
    } else {
        body_stream != Some(false)
    };

    // Image generation models require non-streaming (Google generateContent)
    let is_image_gen = model_type == Some("imageGen")
        || model_type == Some("image")
        || {
            let m = model.to_lowercase();
            m.contains("image") || m.contains("imagen") || m.contains("image-generation")
        };
    if is_image_gen && (provider == "antigravity" || provider == "gemini-cli") {
        stream = false;
    }

    // DeepSeek-TUI: only force non-stream when client did NOT set stream:true
    if client_tool == Some(ClientTool::DeepseekTui) && body_stream != Some(true) {
        stream = false;
    }

    // Accept: application/json preference (do not override explicit stream:true)
    if let Some(accept_val) = accept {
        let a = accept_val.to_lowercase();
        let wants_json = a.contains("application/json");
        let wants_sse = a.contains("text/event-stream");
        if wants_json && !wants_sse && body_stream != Some(true) && !provider_forced {
            stream = false;
        }
    }

    // When provider forces stream but client did not request streaming → SSE→JSON
    let sse_to_json = !client_requested_streaming && provider_forced;

    // Upstream still streams when sse_to_json (collect then convert)
    if sse_to_json {
        stream = true;
    }

    StreamPlan {
        stream,
        client_requested_streaming,
        provider_forced,
        sse_to_json,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_tui_respects_explicit_stream_true() {
        let p = resolve_stream_flags(
            Some(true),
            None,
            "openai",
            "gpt-4",
            Format::OpenAi,
            Some(ClientTool::DeepseekTui),
            None,
        );
        assert!(p.stream);
        assert!(p.client_requested_streaming);
    }

    #[test]
    fn deepseek_tui_forces_false_when_stream_absent() {
        // Use non-forceStream provider so DeepSeek-TUI non-stream is pure non-stream
        // (forceStream providers instead stream upstream + SSE→JSON).
        let p = resolve_stream_flags(
            None,
            None,
            "claude",
            "claude-sonnet-4",
            Format::Claude,
            Some(ClientTool::DeepseekTui),
            None,
        );
        assert!(!p.stream);
        assert!(!p.sse_to_json);
    }

    #[test]
    fn deepseek_tui_with_force_stream_uses_sse_to_json() {
        let p = resolve_stream_flags(
            None,
            None,
            "openai",
            "gpt-4",
            Format::OpenAi,
            Some(ClientTool::DeepseekTui),
            None,
        );
        // Upstream streams; client gets aggregated JSON (cannot parse SSE in -p mode)
        assert!(p.stream);
        assert!(p.sse_to_json);
    }

    #[test]
    fn accept_json_does_not_override_stream_true() {
        let p = resolve_stream_flags(
            Some(true),
            Some("application/json"),
            "openai",
            "gpt-4",
            Format::OpenAi,
            None,
            None,
        );
        // forceStream openai → stream true + sse_to_json false (client requested)
        assert!(p.stream);
        assert!(p.client_requested_streaming);
        assert!(!p.sse_to_json);
    }

    #[test]
    fn force_stream_sse_to_json_when_client_wants_json() {
        let p = resolve_stream_flags(
            Some(false),
            Some("application/json"),
            "codex",
            "o3",
            Format::OpenAi,
            None,
            None,
        );
        assert!(p.stream); // upstream streams
        assert!(p.sse_to_json);
        assert!(!p.client_requested_streaming);
    }

    #[test]
    fn image_gen_antigravity_non_stream() {
        let p = resolve_stream_flags(
            Some(true),
            None,
            "antigravity",
            "imagen-3",
            Format::Antigravity,
            None,
            Some("imageGen"),
        );
        assert!(!p.stream);
    }
}
