//! Golden snapshot tests for translator format pairs.
//!
//! Each test takes a well-known input payload, runs it through the
//! translator function for one format pair, and compares the `serde_json::Value`
//! output against a stored `insta` snapshot.
//!
//! To update snapshots after an intentional translation change:
//!   INSTA_UPDATE=always cargo test -p openproxy -- test::translator
//!
//! To review pending (new/changed) snapshots:
//!   cargo insta review
//!   (requires `cargo install cargo-insta`)

use serde_json::{json, Value};

use crate::core::translator::request::claude_to_openai::claude_to_openai_request;
use crate::core::translator::request::gemini_to_openai::gemini_to_openai_request;
use crate::core::translator::request::openai_to_claude::openai_to_claude_request;
use crate::core::translator::request::openai_to_gemini::openai_to_gemini_request;

use crate::core::translator::response::claude_to_openai::claude_to_openai_response;
use crate::core::translator::response::gemini_to_openai::gemini_to_openai_response;
use crate::core::translator::response::openai_to_gemini::openai_to_gemini_response;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Run a request translator function and return the mutated body.
fn translate_req<F>(body: Value, f: F) -> Value
where
    F: FnOnce(&str, &mut Value, bool, Option<&Value>) -> bool,
{
    let mut b = body;
    let ok = f("test-model", &mut b, false, None);
    assert!(ok, "translation returned false");
    b
}

/// Run a request translator in streaming mode.
fn translate_req_stream<F>(body: Value, f: F) -> Value
where
    F: FnOnce(&str, &mut Value, bool, Option<&Value>) -> bool,
{
    let mut b = body;
    let ok = f("test-model", &mut b, true, None);
    assert!(ok, "translation returned false");
    b
}

/// Run a response translator that takes typed Value chunks.
fn translate_resp_typed<F>(chunks: Vec<Value>, f: F) -> Vec<Value>
where
    F: Fn(&Value, &mut serde_json::Map<String, Value>) -> Vec<Value>,
{
    let mut state = serde_json::Map::new();
    let mut all = Vec::new();
    for chunk in chunks {
        all.extend(f(&chunk, &mut state));
    }
    all
}

/// Run the OpenAI-to-Gemini response translator (byte-level).
fn translate_resp_openai_to_gemini(chunks: Vec<&str>, expected_ok_count: usize) {
    use crate::core::translator::registry::ResponseTransformState;
    let mut state = ResponseTransformState::default();
    let mut ok = 0usize;
    for chunk_str in &chunks {
        let out = openai_to_gemini_response(chunk_str.as_bytes(), &mut state);
        if !out.is_empty() {
            ok += 1;
        }
    }
    // We just verify that events were emitted
    assert_eq!(
        ok, expected_ok_count,
        "openai_to_gemini_response: unexpected output count"
    );
}

// ---------------------------------------------------------------------------
// OpenAI -> Claude request
// ---------------------------------------------------------------------------

