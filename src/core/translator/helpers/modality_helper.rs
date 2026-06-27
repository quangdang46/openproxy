//! Modality stripping — port of 9router `open-sse/translator/concerns/modality.js`
//!
//! Strips multimodal content blocks (vision/audio/pdf) that the target provider/model
//! cannot handle, BEFORE translation. Replaces removed blocks with text placeholders
//! so messages never become empty.

use serde_json::{json, Value};

use crate::core::translator::registry::Format;

/// Declares which modalities a provider/model supports.
#[derive(Debug, Clone)]
pub struct ModalityCapabilities {
    pub vision: bool,
    pub audio_input: bool,
    pub pdf: bool,
}

// ── Placeholder text ──────────────────────────────────────────────

/// Placeholder for the current (last) user turn — explains why media was dropped.
const PH_CURRENT_VISION: &str = "[image omitted: model has no vision support]";
const PH_CURRENT_AUDIO: &str = "[audio omitted: model has no audio support]";
const PH_CURRENT_PDF: &str = "[file omitted: model has no document support]";

/// Placeholder for earlier turns — neutral (combo may route to a different model).
const PH_PREV_VISION: &str = "[Previous image omitted from context.]";
const PH_PREV_AUDIO: &str = "[Previous audio omitted from context.]";
const PH_PREV_PDF: &str = "[Previous file omitted from context.]";

fn placeholder(cap: &str, is_last: bool) -> &'static str {
    match (cap, is_last) {
        ("vision", true) => PH_CURRENT_VISION,
        ("audioInput", true) => PH_CURRENT_AUDIO,
        ("pdf", true) => PH_CURRENT_PDF,
        ("vision", false) => PH_PREV_VISION,
        ("audioInput", false) => PH_PREV_AUDIO,
        ("pdf", false) => PH_PREV_PDF,
        _ => "[omitted]",
    }
}

// ── Block-to-capability maps ──────────────────────────────────────

/// OpenAI chat content block → required capability (null = keep).
fn cap_for_openai_block(block: &Value) -> Option<&'static str> {
    let t = block.get("type")?.as_str()?;
    if t == "image_url" || t == "image" {
        Some("vision")
    } else if t == "input_audio" || t == "audio_url" {
        Some("audioInput")
    } else if t == "file" {
        Some("pdf")
    } else {
        None
    }
}

/// Claude content block → required capability.
fn cap_for_claude_block(block: &Value) -> Option<&'static str> {
    let t = block.get("type")?.as_str()?;
    if t == "image" {
        Some("vision")
    } else if t == "document" {
        Some("pdf")
    } else {
        None
    }
}

/// Gemini inlineData/fileData mime prefix → capability.
fn cap_for_mime(mime: &str) -> Option<&'static str> {
    if mime.starts_with("image/") {
        Some("vision")
    } else if mime.starts_with("audio/") {
        Some("audioInput")
    } else if mime == "application/pdf" {
        Some("pdf")
    } else {
        None
    }
}

// ── Filter helpers ────────────────────────────────────────────────

/// Filter an array of content blocks: drop unsupported, inject one placeholder per modality.
/// `cap_of` maps a block to its required capability (None = plain text, keep).
fn filter_blocks(
    blocks: &mut Vec<Value>,
    cap_of: fn(&Value) -> Option<&'static str>,
    caps: &ModalityCapabilities,
    is_last: bool,
) {
    let mut removed = Vec::new();
    blocks.retain(|block| match cap_of(block) {
        Some("vision") if !caps.vision => {
            if !removed.contains(&"vision") {
                removed.push("vision");
            }
            false
        }
        Some("audioInput") if !caps.audio_input => {
            if !removed.contains(&"audioInput") {
                removed.push("audioInput");
            }
            false
        }
        Some("pdf") if !caps.pdf => {
            if !removed.contains(&"pdf") {
                removed.push("pdf");
            }
            false
        }
        _ => true,
    });
    for cap in &removed {
        blocks.push(json!({"type": "text", "text": placeholder(cap, is_last)}));
    }
}

// ── Format-specific strippers ─────────────────────────────────────

