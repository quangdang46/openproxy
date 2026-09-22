//! OpenAI (+OpenAI-compatible) simulator — non-stream path (bead sim-06).
//!
//! Determinism contract (plan §5): same request bytes → byte-identical
//! response. IDs and `created` derive from the request hash — no wall-clock,
//! no randomness. Default content echoes the last user message (`Echo: …`).
//!
//! SSE streaming arrives in bead sim-07; tools+errors in bead sim-08. This
//! file implements non-stream only; unknown paths return explicit errors.

use serde_json::{json, Value};

use super::engine::{
    estimate_tokens, hash8, hash_created, last_user_text, split_words, SimContext,
};
use super::error::SimulationError;
use crate::core::executor::ProviderFormat;

pub struct OpenAiSimulator;

#[async_trait::async_trait]
impl super::engine::ProviderSimulator for OpenAiSimulator {
    fn format(&self) -> ProviderFormat {
        ProviderFormat::OpenAI
    }

    async fn execute(&self, ctx: &SimContext<'_>) -> Result<Value, SimulationError> {
        validate(ctx)?;
        if ctx.stream {
            // SSE framing is applied by the caller from the content + usage in
            // this envelope (see sse_body()); the Value contract stays whole.
            Ok(stream_envelope(ctx))
        } else {
            Ok(non_stream(ctx))
        }
    }
}

/// OpenAI-compatible simulator: identical protocol, distinct format key so
/// `SimulationEngine::supports(OpenAICompatible)` is true and dispatch works
/// per-format (plan §3.6). Behavior differences are Phase 3.
pub struct OpenAiCompatibleSimulator;

#[async_trait::async_trait]
impl super::engine::ProviderSimulator for OpenAiCompatibleSimulator {
    fn format(&self) -> ProviderFormat {
        ProviderFormat::OpenAICompatible
    }

    async fn execute(&self, ctx: &SimContext<'_>) -> Result<Value, SimulationError> {
        validate(ctx)?;
        if ctx.stream {
            // SSE framing is applied by the caller from the content + usage in
            // this envelope (see sse_body()); the Value contract stays whole.
            Ok(stream_envelope(ctx))
        } else {
            Ok(non_stream(ctx))
        }
    }
}

/// Stream envelope: same ids/usage as non-stream + content chunks + flag.
/// The caller renders SSE frames from this (see sse_body()).
fn stream_envelope(ctx: &SimContext<'_>) -> Value {
    let mut v = non_stream(ctx);
    // Tool path: keep the tool_calls + finish_reason from non_stream, stash a
    // compact tool descriptor for sse_body; no content chunks in this case.
    if let Some(tool_calls) = v["choices"][0]["message"].get("tool_calls").cloned() {
        if let Some(obj) = v.as_object_mut() {
            obj.insert("sim_tool_calls".into(), tool_calls);
            obj.insert("sim_chunks".into(), Value::from(Vec::<String>::new()));
            obj.insert(
                "sim_include_usage".into(),
                Value::from(include_usage_requested(ctx.body)),
            );
        }
        return v;
    }
    let content = v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    v["choices"][0]["message"] = serde_json::json!({"role": "assistant"});
    if let Some(obj) = v.as_object_mut() {
        obj.insert("sim_chunks".into(), Value::from(split_words(&content)));
        obj.insert(
            "sim_include_usage".into(),
            Value::from(include_usage_requested(ctx.body)),
        );
    }
    v
}