#[test]
fn openai_to_claude_simple_text() {
    let body = json!({
        "messages": [
            {"role": "user", "content": "Hello, world!"}
        ]
    });
    let result = translate_req(body, openai_to_claude_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn openai_to_claude_with_system() {
    let body = json!({
        "messages": [
            {"role": "system", "content": "You are a helpful assistant."},
            {"role": "user", "content": "What is Rust?"}
        ]
    });
    let result = translate_req(body, openai_to_claude_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn openai_to_claude_with_developer_role() {
    let body = json!({
        "messages": [
            {"role": "developer", "content": "You are an expert Rust developer."},
            {"role": "user", "content": "Write a fibonacci function"}
        ]
    });
    let result = translate_req(body, openai_to_claude_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn openai_to_claude_with_tools() {
    let body = json!({
        "messages": [
            {"role": "user", "content": "What's the weather in Paris?"}
        ],
        "tools": [{
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get current weather for a city",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "location": {"type": "string"},
                        "unit": {"type": "string", "enum": ["celsius", "fahrenheit"]}
                    },
                    "required": ["location"]
                }
            }
        }],
        "tool_choice": "auto"
    });
    let result = translate_req(body, openai_to_claude_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn openai_to_claude_tool_calls_in_messages() {
    let body = json!({
        "messages": [
            {"role": "user", "content": "Get the weather in Paris"},
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_123",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": "{\"location\": \"Paris\"}"
                    }
                }]
            },
            {
                "role": "tool",
                "tool_call_id": "call_123",
                "content": "{\"temperature\": 22, \"condition\": \"sunny\"}"
            },
            {"role": "user", "content": "Thanks!"}
        ],
        "tools": [{
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get current weather",
                "parameters": {"type": "object", "properties": {"location": {"type": "string"}}, "required": ["location"]}
            }
        }]
    });
    let result = translate_req(body, openai_to_claude_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn openai_to_claude_image_url() {
    let body = json!({
        "messages": [
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": "Describe this image"},
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": "https://example.com/photo.jpg",
                            "detail": "high"
                        }
                    }
                ]
            }
        ]
    });
    let result = translate_req(body, openai_to_claude_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn openai_to_claude_json_schema_output() {
    let body = json!({
        "messages": [
            {"role": "user", "content": "Generate a user profile"}
        ],
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "user_profile",
                "schema": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "age": {"type": "integer"},
                        "email": {"type": "string"}
                    },
                    "required": ["name", "email"]
                }
            }
        }
    });
    let result = translate_req(body, openai_to_claude_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn openai_to_claude_json_object_output() {
    let body = json!({
        "messages": [
            {"role": "user", "content": "Give me a JSON object"}
        ],
        "response_format": {"type": "json_object"}
    });
    let result = translate_req(body, openai_to_claude_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn openai_to_claude_reasoning_effort() {
    let body = json!({
        "messages": [
            {"role": "user", "content": "Solve this math problem step by step"}
        ],
        "reasoning_effort": "high"
    });
    let result = translate_req(body, openai_to_claude_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn openai_to_claude_max_tokens_and_temp() {
    let body = json!({
        "messages": [
            {"role": "user", "content": "Write a poem"}
        ],
        "max_tokens": 500,
        "temperature": 0.7
    });
    let result = translate_req(body, openai_to_claude_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn openai_to_claude_streaming() {
    let body = json!({
        "messages": [
            {"role": "user", "content": "Count to five"}
        ]
    });
    let result = translate_req_stream(body, openai_to_claude_request);
    // Check stream=true was set
    assert!(result
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false));
    insta::assert_json_snapshot!(result);
}

// ---------------------------------------------------------------------------
// Claude -> OpenAI request
// ---------------------------------------------------------------------------

#[test]
fn claude_to_openai_simple_text() {
    let body = json!({
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "Hello!"}]}
        ]
    });
    let result = translate_req(body, claude_to_openai_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn claude_to_openai_text_only_content_flat() {
    // When all parts are text, they should be flattened to a string
    let body = json!({
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "Hello"}, {"type": "text", "text": "there"}]}
        ]
    });
    let result = translate_req(body, claude_to_openai_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn claude_to_openai_with_system() {
    let body = json!({
        "system": [{"type": "text", "text": "You are Claude."}],
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "Hi"}]}
        ]
    });
    let result = translate_req(body, claude_to_openai_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn claude_to_openai_with_tool_use() {
    let body = json!({
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "What's the time?"}]}
        ],
        "tools": [{
            "name": "get_time",
            "description": "Get current time",
            "input_schema": {
                "type": "object",
                "properties": {
                    "timezone": {"type": "string"}
                }
            }
        }]
    });
    let result = translate_req(body, claude_to_openai_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn claude_to_openai_tool_use_and_tool_result() {
    let body = json!({
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "Get weather"}]},
            {
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "I'll check the weather."},
                    {
                        "type": "tool_use",
                        "id": "tu_123",
                        "name": "get_weather",
                        "input": {"location": "Tokyo"}
                    }
                ]
            },
            {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "tu_123",
                    "content": [{"type": "text", "text": "{\"temp\": 28}"}]
                }]
            }
        ],
        "tools": [{
            "name": "get_weather",
            "description": "Get weather",
            "input_schema": {"type": "object", "properties": {"location": {"type": "string"}}}
        }]
    });
    let result = translate_req(body, claude_to_openai_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn claude_to_openai_with_image() {
    let body = json!({
        "messages": [
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": "What's in this image?"},
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/png",
                            "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
                        }
                    }
                ]
            }
        ]
    });
    let result = translate_req(body, claude_to_openai_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn claude_to_openai_thinking_block() {
    let body = json!({
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "Think step by step"}]}
        ],
        "thinking": {"type": "enabled", "budget_tokens": 4096}
    });
    let result = translate_req(body, claude_to_openai_request);
    insta::assert_json_snapshot!(result);
}

