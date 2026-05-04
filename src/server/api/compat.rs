use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};

use crate::server::state::AppState;

use super::chat;

pub async fn messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    forward_compat(state, headers, body, CompatMode::Messages).await
}

pub async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    forward_compat(
        state,
        headers,
        body,
        CompatMode::Responses { compact: false },
    )
    .await
}

pub async fn responses_compact(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    forward_compat(
        state,
        headers,
        body,
        CompatMode::Responses { compact: true },
    )
    .await
}

pub async fn count_tokens(body: Result<Json<Value>, JsonRejection>) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => return invalid_json_response(),
    };

    let total_chars = count_request_chars(&body);
    let input_tokens = total_chars.div_ceil(4) as u64;

    Json(json!({ "input_tokens": input_tokens })).into_response()
}

#[derive(Clone, Copy)]
enum CompatMode {
    Messages,
    Responses { compact: bool },
}

async fn forward_compat(
    state: AppState,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
    mode: CompatMode,
) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => return invalid_json_response(),
    };

    let normalized = normalize_body(body, mode);
    let endpoint = match mode {
        CompatMode::Messages => Some("/v1/messages"),
        CompatMode::Responses { compact: false } => Some("/v1/responses"),
        CompatMode::Responses { compact: true } => Some("/v1/responses/compact"),
    };
    chat::chat_completions_for_endpoint(state, headers, Ok(Json(normalized)), endpoint).await
}

fn normalize_body(mut body: Value, mode: CompatMode) -> Value {
    let Some(fields) = body.as_object_mut() else {
        return body;
    };

    match mode {
        CompatMode::Messages => {
            if let Some(system) = fields.remove("system") {
                prepend_system_message(fields, normalize_content(system));
            }

            if let Some(messages) = fields.get_mut("messages") {
                normalize_messages_value(messages);
            }
        }
        CompatMode::Responses { compact } => {
            if compact {
                fields.insert("_compact".to_string(), Value::Bool(true));
            }

            if !fields.contains_key("max_tokens") && !fields.contains_key("max_completion_tokens") {
                if let Some(max_output_tokens) = fields.get("max_output_tokens").cloned() {
                    fields.insert("max_tokens".to_string(), max_output_tokens);
                }
            }

            if let Some(instructions) = fields.remove("instructions") {
                prepend_system_message(fields, normalize_content(instructions));
            }

            if let Some(input) = fields.remove("input") {
                let converted = input_to_messages(input);
                if let Some(existing) = fields.get_mut("messages").and_then(Value::as_array_mut) {
                    if let Some(mut converted_items) = converted.as_array().cloned() {
                        existing.append(&mut converted_items);
                    }
                } else {
                    fields.insert("messages".to_string(), converted);
                }
            }

            if let Some(messages) = fields.get_mut("messages") {
                normalize_messages_value(messages);
            }
        }
    }

    body
}

fn prepend_system_message(fields: &mut Map<String, Value>, content: Value) {
    let messages = fields
        .entry("messages".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));

    let Some(array) = messages.as_array_mut() else {
        *messages = Value::Array(vec![json!({
            "role": "system",
            "content": content,
        })]);
        return;
    };

    array.insert(
        0,
        json!({
            "role": "system",
            "content": content,
        }),
    );
}

fn normalize_messages_value(messages: &mut Value) {
    let Some(array) = messages.as_array_mut() else {
        return;
    };

    for message in array {
        normalize_message(message);
    }
}

fn normalize_message(message: &mut Value) {
    let Some(fields) = message.as_object_mut() else {
        return;
    };

    if let Some(content) = fields.get_mut("content") {
        *content = normalize_content(content.clone());
    } else if let Some(text) = fields.get("text").and_then(Value::as_str) {
        fields.insert("content".to_string(), Value::String(text.to_string()));
    }
}

