//! Port of `open-sse/translator/helpers/imageHelper.js`.
//!
//! Fetches a remote image URL and returns a base64 data-URI plus its
//! MIME type, used when upstream providers (Codex, Gemini) require
//! inline base64 instead of remote URLs they cannot fetch.

use base64::Engine as _;
use reqwest::Client;
use std::time::Duration;

/// Outcome of a successful fetch.
#[derive(Debug, Clone)]
pub struct FetchedImage {
    /// `data:<mime>;base64,<payload>` URI.
    pub data_url: String,
    pub mime_type: String,
}

/// Fetch `image_url` and return its data-URI + content-type. Returns
/// `None` on any error (matches the JS contract — never throws).
pub async fn fetch_image_as_base64(client: &Client, image_url: &str) -> Option<FetchedImage> {
    if !image_url.starts_with("http://") && !image_url.starts_with("https://") {
        return None;
    }

    let res = client
        .get(image_url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        return None;
    }
    let mime = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_string();
    let bytes = res.bytes().await.ok()?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(FetchedImage {
        data_url: format!("data:{mime};base64,{b64}"),
        mime_type: mime,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_non_http_url() {
        let client = Client::new();
        assert!(fetch_image_as_base64(&client, "data:image/png;base64,abc")
            .await
            .is_none());
        assert!(fetch_image_as_base64(&client, "/local/file.png")
            .await
            .is_none());
    }
}