// ---------------------------------------------------------------------------
// OpenAI -> Gemini request
// ---------------------------------------------------------------------------

#[test]
fn openai_to_gemini_simple_text() {
    let body = json!({
        "messages": [
            {"role": "user", "content": "Hello from OpenAI"}
        ]
    });
    let result = translate_req(body, openai_to_gemini_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn openai_to_gemini_with_system() {
    let body = json!({
        "messages": [
            {"role": "system", "content": "You are a helpful Gemini assistant."},
            {"role": "user", "content": "Tell me about planets"}
        ]
    });
    let result = translate_req(body, openai_to_gemini_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn openai_to_gemini_with_tools() {
    let body = json!({
        "messages": [
            {"role": "user", "content": "Calculate 2+2"}
        ],
        "tools": [{
            "type": "function",
            "function": {
                "name": "calculate",
                "description": "Perform calculation",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "expression": {"type": "string"}
                    },
                    "required": ["expression"]
                }
            }
        }]
    });
    let result = translate_req(body, openai_to_gemini_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn openai_to_gemini_with_tool_calls() {
    let body = json!({
        "messages": [
            {"role": "user", "content": "Search for Rust info"},
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_42",
                    "type": "function",
                    "function": {
                        "name": "search_web",
                        "arguments": "{\"query\": \"Rust programming 2026\"}"
                    }
                }]
            },
            {
                "role": "tool",
                "tool_call_id": "call_42",
                "content": "{\"results\": [\"Rust is a systems language...\"]}"
            }
        ],
        "tools": [{
            "type": "function",
            "function": {
                "name": "search_web",
                "description": "Search the web",
                "parameters": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}
            }
        }]
    });
    let result = translate_req(body, openai_to_gemini_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn openai_to_gemini_generation_config() {
    let body = json!({
        "messages": [
            {"role": "user", "content": "Write code"}
        ],
        "temperature": 0.3,
        "max_tokens": 1024,
        "top_p": 0.95,
        "top_k": 40
    });
    let result = translate_req(body, openai_to_gemini_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn openai_to_gemini_image_base64() {
    let body = json!({
        "messages": [
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": "Describe this image"},
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
                        }
                    }
                ]
            }
        ]
    });
    let result = translate_req(body, openai_to_gemini_request);
    insta::assert_json_snapshot!(result);
}

// ---------------------------------------------------------------------------
// Gemini -> OpenAI request
// ---------------------------------------------------------------------------

