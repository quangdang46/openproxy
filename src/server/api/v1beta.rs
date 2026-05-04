use std::collections::HashSet;
use std::convert::Infallible;

use async_stream::stream;
use axum::body::{to_bytes, Body};
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::server::state::AppState;
use crate::types::ProviderConnection;

use super::chat;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1beta/models",
            get(list_gemini_models).options(cors_options_get),
        )
        .route(
            "/v1beta/models/{*path}",
            post(handle_gemini_models_path).options(cors_options_post),
        )
}

async fn cors_options_get() -> Response {
    cors_preflight_response("GET, OPTIONS")
}

async fn cors_options_post() -> Response {
    cors_preflight_response("GET, POST, OPTIONS")
}

async fn list_gemini_models(State(state): State<AppState>) -> Response {
    let snapshot = state.db.snapshot();
    let mut seen = HashSet::new();
    let mut models = Vec::new();

    for connection in snapshot
        .provider_connections
        .iter()
        .filter(|connection| connection.is_active())
    {
        for model_id in models_for_connection(connection) {
            let name = normalize_model_name(&connection.provider, &model_id);
            if !seen.insert(name.clone()) {
                continue;
            }

            let display_name = name
                .strip_prefix("models/")
                .unwrap_or(name.as_str())
                .replace('/', " ");

            models.push(GeminiModelCard {
                name,
                display_name,
                description: format!("Available model: {model_id}"),
                supported_generation_methods: vec!["generateContent".to_string()],
                input_token_limit: 128_000,
                output_token_limit: 8_192,
            });
        }
    }

    for model in snapshot
        .custom_models
        .iter()
        .filter(|model| model.r#type.is_empty() || model.r#type == "llm")
    {
        let name = normalize_model_name(&model.provider_alias, &model.id);
        if !seen.insert(name.clone()) {
            continue;
        }

        models.push(GeminiModelCard {
            display_name: model.name.clone().unwrap_or_else(|| model.id.clone()),
            description: format!("Custom model: {}", model.id),
            name,
            supported_generation_methods: vec!["generateContent".to_string()],
            input_token_limit: 128_000,
            output_token_limit: 8_192,
        });
    }

    with_cors_json(StatusCode::OK, json!({ "models": models }))
}

async fn handle_gemini_models_path(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(path): axum::extract::Path<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let request_body = match body {
        Ok(Json(body)) => body,
        Err(_) => {
            return with_cors_json(
                StatusCode::BAD_REQUEST,
                json!({ "error": { "message": "Invalid JSON body", "code": 400 } }),
            )
        }
    };

    let (model, stream) = parse_gemini_model_path(&path);
    let converted_body = convert_gemini_to_internal(&request_body, &model, stream);

    let upstream = chat::chat_completions(State(state), headers, Ok(Json(converted_body))).await;
    if stream {
        convert_openai_sse_to_gemini(upstream, &model).await
    } else {
        convert_openai_json_to_gemini(upstream, &model).await
    }
}

fn parse_gemini_model_path(path: &str) -> (String, bool) {
    let trimmed = path.trim_matches('/');
    let mut parts = trimmed
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let last = parts.pop().unwrap_or_default();
    let stream = last.contains(":streamGenerateContent");
    let model_tail = last
        .replace(":streamGenerateContent", "")
        .replace(":generateContent", "");

    if parts.is_empty() {
        (model_tail, stream)
    } else {
        parts.push(model_tail.as_str());
        (parts.join("/"), stream)
    }
}

fn convert_gemini_to_internal(gemini_body: &Value, model: &str, stream: bool) -> Value {
    let mut messages = Vec::new();

    if let Some(system) = gemini_body.get("systemInstruction") {
        let system_text = extract_gemini_text(system);
        if !system_text.is_empty() {
            messages.push(json!({ "role": "system", "content": system_text }));
        }
    }

    if let Some(contents) = gemini_body.get("contents").and_then(Value::as_array) {
        for content in contents {
            let role = match content.get("role").and_then(Value::as_str) {
                Some("model") => "assistant",
                _ => "user",
            };
            let text = content
                .get("parts")
                .map(extract_gemini_text)
                .unwrap_or_default();
            messages.push(json!({ "role": role, "content": text }));
        }
    }

    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(model.to_string()));
    body.insert("messages".to_string(), Value::Array(messages));
    body.insert("stream".to_string(), Value::Bool(stream));

    if let Some(config) = gemini_body
        .get("generationConfig")
        .and_then(Value::as_object)
    {
        if let Some(value) = config.get("maxOutputTokens").cloned() {
            body.insert("max_tokens".to_string(), value);
        }
        if let Some(value) = config.get("temperature").cloned() {
            body.insert("temperature".to_string(), value);
        }
        if let Some(value) = config.get("topP").cloned() {
            body.insert("top_p".to_string(), value);
        }
    }

    Value::Object(body)
}

