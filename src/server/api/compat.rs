use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use chrono::Utc;
use futures_util::StreamExt;
use http_body_util::BodyExt;
use serde_json::{json, Map, Value};

use crate::core::translator::response_transform::{
    AnthropicToOpenAiTransformer, StreamingTransformer,
};
use crate::server::state::AppState;

use super::chat;

pub async fn cors_options() -> Response {
    cors_preflight_response("POST, OPTIONS")
}

pub async fn messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let model = body
        .as_ref()
        .ok()
        .and_then(|b| b.get("model").and_then(|m| m.as_str()));
    let request_id = headers.get("x-request-id").and_then(|v| v.to_str().ok());
    let _log = crate::server::request_logger::RequestLog::start("POST", "/v1/messages", model)
        .with_request_id(request_id);
    let response = forward_compat(state, headers, body, CompatMode::Messages).await;
    _log.finish(response.status().as_u16());
    response
}

pub async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let model = body
        .as_ref()
        .ok()
        .and_then(|b| b.get("model").and_then(|m| m.as_str()));
    let request_id = headers.get("x-request-id").and_then(|v| v.to_str().ok());
    let _log = crate::server::request_logger::RequestLog::start("POST", "/v1/responses", model)
        .with_request_id(request_id);
    let response = forward_compat(
        state,
        headers,
        body,
        CompatMode::Responses { compact: false },
    )
    .await;
    _log.finish(response.status().as_u16());
    response
}

pub async fn responses_compact(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let model = body
        .as_ref()
        .ok()
        .and_then(|b| b.get("model").and_then(|m| m.as_str()));
    let request_id = headers.get("x-request-id").and_then(|v| v.to_str().ok());
    let _log =
        crate::server::request_logger::RequestLog::start("POST", "/v1/responses/compact", model)
            .with_request_id(request_id);
    let response = forward_compat(
        state,
        headers,
        body,
        CompatMode::Responses { compact: true },
    )
    .await;
    _log.finish(response.status().as_u16());
    response
}

pub async fn count_tokens(body: Result<Json<Value>, JsonRejection>) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => return invalid_json_response(),
    };

    let total_chars = count_request_chars(&body);
    let input_tokens = total_chars.div_ceil(4) as u64;

    with_cors_response(Json(json!({ "input_tokens": input_tokens })).into_response())
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
    let response =
        chat::chat_completions_for_endpoint(state, headers, Ok(Json(normalized)), endpoint).await;

    match mode {
        CompatMode::Responses { .. } => {
            with_cors_response(convert_to_responses_api(response).await)
        }
        CompatMode::Messages => with_cors_response(convert_to_messages_api(response).await),
    }
}

// ---------------------------------------------------------------------------
// Responses API SSE format conversion
// ---------------------------------------------------------------------------

/// Convert an OpenAI chat-completion response (streaming or non-streaming) to
/// the OpenAI Responses API format.
async fn convert_to_responses_api(response: Response) -> Response {
    let status = response.status();
    if !status.is_success() {
        return response;
    }

    let is_sse = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("text/event-stream") || ct.contains("text/plain"));

    if !is_sse {
        // Non-streaming — collect the whole body and convert the JSON.
        // If the upstream returned a Claude-format body (type: message),
        // convert it to OpenAI format first (matching 9router's translateNonStreamingResponse).
        let (parts, body) = response.into_parts();
        let body_bytes = match body.collect().await {
            Ok(col) => col.to_bytes(),
            Err(_) => return Response::from_parts(parts, Body::empty()),
        };
        let Ok(body_value) = serde_json::from_slice::<Value>(&body_bytes) else {
            return (status, body_bytes).into_response();
        };

        // Detect Claude-format body: {"type":"message","content":[...],"stop_reason":"end_turn"}
        let chat_completion = if body_value.get("type").and_then(Value::as_str) == Some("message") {
            claude_body_to_chat_completion(&body_value)
        } else {
            body_value
        };

        let responses_json = chat_completion_to_responses_json(&chat_completion);
        return Json(responses_json).into_response();
    }

    // Streaming — wrap the body through the SSE converter.
    // The upstream may return OpenAI SSE (chat.completion.chunk) or
    // Anthropic SSE (message_start / content_block_* / message_delta).
    // We detect Claude format by looking for "type":"message_start" in the data.
    let (parts, body) = response.into_parts();
    let data_stream = body.into_data_stream();
    let converted = async_stream::stream! {
        // Token-buffer re-assembly: SSE frames can be split across TCP segments
        // or combined in a single chunk. We assemble whole `data: ...` frames
        // and pass complete JSON to the converter.
        let mut raw_buf = String::new();
        let mut conv_state = ResponsesSseState::new();
        // Claude SSE → OpenAI SSE transformer (lazy init on first Claude frame)
        let mut claude_xform = AnthropicToOpenAiTransformer::new();

        #[allow(unused_mut)]
        let mut stream = data_stream;

        loop {
            let next = stream.next().await;
            match next {
                Some(Ok(chunk)) => {
                    let chunk_str = String::from_utf8_lossy(&chunk);
                    raw_buf.push_str(&chunk_str);

                    // Process complete SSE frames from the buffer.
                    // Each frame is `data: {...}\n\n` or `event: ...\ndata: {...}\n\n`.
                    loop {
                        if let Some(frame_end) = raw_buf.find("\n\n") {
                            let frame = raw_buf[..frame_end].to_string();
                            raw_buf.drain(..frame_end + 2);

                            let frame = frame.trim();
                            if frame.is_empty() || frame.starts_with(':') {
                                continue; // heartbeat or comment
                            }

                            // Extract JSON from the data: line (may have event: prefix lines)
                            let frame_lower = frame.to_lowercase();
                            let json_str = if let Some(d) = frame.lines()
                                .find(|l| l.trim().starts_with("data:"))
                                .and_then(|l| l.splitn(2, ':').nth(1).map(|s| s.trim()))
                            {
                                d
                            } else {
                                continue;
                            };

                            if json_str == "[DONE]" {
                                break;
                            }

                            // Detect Claude format: has "type":"message_start" or other anthropic types
                            let is_claude_event = json_str.contains("\"type\":\"")
                                && (json_str.contains("\"message_start\"")
                                    || json_str.contains("\"content_block_")
                                    || json_str.contains("\"message_delta\"")
                                    || json_str.contains("\"message_stop\"")
                                    || json_str.contains("\"ping\""));

                            if is_claude_event {
                                // Filter out ping events (no OpenAI equivalent)
                                if json_str.contains("\"ping\"") {
                                    continue;
                                }
                                // Convert Claude SSE → OpenAI SSE lines via transformer
                                let claude_bytes = Bytes::from(format!("data: {json_str}\n\n"));
                                let openai_lines = claude_xform.transform_chunk(&claude_bytes);
                                for line in openai_lines {
                                    if let Some(data) = line.strip_prefix("data: ") {
                                        if data == "[DONE]" {
                                            break;
                                        }
                                        if let Ok(chunk_value) = serde_json::from_str::<Value>(data) {
                                            let events = openai_chunk_to_responses(&mut conv_state, &chunk_value);
                                            for event_bytes in events {
                                                yield Ok::<Bytes, std::io::Error>(Bytes::from(event_bytes));
                                            }
                                        }
                                    }
                                }
                            } else {
                                // Already OpenAI format — pass through directly
                                if let Ok(chunk_value) = serde_json::from_str::<Value>(json_str) {
                                    let events = openai_chunk_to_responses(&mut conv_state, &chunk_value);
                                    for event_bytes in events {
                                        yield Ok::<Bytes, std::io::Error>(Bytes::from(event_bytes));
                                    }
                                }
                            }
                        } else {
                            break;
                        }
                    }
                }
                Some(Err(_)) | None => break,
            }
        }
    };

    let body = Body::from_stream(converted);
    let mut resp = Response::new(body);
    *resp.status_mut() = status;

    // Copy original headers except content-type (we keep it as event-stream)
    for (name, value) in &parts.headers {
        if name.as_str() != "content-length"
            && name.as_str() != "content-type"
            && name.as_str() != "transfer-encoding"
        {
            resp.headers_mut().insert(name, value.clone());
        }
    }
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));

    resp
}

