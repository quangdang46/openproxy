//! AWS Polly TTS — AWS Signature v4 signed requests.
//!
//! Credentials in `provider_specific_data`:
//!   - `accessKeyId`   — AWS IAM Access Key ID (required)
//!   - `region`        — AWS region (default: us-east-1)
//!
//! The `api_key` field holds the AWS Secret Access Key.
//!
//! Model/voice format: `[Engine/]VoiceId[.OutputFormat]`
//!   - `Joanna`               → engine=neural, voice=Joanna, format=mp3
//!   - `neural/Joanna`        → engine=neural, voice=Joanna, format=mp3
//!   - `generative/Matthew`   → engine=generative, voice=Matthew, format=mp3
//!   - `neural/Joanna.pcm`    → engine=neural, voice=Joanna, format=pcm (WAV)
//!   - `Joanna.ogg`           → engine=neural, voice=Joanna, format=ogg_vorbis
//!
//! SSML auto-detection: if the input text starts with `<speak` the request
//! sets `TextType: "ssml"`.

use async_trait::async_trait;
use base64::Engine as _;
use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, HOST};
use reqwest::Client;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::base::{upstream_error, TtsAdapter, TtsError, TtsRequest, TtsResult};

type HmacSha256 = Hmac<Sha256>;

const DEFAULT_REGION: &str = "us-east-1";
const DEFAULT_ENGINE: &str = "neural";
const DEFAULT_VOICE: &str = "Joanna";
const DEFAULT_OUTPUT_FORMAT: &str = "mp3";

pub struct AwsPollyAdapter;
pub static ADAPTER: AwsPollyAdapter = AwsPollyAdapter;

// ---------------------------------------------------------------------------
// AWS Signature v4 helpers
// ---------------------------------------------------------------------------

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn signing_key(secret: &str, date_stamp: &str, region: &str) -> Vec<u8> {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, b"polly");
    hmac_sha256(&k_service, b"aws4_request")
}

fn build_authorization_header(
    access_key: &str,
    secret_key: &str,
    region: &str,
    host: &str,
    timestamp: &str,
    date_stamp: &str,
    payload_hash: &str,
) -> String {
    let algorithm = "AWS4-HMAC-SHA256";
    let credential_scope = format!("{date_stamp}/{region}/polly/aws4_request");
    let signed_headers = "content-type;host;x-amz-date";

    // Canonical request
    let canonical_request = format!(
        "POST\n/v1/speech\n\n\
         content-type:application/json\n\
         host:{host}\n\
         x-amz-date:{timestamp}\n\
         \n\
         {signed_headers}\n\
         {payload_hash}"
    );

    // String-to-sign
    let cs_hash = sha256_hex(canonical_request.as_bytes());
    let string_to_sign = format!("{algorithm}\n{timestamp}\n{credential_scope}\n{cs_hash}");

    // Signature
    let key = signing_key(secret_key, date_stamp, region);
    let signature = hex::encode(hmac_sha256(&key, string_to_sign.as_bytes()));

    format!(
        "{algorithm} Credential={access_key}/{credential_scope}, \
         SignedHeaders={signed_headers}, Signature={signature}"
    )
}

// ---------------------------------------------------------------------------
// Model / voice / format parsing
// ---------------------------------------------------------------------------

struct ParsedModel {
    engine: String,
    voice: String,
    output_format: String,
}

