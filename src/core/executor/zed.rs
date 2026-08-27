//! ZedHosted executor — routes requests to Zed's hosted LLM aggregator
//! (`cloud.zed.dev/completions`), a multi-format proxy fronting
//! Anthropic/OpenAI-Responses/Google/xAI depending on the requested model.
//!
//! Port of 9router `open-sse/executors/zed.js` (EXEC-13). Wire protocol:
//! POST /completions with a thread envelope `{thread_id, prompt_id, provider,
//! model, provider_request}`; response is an NDJSON-ish per-line stream of
//! `{"event": <provider-shaped-chunk>}` / `{"status": …}` / `[DONE]`,
//! translated back to OpenAI Chat Completions chunks by the existing
//! streaming transformers. Auth is a short-lived LLM bearer exchanged from
//! the RSA-decrypted access token (see `crate::oauth::zed_auth`).

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use super::{TransportKind, UpstreamResponse};
use crate::core::translator::response_transform::{transform_sse_stream, transformer_for_provider};
use crate::oauth::zed_auth;
use crate::types::ProviderConnection;
use hyper::http;

const ZED_LLM_BASE_URL: &str = zed_auth::ZED_CLOUD_BASE_URL;

pub struct ZedExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<crate::core::proxy::ProxyTarget>,
}

pub struct ZedExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZedProvider {
    Anthropic,
    OpenAiResponses,
    Google,
    XAi,
}

/// JS normalizeZedProvider: explicit provider field first, then model-name
/// heuristics (claude→Anthropic, gemini→Google, grok/xai→XAi).
fn normalize_zed_provider(raw_provider: Option<&str>, model: &str) -> ZedProvider {
    if let Some(raw) = raw_provider {
        let lower = raw.to_ascii_lowercase().replace(['_', '-'], "");
        match lower.as_str() {
            "anthropic" => return ZedProvider::Anthropic,
            "openai" | "openairesponses" => return ZedProvider::OpenAiResponses,
            "google" => return ZedProvider::Google,
            "xai" => return ZedProvider::XAi,
            _ => {}
        }
    }
    let m = model.to_ascii_lowercase();
    if m.contains("claude") {
        ZedProvider::Anthropic
    } else if m.contains("gemini") {
        ZedProvider::Google
    } else if m.contains("grok") || m.contains("xai") {
        ZedProvider::XAi
    } else {
        ZedProvider::OpenAiResponses
    }
}

/// JS buildProviderRequest — translate the OpenAI chat body into the
/// provider-shaped request Zed's upstream expects.
fn build_provider_request(provider: ZedProvider, model: &str, body: &mut Value) -> Value {
    use crate::core::translator::request::openai_to_claude::openai_to_claude_request;
    use crate::core::translator::request::openai_to_gemini::openai_to_gemini_request;
    match provider {
        ZedProvider::Anthropic => {
            openai_to_claude_request(model, body, true, None);
            body.clone()
        }
        ZedProvider::Google => {
            openai_to_gemini_request(model, body, true, None);
            body.clone()
        }
        // OpenAi → OpenAI Responses shape; xAI is OpenAI-shaped — forward as-is.
        _ => {
            let mut out = body.clone();
            out["model"] = json!(model);
            out["stream"] = json!(true);
            out
        }
    }
}

/// Streaming transformer for the provider leg (JS convertProviderEvent /
/// initProviderState): anthropic + google reuse the existing SSE translators;
/// xAI/OpenAI pass through as already-OpenAI-shaped events.
fn transformer_for_zed(
    provider: ZedProvider,
) -> Box<dyn crate::core::translator::response_transform::StreamingTransformer> {
    match provider {
        ZedProvider::Anthropic => transformer_for_provider("claude").unwrap_or_else(|| {
            Box::new(crate::core::translator::response_transform::OpenAiTransformer::new())
        }),
        ZedProvider::Google => transformer_for_provider("gemini").unwrap_or_else(|| {
            Box::new(crate::core::translator::response_transform::OpenAiTransformer::new())
        }),
        ZedProvider::OpenAiResponses | ZedProvider::XAi => transformer_for_provider("openai")
            .unwrap_or_else(|| {
                Box::new(crate::core::translator::response_transform::OpenAiTransformer::new())
            }),
    }
}

