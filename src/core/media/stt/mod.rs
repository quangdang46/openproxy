//! Speech-to-text (STT) handler.
//!
//! Port of `open-sse/handlers/sttCore.js`. Dispatches by upstream
//! `sttConfig.format`:
//!
//!   - `deepgram`        → raw binary POST + model query param
//!   - `assemblyai`      → upload → submit → poll (max 120s)
//!   - `nvidia-asr`      → multipart upload
//!   - `huggingface-asr` → raw binary POST to `{base}/{model}`
//!   - `gemini-stt`      → generateContent with inline_data audio
//!   - default           → OpenAI/Whisper/Groq-compatible multipart
//!
//! The handler accepts the audio bytes + filename + content-type up
//! front; the caller (axum extractor) is responsible for parsing the
//! incoming `multipart/form-data` request.

use base64::Engine as _;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::multipart::{Form, Part};
use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SttFormat {
    Deepgram,
    AssemblyAi,
    NvidiaAsr,
    HuggingfaceAsr,
    GeminiStt,
    /// OpenAI/Whisper/Groq-compatible multipart upload.
    OpenaiCompat,
}

impl SttFormat {
    pub fn parse(name: &str) -> Self {
        match name {
            "deepgram" => SttFormat::Deepgram,
            "assemblyai" => SttFormat::AssemblyAi,
            "nvidia-asr" => SttFormat::NvidiaAsr,
            "huggingface-asr" => SttFormat::HuggingfaceAsr,
            "gemini-stt" => SttFormat::GeminiStt,
            _ => SttFormat::OpenaiCompat,
        }
    }
}

/// Auth header style for `sttConfig.authHeader`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SttAuthHeader {
    Bearer,
    Token,
    XApiKey,
    Key,
}

impl SttAuthHeader {
    pub fn parse(name: &str) -> Self {
        match name {
            "token" => SttAuthHeader::Token,
            "x-api-key" => SttAuthHeader::XApiKey,
            "key" => SttAuthHeader::Key,
            _ => SttAuthHeader::Bearer,
        }
    }
}

/// Inputs for one STT request.
pub struct SttRequest<'a> {
    pub format: SttFormat,
    pub auth: SttAuthHeader,
    pub base_url: &'a str,
    pub token: Option<&'a str>,
    pub model: &'a str,
    /// Audio bytes (raw payload).
    pub audio: Vec<u8>,
    /// Original filename, e.g. `"audio.mp3"`. Used to derive content-type.
    pub filename: String,
    /// Optional explicit content-type override. Falls back to ext mapping.
    pub content_type: Option<String>,
    pub language: Option<&'a str>,
    pub prompt: Option<&'a str>,
    pub response_format: Option<&'a str>,
    pub temperature: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct SttResult {
    /// Transcribed text.
    pub text: String,
    /// Raw response body when the upstream returned an opaque blob (e.g.
    /// OpenAI-compat returning verbose JSON or SRT). For most providers
    /// this is just `{"text": text}`.
    pub raw_body: Value,
    /// Content-Type of the upstream response (so the caller can pass it
    /// through unchanged when needed).
    pub content_type: String,
}

#[derive(Debug, Error)]
pub enum SttError {
    #[error("HTTP {0}: {1}")]
    Http(u16, String),
    #[error("validation: {0}")]
    Validation(String),
    #[error("upstream: {0}")]
    Upstream(String),
}

fn build_auth_headers(auth: SttAuthHeader, token: Option<&str>) -> Result<HeaderMap, SttError> {
    let mut h = HeaderMap::new();
    let Some(token) = token else {
        return Ok(h);
    };
    if token.is_empty() {
        return Ok(h);
    }
    let (name, value) = match auth {
        SttAuthHeader::Bearer => (AUTHORIZATION, format!("Bearer {token}")),
        SttAuthHeader::Token => (AUTHORIZATION, format!("Token {token}")),
        SttAuthHeader::XApiKey => (HeaderName::from_static("x-api-key"), token.to_string()),
        SttAuthHeader::Key => (AUTHORIZATION, format!("Key {token}")),
    };
    h.insert(
        name,
        HeaderValue::from_str(&value).map_err(|e| SttError::Validation(e.to_string()))?,
    );
    Ok(h)
}

