//! Gemini Live API realtime STT transport (WebSocket).
//!
//! Port of upstream `open-sse/handlers/geminiLiveStt.js` (9router v0.5.104+,
//! #4006). Dispatched from `stt.rs` when the resolved model's transport marker
//! is `"gemini-live"` — never keyed on a hardcoded model id.
//!
//! The REST `generateContent` path only transcribes whole files inline. The
//! Live API's `:bidiGenerateContent` WebSocket is the streaming counterpart:
//! audio goes up as `realtimeInput.mediaChunks` and the server pushes
//! incremental `serverContent.inputTranscription` events back.
//!
//! Transport behavior (parity with the JS):
//!   - `wss://<host>/ws/api<v1beta path>/<model>:bidiGenerateContent?key=<token>`
//!   - setup → `setupComplete` → stream audio (16 KiB base64 chunks) →
//!     flushing `clientContent` turn → `turnComplete`.
//!   - Text accumulation is append-only over `inputTranscription` deltas and
//!     ends on `turnComplete` (or graceful close with partial text).
//!   - Transcription deltas are kept per-frame (`chunks`) so the caller can
//!     shape `verbose_json` segments as `{id, text}` only — the Live API
//!     frames carry NO timestamps, so `start`/`end`/`duration` are
//!     deliberately absent rather than fabricated as zeros.
//!   - `goAway` advisements rotate the socket once per call: setup replay +
//!     byte-offset resume (no audio re-upload).

use std::time::Duration;

use axum::http::StatusCode;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

/// open → setupComplete.
const SETUP_TIMEOUT: Duration = Duration::from_secs(10);
/// Audio streamed → turnComplete.
const TURN_TIMEOUT: Duration = Duration::from_secs(60);
/// Ceiling for client-supplied lifecycle knobs (setup/turn timeout fields).
const MAX_TIMEOUT_MS: u64 = 300_000;
/// ~0.5s of 16-bit 16kHz mono PCM per mediaChunk (JS `CHUNK_BYTES`).
const CHUNK_BYTES: usize = 16_384;
/// Socket rotations honoured per call (JS `GOAWAY_RECONNECTS`).
const GOAWAY_RECONNECTS: u32 = 1;

#[derive(Debug)]
pub struct GeminiLiveResult {
    /// Full transcript (concatenated deltas).
    pub text: String,
    /// Raw `inputTranscription` deltas in arrival order.
    pub chunks: Vec<String>,
}

#[derive(Debug)]
pub struct GeminiLiveError {
    pub status: StatusCode,
    pub message: String,
}

impl GeminiLiveError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

/// REST base (`https://host/v1beta/models`) → Live WS base
/// (`wss://host/ws/api/v1beta/models`), then the bidiGenerateContent endpoint.
/// Mirrors JS `toLiveWsUrl`.
pub fn to_live_ws_url(base_url: &str, model: &str, token: &str) -> Result<String, GeminiLiveError> {
    let mut url = url::Url::parse(base_url).map_err(|e| {
        GeminiLiveError::new(StatusCode::BAD_GATEWAY, format!("gemini live url: {e}"))
    })?;
    url.set_scheme("wss")
        .map_err(|_| GeminiLiveError::new(StatusCode::BAD_GATEWAY, "gemini live url scheme"))?;
    let path = url.path().to_string();
    if !path.starts_with("/ws/") {
        url.set_path(&format!("/ws/api{path}"));
    }
    let base = url.as_str().trim_end_matches('/').to_string();
    Ok(format!(
        "{base}/{model}:bidiGenerateContent?key={}",
        urlencoding::encode(token)
    ))
}

fn field<'a>(fields: &'a std::collections::BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    fields
        .get(key)
        .map(String::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn timeout_field_ms(
    fields: &std::collections::BTreeMap<String, String>,
    key: &str,
    fallback: Duration,
) -> Duration {
    let ms: u64 = field(fields, key)
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|n| *n > 0.0)
        .map(|n| (n as u64).min(MAX_TIMEOUT_MS))
        .unwrap_or_else(|| fallback.as_millis() as u64);
    Duration::from_millis(ms)
}

