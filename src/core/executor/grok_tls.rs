//! TLS-impersonating transport for grok.com — Rust port of
//! `open-sse/services/grokTlsClient.js` (the wreq-js `chrome_146`/`macos`
//! session).
//!
//! Cloudflare Enterprise in front of grok.com rejects non-browser TLS
//! fingerprints (JA3/JA4 + HTTP/2 SETTINGS) even with valid
//! `sso`+`sso-rw`+`cf_clearance` cookies, so chat, media turns, and
//! `assets.grok.com` downloads go through `wreq` with the same emulation
//! profile upstream selects via `createSession({ browser: "chrome_146",
//! os: "macos" })`. Upstream treats the TLS client as optional and hard-fails
//! with `GrokTlsUnavailableError` when wreq-js is absent; here wreq is a
//! compiled-in dependency, so the impersonated path is always available.
//!
//! Clients are cached per proxy target (keyed like [`super::ClientPool`])
//! because `wreq::Client` owns its connection pool — rebuilding per request
//! would drop keep-alive reuse.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use dashmap::DashMap;
use hyper::http::{HeaderMap, Method};
use wreq_util::{Emulation, Platform, Profile};

use crate::core::proxy::ProxyTarget;

/// Upstream chat/media fetch timeout (`grokTlsFetch` `timeoutMs`).
pub const GROK_TLS_TIMEOUT: Duration = Duration::from_secs(120);

/// Buffered response — upstream `grokTlsFetch` buffers the body
/// (`res.arrayBuffer()`) before returning, so callers always see the full
/// payload.
pub struct GrokTlsResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: bytes::Bytes,
}

impl GrokTlsResponse {
    /// UTF-8-lossy body decode — equivalent to upstream `.text()` on the
    /// buffered ArrayBuffer (invalid sequences become U+FFFD either way).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

static CLIENTS: LazyLock<DashMap<String, Arc<wreq::Client>>> = LazyLock::new(DashMap::new);

fn client_key(proxy: Option<&ProxyTarget>) -> String {
    match proxy {
        Some(proxy) if !proxy.url.is_empty() => format!(
            "{}|{}|{}",
            proxy.url,
            proxy.no_proxy,
            proxy.pool_id.as_deref().unwrap_or_default()
        ),
        _ => "default".to_string(),
    }
}

/// Shared wreq client impersonating `chrome_146` on macOS — the Rust
/// equivalent of the lazily-created wreq-js session in
/// `services/grokTlsClient.js`. One client per proxy target.
pub fn client(proxy: Option<&ProxyTarget>) -> Result<Arc<wreq::Client>, wreq::Error> {
    let key = client_key(proxy);
    if let Some(cached) = CLIENTS.get(&key) {
        return Ok(cached.clone());
    }

    // chrome_146 + macOS — same profile/os pair as upstream
    // `createSession({ browser: "chrome_146", os: "macos" })`. The emulation
    // supplies TLS fingerprint, HTTP/2 settings, header order, and default
    // browser headers; request-level headers still override per-field.
    let emulation = Emulation::builder()
        .profile(Profile::Chrome146)
        .platform(Platform::MacOS)
        .build();

    let mut builder = wreq::Client::builder()
        .emulation(emulation)
        // wreq defaults to NOT following redirects; upstream passes
        // `redirect: "follow"` to grokTlsFetch — match it (bounded at 10,
        // same as reqwest's default).
        .redirect(wreq::redirect::Policy::limited(10))
        .connect_timeout(super::client_pool::DEFAULT_CONNECT_TIMEOUT)
        .pool_max_idle_per_host(super::client_pool::CLIENT_POOL_MAX_IDLE_PER_HOST);

    if let Some(proxy) = proxy {
        if !proxy.url.is_empty() {
            let proxy =
                wreq::Proxy::all(&proxy.url)?.no_proxy(wreq::NoProxy::from_string(&proxy.no_proxy));
            builder = builder.proxy(proxy);
        }
    }

    let client = Arc::new(builder.build()?);
    // Benign race: two builders may both construct; the last insert wins and
    // both clients are valid (identical config).
    CLIENTS.insert(key, client.clone());
    Ok(client)
}

/// Single-shot buffered request through the impersonated client — the
/// `grokTlsFetch(url, { method, headers, body, timeoutMs })` port. `body` is
/// sent verbatim (already-serialized JSON or empty for GET/HEAD).
pub async fn request(
    method: Method,
    url: &str,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
    timeout: Duration,
    proxy: Option<&ProxyTarget>,
) -> Result<GrokTlsResponse, wreq::Error> {
    let client = client(proxy)?;
    let mut req = client
        .request(method, url)
        .headers(headers)
        .timeout(timeout);
    if let Some(body) = body {
        req = req.body(body);
    }
    let resp = req.send().await?;
    let content_type = resp
        .headers()
        .get(hyper::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.split(';').next().unwrap_or(ct).trim().to_string());
    let status = resp.status().as_u16();
    let body = resp.bytes().await?;
    Ok(GrokTlsResponse {
        status,
        content_type,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_key_distinguishes_proxy_targets() {
        assert_eq!(client_key(None), "default");
        assert_eq!(
            client_key(Some(&ProxyTarget {
                url: "http://127.0.0.1:7890".into(),
                no_proxy: String::new(),
                strict_proxy: false,
                pool_id: None,
                label: None,
                rtt_ms: None,
            })),
            "http://127.0.0.1:7890||"
        );
    }

    #[tokio::test]
    async fn wreq_client_builds_with_chrome146_emulation() {
        // Construction exercises the emulation profile (chrome_146 + macOS)
        // end-to-end; a live handshake is covered by manual grok-web runs.
        let first = client(None).expect("wreq client builds");
        let second = client(None).expect("cached wreq client");
        assert!(Arc::ptr_eq(&first, &second));
    }
}