/// OpenAI / OpenAI-compatible: messages[].content[] (image_url, input_audio, file).
fn strip_openai(body: &mut Value, caps: &ModalityCapabilities) {
    let messages = match body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        Some(arr) => arr,
        None => return,
    };
    let last_idx = messages.len().saturating_sub(1);
    for (i, msg) in messages.iter_mut().enumerate() {
        let content = match msg.get_mut("content").and_then(|c| c.as_array_mut()) {
            Some(arr) => arr,
            None => continue,
        };
        filter_blocks(content, cap_for_openai_block, caps, i == last_idx);
    }
}

/// Claude: messages[].content[] (image, document).
fn strip_claude(body: &mut Value, caps: &ModalityCapabilities) {
    let messages = match body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        Some(arr) => arr,
        None => return,
    };
    let last_idx = messages.len().saturating_sub(1);
    for (i, msg) in messages.iter_mut().enumerate() {
        let content = match msg.get_mut("content").and_then(|c| c.as_array_mut()) {
            Some(arr) => arr,
            None => continue,
        };
        filter_blocks(content, cap_for_claude_block, caps, i == last_idx);
    }
}

/// OpenAI Responses: input[].content[] (input_image, input_file).
fn strip_responses(body: &mut Value, caps: &ModalityCapabilities) {
    let input = match body.get_mut("input").and_then(|i| i.as_array_mut()) {
        Some(arr) => arr,
        None => return,
    };
    let last_idx = input.len().saturating_sub(1);
    for (i, item) in input.iter_mut().enumerate() {
        let content = match item.get_mut("content").and_then(|c| c.as_array_mut()) {
            Some(arr) => arr,
            None => continue,
        };
        let mut removed = Vec::new();
        content.retain(|block| {
            let t = block.get("type").and_then(|t| t.as_str());
            if t == Some("input_image") && !caps.vision {
                if !removed.contains(&"vision") {
                    removed.push("vision");
                }
                return false;
            }
            if t == Some("input_file") && !caps.pdf {
                if !removed.contains(&"pdf") {
                    removed.push("pdf");
                }
                return false;
            }
            true
        });
        for cap in &removed {
            content.push(json!({"type": "input_text", "text": placeholder(cap, i == last_idx)}));
        }
    }
}

