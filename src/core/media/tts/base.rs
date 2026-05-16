//! Common pieces for TTS adapters.

use async_trait::async_trait;
use base64::Engine as _;
use reqwest::Client;
use thiserror::Error;

use crate::types::ProviderConnection;

/// Browser-style User-Agent sent by Edge-TTS / Google-TTS upstream
/// scrapes — these endpoints reject anything that smells like curl.
pub(crate) const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";

/// Errors a TTS adapter can produce.
#[derive(Debug, Error)]
pub enum TtsError {
    #[error("missing credentials: {0}")]
    MissingCredentials(String),
    #[error("upstream HTTP {status}: {message}")]
    Upstream { status: u16, message: String },
    #[error("invalid response: {0}")]
    Parse(String),
    #[error("network: {0}")]
    Network(String),
}

impl From<reqwest::Error> for TtsError {
    fn from(e: reqwest::Error) -> Self {
        TtsError::Network(e.to_string())
    }
}

/// Inbound TTS request.
#[derive(Debug, Clone)]
pub struct TtsRequest<'a> {
    pub text: &'a str,
    /// Provider-specific model+voice identifier (often `model/voice` form).
    pub model: &'a str,
    pub credentials: &'a ProviderConnection,
    /// Optional language hint (Gemini honours this for prompt phrasing).
    pub language: Option<&'a str>,
}

/// Result returned by an adapter: base64 audio + format string.
#[derive(Debug, Clone)]
pub struct TtsResult {
    /// Base64-encoded audio bytes.
    pub base64: String,
    /// `"mp3"`, `"wav"`, `"ogg"`, …
    pub format: String,
}

#[async_trait]
pub trait TtsAdapter: Send + Sync {
    /// Whether the upstream is no-auth (Edge-TTS, Google-TTS, local-device, comfy).
    fn no_auth(&self) -> bool {
        false
    }

    async fn synthesize(
        &self,
        client: &Client,
        request: &TtsRequest<'_>,
    ) -> Result<TtsResult, TtsError>;
}

/// Read an upstream audio response into `(base64, format)`. Inspects
/// `Content-Type` to pick a sensible format, defaulting to `default_format`.
pub async fn response_to_base64(
    response: reqwest::Response,
    default_format: &str,
) -> Result<TtsResult, TtsError> {
    let ctype = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = response
        .bytes()
        .await
        .map_err(|e| TtsError::Network(e.to_string()))?;
    if bytes.len() < 100 {
        return Err(TtsError::Parse("Upstream returned empty audio".into()));
    }
    let format = if ctype.contains("wav") {
        "wav".to_string()
    } else if ctype.contains("mpeg") || ctype.contains("mp3") {
        "mp3".to_string()
    } else if ctype.contains("ogg") {
        "ogg".to_string()
    } else {
        default_format.to_string()
    };
    Ok(TtsResult {
        base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        format,
    })
}

/// Read an upstream error response and bubble it up as a TtsError.
pub async fn upstream_error(response: reqwest::Response) -> TtsError {
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let msg = if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
        parsed
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .or_else(|| parsed.get("message").and_then(|v| v.as_str()))
            .or_else(|| {
                parsed
                    .pointer("/detail/message")
                    .and_then(|v| v.as_str())
            })
            .or_else(|| parsed.get("detail").and_then(|v| v.as_str()))
            .map(str::to_string)
            .unwrap_or(text.clone())
    } else if !text.is_empty() {
        text.clone()
    } else {
        format!("Upstream error ({status})")
    };
    TtsError::Upstream {
        status,
        message: msg,
    }
}

/// Parse a `model/voice` string against a list of known model ids
/// (longest-prefix wins). Mirrors the JS `parseModelVoice` helper.
pub fn parse_model_voice<'a>(
    model: &'a str,
    default_model: &'a str,
    default_voice: &'a str,
    known_models: &[&str],
) -> (String, String) {
    if model.is_empty() {
        return (default_model.to_string(), default_voice.to_string());
    }
    let mut sorted: Vec<&str> = known_models.iter().copied().filter(|m| !m.is_empty()).collect();
    sorted.sort_by(|a, b| b.len().cmp(&a.len()));
    for id in sorted {
        if model == id {
            return (id.to_string(), default_voice.to_string());
        }
        let prefix = format!("{id}/");
        if let Some(rest) = model.strip_prefix(&prefix) {
            return (id.to_string(), rest.to_string());
        }
    }
    if let Some(idx) = model.rfind('/') {
        return (model[..idx].to_string(), model[idx + 1..].to_string());
    }
    if !default_model.is_empty() {
        (default_model.to_string(), default_voice.to_string())
    } else {
        (model.to_string(), default_voice.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_model_voice_picks_longest_prefix() {
        let known = ["tts-1", "tts-1-hd"];
        let (m, v) = parse_model_voice("tts-1-hd/alloy", "default", "default-voice", &known);
        assert_eq!(m, "tts-1-hd");
        assert_eq!(v, "alloy");
    }

    #[test]
    fn parse_model_voice_fallback_to_default_model() {
        let (m, v) = parse_model_voice("Kore", "gemini-tts", "Kore", &["gemini-tts"]);
        assert_eq!(m, "gemini-tts");
        assert_eq!(v, "Kore");
    }

    #[test]
    fn parse_model_voice_empty_returns_defaults() {
        let (m, v) = parse_model_voice("", "tts-1", "alloy", &["tts-1"]);
        assert_eq!(m, "tts-1");
        assert_eq!(v, "alloy");
    }
}