fn error_chunk(model: &str, message: &str) -> String {
    format!(
        "data: {}\n\n",
        json!({
            "id": format!("chatcmpl-zed-error-{}", chrono::Utc::now().timestamp_millis()),
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {"content": format!("[Zed error] {message}")},
                "finish_reason": "stop",
            }],
        })
    )
}

#[derive(Clone)]
pub struct ZedExecutor {
    pool: std::sync::Arc<crate::core::executor::ClientPool>,
}

impl ZedExecutor {
    pub fn new(
        pool: std::sync::Arc<crate::core::executor::ClientPool>,
    ) -> Result<Self, std::convert::Infallible> {
        Ok(Self { pool })
    }

    /// Exchange the RSA-decrypted access token for a short-lived LLM bearer.
    /// Organization id comes from psd (`organizationId`); when missing we
    /// probe `/client/users/me` best-effort like the JS flow does.
    async fn resolve_llm_token(&self, credentials: &ProviderConnection) -> Result<String, String> {
        let psd = &credentials.provider_specific_data;
        let user_id = psd
            .get("userId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let access_token = credentials
            .access_token
            .as_deref()
            .or(credentials.api_key.as_deref())
            .unwrap_or_default()
            .to_string();
        let system_id = psd.get("systemId").and_then(Value::as_str);

        let mut organization_id = psd
            .get("organizationId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let client = reqwest::Client::new();
        if organization_id.is_empty() {
            let auth = zed_auth::build_user_auth_header(&user_id, &access_token)?;
            let mut request = client
                .get(format!("{ZED_LLM_BASE_URL}/client/users/me"))
                .header("Accept", "application/json")
                .header("Authorization", auth);
            if let Some(sid) = system_id.filter(|s| !s.is_empty()) {
                request = request.header("x-zed-system-id", sid);
            }
            if let Ok(info) = request.send().await {
                if let Ok(data) = info.json::<Value>().await {
                    organization_id = data
                        .pointer("/organization/id")
                        .or_else(|| data.get("organizationId"))
                        .or_else(|| data.get("organization_id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                }
            }
        }

        zed_auth::fetch_llm_token(
            &client,
            &user_id,
            &access_token,
            &organization_id,
            system_id,
        )
        .await
    }

    /// Convert one NDJSON line into OpenAI SSE frames via the provider
    /// transformer (JS processLine + unwrapZedLine).
    fn line_to_sse(
        line: &str,
        transformer: &mut dyn crate::core::translator::response_transform::StreamingTransformer,
    ) -> Vec<String> {
        let text = line.trim().trim_end_matches('\r');
        if text.is_empty() {
            return Vec::new();
        }
        let payload = text
            .strip_prefix("data:")
            .map(str::trim_start)
            .unwrap_or(text);
        if payload == "[DONE]" {
            return vec![String::new()]; // sentinel handled by caller
        }
        let Ok(parsed) = serde_json::from_str::<Value>(payload) else {
            return Vec::new(); // skip non-JSON banner lines (JS returns null)
        };

        // Status frames.
        if parsed.get("status").is_some() {
            let status = parsed.get("status").cloned().unwrap_or(Value::Null);
            let status_type = if status.is_string() {
                status.as_str().unwrap_or("").to_string()
            } else {
                status
                    .as_object()
                    .and_then(|o| o.keys().next().cloned())
                    .unwrap_or_default()
            };
            if status_type == "failed" {
                let failed = status.get("failed").cloned().unwrap_or(status.clone());
                let message = failed
                    .get("message")
                    .or_else(|| failed.get("error"))
                    .or_else(|| failed.get("code"))
                    .and_then(Value::as_str)
                    .unwrap_or("request failed")
                    .to_string();
                return vec![error_chunk("__MODEL__", &message), String::new()];
            }
            // stream_ended / other statuses → caller finishes the stream.
            return vec![String::new()];
        }

        // Provider event frame.
        let Some(event) = parsed.get("event") else {
            return Vec::new();
        };
        let sse_text = serde_json::to_vec(event).unwrap_or_default();
        let bytes = bytes::Bytes::from_owner(sse_text);
        transform_sse_stream(&bytes, transformer)
    }
}

impl ZedExecutor {
    async fn post_completions(
        &self,
        url: &str,
        token: &str,
        payload: &Value,
    ) -> Result<reqwest::Response, String> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|e| format!("invalid auth header: {e}"))?,
        );
        headers.insert(
            "x-zed-client-supports-status-messages",
            HeaderValue::from_static("1"),
        );
        headers.insert(
            "x-zed-client-supports-stream-ended-request-completion-status",
            HeaderValue::from_static("1"),
        );
        headers.insert("x-zed-client-supports-x-ai", HeaderValue::from_static("1"));
        let client = self
            .pool
            .get("zed", None)
            .map_err(|e| format!("client pool: {e}"))?;
        client
            .post(url)
            .headers(headers)
            .json(payload)
            .send()
            .await
            .map_err(|e| format!("Zed completions request failed: {e}"))
    }

    pub async fn execute_request(
        &self,
        request: ZedExecutionRequest,
    ) -> Result<ZedExecutorResponse, String> {
        let llm_token = self.resolve_llm_token(&request.credentials).await?;

        // Provider inference from the model name (catalog lookup is not yet
        // wired; JS falls back to the same heuristics when offline).
        let provider = normalize_zed_provider(None, &request.model);
        let mut provider_body = request.body.clone();
        let provider_request = build_provider_request(provider, &request.model, &mut provider_body);

        let thread_id = request
            .body
            .get("thread_id")
            .cloned()
            .or_else(|| {
                request
                    .credentials
                    .provider_specific_data
                    .get("threadId")
                    .cloned()
            })
            .unwrap_or(json!(uuid::Uuid::new_v4().to_string()));
        let prompt_id = request
            .body
            .get("prompt_id")
            .cloned()
            .unwrap_or(Value::Null);

        let payload = json!({
            "thread_id": thread_id,
            "prompt_id": prompt_id,
            "provider": match provider {
                ZedProvider::Anthropic => "Anthropic",
                ZedProvider::OpenAiResponses => "OpenAi",
                ZedProvider::Google => "Google",
                ZedProvider::XAi => "XAi",
            },
            "model": request.model,
            "provider_request": provider_request,
        });

        let url = format!("{ZED_LLM_BASE_URL}/completions");
        let response = self.post_completions(&url, &llm_token, &payload).await?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Zed returned HTTP {}: {text}", status.as_u16()));
        }

        // Drain the NDJSON line stream and translate each event to OpenAI SSE.
        let mut transformer = transformer_for_zed(provider);
        let mut sse = String::new();
        let model_name = request.model.clone();
        let mut buffer = String::new();
        let mut upstream = response.bytes_stream();
        'outer: while let Some(chunk) = upstream.next().await {
            let bytes = chunk.map_err(|e| format!("zed stream read failed: {e}"))?;
            buffer.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(nl) = buffer.find('\n') {
                let line: String = buffer.drain(..=nl).collect();
                for frame in Self::line_to_sse(&line, transformer.as_mut()) {
                    if frame.is_empty() {
                        break 'outer; // [DONE] or terminal status
                    }
                    sse.push_str(&frame.replace("__MODEL__", &model_name));
                }
            }
        }
        // Flush any trailing partial line.
        if !buffer.trim().is_empty() {
            for frame in Self::line_to_sse(&buffer, transformer.as_mut()) {
                if !frame.is_empty() {
                    sse.push_str(&frame.replace("__MODEL__", &model_name));
                }
            }
        }
        sse.push_str("data: [DONE]\n\n");

        let mut http_resp = http::Response::new(reqwest::Body::from(sse));
        *http_resp.status_mut() = reqwest::StatusCode::OK;
        http_resp.headers_mut().insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("text/event-stream"),
        );
        http_resp.headers_mut().insert(
            reqwest::header::CACHE_CONTROL,
            reqwest::header::HeaderValue::from_static("no-cache"),
        );

        Ok(ZedExecutorResponse {
            response: UpstreamResponse::Reqwest(reqwest::Response::from(http_resp)),
            url,
            headers: HeaderMap::new(),
            transformed_body: payload,
            transport: TransportKind::Reqwest,
        })
    }
}
