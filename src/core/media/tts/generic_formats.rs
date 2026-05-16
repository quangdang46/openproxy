//! Config-driven TTS handlers for providers without a special adapter
//! (hyperbolic, deepgram, nvidia, huggingface, inworld, cartesia, playht,
//! coqui, tortoise, openai-compat). Mirrors `genericFormats.js`.

use base64::Engine as _;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{json, Value};

use super::base::{response_to_base64, upstream_error, TtsError, TtsResult};

/// Format key used by `ttsConfig.format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenericFormat {
    Hyperbolic,
    Deepgram,
    NvidiaTts,
    HuggingfaceTts,
    Inworld,
    Cartesia,
    Playht,
    Coqui,
    Tortoise,
    OpenaiCompat,
    MinimaxTts,
}

impl GenericFormat {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "hyperbolic" => GenericFormat::Hyperbolic,
            "deepgram" => GenericFormat::Deepgram,
            "nvidia-tts" => GenericFormat::NvidiaTts,
            "huggingface-tts" => GenericFormat::HuggingfaceTts,
            "inworld" => GenericFormat::Inworld,
            "cartesia" => GenericFormat::Cartesia,
            "playht" => GenericFormat::Playht,
            "coqui" => GenericFormat::Coqui,
            "tortoise" => GenericFormat::Tortoise,
            "openai" => GenericFormat::OpenaiCompat,
            "minimax-tts" => GenericFormat::MinimaxTts,
            _ => return None,
        })
    }
}

/// Inputs accepted by [`synthesize_via_format`].
#[derive(Debug, Clone)]
pub struct GenericTtsRequest<'a> {
    pub format: GenericFormat,
    pub base_url: &'a str,
    pub api_key: Option<&'a str>,
    pub text: &'a str,
    pub model_id: &'a str,
    pub voice_id: &'a str,
}

/// Dispatch a generic config-driven TTS request.
pub async fn synthesize_via_format(
    client: &Client,
    request: GenericTtsRequest<'_>,
) -> Result<TtsResult, TtsError> {
    use GenericFormat::*;
    match request.format {
        Hyperbolic => hyperbolic(client, request).await,
        Deepgram => deepgram(client, request).await,
        NvidiaTts => nvidia(client, request).await,
        HuggingfaceTts => huggingface(client, request).await,
        Inworld => inworld(client, request).await,
        Cartesia => cartesia(client, request).await,
        Playht => playht(client, request).await,
        Coqui => coqui(client, request).await,
        Tortoise => tortoise(client, request).await,
        OpenaiCompat => openai_compat(client, request).await,
        MinimaxTts => Err(TtsError::Parse(
            "minimax-tts dispatched via the dedicated adapter".into(),
        )),
    }
}

fn require_key<'a>(req: &GenericTtsRequest<'a>, provider: &str) -> Result<&'a str, TtsError> {
    req.api_key
        .filter(|s| !s.is_empty())
        .ok_or_else(|| TtsError::MissingCredentials(provider.to_string()))
}

fn bearer_headers(api_key: &str) -> Result<HeaderMap, TtsError> {
    let mut h = HeaderMap::new();
    h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    h.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|e| TtsError::Parse(e.to_string()))?,
    );
    Ok(h)
}

async fn hyperbolic(client: &Client, req: GenericTtsRequest<'_>) -> Result<TtsResult, TtsError> {
    let key = require_key(&req, "hyperbolic")?;
    let res = client
        .post(req.base_url)
        .headers(bearer_headers(key)?)
        .json(&json!({"text": req.text}))
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(upstream_error(res).await);
    }
    let v: Value = res.json().await.map_err(TtsError::from)?;
    let audio = v
        .get("audio")
        .and_then(|s| s.as_str())
        .ok_or_else(|| TtsError::Parse("hyperbolic: no audio".into()))?
        .to_string();
    Ok(TtsResult {
        base64: audio,
        format: "mp3".to_string(),
    })
}

async fn deepgram(client: &Client, req: GenericTtsRequest<'_>) -> Result<TtsResult, TtsError> {
    let key = require_key(&req, "deepgram")?;
    let mut url = reqwest::Url::parse(req.base_url)
        .map_err(|e| TtsError::Parse(format!("deepgram url: {e}")))?;
    let model = if req.model_id.is_empty() {
        "aura-asteria-en"
    } else {
        req.model_id
    };
    url.query_pairs_mut().append_pair("model", model);
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Token {key}"))
            .map_err(|e| TtsError::Parse(e.to_string()))?,
    );
    let res = client
        .post(url)
        .headers(headers)
        .json(&json!({"text": req.text}))
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(upstream_error(res).await);
    }
    response_to_base64(res, "mp3").await
}

async fn nvidia(client: &Client, req: GenericTtsRequest<'_>) -> Result<TtsResult, TtsError> {
    let key = require_key(&req, "nvidia-tts")?;
    let voice = if req.voice_id.is_empty() {
        "default"
    } else {
        req.voice_id
    };
    let body = json!({
        "input": {"text": req.text},
        "voice": voice,
        "model": req.model_id,
    });
    let res = client
        .post(req.base_url)
        .headers(bearer_headers(key)?)
        .json(&body)
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(upstream_error(res).await);
    }
    response_to_base64(res, "wav").await
}

