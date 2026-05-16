//! Google Translate TTS — no auth, scrapes the translate.google.com
//! batchexecute endpoint with a rotating token. Best-effort port: token
//! caching uses a per-process RwLock<Option<Token>> instead of the JS
//! module-level singleton so calls from different tasks share the
//! refresh cycle.

use async_trait::async_trait;
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use rand::Rng;
use regex::Regex;
use reqwest::header::{HeaderValue, REFERER};
use reqwest::Client;
use serde_json::Value;
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicU64, Ordering};

use super::base::{TtsAdapter, TtsError, TtsRequest, TtsResult, UA};

const REFRESH: Duration = Duration::from_secs(11 * 60);

#[derive(Clone)]
struct Token {
    f_sid: String,
    bl: String,
    fetched: Instant,
}

static CACHE: Lazy<RwLock<Option<Token>>> = Lazy::new(|| RwLock::new(None));
static IDX: AtomicU64 = AtomicU64::new(0);

static RE_FSID: Lazy<Regex> = Lazy::new(|| Regex::new(r#""FdrFJe":"(.*?)""#).unwrap());
static RE_CFB2H: Lazy<Regex> = Lazy::new(|| Regex::new(r#""cfb2h":"(.*?)""#).unwrap());

async fn get_token(client: &Client) -> Result<Token, TtsError> {
    if let Some(t) = CACHE.read().clone() {
        if t.fetched.elapsed() < REFRESH {
            return Ok(t);
        }
    }
    let res = client
        .get("https://translate.google.com/")
        .header("User-Agent", UA)
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(TtsError::Upstream {
            status: res.status().as_u16(),
            message: format!("translate.google.com: {}", res.status()),
        });
    }
    let html = res.text().await.map_err(TtsError::from)?;
    let f_sid = RE_FSID
        .captures(&html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .ok_or_else(|| TtsError::Parse("Google: missing FdrFJe".into()))?;
    let bl = RE_CFB2H
        .captures(&html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .ok_or_else(|| TtsError::Parse("Google: missing cfb2h".into()))?;
    let token = Token {
        f_sid,
        bl,
        fetched: Instant::now(),
    };
    *CACHE.write() = Some(token.clone());
    Ok(token)
}

pub struct GoogleTtsAdapter;
pub static ADAPTER: GoogleTtsAdapter = GoogleTtsAdapter;

#[async_trait]
impl TtsAdapter for GoogleTtsAdapter {
    fn no_auth(&self) -> bool {
        true
    }

    async fn synthesize(
        &self,
        client: &Client,
        request: &TtsRequest<'_>,
    ) -> Result<TtsResult, TtsError> {
        let lang = if request.model.is_empty() {
            "en"
        } else {
            request.model
        };
        let token = get_token(client).await?;
        let clean: String = request
            .text
            .chars()
            .map(|c| match c {
                '@' | '^' | '*' | '(' | ')' | '\\' | '/' | '-' | '_' | '+' | '=' | '>' | '<'
                | '"' | '\'' | '\u{201c}' | '\u{201d}' | '\u{3010}' | '\u{3011}' => ' ',
                _ => c,
            })
            .collect();
        let clean = clean.replace(", ", ". ");
        let rpc_id = "jQ1olc";
        let req_id = (IDX.fetch_add(1, Ordering::Relaxed) + 1) * 100_000
            + rand::thread_rng().gen_range(1000..10000);
        let payload = serde_json::json!([clean, lang, Value::Null, "undefined", [0]]);
        let body = format!(
            "f.req={}",
            urlencoding::encode(&serde_json::json!([[[rpc_id, payload.to_string(), Value::Null, "generic"]]]).to_string())
        );

        let url = format!(
            "https://translate.google.com/_/TranslateWebserverUi/data/batchexecute?rpcids={rpc_id}&f.sid={fsid}&bl={bl}&hl={lang}&soc-app=1&soc-platform=1&soc-device=1&_reqid={req_id}&rt=c",
            fsid = urlencoding::encode(&token.f_sid),
            bl = urlencoding::encode(&token.bl),
            lang = lang
        );

        let res = client
            .post(&url)
            .header(
                "Content-Type",
                HeaderValue::from_static("application/x-www-form-urlencoded"),
            )
            .header(REFERER, HeaderValue::from_static("https://translate.google.com/"))
            .header("User-Agent", UA)
            .body(body)
            .send()
            .await?;
        if !res.status().is_success() {
            return Err(TtsError::Upstream {
                status: res.status().as_u16(),
                message: format!("google tts: {}", res.status()),
            });
        }
        let text = res.text().await?;
        // Response shape: prelude + JSON on line[3].
        let lines: Vec<&str> = text.split('\n').collect();
        let raw = lines
            .get(3)
            .ok_or_else(|| TtsError::Parse("Google TTS: short response".into()))?;
        let split: Value = serde_json::from_str(raw)
            .map_err(|e| TtsError::Parse(format!("google split: {e}")))?;
        let inner_str = split
            .pointer("/0/2")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TtsError::Parse("Google TTS: missing payload".into()))?;
        let inner: Value = serde_json::from_str(inner_str)
            .map_err(|e| TtsError::Parse(format!("google inner: {e}")))?;
        let b64 = inner
            .pointer("/0")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TtsError::Parse("Google TTS: empty audio".into()))?;
        if b64.len() < 100 {
            return Err(TtsError::Parse("Google TTS returned empty audio".into()));
        }
        Ok(TtsResult {
            base64: b64.to_string(),
            format: "mp3".to_string(),
        })
    }
}
