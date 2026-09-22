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
            return Err(SimulationError::Internal(
                "streaming not yet wired (bead sim-07)".into(),
            ));
        }
        Ok(non_stream(ctx))
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
            return Err(SimulationError::Internal(
                "streaming not yet wired (bead sim-07)".into(),
            ));
        }
        Ok(non_stream(ctx))
    }
}

/// Build a non-stream `chat.completion` object.
fn non_stream(ctx: &SimContext<'_>) -> Value {
    let body = ctx.body;
    let canon = serde_json::to_vec(body).unwrap_or_default();
    let h = hash8(&canon);
    let content = format!("Echo: {}", last_user_text(body));
    let prompt_tokens = estimate_tokens(&last_user_text(body)) + 8;
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
    async fn stream_rejected_until_sim07() {
        let engine = SimulationEngine::new().register(Arc::new(OpenAiSimulator));
        let body = json!({"model": "gpt-4o", "messages": []});
        let sctx = SimContext {
            provider: "openai",
            model: "gpt-4o",
            body: &body,
            stream: true,
        };
        let err = engine
            .execute(ProviderFormat::OpenAI, "openai", &sctx)
            .await
            .unwrap_err();
        assert!(matches!(err, SimulationError::Internal(_)));
    }
}