/// Convert a completed chat-completion JSON object to a Responses API JSON object.
/// Used for non-streaming responses.
fn chat_completion_to_responses_json(source: &Value) -> Value {
    let response_id = source
        .get("id")
        .and_then(|v| v.as_str())
        .map(|id| {
            if id.starts_with("chatcmpl-") {
                format!("resp_{}", &id[9..])
            } else {
                format!("resp_{}", id)
            }
        })
        .unwrap_or_else(|| format!("resp_{:x}", Utc::now().timestamp_nanos_opt().unwrap_or(0)));
    let created = source
        .get("created")
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| Utc::now().timestamp());
    let model = source.get("model").and_then(|v| v.as_str()).unwrap_or("");

    let mut output = Vec::new();
    let mut usage = None;

    if let Some(choices) = source.get("choices").and_then(|v| v.as_array()) {
        for (idx, choice) in choices.iter().enumerate() {
            let finish_reason = choice.get("finish_reason").and_then(|v| v.as_str());
            if let Some(message) = choice.get("message") {
                let role = message
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("assistant");
                let content = message
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // Build content parts array
                let mut content_parts = Vec::new();

                // Reasoning content (non-standard field like DeepSeek)
                if let Some(reasoning) = message.get("reasoning_content").and_then(|v| v.as_str()) {
                    if !reasoning.is_empty() {
                        content_parts.push(json!({
                            "id": format!("item_{}_reasoning", idx),
                            "type": "summary_text",
                            "text": reasoning,
                        }));
                    }
                }

                content_parts.push(json!({
                    "id": format!("item_{}_text", idx),
                    "type": "output_text",
                    "text": content,
                    "annotations": [],
                }));

                let item = json!({
                    "id": format!("item_{}", idx),
                    "type": "message",
                    "role": role,
                    "status": if finish_reason.is_some() { "completed" } else { "in_progress" },
                    "content": content_parts,
                });
                output.push(item);
            }
        }
    }

    if let Some(u) = source.get("usage") {
        let mut mapped = json!({});
        if let Some(m) = mapped.as_object_mut() {
            if let Some(v) = u.get("prompt_tokens").and_then(Value::as_u64) {
                m.insert("input_tokens".into(), json!(v));
            }
            if let Some(v) = u.get("completion_tokens").and_then(Value::as_u64) {
                m.insert("output_tokens".into(), json!(v));
            }
            if let Some(v) = u.get("total_tokens").and_then(Value::as_u64) {
                m.insert("total_tokens".into(), json!(v));
            }
        }
        usage = Some(mapped);
    }

    let mut resp = json!({
        "id": response_id,
        "object": "response",
        "created_at": created,
        "status": "completed",
        "model": model,
        "output": output,
    });

    if let Some(u) = usage {
        resp.as_object_mut().unwrap().insert("usage".to_string(), u);
    }

    resp
}

/// State machine that tracks the SSE conversion from chat.completion.chunk
/// events to Responses API events.
struct ResponsesSseState {
    started: bool,
    response_id: String,
    created: i64,
    model: String,
    seq: u64,
    // Per-choice-index tracking
    msg_item_added: HashMap<usize, bool>,
    msg_content_added: HashMap<usize, bool>,
    msg_text_buf: HashMap<usize, String>,
    msg_item_done: HashMap<usize, bool>,
    added_item_id_map: HashMap<usize, String>,
    added_content_part_id_map: HashMap<usize, String>,
    // Reasoning tracking
    reasoning_id: Option<String>,
    reasoning_buf: Option<String>,
    reasoning_done: bool,
    reasoning_item_added: bool,
    reasoning_content_added: bool,
    // Global state
    completed_sent: bool,
}

impl ResponsesSseState {
    fn new() -> Self {
        Self {
            started: false,
            response_id: String::new(),
            created: 0,
            model: String::new(),
            seq: 0,
            msg_item_added: HashMap::new(),
            msg_content_added: HashMap::new(),
            msg_text_buf: HashMap::new(),
            msg_item_done: HashMap::new(),
            added_item_id_map: HashMap::new(),
            added_content_part_id_map: HashMap::new(),
            reasoning_id: None,
            reasoning_buf: None,
            reasoning_done: false,
            reasoning_item_added: false,
            reasoning_content_added: false,
            completed_sent: false,
        }
    }
}

static RESP_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

fn generate_response_id() -> String {
    let n = RESP_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("resp_{:x}", n)
}

fn response_skeleton(state: &ResponsesSseState) -> Value {
    let mut resp = json!({
        "id": state.response_id,
        "object": "response",
        "created_at": state.created,
        "status": "in_progress",
    });
    if !state.model.is_empty() {
        resp.as_object_mut()
            .unwrap()
            .insert("model".to_string(), json!(state.model));
    }
    resp
}

/// Format a single SSE frame for a Responses API event.
fn format_sse_event(event: &str, data: &Value) -> Vec<u8> {
    let json_str = serde_json::to_string(data).unwrap_or_default();
    format!("event: {event}\ndata: {json_str}\n\n").into_bytes()
}

