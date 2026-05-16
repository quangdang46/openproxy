//! Runway ML — async submit + /tasks/{id} polling.

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{json, Value};

use super::base::{
    now_secs, size_to_aspect_ratio, sleep, ImageAdapter, ImageRequest, ImageResponse,
    ParseContext, POLL_INTERVAL_MS, POLL_TIMEOUT_MS,
};

const BASE_URL: &str = "https://api.dev.runwayml.com/v1";

pub struct RunwaymlAdapter;
pub static ADAPTER: RunwaymlAdapter = RunwaymlAdapter;

#[async_trait]
impl ImageAdapter for RunwaymlAdapter {
    fn build_url(&self, request: &ImageRequest<'_>) -> Result<String, String> {
        let path = if request.model.contains("image") {
            "text_to_image"
        } else {
            "image_to_video"
        };
        Ok(format!("{BASE_URL}/{path}"))
    }

    fn build_headers(
        &self,
        request: &ImageRequest<'_>,
        _body: &Value,
    ) -> Result<HeaderMap, String> {
        let key = request
            .credentials
            .api_key
            .as_deref()
            .or(request.credentials.access_token.as_deref())
            .unwrap_or("");
        let mut h = HeaderMap::new();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|e| format!("auth header: {e}"))?,
        );
        h.insert("X-Runway-Version", HeaderValue::from_static("2024-11-06"));
        Ok(h)
    }

    async fn build_body(&self, request: &ImageRequest<'_>) -> Result<Value, String> {
        let prompt = request
            .prompt()
            .ok_or_else(|| "Missing required field: prompt".to_string())?;
        let is_video = !request.model.contains("image");
        let ratio = size_to_aspect_ratio(request.size().unwrap_or(""));
        if is_video {
            let mut req = json!({
                "promptText": prompt,
                "model": request.model,
                "ratio": ratio,
                "duration": 5,
            });
            if let Some(img) = request.image().and_then(|v| v.as_str()) {
                if let Some(obj) = req.as_object_mut() {
                    obj.insert("promptImage".into(), json!(img));
                }
            }
            Ok(req)
        } else {
            let mut req = json!({
                "promptText": prompt,
                "model": request.model,
                "ratio": ratio,
            });
            if let Some(img) = request.image().and_then(|v| v.as_str()) {
                if let Some(obj) = req.as_object_mut() {
                    obj.insert(
                        "referenceImages".into(),
                        json!([{"uri": img}]),
                    );
                }
            }
            Ok(req)
        }
    }

    async fn parse_response(
        &self,
        client: &Client,
        response: reqwest::Response,
        ctx: ParseContext<'_>,
    ) -> Result<ImageResponse, String> {
        let submit: Value = response
            .json()
            .await
            .map_err(|e| format!("parse runway submit: {e}"))?;
        let id = submit
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Runway: no task id".to_string())?
            .to_string();
        let task_url = format!("{BASE_URL}/tasks/{id}");

        let deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(POLL_TIMEOUT_MS);
        loop {
            if std::time::Instant::now() > deadline {
                return Err("Runway polling timeout".to_string());
            }
            sleep(POLL_INTERVAL_MS).await;
            let r = client
                .get(&task_url)
                .headers(ctx.headers.clone())
                .send()
                .await
                .map_err(|e| format!("runway poll: {e}"))?;
            if !r.status().is_success() {
                return Err(format!("Runway poll HTTP {}", r.status()));
            }
            let s: Value = r
                .json()
                .await
                .map_err(|e| format!("parse runway poll: {e}"))?;
            match s.get("status").and_then(|v| v.as_str()) {
                Some("SUCCEEDED") => return Ok(ImageResponse::Json(s)),
                Some("FAILED") | Some("CANCELLED") => {
                    let msg = s
                        .get("failure")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Runway task failed");
                    return Err(msg.to_string());
                }
                _ => {}
            }
        }
    }

    fn normalize(&self, body: &Value, _prompt: &str) -> Value {
        let outputs = body
            .get("output")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let data: Vec<Value> = outputs
            .iter()
            .filter_map(|v| v.as_str())
            .map(|url| json!({"url": url}))
            .collect();
        json!({"created": now_secs(), "data": data})
    }
}