#[test]
fn gemini_to_openai_simple_text() {
    let body = json!({
        "contents": [
            {
                "role": "user",
                "parts": [{"text": "Hello Gemini"}]
            }
        ]
    });
    let result = translate_req(body, gemini_to_openai_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn gemini_to_openai_with_system_instruction() {
    let body = json!({
        "systemInstruction": {
            "parts": [{"text": "You are a Gemini assistant."}]
        },
        "contents": [
            {
                "role": "user",
                "parts": [{"text": "Explain quantum computing"}]
            }
        ]
    });
    let result = translate_req(body, gemini_to_openai_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn gemini_to_openai_function_call_and_response() {
    let body = json!({
        "contents": [
            {"role": "user", "parts": [{"text": "What's 2+2?"}]},
            {
                "role": "model",
                "parts": [
                    {
                        "functionCall": {
                            "name": "calculate",
                            "args": {"expression": "2+2"}
                        }
                    }
                ]
            },
            {
                "role": "function",
                "parts": [
                    {
                        "functionResponse": {
                            "name": "calculate",
                            "response": {"result": 4}
                        }
                    }
                ]
            }
        ]
    });
    let mut result = translate_req(body, gemini_to_openai_request);
    // The translator generates tool_call IDs with timestamps;
    // strip them for stable snapshot comparison.
    if let Some(msgs) = result.get_mut("messages").and_then(|v| v.as_array_mut()) {
        for msg in msgs.iter_mut() {
            if let Some(tcs) = msg.get_mut("tool_calls").and_then(|v| v.as_array_mut()) {
                for tc in tcs.iter_mut() {
                    if let Some(obj) = tc.as_object_mut() {
                        obj.remove("id");
                    }
                }
            }
        }
    }
    insta::assert_json_snapshot!(result);
}

#[test]
fn gemini_to_openai_generation_config() {
    let body = json!({
        "contents": [
            {"role": "user", "parts": [{"text": "Write a story"}]}
        ],
        "generationConfig": {
            "temperature": 0.8,
            "maxOutputTokens": 2048,
            "topP": 0.9
        }
    });
    let result = translate_req(body, gemini_to_openai_request);
    insta::assert_json_snapshot!(result);
}

#[test]
fn gemini_to_openai_thought_parts() {
    let body = json!({
        "contents": [
            {"role": "user", "parts": [{"text": "Think step by step: 23 * 45"}]},
            {
                "role": "model",
                "parts": [
                    {"thought": true, "text": "Let me compute 23 * 45 = 1035"},
                    {"text": "The answer is 1035."}
                ]
            }
        ]
    });
    let result = translate_req(body, gemini_to_openai_request);
    insta::assert_json_snapshot!(result);
}

// ---------------------------------------------------------------------------
// Response translators (streaming)
// ---------------------------------------------------------------------------

#[test]
fn claude_to_openai_response_stream() {
    // Simulate a Claude streaming response: message_start -> text delta -> message_done
    let chunks = vec![
        json!({
            "type": "message_start",
            "message": {
                "id": "msg_abc123",
                "model": "claude-sonnet-4-6",
                "role": "assistant",
                "content": [],
                "usage": {"input_tokens": 10, "output_tokens": 1}
            }
        }),
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "Hello from Claude!"}
        }),
        json!({
            "type": "content_block_stop",
            "index": 0
        }),
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {"output_tokens": 12}
        }),
        json!({
            "type": "message_stop"
        }),
    ];

    let outputs = translate_resp_typed(chunks, claude_to_openai_response);
    assert!(
        !outputs.is_empty(),
        "expected at least one SSE output chunk"
    );

    // Verify output structure: each entry has choices[0].delta
    for output in &outputs {
        assert!(
            output.get("choices").is_some(),
            "every openai SSE chunk must have a choices array; got: {output}"
        );
    }

    // The first chunk should have role="assistant"
    let first = &outputs[0];
    let first_role = first["choices"][0]["delta"]["role"].as_str();
    assert_eq!(
        first_role,
        Some("assistant"),
        "first chunk should declare role=assistant"
    );

    // Collect all delta content to verify full message
    let full_text: String = outputs
        .iter()
        .filter_map(|o| o["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(
        full_text, "Hello from Claude!",
        "translated text should match"
    );

    // Final chunk should have finish_reason
    let last_finish = outputs
        .last()
        .and_then(|o| o["choices"][0]["finish_reason"].as_str());
    assert_eq!(
        last_finish,
        Some("stop"),
        "last chunk should have finish_reason=stop"
    );

    // The snapshot compares the last chunk's stop-reason and usage fields.
    // Strip the time-dependent `created` field before snapshot assertion.
    let mut last = outputs.last().unwrap().clone();
    if let Some(obj) = last.as_object_mut() {
        obj.remove("created");
    }
    insta::assert_json_snapshot!(last);
}