/// Convert a single chat.completion.chunk JSON to zero or more Responses API
/// SSE event frames (as raw bytes).
fn openai_chunk_to_responses(state: &mut ResponsesSseState, chunk: &Value) -> Vec<Vec<u8>> {
    let mut frames: Vec<Vec<u8>> = Vec::new();

    // ── Initialisation ────────────────────────────────────────────────
    if !state.started {
        state.started = true;
        state.response_id = chunk
            .get("id")
            .and_then(|v| v.as_str())
            .map(|id| {
                if id.starts_with("chatcmpl-") {
                    format!("resp_{}", &id[9..])
                } else {
                    generate_response_id()
                }
            })
            .unwrap_or_else(generate_response_id);
        state.created = chunk
            .get("created")
            .and_then(|v| v.as_i64())
            .unwrap_or_else(|| Utc::now().timestamp());
        state.model = chunk
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        state.seq += 1;
        frames.push(format_sse_event(
            "response.created",
            &json!({
                "type": "response.created",
                "response": response_skeleton(state),
            }),
        ));

        state.seq += 1;
        frames.push(format_sse_event(
            "response.in_progress",
            &json!({
                "type": "response.in_progress",
                "response": response_skeleton(state),
            }),
        ));
    }

    // ── Process choices ──────────────────────────────────────────────
    let Some(choices) = chunk.get("choices").and_then(|v| v.as_array()) else {
        return frames;
    };

    for choice in choices {
        let index = choice.get("index").and_then(|v| v.as_i64()).unwrap_or(0) as usize;
        let delta = choice
            .get("delta")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let delta = Value::Object(delta);
        let finish_reason = choice.get("finish_reason").and_then(|v| v.as_str());

        // ── Reasoning content ────────────────────────────────────────
        if let Some(reasoning) = delta
            .get("reasoning_content")
            .and_then(|v| v.as_str())
            .filter(|r| !r.is_empty())
        {
            if !state.reasoning_item_added {
                state.reasoning_item_added = true;
                let rid = generate_response_id();
                state.reasoning_id = Some(rid.clone());
                state.seq += 1;
                frames.push(format_sse_event(
                    "response.output_item.added",
                    &json!({
                        "type": "response.output_item.added",
                        "part_index": index,
                        "item": {
                            "id": rid,
                            "type": "reasoning",
                            "status": "in_progress",
                            "role": "assistant",
                        },
                    }),
                ));
            }

            if !state.reasoning_content_added {
                state.reasoning_content_added = true;
                state.seq += 1;
                frames.push(format_sse_event(
                    "response.reasoning_summary_part.added",
                    &json!({
                        "type": "response.reasoning_summary_part.added",
                        "part_index": index,
                        "part": {
                            "id": format!("reasoning_part_{}", state.seq),
                            "type": "summary_text",
                        },
                    }),
                ));
            }

            state.seq += 1;
            state
                .reasoning_buf
                .get_or_insert_with(String::new)
                .push_str(reasoning);
            frames.push(format_sse_event(
                "response.reasoning_summary_text.delta",
                &json!({
                    "type": "response.reasoning_summary_text.delta",
                    "delta": reasoning,
                }),
            ));
        }

        // ── Regular text content ─────────────────────────────────────
        if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
            if !state.msg_item_added.contains_key(&index) || !state.msg_item_added[&index] {
                state.msg_item_added.insert(index, true);
                let item_id = generate_response_id();
                state.added_item_id_map.insert(index, item_id.clone());
                state.seq += 1;
                frames.push(format_sse_event(
                    "response.output_item.added",
                    &json!({
                        "type": "response.output_item.added",
                        "part_index": index,
                        "item": {
                            "id": item_id,
                            "type": "message",
                            "status": "in_progress",
                            "role": "assistant",
                            "content": [],
                        },
                    }),
                ));
            }

            if !state.msg_content_added.contains_key(&index) || !state.msg_content_added[&index] {
                state.msg_content_added.insert(index, true);
                let part_id = generate_response_id();
                state
                    .added_content_part_id_map
                    .insert(index, part_id.clone());
                state.seq += 1;
                frames.push(format_sse_event(
                    "response.content_part.added",
                    &json!({
                        "type": "response.content_part.added",
                        "part_index": index,
                        "part": {
                            "id": part_id,
                            "type": "output_text",
                            "text": "",
                            "annotations": [],
                        },
                    }),
                ));
            }

            let buf = state.msg_text_buf.entry(index).or_insert_with(String::new);
            buf.push_str(content);
            // Only emit delta events for non-empty content to avoid
            // flooding the client with empty frames (many providers
            // send empty content chunks during streaming).
            if !content.is_empty() {
                state.seq += 1;
                frames.push(format_sse_event(
                    "response.output_text.delta",
                    &json!({
                        "type": "response.output_text.delta",
                        "delta": content,
                    }),
                ));
            }
        }

        // ── Finish reason ────────────────────────────────────────────
        if let Some(_reason) = finish_reason {
            // Close reasoning if we actually emitted reasoning events
            if !state.reasoning_done && state.reasoning_item_added {
                let reasoning_text = state
                    .reasoning_buf
                    .as_ref()
                    .map(|s| s.as_str())
                    .unwrap_or("");
                state.reasoning_done = true;
                state.seq += 1;
                frames.push(format_sse_event(
                    "response.reasoning_summary_text.done",
                    &json!({
                        "type": "response.reasoning_summary_text.done",
                        "part_index": index,
                        "text": reasoning_text,
                    }),
                ));
                state.seq += 1;
                frames.push(format_sse_event(
                    "response.content_part.done",
                    &json!({
                        "type": "response.content_part.done",
                        "part_index": index,
                        "part": {
                            "id": state
                                .added_content_part_id_map
                                .get(&index)
                                .map(|s| s.as_str())
                                .unwrap_or(""),
                            "type": "summary_text",
                        },
                    }),
                ));
            }

            // Close message text
            if !state.msg_item_done.contains_key(&index) || !state.msg_item_done[&index] {
                let text = state
                    .msg_text_buf
                    .get(&index)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                let part_id = state
                    .added_content_part_id_map
                    .get(&index)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                let item_id = state
                    .added_item_id_map
                    .get(&index)
                    .map(|s| s.as_str())
                    .unwrap_or("");

                state.seq += 1;
                frames.push(format_sse_event(
                    "response.output_text.done",
                    &json!({
                        "type": "response.output_text.done",
                        "part_index": index,
                        "text": text,
                    }),
                ));

                state.seq += 1;
                frames.push(format_sse_event(
                    "response.content_part.done",
                    &json!({
                        "type": "response.content_part.done",
                        "part_index": index,
                        "part": {
                            "id": part_id,
                            "type": "output_text",
                            "text": text,
                            "annotations": [],
                        },
                    }),
                ));

                state.seq += 1;
                frames.push(format_sse_event(
                    "response.output_item.done",
                    &json!({
                        "type": "response.output_item.done",
                        "part_index": index,
                        "item": {
                            "id": item_id,
                            "type": "message",
                            "role": "assistant",
                            "content": [
                                {
                                    "id": part_id,
                                    "type": "output_text",
                                    "text": text,
                                    "annotations": [],
                                }
                            ],
                        },
                    }),
                ));

                state.msg_item_done.insert(index, true);
            }
        }
    }

    // ── Usage + completed (last chunk) ──────────────────────────────
    let usage_in_chunk = chunk.get("usage").filter(|v| !v.is_null());
    let has_finish_reason =
        chunk
            .get("choices")
            .and_then(|v| v.as_array())
            .is_some_and(|choices| {
                choices
                    .iter()
                    .any(|c| c.get("finish_reason").and_then(|v| v.as_str()).is_some())
            });

    if usage_in_chunk.is_some() || (has_finish_reason && !state.completed_sent) {
        state.completed_sent = true;

        // Build final output array with all choices
        let mut output = Vec::new();
        for &idx in state.msg_item_done.keys() {
            let text = state
                .msg_text_buf
                .get(&idx)
                .map(|s| s.as_str())
                .unwrap_or("");
            let part_id = state
                .added_content_part_id_map
                .get(&idx)
                .map(|s| s.as_str())
                .unwrap_or("");
            let item_id = state
                .added_item_id_map
                .get(&idx)
                .map(|s| s.as_str())
                .unwrap_or("");
            output.push(json!({
                "id": item_id,
                "type": "message",
                "role": "assistant",
                "content": [
                    {
                        "id": part_id,
                        "type": "output_text",
                        "text": text,
                        "annotations": [],
                    }
                ],
            }));
        }

        // If we had reasoning output, include it too
        if state.reasoning_item_added {
            let reasoning_text = state
                .reasoning_buf
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("");
            let reasoning_id = state
                .reasoning_id
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("");
            output.insert(
                0,
                json!({
                    "id": reasoning_id,
                    "type": "reasoning",
                    "role": "assistant",
                    "content": [
                        {
                            "id": format!("{}_summary", reasoning_id),
                            "type": "summary_text",
                            "text": reasoning_text,
                        }
                    ],
                }),
            );
        }

        let mut skeleton = response_skeleton(state);
        let obj = skeleton.as_object_mut().unwrap();
        obj.insert("status".to_string(), json!("completed"));
        obj.insert("background".to_string(), json!(false));
        obj.insert("error".into(), Value::Null);
        if !output.is_empty() {
            obj.insert("output".to_string(), json!(output));
        }
        // Include usage from the last chunk (fix: 9router omit bug — sendCompleted()
        // in the JS version omits usage, breaking downstream consumers).
        // Responses API spec requires usage in response.completed.
        if let Some(usage) = chunk.get("usage").filter(|v| !v.is_null()) {
            obj.insert("usage".to_string(), usage.clone());
        }

        state.seq += 1;
        frames.push(format_sse_event(
            "response.completed",
            &json!({
                "type": "response.completed",
                "response": skeleton,
            }),
        ));
    }

    frames
}