fn include_usage_requested(body: &Value) -> bool {
    body.get("stream_options")
        .and_then(|o| o.get("include_usage"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Render the full SSE body (frames + terminal [DONE]) from a stream envelope.
/// Byte-deterministic: same envelope -> same bytes.
pub fn sse_body(envelope: &Value) -> String {
    let id = envelope["id"].as_str().unwrap_or("chatcmpl-sim-00000000");
    let model = envelope["model"].as_str().unwrap_or("sim");
    let created = envelope["created"].as_i64().unwrap_or(0);
    let chunks: Vec<String> = envelope["sim_chunks"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let mut out = String::new();
    // Role-establishing first chunk (OpenAI convention).
    out.push_str(&format!(
        "data: {}

",
        serde_json::json!({"id": id, "object": "chat.completion.chunk",
            "created": created, "model": model,
            "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": null}]})
    ));
    for c in &chunks {
        out.push_str(&format!(
            "data: {}

",
            serde_json::json!({"id": id, "object": "chat.completion.chunk",
                "created": created, "model": model,
                "choices": [{"index": 0, "delta": {"content": c}, "finish_reason": null}]})
        ));
    }
    // Tool path: emit tool_calls delta chunks, terminal finish_reason tool_calls.
    if let Some(tool_calls) = envelope.get("sim_tool_calls") {
        out.push_str(&format!(
            "data: {}

",
            serde_json::json!({"id": id, "object": "chat.completion.chunk",
                "created": created, "model": model,
                "choices": [{"index": 0, "delta": {"tool_calls": tool_calls},
                    "finish_reason": Value::Null}]})
        ));
        out.push_str(&format!(
            "data: {}

",
            serde_json::json!({"id": id, "object": "chat.completion.chunk",
                "created": created, "model": model,
                "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]})
        ));
    } else {
        // Terminal chunk: stop reason, no content.
        out.push_str(&format!(
            "data: {}

",
            serde_json::json!({"id": id, "object": "chat.completion.chunk",
                "created": created, "model": model,
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
        ));
    }
    // Optional usage chunk (stream_options.include_usage).
    if envelope["sim_include_usage"].as_bool().unwrap_or(false) {
        if let Some(usage) = envelope.get("usage") {
            out.push_str(&format!(
                "data: {}

",
                serde_json::json!({"id": id, "object": "chat.completion.chunk",
                    "created": created, "model": model,
                    "choices": [], "usage": usage})
            ));
        }
    }
    out.push_str(
        "data: [DONE]

",
    );
    out
}

/// Interim known-model list (bead sim-08). Bead sim-11 replaces this with
/// models.rs registry. Unknown ids → provider-correct 404 (validates error path).
fn is_known_model(model: &str) -> bool {
    const KNOWN: &[&str] = &[
        "gpt-5",
        "gpt-5-mini",
        "gpt-5-nano",
        "gpt-4o",
        "gpt-4o-mini",
        "gpt-4.1",
        "gpt-4.1-mini",
        "gpt-4-turbo",
        "gpt-4",
        "gpt-3.5-turbo",
        "o1",
        "o1-mini",
        "o3",
        "o3-mini",
        "o4-mini",
    ];
    KNOWN.contains(&model)
}

/// Validate request shape + model. Returns provider-correct rejection.
fn validate(ctx: &SimContext<'_>) -> Result<(), SimulationError> {
    let has_messages = ctx
        .body
        .get("messages")
        .and_then(|m| m.as_array())
        .is_some_and(|a| !a.is_empty());
    if !has_messages {
        return Err(SimulationError::Validation {
            status: 400,
            body: serde_json::json!({"error": {
                "message": "Invalid request: 'messages' must be a non-empty array.",
                "type": "invalid_request_error",
                "param": "messages",
                "code": Value::Null,
            }}),
            retry_after: None,
        });
    }
    if !is_known_model(ctx.model) {
        return Err(SimulationError::Validation {
            status: 404,
            body: serde_json::json!({"error": {
                "message": format!("The model '{}' does not exist", ctx.model),
                "type": "invalid_request_error",
                "param": Value::Null,
                "code": "model_not_found",
            }}),
            retry_after: None,
        });
    }
    Ok(())
}

/// First offered tool name, if any (tool *echo*, not scripted outputs).
fn first_tool_name(body: &Value) -> Option<String> {
    body.get("tools")?.as_array()?.first().and_then(|t| {
        t.get("function")
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
            .map(str::to_string)
    })
}

fn tool_call_id(canon: &[u8]) -> String {
    format!("call_sim_{}", hash8(canon))
}

/// Tool echo (NOT scripted outputs): echo the FIRST offered tool with mock
/// input (plan §5, bead sim-08). Plain-text requests keep finish_reason stop.
fn tool_echo_value(canon: &[u8], body: &Value) -> Option<Value> {
    let name = first_tool_name(body)?;
    Some(json!([{
        "id": tool_call_id(canon),
        "type": "function",
        "function": {"name": name, "arguments": "{}"},
    }]))
}

/// Build a non-stream `chat.completion` object.
fn non_stream(ctx: &SimContext<'_>) -> Value {
    let body = ctx.body;
    let canon = serde_json::to_vec(body).unwrap_or_default();
    let h = hash8(&canon);
    let user_text = last_user_text(body);
    let content = format!("Echo: {user_text}");
    // +8: per-message framing overhead approximation (role/name/primes).
    let prompt_tokens = estimate_tokens(&user_text) + 8;
    let completion_tokens = estimate_tokens(&content);
    let usage = json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": prompt_tokens + completion_tokens,
    });
    let base = json!({
        "id": format!("chatcmpl-sim-{h}"),
        "object": "chat.completion",
        "created": hash_created(&canon),
        "model": ctx.model,
    });
    match tool_echo_value(&canon, body) {
        Some(tool_calls) => json!({
            "id": base["id"], "object": base["object"],
            "created": base["created"], "model": base["model"],
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": Value::Null, "tool_calls": tool_calls},
                "finish_reason": "tool_calls",
            }],
            "usage": usage,
        }),
        None => json!({
            "id": base["id"], "object": base["object"],
            "created": base["created"], "model": base["model"],
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": content},
                "finish_reason": "stop",
            }],
            "usage": usage,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::simulation::engine::SimulationEngine;
    use std::sync::Arc;

    fn ctx<'a>(body: &'a Value) -> SimContext<'a> {
        SimContext {
            provider: "openai",
            model: "gpt-4o",
            body,
            stream: false,
        }
    }

    #[tokio::test]
    async fn non_stream_schema_and_echo() {
        let engine = SimulationEngine::new().register(Arc::new(OpenAiSimulator));
        let body = json!({"model": "gpt-4o",
            "messages": [{"role": "user", "content": "hello sim"}]});
        let v = engine
            .execute(ProviderFormat::OpenAI, "openai", &ctx(&body))
            .await
            .unwrap();
        assert_eq!(v["object"], "chat.completion");
        assert!(v["id"].as_str().unwrap().starts_with("chatcmpl-sim-"));
        assert_eq!(v["model"], "gpt-4o");
        assert_eq!(v["choices"][0]["message"]["content"], "Echo: hello sim");
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
        assert!(v["usage"]["total_tokens"].as_u64().unwrap() > 0);
        assert!(v["created"].as_i64().unwrap() > 1_700_000_000);
    }

    #[tokio::test]
    async fn deterministic_bytes() {
        let engine = SimulationEngine::new().register(Arc::new(OpenAiSimulator));
        let body = json!({"model": "gpt-4o",
            "messages": [{"role": "user", "content": "same"}]});
        let a = engine
            .execute(ProviderFormat::OpenAI, "openai", &ctx(&body))
            .await
            .unwrap();
        let b = engine
            .execute(ProviderFormat::OpenAI, "openai", &ctx(&body))
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
        // Canonical: serde_json Map is BTreeMap-backed (sorted keys), so
        // to_vec key order is stable regardless of insertion order.
    }

    #[tokio::test]
    async fn compatible_format_dispatched_per_format() {
        let engine = SimulationEngine::mvp();
        assert!(engine.supports(ProviderFormat::OpenAI));
        assert!(engine.supports(ProviderFormat::OpenAICompatible));
        let body = json!({"model": "x", "messages": [{"role": "user", "content": "hi"}]});
        let v = engine
            .execute(ProviderFormat::OpenAICompatible, "openrouter", &ctx(&body))
            .await
            .unwrap();
        assert_eq!(v["object"], "chat.completion");
        assert_eq!(v["choices"][0]["message"]["content"], "Echo: hi");
    }

    #[tokio::test]
    async fn tool_echo_non_stream() {
        let engine = SimulationEngine::mvp();
        let body = json!({"model": "gpt-4o",
            "messages": [{"role": "user", "content": "search"}],
            "tools": [{"type": "function",
                "function": {"name": "web_search", "parameters": {}}}]});
        let v = engine
            .execute(ProviderFormat::OpenAI, "openai", &ctx(&body))
            .await
            .unwrap();
        assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
        let calls = &v["choices"][0]["message"]["tool_calls"];
        assert_eq!(calls[0]["type"], "function");
        assert_eq!(calls[0]["function"]["name"], "web_search");
        assert!(calls[0]["id"].as_str().unwrap().starts_with("call_sim_"));
        // No tools -> plain text path unchanged.
        let plain = json!({"model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]});
        let pv = engine
            .execute(ProviderFormat::OpenAI, "openai", &ctx(&plain))
            .await
            .unwrap();
        assert_eq!(pv["choices"][0]["finish_reason"], "stop");
    }

    #[tokio::test]
    async fn tool_echo_stream_frames() {
        let engine = SimulationEngine::mvp();
        let body = json!({"model": "gpt-4o",
            "messages": [{"role": "user", "content": "search"}],
            "tools": [{"type": "function",
                "function": {"name": "web_search", "parameters": {}}}]});
        let sctx = SimContext {
            provider: "openai",
            model: "gpt-4o",
            body: &body,
            stream: true,
        };
        let env = engine
            .execute(ProviderFormat::OpenAI, "openai", &sctx)
            .await
            .unwrap();
        let text = sse_body(&env);
        assert!(text.contains("tool_calls"), "tool delta chunks");
        assert!(text.contains("finish_reason"), "terminal tool_calls");
        assert!(text.ends_with(
            "data: [DONE]

"
        ));
    }

    #[tokio::test]
    async fn unknown_model_404_envelope() {
        let engine = SimulationEngine::mvp();
        let body = json!({"model": "gpt-999",
            "messages": [{"role": "user", "content": "hi"}]});
        let sctx = SimContext {
            provider: "openai",
            model: "gpt-999",
            body: &body,
            stream: false,
        };
        let err = engine
            .execute(ProviderFormat::OpenAI, "openai", &sctx)
            .await
            .unwrap_err();
        match err {
            SimulationError::Validation { status, body, .. } => {
                assert_eq!(status, 404);
                assert_eq!(body["error"]["code"], "model_not_found");
                assert_eq!(body["error"]["type"], "invalid_request_error");
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn malformed_body_400_envelope() {
        let engine = SimulationEngine::mvp();
        for body in [
            json!({"model": "gpt-4o"}),
            json!({"model": "gpt-4o", "messages": []}),
        ] {
            let sctx = SimContext {
                provider: "openai",
                model: "gpt-4o",
                body: &body,
                stream: false,
            };
            let err = engine
                .execute(ProviderFormat::OpenAI, "openai", &sctx)
                .await
                .unwrap_err();
            match err {
                SimulationError::Validation { status, .. } => assert_eq!(status, 400),
                other => panic!("wrong error: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn stream_envelope_and_frames() {
        let engine = SimulationEngine::new().register(Arc::new(OpenAiSimulator));
        let body = json!({"model": "gpt-4o",
            "messages": [{"role": "user", "content": "hello world"}],
            "stream_options": {"include_usage": true}});
        let sctx = SimContext {
            provider: "openai",
            model: "gpt-4o",
            body: &body,
            stream: true,
        };
        let env = engine
            .execute(ProviderFormat::OpenAI, "openai", &sctx)
            .await
            .unwrap();
        let text = sse_body(&env);
        // Frame order: role chunk, content chunks, stop chunk, usage chunk, DONE.
        let frames: Vec<&str> = text
            .split(
                "

",
            )
            .filter(|f| !f.is_empty())
            .collect();
        assert!(frames[0].contains("role"), "first frame role");
        assert!(
            text.contains("Echo: hello world") == false,
            "content must be chunked"
        );
        assert!(text.contains("hello"), "chunk content present");
        assert!(text.contains("finish_reason"), "terminal stop");
        assert!(text.contains("usage"), "include_usage chunk");
        assert!(
            text.ends_with(
                "data: [DONE]

"
            ),
            "DONE terminal"
        );
        // Determinism: same envelope -> same bytes.
        assert_eq!(sse_body(&env), sse_body(&env));
        // Without include_usage: no usage chunk.
        let body2 = json!({"model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]});
        let sctx2 = SimContext {
            provider: "openai",
            model: "gpt-4o",
            body: &body2,
            stream: true,
        };
        let env2 = engine
            .execute(ProviderFormat::OpenAI, "openai", &sctx2)
            .await
            .unwrap();
        assert!(!sse_body(&env2).contains("usage"), "no usage without flag");
    }
}
