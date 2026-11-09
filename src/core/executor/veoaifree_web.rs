//! veoaifree.com free media generator — WordPress AJAX API, no auth.
//! Port of 9router `open-sse/executors/veoaifree-web.js` (itself ported from
//! OmniRoute). Rate-limited (~6 req/hour per IP).
//!
//! Image: `banan-image-generator` returns comma-separated URLs or raw
//! base64. Video: `full-video-generate` → `final-video-results` polling →
//! MP4 artifact download with manual redirect following and per-hop
//! SSRF checks (HTTPS only, public hosts only, ≤100MB, `ftyp` magic).

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::Duration;

use base64::Engine as _;
use reqwest::header::{HeaderMap, COOKIE, CONTENT_TYPE, USER_AGENT};

use crate::core::proxy::ProxyTarget;
use crate::types::ProviderConnection;

use super::uc_web::UcMediaAsset;
use super::ClientPool;

const BASE_URL: &str = "https://veoaifree.com";
const AJAX_URL: &str = "https://veoaifree.com/wp-admin/admin-ajax.php";
const USER_AGENT_VALUE: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36";
const POLL_INTERVAL_MS: u64 = 20_000;
const MAX_POLLS: u32 = 30; // 10 minutes
const FETCH_TIMEOUT_MS: u64 = 30_000;
const ARTIFACT_MAX_BYTES: usize = 100 * 1024 * 1024;
const ARTIFACT_MAX_REDIRECTS: u32 = 3;
const ARTIFACT_READY_MAX_ATTEMPTS: u32 = 12;
const ARTIFACT_READY_RETRY_INTERVAL_MS: u64 = 5_000;
const NONCE_RE: &str = "nonce\":\"";

#[derive(Debug)]
pub struct VeoError {
    pub status: u16,
    pub retryable: bool,
    pub message: String,
}

impl VeoError {
    fn new(status: u16, message: impl Into<String>) -> Self {
        Self { status, retryable: false, message: message.into() }
    }
    fn retryable(message: impl Into<String>) -> Self {
        Self { status: 502, retryable: true, message: message.into() }
    }
}

