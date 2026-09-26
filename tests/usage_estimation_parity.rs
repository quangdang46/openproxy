//! Bead openproxy-4inl.2 — terminal-frame usage when the provider reports none.
//!
//! A `stream_options.include_usage` block is opt-in, so by default the upstream
//! stream carries no usage at all. 9router substitutes an estimate on the
//! terminal frame (stream.js:356-366 → `estimateUsage`) and pads every
//! client-visible block with 2000 tokens of context head-room
//! (`addBufferToUsage`). OpenProxy emitted the literal `{input_tokens: 0,
//! output_tokens: 0}`, so every streamed request metered as free.
//!
//! The buffer must not reach the value that gets persisted: 9router pads only
//! the copy on the wire, because the unbuffered one is what cost and stats
//! are computed from.

use openproxy::core::translator::registry::ResponseTransformState;
use openproxy::core::translator::response::claude_to_openai::claude_to_openai_streaming;
use openproxy::core::translator::response::openai_to_claude::openai_to_claude_streaming;
use openproxy::core::translator::response::usage::{estimate_input_tokens, BUFFER_TOKENS};
use serde_json::{json, Value};

/// Feed Claude SSE events through the streaming transform and return the
/// parsed `data:` payloads.
fn claude_to_openai(events: &[Value], request_body: Option<Value>) -> Vec<Value> {
    let mut state = ResponseTransformState::default();
    state.request_body = request_body;

    events
        .iter()
        .flat_map(|event| {
            claude_to_openai_streaming(format!("data: {event}\n\n").as_bytes(), &mut state)
        })
        .filter_map(|line| {
            line.trim()
                .strip_prefix("data: ")
                .filter(|payload| *payload != "[DONE]")
                .and_then(|payload| serde_json::from_str::<Value>(payload).ok())
        })
        .collect()
}

/// Feed OpenAI `chat.completion.chunk` frames through the streaming transform
/// and return the parsed Claude SSE events.
fn openai_to_claude(chunks: &[Value], request_body: Option<Value>) -> Vec<Value> {
    let mut state = ResponseTransformState::default();
    state.request_body = request_body;

    chunks
        .iter()
        .flat_map(|chunk| {
            openai_to_claude_streaming(format!("data: {chunk}\n\n").as_bytes(), &mut state)
        })
        .filter_map(|line| {
            line.lines()
                .find_map(|l| l.trim().strip_prefix("data: "))
                .and_then(|payload| serde_json::from_str::<Value>(payload).ok())
        })
        .collect()
}

fn final_chunk(chunks: &[Value]) -> &Value {
    chunks.last().expect("at least one chunk")
}

fn message_delta(events: &[Value]) -> &Value {
    events
        .iter()
        .find(|e| e["type"] == "message_delta")
        .expect("a terminal message_delta")
}

#[test]
fn terminal_frame_carries_estimated_usage_when_the_provider_is_silent() {
    // A Claude-source stream whose message_delta carries no usage block — the
    // shape every provider emits unless the client asked for usage.
    let chunks = claude_to_openai(
        &[
            json!({"type": "message_start", "message": {"id": "m", "model": "claude"}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "The quick brown fox jumps over the lazy dog."}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
            json!({"type": "message_stop"}),
        ],
        Some(json!({"model": "claude", "messages": [{"role": "user", "content": "hi"}]})),
    );

    let usage = &final_chunk(&chunks)["usage"];
    // 44 characters of text → 11 output tokens. Zero would mean the request
    // meters as free.
    assert_eq!(usage["completion_tokens"], 11);
    assert_eq!(usage["estimated"], true);
    // The input half comes from the request body, plus the +2000 head-room
    // 9router pads every client-visible block with.
    let request = json!({"model": "claude", "messages": [{"role": "user", "content": "hi"}]});
    assert_eq!(
        usage["prompt_tokens"].as_u64().unwrap(),
        estimate_input_tokens(Some(&request)) + BUFFER_TOKENS
    );
}

#[test]
fn claude_terminal_frame_buffers_the_client_copy_but_not_the_stored_one() {
    let mut state = ResponseTransformState::default();
    let events = [
        json!({"type": "message_start", "message": {"id": "m", "model": "claude"}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "hello"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"input_tokens": 120, "output_tokens": 45}}),
        json!({"type": "message_stop"}),
    ];

    let mut emitted: Option<Value> = None;
    for event in &events {
        for line in claude_to_openai_streaming(format!("data: {event}\n\n").as_bytes(), &mut state)
        {
            if let Some(payload) = line.trim().strip_prefix("data: ") {
                if let Ok(value) = serde_json::from_str::<Value>(payload) {
                    emitted = Some(value);
                }
            }
        }
    }

    let usage = &emitted.expect("a terminal chunk")["usage"];
    assert_eq!(usage["prompt_tokens"], 2120);
    // completion_tokens is deliberately unpadded — that is 9router's rule, and
    // the padded total still reads as prompt + completion.
    assert_eq!(usage["completion_tokens"], 45);
    assert_eq!(usage["total_tokens"], 2165);

    // What the cost/stats path reads is the provider's own number, untouched.
    assert_eq!(state.anthropic.claude_state["usage"]["prompt_tokens"], 120);
    assert_eq!(state.anthropic.claude_state["usage"]["total_tokens"], 165);
}

#[test]
fn openai_to_claude_terminal_frame_estimates_when_usage_is_absent() {
    let events = openai_to_claude(
        &[
            json!({"id": "chatcmpl-1", "model": "gpt-4o", "choices": [{"index": 0, "delta": {"role": "assistant", "content": "hello there friend"}}]}),
            json!({"id": "chatcmpl-1", "model": "gpt-4o", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
        ],
        Some(json!({"model": "gpt-4o", "messages": []})),
    );

    let request = json!({"model": "gpt-4o", "messages": []});
    let usage = &message_delta(&events)["usage"];
    assert_eq!(usage["output_tokens"], 4);
    assert_eq!(usage["estimated"], true);
    assert_eq!(
        usage["input_tokens"].as_u64().unwrap(),
        estimate_input_tokens(Some(&request)) + BUFFER_TOKENS
    );
}

#[test]
fn openai_to_claude_keeps_the_zero_literal_when_there_was_no_content() {
    // 9router leaves the translator's own 0/0 literal in place when it has
    // nothing to estimate from.
    let events = openai_to_claude(
        &[
            json!({"id": "chatcmpl-1", "model": "gpt-4o", "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": "stop"}]}),
        ],
        None,
    );

    assert_eq!(
        message_delta(&events)["usage"],
        json!({"input_tokens": 0, "output_tokens": 0})
    );
}
