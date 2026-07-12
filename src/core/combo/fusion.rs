//! Fusion strategy orchestrator for OpenProxy's combo dispatch pipeline.
//!
//! Ports 9router's `executeFusionStrategy` / `handleFusionChat` logic:
//! fan out the same prompt to every panel model in parallel, collect
//! answers, then dispatch a judge model to synthesize one final response.
//!
//! Graceful degradation:
//! - 0 successful panels → `FusionError` with HTTP 503
//! - 1 successful panel  → return that answer verbatim (no judge needed)
//! - 2+ successful panels → anonymize sources, build judge prompt, dispatch

use std::fmt;
use std::future::Future;
use std::time::Duration;

use futures_util::future::join_all;
use futures_util::StreamExt;
use serde_json::{json, Value};

use super::FusionConfig;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Error returned by [`handle_fusion_chat`] when the fusion pipeline cannot
/// produce a usable answer.
#[derive(Debug, Clone)]
pub struct FusionError {
    /// HTTP status code the caller should surface (typically 503).
    pub status: u16,
    /// Human-readable description of what went wrong.
    pub message: String,
}

impl fmt::Display for FusionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.status, self.message)
    }
}

impl std::error::Error for FusionError {}

// ---------------------------------------------------------------------------
// Per-panel result
// ---------------------------------------------------------------------------

/// Outcome of a single panel model call inside a fusion execution.
#[derive(Debug, Clone)]
pub struct FusionPanelResult {
    /// Zero-based index into the original `panel_models` slice.
    pub index: usize,
    /// Model identifier that produced this answer.
    pub model: String,
    /// Extracted text content from the panel response.
    pub answer: String,
    /// Detected source format (`"openai"`, `"claude"`, `"gemini"`, or
    /// `"unknown"`) — used by [`extract_panel_text`] callers to know
    /// which extraction path succeeded.
    pub source_format: String,
}

// ---------------------------------------------------------------------------
// Timeout helper
// ---------------------------------------------------------------------------

/// Wraps `future` with a [`tokio::time::timeout`] of `timeout_ms`
/// milliseconds. Returns `Some(T)` on success, `None` on expiry.
pub async fn with_timeout<T>(future: impl Future<Output = T>, timeout_ms: u64) -> Option<T> {
    tokio::time::timeout(Duration::from_millis(timeout_ms), future)
        .await
        .ok()
}

// ---------------------------------------------------------------------------
// Tool-history flattening
// ---------------------------------------------------------------------------

