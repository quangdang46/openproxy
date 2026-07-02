//! Cursor to OpenAI response translator — passthrough.

use serde_json::Value;

use crate::core::translator::registry::{
    CursorResponseState, ResponseTransformState,
};

/// Typed inner: returns Vec<Value> of OpenAI SSE events for a parsed JSON chunk.
pub fn cursor_to_openai_response(
    chunk: &Value,
    _state: &mut serde_json::Map<String, Value>,
) -> Vec<Value> {
    if chunk.get("object").and_then(|v| v.as_str()) == Some("chat.completion.chunk")
        && chunk.get("choices").is_some()
    {
        return vec![chunk.clone()];
    }
    if chunk.get("object").and_then(|v| v.as_str()) == Some("chat.completion")
        && chunk.get("choices").is_some()
    {
        return vec![chunk.clone()];
    }
    vec![chunk.clone()]
}

/// Registry-compatible streaming wrapper.
/// Signature matches `registry::ResponseTransformFn`.
pub fn cursor_to_openai_streaming(
    chunk: &[u8],
    state: &mut ResponseTransformState,
) -> Vec<String> {
    let val: Value = match serde_json::from_slice(chunk) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let inner = &mut state.cursor.state;
    let results = cursor_to_openai_response(&val, inner);
    results
        .into_iter()
        .map(|v| format!("data: {}\n\n", serde_json::to_string(&v).unwrap_or_default()))
        .collect()
}
