//! Token-usage estimation and per-format shaping.
//!
//! Port of `open-sse/utils/usageTracking.js`. A provider that streams without
//! a `usage` block — the default, since `stream_options.include_usage` is off
//! unless the client asks for it — would otherwise leave the terminal frame at
//! 0/0 and every streamed request would meter as free. 9router fills that hole
//! with a ~4-characters-per-token estimate and pads every client-visible block
//! with [`BUFFER_TOKENS`] so a follow-up request built from the transcript
//! cannot overflow the provider's context window.

use serde_json::{json, Map, Value};

use crate::core::translator::registry::Format;

/// Head-room added to every client-visible usage block. 9router keeps this OFF
/// the value it persists for cost/stats, so the buffer never inflates a bill.
pub const BUFFER_TOKENS: u64 = 2000;

/// Per-stream scratch key holding the running count of emitted assistant text.
const TOTAL_CONTENT_LENGTH: &str = "totalContentLength";

/// Per-stream scratch key holding the request this stream answers, when the
/// caller offered one. Only sizes the input half of an estimate.
pub const REQUEST_BODY: &str = "requestBody";

/// Token fields whose presence with a value above zero makes a usage block
/// "valid" (9router `hasValidUsage`) — the test the terminal-frame rule uses to
/// decide between forwarding the provider's number and estimating one.
const TOKEN_FIELDS: &[&str] = &[
    "prompt_tokens",
    "completion_tokens",
    "total_tokens",
    "input_tokens",
    "output_tokens",
    "promptTokenCount",
    "candidatesTokenCount",
];

const CLAUDE_FIELDS: &[&str] = &[
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
    "estimated",
];

const GEMINI_FIELDS: &[&str] = &[
    "promptTokenCount",
    "candidatesTokenCount",
    "totalTokenCount",
    "cachedContentTokenCount",
    "thoughtsTokenCount",
    "estimated",
];

const RESPONSES_FIELDS: &[&str] = &[
    "input_tokens",
    "output_tokens",
    "input_tokens_details",
    "output_tokens_details",
    "estimated",
];

const OPENAI_FIELDS: &[&str] = &[
    "prompt_tokens",
    "completion_tokens",
    "total_tokens",
    "cached_tokens",
    "reasoning_tokens",
    "prompt_tokens_details",
    "completion_tokens_details",
    "estimated",
];

/// Character count the way 9router's `String#length` sees it. The estimates
/// divide this by four, so a request written in a non-Latin script would be
/// over-counted by ~3x if we used Rust's byte length instead.
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// True when `usage` carries at least one token field above zero.
pub fn has_valid_usage(usage: &Value) -> bool {
    TOKEN_FIELDS
        .iter()
        .any(|f| usage.get(*f).and_then(Value::as_u64).is_some_and(|n| n > 0))
}

/// ~4 characters per token across the request body, rounded up (9router
/// `estimateInputTokens`).
pub fn estimate_input_tokens(body: Option<&Value>) -> u64 {
    // JS `typeof body !== "object"` rejects null and primitives; arrays pass.
    let Some(body) = body.filter(|b| b.is_object() || b.is_array()) else {
        return 0;
    };
    let Ok(serialized) = serde_json::to_string(body) else {
        return 0;
    };
    let chars = utf16_len(&serialized);
    chars.div_ceil(4) as u64
}

/// ~4 characters per token of emitted content, never below 1 for a non-empty
/// stream (9router `estimateOutputTokens`).
pub fn estimate_output_tokens(content_length: usize) -> u64 {
    if content_length == 0 {
        return 0;
    }
    (content_length / 4).max(1) as u64
}

/// Build a usage block in `target`'s shape and pad it. `completion_tokens` is
/// deliberately left alone: 9router's `addBufferToUsage` pads input and total
/// only, so a padded block still reads as prompt+completion.
pub fn format_usage(input_tokens: u64, output_tokens: u64, target: Format) -> Value {
    let usage = if target == Format::Claude {
        json!({
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "estimated": true,
        })
    } else {
        json!({
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens,
            "estimated": true,
        })
    };
    add_buffer_to_usage(&usage)
}

/// Full estimate for a stream whose provider reported no usage at all.
pub fn estimate_usage(body: Option<&Value>, content_length: usize, target: Format) -> Value {
    format_usage(
        estimate_input_tokens(body),
        estimate_output_tokens(content_length),
        target,
    )
}

fn bump(usage: &mut Map<String, Value>, key: &str) -> bool {
    let Some(current) = usage.get(key).and_then(Value::as_u64) else {
        return false;
    };
    usage.insert(key.to_string(), Value::from(current + BUFFER_TOKENS));
    true
}

