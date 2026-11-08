//! HuggingFace Inference Providers router — returns raw image bytes.
//!
//! Port of `open-sse/handlers/imageProviders/huggingface.js` (9router
//! cf663f5). The retired `api-inference.huggingface.co` host is replaced by
//! the router at `router.huggingface.co`, addressed as
//! `<baseUrl>/<provider>/<providerModelId>`. The client-facing model id is a
//! Hub id (e.g. `black-forest-labs/FLUX.1-schnell`); the provider-resolved id
//! the router expects is resolved through [`model_route`] — the Rust
//! equivalent of upstream `imageConfig.modelMap`.

use async_trait::async_trait;
use base64::Engine as _;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{json, Value};

use super::base::{now_secs, ImageAdapter, ImageRequest, ImageResponse, ParseContext};

pub struct HuggingfaceAdapter;
pub static ADAPTER: HuggingfaceAdapter = HuggingfaceAdapter;

const BASE_URL: &str = "https://router.huggingface.co";

/// Resolved router path for a Hub model id. `ImageToImage` entries also
/// change the request shape: the source image goes in `inputs` and the
/// prompt under `parameters.prompt` (upstream `imageConfig.modelMap`
/// `{ path, task: "image-to-image" }` values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelRoute {
    /// `{ path }` — plain text-to-image; body is `{ inputs: prompt }`.
    TextToImage(&'static str),
    /// `{ path, task: "image-to-image" }` — body is
    /// `{ inputs: <base64 image>, parameters: { prompt } }`.
    ImageToImage(&'static str),
}

/// `imageConfig.modelMap` from `open-sse/providers/registry/huggingface.js`.
/// Only providers the router actually forwards to are listed (replicate,
/// wavespeed and deepinfra reject router traffic upstream).
fn model_route(model: &str) -> Option<ModelRoute> {
    Some(match model {
        "black-forest-labs/FLUX.1-schnell" => ModelRoute::TextToImage("fal-ai/fal-ai/flux/schnell"),
        "black-forest-labs/FLUX.1-dev" => ModelRoute::TextToImage("fal-ai/fal-ai/flux/dev"),
        "black-forest-labs/FLUX.1-Krea-dev" => ModelRoute::TextToImage("fal-ai/fal-ai/flux/krea"),
        "black-forest-labs/FLUX.1-Kontext-dev" => {
            ModelRoute::ImageToImage("fal-ai/fal-ai/flux-kontext/dev")
        }
        "black-forest-labs/FLUX.2-dev" => ModelRoute::ImageToImage("fal-ai/fal-ai/flux-2/edit"),
        "black-forest-labs/FLUX.2-klein-9B" => {
            ModelRoute::ImageToImage("fal-ai/fal-ai/flux-2/klein/9b/edit")
        }
        "black-forest-labs/FLUX.2-klein-4B" => {
            ModelRoute::ImageToImage("fal-ai/fal-ai/flux-2/klein/4b/distilled/edit")
        }
        "black-forest-labs/FLUX.2-klein-base-9B" => {
            ModelRoute::ImageToImage("fal-ai/fal-ai/flux-2/klein/9b/base/edit")
        }
        "black-forest-labs/FLUX.2-klein-base-4B" => {
            ModelRoute::ImageToImage("fal-ai/fal-ai/flux-2/klein/4b/base/edit")
        }
        "stabilityai/stable-diffusion-xl-base-1.0" => {
            ModelRoute::TextToImage("fal-ai/fal-ai/fast-sdxl")
        }
        "stabilityai/stable-diffusion-3.5-large" => {
            ModelRoute::TextToImage("fal-ai/fal-ai/stable-diffusion-v35-large")
        }
        "stabilityai/stable-diffusion-3.5-large-turbo" => {
            ModelRoute::TextToImage("fal-ai/fal-ai/stable-diffusion-v35-large/turbo")
        }
        "Qwen/Qwen-Image" => ModelRoute::TextToImage("fal-ai/fal-ai/qwen-image"),
        "Qwen/Qwen-Image-2512" => ModelRoute::TextToImage("fal-ai/fal-ai/qwen-image-2512"),
        "Qwen/Qwen-Image-Edit" => ModelRoute::ImageToImage("fal-ai/fal-ai/qwen-image-edit"),
        "Qwen/Qwen-Image-Edit-2509" => {
            ModelRoute::ImageToImage("fal-ai/fal-ai/qwen-image-edit-2509")
        }
        "Qwen/Qwen-Image-Edit-2511" => {
            ModelRoute::ImageToImage("fal-ai/fal-ai/qwen-image-edit-plus")
        }
        "ideogram-ai/ideogram-4-fp8" => ModelRoute::TextToImage("fal-ai/ideogram/v4"),
        "tencent/HunyuanImage-3.0" => {
            ModelRoute::TextToImage("fal-ai/fal-ai/hunyuan-image/v3/text-to-image")
        }
        "Tongyi-MAI/Z-Image-Turbo" => ModelRoute::TextToImage("fal-ai/fal-ai/z-image/turbo"),
        "krea/Krea-2-Turbo" => ModelRoute::TextToImage("fal-ai/fal-ai/krea-2/turbo"),
        "HiDream-ai/HiDream-I1-Fast" => ModelRoute::TextToImage("fal-ai/fal-ai/hidream-i1-fast"),
        "playgroundai/playground-v2.5-1024px-aesthetic" => {
            ModelRoute::TextToImage("fal-ai/fal-ai/playground-v25")
        }
        _ => return None,
    })
}

fn route_path(route: ModelRoute) -> &'static str {
    match route {
        ModelRoute::TextToImage(p) | ModelRoute::ImageToImage(p) => p,
    }
}