impl std::fmt::Display for VeoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Session context — accumulates Set-Cookie pairs across calls (upstream
/// `buildContext`). The WP nonce session cookies matter for subsequent AJAX.
struct Session {
    cookie_jar: BTreeMap<String, String>,
}

impl Session {
    fn cookie_header(&self) -> String {
        self.cookie_jar
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
    fn merge_set_cookies(&mut self, headers: &HeaderMap) {
        for value in headers.get_all(reqwest::header::SET_COOKIE) {
            let Ok(entry) = value.to_str() else { continue };
            let pair = entry.split(';').next().unwrap_or("").trim();
            if let Some(eq) = pair.find('=') {
                let (name, val) = (pair[..eq].trim(), pair[eq + 1..].trim());
                if !name.is_empty() && !val.is_empty() {
                    self.cookie_jar.insert(name.to_string(), val.to_string());
                }
            }
        }
    }
}

fn map_aspect_ratio(size: Option<&str>, prefix: &str) -> String {
    let value = size.unwrap_or("").to_lowercase();
    if value.contains("9:16") || value.contains("portrait") {
        format!("{prefix}_PORTRAIT")
    } else if value.contains("1:1") || value.contains("square") {
        format!("{prefix}_SQUARE")
    } else {
        format!("{prefix}_LANDSCAPE") // default 16:9
    }
}

/// SSRF guard — upstream `assertPublicHost`: HTTPS only, literal private IPs
/// and hostnames resolving to private/loopback/link-local addresses are
/// rejected. Redirects are followed manually and each hop is re-checked.
async fn assert_public_host(hostname: &str) -> Result<(), VeoError> {
    let bare = hostname
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(hostname);
    let blocked = |addr: IpAddr| -> bool {
        // is_private_ip takes &str — covers v4 ranges, loopback, ULA, link-local.
        crate::core::dns::is_private_ip(&addr.to_string())
    };
    if let Ok(ip) = bare.parse::<IpAddr>() {
        if blocked(ip) {
            return Err(VeoError::new(400, "artifact host is a blocked address"));
        }
        return Ok(());
    }
    // DNS resolution — check every A/AAAA answer.
    let port = 443u16;
    let addrs: Vec<IpAddr> = match tokio::net::lookup_host((bare, port)).await {
        Ok(iter) => iter.map(|sa| sa.ip()).collect(),
        Err(_) => {
            return Err(VeoError::new(400, "artifact host could not be resolved"));
        }
    };
    if addrs.is_empty() {
        return Err(VeoError::new(400, "artifact host could not be resolved"));
    }
    for addr in addrs {
        if blocked(addr) {
            return Err(VeoError::new(400, "artifact host resolves to a blocked address"));
        }
    }
    Ok(())
}

pub struct VeoAiFreeWebExecutor {
    pool: std::sync::Arc<ClientPool>,
}

impl VeoAiFreeWebExecutor {
    pub fn new(pool: std::sync::Arc<ClientPool>) -> Self {
        Self { pool }
    }

    fn client(&self, proxy: Option<&ProxyTarget>) -> Result<reqwest::Client, VeoError> {
        Ok(self
            .pool
            .get("veoaifree-web", proxy)
            .map_err(|e| VeoError::new(502, e.to_string()))?
            .as_ref()
            .clone())
    }

    /// Artifact downloads follow redirects manually (per-hop SSRF checks), so
    /// they need a client that never follows them.
    fn client_no_redirect(&self, proxy: Option<&ProxyTarget>) -> Result<reqwest::Client, VeoError> {
        Ok(self
            .pool
            .get_no_redirect("veoaifree-web", proxy)
            .map_err(|e| VeoError::new(502, e.to_string()))?
            .as_ref()
            .clone())
    }

    async fn session_fetch(
        &self,
        client: &reqwest::Client,
        session: &mut Session,
        url: &str,
        init: RequestInit,
    ) -> Result<reqwest::Response, VeoError> {
        let mut req = match init.method.as_str() {
            "POST" => client.post(url),
            "GET" => client.get(url),
            _ => client.get(url),
        };
        req = req
            .timeout(Duration::from_millis(FETCH_TIMEOUT_MS))
            .header(USER_AGENT, USER_AGENT_VALUE)
            .header(reqwest::header::REFERER, format!("{BASE_URL}/"));
        if !session.cookie_jar.is_empty() {
            req = req.header(COOKIE, session.cookie_header());
        }
        if let Some(ct) = init.content_type {
            req = req.header(CONTENT_TYPE, ct);
        }
        if let Some(origin) = init.origin {
            req = req.header(reqwest::header::ORIGIN, origin);
        }
        if let Some(body) = init.body {
            req = req.body(body);
        }
        // redirect: manual — artifact downloads follow hops with SSRF checks;
        // session calls never redirect (fetch() default follows but upstream
        // uses same-origin endpoints; keep manual=false behavior via reqwest
        // default policy for session calls).
        let res = req
            .send()
            .await
            .map_err(|e| VeoError::new(502, e.to_string()))?;
        session.merge_set_cookies(res.headers());
        Ok(res)
    }

    async fn fetch_nonce(&self, client: &reqwest::Client, session: &mut Session) -> Result<String, VeoError> {
        let res = self.session_fetch(client, session, BASE_URL, RequestInit::default()).await?;
        let html = res.text().await.map_err(|e| VeoError::new(502, e.to_string()))?;
        let Some(pos) = html.find(NONCE_RE) else {
            return Err(VeoError::new(
                502,
                "failed to extract CSRF nonce from veoaifree.com (site layout changed or rate limited)",
            ));
        };
        let rest = &html[pos + NONCE_RE.len()..];
        let end = rest.find('"').unwrap_or(0);
        let nonce = &rest[..end];
        if nonce.is_empty() || !nonce.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(VeoError::new(
                502,
                "failed to extract CSRF nonce from veoaifree.com (site layout changed or rate limited)",
            ));
        }
        Ok(nonce.to_string())
    }

    async fn post_ajax(
        &self,
        client: &reqwest::Client,
        session: &mut Session,
        nonce: &str,
        params: &[(&str, String)],
    ) -> Result<String, VeoError> {
        let mut body = format!("action=veo_video_generator&nonce={nonce}");
        for (k, v) in params {
            body.push('&');
            body.push_str(&form_encode(k));
            body.push('=');
            body.push_str(&form_encode(v));
        }
        let res = self
            .session_fetch(
                client,
                session,
                AJAX_URL,
                RequestInit {
                    method: "POST".into(),
                    content_type: Some("application/x-www-form-urlencoded".into()),
                    origin: Some(BASE_URL.into()),
                    body: Some(body),
                    ..Default::default()
                },
            )
            .await?;
        res.text().await.map_err(|e| VeoError::new(502, e.to_string()))
    }

    /// `downloadArtifact` — manual redirects (≤3), per-hop HTTPS + public
    /// host check, retryable-status signalling, MP4 magic validation, ≤100MB.
    async fn download_artifact(
        &self,
        client: &reqwest::Client,
        session: &mut Session,
        artifact_url: &str,
    ) -> Result<Vec<u8>, VeoError> {
        let mut current = artifact_url.to_string();
        for _redirect in 0..=ARTIFACT_MAX_REDIRECTS {
            let parsed = url::Url::parse(&current)
                .map_err(|_| VeoError::new(400, "artifact URL must use HTTPS"))?;
            if parsed.scheme() != "https" {
                return Err(VeoError::new(400, "artifact URL must use HTTPS"));
            }
            let host = parsed
                .host_str()
                .ok_or_else(|| VeoError::new(400, "artifact URL must use HTTPS"))?;
            assert_public_host(host).await?;

            // Manual redirect following: this client never follows redirects,
            // so the 3xx hop is visible below and each hop gets an SSRF check.
            let mut req = client
                .get(&current)
                .timeout(Duration::from_millis(FETCH_TIMEOUT_MS))
                .header(USER_AGENT, USER_AGENT_VALUE)
                .header(reqwest::header::REFERER, format!("{BASE_URL}/"));
            if !session.cookie_jar.is_empty() {
                req = req.header(COOKIE, session.cookie_header());
            }
            let res = req.send().await.map_err(|e| VeoError::new(502, e.to_string()))?;
            session.merge_set_cookies(res.headers());
            let status = res.status().as_u16();
            if (300..400).contains(&status) {
                let location = res
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| VeoError::new(502, "artifact redirect is missing a destination"))?;
                current = parsed
                    .join(location)
                    .map_err(|_| VeoError::new(502, "artifact redirect is missing a destination"))?
                    .to_string();
                continue;
            }
            if matches!(status, 202 | 404 | 409 | 425) {
                return Err(VeoError::retryable("artifact not available yet"));
            }
            if !(200..300).contains(&status) {
                return Err(VeoError::new(status, format!("artifact download failed (HTTP {status})")));
            }
            let content_type = res
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_lowercase();
            if content_type != "video/mp4" && content_type != "application/octet-stream" {
                return Err(VeoError::new(
                    502,
                    format!("artifact returned unsupported content type ({content_type})"),
                ));
            }
            let bytes = res
                .bytes()
                .await
                .map_err(|e| VeoError::new(502, e.to_string()))?;
            if bytes.len() > ARTIFACT_MAX_BYTES {
                return Err(VeoError::new(502, "artifact exceeds 100MB limit"));
            }
            if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
                return Err(VeoError::new(502, "artifact is not a valid MP4 file"));
            }
            return Ok(bytes.to_vec());
        }
        Err(VeoError::new(502, "artifact exceeded the redirect limit"))
    }

    async fn download_artifact_with_retry(
        &self,
        client: &reqwest::Client,
        session: &mut Session,
        url: &str,
    ) -> Result<Vec<u8>, VeoError> {
        let mut last_error = VeoError::new(502, "artifact did not become available in time");
        for _ in 0..ARTIFACT_READY_MAX_ATTEMPTS {
            match self.download_artifact(client, session, url).await {
                Ok(bytes) => return Ok(bytes),
                Err(e) if e.retryable => {
                    last_error = e;
                    tokio::time::sleep(Duration::from_millis(ARTIFACT_READY_RETRY_INTERVAL_MS))
                        .await;
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_error)
    }

    /// `generateMedia` — no credentials required (free endpoint).
    #[allow(clippy::too_many_arguments)]
    pub async fn generate_media(
        &self,
        prompt: &str,
        kind: &str,
        size: Option<&str>,
        _model: Option<&str>,
        _duration: Option<f64>,
        _resolution: Option<&str>,
        _images: &[String],
        _credentials: &ProviderConnection,
        proxy: Option<&ProxyTarget>,
    ) -> Result<Vec<UcMediaAsset>, VeoError> {
        if kind != "video" && kind != "image" {
            return Err(VeoError::new(400, "veoaifree-web supports image and video generation only"));
        }
        if prompt.trim().is_empty() {
            return Err(VeoError::new(400, "prompt is required"));
        }
        let client = self.client(proxy)?;
        let mut session = Session { cookie_jar: BTreeMap::new() };
        let nonce = self.fetch_nonce(&client, &mut session).await?;

        if kind == "image" {
            let result = self
                .post_ajax(
                    &client,
                    &mut session,
                    &nonce,
                    &[
                        ("promptIMG", prompt.to_string()),
                        ("totalVariationsIMG", "1".to_string()),
                        ("aspectRatioIMG", map_aspect_ratio(size, "IMAGE_ASPECT_RATIO")),
                        ("actionType", "banan-image-generator".to_string()),
                    ],
                )
                .await?;
            let trimmed = result.trim();
            if trimmed.is_empty()
                || trimmed == "0"
                || trimmed.to_lowercase().contains("error")
            {
                return Err(VeoError::new(
                    502,
                    format!("veoaifree image generation failed: {}", trimmed.chars().take(120).collect::<String>()),
                ));
            }
            let assets = trimmed
                .split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(|part| {
                    if part.to_lowercase().starts_with("http") {
                        UcMediaAsset { b64: String::new(), mime: "image/png".into(), url: Some(part.to_string()) }
                    } else {
                        UcMediaAsset {
                            b64: part.to_string(),
                            mime: "image/png".into(),
                            url: None,
                        }
                    }
                })
                .collect();
            return Ok(assets);
        }

        // Video: submit → poll final-video-results → download artifact.
        let gen_result = self
            .post_ajax(
                &client,
                &mut session,
                &nonce,
                &[
                    ("prompt", prompt.to_string()),
                    ("totalVariations", "1".to_string()),
                    ("aspectRatio", map_aspect_ratio(size, "VIDEO_ASPECT_RATIO")),
                    ("actionType", "full-video-generate".to_string()),
                ],
            )
            .await?;
        let scene_data = gen_result.trim().to_string();
        if scene_data.is_empty()
            || scene_data == "0"
            || scene_data.to_lowercase().contains("error")
        {
            return Err(VeoError::new(
                502,
                format!("veoaifree video submit failed: {}", scene_data.chars().take(120).collect::<String>()),
            ));
        }

        for poll in 0..MAX_POLLS {
            tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
            let poll_result = self
                .post_ajax(
                    &client,
                    &mut session,
                    &nonce,
                    &[
                        ("sceneData", scene_data.clone()),
                        ("actionType", "final-video-results".to_string()),
                    ],
                )
                .await?;
            let poll_result = poll_result.trim();
            if poll_result.is_empty()
                || poll_result == "0"
                || poll_result.to_lowercase().contains("error")
            {
                continue;
            }
            let url = poll_result
                .split([',', '\n'])
                .map(str::trim)
                .find(|part| part.to_lowercase().starts_with("http"));
            let Some(url) = url else { continue };
            let dl_client = self.client_no_redirect(proxy)?;
            let buffer = self
                .download_artifact_with_retry(&dl_client, &mut session, url)
                .await?;
            tracing::info!(
                "VEOAI-FREE video ready after {} poll(s)",
                poll + 1
            );
            return Ok(vec![UcMediaAsset {
                b64: base64::engine::general_purpose::STANDARD.encode(buffer),
                mime: "video/mp4".into(),
                url: None,
            }]);
        }
        Err(VeoError::new(
            502,
            "veoaifree video generation timed out after 10 minutes",
        ))
    }
}

struct RequestInit {
    method: String,
    content_type: Option<String>,
    origin: Option<String>,
    body: Option<String>,
}

impl Default for RequestInit {
    fn default() -> Self {
        Self {
            method: "GET".into(),
            content_type: None,
            origin: None,
            body: None,
        }
    }
}

fn form_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
