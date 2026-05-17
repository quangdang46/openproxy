//! Fal.ai — async submit + queue polling.

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{json, Value};

use super::base::{
    now_secs, size_to_aspect_ratio, sleep, ImageAdapter, ImageRequest, ImageResponse, ParseContext,
    POLL_INTERVAL_MS, POLL_TIMEOUT_MS,
};

const BASE_URL: &str = "https://queue.fal.run";

pub struct FalAiAdapter;
pub static ADAPTER: FalAiAdapter = FalAiAdapter;

#[async_trait]
impl ImageAdapter for FalAiAdapter {
    fn build_url(&self, request: &ImageRequest<'_>) -> Result<String, String> {
        Ok(format!("{BASE_URL}/{}", request.model))
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
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Key {key}"))
                .map_err(|e| format!("auth header: {e}"))?,
        );
        Ok(headers)
    }

    async fn build_body(&self, request: &ImageRequest<'_>) -> Result<Value, String> {
        let prompt = request
            .prompt()
            .ok_or_else(|| "Missing required field: prompt".to_string())?;
        let mut req = json!({"prompt": prompt, "num_images": request.n()});
        if let Some(size) = request.size() {
            if let Some(obj) = req.as_object_mut() {
                obj.insert("image_size".into(), json!(size_to_aspect_ratio(size)));
            }
        }
        if let Some(image) = request.image().and_then(|v| v.as_str()) {
            if let Some(obj) = req.as_object_mut() {
                obj.insert("image_url".into(), json!(image));
            }
        }
        Ok(req)
    }

    async fn parse_response(
        &self,
        client: &Client,
        response: reqwest::Response,
        ctx: ParseContext<'_>,
    ) -> Result<ImageResponse, String> {
        let body: Value = response
            .json()
            .await
            .map_err(|e| format!("parse fal submit: {e}"))?;
        let status_url = body
            .get("status_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Fal: no status_url".to_string())?
            .to_string();
        let response_url = body
            .get("response_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Fal: no response_url".to_string())?
            .to_string();

        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(POLL_TIMEOUT_MS);
        loop {
            if std::time::Instant::now() > deadline {
                return Err("Fal polling timeout".to_string());
            }
            sleep(POLL_INTERVAL_MS).await;
            let r = client
                .get(&status_url)
                .headers(ctx.headers.clone())
                .send()
                .await
                .map_err(|e| format!("fal status: {e}"))?;
            if !r.status().is_success() {
                return Err(format!("Fal status HTTP {}", r.status()));
            }
            let s: Value = r
                .json()
                .await
                .map_err(|e| format!("parse fal status: {e}"))?;
            match s.get("status").and_then(|v| v.as_str()) {
                Some("COMPLETED") => {
                    let fr = client
                        .get(&response_url)
                        .headers(ctx.headers.clone())
                        .send()
                        .await
                        .map_err(|e| format!("fal response: {e}"))?;
                    let parsed: Value = fr
                        .json()
                        .await
                        .map_err(|e| format!("parse fal final: {e}"))?;
                    return Ok(ImageResponse::Json(parsed));
                }
                Some("FAILED") => {
                    let msg = s
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Fal generation failed");
                    return Err(msg.to_string());
                }
                _ => {}
            }
        }
    }

    fn normalize(&self, body: &Value, _prompt: &str) -> Value {
        let images = if let Some(arr) = body.get("images").and_then(|v| v.as_array()) {
            arr.clone()
        } else if let Some(img) = body.get("image") {
            vec![img.clone()]
        } else {
            Vec::new()
        };
        let data: Vec<Value> = images
            .iter()
            .map(|v| {
                let url = v
                    .get("url")
                    .and_then(|x| x.as_str())
                    .or_else(|| v.as_str())
                    .unwrap_or("");
                json!({"url": url})
            })
            .collect();
        json!({"created": now_secs(), "data": data})
    }
}