/// A connection may point at its own endpoint (self-hosted Text Generation
/// Inference / TGI container). That endpoint already knows its own model ids,
/// so the router mapping does not apply and the Hub id passes through
/// verbatim. Mirrors upstream `customBaseUrl(creds)`.
fn custom_base_url(credentials: &crate::types::ProviderConnection) -> Option<String> {
    credentials
        .provider_specific_data
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('/').to_string())
}

/// Extract the source image for image-to-image requests. Accepts `image`
/// (string) or `images[0]` (array), as a data URL, bare base64, or remote
/// http(s) URL — mirroring upstream `sourceImage(body)`. Remote URLs are
/// fetched through the SSRF-hardened image helper (upstream uses a plain
/// `fetch`; we keep the stricter local contract).
async fn source_image(request: &ImageRequest<'_>) -> Result<String, String> {
    // Upstream `body?.image || images[0]`: a truthy non-string `image` wins
    // and then fails the string check (→ "requires a source image"), it does
    // not fall back to `images`.
    let image_field = request.body.get("image");
    let image_truthy = image_field
        .map(|v| match v.as_str() {
            Some(s) => !s.trim().is_empty(),
            None => !v.is_null(),
        })
        .unwrap_or(false);
    let raw_val = if image_truthy {
        image_field
    } else {
        request
            .body
            .get("images")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
    };
    let raw = raw_val
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            format!(
                "HuggingFace: model \"{}\" requires a source image. \
                 Send it as \"image\" (or \"images\") in the request body.",
                request.model
            )
        })?;

    if raw.starts_with("http://") || raw.starts_with("https://") {
        // `fetch_image_as_base64` builds its own redirect-safe client; the
        // passed client is only a fallback, so a throwaway is fine here.
        let fetched = crate::core::translator::helpers::image_helper::fetch_image_as_base64(
            &Client::new(),
            raw,
        )
        .await
        .ok_or_else(|| format!("HuggingFace: failed to fetch source image {raw}"))?;
        // Strip the `data:<mime>;base64,` prefix — the router wants raw base64.
        return fetched
            .data_url
            .split_once(",")
            .map(|(_, b64)| b64.to_string())
            .ok_or_else(|| "HuggingFace: malformed fetched image data".to_string());
    }

    // data URL → keep the base64 payload; anything else is treated as bare
    // base64. Mirrors upstream regex ^data:image\/[^;]+;base64,(.+)$ — the
    // mime segment must not itself contain ';'.
    if let Some(idx) = raw.find(";base64,") {
        let prefix = &raw[..idx];
        if let Some(mime) = prefix.strip_prefix("data:image/") {
            if !mime.is_empty() && !mime.contains(';') {
                return Ok(raw[idx + ";base64,".len()..].to_string());
            }
        }
    }
    Ok(raw.to_string())
}