fn resolve_audio_content_type(filename: &str, override_ct: Option<&str>) -> String {
    if let Some(ct) = override_ct.filter(|c| c.starts_with("audio/")) {
        return ct.to_string();
    }
    let lower = filename.to_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "mp3" => "audio/mpeg",
        "mp4" | "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "webm" => "audio/webm",
        "aac" => "audio/aac",
        "opus" => "audio/opus",
        _ => "application/octet-stream",
    }
    .to_string()
}

async fn upstream_err(res: reqwest::Response) -> SttError {
    let status = res.status().as_u16();
    let text = res.text().await.unwrap_or_default();
    let msg = if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
        parsed
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .or_else(|| parsed.get("error").and_then(|v| v.as_str()))
            .or_else(|| parsed.get("message").and_then(|v| v.as_str()))
            .map(str::to_string)
            .unwrap_or(text.clone())
    } else if !text.is_empty() {
        text
    } else {
        format!("Upstream error ({status})")
    };
    SttError::Http(status, msg)
}

/// Run an STT request end-to-end. Returns the transcribed text and the
/// raw upstream body (so the caller can echo verbose JSON / SRT back to
/// the client when requested).
pub async fn handle_stt(client: &Client, request: SttRequest<'_>) -> Result<SttResult, SttError> {
    if request.audio.is_empty() {
        return Err(SttError::Validation("Missing required field: file".into()));
    }
    if request.token.is_none() && !matches!(request.format, SttFormat::Deepgram) {
        // Most STT providers require auth; deepgram is the only one we
        // sometimes accept un-authenticated (it's still rejected by the
        // upstream, but the JS shape expects us to forward the call).
    }

    match request.format {
        SttFormat::Deepgram => transcribe_deepgram(client, request).await,
        SttFormat::AssemblyAi => transcribe_assemblyai(client, request).await,
        SttFormat::NvidiaAsr => transcribe_nvidia(client, request).await,
        SttFormat::HuggingfaceAsr => transcribe_huggingface(client, request).await,
        SttFormat::GeminiStt => transcribe_gemini(client, request).await,
        SttFormat::OpenaiCompat => transcribe_openai_compat(client, request).await,
    }
}

async fn transcribe_deepgram(client: &Client, req: SttRequest<'_>) -> Result<SttResult, SttError> {
    let mut url = reqwest::Url::parse(req.base_url)
        .map_err(|e| SttError::Validation(format!("deepgram url: {e}")))?;
    url.query_pairs_mut().append_pair("model", req.model);
    url.query_pairs_mut().append_pair("smart_format", "true");
    url.query_pairs_mut().append_pair("punctuate", "true");
    if let Some(lang) = req.language.filter(|s| !s.trim().is_empty()) {
        url.query_pairs_mut().append_pair("language", lang.trim());
    } else {
        url.query_pairs_mut().append_pair("detect_language", "true");
    }

    let mut headers = build_auth_headers(req.auth, req.token)?;
    let ct = resolve_audio_content_type(&req.filename, req.content_type.as_deref());
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(&ct).map_err(|e| SttError::Validation(e.to_string()))?,
    );

    let res = client
        .post(url)
        .headers(headers)
        .body(req.audio)
        .send()
        .await
        .map_err(|e| SttError::Upstream(e.to_string()))?;
    if !res.status().is_success() {
        return Err(upstream_err(res).await);
    }
    let body: Value = res
        .json()
        .await
        .map_err(|e| SttError::Upstream(e.to_string()))?;
    let text = body
        .pointer("/results/channels/0/alternatives/0/transcript")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(SttResult {
        text,
        raw_body: body,
        content_type: "application/json".into(),
    })
}