/// One socket generation's outcome. `GoAway` carries the audio byte offset
/// already streamed so the next generation resumes without re-upload.
/// Accumulated `text`/`chunks` live in the caller so they survive a goAway
/// rotation (JS keeps them in the outer closure).
enum Attempt {
    Done,
    /// Server sent `goAway` — rotate the socket once. Carries the audio byte
    /// offset already handed to this socket so the next generation resumes
    /// without re-upload (JS `sentBytes`), plus the reconnect delay derived
    /// from the `goAway.time` deadline (clamped to the setup timeout, JS
    /// `scheduleGoAwayReconnect`).
    GoAway {
        offset: usize,
        delay: Duration,
    },
    Failed(GeminiLiveError),
}

/// JS `scheduleGoAwayReconnect`: delay = `goAway.time - now`, clamped to
/// `[0, setupTimeoutMs]`; 0 when the timestamp is absent/unparseable.
fn goaway_delay(go_away: &Value, setup_timeout: Duration) -> Duration {
    let deadline = go_away
        .get("time")
        .and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp_millis());
    let delay_ms = deadline
        .map(|ms| ms - chrono::Utc::now().timestamp_millis())
        .unwrap_or(0)
        .clamp(0, setup_timeout.as_millis() as i64);
    Duration::from_millis(delay_ms as u64)
}

/// Run one WebSocket generation: connect → setup → stream audio → collect
/// transcription until `turnComplete`/close/goAway. Appends deltas into the
/// caller-owned `text`/`chunks`.
async fn run_attempt(
    ws_url: &str,
    model: &str,
    system_text: &str,
    mime_type: &str,
    audio: &[u8],
    sent_bytes_start: usize,
    setup_timeout: Duration,
    turn_timeout: Duration,
    text: &mut String,
    chunks: &mut Vec<String>,
) -> Attempt {
    let (mut ws, _resp) =
        match tokio::time::timeout(setup_timeout, tokio_tungstenite::connect_async(ws_url)).await {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                return Attempt::Failed(GeminiLiveError::new(
                    StatusCode::BAD_GATEWAY,
                    format!("Gemini Live websocket connection failed: {e}"),
                ));
            }
            Err(_) => {
                return Attempt::Failed(GeminiLiveError::new(
                    StatusCode::GATEWAY_TIMEOUT,
                    "Gemini Live timed out connecting",
                ));
            }
        };

    let mut sent_bytes = sent_bytes_start;
    // JS arms two phases: open→setupComplete uses setup_timeout, then
    // setupComplete→turnComplete re-arms with turn_timeout.
    let mut phase_deadline = tokio::time::Instant::now() + setup_timeout;

    // Send the setup frame immediately after connect (JS sends on `open`).
    let setup = json!({
        "setup": {
            "model": format!("models/{model}"),
            "generationConfig": {
                "responseModalities": ["TEXT"],
                "inputAudioTranscription": {},
            },
            "systemInstruction": { "parts": [{ "text": system_text }] },
        }
    });
    if ws
        .send(Message::Text(setup.to_string().into()))
        .await
        .is_err()
    {
        return Attempt::Failed(GeminiLiveError::new(
            StatusCode::BAD_GATEWAY,
            "Gemini Live socket closed while sending setup",
        ));
    }

    loop {
        let now = tokio::time::Instant::now();
        if now >= phase_deadline {
            return Attempt::Failed(GeminiLiveError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "Gemini Live transcription timed out",
            ));
        }
        let remaining = phase_deadline - now;
        let frame = match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(msg))) => msg,
            Ok(Some(Err(e))) => {
                return Attempt::Failed(GeminiLiveError::new(
                    StatusCode::BAD_GATEWAY,
                    format!("Gemini Live websocket error: {e}"),
                ));
            }
            Ok(None) => {
                // Graceful close: partial transcript beats a hard error;
                // silence is one.
                if !text.trim().is_empty() {
                    return Attempt::Done;
                }
                return Attempt::Failed(GeminiLiveError::new(
                    StatusCode::BAD_GATEWAY,
                    "Gemini Live socket closed before completion",
                ));
            }
            Err(_) => {
                return Attempt::Failed(GeminiLiveError::new(
                    StatusCode::GATEWAY_TIMEOUT,
                    "Gemini Live transcription timed out",
                ));
            }
        };

        let Message::Text(raw) = frame else {
            continue; // ping/pong/binary carry no Live API semantics
        };
        let parsed: Value = match serde_json::from_str(raw.as_str()) {
            Ok(v) => v,
            Err(_) => continue, // non-JSON frames carry no Live API semantics
        };

        if let Some(err) = parsed.get("error") {
            let status = err.get("status").and_then(Value::as_i64).unwrap_or(502);
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            return Attempt::Failed(GeminiLiveError::new(
                StatusCode::from_u16(status.clamp(400, 599) as u16)
                    .unwrap_or(StatusCode::BAD_GATEWAY),
                format!("Gemini Live error: {message}"),
            ));
        }
        if let Some(go_away) = parsed.get("goAway") {
            // Caller rotates the socket; close this one.
            let delay = goaway_delay(go_away, setup_timeout);
            let _ = ws.close(None).await;
            return Attempt::GoAway {
                offset: sent_bytes,
                delay,
            };
        }

        let Some(sc) = parsed.get("serverContent") else {
            continue;
        };

        if let Some(delta) = sc
            .pointer("/inputTranscription/text")
            .and_then(Value::as_str)
        {
            if !delta.trim().is_empty() {
                text.push_str(delta);
                chunks.push(delta.to_string());
            }
        }

        if sc.get("setupComplete").is_some() {
            // setupComplete flips the arm: re-arm with the turn timeout,
            // then stream the remaining audio + flushing text turn.
            phase_deadline = tokio::time::Instant::now() + turn_timeout;
            if !stream_audio_and_prompt(&mut ws, audio, mime_type, system_text, &mut sent_bytes)
                .await
            {
                return Attempt::Failed(GeminiLiveError::new(
                    StatusCode::BAD_GATEWAY,
                    "Gemini Live socket closed while streaming audio",
                ));
            }
            continue;
        }
        if sc.get("turnComplete").is_some() {
            return Attempt::Done;
        }
    }
}

