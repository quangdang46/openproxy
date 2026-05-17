//! NanoBanana — async submit + record-info polling.

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{json, Value};

use super::base::{
    empty_normalized, now_secs, size_to_aspect_ratio, sleep, ImageAdapter, ImageRequest,
    ImageResponse, ParseContext, POLL_INTERVAL_MS, POLL_TIMEOUT_MS,
};

const SUBMIT_URL: &str = "https://api.nanobananaapi.ai/api/v1/nanobanana/generate";
const POLL_BASE: &str = "https://api.nanobananaapi.ai/api/v1/nanobanana/record-info";

pub struct NanobananaAdapter;
pub static ADAPTER: NanobananaAdapter = NanobananaAdapter;

#[async_trait]
impl ImageAdapter for NanobananaAdapter {
    fn build_url(&self, _: &ImageRequest<'_>) -> Result<String, String> {
        Ok(SUBMIT_URL.to_string())
    }

    fn build_headers(
        &self,
        request: &ImageRequest<'_>,
        _body: &Value,
    ) -> Result<HeaderMap, String> {
        let mut h = HeaderMap::new();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let key = request
            .credentials
            .api_key
            .as_deref()
            .or(request.credentials.access_token.as_deref())
            .unwrap_or("");
        if !key.is_empty() {
            h.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|e| format!("auth header: {e}"))?,
            );
        }
        Ok(h)
    }

    async fn build_body(&self, request: &ImageRequest<'_>) -> Result<Value, String> {
        let prompt = request
            .prompt()
            .ok_or_else(|| "Missing required field: prompt".to_string())?;
        let ratio = size_to_aspect_ratio(request.size().unwrap_or(""));
        let images_arr = request
            .body
            .get("images")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let single = request.image().and_then(|v| v.as_str()).map(str::to_string);
        let is_edit = !images_arr.is_empty() || single.is_some();

        let mut req = json!({
            "prompt": prompt,
            "type": if is_edit { "IMAGETOIAMGE" } else { "TEXTTOIAMGE" },
            "numImages": request.n(),
            "image_size": ratio,
            "callBackUrl": "https://localhost/callback",
        });
        if is_edit {
            let mut urls: Vec<Value> = images_arr
                .iter()
                .filter_map(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| json!(s))
                .collect();
            if let Some(s) = single {
                urls.push(json!(s));
            }
            if let Some(obj) = req.as_object_mut() {
                obj.insert("imageUrls".into(), Value::Array(urls));
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
        let submit: Value = response
            .json()
            .await
            .map_err(|e| format!("parse nanobanana submit: {e}"))?;
        if submit.get("code").and_then(|v| v.as_u64()) != Some(200) {
            let msg = submit
                .get("msg")
                .and_then(|v| v.as_str())
                .unwrap_or("NanoBanana submit failed");
            return Err(msg.to_string());
        }
        let task_id = submit
            .pointer("/data/taskId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "NanoBanana: no taskId".to_string())?
            .to_string();
        let poll_url = format!("{POLL_BASE}?taskId={}", urlencoding::encode(&task_id));

        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(POLL_TIMEOUT_MS);
        loop {
            if std::time::Instant::now() > deadline {
                return Err("NanoBanana polling timeout".to_string());
            }
            sleep(POLL_INTERVAL_MS).await;
            let r = client
                .get(&poll_url)
                .headers(ctx.headers.clone())
                .send()
                .await
                .map_err(|e| format!("nanobanana poll: {e}"))?;
            if !r.status().is_success() {
                return Err(format!("NanoBanana poll HTTP {}", r.status()));
            }
            let s: Value = r
                .json()
                .await
                .map_err(|e| format!("parse nanobanana poll: {e}"))?;
            let flag = s.pointer("/data/successFlag").and_then(|v| v.as_u64());
            match flag {
                Some(1) => {
                    return Ok(ImageResponse::Json(
                        s.get("data").cloned().unwrap_or(Value::Null),
                    ))
                }
                Some(2) | Some(3) => {
                    let msg = s
                        .pointer("/data/errorMessage")
                        .and_then(|v| v.as_str())
                        .unwrap_or("NanoBanana generation failed");
                    return Err(msg.to_string());
                }
                _ => {}
            }
        }
    }

    fn normalize(&self, body: &Value, prompt: &str) -> Value {
        let url = body
            .pointer("/response/resultImageUrl")
            .and_then(|v| v.as_str())
            .or_else(|| {
                body.pointer("/response/originImageUrl")
                    .and_then(|v| v.as_str())
            });
        if let Some(url) = url {
            return json!({
                "created": now_secs(),
                "data": [{"url": url, "revised_prompt": prompt}],
            });
        }
        empty_normalized()
    }
}