/// Add [`BUFFER_TOKENS`] to input/prompt/total, deriving `total_tokens` from
/// prompt+completion when the provider left it out (9router
/// `addBufferToUsage`).
pub fn add_buffer_to_usage(usage: &Value) -> Value {
    let Some(mut result) = usage.as_object().cloned() else {
        return usage.clone();
    };

    bump(&mut result, "input_tokens");
    bump(&mut result, "prompt_tokens");
    if !bump(&mut result, "total_tokens")
        && result.contains_key("prompt_tokens")
        && result.contains_key("completion_tokens")
    {
        // Derived from the ALREADY padded prompt, matching the JS ordering.
        let total = result["prompt_tokens"].as_u64().unwrap_or(0)
            + result["completion_tokens"].as_u64().unwrap_or(0);
        result.insert("total_tokens".into(), Value::from(total));
    }

    Value::Object(result)
}

/// Keep only the fields `target`'s wire format defines (9router
/// `filterUsageForFormat`). Gemini-cli and Antigravity share Gemini's list;
/// openai-response shares openai-responses'; everything else — including
/// Vertex, which 9router leaves on the OpenAI list — gets the OpenAI one.
pub fn filter_usage_for_format(usage: &Value, target: Format) -> Value {
    let fields = match target {
        Format::Claude => CLAUDE_FIELDS,
        Format::Gemini | Format::GeminiCli | Format::Antigravity => GEMINI_FIELDS,
        Format::OpenAiResponses | Format::OpenAiResponse => RESPONSES_FIELDS,
        _ => OPENAI_FIELDS,
    };

    let mut filtered = Map::new();
    for field in fields {
        if let Some(value) = usage.get(*field) {
            filtered.insert((*field).to_string(), value.clone());
        }
    }
    Value::Object(filtered)
}

/// Fold emitted assistant text into the counter that sizes the output half of
/// an estimate. 9router accumulates the raw source deltas (stream.js:293-300),
/// not the text that survives translation.
pub fn note_content(state: &mut Map<String, Value>, text: &str) {
    if text.is_empty() {
        return;
    }
    let running = state
        .get(TOTAL_CONTENT_LENGTH)
        .and_then(Value::as_u64)
        .unwrap_or(0);
    state.insert(
        TOTAL_CONTENT_LENGTH.into(),
        Value::from(running + utf16_len(text) as u64),
    );
}

/// Emitted assistant characters seen so far, in UTF-16 units.
pub fn content_length(state: &Map<String, Value>) -> usize {
    state
        .get(TOTAL_CONTENT_LENGTH)
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize
}

/// Copy the request body into the per-stream scratch map, once.
pub fn seed_request_body(state: &mut Map<String, Value>, body: Option<&Value>) {
    if let Some(body) = body {
        state
            .entry(REQUEST_BODY.to_string())
            .or_insert_with(|| body.clone());
    }
}