async fn huggingface(client: &Client, req: GenericTtsRequest<'_>) -> Result<TtsResult, TtsError> {
    let key = require_key(&req, "huggingface-tts")?;
    if req.model_id.is_empty() || req.model_id.contains("..") {
        return Err(TtsError::Parse("Invalid HuggingFace model ID".into()));
    }
    let res = client
        .post(format!("{}/{}", req.base_url.trim_end_matches('/'), req.model_id))
        .headers(bearer_headers(key)?)
        .json(&json!({"inputs": req.text}))
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(upstream_error(res).await);
    }
    response_to_base64(res, "wav").await
}

async fn inworld(client: &Client, req: GenericTtsRequest<'_>) -> Result<TtsResult, TtsError> {
    let key = require_key(&req, "inworld")?;
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Basic {key}"))
            .map_err(|e| TtsError::Parse(e.to_string()))?,
    );
    let body = json!({
        "text": req.text,
        "voiceId": if req.voice_id.is_empty() { "Alex" } else { req.voice_id },
        "modelId": if req.model_id.is_empty() { "inworld-tts-1.5-mini" } else { req.model_id },
        "audioConfig": {"audioEncoding": "MP3"},
    });
    let res = client
        .post(req.base_url)
        .headers(headers)
        .json(&body)
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(upstream_error(res).await);
    }
    let v: Value = res.json().await.map_err(TtsError::from)?;
    let audio = v
        .get("audioContent")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TtsError::Parse("inworld: no audioContent".into()))?
        .to_string();
    Ok(TtsResult {
        base64: audio,
        format: "mp3".to_string(),
    })
}

async fn cartesia(client: &Client, req: GenericTtsRequest<'_>) -> Result<TtsResult, TtsError> {
    let key = require_key(&req, "cartesia")?;
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        "X-API-Key",
        HeaderValue::from_str(key).map_err(|e| TtsError::Parse(e.to_string()))?,
    );
    headers.insert("Cartesia-Version", HeaderValue::from_static("2024-06-10"));
    let mut body = json!({
        "model_id": if req.model_id.is_empty() { "sonic-2" } else { req.model_id },
        "transcript": req.text,
        "output_format": {"container": "mp3", "bit_rate": 128000, "sample_rate": 44100},
    });
    if !req.voice_id.is_empty() {
        if let Some(obj) = body.as_object_mut() {
            obj.insert(
                "voice".into(),
                json!({"mode": "id", "id": req.voice_id}),
            );
        }
    }
    let res = client
        .post(req.base_url)
        .headers(headers)
        .json(&body)
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(upstream_error(res).await);
    }
    response_to_base64(res, "mp3").await
}

async fn playht(client: &Client, req: GenericTtsRequest<'_>) -> Result<TtsResult, TtsError> {
    let combined = req.api_key.unwrap_or("");
    let mut split = combined.splitn(2, ':');
    let user_id = split.next().unwrap_or("").to_string();
    let key = split.next().unwrap_or("").to_string();
    let auth_token = if key.is_empty() {
        combined.to_string()
    } else {
        key
    };
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert("Accept", HeaderValue::from_static("audio/mpeg"));
    if !user_id.is_empty() {
        headers.insert(
            "X-USER-ID",
            HeaderValue::from_str(&user_id).map_err(|e| TtsError::Parse(e.to_string()))?,
        );
    }
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {auth_token}"))
            .map_err(|e| TtsError::Parse(e.to_string()))?,
    );
    let voice = if req.voice_id.is_empty() {
        "s3://voice-cloning-zero-shot/d9ff78ba-d016-47f6-b0ef-dd630f59414e/female-cs/manifest.json"
    } else {
        req.voice_id
    };
    let engine = if req.model_id.is_empty() {
        "PlayDialog"
    } else {
        req.model_id
    };
    let body = json!({
        "text": req.text,
        "voice": voice,
        "voice_engine": engine,
        "output_format": "mp3",
        "speed": 1,
    });
    let res = client
        .post(req.base_url)
        .headers(headers)
        .json(&body)
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(upstream_error(res).await);
    }
    response_to_base64(res, "mp3").await
}

async fn coqui(client: &Client, req: GenericTtsRequest<'_>) -> Result<TtsResult, TtsError> {
    let mut body = json!({"text": req.text});
    if !req.voice_id.is_empty() {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("speaker_id".into(), json!(req.voice_id));
        }
    }
    let res = client
        .post(req.base_url)
        .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
        .json(&body)
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(upstream_error(res).await);
    }
    response_to_base64(res, "wav").await
}

async fn tortoise(client: &Client, req: GenericTtsRequest<'_>) -> Result<TtsResult, TtsError> {
    let voice = if req.voice_id.is_empty() {
        "random"
    } else {
        req.voice_id
    };
    let body = json!({"text": req.text, "voice": voice});
    let res = client
        .post(req.base_url)
        .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
        .json(&body)
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(upstream_error(res).await);
    }
    response_to_base64(res, "wav").await
}

async fn openai_compat(
    client: &Client,
    req: GenericTtsRequest<'_>,
) -> Result<TtsResult, TtsError> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Some(key) = req.api_key.filter(|s| !s.is_empty()) {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|e| TtsError::Parse(e.to_string()))?,
        );
    }
    let voice = if req.voice_id.is_empty() {
        "alloy"
    } else {
        req.voice_id
    };
    let body = json!({
        "model": req.model_id,
        "input": req.text,
        "voice": voice,
        "response_format": "mp3",
        "speed": 1.0,
    });
    let res = client
        .post(req.base_url)
        .headers(headers)
        .json(&body)
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(upstream_error(res).await);
    }
    response_to_base64(res, "mp3").await
}

#[allow(dead_code)]
fn _force_base64_link() {
    let _ = base64::engine::general_purpose::STANDARD.encode([0u8]);
}
