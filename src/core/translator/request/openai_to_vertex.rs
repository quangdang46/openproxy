//! OpenAI to Vertex request translator.
//!
//! Converts via Gemini then post-processes for Vertex AI compatibility.

use serde_json::Value;
use crate::core::translator::request::openai_to_gemini::openai_to_gemini_request;

const DEFAULT_THINKING_VERTEX_SIGNATURE: &str = "vertex-thinking-signature";

fn post_process_for_vertex(body: &mut Value) {
    let Some(contents) = body.get_mut("contents").and_then(|v| v.as_array_mut()) else {
        return;
    };
    for turn in contents {
        let Some(parts) = turn.get_mut("parts").and_then(|v| v.as_array_mut()) else {
            continue;
        };
        for part in parts {
            if let Some(obj) = part.as_object_mut() {
                if obj.contains_key("thoughtSignature") {
                    obj.insert("thoughtSignature".to_string(), Value::String(DEFAULT_THINKING_VERTEX_SIGNATURE.to_string()));
                }
                if let Some(fc) = obj.get_mut("functionCall") {
                    if let Some(fc_obj) = fc.as_object_mut() {
                        fc_obj.remove("id");
                    }
                }
                if let Some(fr) = obj.get_mut("functionResponse") {
                    if let Some(fr_obj) = fr.as_object_mut() {
                        fr_obj.remove("id");
                    }
                }
            }
        }
    }
}

pub fn openai_to_vertex_request(
    model: &str,
    body: &mut Value,
    stream: bool,
    credentials: Option<&Value>,
) -> bool {
    if !openai_to_gemini_request(model, body, stream, credentials) {
        return false;
    }
    post_process_for_vertex(body);
    true
}