/// The usage block a client sees on a terminal frame (9router stream.js:356-366).
///
/// `tracked` is what the stream accumulated, already expressed in `target`'s
/// shape, or `None` when the provider sent no usage at all. An estimate is
/// substituted when `tracked` carries no usable token count AND the stream
/// produced content; otherwise `tracked` is forwarded with head-room. `None`
/// means there is nothing to say — the caller keeps whatever it would have
/// emitted without a usage block.
///
/// `state["usage"]` is left unbuffered: 9router persists that one for
/// cost/stats, and padding it there would bill the client for head-room.
pub fn terminal_usage_block(
    state: &mut Map<String, Value>,
    target: Format,
    tracked: Option<Value>,
) -> Option<Value> {
    if !tracked.as_ref().is_some_and(has_valid_usage) && content_length(state) > 0 {
        let estimated = estimate_usage(state.get(REQUEST_BODY), content_length(state), target);
        state.insert("usage".into(), estimated.clone());
        return Some(filter_usage_for_format(&estimated, target));
    }

    tracked.map(|usage| filter_usage_for_format(&add_buffer_to_usage(&usage), target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_usage_matches_9router_math() {
        // Pad the body to exactly 400 serialized characters so the arithmetic
        // is checkable by hand: 400/4 = 100 in, 40/4 = 10 out. 9router's
        // addBufferToUsage pads prompt and total, never completion.
        let body = json!({"pad": "a".repeat(400 - "{\"pad\":\"\"}".len())});
        assert_eq!(serde_json::to_string(&body).unwrap().len(), 400);

        let usage = estimate_usage(Some(&body), 40, Format::OpenAi);

        assert_eq!(usage["prompt_tokens"], 2100);
        assert_eq!(usage["completion_tokens"], 10);
        assert_eq!(usage["total_tokens"], 2110);
        assert_eq!(usage["estimated"], true);
    }

    #[test]
    fn format_usage_is_claude_shaped_for_claude_clients() {
        let usage = format_usage(100, 10, Format::Claude);
        assert_eq!(usage["input_tokens"], 2100);
        assert_eq!(usage["output_tokens"], 10);
        assert_eq!(usage["estimated"], true);
        assert!(usage.get("prompt_tokens").is_none());
    }

    #[test]
    fn add_buffer_does_not_double_total() {
        let usage = add_buffer_to_usage(&json!({
            "prompt_tokens": 100,
            "completion_tokens": 40,
            "total_tokens": 500,
        }));
        // 500 was already the provider's own total; it gets padded, not
        // recomputed from prompt+completion.
        assert_eq!(usage["total_tokens"], 2500);
    }

    #[test]
    fn add_buffer_derives_a_missing_total_from_the_padded_prompt() {
        let usage = add_buffer_to_usage(&json!({
            "prompt_tokens": 100,
            "completion_tokens": 40,
        }));
        assert_eq!(usage["prompt_tokens"], 2100);
        assert_eq!(usage["completion_tokens"], 40);
        assert_eq!(usage["total_tokens"], 2140);
    }

    #[test]
    fn filter_keeps_only_the_fields_a_format_defines() {
        let usage = json!({
            "prompt_tokens": 1,
            "input_tokens": 2,
            "promptTokenCount": 3,
            "estimated": true,
        });

        assert_eq!(
            filter_usage_for_format(&usage, Format::Claude),
            json!({"input_tokens": 2, "estimated": true})
        );
        assert_eq!(
            filter_usage_for_format(&usage, Format::Gemini),
            json!({"promptTokenCount": 3, "estimated": true})
        );
        // gemini-cli and antigravity share the gemini list…
        assert_eq!(
            filter_usage_for_format(&usage, Format::GeminiCli),
            filter_usage_for_format(&usage, Format::Gemini)
        );
        assert_eq!(
            filter_usage_for_format(&usage, Format::Antigravity),
            filter_usage_for_format(&usage, Format::Gemini)
        );
        // …and openai-response shares openai-responses'.
        assert_eq!(
            filter_usage_for_format(&usage, Format::OpenAiResponse),
            filter_usage_for_format(&usage, Format::OpenAiResponses)
        );
        // Vertex falls through to the OpenAI list, as in 9router.
        assert_eq!(
            filter_usage_for_format(&usage, Format::Vertex),
            json!({"prompt_tokens": 1, "estimated": true})
        );
    }

    #[test]
    fn has_valid_usage_ignores_zero_and_missing_fields() {
        assert!(!has_valid_usage(&json!({})));
        assert!(!has_valid_usage(
            &json!({"prompt_tokens": 0, "output_tokens": 0})
        ));
        assert!(has_valid_usage(
            &json!({"prompt_tokens": 0, "output_tokens": 7})
        ));
        assert!(has_valid_usage(&json!({"promptTokenCount": 1})));
    }

    #[test]
    fn input_estimate_counts_utf16_units_not_bytes() {
        let serialized = serde_json::to_string(&json!({"content": "日本語"})).unwrap();
        // 17 UTF-16 units against 23 bytes; JS length is the former, and the
        // byte count would inflate a CJK request by a third.
        assert_eq!(utf16_len(&serialized), 17);
        assert_eq!(serialized.len(), 23);
        assert_eq!(
            estimate_input_tokens(Some(&json!({"content": "日本語"}))),
            5
        );
    }

    #[test]
    fn terminal_block_estimates_when_the_provider_was_silent() {
        let mut state = Map::new();
        note_content(&mut state, &"a".repeat(40));
        seed_request_body(&mut state, Some(&json!({"messages": []})));

        let block = terminal_usage_block(&mut state, Format::OpenAi, None).unwrap();
        assert_eq!(block["estimated"], true);
        assert_eq!(block["completion_tokens"], 10);
        // The estimate is what gets persisted, buffered and all (9router
        // stream.js:361 stores `estimated`, not the unbuffered provider value).
        assert_eq!(state["usage"]["completion_tokens"], 10);
    }

    #[test]
    fn terminal_block_buffers_the_client_copy_but_not_the_stored_one() {
        let tracked = json!({"prompt_tokens": 100, "completion_tokens": 40, "total_tokens": 140});
        let mut state = Map::new();
        // What the transform stored while walking the stream.
        state.insert("usage".into(), tracked.clone());

        let block = terminal_usage_block(&mut state, Format::OpenAi, Some(tracked)).unwrap();
        assert_eq!(block["prompt_tokens"], 2100);
        assert_eq!(block["total_tokens"], 2140);
        // Cost/stats read this one — it must stay exactly what the provider said.
        assert_eq!(state["usage"]["prompt_tokens"], 100);
    }

    #[test]
    fn terminal_block_stays_silent_when_there_is_nothing_to_say() {
        let mut state = Map::new();
        assert!(terminal_usage_block(&mut state, Format::Claude, None).is_none());
    }

    #[test]
    fn terminal_block_forwards_an_all_zero_block_when_there_was_no_content() {
        // 9router's second branch is keyed on `state.usage` being truthy, not
        // on it being valid, so a zeroed block is padded rather than dropped.
        let mut state = Map::new();
        let tracked = json!({"input_tokens": 0, "output_tokens": 0});
        let block = terminal_usage_block(&mut state, Format::Claude, Some(tracked)).unwrap();
        assert_eq!(block["input_tokens"], 2000);
        assert_eq!(block["output_tokens"], 0);
    }
}
