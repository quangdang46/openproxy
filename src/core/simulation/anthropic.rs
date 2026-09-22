//! Anthropic (+compatible) simulator — non-stream + SSE (bead sim-09).
//!
//! Determinism contract (plan §5): same request bytes → byte-identical
//! response. IDs derive from the request hash — no wall-clock, no randomness.
//!
//! SSE uses Anthropic **named events** (`event: <type>\ndata: {...}`) in the
//! exact lifecycle order the real API emits, because our downstream SSE
//! translator parses event names (plan §5, bead spec):
//! `message_start → content_block_start → content_block_delta* →
//!   content_block_stop → message_delta → message_stop`.
//!
//! Tool echo (NOT scripted outputs): echo the FIRST offered tool as a
//! `tool_use` block with `stop_reason: "tool_use"`.
//! Errors use the Anthropic envelope `{type:"error", error:{type, message}}`.

use serde_json::{json, Value};

use super::engine::{estimate_tokens, hash8, split_words, SimContext};
use super::error::SimulationError;
use crate::core::executor::ProviderFormat;

pub struct AnthropicSimulator;

#[async_trait::async_trait]
impl super::engine::ProviderSimulator for AnthropicSimulator {
    fn format(&self) -> ProviderFormat {
        ProviderFormat::Anthropic
    }

    async fn execute(&self, ctx: &SimContext<'_>) -> Result<Value, SimulationError> {
        validate(ctx)?;
        if ctx.stream {
            Ok(stream_envelope(ctx))
        } else {
            Ok(non_stream(ctx))
        }
    }
}

/// Anthropic-compatible simulator: identical protocol, distinct format key
/// (plan §3.6). NOTE: `ClaudeCompatible` is NOT registered (engine maps it
/// to Anthropic at dispatch in DefaultExecutor::execute_simulated).
pub struct AnthropicCompatibleSimulator;

#[async_trait::async_trait]
impl super::engine::ProviderSimulator for AnthropicCompatibleSimulator {
    fn format(&self) -> ProviderFormat {
        ProviderFormat::AnthropicCompatible
    }

    async fn execute(&self, ctx: &SimContext<'_>) -> Result<Value, SimulationError> {
        validate(ctx)?;
        if ctx.stream {
            Ok(stream_envelope(ctx))
        } else {
            Ok(non_stream(ctx))
        }
    }
}

/// Interim known-model list (bead sim-09). Bead sim-11 replaces with models.rs.
fn is_known_model(model: &str) -> bool {
    const KNOWN: &[&str] = &[
        "claude-opus-4-6",
        "claude-opus-4-1",
        "claude-sonnet-4-6",
        "claude-sonnet-4-5",
        "claude-haiku-4-5",
        "claude-3-7-sonnet-latest",
        "claude-3-5-sonnet-latest",
        "claude-3-5-haiku-latest",
        "claude-3-opus-latest",
    ];
    KNOWN.contains(&model)
}

