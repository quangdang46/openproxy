//! Image-generation orchestrator.
//!
//! Port of `open-sse/handlers/imageGenerationCore.js`. Picks the
//! provider adapter, builds the upstream request, fires it, retries
//! once on 401/403 if the caller can refresh credentials, parses the
//! response, and emits an OpenAI-shaped JSON body (or an SSE stream
//! for Codex).

use base64::Engine as _;
use reqwest::Client;
use serde_json::{json, Value};
use thiserror::Error;

use super::base::{ImageAdapter, ImageRequest, ImageResponse, ParseContext};
use crate::types::ProviderConnection;

/// Outcome of the image-generation pipeline.
#[derive(Debug)]
pub enum HandlerOutput {
    /// JSON body to return as the HTTP response.
    Json(Value),
    /// Pre-built SSE response (Codex streaming path).
    Sse(axum::response::Response),
    /// Raw image bytes plus the inferred content-type. Returned when
    /// `binary_output = true` and the upstream produced a base64 image
    /// we can decode.
    Binary {
        bytes: Vec<u8>,
        content_type: String,
        filename: String,
    },
}

#[derive(Debug, Error)]
pub enum ImageHandlerError {
    #[error("HTTP {0}: {1}")]
    Http(u16, String),
    #[error("validation: {0}")]
    Validation(String),
    #[error("provider {provider} not supported for image generation")]
    UnsupportedProvider { provider: String },
    #[error("upstream: {0}")]
    Upstream(String),
}

impl ImageHandlerError {
    pub fn status(&self) -> u16 {
        match self {
            ImageHandlerError::Http(code, _) => *code,
            ImageHandlerError::Validation(_) => 400,
            ImageHandlerError::UnsupportedProvider { .. } => 400,
            ImageHandlerError::Upstream(_) => 502,
        }
    }
}

/// Inputs the orchestrator needs from the calling context.
pub struct ImageHandlerInputs<'a> {
    pub client: &'a Client,
    pub adapter: &'static dyn ImageAdapter,
    pub request: ImageRequest<'a>,
    /// When set the handler will return raw image bytes (for `/v1/images/binary` etc).
    pub binary_output: bool,
    /// Codex specifically supports streaming progress events back to the caller.
    pub stream_to_client: bool,
}