// ══════════════════════════════════════════════════════════════════════════
// Anthropic Messages API format converter
// ══════════════════════════════════════════════════════════════════════════

/// Convert an OpenAI chat-completion response (streaming or non-streaming) to
/// the Anthropic Messages API format.
async fn convert_to_messages_api(response: Response) -> Response {
    let status = response.status();
    if !status.is_success() {
        return response;
    }

    let is_sse = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("text/event-stream") || ct.contains("text/plain"));

    if !is_sse {
        let (parts, body) = response.into_parts();
        let body_bytes = match body.collect().await {
            Ok(col) => col.to_bytes(),
            Err(_) => return Response::from_parts(parts, Body::empty()),
        };
        let Ok(chat_completion) = serde_json::from_slice::<Value>(&body_bytes) else {
            return Response::from_parts(parts, Body::from(body_bytes));
        };
        let messages_json = chat_completion_to_messages_json(&chat_completion);
        return Json(messages_json).into_response();
    }

    let (parts, body) = response.into_parts();
    let data_stream = body.into_data_stream();
    let converted = async_stream::stream! {
        let buf = &mut String::new();
        let mut conv_state = MessagesSseState::new();

        let mut stream = data_stream;
        loop {
            let next = stream.next().await;
            match next {
                Some(Ok(chunk)) => {
                    let chunk_str = String::from_utf8_lossy(&chunk);
                    buf.push_str(&chunk_str);

                    loop {
                        if let Some(frame_end) = buf.find("\n\n") {
                            let frame = buf[..frame_end].to_string();
                            buf.drain(..frame_end + 2);

                            let frame = frame.trim();
                            if frame.is_empty() || frame.starts_with(':') {
                                continue;
                            }

                            let json_str = frame.strip_prefix("data:").unwrap_or(&frame).trim();
                            if json_str == "[DONE]" {
                                break;
                            }

                            if let Ok(chunk_value) = serde_json::from_str::<Value>(json_str) {
                                let events = openai_chunk_to_messages(&mut conv_state, &chunk_value);
                                for event_bytes in events {
                                    yield Ok::<Bytes, std::io::Error>(Bytes::from(event_bytes));
                                }
                            }
                        } else {
                            break;
                        }
                    }
                }
                Some(Err(_)) | None => break,
            }
        }

        // Send message_stop if not already sent
        if !conv_state.stop_sent {
            conv_state.stop_sent = true;
            yield Ok::<Bytes, std::io::Error>(Bytes::from("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
        }
    };

    let body = Body::from_stream(converted);
    let mut resp = Response::new(body);
    *resp.status_mut() = status;

    for (name, value) in &parts.headers {
        if name.as_str() != "content-length"
            && name.as_str() != "content-type"
            && name.as_str() != "transfer-encoding"
        {
            resp.headers_mut().insert(name, value.clone());
        }
    }
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));

    resp
}

/// Map OpenAI finish_reason to Anthropic stop_reason.
fn stop_reason_map(finish_reason: Option<&str>) -> Value {
    match finish_reason {
        Some("stop") => json!("end_turn"),
        Some("length") => json!("max_tokens"),
        Some("tool_calls") => json!("tool_use"),
        Some(other) => json!(other),
        None => Value::Null,
    }
}