#[async_trait]
impl ImageAdapter for HuggingfaceAdapter {
    fn build_url(&self, request: &ImageRequest<'_>) -> Result<String, String> {
        // Custom endpoint: the model id lands in a URL path verbatim, so
        // reject traversal/query injection (mirrors upstream + sttCore).
        if let Some(base) = custom_base_url(request.credentials) {
            let model = request.model;
            if model.contains("..")
                || model.contains("//")
                || model.contains('?')
                || model.contains('#')
            {
                return Err(format!("HuggingFace: invalid model ID \"{model}\""));
            }
            return Ok(format!("{base}/{model}"));
        }

        let route = model_route(request.model).ok_or_else(|| {
            format!(
                "HuggingFace: no HuggingFace router mapping for model \"{}\". \
                 Add it to imageConfig.modelMap in open-sse/providers/registry/huggingface.js, \
                 or set a custom base URL on the connection.",
                request.model
            )
        })?;
        Ok(format!("{BASE_URL}/{}", route_path(route)))
    }

    fn build_headers(
        &self,
        request: &ImageRequest<'_>,
        _body: &Value,
    ) -> Result<HeaderMap, String> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let key = request
            .credentials
            .api_key
            .as_deref()
            .or(request.credentials.access_token.as_deref())
            .unwrap_or("");
        if !key.is_empty() {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|e| format!("auth header: {e}"))?,
            );
        }
        Ok(headers)
    }

    async fn build_body(&self, request: &ImageRequest<'_>) -> Result<Value, String> {
        // Upstream `buildBody` resolves the task from the modelMap alone —
        // it has no access to credentials, so a custom baseUrl endpoint with
        // a mapped image-to-image model still gets the i2i payload shape.
        let task_is_i2i = matches!(
            model_route(request.model),
            Some(ModelRoute::ImageToImage(_))
        );

        if task_is_i2i {
            let image = source_image(request).await?;
            // inputs carries the source image; the prompt moves under
            // parameters (JSON.stringify drops undefined → omit when absent).
            let mut parameters = serde_json::Map::new();
            if let Some(prompt) = request.prompt() {
                parameters.insert("prompt".into(), json!(prompt));
            }
            return Ok(json!({"inputs": image, "parameters": Value::Object(parameters)}));
        }

        let prompt = request
            .prompt()
            .ok_or_else(|| "Missing required field: prompt".to_string())?;
        Ok(json!({"inputs": prompt}))
    }

    async fn parse_response(
        &self,
        _client: &Client,
        response: reqwest::Response,
        _ctx: ParseContext<'_>,
    ) -> Result<ImageResponse, String> {
        let bytes = response
            .bytes()
            .await
            .map_err(|e| format!("read hf bytes: {e}"))?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        Ok(ImageResponse::Json(json!({
            "created": now_secs(),
            "data": [{"b64_json": b64}],
        })))
    }

    fn normalize(&self, body: &Value, _prompt: &str) -> Value {
        body.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProviderConnection;
    use std::collections::BTreeMap;

    fn conn(psd: BTreeMap<String, Value>) -> ProviderConnection {
        ProviderConnection {
            id: "hf-1".into(),
            provider: "huggingface".into(),
            auth_type: "apikey".into(),
            provider_specific_data: psd,
            ..Default::default()
        }
    }

    fn req<'a>(
        model: &'a str,
        body: &'a Value,
        credentials: &'a ProviderConnection,
    ) -> ImageRequest<'a> {
        ImageRequest {
            body,
            model,
            credentials,
        }
    }

    #[test]
    fn router_url_resolves_model_map_path() {
        let c = conn(BTreeMap::new());
        let body = json!({"prompt": "a cat"});
        let r = req("black-forest-labs/FLUX.1-schnell", &body, &c);
        assert_eq!(
            ADAPTER.build_url(&r).unwrap(),
            "https://router.huggingface.co/fal-ai/fal-ai/flux/schnell"
        );
    }

    #[test]
    fn image_to_image_model_resolves_edit_path() {
        let c = conn(BTreeMap::new());
        let body = json!({"prompt": "make it blue"});
        let r = req("Qwen/Qwen-Image-Edit", &body, &c);
        assert_eq!(
            ADAPTER.build_url(&r).unwrap(),
            "https://router.huggingface.co/fal-ai/fal-ai/qwen-image-edit"
        );
    }

    #[test]
    fn unmapped_model_errors_without_custom_base_url() {
        let c = conn(BTreeMap::new());
        let body = json!({"prompt": "x"});
        let r = req("unknown/model", &body, &c);
        let err = ADAPTER.build_url(&r).unwrap_err();
        assert!(err.contains("no HuggingFace router mapping"), "{err}");
    }

    #[test]
    fn custom_base_url_passes_model_verbatim() {
        let mut psd = BTreeMap::new();
        psd.insert("baseUrl".into(), json!("http://tgi.local:8080/v1/"));
        let c = conn(psd);
        let body = json!({"prompt": "x"});
        // Unmapped model is fine on a custom endpoint.
        let r = req("my-org/my-model", &body, &c);
        assert_eq!(
            ADAPTER.build_url(&r).unwrap(),
            "http://tgi.local:8080/v1/my-org/my-model"
        );
    }

    #[test]
    fn custom_base_url_rejects_traversal() {
        let mut psd = BTreeMap::new();
        psd.insert("baseUrl".into(), json!("http://tgi.local:8080"));
        let c = conn(psd);
        let body = json!({"prompt": "x"});
        for bad in ["a/../b", "a//b", "a?b", "a#b"] {
            let r = req(bad, &body, &c);
            assert!(ADAPTER.build_url(&r).is_err(), "{bad} should fail");
        }
    }

    #[tokio::test]
    async fn text_to_image_body_is_inputs_prompt() {
        let c = conn(BTreeMap::new());
        let body = json!({"prompt": "a cat"});
        let r = req("black-forest-labs/FLUX.1-schnell", &body, &c);
        assert_eq!(
            ADAPTER.build_body(&r).await.unwrap(),
            json!({"inputs": "a cat"})
        );
    }

    #[tokio::test]
    async fn image_to_image_body_carries_image_and_parameters_prompt() {
        let c = conn(BTreeMap::new());
        let body = json!({
            "prompt": "make it blue",
            "image": "data:image/png;base64,aGVsbG8="
        });
        let r = req("Qwen/Qwen-Image-Edit", &body, &c);
        assert_eq!(
            ADAPTER.build_body(&r).await.unwrap(),
            json!({"inputs": "aGVsbG8=", "parameters": {"prompt": "make it blue"}})
        );
    }

    #[tokio::test]
    async fn image_to_image_accepts_images_array_and_bare_base64() {
        let c = conn(BTreeMap::new());
        let body = json!({"prompt": "p", "images": ["QUJD"]});
        let r = req("black-forest-labs/FLUX.2-dev", &body, &c);
        assert_eq!(
            ADAPTER.build_body(&r).await.unwrap(),
            json!({"inputs": "QUJD", "parameters": {"prompt": "p"}})
        );
    }

    #[tokio::test]
    async fn image_to_image_requires_source_image() {
        let c = conn(BTreeMap::new());
        let body = json!({"prompt": "p"});
        let r = req("Qwen/Qwen-Image-Edit", &body, &c);
        let err = ADAPTER.build_body(&r).await.unwrap_err();
        assert!(err.contains("requires a source image"), "{err}");
    }

    #[tokio::test]
    async fn custom_base_url_still_uses_mapped_image_to_image_shape() {
        let mut psd = BTreeMap::new();
        psd.insert("baseUrl".into(), json!("http://tgi.local:8080"));
        let c = conn(psd);
        // Upstream buildBody keys the task off modelMap only (no creds), so a
        // mapped i2i model keeps the i2i payload even on a custom endpoint.
        let body = json!({"prompt": "p", "image": "QUJD"});
        let r = req("Qwen/Qwen-Image-Edit", &body, &c);
        assert_eq!(
            ADAPTER.build_body(&r).await.unwrap(),
            json!({"inputs": "QUJD", "parameters": {"prompt": "p"}})
        );
        // ...while an unmapped model on the same endpoint stays t2i.
        let body2 = json!({"prompt": "p"});
        let r2 = req("my-org/my-model", &body2, &c);
        assert_eq!(
            ADAPTER.build_body(&r2).await.unwrap(),
            json!({"inputs": "p"})
        );
    }
}