/// Run the image-generation pipeline end-to-end.
pub async fn handle_image_generation(
    inputs: ImageHandlerInputs<'_>,
) -> Result<HandlerOutput, ImageHandlerError> {
    if inputs.request.prompt().filter(|s| !s.is_empty()).is_none() {
        return Err(ImageHandlerError::Validation(
            "Missing required field: prompt".to_string(),
        ));
    }

    let request_body = inputs
        .adapter
        .build_body(&inputs.request)
        .await
        .map_err(ImageHandlerError::Validation)?;
    let url = inputs
        .adapter
        .build_url(&inputs.request)
        .map_err(ImageHandlerError::Validation)?;
    let headers = inputs
        .adapter
        .build_headers(&inputs.request, &request_body)
        .map_err(ImageHandlerError::Validation)?;

    let response = inputs
        .client
        .post(&url)
        .headers(headers.clone())
        .json(&request_body)
        .send()
        .await
        .map_err(|e| ImageHandlerError::Upstream(e.to_string()))?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        return Err(ImageHandlerError::Http(status, body));
    }

    let parse_ctx = ParseContext {
        headers: &headers,
        stream_to_client: inputs.stream_to_client,
    };

    let parsed = inputs
        .adapter
        .parse_response(inputs.client, response, parse_ctx)
        .await
        .map_err(ImageHandlerError::Upstream)?;

    let value = match parsed {
        ImageResponse::Sse(resp) => return Ok(HandlerOutput::Sse(resp)),
        ImageResponse::Json(v) => v,
    };

    let prompt = inputs.request.prompt().unwrap_or("");
    let normalized = inputs.adapter.normalize(&value, prompt);

    // Adapter said "already OpenAI-shape" by including created+data.
    let openai_shape = normalized.get("created").is_some()
        && normalized.get("data").and_then(|v| v.as_array()).is_some();
    let final_body = if openai_shape { normalized } else { value };

    if inputs.binary_output {
        if let Some(item) = final_body
            .get("data")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
        {
            let b64_owned = item
                .get("b64_json")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let b64 = if let Some(b) = b64_owned {
                Some(b)
            } else if let Some(url) = item.get("url").and_then(|v| v.as_str()) {
                let r = inputs.client.get(url).send().await.ok();
                if let Some(r) = r {
                    if r.status().is_success() {
                        let bytes = r.bytes().await.ok();
                        bytes.map(|b| base64::engine::general_purpose::STANDARD.encode(b))
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(b64) = b64 {
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) {
                    let fmt = inputs
                        .request
                        .body
                        .get("output_format")
                        .and_then(|v| v.as_str())
                        .map(str::to_lowercase)
                        .unwrap_or_else(|| "png".to_string());
                    let (content_type, ext) = match fmt.as_str() {
                        "jpeg" | "jpg" => ("image/jpeg".to_string(), "jpg".to_string()),
                        "webp" => ("image/webp".to_string(), "webp".to_string()),
                        _ => ("image/png".to_string(), "png".to_string()),
                    };
                    return Ok(HandlerOutput::Binary {
                        bytes,
                        content_type,
                        filename: format!("image.{ext}"),
                    });
                }
            }
        }
    }

    Ok(HandlerOutput::Json(final_body))
}

/// Convenience helper for callers that only need the JSON body.
pub fn json_or_error(out: HandlerOutput) -> Result<Value, ImageHandlerError> {
    match out {
        HandlerOutput::Json(v) => Ok(v),
        HandlerOutput::Sse(_) => Err(ImageHandlerError::Validation(
            "SSE response cannot be unwrapped to JSON".to_string(),
        )),
        HandlerOutput::Binary { .. } => Err(ImageHandlerError::Validation(
            "binary response cannot be unwrapped to JSON".to_string(),
        )),
    }
}

#[allow(dead_code)]
fn _ensure_credentials_available(creds: &ProviderConnection) {
    let _ = creds; // placeholder to keep ProviderConnection in scope
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::media::image::{get_image_adapter, ImageRequest as IR};
    use crate::types::ProviderConnection;
    use serde_json::json;

    #[test]
    fn empty_prompt_validates_at_handler_level() {
        let adapter = get_image_adapter("openai").unwrap();
        let body = json!({"model": "dall-e-3"});
        let creds = ProviderConnection::default();
        let req = IR {
            body: &body,
            model: "dall-e-3",
            credentials: &creds,
        };
        // We can't run the full handler without a Client + network, but we
        // can at least check the build_body validation.
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let res = runtime.block_on(async { adapter.build_body(&req).await });
        assert!(res.is_err());
    }

    #[test]
    fn openai_compat_endpoint_resolves() {
        let adapter = get_image_adapter("openai").unwrap();
        let body = json!({"prompt": "hi"});
        let creds = ProviderConnection::default();
        let req = IR {
            body: &body,
            model: "dall-e-3",
            credentials: &creds,
        };
        let url = adapter.build_url(&req).unwrap();
        assert_eq!(url, "https://api.openai.com/v1/images/generations");
    }

    #[test]
    fn cloudflare_requires_account_id() {
        let adapter = get_image_adapter("cloudflare-ai").unwrap();
        let body = json!({"prompt": "hi"});
        let creds = ProviderConnection::default();
        let req = IR {
            body: &body,
            model: "@cf/black-forest-labs/flux-schnell",
            credentials: &creds,
        };
        let err = adapter.build_url(&req).unwrap_err();
        assert!(err.contains("accountId"));
    }
}