/// Convert OpenAI usage to Anthropic usage format.
fn usage_to_anthropic(usage: Option<&Value>) -> Option<Value> {
    let usage = usage?;
    let input_tokens = usage
        .get("prompt_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    Some(json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
    }))
}

/// Convert a completed chat-completion JSON object to Anthropic Messages API JSON.
fn chat_completion_to_messages_json(source: &Value) -> Value {
    let msg_id = source
        .get("id")
        .and_then(|v| v.as_str())
        .map(|id| {
            if id.starts_with("chatcmpl-") {
                format!("msg_{}", &id[9..])
            } else {
                format!("msg_{}", id)
            }
        })
        .unwrap_or_else(|| format!("msg_{:x}", Utc::now().timestamp_nanos_opt().unwrap_or(0)));
    let model = source.get("model").and_then(|v| v.as_str()).unwrap_or("");

    let mut content = Vec::new();
    let mut stop_reason = Value::Null;
    let mut usage = None;

    if let Some(choices) = source.get("choices").and_then(|v| v.as_array()) {
        if let Some(first_choice) = choices.first() {
            stop_reason =
                stop_reason_map(first_choice.get("finish_reason").and_then(|v| v.as_str()));

            if let Some(message) = first_choice.get("message") {
                // reasoning_content → thinking block
                if let Some(reasoning) = message.get("reasoning_content").and_then(|v| v.as_str()) {
                    if !reasoning.is_empty() {
                        content.push(json!({
                            "type": "thinking",
                            "thinking": reasoning,
                            "signature": null,
                        }));
                    }
                }

                let text = message
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !text.is_empty() {
                    content.push(json!({
                        "type": "text",
                        "text": text,
                    }));
                }

                // Convert OpenAI tool_calls to Anthropic tool_use blocks
                if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tool_calls {
                        let tc_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let args_str = tc
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}");
                        let input: Value =
                            serde_json::from_str(args_str).unwrap_or(Value::Object(Map::new()));

                        content.push(json!({
                            "type": "tool_use",
                            "id": tc_id,
                            "name": name,
                            "input": input,
                        }));
                    }
                }
            }
        }
    }

    if content.is_empty() {
        content.push(json!({
            "type": "text",
            "text": "",
        }));
    }

    if let Some(u) = source.get("usage") {
        usage = usage_to_anthropic(Some(u));
    }

    json!({
        "id": msg_id,
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": model,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": usage,
    })
}

/// State machine for streaming chat.completion.chunk → Anthropic Messages API SSE.
struct MessagesSseState {
    started: bool,
    msg_id: String,
    model: String,

    // Thinking block state
    thinking_started: bool,
    thinking_buf: String,
    thinking_stopped: bool,

    // Text block state
    text_started: bool,
    text_buf: String,
    text_stopped: bool,

    // Block index counter
    block_idx: usize,

    // Whether final events were sent
    stop_sent: bool,

    // Tool call streaming state
    // Each tool call is tracked by its index in the delta's tool_calls array.
    // Vectors are grown on demand when a new index appears.
    toolcall_active: Vec<bool>,  // per-index: content_block_start emitted?
    toolcall_ids: Vec<String>,   // per-index: tool call id
    toolcall_names: Vec<String>, // per-index: tool call name
    toolcall_args: Vec<String>,  // per-index: accumulated arguments JSON
    toolcall_start_indices: Vec<usize>, // per-index: block_idx at content_block_start
}

impl MessagesSseState {
    fn new() -> Self {
        Self {
            started: false,
            msg_id: String::new(),
            model: String::new(),
            thinking_started: false,
            thinking_buf: String::new(),
            thinking_stopped: false,
            text_started: false,
            text_buf: String::new(),
            text_stopped: false,
            block_idx: 0,
            stop_sent: false,
            toolcall_active: Vec::new(),
            toolcall_ids: Vec::new(),
            toolcall_names: Vec::new(),
            toolcall_args: Vec::new(),
            toolcall_start_indices: Vec::new(),
        }
    }

    /// Ensure tool call vectors are large enough for the given index.
    fn ensure_toolcall_idx(&mut self, idx: usize) {
        while self.toolcall_active.len() <= idx {
            self.toolcall_active.push(false);
            self.toolcall_ids.push(String::new());
            self.toolcall_names.push(String::new());
            self.toolcall_args.push(String::new());
            self.toolcall_start_indices.push(0);
        }
    }

    /// True if any tool call is currently active (started but not finished).
    fn has_active_toolcalls(&self) -> bool {
        self.toolcall_active.iter().any(|&a| a)
    }
}

/// Format an Anthropic Messages API SSE event frame.
fn format_messages_sse_event(event: &str, data: &Value) -> Vec<u8> {
    let json_str = serde_json::to_string(data).unwrap_or_default();
    format!("event: {event}\ndata: {json_str}\n\n").into_bytes()
}