fn extract_gemini_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Object(object) => object
            .get("parts")
            .map(extract_gemini_text)
            .or_else(|| {
                object
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

async fn convert_openai_json_to_gemini(response: Response, fallback_model: &str) -> Response {
    let status = response.status();
    let body_bytes = match to_bytes(response.into_body(), usize::MAX).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return with_cors_json(
                StatusCode::BAD_GATEWAY,
                json!({ "error": { "message": "Failed to read upstream response", "code": 502 } }),
            )
        }
    };

    let parsed = match serde_json::from_slice::<Value>(&body_bytes) {
        Ok(parsed) => parsed,
        Err(_) => {
            let mut raw = Response::new(Body::from(body_bytes));
            *raw.status_mut() = status;
            raw.headers_mut().insert(
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_static("*"),
            );
            raw.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            return raw;
        }
    };

    if !status.is_success() || parsed.get("error").is_some() {
        return with_cors_json(status, parsed);
    }

    if parsed.get("candidates").is_some() {
        return with_cors_json(status, parsed);
    }

    let Some(choice) = parsed
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    else {
        return with_cors_json(status, parsed);
    };

    let message = choice.get("message").cloned().unwrap_or_else(|| json!({}));
    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .unwrap_or("stop");

    let mut parts = Vec::new();
    if let Some(reasoning) = message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        parts.push(json!({ "text": reasoning, "thought": true }));
    }

    let content = message_to_text(&message);
    parts.push(json!({ "text": content }));

    let mut gemini_response = json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": parts,
            },
            "finishReason": finish_reason_to_gemini(finish_reason),
            "index": 0,
        }],
        "modelVersion": parsed
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(fallback_model),
    });

    if let Some(usage) = parsed.get("usage") {
        let mut usage_metadata = json!({
            "promptTokenCount": usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
            "candidatesTokenCount": usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0),
            "totalTokenCount": usage.get("total_tokens").and_then(Value::as_u64).unwrap_or(0),
        });
        if let Some(reasoning_tokens) = usage
            .get("completion_tokens_details")
            .and_then(|details| details.get("reasoning_tokens"))
            .and_then(Value::as_u64)
        {
            usage_metadata["thoughtsTokenCount"] = Value::from(reasoning_tokens);
        }
        gemini_response["usageMetadata"] = usage_metadata;
    }

    with_cors_json(status, gemini_response)
}

async fn convert_openai_sse_to_gemini(response: Response, fallback_model: &str) -> Response {
    let status = response.status();
    if !status.is_success() {
        return with_cors_nonstream_response(response).await;
    }

    let mut headers = response.headers().clone();
    let mut body_stream = response.into_body().into_data_stream();
    let mut state = GeminiSseTransformState::default();
    let fallback_model = fallback_model.to_string();

    let stream = stream! {
        while let Some(next) = body_stream.next().await {
            let Ok(chunk) = next else {
                break;
            };

            for event in state.transform(&chunk, &fallback_model) {
                yield Ok::<_, Infallible>(event);
            }
        }
    };

    let mut proxied = Response::new(Body::from_stream(stream));
    *proxied.status_mut() = status;
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    *proxied.headers_mut() = headers;
    proxied
}

async fn with_cors_nonstream_response(response: Response) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let body_bytes = match to_bytes(response.into_body(), usize::MAX).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return with_cors_json(
                StatusCode::BAD_GATEWAY,
                json!({ "error": { "message": "Failed to read upstream response", "code": 502 } }),
            )
        }
    };

    let mut proxied = Response::new(Body::from(body_bytes));
    *proxied.status_mut() = status;
    for (name, value) in &headers {
        proxied.headers_mut().insert(name, value.clone());
    }
    proxied.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    proxied
}

fn with_cors_json(status: StatusCode, payload: Value) -> Response {
    let mut response = (status, Json(payload)).into_response();
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response
}

fn cors_preflight_response(methods: &str) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_str(methods).unwrap_or(HeaderValue::from_static("GET, POST, OPTIONS")),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
    response
}

fn models_for_connection(connection: &ProviderConnection) -> Vec<String> {
    if let Some(enabled_models) = connection
        .provider_specific_data
        .get("enabledModels")
        .and_then(Value::as_array)
    {
        let models: Vec<_> = enabled_models
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect();

        if !models.is_empty() {
            return models;
        }
    }

    connection
        .default_model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| vec![value.to_string()])
        .unwrap_or_default()
}

fn normalize_model_name(provider: &str, model_id: &str) -> String {
    if model_id.contains('/') {
        format!("models/{model_id}")
    } else {
        format!("models/{provider}/{model_id}")
    }
}

fn finish_reason_to_gemini(reason: &str) -> &'static str {
    match reason {
        "length" => "MAX_TOKENS",
        "content_filter" => "SAFETY",
        _ => "STOP",
    }
}

fn message_to_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| part.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[derive(Default)]
struct GeminiSseTransformState {
    buffer: String,
}

impl GeminiSseTransformState {
    fn transform(&mut self, chunk: &[u8], fallback_model: &str) -> Vec<bytes::Bytes> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        let mut output = Vec::new();