fn parse_model(model: &str) -> ParsedModel {
    let mut engine = DEFAULT_ENGINE.to_string();
    let mut voice = DEFAULT_VOICE.to_string();
    let mut output_format = DEFAULT_OUTPUT_FORMAT.to_string();

    if model.is_empty() {
        return ParsedModel {
            engine,
            voice,
            output_format,
        };
    }

    // Split off optional .OutputFormat suffix
    let (rest, fmt) = if let Some(dot) = model.rfind('.') {
        let ext = &model[dot + 1..];
        match ext {
            "mp3" | "pcm" | "ogg" | "wav" | "ogg_vorbis" | "ogg_opus" => (&model[..dot], Some(ext)),
            _ => (model, None),
        }
    } else {
        (model, None)
    };

    if let Some(f) = fmt {
        output_format = match f {
            "ogg" | "ogg_vorbis" => "ogg_vorbis".to_string(),
            "ogg_opus" => "ogg_opus".to_string(),
            "wav" => "pcm".to_string(),
            _ => f.to_string(),
        };
    }

    // Split Engine/Voice
    if let Some(slash) = rest.find('/') {
        let eng = &rest[..slash];
        let v = &rest[slash + 1..];
        if !eng.is_empty() {
            engine = eng.to_string();
        }
        if !v.is_empty() {
            voice = v.to_string();
        }
    } else if !rest.is_empty() {
        voice = rest.to_string();
    }

    ParsedModel {
        engine,
        voice,
        output_format,
    }
}

// ---------------------------------------------------------------------------
// PCM -> WAV (Polly PCM is signed 16-bit LE)
// ---------------------------------------------------------------------------

fn pcm_to_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let channels: u16 = 1;
    let bits_per_sample: u16 = 16;
    let data_size = pcm.len() as u32;
    let byte_rate = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
    let block_align = channels * bits_per_sample / 8;

    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_size).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_size.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

/// Native sample rate per Polly engine.
fn engine_sample_rate(engine: &str) -> u32 {
    match engine {
        "standard" => 16000,
        "generative" => 24000,
        _ => 22050, // neural, long-form, and default
    }
}

/// Map a Polly OutputFormat value to the format string returned in
/// [`TtsResult`].  PCM is handled separately (wrapped in WAV).
fn map_format(output_format: &str) -> &str {
    match output_format {
        "ogg_vorbis" | "ogg_opus" => "ogg",
        _ => "mp3",
    }
}