/// Convert a single chat.completion.chunk to Anthropic Messages API SSE events.
fn openai_chunk_to_messages(state: &mut MessagesSseState, chunk: &Value) -> Vec<Vec<u8>> {
    let mut frames: Vec<Vec<u8>> = Vec::new();

    // ── Initialize on first chunk ──────────────────────────────────────────
    if !state.started {
        state.started = true;
        state.msg_id = chunk
            .get("id")
            .and_then(|v| v.as_str())
            .map(|id| {
                if id.starts_with("chatcmpl-") {
                    format!("msg_{}", &id[9..])
                } else {
                    format!("msg_{}", id)
                }
            })
            .unwrap_or_else(|| format!("msg_{:x}", Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        state.model = chunk
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // emit message_start
        frames.push(format_messages_sse_event(
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": state.msg_id,
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": state.model,
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": { "input_tokens": 0, "output_tokens": 0 },
                },
            }),
        ));
    }

    let Some(choices) = chunk.get("choices").and_then(|v| v.as_array()) else {
        return frames;
    };

    for choice in choices {
        let delta = choice
            .get("delta")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let delta = Value::Object(delta);
        let finish_reason = choice.get("finish_reason").and_then(|v| v.as_str());

        // ── Reasoning / thinking content ──────────────────────────────────────
        if let Some(reasoning) = delta
            .get("reasoning_content")
            .and_then(|v| v.as_str())
            .filter(|r| !r.is_empty())
        {
            if !state.thinking_started {
                state.thinking_started = true;
                let idx = state.block_idx;
                state.block_idx += 1;
                frames.push(format_messages_sse_event(
                    "content_block_start",
                    &json!({
                        "type": "content_block_start",
                        "index": idx,
                        "content_block": {
                            "type": "thinking",
                            "thinking": "",
                            "signature": null,
                        },
                    }),
                ));
            }

            state.thinking_buf.push_str(reasoning);
            let thinking_idx = if state.thinking_started && !state.text_started {
                0
            } else {
                0
            };
            frames.push(format_messages_sse_event(
                "content_block_delta",
                &json!({
                    "type": "content_block_delta",
                    "index": thinking_idx,
                    "delta": {
                        "type": "thinking_delta",
                        "thinking": reasoning,
                    },
                }),
            ));
        }

        // ── Text content ──────────────────────────────────────────────────────
        if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
            // Skip empty content deltas when tool calls are being streamed
            // (DeepSeek sends content:"" alongside finish_reason:"tool_calls")
            let skip_empty = state.has_active_toolcalls() && content.is_empty();
            if !skip_empty {
                // Close thinking block if we're transitioning to text
                if state.thinking_started && !state.thinking_stopped {
                    state.thinking_stopped = true;
                    frames.push(format_messages_sse_event(
                        "content_block_stop",
                        &json!({
                            "type": "content_block_stop",
                            "index": 0,
                        }),
                    ));
                }

                if !state.text_started {
                    state.text_started = true;
                    let idx = state.block_idx;
                    state.block_idx += 1;
                    frames.push(format_messages_sse_event(
                        "content_block_start",
                        &json!({
                            "type": "content_block_start",
                            "index": idx,
                            "content_block": {
                                "type": "text",
                                "text": "",
                            },
                        }),
                    ));
                }

                state.text_buf.push_str(content);
                if !content.is_empty() {
                    let text_idx = if state.thinking_started { 1 } else { 0 };
                    frames.push(format_messages_sse_event(
                        "content_block_delta",
                        &json!({
                            "type": "content_block_delta",
                            "index": text_idx,
                            "delta": {
                                "type": "text_delta",
                                "text": content,
                            },
                        }),
                    ));
                }
            }
        }

        // ── Tool calls ──────────────────────────────────────────────────────
        // OpenAI streams tool_calls in the delta. Convert to Anthropic tool_use
        // content blocks with input_json_delta for streaming arguments.
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            // Close thinking block if open (use a simple flag to avoid double-close)
            if state.thinking_started && !state.thinking_stopped {
                state.thinking_stopped = true;
                frames.push(format_messages_sse_event(
                    "content_block_stop",
                    &json!({"type": "content_block_stop", "index": 0}),
                ));
            }

            for tc in tool_calls {
                let tcidx = tc.get("index").and_then(|v| v.as_i64()).unwrap_or(0) as usize;
                state.ensure_toolcall_idx(tcidx);

                // First chunk: has id + function.name → emit content_block_start
                if let Some(id) = tc
                    .get("id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    if !state.toolcall_active[tcidx] {
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        state.toolcall_active[tcidx] = true;
                        state.toolcall_ids[tcidx] = id.to_string();
                        state.toolcall_names[tcidx] = name.to_string();

                        let idx = state.block_idx;
                        state.block_idx += 1;
                        state.toolcall_start_indices[tcidx] = idx;

                        frames.push(format_messages_sse_event(
                            "content_block_start",
                            &json!({
                                "type": "content_block_start",
                                "index": idx,
                                "content_block": {
                                    "type": "tool_use",
                                    "id": id,
                                    "name": name,
                                },
                            }),
                        ));
                    }
                }

                // Subsequent chunks: arguments delta → emit content_block_delta (input_json_delta)
                if let Some(args) = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|v| v.as_str())
                {
                    if !args.is_empty() && state.toolcall_active[tcidx] {
                        state.toolcall_args[tcidx].push_str(args);
                        let tcidx_start = state.toolcall_start_indices[tcidx];
                        frames.push(format_messages_sse_event(
                            "content_block_delta",
                            &json!({
                                "type": "content_block_delta",
                                "index": tcidx_start,
                                "delta": {
                                    "type": "input_json_delta",
                                    "partial_json": args,
                                },
                            }),
                        ));
                    }
                }
            }
        }

        // ── Finish reason → close blocks + message_delta + message_stop ──────
        if finish_reason.is_some() && !state.stop_sent {
            // Close tool call blocks
            for tcidx in 0..state.toolcall_active.len() {
                if state.toolcall_active[tcidx] {
                    state.toolcall_active[tcidx] = false;
                    let tcidx_start = state.toolcall_start_indices[tcidx];
                    frames.push(format_messages_sse_event(
                        "content_block_stop",
                        &json!({"type": "content_block_stop", "index": tcidx_start}),
                    ));
                }
            }

            // Close text block if open
            if state.text_started && !state.text_stopped {
                state.text_stopped = true;
                let text_idx = if state.thinking_started { 1 } else { 0 };
                frames.push(format_messages_sse_event(
                    "content_block_stop",
                    &json!({
                        "type": "content_block_stop",
                        "index": text_idx,
                    }),
                ));
            }

            // Close thinking block if still open
            if state.thinking_started && !state.thinking_stopped {
                state.thinking_stopped = true;
                frames.push(format_messages_sse_event(
                    "content_block_stop",
                    &json!({
                        "type": "content_block_stop",
                        "index": 0,
                    }),
                ));
            }

            let stop_reason = stop_reason_map(finish_reason);
            let usage = chunk
                .get("usage")
                .and_then(|u| usage_to_anthropic(Some(u)))
                .unwrap_or(json!({ "input_tokens": 0, "output_tokens": 0 }));

            frames.push(format_messages_sse_event(
                "message_delta",
                &json!({
                    "type": "message_delta",
                    "delta": {
                        "stop_reason": stop_reason,
                        "stop_sequence": null,
                    },
                    "usage": usage,
                }),
            ));

            state.stop_sent = true;
            frames.push(format_messages_sse_event(
                "message_stop",
                &json!({
                    "type": "message_stop",
                }),
            ));
        }
    }

    frames
}

// ---------------------------------------------------------------------------
// Body normalisation (unchanged below this line)
// ---------------------------------------------------------------------------

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

            normalize_tools(fields);
            normalize_tool_choice(fields);
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

            normalize_tools(fields);
            normalize_tool_choice(fields);
        }
    }

    body
}