/// Last user text from an Anthropic messages body (string or content blocks).
fn last_user_text(body: &Value) -> String {
    body.get("messages")
        .and_then(|m| m.as_array())
        .map(|msgs| {
            msgs.iter()
                .rev()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                .and_then(|m| m.get("content"))
                .map(|c| match c {
                    Value::String(s) => s.clone(),
                    Value::Array(parts) => parts
                        .iter()
                        .filter_map(|p| {
                            if p.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                                p.get("content").and_then(|c| c.as_str())
                            } else {
                                p.get("text").and_then(|t| t.as_str())
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                    other => other.to_string(),
                })
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// First offered tool name, if any.
fn first_tool_name(body: &Value) -> Option<String> {
    body.get("tools")?
        .as_array()?
        .first()
        .and_then(|t| t.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_string)
}

fn tool_use_id(canon: &[u8]) -> String {
    format!("toolu_sim_{}", hash8(canon))
}

fn validate(ctx: &SimContext<'_>) -> Result<(), SimulationError> {
    let has_messages = ctx
        .body
        .get("messages")
        .and_then(|m| m.as_array())
        .is_some_and(|a| !a.is_empty());
    if !has_messages {
        return Err(SimulationError::Validation {
            status: 400,
            body: json!({"type": "error", "error": {
                "type": "invalid_request_error",
                "message": "Invalid request: 'messages' must be a non-empty array.",
            }}),
            retry_after: None,
        });
    }
    if !is_known_model(ctx.model) {
        return Err(SimulationError::Validation {
            status: 404,
            body: json!({"type": "error", "error": {
                "type": "not_found_error",
                "message": format!("The model '{}' does not exist", ctx.model),
            }}),
            retry_after: None,
        });
    }
    Ok(())
}

/// Build a non-stream `message` object.
fn non_stream(ctx: &SimContext<'_>) -> Value {
    let body = ctx.body;
    let canon = serde_json::to_vec(body).unwrap_or_default();
    let h = hash8(&canon);
    let user_text = last_user_text(body);
    let content_text = format!("Echo: {user_text}");
    // +8 framing overhead, same convention as OpenAI sim (bead sim-08 nit).
    let input_tokens = estimate_tokens(&user_text) + 8;
    let output_tokens = estimate_tokens(&content_text);
    let base = json!({
        "id": format!("msg_sim_{h}"),
        "type": "message",
        "role": "assistant",
        "model": ctx.model,
        "usage": {"input_tokens": input_tokens, "output_tokens": output_tokens},
    });
    match first_tool_name(body) {
        Some(name) => json!({
            "id": base["id"], "type": base["type"], "role": base["role"],
            "model": base["model"],
            "content": [{
                "type": "text", "text": content_text,
            }, {
                "type": "tool_use",
                "id": tool_use_id(&canon),
                "name": name,
                "input": {},
            }],
            "stop_reason": "tool_use",
            "usage": base["usage"],
        }),
        None => json!({
            "id": base["id"], "type": base["type"], "role": base["role"],
            "model": base["model"],
            "content": [{"type": "text", "text": content_text}],
            "stop_reason": "end_turn",
            "usage": base["usage"],
        }),
    }
}

/// Stream envelope: full message object + internal chunk/tool descriptors.
/// The caller renders named SSE events from this (see `sse_body()`).
fn stream_envelope(ctx: &SimContext<'_>) -> Value {
    let mut v = non_stream(ctx);
    let is_tool = v["stop_reason"].as_str() == Some("tool_use");
    if is_tool {
        if let Some(obj) = v.as_object_mut() {
            obj.insert("sim_is_tool".into(), Value::from(true));
            obj.insert(
                "sim_include_usage".into(),
                Value::from(include_usage_requested(ctx.body)),
            );
        }
        return v;
    }
    let content = v["content"][0]["text"].as_str().unwrap_or("").to_string();
    if let Some(obj) = v.as_object_mut() {
        obj.insert("sim_chunks".into(), Value::from(split_words(&content)));
        obj.insert("sim_is_tool".into(), Value::from(false));
        obj.insert(
            "sim_include_usage".into(),
            Value::from(include_usage_requested(ctx.body)),
        );
    }
    v
}

fn include_usage_requested(body: &Value) -> bool {
    // Anthropic has no stream_options; usage always rides message_delta.
    // Honor an explicit opt-out only (sim symmetry with OpenAI include_usage).
    body.get("sim_include_usage")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

/// Render the full named-event SSE body from a stream envelope.
/// Byte-deterministic: same envelope → same bytes.
pub fn sse_body(envelope: &Value) -> String {
    let id = envelope["id"].as_str().unwrap_or("msg_sim_00000000");
    let model = envelope["model"].as_str().unwrap_or("sim");
    let input_tokens = envelope["usage"]["input_tokens"].as_u64().unwrap_or(0);
    let output_tokens = envelope["usage"]["output_tokens"].as_u64().unwrap_or(0);
    let mut out = String::new();
    let mut emit = |event: &str, data: Value| {
        out.push_str(&format!(
            "event: {event}\ndata: {}\n\n",
            serde_json::to_string(&data).unwrap_or_default()
        ));
    };
    // 1. message_start
    emit(
        "message_start",
        json!({"type": "message_start", "message": {
            "id": id, "type": "message", "role": "assistant", "model": model,
            "content": [], "stop_reason": null, "stop_sequence": null,
            "usage": {"input_tokens": input_tokens, "output_tokens": 0},
        }}),
    );
    if envelope["sim_is_tool"].as_bool().unwrap_or(false) {
        // 2a–4a. tool_use block lifecycle.
        let block = &envelope["content"][1];
        let tool_id = block["id"].as_str().unwrap_or("toolu_sim_00000000");
        let tool_name = block["name"].as_str().unwrap_or("tool");
        emit(
            "content_block_start",
            json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "tool_use", "id": tool_id,
                    "name": tool_name, "input": {}}}),
        );
        emit(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": "{}"}}),
        );
        emit(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 0}),
        );
        // 5a. message_delta with tool_use stop + full usage.
        emit(
            "message_delta",
            json!({"type": "message_delta",
                "delta": {"stop_reason": "tool_use", "stop_sequence": null},
                "usage": {"input_tokens": input_tokens,
                    "output_tokens": output_tokens}}),
        );
    } else {
        // 2b. text block start.
        emit(
            "content_block_start",
            json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}}),
        );
        // 3b. text deltas.
        let chunks: Vec<String> = envelope["sim_chunks"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        for c in &chunks {
            emit(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0,
                    "delta": {"type": "text_delta", "text": c}}),
            );
        }
        // 4b. block stop.
        emit(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 0}),
        );
        // 5b. message_delta with end_turn + full usage.
        emit(
            "message_delta",
            json!({"type": "message_delta",
                "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                "usage": {"input_tokens": input_tokens,
                    "output_tokens": output_tokens}}),
        );
    }
    // 6. message_stop (usage chunk rides message_delta in Anthropic; nothing extra).
    emit("message_stop", json!({"type": "message_stop"}));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::simulation::engine::SimulationEngine;
    use std::sync::Arc;

    fn ctx<'a>(body: &'a Value) -> SimContext<'a> {
        SimContext {
            provider: "anthropic",
            model: "claude-sonnet-4-6",
            body,
            stream: false,
        }
    }

    fn msgs(text: &str) -> Value {
        json!({"model": "claude-sonnet-4-6",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": text}]})
    }

    #[tokio::test]
    async fn non_stream_schema_and_echo() {
        let engine = SimulationEngine::new().register(Arc::new(AnthropicSimulator));
        let body = msgs("hello sim");
        let v = engine
            .execute(ProviderFormat::Anthropic, "anthropic", &ctx(&body))
            .await
            .unwrap();
        assert_eq!(v["type"], "message");
        assert!(v["id"].as_str().unwrap().starts_with("msg_sim_"));
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["content"][0]["type"], "text");
        assert_eq!(v["content"][0]["text"], "Echo: hello sim");
        assert_eq!(v["stop_reason"], "end_turn");
        assert!(v["usage"]["output_tokens"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn deterministic_bytes() {
        let engine = SimulationEngine::new().register(Arc::new(AnthropicSimulator));
        let body = msgs("same");
        let a = engine
            .execute(ProviderFormat::Anthropic, "anthropic", &ctx(&body))
            .await
            .unwrap();
        let b = engine
            .execute(ProviderFormat::Anthropic, "anthropic", &ctx(&body))
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
        // Canonical: serde_json Map is BTreeMap-backed (sorted keys).
    }

    #[tokio::test]
    async fn tool_echo_both_modes() {
        let engine = SimulationEngine::new().register(Arc::new(AnthropicSimulator));
        let body = json!({"model": "claude-sonnet-4-6", "max_tokens": 64,
            "messages": [{"role": "user", "content": "search"}],
            "tools": [{"name": "web_search",
                "description": "search", "input_schema": {"type": "object"}}]});
        let v = engine
            .execute(ProviderFormat::Anthropic, "anthropic", &ctx(&body))
            .await
            .unwrap();
        assert_eq!(v["stop_reason"], "tool_use");
        assert_eq!(v["content"][1]["type"], "tool_use");
        assert_eq!(v["content"][1]["name"], "web_search");
        assert!(v["content"][1]["id"]
            .as_str()
            .unwrap()
            .starts_with("toolu_sim_"));
        // Stream variant carries the same stop + tool descriptor.
        let sctx = SimContext {
            stream: true,
            ..ctx(&body)
        };
        let env = engine
            .execute(ProviderFormat::Anthropic, "anthropic", &sctx)
            .await
            .unwrap();
        assert_eq!(env["stop_reason"], "tool_use");
        let text = sse_body(&env);
        assert!(text.contains("input_json_delta"), "tool delta");
        assert!(text.contains("tool_use"), "stop reason in delta");
    }

    #[tokio::test]
    async fn stream_named_event_order() {
        let engine = SimulationEngine::new().register(Arc::new(AnthropicSimulator));
        let body = msgs("hello world");
        let sctx = SimContext {
            stream: true,
            ..ctx(&body)
        };
        let env = engine
            .execute(ProviderFormat::Anthropic, "anthropic", &sctx)
            .await
            .unwrap();
        let text = sse_body(&env);
        let order = [
            "event: message_start",
            "event: content_block_start",
            "event: content_block_delta",
            "event: content_block_stop",
            "event: message_delta",
            "event: message_stop",
        ];
        let mut pos = 0;
        for ev in order {
            let idx = text[pos..]
                .find(ev)
                .unwrap_or_else(|| panic!("missing {ev}"));
            pos += idx + ev.len();
        }
        assert!(text.contains("text_delta"), "delta type");
        assert!(text.contains("end_turn"), "stop reason");
        assert_eq!(sse_body(&env), sse_body(&env), "deterministic");
    }

    #[tokio::test]
    async fn unknown_model_404_envelope() {
        let engine = SimulationEngine::new().register(Arc::new(AnthropicSimulator));
        let body = msgs("hi");
        let sctx = SimContext {
            model: "claude-999",
            ..ctx(&body)
        };
        let err = engine
            .execute(ProviderFormat::Anthropic, "anthropic", &sctx)
            .await
            .unwrap_err();
        match err {
            SimulationError::Validation { status, body, .. } => {
                assert_eq!(status, 404);
                assert_eq!(body["error"]["type"], "not_found_error");
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn malformed_body_400_envelope() {
        let engine = SimulationEngine::new().register(Arc::new(AnthropicSimulator));
        let body = json!({"model": "claude-sonnet-4-6", "max_tokens": 4});
        let err = engine
            .execute(ProviderFormat::Anthropic, "anthropic", &ctx(&body))
            .await
            .unwrap_err();
        match err {
            SimulationError::Validation { status, .. } => assert_eq!(status, 400),
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn compatible_format_dispatched() {
        let engine = SimulationEngine::new().register(Arc::new(AnthropicCompatibleSimulator));
        assert!(engine.supports(ProviderFormat::AnthropicCompatible));
        let body = msgs("hi");
        let v = engine
            .execute(
                ProviderFormat::AnthropicCompatible,
                "openrouter",
                &ctx(&body),
            )
            .await
            .unwrap();
        assert_eq!(v["type"], "message");
    }
}