// ---------------------------------------------------------------------------
// TtsAdapter implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl TtsAdapter for AwsPollyAdapter {
    async fn synthesize(
        &self,
        client: &Client,
        request: &TtsRequest<'_>,
    ) -> Result<TtsResult, TtsError> {
        let secret_key = request
            .credentials
            .api_key
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                TtsError::MissingCredentials("aws-polly (api_key = AWS Secret Access Key)".into())
            })?;

        let access_key = request
            .credentials
            .provider_specific_data
            .get("accessKeyId")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                TtsError::MissingCredentials(
                    "aws-polly: provider_specific_data.accessKeyId is required".into(),
                )
            })?;

        let region = request
            .credentials
            .provider_specific_data
            .get("region")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_REGION);

        let parsed = parse_model(request.model);

        // SSML auto-detection
        let text_type = if request.text.trim().starts_with("<speak") {
            "ssml"
        } else {
            "text"
        };

        let body_val = json!({
            "Text": request.text,
            "OutputFormat": parsed.output_format,
            "VoiceId": parsed.voice,
            "Engine": parsed.engine,
            "TextType": text_type,
        });
        let body_bytes = serde_json::to_vec(&body_val)
            .map_err(|e| TtsError::Parse(format!("serialize body: {e}")))?;
        let payload_hash = sha256_hex(&body_bytes);

        let now = Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();

        let host = format!("polly.{region}.amazonaws.com");

        let auth = build_authorization_header(
            access_key,
            secret_key,
            region,
            &host,
            &amz_date,
            &date_stamp,
            &payload_hash,
        );

        let url = format!("https://{host}/v1/speech");

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            HOST,
            HeaderValue::from_str(&host).map_err(|e| TtsError::Parse(e.to_string()))?,
        );
        headers.insert(
            "x-amz-date",
            HeaderValue::from_str(&amz_date).map_err(|e| TtsError::Parse(e.to_string()))?,
        );
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth).map_err(|e| TtsError::Parse(e.to_string()))?,
        );

        let res = client
            .post(&url)
            .headers(headers)
            .body(body_bytes)
            .send()
            .await?;

        if !res.status().is_success() {
            return Err(upstream_error(res).await);
        }

        let raw = res.bytes().await?;
        if raw.len() < 100 {
            return Err(TtsError::Parse("AWS Polly returned empty audio".into()));
        }

        // For PCM output, wrap the raw PCM in a WAV container.
        let (audio_bytes, format_name) = if parsed.output_format == "pcm" {
            let sr = engine_sample_rate(&parsed.engine);
            let wav = pcm_to_wav(&raw, sr);
            (wav.into(), "wav".to_string())
        } else {
            (raw.to_vec(), map_format(&parsed.output_format).to_string())
        };

        Ok(TtsResult {
            base64: base64::engine::general_purpose::STANDARD.encode(&audio_bytes),
            format: format_name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_model_empty() {
        let m = parse_model("");
        assert_eq!(m.engine, DEFAULT_ENGINE);
        assert_eq!(m.voice, DEFAULT_VOICE);
        assert_eq!(m.output_format, DEFAULT_OUTPUT_FORMAT);
    }

    #[test]
    fn parse_model_voice_only() {
        let m = parse_model("Joanna");
        assert_eq!(m.engine, DEFAULT_ENGINE);
        assert_eq!(m.voice, "Joanna");
        assert_eq!(m.output_format, DEFAULT_OUTPUT_FORMAT);
    }

    #[test]
    fn parse_model_engine_voice() {
        let m = parse_model("generative/Matthew");
        assert_eq!(m.engine, "generative");
        assert_eq!(m.voice, "Matthew");
        assert_eq!(m.output_format, DEFAULT_OUTPUT_FORMAT);
    }

    #[test]
    fn parse_model_with_format_mp3() {
        let m = parse_model("Joanna.mp3");
        assert_eq!(m.engine, DEFAULT_ENGINE);
        assert_eq!(m.voice, "Joanna");
        assert_eq!(m.output_format, "mp3");
    }

    #[test]
    fn parse_model_with_format_pcm() {
        let m = parse_model("generative/Matthew.pcm");
        assert_eq!(m.engine, "generative");
        assert_eq!(m.voice, "Matthew");
        assert_eq!(m.output_format, "pcm");
    }

    #[test]
    fn parse_model_with_ogg_opus() {
        let m = parse_model("neural/Salli.ogg_opus");
        assert_eq!(m.engine, "neural");
        assert_eq!(m.voice, "Salli");
        assert_eq!(m.output_format, "ogg_opus");
    }

    #[test]
    fn parse_model_standard_engine() {
        let m = parse_model("standard/Ivy");
        assert_eq!(m.engine, "standard");
        assert_eq!(m.voice, "Ivy");
        assert_eq!(m.output_format, DEFAULT_OUTPUT_FORMAT);
    }

    #[test]
    fn sha256_hex_output() {
        let h = sha256_hex(b"hello");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn pcm_to_wav_header_size() {
        let pcm = vec![0u8; 1000];
        let wav = pcm_to_wav(&pcm, 22050);
        assert_eq!(wav.len(), 44 + 1000);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
    }

    #[test]
    fn engine_sample_rate_values() {
        assert_eq!(engine_sample_rate("standard"), 16000);
        assert_eq!(engine_sample_rate("neural"), 22050);
        assert_eq!(engine_sample_rate("long-form"), 22050);
        assert_eq!(engine_sample_rate("generative"), 24000);
        assert_eq!(engine_sample_rate("unknown"), 22050);
    }

    #[test]
    fn map_format_mappings() {
        assert_eq!(map_format("mp3"), "mp3");
        assert_eq!(map_format("ogg_vorbis"), "ogg");
        assert_eq!(map_format("ogg_opus"), "ogg");
    }
}