/// Detect whether a tool definition is in Anthropic format (has bare `name`/`input_schema`)
/// and convert it to OpenAI function format (`type:function`, `function:{name,description,parameters}`).
/// If the tool is already in OpenAI format or has no recognizable structure, leave it unchanged.
fn normalize_tools(fields: &mut Map<String, Value>) {
    let Some(tools) = fields.get("tools").and_then(|v| v.as_array()).cloned() else {
        return;
    };

    let converted: Vec<Value> = tools
        .into_iter()
        .filter(|tool| {
            // Drop non-function tools (e.g. namespace) — DeepSeek rejects unknown types
            let t = tool.get("type").and_then(|v| v.as_str()).unwrap_or("");
            t.is_empty() || t == "function"
        })
        .map(|tool| {
            let has_function_field = tool.get("function").is_some();
            let is_function_type = tool.get("type").and_then(|v| v.as_str()) == Some("function");

            // Already proper OpenAI format {type:"function", function:{name,...}}
            if has_function_field {
                return tool;
            }

            // type:"function" but missing function:{} (e.g. flat Claude-style)
            // → convert to proper OpenAI format
            if is_function_type {
                let name = tool.get("name").cloned().unwrap_or(Value::Null);
                let description = tool.get("description").cloned().unwrap_or_default();
                let parameters = tool
                    .get("parameters")
                    .cloned()
                    .or_else(|| tool.get("input_schema").cloned())
                    .unwrap_or(json!({"type":"object","properties":{}}));
                return json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": description,
                        "parameters": parameters,
                    }
                });
            }

            let has_name = tool.get("name").and_then(|v| v.as_str()).is_some();
            let has_input_schema = tool.get("input_schema").is_some();
            if !has_name || !has_input_schema {
                return tool;
            }

            let name = tool.get("name").cloned().unwrap_or(Value::Null);
            let description = tool
                .get("description")
                .cloned()
                .unwrap_or(Value::String(String::new()));
            let parameters = tool
                .get("input_schema")
                .cloned()
                .unwrap_or(Value::Object(Map::new()));

            json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": description,
                    "parameters": parameters,
                }
            })
        })
        .collect();

    if converted
        .iter()
        .any(|t| t.get("type").and_then(|v| v.as_str()) == Some("function"))
    {
        fields.insert("tools".to_string(), Value::Array(converted));
    }
}

/// Convert Anthropic-style tool_choice to OpenAI-style tool_choice.
///
/// Anthropic: `{"type":"auto"}`, `{"type":"any"}`, `{"type":"tool","name":"xxx"}`
/// OpenAI:   `"auto"`,            `"required"`,     `{"type":"function","function":{"name":"xxx"}}`
///
/// If tool_choice is already a plain string or valid OpenAI object, leave it unchanged.
fn normalize_tool_choice(fields: &mut Map<String, Value>) {
    let Some(tc) = fields.get("tool_choice").cloned() else {
        return;
    };

    if tc.is_string() {
        return;
    }

    let Some(obj) = tc.as_object() else {
        return;
    };

    let tc_type = obj.get("type").and_then(|v| v.as_str());
    let name = obj.get("name").and_then(|v| v.as_str());

    match tc_type {
        Some("auto") => {
            fields.insert("tool_choice".to_string(), Value::String("auto".to_string()));
        }
        Some("any") => {
            fields.insert(
                "tool_choice".to_string(),
                Value::String("required".to_string()),
            );
        }
        Some("tool") if name.is_some() => {
            fields.insert(
                "tool_choice".to_string(),
                json!({
                    "type": "function",
                    "function": { "name": name }
                }),
            );
        }
        _ => {
            // Already in OpenAI format or unrecognized — leave unchanged
        }
    }
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
    with_cors_response(
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid JSON body" })),
        )
            .into_response(),
    )
}

fn with_cors_response(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("POST, OPTIONS"),
    );
    response
}

fn cors_preflight_response(methods: &str) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_str(methods).unwrap_or(HeaderValue::from_static("POST, OPTIONS")),
    );
    response
}

