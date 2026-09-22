//! Gemini simulator — non-stream + stream (bead sim-10).
//!
//! Determinism contract (plan §5): same request bytes → byte-identical
//! response. No wall-clock, no randomness.
//!
//! Request shape (Google AI Studio): `{contents:[{parts:[{text}]}], ...}`.
//! Non-stream: `{candidates:[{content:{parts:[{text}], role:"model"},
//! finishReason:"STOP"}], usageMetadata:{...}}`.
//! Stream: SSE `data:` chunks in Gemini shape (one JSON per chunk, same
//! candidates envelope, partial text), no `[DONE]` (Gemini terminates the
//! stream; our caller closes the body).
//! Errors: `{error:{code, message, status}}` (400 INVALID_ARGUMENT,
//! 404 NOT_FOUND).

use serde_json::{json, Value};

use super::engine::{estimate_tokens, hash8, split_words, SimContext};
use super::error::SimulationError;
use crate::core::executor::ProviderFormat;

pub struct GeminiSimulator;

#[async_trait::async_trait]
impl super::engine::ProviderSimulator for GeminiSimulator {
    fn format(&self) -> ProviderFormat {
        ProviderFormat::Gemini
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

/// Interim known-model list (bead sim-10). Bead sim-11 replaces with models.rs.
fn is_known_model(model: &str) -> bool {
    const KNOWN: &[&str] = &[
        "gemini-2.5-pro",
        "gemini-2.5-flash",
        "gemini-2.5-flash-lite",
        "gemini-2.0-flash",
        "gemini-2.0-flash-lite",
        "gemini-1.5-pro",
        "gemini-1.5-flash",
    ];
    KNOWN.contains(&model)
}

/// Last user text from a Gemini contents body.
fn last_user_text(body: &Value) -> String {
    body.get("contents")
        .and_then(|c| c.as_array())
        .map(|contents| {
            contents
                .iter()
                .rev()
                .filter(|c| c.get("role").and_then(|r| r.as_str()) != Some("model"))
                .flat_map(|c| {
                    c.get("parts")
                        .and_then(|p| p.as_array())
                        .cloned()
                        .unwrap_or_default()
                })
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()).map(str::to_string))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

fn validate(ctx: &SimContext<'_>) -> Result<(), SimulationError> {
    let has_contents = ctx
        .body
        .get("contents")
        .and_then(|c| c.as_array())
        .is_some_and(|a| !a.is_empty());
    if !has_contents {
        return Err(SimulationError::Validation {
            status: 400,
            body: json!({"error": {
                "code": 400,
                "message": "Invalid request: 'contents' must be a non-empty array.",
                "status": "INVALID_ARGUMENT",
            }}),
            retry_after: None,
        });
    }
    if !is_known_model(ctx.model) {
        return Err(SimulationError::Validation {
            status: 404,
            body: json!({"error": {
                "code": 404,
                "message": format!("Model '{}' not found.", ctx.model),
                "status": "NOT_FOUND",
            }}),
            retry_after: None,
        });
    }
    Ok(())
}

/// Build a non-stream `generateContent` response.
fn non_stream(ctx: &SimContext<'_>) -> Value {
    let user_text = last_user_text(ctx.body);
    let content = format!("Echo: {user_text}");
    // +8 framing overhead, same convention as OpenAI/Anthropic sims.
    let prompt = estimate_tokens(&user_text) + 8;
    let candidates = estimate_tokens(&content);
    json!({
        "candidates": [{
            "content": {"parts": [{"text": content}], "role": "model"},
            "finishReason": "STOP",
            "index": 0,
        }],
        "usageMetadata": {
            "promptTokenCount": prompt,
            "candidatesTokenCount": candidates,
            "totalTokenCount": prompt + candidates,
        },
        "modelVersion": ctx.model,
    })
}

/// Stream envelope: full response + internal chunk list for `sse_body()`.
fn stream_envelope(ctx: &SimContext<'_>) -> Value {
    let mut v = non_stream(ctx);
    let content = v["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string();
    if let Some(obj) = v.as_object_mut() {
        obj.insert("sim_chunks".into(), Value::from(split_words(&content)));
    }
    v
}

/// Render SSE body: one Gemini-shape JSON per chunk + final STOP chunk.
/// Byte-deterministic: same envelope → same bytes. No `[DONE]` (Gemini
/// convention: stream close terminates; cf. OpenAI `[DONE]`, Anthropic
/// `message_stop`).
pub fn sse_body(envelope: &Value) -> String {
    let model = envelope["modelVersion"].as_str().unwrap_or("sim");
    let usage = envelope
        .get("usageMetadata")
        .cloned()
        .unwrap_or(Value::Null);
    let chunks: Vec<String> = envelope["sim_chunks"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let mut out = String::new();
    for c in &chunks {
        out.push_str(&format!(
            "data: {}\n\n",
            serde_json::json!({"candidates": [{
                "content": {"parts": [{"text": c}], "role": "model"},
                "index": 0,
            }], "modelVersion": model})
        ));
    }
    // Final chunk carries finishReason + usage.
    out.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"candidates": [{
            "content": {"parts": [], "role": "model"},
            "finishReason": "STOP", "index": 0,
        }], "usageMetadata": usage, "modelVersion": model})
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::simulation::engine::SimulationEngine;
    use std::sync::Arc;

    fn ctx<'a>(body: &'a Value) -> SimContext<'a> {
        SimContext {
            provider: "gemini",
            model: "gemini-2.5-flash",
            body,
            stream: false,
        }
    }

    fn contents(text: &str) -> Value {
        json!({"contents": [{"parts": [{"text": text}]}]})
    }

    #[tokio::test]
    async fn non_stream_schema_and_echo() {
        let engine = SimulationEngine::new().register(Arc::new(GeminiSimulator));
        let body = contents("hello sim");
        let v = engine
            .execute(ProviderFormat::Gemini, "gemini", &ctx(&body))
            .await
            .unwrap();
        assert_eq!(
            v["candidates"][0]["content"]["parts"][0]["text"],
            "Echo: hello sim"
        );
        assert_eq!(v["candidates"][0]["content"]["role"], "model");
        assert_eq!(v["candidates"][0]["finishReason"], "STOP");
        assert!(v["usageMetadata"]["totalTokenCount"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn deterministic_bytes() {
        let engine = SimulationEngine::new().register(Arc::new(GeminiSimulator));
        let body = contents("same");
        let a = engine
            .execute(ProviderFormat::Gemini, "gemini", &ctx(&body))
            .await
            .unwrap();
        let b = engine
            .execute(ProviderFormat::Gemini, "gemini", &ctx(&body))
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
        // Canonical: serde_json Map is BTreeMap-backed (sorted keys).
    }

    #[tokio::test]
    async fn stream_chunks_and_terminal() {
        let engine = SimulationEngine::new().register(Arc::new(GeminiSimulator));
        let body = contents("hello world");
        let sctx = SimContext {
            stream: true,
            ..ctx(&body)
        };
        let env = engine
            .execute(ProviderFormat::Gemini, "gemini", &sctx)
            .await
            .unwrap();
        let text = sse_body(&env);
        assert!(text.contains("hello"), "chunk content");
        assert!(text.contains("\"finishReason\":\"STOP\""), "terminal");
        assert!(text.contains("usageMetadata"), "usage in terminal");
        assert!(!text.contains("[DONE]"), "no OpenAI DONE marker");
        assert_eq!(sse_body(&env), sse_body(&env), "deterministic");
    }

    #[tokio::test]
    async fn unknown_model_404_envelope() {
        let engine = SimulationEngine::new().register(Arc::new(GeminiSimulator));
        let body = contents("hi");
        let sctx = SimContext {
            model: "gemini-999",
            ..ctx(&body)
        };
        let err = engine
            .execute(ProviderFormat::Gemini, "gemini", &sctx)
            .await
            .unwrap_err();
        match err {
            SimulationError::Validation { status, body, .. } => {
                assert_eq!(status, 404);
                assert_eq!(body["error"]["status"], "NOT_FOUND");
                assert_eq!(body["error"]["code"], 404);
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn malformed_body_400_envelope() {
        let engine = SimulationEngine::new().register(Arc::new(GeminiSimulator));
        let body = json!({});
        let err = engine
            .execute(ProviderFormat::Gemini, "gemini", &ctx(&body))
            .await
            .unwrap_err();
        match err {
            SimulationError::Validation { status, body, .. } => {
                assert_eq!(status, 400);
                assert_eq!(body["error"]["status"], "INVALID_ARGUMENT");
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_mode_validation_rejected() {
        // Stream + unknown model: validation runs before envelope (JSON error,
        // not SSE) — same contract as OpenAI/Anthropic sims.
        let engine = SimulationEngine::new().register(Arc::new(GeminiSimulator));
        let body = contents("hi");
        let sctx = SimContext {
            model: "gemini-999",
            stream: true,
            ..ctx(&body)
        };
        let err = engine
            .execute(ProviderFormat::Gemini, "gemini", &sctx)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            SimulationError::Validation { status: 404, .. }
        ));
    }
}