async fn transcribe_assemblyai(
    client: &Client,
    req: SttRequest<'_>,
) -> Result<SttResult, SttError> {
    let auth = build_auth_headers(req.auth, req.token)?;

    // 1. Upload.
    let up = client
        .post("https://api.assemblyai.com/v2/upload")
        .headers(auth.clone())
        .header(CONTENT_TYPE, "application/octet-stream")
        .body(req.audio)
        .send()
        .await
        .map_err(|e| SttError::Upstream(e.to_string()))?;
    if !up.status().is_success() {
        return Err(upstream_err(up).await);
    }
    let up_body: Value = up
        .json()
        .await
        .map_err(|e| SttError::Upstream(e.to_string()))?;
    let upload_url = up_body
        .get("upload_url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SttError::Upstream("AssemblyAI: no upload_url".into()))?
        .to_string();

    // 2. Submit.
    let sub = client
        .post(req.base_url)
        .headers(auth.clone())
        .header(CONTENT_TYPE, "application/json")
        .json(&json!({
            "audio_url": upload_url,
            "speech_models": [req.model],
            "language_detection": true,
        }))
        .send()
        .await
        .map_err(|e| SttError::Upstream(e.to_string()))?;
    if !sub.status().is_success() {
        return Err(upstream_err(sub).await);
    }
    let sub_body: Value = sub
        .json()
        .await
        .map_err(|e| SttError::Upstream(e.to_string()))?;
    let id = sub_body
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SttError::Upstream("AssemblyAI: no id".into()))?
        .to_string();

    // 3. Poll.
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let poll = client
            .get(format!("{}/{}", req.base_url.trim_end_matches('/'), id))
            .headers(auth.clone())
            .send()
            .await
            .map_err(|e| SttError::Upstream(e.to_string()))?;
        if !poll.status().is_success() {
            continue;
        }
        let r: Value = poll
            .json()
            .await
            .map_err(|e| SttError::Upstream(e.to_string()))?;
        match r.get("status").and_then(|v| v.as_str()) {
            Some("completed") => {
                let text = r
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                return Ok(SttResult {
                    text,
                    raw_body: r,
                    content_type: "application/json".into(),
                });
            }
            Some("error") => {
                let msg = r
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("AssemblyAI failed");
                return Err(SttError::Http(500, msg.to_string()));
            }
            _ => {}
        }
    }
    Err(SttError::Http(504, "AssemblyAI timeout after 120s".into()))
}

async fn transcribe_nvidia(client: &Client, req: SttRequest<'_>) -> Result<SttResult, SttError> {
    let part = Part::bytes(req.audio.clone()).file_name(req.filename.clone());
    let form = Form::new()
        .part("file", part)
        .text("model", req.model.to_string());
    let res = client
        .post(req.base_url)
        .headers(build_auth_headers(req.auth, req.token)?)
        .multipart(form)
        .send()
        .await
        .map_err(|e| SttError::Upstream(e.to_string()))?;
    if !res.status().is_success() {
        return Err(upstream_err(res).await);
    }
    let body: Value = res
        .json()
        .await
        .map_err(|e| SttError::Upstream(e.to_string()))?;
    let text = body
        .get("text")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("transcript").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    Ok(SttResult {
        text,
        raw_body: body,
        content_type: "application/json".into(),
    })
}

async fn transcribe_huggingface(
    client: &Client,
    req: SttRequest<'_>,
) -> Result<SttResult, SttError> {
    if req.model.contains("..") || req.model.contains("//") {
        return Err(SttError::Validation("Invalid model ID".into()));
    }
    let url = format!("{}/{}", req.base_url.trim_end_matches('/'), req.model);
    let mut headers = build_auth_headers(req.auth, req.token)?;
    let ct = resolve_audio_content_type(&req.filename, req.content_type.as_deref());
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(&ct).map_err(|e| SttError::Validation(e.to_string()))?,
    );
    let res = client
        .post(&url)
        .headers(headers)
        .body(req.audio)
        .send()
        .await
        .map_err(|e| SttError::Upstream(e.to_string()))?;
    if !res.status().is_success() {
        return Err(upstream_err(res).await);
    }
    let body: Value = res
        .json()
        .await
        .map_err(|e| SttError::Upstream(e.to_string()))?;
    let text = body
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(SttResult {
        text,
        raw_body: body,
        content_type: "application/json".into(),
    })
}

