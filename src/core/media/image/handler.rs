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
use std::time::Duration;
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

    // 9router parity (imageGenerationCore.js:121-155): on 401/403, refresh the
    // OAuth credentials (once) and re-fire the request with the rebuilt
    // body/headers/url. Only applies to non-noAuth adapters whose connection
    // has a refresh token. The retried request reuses the rebuilt headers.
    let mut request_body = inputs
        .adapter
        .build_body(&inputs.request)
        .await
        .map_err(ImageHandlerError::Validation)?;
    let mut url = inputs
        .adapter
        .build_url(&inputs.request)
        .map_err(ImageHandlerError::Validation)?;
    let mut headers = inputs
        .adapter
        .build_headers(&inputs.request, &request_body)
        .map_err(ImageHandlerError::Validation)?;

    let mut response = inputs
        .client
        .post(&url)
        .headers(headers.clone())
        .json(&request_body)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| ImageHandlerError::Upstream(e.to_string()))?;

    // One-shot refresh retry on 401/403 (never retry twice, never for
    // no_auth adapters like sdwebui/comfyui, never on other statuses).
    let status = response.status().as_u16();
    if (status == 401 || status == 403)
        && !inputs.adapter.no_auth()
        && inputs
            .request
            .credentials
            .refresh_token
            .as_deref()
            .is_some_and(|r| !r.is_empty())
    {
        let refresh_token = inputs
            .request
            .credentials
            .refresh_token
            .as_deref()
            .unwrap_or("")
            .to_string();
        let provider_specific_data = inputs.request.credentials.provider_specific_data.clone();
        let provider = inputs.request.credentials.provider.clone();
        // Drop the first response (its body is the 401/403 error we won't use).
        drop(response);
        if let Ok(refresh) = crate::oauth::token_refresh::dispatch_oauth_refresh(
            &provider,
            &refresh_token,
            &provider_specific_data,
        )
        .await
        {
            let mut refreshed = inputs.request.credentials.clone();
            refreshed.access_token = Some(refresh.access_token.clone());
            if let Some(rt) = refresh.refresh_token {
                refreshed.refresh_token = Some(rt);
            }
            // `expires_at` is an ISO timestamp string; `refresh.expires_in` is
            // seconds. The access token is what matters for the retry, so we
            // leave expires_at as-is.
            // Rebuild body/url/headers against the refreshed credentials and
            // re-fire exactly once.
            let retry_req = ImageRequest {
                body: inputs.request.body,
                model: inputs.request.model,
                credentials: &refreshed,
            };
            let rebuilt = (
                inputs.adapter.build_body(&retry_req).await,
                inputs.adapter.build_url(&retry_req),
                inputs.adapter.build_headers(&retry_req, &request_body),
            );
            if let (Ok(new_body), Ok(new_url), Ok(new_headers)) = rebuilt {
                request_body = new_body;
                url = new_url;
                headers = new_headers;
            }
        }
        // Re-fire once with the (possibly rebuilt) request. If the refresh
        // failed, this re-sends the original body/headers — matching the JS
        // behaviour of returning the original 401/403 on refresh failure, but
        // we still go through the retry so parse/error handling stays uniform.
        response = inputs
            .client
            .post(&url)
            .headers(headers.clone())
            .json(&request_body)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| ImageHandlerError::Upstream(e.to_string()))?;
    }

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

    /// Test adapter whose URL points at a wiremock server, so the handler's
    /// retry (which re-fires the request) can be observed.
    struct TestAdapter {
        base_url: String,
    }

    #[async_trait::async_trait]
    impl ImageAdapter for TestAdapter {
        fn build_url(&self, _request: &ImageRequest<'_>) -> Result<String, String> {
            Ok(format!("{}/images/generations", self.base_url))
        }
        fn build_headers(
            &self,
            request: &ImageRequest<'_>,
            _body: &Value,
        ) -> Result<reqwest::header::HeaderMap, String> {
            let mut h = reqwest::header::HeaderMap::new();
            let token = request
                .credentials
                .api_key
                .as_deref()
                .or(request.credentials.access_token.as_deref())
                .unwrap_or("");
            h.insert(
                reqwest::header::AUTHORIZATION,
                reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|e| e.to_string())?,
            );
            Ok(h)
        }
        async fn build_body(&self, _request: &ImageRequest<'_>) -> Result<Value, String> {
            Ok(json!({ "prompt": "test", "model": "test-model" }))
        }
    }

    #[tokio::test]
    async fn image_handler_retries_once_after_refresh() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // The handler must fire the upstream request exactly once when there
        // is no refresh_token, and exactly twice (401 → refresh → re-fire)
        // when a refresh_token is present — even though the refresh itself
        // fails offline, the JS path still re-fires once and returns the 401.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/images/generations"))
            .and(header("Authorization", "Bearer sk-expired"))
            .respond_with(ResponseTemplate::new(401))
            .up_to_n_times(20)
            .mount(&server)
            .await;

        // The handler requires a `&'static` adapter; leak a boxed instance.
        let adapter: &'static TestAdapter = Box::leak(Box::new(TestAdapter {
            base_url: server.uri(),
        }));
        let client = reqwest::Client::new();

        // Case 1: no refresh_token → 1 request (no retry).
        let mut creds = ProviderConnection::default();
        creds.provider = "openai".into();
        creds.api_key = Some("sk-expired".into());
        let body = json!({ "prompt": "test", "model": "test-model" });
        let req = IR {
            body: &body,
            model: "test-model",
            credentials: &creds,
        };
        let inputs = ImageHandlerInputs {
            client: &client,
            adapter: adapter,
            request: req,
            binary_output: false,
            stream_to_client: false,
        };
        let _ = handle_image_generation(inputs).await;
        let after_case1 = server.received_requests().await.unwrap().len();
        assert_eq!(after_case1, 1, "no refresh creds → 1 request");

        // Case 2: has refresh_token → handler attempts refresh. Using an
        // unknown provider makes dispatch_oauth_refresh fail immediately
        // (no network), and per JS the handler still re-fires once → this case
        // alone makes 2 requests.
        let mut creds2 = ProviderConnection::default();
        creds2.provider = "no-such-provider".into();
        creds2.api_key = Some("sk-expired".into());
        creds2.refresh_token = Some("rt-123".into());
        let body2 = json!({ "prompt": "test", "model": "test-model" });
        let req2 = IR {
            body: &body2,
            model: "test-model",
            credentials: &creds2,
        };
        let inputs2 = ImageHandlerInputs {
            client: &client,
            adapter: adapter,
            request: req2,
            binary_output: false,
            stream_to_client: false,
        };
        let _ = handle_image_generation(inputs2).await;
        let after_case2 = server.received_requests().await.unwrap().len();

        assert_eq!(
            after_case2 - after_case1,
            2,
            "refresh creds present → handler re-fires once (2 requests in this case)"
        );
    }
}