fn normalize_content(content: Value) -> Value {
    match content {
        Value::Array(parts) => Value::Array(
            parts
                .into_iter()
                .filter_map(|part| match part {
                    Value::String(text) => Some(json!({ "type": "text", "text": text })),
                    Value::Object(mut map) => {
                        if map
                            .get("type")
                            .and_then(Value::as_str)
                            .is_some_and(|kind| matches!(kind, "input_text" | "output_text"))
                        {
                            map.insert("type".to_string(), Value::String("text".to_string()));
                        }
                        Some(Value::Object(map))
                    }
                    _ => None,
                })
                .collect(),
        ),
        Value::Object(mut map) => {
            if map
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| matches!(kind, "input_text" | "output_text"))
            {
                map.insert("type".to_string(), Value::String("text".to_string()));
            }
            Value::Object(map)
        }
        other => other,
    }
}

fn input_to_messages(input: Value) -> Value {
    match input {
        Value::String(text) => Value::Array(vec![json!({
            "role": "user",
            "content": text,
        })]),
        Value::Array(items) => {
            let mut messages = Vec::new();
            for item in items {
                push_input_item(&mut messages, item);
            }
            Value::Array(messages)
        }
        Value::Object(map) => {
            let mut messages = Vec::new();
            push_input_item(&mut messages, Value::Object(map));
            Value::Array(messages)
        }
        _ => Value::Array(Vec::new()),
    }
}

fn push_input_item(messages: &mut Vec<Value>, item: Value) {
    match item {
        Value::String(text) => messages.push(json!({
            "role": "user",
            "content": text,
        })),
        Value::Object(mut fields) => {
            if let Some(role) = fields
                .get("role")
                .and_then(Value::as_str)
                .map(str::to_string)
            {
                let content = fields
                    .remove("content")
                    .map(normalize_content)
                    .or_else(|| {
                        fields
                            .get("text")
                            .and_then(Value::as_str)
                            .map(|text| Value::String(text.to_string()))
                    })
                    .unwrap_or_else(|| Value::String(String::new()));

                messages.push(json!({
                    "role": role,
                    "content": content,
                }));
                return;
            }

            if fields
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| matches!(kind, "input_text" | "text" | "output_text"))
            {
                if let Some(text) = fields.get("text").and_then(Value::as_str) {
                    messages.push(json!({
                        "role": "user",
                        "content": text,
                    }));
                }
            }
        }
        _ => {}
    }
}

fn count_request_chars(body: &Value) -> usize {
    let mut total = 0;

    for field in ["messages", "input", "instructions", "system"] {
        if let Some(value) = body.get(field) {
            total += count_chars(value);
        }
    }

    total
}

fn count_chars(value: &Value) -> usize {
    match value {
        Value::String(text) => text.chars().count(),
        Value::Array(items) => items.iter().map(count_chars).sum(),
        Value::Object(fields) => {
            if let Some(content) = fields.get("content") {
                return count_chars(content);
            }

            if let Some(text) = fields.get("text") {
                return count_chars(text);
            }

            0
        }
        _ => 0,
    }
}

fn invalid_json_response() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": "Invalid JSON body" })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_input_is_normalized_into_messages() {
        let body = json!({
            "model": "openai/gpt-4o-mini",
            "instructions": "Be terse",
            "input": [
                { "role": "user", "content": [{ "type": "input_text", "text": "Hello" }] }
            ],
            "max_output_tokens": 64
        });

        let normalized = normalize_body(body, CompatMode::Responses { compact: true });

        assert_eq!(normalized["max_tokens"], 64);
        assert_eq!(normalized["_compact"], true);
        assert!(normalized.get("input").is_none());
        assert_eq!(normalized["messages"][0]["role"], "system");
        assert_eq!(normalized["messages"][0]["content"], "Be terse");
        assert_eq!(normalized["messages"][1]["content"][0]["type"], "text");
    }

    #[test]
    fn messages_route_promotes_system_field() {
        let body = json!({
            "model": "openai/gpt-4o-mini",
            "system": "Stay concise",
            "messages": [{ "role": "user", "content": "Ping" }]
        });

        let normalized = normalize_body(body, CompatMode::Messages);
        let messages = normalized["messages"].as_array().expect("messages array");

        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "Stay concise");
        assert_eq!(messages[1]["role"], "user");
    }

    #[test]
    fn token_counter_counts_nested_text_parts() {
        let request = json!({
            "messages": [
                { "role": "user", "content": "abcd" },
                { "role": "assistant", "content": [{ "type": "text", "text": "efghij" }] }
            ]
        });

        assert_eq!(count_request_chars(&request), 10);
    }
}
