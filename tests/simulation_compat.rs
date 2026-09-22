//! Provider compatibility contract (bead sim-18, plan §5.2).
//!
//! Pinned-shape gate: for every fixture in
//! `tests/simulation_compat_fixtures/requests.json`, the engine output must
//! keep the documented protocol shape — HTTP-mapped status, JSON schema
//! (required keys), SSE event ordering, tool-call shape, usage shape, error
//! shape — for non-stream AND stream, across OpenAI / Anthropic / Gemini.
//!
//! This is a CONTRACT test, not a behavior duplicate of unit tests: it locks
//! the wire shapes an external oracle (LLMock, `scripts/sim-compat.sh`)
//! diffs against. If this file fails, either the engine drifted or the
//! fixtures need a deliberate update (never silent).
//!
//! Known oracle notes (reviewer sim-14/18): single-delta tool streaming,
//! single tool_use block at index 0, `is_terminal` substring edge — the
//! oracle may flag fidelity there; contract asserts structure, not realism.

use openproxy::core::executor::ProviderFormat;
use openproxy::core::simulation::{SimContext, SimulationEngine};
use serde_json::Value;

fn fixtures() -> Value {
    let text = include_str!("simulation_compat_fixtures/requests.json");
    serde_json::from_str(text).expect("fixtures parse")
}

fn format_of(s: &str) -> ProviderFormat {
    match s {
        "openai" => ProviderFormat::OpenAI,
        "anthropic" => ProviderFormat::Anthropic,
        "gemini" => ProviderFormat::Gemini,
        other => panic!("unknown format {other}"),
    }
}

fn require_keys(v: &Value, keys: &[&str], ctx: &str) {
    let obj = v
        .as_object()
        .unwrap_or_else(|| panic!("{ctx} not an object"));
    for k in keys {
        assert!(obj.contains_key(*k), "{ctx} missing key {k}");
    }
}

#[tokio::test]
async fn contract_shapes_hold_for_all_fixtures() {
    let engine = SimulationEngine::mvp();
    let fixtures = fixtures();
    let requests = fixtures["requests"].as_array().unwrap();
    assert!(!requests.is_empty(), "fixtures must not be empty");

    for req in requests {
        let id = req["id"].as_str().unwrap();
        let format = format_of(req["format"].as_str().unwrap());
        let model = req["model"].as_str().unwrap();
        let stream = req["stream"].as_bool().unwrap();
        let body = &req["body"];
        let ctx = SimContext {
            provider: "compat-test",
            model,
            body,
            stream,
        };
        let out = engine
            .execute(format, "compat-test", &ctx)
            .await
            .unwrap_or_else(|e| panic!("{id}: engine failed: {e:?}"));

        match req["format"].as_str().unwrap() {
            "openai" => {
                require_keys(
                    &out,
                    &["id", "object", "created", "model", "choices", "usage"],
                    id,
                );
                assert_eq!(out["object"], "chat.completion");
                require_keys(
                    &out["choices"][0],
                    &["index", "message", "finish_reason"],
                    id,
                );
                require_keys(
                    &out["usage"],
                    &["prompt_tokens", "completion_tokens", "total_tokens"],
                    id,
                );
                if id == "openai-tools" {
                    assert_eq!(out["choices"][0]["finish_reason"], "tool_calls");
                    require_keys(
                        &out["choices"][0]["message"]["tool_calls"][0],
                        &["id", "type", "function"],
                        id,
                    );
                }
                if stream {
                    let sse = openproxy::core::simulation::sse_body_openai(&out);
                    assert!(sse.contains("chat.completion.chunk"), "{id} chunks");
                    assert!(sse.ends_with("data: [DONE]\n\n"), "{id} DONE");
                    if id == "openai-stream" {
                        assert!(sse.contains("\"usage\""), "{id} usage chunk");
                    }
                }
            }
            "anthropic" => {
                require_keys(
                    &out,
                    &[
                        "id",
                        "type",
                        "role",
                        "model",
                        "content",
                        "stop_reason",
                        "usage",
                    ],
                    id,
                );
                assert_eq!(out["type"], "message");
                require_keys(&out["usage"], &["input_tokens", "output_tokens"], id);
                if id == "anthropic-tools" {
                    assert_eq!(out["stop_reason"], "tool_use");
                    assert_eq!(out["content"][1]["type"], "tool_use");
                }
                if stream {
                    let sse = openproxy::core::simulation::sse_body_anthropic(&out);
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
                        let idx = sse[pos..]
                            .find(ev)
                            .unwrap_or_else(|| panic!("{id} missing {ev}"));
                        pos += idx + ev.len();
                    }
                }
            }
            "gemini" => {
                require_keys(&out, &["candidates", "usageMetadata", "modelVersion"], id);
                require_keys(&out["candidates"][0]["content"], &["parts", "role"], id);
                assert_eq!(out["candidates"][0]["finishReason"], "STOP");
                require_keys(
                    &out["usageMetadata"],
                    &[
                        "promptTokenCount",
                        "candidatesTokenCount",
                        "totalTokenCount",
                    ],
                    id,
                );
                if stream {
                    let sse = openproxy::core::simulation::sse_body_gemini(&out);
                    assert!(sse.contains("candidates"), "{id} chunks");
                    assert!(sse.contains("STOP"), "{id} terminal");
                    assert!(!sse.contains("[DONE]"), "{id} no OpenAI marker");
                }
            }
            other => panic!("unhandled format {other}"),
        }
    }
}

#[tokio::test]
async fn contract_error_shapes() {
    let engine = SimulationEngine::mvp();
    // Unknown model per format → provider-correct status + envelope keys.
    for (format, model, body, status_key) in [
        (
            ProviderFormat::OpenAI,
            "gpt-999",
            serde_json::json!({"model": "gpt-999",
                "messages": [{"role": "user", "content": "hi"}]}),
            ("error", "model_not_found"),
        ),
        (
            ProviderFormat::Anthropic,
            "claude-999",
            serde_json::json!({"model": "claude-999", "max_tokens": 4,
                "messages": [{"role": "user", "content": "hi"}]}),
            ("error", "not_found_error"),
        ),
        (
            ProviderFormat::Gemini,
            "gemini-999",
            serde_json::json!({"contents": [{"parts": [{"text": "hi"}]}]}),
            ("error", "NOT_FOUND"),
        ),
    ] {
        let ctx = SimContext {
            provider: "compat-test",
            model,
            body: &body,
            stream: false,
        };
        let err = engine
            .execute(format, "compat-test", &ctx)
            .await
            .unwrap_err();
        match err {
            openproxy::core::simulation::SimulationError::Validation { status, body, .. } => {
                assert_eq!(status, 404);
                let (outer, inner) = status_key;
                if format == ProviderFormat::Anthropic {
                    assert_eq!(body["type"], "error");
                    assert_eq!(body["error"]["type"], inner);
                } else if format == ProviderFormat::Gemini {
                    assert_eq!(body[outer]["status"], inner);
                } else {
                    assert_eq!(body[outer]["code"], inner);
                }
            }
            other => panic!("wrong error: {other:?}"),
        }
    }
}
