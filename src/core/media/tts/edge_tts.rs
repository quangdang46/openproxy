//! Microsoft Edge / Bing TTS — no auth, scrapes the Bing translator
//! endpoint with a short-lived token.

use async_trait::async_trait;
use base64::Engine as _;
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use regex::Regex;
use reqwest::header::HeaderValue;
use reqwest::Client;
use std::time::{Duration, Instant};

use super::base::{TtsAdapter, TtsError, TtsRequest, TtsResult, UA};

const REFRESH: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
struct Token {
    key: String,
    token: String,
    cookie: String,
    fetched: Instant,
}

static CACHE: Lazy<RwLock<Option<Token>>> = Lazy::new(|| RwLock::new(None));

static RE_TOKEN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"params_AbusePreventionHelper\s*=\s*\[([^,]+),([^,]+),").unwrap()
});

async fn fetch_token(client: &Client) -> Result<Token, TtsError> {
    let res = client
        .get("https://www.bing.com/translator")
        .header("User-Agent", UA)
        .header("Accept-Language", "vi,en-US;q=0.9,en;q=0.8")
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(TtsError::Upstream {
            status: res.status().as_u16(),
            message: "bing translator".into(),
        });
    }
    // Reqwest doesn't expose getSetCookie in stable; collect Set-Cookie headers
    // manually.
    let cookie = res
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|s| s.split(';').next())
        .collect::<Vec<_>>()
        .join("; ");
    let html = res.text().await?;
    let caps = RE_TOKEN
        .captures(&html)
        .ok_or_else(|| TtsError::Parse("Bing: missing token".into()))?;
    let key = caps
        .get(1)
        .map(|m| m.as_str().trim().to_string())
        .unwrap_or_default();
    let token = caps
        .get(2)
        .map(|m| m.as_str().trim().trim_matches('"').to_string())
        .unwrap_or_default();
    Ok(Token {
        key,
        token,
        cookie,
        fetched: Instant::now(),
    })
}

async fn get_token(client: &Client) -> Result<Token, TtsError> {
    if let Some(t) = CACHE.read().clone() {
        if t.fetched.elapsed() < REFRESH {
            return Ok(t);
        }
    }
    let token = fetch_token(client).await?;
    *CACHE.write() = Some(token.clone());
    Ok(token)
}

pub struct EdgeTtsAdapter;
pub static ADAPTER: EdgeTtsAdapter = EdgeTtsAdapter;

#[async_trait]
impl TtsAdapter for EdgeTtsAdapter {
    fn no_auth(&self) -> bool {
        true
    }

    async fn synthesize(
        &self,
        client: &Client,
        request: &TtsRequest<'_>,
    ) -> Result<TtsResult, TtsError> {
        let voice_id = if request.model.is_empty() {
            "vi-VN-HoaiMyNeural".to_string()
        } else {
            request.model.to_string()
        };

        let mut token = get_token(client).await?;
        let mut res = post_tts(client, request.text, &voice_id, &token).await?;

        if res.status().as_u16() == 429 || res.status().as_u16() == 403 {
            *CACHE.write() = None;
            token = get_token(client).await?;
            res = post_tts(client, request.text, &voice_id, &token).await?;
        }

        if !res.status().is_success() {
            let status = res.status().as_u16();
            let body = res.text().await.unwrap_or_default();
            return Err(TtsError::Upstream {
                status,
                message: if body.is_empty() {
                    "bing tts".into()
                } else {
                    body
                },
            });
        }

        let bytes = res.bytes().await?;
        if bytes.len() < 1024 {
            return Err(TtsError::Parse("Bing TTS returned empty audio".into()));
        }
        Ok(TtsResult {
            base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            format: "mp3".to_string(),
        })
    }
}

async fn post_tts(
    client: &Client,
    text: &str,
    voice_id: &str,
    token: &Token,
) -> Result<reqwest::Response, TtsError> {
    let mut parts = voice_id.split('-');
    let lang = parts
        .next()
        .map(str::to_string)
        .unwrap_or_else(|| "en".to_string());
    let region = parts
        .next()
        .map(str::to_string)
        .unwrap_or_else(|| "US".to_string());
    let xml_lang = format!("{lang}-{region}");
    let gender = if voice_id.to_lowercase().contains("male") {
        "Male"
    } else {
        "Female"
    };
    let ssml = format!(
        "<speak version='1.0' xml:lang='{xml_lang}'><voice xml:lang='{xml_lang}' xml:gender='{gender}' name='{voice_id}'><prosody rate='0.00%'>{text}</prosody></voice></speak>"
    );
    let body = format!(
        "ssml={}&token={}&key={}",
        urlencoding::encode(&ssml),
        urlencoding::encode(&token.token),
        urlencoding::encode(&token.key),
    );

    let mut req = client
        .post("https://www.bing.com/tfettts?isVertical=1&&IG=1&IID=translator.5023&SFX=1")
        .header(
            "Content-Type",
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        )
        .header("Accept", HeaderValue::from_static("*/*"))
        .header("Origin", HeaderValue::from_static("https://www.bing.com"))
        .header(
            "Referer",
            HeaderValue::from_static("https://www.bing.com/translator"),
        )
        .header("User-Agent", UA)
        .body(body);
    if !token.cookie.is_empty() {
        req = req.header(
            "Cookie",
            HeaderValue::from_str(&token.cookie).map_err(|e| TtsError::Parse(e.to_string()))?,
        );
    }
    let res = req.send().await?;
    Ok(res)
}