#[test]
fn gemini_to_openai_response_stream() {
    // Simulate a Gemini streaming response with a candidate text part
    let chunks = vec![json!({
        "response": {
            "responseId": "test-gemini-response-1",
            "candidates": [{
                "index": 0,
                "content": {
                    "role": "model",
                    "parts": [{"text": "Hello from Gemini!"}]
                },
                "finishReason": "STOP",
                "usageMetadata": {
                    "promptTokenCount": 10,
                    "candidatesTokenCount": 5
                }
            }]
        }
    })];

    use std::collections::HashMap;
    let mut state: HashMap<String, Value> = HashMap::new();
    let mut outputs: Vec<Value> = Vec::new();
    for chunk in &chunks {
        outputs.extend(gemini_to_openai_response(chunk, &mut state));
    }

    assert!(!outputs.is_empty(), "expected at least one output chunk");

    let first = &outputs[0];
    assert_eq!(
        first["choices"][0]["delta"]["role"].as_str(),
        Some("assistant"),
        "first chunk should have role=assistant"
    );

    let full_content: String = outputs
        .iter()
        .filter_map(|o| o["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(full_content, "Hello from Gemini!");

    let last_finish = outputs
        .last()
        .and_then(|o| o["choices"][0]["finish_reason"].as_str());
    assert_eq!(last_finish, Some("stop"));

    // Strip time-dependent `created` field before snapshot assertion
    let mut clean_first = first.clone();
    if let Some(obj) = clean_first.as_object_mut() {
        obj.remove("created");
    }
    insta::assert_json_snapshot!("gemini_to_openai_response_first_chunk", clean_first);
}

#[test]
fn openai_to_gemini_response_basic() {
    // Simulate an OpenAI streaming response being converted to Gemini format.
    // The `openai_to_gemini_response` function expects raw JSON lines (the SSE
    // `data: ` prefix is stripped upstream in the response transform pipeline).
    let chunk1 = r#"{"id":"chatcmpl-abc","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"Hello from "},"finish_reason":null}]}"#;
    let chunk2 = r#"{"id":"chatcmpl-abc","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"OpenAI!"},"finish_reason":"stop"}]}"#;

    translate_resp_openai_to_gemini(vec![chunk1, chunk2], 2);
}

// ---------------------------------------------------------------------------
// Edge-case / error handling
// ---------------------------------------------------------------------------

#[test]
fn openai_to_claude_empty_messages() {
    let body = json!({
        "messages": []
    });
    let mut b = body;
    let ok = openai_to_claude_request("test-model", &mut b, false, None);
    assert!(ok, "empty messages should still succeed");
    insta::assert_json_snapshot!(b);
}

#[test]
fn claude_to_openai_empty_messages() {
    let body = json!({
        "messages": []
    });
    let mut b = body;
    let ok = claude_to_openai_request("test-model", &mut b, false, None);
    assert!(ok, "empty messages should still succeed");
    insta::assert_json_snapshot!(b);
}

#[test]
fn openai_to_gemini_empty_messages() {
    let body = json!({
        "messages": []
    });
    let mut b = body;
    let ok = openai_to_gemini_request("test-model", &mut b, false, None);
    assert!(ok, "empty messages should still succeed");
    insta::assert_json_snapshot!(b);
}

#[test]
fn gemini_to_openai_empty_contents() {
    let body = json!({
        "contents": []
    });
    let mut b = body;
    let ok = gemini_to_openai_request("test-model", &mut b, false, None);
    assert!(ok, "empty contents should still succeed");
    insta::assert_json_snapshot!(b);
}

#[test]
fn openai_to_claude_invalid_body_not_object() {
    let body = json!("not an object");
    let mut b = body;
    let ok = openai_to_claude_request("test-model", &mut b, false, None);
    assert!(!ok, "non-object body should fail");
}

#[test]
fn gemini_to_openai_no_contents_key() {
    let body = json!({});
    let mut b = body;
    let ok = gemini_to_openai_request("test-model", &mut b, false, None);
    // Accept either false (no contents) or true (with empty messages list) —
    // just verify it doesn't panic.
    let _ = ok;
}
