//! Cursor to OpenAI response translator — passthrough.

use serde_json::Value;

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
