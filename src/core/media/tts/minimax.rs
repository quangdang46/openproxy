//! MiniMax T2A — non-streaming, returns hex-encoded audio.

use async_trait::async_trait;
use base64::Engine as _;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{json, Value};

use super::base::{TtsAdapter, TtsError, TtsRequest, TtsResult};

pub struct MinimaxAdapter;
pub static ADAPTER: MinimaxAdapter = MinimaxAdapter;

const DEFAULT_BASE: &str = "https://api.minimaxi.com/v1/t2a_v2";
const DEFAULT_MODEL: &str = "speech-2.8-hd";
const DEFAULT_VOICE: &str = "English_expressive_narrator";

#[async_trait]
impl TtsAdapter for MinimaxAdapter {
    async fn synthesize(
        &self,
        client: &Client,
        request: &TtsRequest<'_>,
    ) -> Result<TtsResult, TtsError> {
        let api_key = request
            .credentials
            .api_key
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| TtsError::MissingCredentials("minimax".to_string()))?;

        let (model_id, voice_id) = if request.model.contains('/') {
            let mut parts = request.model.splitn(2, '/');
            (
                parts.next().unwrap_or("").to_string(),
                parts.next().unwrap_or("").to_string(),
            )
        } else if !request.model.is_empty() {
            (DEFAULT_MODEL.to_string(), request.model.to_string())
        } else {
            (DEFAULT_MODEL.to_string(), DEFAULT_VOICE.to_string())
        };
        let model_id = if model_id.is_empty() {
            DEFAULT_MODEL.to_string()
        } else {
            model_id
        };
        let voice_id = if voice_id.is_empty() {
            DEFAULT_VOICE.to_string()
        } else {
            voice_id
        };

        let base = request
            .credentials
            .provider_specific_data
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_BASE);

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|e| TtsError::Parse(e.to_string()))?,
        );

        let body = json!({
            "model": model_id,
            "text": request.text,
            "stream": false,
            "language_boost": "auto",
            "output_format": "hex",
            "voice_setting": {
                "voice_id": voice_id,
                "speed": 1,
                "vol": 1,
                "pitch": 0,
            },
            "audio_setting": {
                "sample_rate": 32000,
                "bitrate": 128000,
                "format": "mp3",
                "channel": 1,
            }
        });

        let res = client
            .post(base)
            .headers(headers)
            .json(&body)
            .send()
            .await?;
        let status = res.status();
        let raw = res.text().await.unwrap_or_default();
        let parsed: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        let base_resp = parsed.get("base_resp").or_else(|| parsed.get("baseResp"));
        let status_code = base_resp
            .and_then(|b| b.get("status_code").or_else(|| b.get("statusCode")))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let status_msg = base_resp
            .and_then(|b| b.get("status_msg").or_else(|| b.get("statusMsg")))
            .and_then(|v| v.as_str())
            .or_else(|| parsed.get("message").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();

        if !status.is_success() {
            return Err(TtsError::Upstream {
                status: status.as_u16(),
                message: if status_msg.is_empty() {
                    raw
                } else {
                    status_msg
                },
            });
        }
        if status_code != 0 {
            return Err(TtsError::Upstream {
                status: status.as_u16(),
                message: if status_msg.is_empty() {
                    "MiniMax TTS upstream error".into()
                } else {
                    status_msg
                },
            });
        }

        let audio_hex = parsed
            .pointer("/data/audio")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TtsError::Parse("MiniMax TTS returned no audio".into()))?
            .trim();
        if audio_hex.is_empty()
            || audio_hex.len() % 2 != 0
            || !audio_hex.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(TtsError::Parse("MiniMax TTS returned invalid audio".into()));
        }
        let bytes =
            hex::decode(audio_hex).map_err(|e| TtsError::Parse(format!("hex decode: {e}")))?;
        let format = parsed
            .pointer("/extra_info/audio_format")
            .or_else(|| parsed.pointer("/extraInfo/audioFormat"))
            .and_then(|v| v.as_str())
            .unwrap_or("mp3")
            .to_string();
        Ok(TtsResult {
            base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            format,
        })
    }
}