async fn transcribe_gemini(client: &Client, req: SttRequest<'_>) -> Result<SttResult, SttError> {
    let token = req
        .token
        .ok_or_else(|| SttError::Validation("gemini-stt requires API key".into()))?;
    let mime = resolve_audio_content_type(&req.filename, req.content_type.as_deref());
    let b64 = base64::engine::general_purpose::STANDARD.encode(&req.audio);
    let mut prompt = req
        .prompt
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            "Generate a transcript of the speech. Return only the transcribed text, no commentary."
                .into()
        });
    if let Some(lang) = req.language.filter(|s| !s.trim().is_empty()) {
        prompt.push_str(&format!(" Language: {}.", lang.trim()));
    }

    let url = format!(
        "{}/{}:generateContent?key={}",
        req.base_url.trim_end_matches('/'),
        req.model,
        urlencoding::encode(token)
    );
    let body = json!({
        "contents": [{
            "parts": [
                {"text": prompt},
                {"inline_data": {"mime_type": mime, "data": b64}}
            ]
        }]
    });
    let res = client
        .post(&url)
        .header(CONTENT_TYPE, "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| SttError::Upstream(e.to_string()))?;
    if !res.status().is_success() {
        return Err(upstream_err(res).await);
    }
    let parsed: Value = res
        .json()
        .await
        .map_err(|e| SttError::Upstream(e.to_string()))?;
    let text = parsed
        .pointer("/candidates/0/content/parts")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| p.get("text").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    Ok(SttResult {
        text,
        raw_body: parsed,
        content_type: "application/json".into(),
    })
}

async fn transcribe_openai_compat(
    client: &Client,
    req: SttRequest<'_>,
) -> Result<SttResult, SttError> {
    let part = Part::bytes(req.audio.clone()).file_name(req.filename.clone());
    let mut form = Form::new()
        .part("file", part)
        .text("model", req.model.to_string());
    if let Some(v) = req.language.filter(|s| !s.is_empty()) {
        form = form.text("language", v.to_string());
    }
    if let Some(v) = req.prompt.filter(|s| !s.is_empty()) {
        form = form.text("prompt", v.to_string());
    }
    if let Some(v) = req.response_format.filter(|s| !s.is_empty()) {
        form = form.text("response_format", v.to_string());
    }
    if let Some(v) = req.temperature.filter(|s| !s.is_empty()) {
        form = form.text("temperature", v.to_string());
    }

    let res = client
        .post(req.base_url)
        .headers(build_auth_headers(req.auth, req.token)?)
        .multipart(form)
        .send()
        .await
        .map_err(|e| SttError::Upstream(e.to_string()))?;
    if !res.status().is_success() {
        return Err(upstream_err(res).await);
    }
    let ct = res
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let body_text = res
        .text()
        .await
        .map_err(|e| SttError::Upstream(e.to_string()))?;
    let body_value: Value = serde_json::from_str(&body_text).unwrap_or(Value::Null);
    let text = body_value
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(SttResult {
        text,
        raw_body: if body_value.is_null() {
            json!({"raw": body_text})
        } else {
            body_value
        },
        content_type: ct,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_content_type_from_extension() {
        assert_eq!(resolve_audio_content_type("file.mp3", None), "audio/mpeg");
        assert_eq!(resolve_audio_content_type("file.wav", None), "audio/wav");
        assert_eq!(
            resolve_audio_content_type("unknown.xyz", None),
            "application/octet-stream"
        );
        assert_eq!(
            resolve_audio_content_type("file.mp3", Some("audio/x-flac")),
            "audio/x-flac"
        );
    }

    #[test]
    fn auth_header_dispatches_correctly() {
        let h = build_auth_headers(SttAuthHeader::Token, Some("t")).unwrap();
        assert_eq!(h.get(AUTHORIZATION).unwrap(), "Token t");
        let h = build_auth_headers(SttAuthHeader::XApiKey, Some("t")).unwrap();
        assert_eq!(h.get("x-api-key").unwrap(), "t");
        let h = build_auth_headers(SttAuthHeader::Key, Some("t")).unwrap();
        assert_eq!(h.get(AUTHORIZATION).unwrap(), "Key t");
        let h = build_auth_headers(SttAuthHeader::Bearer, Some("t")).unwrap();
        assert_eq!(h.get(AUTHORIZATION).unwrap(), "Bearer t");
    }

    #[test]
    fn empty_audio_validates() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let client = Client::new();
        let res = runtime.block_on(handle_stt(
            &client,
            SttRequest {
                format: SttFormat::OpenaiCompat,
                auth: SttAuthHeader::Bearer,
                base_url: "http://localhost",
                token: Some("k"),
                model: "whisper-1",
                audio: vec![],
                filename: "f.mp3".into(),
                content_type: None,
                language: None,
                prompt: None,
                response_format: None,
                temperature: None,
            },
        ));
        assert!(matches!(res, Err(SttError::Validation(_))));
    }
}