/// Stream every byte not yet sent as base64 `realtimeInput.mediaChunks`,
/// then the flushing `clientContent` text turn (JS `streamAudioAndPrompt`).
/// Returns false when the socket dies mid-send.
async fn stream_audio_and_prompt(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    audio: &[u8],
    mime_type: &str,
    system_text: &str,
    sent_bytes: &mut usize,
) -> bool {
    let mut offset = *sent_bytes;
    while offset < audio.len() {
        let end = (offset + CHUNK_BYTES).min(audio.len());
        let b64 = base64::engine::general_purpose::STANDARD.encode(&audio[offset..end]);
        let frame = json!({
            "realtimeInput": {
                "mediaChunks": [{ "mimeType": mime_type, "data": b64 }]
            }
        });
        if ws
            .send(Message::Text(frame.to_string().into()))
            .await
            .is_err()
        {
            return false;
        }
        offset = end;
    }
    *sent_bytes = offset;
    // Final user turn: flushes the recognizer and yields turnComplete.
    let flush = json!({
        "clientContent": {
            "turns": [{ "parts": [{ "text": system_text }] }],
            "turnComplete": true,
        }
    });
    ws.send(Message::Text(flush.to_string().into()))
        .await
        .is_ok()
}

/// Transcribe an audio buffer via the Gemini Live bidirectional stream.
/// Mirrors JS `transcribeGeminiLive` including the single goAway rotation.
pub async fn transcribe_gemini_live(
    base_url: &str,
    model: &str,
    token: &str,
    audio: &[u8],
    mime_type: &str,
    fields: &std::collections::BTreeMap<String, String>,
) -> Result<GeminiLiveResult, GeminiLiveError> {
    if audio.is_empty() {
        return Err(GeminiLiveError::new(
            StatusCode::BAD_REQUEST,
            "Empty audio file",
        ));
    }
    let ws_url = to_live_ws_url(base_url, model, token)?;

    let instruction = field(fields, "prompt").unwrap_or("Transcribe the spoken audio verbatim.");
    let language = field(fields, "language").unwrap_or("");
    let setup_timeout = timeout_field_ms(fields, "setup_timeout_ms", SETUP_TIMEOUT);
    let turn_timeout = timeout_field_ms(fields, "turn_timeout_ms", TURN_TIMEOUT);
    // system_instruction overrides the built-in transcription directive
    // wholesale; prompt/language only shape the default.
    let system_text = field(fields, "system_instruction")
        .map(str::to_string)
        .unwrap_or_else(|| {
            if language.is_empty() {
                instruction.to_string()
            } else {
                format!("{instruction} Language: {language}.")
            }
        });

    let mut sent_bytes = 0usize;
    let mut text = String::new();
    let mut chunks: Vec<String> = Vec::new();
    let mut goaway_reconnects = GOAWAY_RECONNECTS;
    loop {
        match run_attempt(
            &ws_url,
            model,
            &system_text,
            mime_type,
            audio,
            sent_bytes,
            setup_timeout,
            turn_timeout,
            &mut text,
            &mut chunks,
        )
        .await
        {
            Attempt::Done => {
                return Ok(GeminiLiveResult { text, chunks });
            }
            Attempt::GoAway { offset, delay } => {
                if goaway_reconnects == 0 {
                    return Err(GeminiLiveError::new(
                        StatusCode::BAD_GATEWAY,
                        "Gemini Live socket closed before completion",
                    ));
                }
                goaway_reconnects -= 1;
                sent_bytes = offset;
                // Wait out the server-named deadline before re-dialing.
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                continue;
            }
            Attempt::Failed(err) => return Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_maps_rest_base_to_live_endpoint() {
        let url = to_live_ws_url(
            "https://generativelanguage.googleapis.com/v1beta/models",
            "gemini-2.5-flash-native-audio-preview-09-17",
            "k ey",
        )
        .unwrap();
        assert_eq!(
            url,
            "wss://generativelanguage.googleapis.com/ws/api/v1beta/models/gemini-2.5-flash-native-audio-preview-09-17:bidiGenerateContent?key=k%20ey"
        );
    }

    #[test]
    fn ws_url_does_not_double_ws_prefix() {
        let url = to_live_ws_url("https://host/ws/api/v1beta/models", "m", "t").unwrap();
        assert!(url.starts_with("wss://host/ws/api/v1beta/models/m:bidiGenerateContent"));
        assert!(!url.contains("/ws/api/ws/api"));
    }

    #[test]
    fn timeout_fields_clamp_and_default() {
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("setup_timeout_ms".to_string(), "999999".to_string());
        fields.insert("turn_timeout_ms".to_string(), "abc".to_string());
        assert_eq!(
            timeout_field_ms(&fields, "setup_timeout_ms", SETUP_TIMEOUT),
            Duration::from_millis(MAX_TIMEOUT_MS)
        );
        assert_eq!(
            timeout_field_ms(&fields, "turn_timeout_ms", TURN_TIMEOUT),
            TURN_TIMEOUT
        );
        fields.insert("turn_timeout_ms".to_string(), "5000".to_string());
        assert_eq!(
            timeout_field_ms(&fields, "turn_timeout_ms", TURN_TIMEOUT),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn goaway_delay_parses_deadline_and_clamps() {
        // Far-future deadline → clamped to setup timeout.
        let future = json!({"time": "2999-01-01T00:00:00Z"});
        assert_eq!(
            goaway_delay(&future, Duration::from_secs(10)),
            Duration::from_secs(10)
        );
        // Past/unparseable/missing → immediate reconnect.
        assert_eq!(
            goaway_delay(
                &json!({"time": "2000-01-01T00:00:00Z"}),
                Duration::from_secs(10)
            ),
            Duration::ZERO
        );
        assert_eq!(
            goaway_delay(&json!({}), Duration::from_secs(10)),
            Duration::ZERO
        );
        assert_eq!(
            goaway_delay(&json!({"time": "not-a-date"}), Duration::from_secs(10)),
            Duration::ZERO
        );
    }

    #[test]
    fn empty_audio_rejected() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(transcribe_gemini_live(
                "https://generativelanguage.googleapis.com/v1beta/models",
                "m",
                "t",
                &[],
                "audio/wav",
                &std::collections::BTreeMap::new(),
            ))
            .unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }
}