/// Convert a Claude-format non-streaming response body to OpenAI chat.completion format.
/// Port of 9router's translateNonStreamingResponse Claude branch.
fn claude_body_to_chat_completion(body: &Value) -> Value {
    // body: { "type":"message", "content":[{"type":"text","text":"..."}],
    //         "stop_reason":"end_turn", "usage":{...}, "id":"msg_...", "model":"..." }
    let mut text_content = String::new();
    let mut thinking_content = String::new();
    let mut tool_calls = Vec::new();

    if let Some(content) = body.get("content").and_then(Value::as_array) {
        for block in content {
            let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
            match block_type {
                "text" => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        text_content.push_str(text);
                    }
                }
                "thinking" => {
                    if let Some(t) = block.get("thinking").and_then(Value::as_str) {
                        thinking_content.push_str(t);
                    }
                }
                "tool_use" => {
                    let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                    let args = block.get("input").cloned().unwrap_or(json!({}));
                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string())
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    let mut message = json!({"role": "assistant"});
    if !text_content.is_empty() {
        message["content"] = json!(text_content);
    }
    if !thinking_content.is_empty() {
        message["reasoning_content"] = json!(thinking_content);
    }
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }
    if !text_content.is_empty() || tool_calls.is_empty() {
        message["content"] = message.get("content").cloned().unwrap_or(json!(""));
    }

    let stop_reason = body
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or("stop");
    let finish_reason = match stop_reason {
        "end_turn" => "stop",
        "tool_use" => "tool_calls",
        other => other,
    };

    let id = body
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("msg_unknown");
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let created = chrono::Utc::now().timestamp();

    let mut result = json!({
        "id": format!("chatcmpl-{}", id),
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason
        }]
    });

    if let Some(usage) = body.get("usage") {
        let input_tokens = usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let output_tokens = usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        result["usage"] = json!({
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens
        });
    }

    result
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

    // ── Responses API SSE conversion tests ──────────────────────────

    #[test]
    fn openai_chunk_to_responses_starts_with_created_event() {
        let mut state = ResponsesSseState::new();
        let chunk = json!({
            "id": "chatcmpl-abc123",
            "object": "chat.completion.chunk",
            "created": 1712345678,
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "Hello" },
                "finish_reason": null
            }]
        });

        let frames = openai_chunk_to_responses(&mut state, &chunk);
        let all_frames = frames.concat();
        let sse = String::from_utf8_lossy(&all_frames);

        assert!(
            sse.contains("event: response.created\n"),
            "should emit response.created"
        );
        assert!(
            sse.contains("event: response.in_progress\n"),
            "should emit response.in_progress"
        );
        assert!(
            sse.contains("event: response.output_item.added\n"),
            "should emit output_item.added"
        );
        assert!(
            sse.contains("event: response.content_part.added\n"),
            "should emit content_part.added"
        );
        assert!(
            sse.contains("event: response.output_text.delta\n"),
            "should emit output_text.delta"
        );
        assert!(
            sse.contains("\"delta\":\"Hello\""),
            "delta should contain 'Hello'"
        );
    }

    #[test]
    fn openai_chunk_to_responses_completes_on_finish_reason() {
        let mut state = ResponsesSseState::new();
        let chunk1 = json!({
            "id": "chatcmpl-abc123",
            "object": "chat.completion.chunk",
            "created": 1712345678,
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "Hi" },
                "finish_reason": null
            }]
        });
        let chunk2 = json!({
            "id": "chatcmpl-abc123",
            "object": "chat.completion.chunk",
            "created": 1712345678,
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
        });

        let _ = openai_chunk_to_responses(&mut state, &chunk1);
        let frames = openai_chunk_to_responses(&mut state, &chunk2);
        let all_frames = frames.concat();
        let sse = String::from_utf8_lossy(&all_frames);

        assert!(
            sse.contains("event: response.output_text.done\n"),
            "should emit output_text.done"
        );
        assert!(
            sse.contains("event: response.content_part.done\n"),
            "should emit content_part.done"
        );
        assert!(
            sse.contains("event: response.output_item.done\n"),
            "should emit output_item.done"
        );
        assert!(
            sse.contains("event: response.completed\n"),
            "should emit completed"
        );
        assert!(
            sse.contains("\"total_tokens\":12"),
            "usage should be included"
        );
    }

    #[test]
    fn openai_chunk_to_responses_handles_reasoning_content() {
        let mut state = ResponsesSseState::new();
        let chunk = json!({
            "id": "chatcmpl-def456",
            "object": "chat.completion.chunk",
            "created": 1712345678,
            "model": "deepseek-chat",
            "choices": [{
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "reasoning_content": "Let me think..."
                },
                "finish_reason": null
            }]
        });

        let frames = openai_chunk_to_responses(&mut state, &chunk);
        let all_frames = frames.concat();
        let sse = String::from_utf8_lossy(&all_frames);

        assert!(
            sse.contains("event: response.reasoning_summary_text.delta\n"),
            "should emit reasoning delta"
        );
        assert!(
            sse.contains("\"delta\":\"Let me think...\""),
            "reasoning delta content"
        );
    }

    #[test]
    fn chat_completion_to_responses_json_converts_non_streaming() {
        let chat = json!({
            "id": "chatcmpl-xyz789",
            "object": "chat.completion",
            "created": 1712345678,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello there!"
                },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8 }
        });

        let resp = chat_completion_to_responses_json(&chat);

        assert_eq!(resp["object"], "response");
        assert_eq!(resp["status"], "completed");
        assert_eq!(resp["model"], "gpt-4o");
        assert_eq!(resp["output"][0]["type"], "message");
        assert_eq!(resp["output"][0]["content"][0]["type"], "output_text");
        assert_eq!(resp["output"][0]["content"][0]["text"], "Hello there!");
        assert_eq!(resp["usage"]["total_tokens"], 8);
    }

    #[test]
    fn chat_completion_to_responses_json_includes_reasoning() {
        let chat = json!({
            "id": "chatcmpl-rst000",
            "object": "chat.completion",
            "created": 1712345678,
            "model": "deepseek-r1",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Final answer",
                    "reasoning_content": "Step by step thinking..."
                },
                "finish_reason": "stop"
            }],
            "usage": { "total_tokens": 42 }
        });

        let resp = chat_completion_to_responses_json(&chat);

        assert_eq!(resp["output"][0]["type"], "message");
        assert_eq!(
            resp["output"][0]["content"].as_array().unwrap().len(),
            2,
            "should have both reasoning and text content parts"
        );
        assert_eq!(resp["output"][0]["content"][0]["type"], "summary_text");
        assert_eq!(
            resp["output"][0]["content"][0]["text"],
            "Step by step thinking..."
        );
        assert_eq!(resp["output"][0]["content"][1]["type"], "output_text");
        assert_eq!(resp["output"][0]["content"][1]["text"], "Final answer");
    }

    fn normalize_tools_converts_anthropic_to_openai() {
        let mut map = Map::new();
        map.insert(
            "tools".to_string(),
            json!([
                {
                    "name": "get_weather",
                    "description": "Get weather",
                    "input_schema": {
                        "type": "object",
                        "properties": { "location": { "type": "string" } },
                        "required": ["location"]
                    }
                }
            ]),
        );
        map.insert("tool_choice".to_string(), json!({"type": "auto"}));

        normalize_tools(&mut map);
        normalize_tool_choice(&mut map);

        let tools = map.get("tools").and_then(|v| v.as_array()).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
        assert_eq!(tools[0]["function"]["description"], "Get weather");
        assert!(tools[0]["function"]["parameters"]["properties"]["location"].is_object());

        let tc = map.get("tool_choice").unwrap();
        assert_eq!(tc, "auto");
    }

    fn normalize_tools_leaves_openai_format_unchanged() {
        let mut map = Map::new();
        map.insert(
            "tools".to_string(),
            json!([
                {
                    "type": "function",
                    "function": {
                        "name": "existing_tool",
                        "description": "Already in correct format",
                        "parameters": { "type": "object", "properties": {} }
                    }
                }
            ]),
        );

        normalize_tools(&mut map);

        let tools = map.get("tools").and_then(|v| v.as_array()).unwrap();
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "existing_tool");
    }

    fn normalize_tool_choice_converts_any_to_required() {
        let mut map = Map::new();
        map.insert("tool_choice".to_string(), json!({"type": "any"}));
        normalize_tool_choice(&mut map);
        assert_eq!(map.get("tool_choice").unwrap(), "required");
    }

    fn normalize_tool_choice_converts_tool_with_name() {
        let mut map = Map::new();
        map.insert(
            "tool_choice".to_string(),
            json!({"type": "tool", "name": "my_func"}),
        );
        normalize_tool_choice(&mut map);
        let tc = map.get("tool_choice").unwrap();
        assert_eq!(tc["type"], "function");
        assert_eq!(tc["function"]["name"], "my_func");
    }

    fn normalize_body_converts_anthropic_tools_in_messages_mode() {
        let body = json!({
            "model": "claude-sonnet-4",
            "system": "Be helpful",
            "messages": [{"role": "user", "content": "Hi"}],
            "tools": [
                {
                    "name": "my_tool",
                    "description": "The tool",
                    "input_schema": { "type": "object", "properties": {} }
                }
            ],
            "tool_choice": {"type": "any"}
        });

        let normalized = normalize_body(body, CompatMode::Messages);
        let tools = normalized["tools"].as_array().unwrap();
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "my_tool");
        assert_eq!(normalized["tool_choice"], "required");
    }
}