        while let Some(pos) = self.buffer.find('\n') {
            let line = self.buffer[..pos].trim_end_matches('\r').to_string();
            self.buffer.drain(..=pos);

            if let Some(event) = self.transform_line(&line, fallback_model) {
                output.push(event);
            }
        }

        output
    }

    fn transform_line(&self, line: &str, fallback_model: &str) -> Option<bytes::Bytes> {
        let data = line.strip_prefix("data:")?.trim();
        if data.is_empty() || data == "[DONE]" {
            return None;
        }

        let parsed = serde_json::from_str::<Value>(data).ok()?;
        let choice = parsed.get("choices")?.as_array()?.first()?;
        let delta = choice.get("delta").cloned().unwrap_or_else(|| json!({}));
        let finish_reason = choice.get("finish_reason").and_then(Value::as_str);

        let mut parts = Vec::new();
        if let Some(reasoning) = delta
            .get("reasoning_content")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            parts.push(json!({ "text": reasoning, "thought": true }));
        }
        if let Some(content) = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            parts.push(json!({ "text": content }));
        }

        if parts.is_empty() && finish_reason.is_none() {
            return None;
        }

        let mut chunk = json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": if parts.is_empty() { vec![json!({ "text": "" })] } else { parts },
                },
                "index": 0,
            }]
        });

        if let Some(reason) = finish_reason {
            chunk["candidates"][0]["finishReason"] =
                Value::String(finish_reason_to_gemini(reason).to_string());
            if let Some(usage) = parsed.get("usage") {
                let mut usage_metadata = json!({
                    "promptTokenCount": usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
                    "candidatesTokenCount": usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0),
                    "totalTokenCount": usage.get("total_tokens").and_then(Value::as_u64).unwrap_or(0),
                });
                if let Some(reasoning_tokens) = usage
                    .get("completion_tokens_details")
                    .and_then(|details| details.get("reasoning_tokens"))
                    .and_then(Value::as_u64)
                {
                    usage_metadata["thoughtsTokenCount"] = Value::from(reasoning_tokens);
                }
                chunk["usageMetadata"] = usage_metadata;
            }

            chunk["modelVersion"] = Value::String(
                parsed
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or(fallback_model)
                    .to_string(),
            );
        }

        Some(bytes::Bytes::from(format!(
            "data: {}\r\n\r\n",
            serde_json::to_string(&chunk).ok()?
        )))
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiModelCard {
    name: String,
    display_name: String,
    description: String,
    supported_generation_methods: Vec<String>,
    input_token_limit: u32,
    output_token_limit: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_path_parsing_supports_provider_and_stream_action() {
        let (model, stream) = parse_gemini_model_path("custom/gpt-4o-mini:streamGenerateContent");
        assert_eq!(model, "custom/gpt-4o-mini");
        assert!(stream);

        let (model, stream) = parse_gemini_model_path("gpt-4o-mini:generateContent");
        assert_eq!(model, "gpt-4o-mini");
        assert!(!stream);
    }

    #[test]
    fn gemini_request_conversion_maps_system_and_contents() {
        let converted = convert_gemini_to_internal(
            &json!({
                "systemInstruction": {
                    "parts": [{"text": "Be terse"}]
                },
                "contents": [
                    { "role": "user", "parts": [{"text": "Ping"}] },
                    { "role": "model", "parts": [{"text": "Pong"}] }
                ],
                "generationConfig": {
                    "maxOutputTokens": 32,
                    "temperature": 0.5,
                    "topP": 0.9
                }
            }),
            "custom/gpt-4o-mini",
            true,
        );

        assert_eq!(converted["model"], "custom/gpt-4o-mini");
        assert_eq!(converted["stream"], true);
        assert_eq!(converted["messages"][0]["role"], "system");
        assert_eq!(converted["messages"][0]["content"], "Be terse");
        assert_eq!(converted["messages"][1]["role"], "user");
        assert_eq!(converted["messages"][1]["content"], "Ping");
        assert_eq!(converted["messages"][2]["role"], "assistant");
        assert_eq!(converted["messages"][2]["content"], "Pong");
        assert_eq!(converted["max_tokens"], 32);
    }

    #[test]
    fn gemini_sse_transformer_converts_openai_chunks() {
        let mut state = GeminiSseTransformState::default();
        let output = state.transform(
            br#"data: {"choices":[{"delta":{"content":"hello"},"finish_reason":null}]}
data: {"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}
data: [DONE]
"#,
            "custom/gpt-4o-mini",
        );

        assert_eq!(output.len(), 2);
        let first = String::from_utf8(output[0].to_vec()).unwrap();
        assert!(first.contains("\"candidates\""));
        assert!(first.contains("\"text\":\"hello\""));

        let second = String::from_utf8(output[1].to_vec()).unwrap();
        assert!(second.contains("\"finishReason\":\"STOP\""));
        assert!(second.contains("\"usageMetadata\""));
    }
}