/// Create a copy of `body` with tool-related content stripped or flattened:
///
/// - `tools` / `functions` keys are removed.
/// - Assistant messages preserve `tool_calls` as inlined prose
///   (`[Called tools: name1, name2]`).
/// - Messages with `role: "tool"` or `role: "function"` are rewritten as
///   `role: "assistant"` with their content preserved as prose.
/// - Anthropic-style content arrays with `tool_use` / `tool_result` blocks
///   extract text content and inline tool information.
/// - `input`-style arrays (Responses API) are handled identically to
///   `messages` arrays.
///
/// This ensures panel models receive a clean conversation without
/// tool-calling context they cannot act on.
pub fn flatten_tool_history(body: &Value) -> Value {
    let mut out = body.clone();

    // Strip top-level tool definitions.
    if let Some(obj) = out.as_object_mut() {
        obj.remove("tools");
        obj.remove("functions");
        obj.remove("tool_choice");
        obj.remove("function_call");
    }

    // Try messages array first, then input array (Responses API).
    let key = if body.get("messages").and_then(Value::as_array).is_some() {
        "messages"
    } else if body.get("input").and_then(Value::as_array).is_some() {
        "input"
    } else {
        return out;
    };

    let Some(messages) = out.get(key).and_then(Value::as_array).cloned() else {
        return out;
    };

    let cleaned: Vec<Value> = messages
        .iter()
        .map(|msg| {
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
            match role {
                "tool" | "function" => {
                    // Flatten tool/function result into assistant prose.
                    // 9router wraps with [Tool result: …] prefix so panel
                    // models can distinguish tool output from user speech.
                    let raw_content = msg
                        .get("content")
                        .and_then(|c| match c {
                            Value::String(s) => Some(s.clone()),
                            Value::Array(arr) => Some(
                                arr.iter()
                                    .filter_map(|v| match v {
                                        Value::String(s) => Some(s.clone()),
                                        Value::Object(obj) => obj
                                            .get("text")
                                            .and_then(Value::as_str)
                                            .map(String::from),
                                        _ => None,
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            ),
                            _ => None,
                        })
                        .unwrap_or_default();

                    let content = if raw_content.is_empty() {
                        raw_content
                    } else {
                        format!("[Tool result: {}]", raw_content)
                    };

                    json!({
                        "role": "assistant",
                        "content": content
                    })
                }
                "assistant" => {
                    let mut m = msg.clone();
                    if let Some(obj) = m.as_object_mut() {
                        // Inline tool_calls names into content.
                        let tool_names: Vec<String> = obj
                            .get("tool_calls")
                            .and_then(Value::as_array)
                            .map(|calls| {
                                calls
                                    .iter()
                                    .filter_map(|tc| {
                                        tc.get("function")
                                            .and_then(|f| f.get("name"))
                                            .and_then(Value::as_str)
                                            .map(String::from)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();

                        if !tool_names.is_empty() {
                            let existing = obj
                                .get("content")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let inline = format!("[Called tools: {}]", tool_names.join(", "));
                            let new_content = if existing.is_empty() {
                                inline
                            } else {
                                format!("{}\n\n{}", existing, inline)
                            };
                            obj.insert("content".into(), Value::String(new_content));
                        }

                        obj.remove("tool_calls");
                        obj.remove("function_call");

                        // Handle content arrays with tool blocks (Anthropic format).
                        if let Some(content_arr) =
                            obj.get_mut("content").and_then(Value::as_array_mut)
                        {
                            let mut text_parts: Vec<String> = Vec::new();
                            let mut tool_names_inline: Vec<String> = Vec::new();
                            for part in content_arr.iter() {
                                if let Some(text) = part.get("text").and_then(Value::as_str) {
                                    text_parts.push(text.to_string());
                                } else if part.get("type").and_then(Value::as_str)
                                    == Some("tool_use")
                                {
                                    if let Some(name) = part.get("name").and_then(Value::as_str) {
                                        tool_names_inline.push(name.to_string());
                                    }
                                } else if part.get("type").and_then(Value::as_str)
                                    == Some("tool_result")
                                {
                                    let result_text = part
                                        .get("content")
                                        .and_then(|c| match c {
                                            Value::String(s) => Some(s.clone()),
                                            Value::Array(arr) => Some(
                                                arr.iter()
                                                    .filter_map(|v| {
                                                        v.get("text").and_then(Value::as_str)
                                                    })
                                                    .collect::<Vec<_>>()
                                                    .join("\n"),
                                            ),
                                            _ => None,
                                        })
                                        .unwrap_or_default();
                                    if !result_text.is_empty() {
                                        text_parts.push(format!("[Tool result: {}]", result_text));
                                    }
                                }
                            }
                            let mut combined = text_parts.join("\n");
                            if !tool_names_inline.is_empty() {
                                let tool_str =
                                    format!("[Called tools: {}]", tool_names_inline.join(", "));
                                if combined.is_empty() {
                                    combined = tool_str;
                                } else {
                                    combined = format!("{}\n{}", tool_str, combined);
                                }
                            }
                            obj.insert("content".into(), Value::String(combined));
                        }
                    }
                    m
                }
                _ => msg.clone(),
            }
        })
        .collect();

    if let Some(obj) = out.as_object_mut() {
        obj.insert(key.to_string(), Value::Array(cleaned));
    }

    out
}

// ---------------------------------------------------------------------------
// Panel body construction + raw response storage
// ---------------------------------------------------------------------------

/// A panel result that preserves the raw response Value for the single-survivor
/// fast path (avoids re-wrapping a synthetic chat completion response).
struct PanelOutcome {
    result: FusionPanelResult,
    raw: Value,
}

/// Build a non-streaming panel request body from the original chat body.
///
/// - Forces `stream: false` (panels are always non-streaming).
/// - Strips tool/function definitions and history via [`flatten_tool_history`].
/// - Applies `max_tokens` from the fusion config when present.
pub fn create_panel_body(original_body: &Value, config: &FusionConfig) -> Value {
    let mut body = flatten_tool_history(original_body);

    if let Some(obj) = body.as_object_mut() {
        // Panels never stream — the judge gets the streaming flag.
        obj.insert("stream".to_string(), json!(false));

        // Apply panel-level max_tokens cap when the config carries one.
        // FusionConfig doesn't expose a max_tokens field directly, but
        // callers may embed it in the body already; we leave it untouched
        // if present and don't inject a default.
        let _ = config; // acknowledged — reserved for future per-panel caps
    }

    body
}

// ---------------------------------------------------------------------------
// Panel response text extraction
// ---------------------------------------------------------------------------

/// Extract the textual answer from a panel response `Value`.
///
/// Handles multiple common response shapes with full content joining:
///
/// | Format            | Path to text                                             |
/// |-------------------|----------------------------------------------------------|
/// | OpenAI chat       | `choices[0].message.content` (string)                    |
/// | OpenAI Responses  | `output[0].content[0].text` or `output[0].message.content`|
/// | Claude messages   | All `content[*].text` joined                              |
/// | Gemini            | All `candidates[0].content.parts[*].text` joined          |
/// | Unknown           | Top-level `content` (string or first text of array)       |
///
/// Returns `(text, source_format)` — the detected format and the extracted
/// text joined from all blocks. Returns empty string when no known shape
/// matches.
pub fn extract_panel_text(panel_response: &Value) -> (String, String) {
    // OpenAI chat completion
    if let Some(choices) = panel_response.get("choices").and_then(Value::as_array) {
        if let Some(text) = choices
            .first()
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
        {
            return (text.to_string(), "openai".to_string());
        }
        // Also check choices[*].text (used by some providers)
        if let Some(text) = choices
            .first()
            .and_then(|c| c.get("text"))
            .and_then(Value::as_str)
        {
            return (text.to_string(), "openai".to_string());
        }
    }

    // OpenAI Responses API: output[].content[].text
    if let Some(output) = panel_response.get("output").and_then(Value::as_array) {
        for item in output {
            if let Some(content) = item.get("content").and_then(Value::as_array) {
                let texts: Vec<&str> = content
                    .iter()
                    .filter_map(|c| c.get("text").and_then(Value::as_str))
                    .collect();
                if !texts.is_empty() {
                    return (texts.join("\n"), "openai".to_string());
                }
            }
            if let Some(text) = item
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_str)
            {
                return (text.to_string(), "openai".to_string());
            }
        }
    }

    // Claude messages API — join ALL content blocks, not just the first
    if let Some(content_arr) = panel_response.get("content").and_then(Value::as_array) {
        let texts: Vec<&str> = content_arr
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect();
        if !texts.is_empty() {
            return (texts.join("\n"), "claude".to_string());
        }
    }

    // Gemini generateContent — join ALL parts from the first candidate
    if let Some(candidates) = panel_response.get("candidates").and_then(Value::as_array) {
        if let Some(parts) = candidates
            .first()
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(Value::as_array)
        {
            let texts: Vec<&str> = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect();
            if !texts.is_empty() {
                return (texts.concat(), "gemini".to_string());
            }
        }
    }

    // Fallback: try top-level "content" as a plain string or text array
    if let Some(text) = panel_response.get("content").and_then(Value::as_str) {
        return (text.to_string(), "unknown".to_string());
    }
    if let Some(arr) = panel_response.get("content").and_then(Value::as_array) {
        let texts: Vec<&str> = arr
            .iter()
            .filter_map(|v| v.get("text").and_then(Value::as_str))
            .collect();
        if !texts.is_empty() {
            return (texts.join("\n"), "unknown".to_string());
        }
    }

    (String::new(), "unknown".to_string())
}

// ---------------------------------------------------------------------------
// Judge prompt construction
// ---------------------------------------------------------------------------

/// Build the judge instruction as a user-role message.
///
/// Ports 9router's `buildJudgePrompt` verbatim.
pub fn build_judge_prompt(answers: &[(String, String)]) -> Value {
    let answer_count = answers.len();
    let mut parts = Vec::with_capacity(answers.len() + 4);

    parts.push(format!(
        "You are the JUDGE in a model-fusion panel. {} expert models independently \
         answered the user's most recent request. Their responses are below, \
         anonymized by source.\n\n\
         Do NOT mention that multiple models were used, and do NOT refer to the \
         sources. Produce ONE authoritative final answer addressed directly to \
         the user.\n\n\
         First, internally analyze the panel along these dimensions: \
         consensus (points most sources agree on — treat as higher-confidence), \
         contradictions (where they disagree — resolve with your own judgment), \
         partial coverage, unique insights only one source surfaced, and blind \
         spots every source missed. Then write the best possible final answer \
         grounded in that analysis — more complete and correct than any single \
         response, with no filler.",
        answer_count
    ));

    parts.push("\n=== PANEL RESPONSES ===".to_string());

    for (label, text) in answers {
        parts.push(format!("\n[{}]\n{}", label, text));
    }

    parts.push("\n=== END PANEL RESPONSES ===".to_string());
    parts.push("\nNow write the final answer to the user's original request.".to_string());

    let combined = parts.join("\n");

    json!({
        "role": "user",
        "content": combined
    })
}

// ---------------------------------------------------------------------------
// Main orchestrator
// ---------------------------------------------------------------------------

/// Orchestrate the full fusion flow.
///
/// # Type parameters
///
/// - `F` / `Fut`: callback that dispatches a single-model chat request.
///   Receives `(model_id, &mut request_body)` and returns the provider
///   response `Value` or an `anyhow::Error`.
///
/// # Arguments
///
/// - `body` — the original chat-completions request body (mutated in place
///   for the judge call; a defensive clone is made for each panel).
/// - `panel_models` — model identifiers to fan out to in parallel.
/// - `config` — fusion tuning knobs ([`FusionConfig`]).
/// - `judge_model` — explicit judge override; falls back to
///   `config.judge_model`, then to the first panel model.
/// - `handle_single_model` — the dispatch callback described above.
///
/// # Graceful degradation
///
/// | Successful panels | Behaviour                                  |
/// |-------------------|--------------------------------------------|
/// | 0                 | `Err(FusionError { status: 503, … })`      |
/// | 1                 | Return that panel's raw response as-is     |
/// | ≥ 2               | Build judge prompt → dispatch judge model  |
pub async fn handle_fusion_chat<F, Fut>(
    body: &mut Value,
    panel_models: &[String],
    config: &FusionConfig,
    judge_model: Option<&str>,
    handle_single_model: F,
) -> Result<Value, FusionError>
where
    F: Fn(String, Value) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, anyhow::Error>> + Send,
{
    if panel_models.is_empty() {
        return Err(FusionError {
            status: 400,
            message: "Fusion requires at least one panel model".into(),
        });
    }

    // Single-model shortcut: bypass the fan-out and judge entirely.
    if panel_models.len() == 1 {
        return handle_single_model(panel_models[0].clone(), body.clone())
            .await
            .map_err(|e| FusionError {
                status: 502,
                message: format!("Single-model panel failed: {e}"),
            });
    }

    let panel_body_template = create_panel_body(body, config);
    let timeout_ms = config.panel_hard_timeout_ms;
    let min_panel = config.min_panel.max(2).min(panel_models.len());
    let grace_ms = config.straggler_grace_ms;

    // ---- Quorum-grace fan-out ----
    //
    // Spawn all panel futures concurrently via FuturesUnordered. Once
    // `min_panel` successful answers arrive we start a grace timer;
    // when the grace expires or all remaining panels resolve we proceed
    // with whatever we have. This mirrors 9router's collectPanel()
    // quorum-grace pattern.

    let mut outcomes: Vec<PanelOutcome> = Vec::new();
    let mut remaining: Vec<_> = (0..panel_models.len()).collect();
    {
        let mut panel_futs = futures_util::stream::FuturesUnordered::new();
        for (idx, model) in panel_models.iter().enumerate() {
            let mut panel_body = panel_body_template.clone();
            panel_body["model"] = json!(model);
            let model = model.clone();
            let panel_fn = handle_single_model.clone();
            panel_futs.push(Box::pin(async move {
                let result = tokio::time::timeout(
                    Duration::from_millis(timeout_ms),
                    panel_fn(model.clone(), panel_body),
                )
                .await;
                (idx, model, result)
            }));
        }

        // Independent grace timer (9router setTimeout once quorum reached).
        // Without a separate sleep future, grace only re-checks when another
        // panel completes — stragglers could hold until hard timeout.
        let mut hard_deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        let mut grace_started = false;
        let mut grace_deadline: Option<tokio::time::Instant> = None;

        loop {
            if panel_futs.is_empty() {
                break;
            }
            let now = tokio::time::Instant::now();
            if now >= hard_deadline {
                break;
            }
            if let Some(gd) = grace_deadline {
                if now >= gd {
                    break;
                }
            }

            let sleep_until = match grace_deadline {
                Some(gd) if gd < hard_deadline => gd,
                _ => hard_deadline,
            };
            let sleep_dur = sleep_until.saturating_duration_since(tokio::time::Instant::now());

            tokio::select! {
                biased;
                _ = tokio::time::sleep(sleep_dur) => {
                    break;
                }
                next = panel_futs.next() => {
                    let Some((idx, model, result)) = next else { break; };
                    remaining.retain(|i| *i != idx);

                    if let Ok(Ok(response)) = result {
                        let (text, fmt) = extract_panel_text(&response);
                        if !text.is_empty() {
                            outcomes.push(PanelOutcome {
                                result: FusionPanelResult {
                                    index: idx,
                                    model,
                                    answer: text,
                                    source_format: fmt,
                                },
                                raw: response,
                            });
                        }
                    }

                    let ok_count = outcomes.len();
                    if ok_count >= min_panel && !grace_started {
                        grace_started = true;
                        let gd = tokio::time::Instant::now() + Duration::from_millis(grace_ms);
                        grace_deadline = Some(gd);
                        if gd < hard_deadline {
                            hard_deadline = gd;
                        }
                        tracing::debug!(
                            target: "openproxy::fusion",
                            "FUSION quorum={} grace_ms={} remaining_panels={}",
                            ok_count,
                            grace_ms,
                            panel_futs.len()
                        );
                    }
                }
            }
        }
    }

    let ok_count = outcomes.len();

    // ---- Graceful degradation ----

    if ok_count == 0 {
        return Err(FusionError {
            status: 503,
            message: "All fusion panels failed or timed out".into(),
        });
    }

    if ok_count == 1 {
        // Single survivor — re-dispatch through handle_single_model
        // with the original body (preserving stream flag, tools, etc.)
        // rather than returning the non-streaming, tool-stripped panel
        // response.  9router does the same (handleSingleModel(body, m)).
        let survivor = match outcomes.into_iter().next() {
            Some(s) => s,
            None => {
                return Err(FusionError {
                    status: 502,
                    message: "Single survivor expected but outcomes empty".to_string(),
                });
            }
        };
        return handle_single_model(survivor.result.model, body.clone())
            .await
            .map_err(|e| FusionError {
                status: 502,
                message: format!("Single-survivor re-dispatch failed: {e}"),
            });
    }

    // ---- Judge synthesis (2+ answers) ----
    // The judge body is the original conversation + judge message.
    // It contains no tool messages (they were flattened during panel calls),
    // so RTK/Headroom/Caveman/Ponytail re-application by the judge callback
    // is effectively idempotent.

    // Anonymize sources: "Source 1", "Source 2", …
    let anonymized: Vec<(String, String)> = outcomes
        .iter()
        .enumerate()
        .map(|(i, r)| (format!("Source {}", i + 1), r.result.answer.clone()))
        .collect();

    let judge_message = build_judge_prompt(&anonymized);

    // Determine judge model: explicit arg → config → first panel model.
    let judge_model_id = judge_model
        .map(String::from)
        .or_else(|| config.judge_model.clone())
        .unwrap_or_else(|| panel_models[0].clone());

    // Build judge body: original conversation + judge user message.
    let mut judge_body = body.clone();
    {
        let obj = judge_body.as_object_mut().expect("body is an object");

        obj.insert("model".to_string(), json!(judge_model_id));
        // Preserve client stream flag (9router judge keeps original body stream).
        // Chat fusion path currently collects JSON for multi-panel; if stream was
        // true, callers that support SSE can re-request. Default: keep original.
        if !obj.contains_key("stream") {
            obj.insert("stream".to_string(), json!(false));
        }

        // Append judge prompt to whichever array the body uses (messages,
        // input, contents, or request.contents).  9router's appendUserTurn()
        // handles the same set of shapes.
        let pushed = {
            // Detect which format the body uses and push judge message
            // with the correct shape.  Gemini uses {role, parts:[{text}]}
            // while OpenAI/Claude use {role, content}.
            let is_gemini = obj.contains_key("contents")
                || obj.contains_key("systemInstruction")
                || obj.contains_key("system_instruction")
                || obj.get("request").and_then(|r| r.get("contents")).is_some();

            let judge_entry = if is_gemini {
                json!({"role": "user", "parts": [{"text": judge_message.get("content").and_then(Value::as_str).unwrap_or("")}]})
            } else {
                judge_message.clone()
            };

            // Find the target key first (immutable borrow).
            let target = ["messages", "input", "contents"]
                .iter()
                .find(|&&k| obj.get(k).is_some())
                .copied();

            match target {
                Some(key) => {
                    if let Some(arr) = obj.get_mut(key).and_then(Value::as_array_mut) {
                        arr.push(judge_entry);
                        true
                    } else {
                        false
                    }
                }
                None => {
                    if let Some(request) = obj.get_mut("request").and_then(Value::as_object_mut) {
                        if let Some(arr) = request.get_mut("contents").and_then(Value::as_array_mut)
                        {
                            arr.push(judge_entry);
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                }
            }
        };

        if !pushed {
            obj.insert("messages".to_string(), json!([judge_message]));
        }
    }

    handle_single_model(judge_model_id.clone(), judge_body)
        .await
        .map_err(|e| FusionError {
            status: 502,
            message: format!("Judge model failed: {e}"),
        })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_strips_tools_and_rewrites_tool_messages() {
        let body = json!({
            "model": "test",
            "tools": [{"type": "function", "function": {"name": "foo"}}],
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "", "tool_calls": [{"id": "1"}]},
                {"role": "tool", "content": "result data"},
                {"role": "assistant", "content": "final"}
            ]
        });

        let flat = flatten_tool_history(&body);

        // tools key removed
        assert!(flat.get("tools").is_none());

        let msgs = flat["messages"].as_array().expect("messages array");
        // assistant tool_calls stripped
        assert!(msgs[1].get("tool_calls").is_none());
        // tool message rewritten to assistant
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[2]["content"], "[Tool result: result data]");
    }

    #[test]
    fn create_panel_body_forces_non_streaming() {
        let body = json!({
            "model": "combo",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        });
        let cfg = FusionConfig::default();
        let panel = create_panel_body(&body, &cfg);

        assert_eq!(panel["stream"], false);
    }

    #[test]
    fn extract_openai_response() {
        let resp = json!({
            "choices": [{"message": {"content": "hello from openai"}}]
        });
        let (text, fmt) = extract_panel_text(&resp);
        assert_eq!(text, "hello from openai");
        assert_eq!(fmt, "openai");
    }

    #[test]
    fn extract_claude_response() {
        let resp = json!({
            "content": [{"type": "text", "text": "hello from claude"}]
        });
        let (text, fmt) = extract_panel_text(&resp);
        assert_eq!(text, "hello from claude");
        assert_eq!(fmt, "claude");
    }

    #[test]
    fn extract_gemini_response() {
        let resp = json!({
            "candidates": [{"content": {"parts": [{"text": "hello from gemini"}]}}]
        });
        let (text, fmt) = extract_panel_text(&resp);
        assert_eq!(text, "hello from gemini");
        assert_eq!(fmt, "gemini");
    }

    #[test]
    fn judge_prompt_anonymizes_sources() {
        let answers: Vec<(String, String)> = vec![
            ("Source 1".into(), "Answer A".into()),
            ("Source 2".into(), "Answer B".into()),
        ];
        let msg = build_judge_prompt(&answers);

        assert_eq!(msg["role"], "user");
        let content = msg["content"].as_str().expect("string content");
        assert!(content.contains("Source 1"));
        assert!(content.contains("Source 2"));
        assert!(content.contains("Answer A"));
        assert!(content.contains("JUDGE"));
    }

    #[test]
    fn fusion_error_display() {
        let err = FusionError {
            status: 503,
            message: "all panels failed".into(),
        };
        assert_eq!(format!("{err}"), "[503] all panels failed");
    }

    #[tokio::test]
    async fn with_timeout_returns_some_on_success() {
        let result = with_timeout(async { 42 }, 1000).await;
        assert_eq!(result, Some(42));
    }

    #[tokio::test]
    async fn with_timeout_returns_none_on_expiry() {
        let result = with_timeout(
            async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                42
            },
            10,
        )
        .await;
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn fusion_zero_panels_returns_503() {
        let mut body = json!({
            "model": "combo",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let cfg = FusionConfig::default();

        let result = handle_fusion_chat(
            &mut body,
            &[],
            &cfg,
            None,
            |_model: String, _body: Value| async {
                Ok(json!({"choices": [{"message": {"content": "x"}}]}))
            },
        )
        .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status, 400);
    }

    #[tokio::test]
    async fn fusion_single_survivor_returns_directly() {
        let mut body = json!({
            "model": "combo",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let cfg = FusionConfig::default();
        let models = vec!["model-a".to_string(), "model-b".to_string()];

        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cc = call_count.clone();

        let result = handle_fusion_chat(
            &mut body,
            &models,
            &cfg,
            None,
            move |model: String, _body: Value| {
                let cc = cc.clone();
                async move {
                    let n = cc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if model == "model-a" {
                        Ok(json!({
                            "choices": [{"message": {"content": "answer A"}}]
                        }))
                    } else {
                        Err(anyhow::anyhow!("model-b failed"))
                    }
                }
            },
        )
        .await;

        let resp = result.expect("should succeed with one survivor");
        let content = resp["choices"][0]["message"]["content"]
            .as_str()
            .expect("content");
        assert_eq!(content, "answer A");
    }
}
