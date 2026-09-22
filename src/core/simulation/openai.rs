//! OpenAI (+OpenAI-compatible) simulator — non-stream path (bead sim-06).
//!
//! Determinism contract (plan §5): same request bytes → byte-identical
//! response. IDs and `created` derive from the request hash — no wall-clock,
//! no randomness. Default content echoes the last user message (`Echo: …`).
//!
//! SSE streaming arrives in bead sim-07; tools+errors in bead sim-08. This
//! file implements non-stream only; unknown paths return explicit errors.

use serde_json::{json, Value};

use super::engine::{estimate_tokens, hash8, hash_created, last_user_text, SimContext};
use super::error::SimulationError;
use crate::core::executor::ProviderFormat;

pub struct OpenAiSimulator;

#[async_trait::async_trait]
impl super::engine::ProviderSimulator for OpenAiSimulator {
    fn format(&self) -> ProviderFormat {
        ProviderFormat::OpenAI
    }

    async fn execute(&self, ctx: &SimContext<'_>) -> Result<Value, SimulationError> {
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
        if ctx.stream {
            // SSE framing is applied by the caller from the content + usage in
            // this envelope (see sse_body()); the Value contract stays whole.
            Ok(stream_envelope(ctx))
        } else {
            Ok(non_stream(ctx))
        }
    }
}

/// Split content into word-boundary chunks (deterministic, no randomness).
fn split_words(content: &str) -> Vec<String> {
    let words: Vec<&str> = content.split_inclusive(char::is_whitespace).collect();
    if words.is_empty() && !content.is_empty() {
        return vec![content.to_string()];
    }
    // Merge tiny pieces so chunks look like real token streaming (1-3 words).
    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut n = 0;
    for w in words {
        cur.push_str(w);
        n += 1;
        if n >= 2 {
            chunks.push(std::mem::take(&mut cur));
            n = 0;
        }
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

/// Stream envelope: same ids/usage as non-stream + content chunks + flag.
/// The caller renders SSE frames from this (see sse_body()).
fn stream_envelope(ctx: &SimContext<'_>) -> Value {
    let mut v = non_stream(ctx);
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
    // Terminal chunk: stop reason, no content.
    out.push_str(&format!(
        "data: {}

",
        serde_json::json!({"id": id, "object": "chat.completion.chunk",
            "created": created, "model": model,
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
    ));
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
    json!({
        "id": format!("chatcmpl-sim-{h}"),
        "object": "chat.completion",
        "created": hash_created(&canon),
        "model": ctx.model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop",
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        },
    })
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
        assert!(!engine.supports(ProviderFormat::Gemini));
        let body = json!({"model": "x", "messages": [{"role": "user", "content": "hi"}]});
        let v = engine
            .execute(ProviderFormat::OpenAICompatible, "openrouter", &ctx(&body))
            .await
            .unwrap();
        assert_eq!(v["object"], "chat.completion");
        assert_eq!(v["choices"][0]["message"]["content"], "Echo: hi");
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