/// Gemini/GeminiCli: contents[].parts[] (inlineData, fileData by mime).
fn strip_gemini(body: &mut Value, caps: &ModalityCapabilities) {
    let contents = match body.get_mut("contents").and_then(|c| c.as_array_mut()) {
        Some(arr) => arr,
        None => return,
    };
    let last_idx = contents.len().saturating_sub(1);
    for (i, c) in contents.iter_mut().enumerate() {
        let parts = match c.get_mut("parts").and_then(|p| p.as_array_mut()) {
            Some(arr) => arr,
            None => continue,
        };
        let mut removed = Vec::new();
        parts.retain(|part| {
            let mime = part
                .get("inlineData")
                .and_then(|d| d.get("mimeType"))
                .and_then(|m| m.as_str())
                .or_else(|| {
                    part.get("fileData")
                        .and_then(|d| d.get("mimeType"))
                        .and_then(|m| m.as_str())
                });
            match mime.and_then(cap_for_mime) {
                Some("vision") if !caps.vision => {
                    if !removed.contains(&"vision") {
                        removed.push("vision");
                    }
                    false
                }
                Some("audioInput") if !caps.audio_input => {
                    if !removed.contains(&"audioInput") {
                        removed.push("audioInput");
                    }
                    false
                }
                Some("pdf") if !caps.pdf => {
                    if !removed.contains(&"pdf") {
                        removed.push("pdf");
                    }
                    false
                }
                _ => true,
            }
        });
        for cap in &removed {
            parts.push(json!({"text": placeholder(cap, i == last_idx)}));
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────

/// Remove media blocks the model can't read, in-place on the source-format body.
///
/// Returns `true` if any modality was stripped (any cap was false for a present block type).
/// This matches 9router `modality.js` `stripUnsupportedModalities()` — handles 6 format groups.
pub fn strip_unsupported_modalities(
    body: &mut Value,
    source_format: Format,
    caps: &ModalityCapabilities,
) -> bool {
    // Fast exit: model supports everything we'd strip.
    if caps.vision && caps.audio_input && caps.pdf {
        return false;
    }

    match source_format {
        Format::OpenAi | Format::Ollama | Format::Kiro | Format::Cursor | Format::CommandCode => {
            strip_openai(body, caps);
        }
        Format::Claude => {
            strip_claude(body, caps);
        }
        Format::OpenAiResponses | Format::OpenAiResponse | Format::Codex => {
            strip_responses(body, caps);
        }
        Format::Gemini | Format::GeminiCli | Format::Vertex => {
            strip_gemini(body, caps);
        }
        Format::Antigravity => {
            // Antigravity nests Gemini contents under body["request"]["contents"]
            if let Some(request) = body.get_mut("request") {
                if let Some(contents) = request.get_mut("contents") {
                    if let Some(arr) = contents.as_array_mut() {
                        let last_idx = arr.len().saturating_sub(1);
                        for (i, c) in arr.iter_mut().enumerate() {
                            let parts = match c.get_mut("parts").and_then(|p| p.as_array_mut()) {
                                Some(arr) => arr,
                                None => continue,
                            };
                            let mut removed = Vec::new();
                            parts.retain(|part| {
                                let mime = part
                                    .get("inlineData")
                                    .and_then(|d| d.get("mimeType"))
                                    .and_then(|m| m.as_str())
                                    .or_else(|| {
                                        part.get("fileData")
                                            .and_then(|d| d.get("mimeType"))
                                            .and_then(|m| m.as_str())
                                    });
                                match mime.and_then(cap_for_mime) {
                                    Some("vision") if !caps.vision => {
                                        if !removed.contains(&"vision") {
                                            removed.push("vision");
                                        }
                                        false
                                    }
                                    Some("audioInput") if !caps.audio_input => {
                                        if !removed.contains(&"audioInput") {
                                            removed.push("audioInput");
                                        }
                                        false
                                    }
                                    Some("pdf") if !caps.pdf => {
                                        if !removed.contains(&"pdf") {
                                            removed.push("pdf");
                                        }
                                        false
                                    }
                                    _ => true,
                                }
                            });
                            for cap in &removed {
                                parts.push(json!({"text": placeholder(cap, i == last_idx)}));
                            }
                        }
                    }
                }
            }
        }
    }
    true
}

/// Compute default capabilities for a given source format.
///
/// This is a best-effort mapping that covers the major capability patterns:
/// - OpenAI (GPT-4o+): vision, no audio/pdf by default
/// - Claude: vision, pdf, no audio
/// - Gemini: vision, audio, pdf
/// - Others: conservative (vision-only or text-only)
pub fn capabilities_for_format(source_format: Format) -> ModalityCapabilities {
    match source_format {
        Format::Claude => ModalityCapabilities {
            vision: true,
            audio_input: false,
            pdf: true,
        },
        Format::Gemini | Format::GeminiCli | Format::Vertex | Format::Antigravity => {
            ModalityCapabilities {
                vision: true,
                audio_input: true,
                pdf: true,
            }
        }
        Format::Kiro => ModalityCapabilities {
            vision: true,
            audio_input: false,
            pdf: false,
        },
        _ => ModalityCapabilities {
            // OpenAI, Codex, Ollama, Cursor, CommandCode — conservative
            vision: true,
            audio_input: false,
            pdf: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn caps_all_true() -> ModalityCapabilities {
        ModalityCapabilities {
            vision: true,
            audio_input: true,
            pdf: true,
        }
    }

    fn caps_no_vision() -> ModalityCapabilities {
        ModalityCapabilities {
            vision: false,
            audio_input: true,
            pdf: true,
        }
    }

    fn caps_text_only() -> ModalityCapabilities {
        ModalityCapabilities {
            vision: false,
            audio_input: false,
            pdf: false,
        }
    }

    #[test]
    fn test_fast_exit_when_all_caps_supported() {
        let mut body = json!({"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"http://example.com/img.png"}}]}]});
        // Fast exit: all caps true → no stripping, returns false
        assert!(!strip_unsupported_modalities(
            &mut body,
            Format::OpenAi,
            &caps_all_true()
        ));
        // Body unchanged
        assert!(body["messages"][0]["content"][0].get("image_url").is_some());
    }

    #[test]
    fn test_strip_openai_image_url() {
        let mut body = json!({"messages":[{"role":"user","content":[
            {"type":"text","text":"hello"},
            {"type":"image_url","image_url":{"url":"http://example.com/img.png"}}
        ]}]});
        assert!(strip_unsupported_modalities(
            &mut body,
            Format::OpenAi,
            &caps_no_vision()
        ));
        // Image block replaced with placeholder
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "text");
        assert!(content[1]["text"]
            .as_str()
            .unwrap()
            .contains("image omitted"));
    }

    #[test]
    fn test_strip_claude_image() {
        let mut body = json!({"messages":[{"role":"user","content":[
            {"type":"text","text":"hello"},
            {"type":"image","source":{"type":"url","url":"http://example.com/img.png"}}
        ]}]});
        assert!(strip_unsupported_modalities(
            &mut body,
            Format::Claude,
            &caps_no_vision()
        ));
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[1]["type"], "text");
        assert!(content[1]["text"]
            .as_str()
            .unwrap()
            .contains("image omitted"));
    }

    #[test]
    fn test_strip_gemini_inline_data() {
        let mut body = json!({"contents":[{"role":"user","parts":[
            {"text":"hello"},
            {"inlineData":{"mimeType":"image/png","data":"abc123"}}
        ]}]});
        assert!(strip_unsupported_modalities(
            &mut body,
            Format::Gemini,
            &caps_no_vision()
        ));
        let parts = body["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts[1]["text"].as_str().unwrap().contains("image omitted"));
    }

    #[test]
    fn test_strip_responses_input_image() {
        let mut body = json!({"input":[{"content":[
            {"type":"input_text","text":"hello"},
            {"type":"input_image","image_url":"http://example.com/img.png"}
        ]}]});
        assert!(strip_unsupported_modalities(
            &mut body,
            Format::OpenAiResponses,
            &caps_no_vision()
        ));
        let content = body["input"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert!(content[1]["text"]
            .as_str()
            .unwrap()
            .contains("image omitted"));
    }

    #[test]
    fn test_placeholder_differs_by_turn() {
        let mut body = json!({"messages":[
            {"role":"user","content":[{"type":"image_url","image_url":{"url":"http://img.png"}}]},
            {"role":"user","content":[{"type":"image_url","image_url":{"url":"http://img2.png"}}]},
        ]});
        assert!(strip_unsupported_modalities(
            &mut body,
            Format::OpenAi,
            &caps_no_vision()
        ));
        let c0 = &body["messages"][0]["content"].as_array().unwrap()[0];
        let c1 = &body["messages"][1]["content"].as_array().unwrap()[0];
        assert!(c0["text"]
            .as_str()
            .unwrap()
            .contains("Previous image omitted"));
        assert!(c1["text"].as_str().unwrap().contains("image omitted"));
    }

    #[test]
    fn test_no_op_when_no_multimodal_blocks() {
        let mut body = json!({"messages":[{"role":"user","content":"plain text"}]});
        // text-only messages are not content arrays, so nested loops skip
        assert!(strip_unsupported_modalities(
            &mut body,
            Format::OpenAi,
            &caps_no_vision()
        ));
        assert_eq!(body["messages"][0]["content"], "plain text");
    }

    #[test]
    fn test_strip_multiple_modalities() {
        let mut body = json!({"messages":[{"role":"user","content":[
            {"type":"image_url","image_url":{"url":"http://img.png"}},
            {"type":"input_audio","input_audio":{"data":"..."}},
            {"type":"file","file":{"filename":"doc.pdf"}},
        ]}]});
        assert!(strip_unsupported_modalities(
            &mut body,
            Format::OpenAi,
            &caps_text_only()
        ));
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        for part in content {
            assert_eq!(part["type"], "text");
        }
    }
}
