//! UC (uncensored.com) persona-web provider — image + video via the
//! un-metered internal API. Port of 9router `open-sse/executors/uc-web.js`.
//!
//! Credential: the full uncensored.com cookie header (must include the
//! durable Clerk `__client` cookie). A short-lived `__session` JWT is minted
//! from Clerk per request; the account uid comes from the minted token's
//! claims. Pure HTTP — no browser.
//!
//! Clerk rotates `__client` on every `/v1/client` call; the new value is
//! returned to the caller (`rotated_cookie_header`) so the connection can
//! be persisted instead of letting the rotation chain drift.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};

use crate::core::proxy::ProxyTarget;
use crate::types::ProviderConnection;

use super::ClientPool;

const UC_ORIGIN: &str = "https://uncensored.com";
const UC_CLERK_FAPI: &str = "https://clerk.uncensored.com";
const UC_CLERK_JS_VERSION: &str = "5.127.1";
const UC_IMAGE_URL: &str = "https://internal.chatuncensored.ai/v2/image-gen";
const UC_VIDEO_URL: &str = "https://internal.chatuncensored.ai/image_to_video";
const UC_SIGNED_URL: &str = "https://internal-6.pubyar.com/generate-signed-url";
const POLL_INTERVAL_MS: u64 = 5000;
const POLL_TIMEOUT_MS: u64 = 300_000;

const UC_IMAGE_MODE: &str = "dev";
const UC_MODERATION_MODE: &str = "SUPER_LIGHT";
const UC_COUNTRY: &str = "US";
const UC_DEFAULT_IMAGE_MODEL: &str = "seedream-v4.5";
const UC_DEFAULT_VIDEO_MODEL: &str = "default";

const WEB_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/155.0.0.0 Safari/537.36";

#[derive(Debug)]
pub struct UcWebError {
    pub status: u16,
    pub message: String,
}

impl UcWebError {
    fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, message)
    }
    fn auth(message: impl Into<String>) -> Self {
        Self::new(401, message)
    }
}

impl std::fmt::Display for UcWebError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// aspect_ratio → pixel size accepted by /v2/image-gen for the generic models.
fn aspect_size(aspect: &str) -> Option<(&'static str, &'static str)> {
    Some(match aspect {
        "1:1" => ("1024", "1024"),
        "16:9" => ("1024", "576"),
        "9:16" => ("576", "1024"),
        "4:3" => ("1024", "768"),
        "3:4" => ("768", "1024"),
        "3:2" => ("1152", "768"),
        "2:3" => ("768", "1152"),
        "4:5" => ("896", "1120"),
        "5:4" => ("1120", "896"),
        "21:9" => ("1344", "576"),
        _ => return None,
    })
}

struct ImageModelSpec {
    /// `model_version` sent upstream (defaults to the registry id).
    upstream: Option<&'static str>,
    aspects: &'static [&'static str],
    resolutions: &'static [&'static str],
    qualities: &'static [&'static str],
}

const ANY_ASPECTS: &[&str] = &[];
const RES_1K_2K: &[&str] = &["1k", "2k"];

fn image_model_spec(id: &str) -> Option<ImageModelSpec> {
    use ImageModelSpec as S;
    let spec = match id {
        "seedream-v4.5" => S {
            upstream: None,
            aspects: &["1:1", "16:9", "9:16", "4:3", "3:4"],
            resolutions: &[],
            qualities: &[],
        },
        "seedream-v5" => S {
            upstream: None,
            aspects: &["1:1", "16:9", "9:16", "4:3", "3:4", "3:2", "2:3"],
            resolutions: &[],
            qualities: &[],
        },
        "seedream-v5-flash" => S {
            upstream: None,
            aspects: &[
                "1:1", "16:9", "9:16", "4:3", "3:4", "3:2", "2:3", "4:5", "5:4", "21:9",
            ],
            resolutions: &["1k", "1.5k", "2k"],
            qualities: &[],
        },
        "nano-banana-pro-ultra" => S {
            upstream: None,
            aspects: &[
                "1:1", "3:2", "2:3", "3:4", "4:3", "4:5", "5:4", "9:16", "16:9", "21:9",
            ],
            resolutions: &["4k", "8k"],
            qualities: &[],
        },
        "nano-banana-pro" => S {
            upstream: None,
            aspects: &[
                "1:1", "2:3", "3:2", "3:4", "4:3", "4:5", "5:4", "9:16", "16:9", "21:9",
            ],
            resolutions: &["1k", "2k", "4k"],
            qualities: &[],
        },
        "nano-banana" => S {
            upstream: None,
            aspects: &[
                "1:1", "9:16", "16:9", "3:4", "4:3", "3:2", "2:3", "5:4", "4:5", "21:9",
            ],
            resolutions: &[],
            qualities: &[],
        },
        "nano-banana-2" => S {
            upstream: None,
            aspects: &[
                "1:1", "3:2", "2:3", "3:4", "4:3", "4:5", "5:4", "9:16", "16:9", "21:9",
            ],
            resolutions: &["1k", "2k", "4k"],
            qualities: &[],
        },
        "gpt-image" => S {
            upstream: None,
            aspects: &["1:1", "2:3", "3:2"],
            resolutions: &[],
            qualities: &["medium", "high"],
        },
        "gpt-image-2" => S {
            upstream: None,
            aspects: &[
                "1:1", "3:2", "2:3", "3:4", "4:3", "4:5", "5:4", "9:16", "16:9", "21:9",
            ],
            resolutions: &[],
            qualities: &[],
        },
        "gpt-image-2.5-flare" | "gpt-image-2.5-sunburst" => S {
            upstream: None,
            aspects: &[
                "1:1", "3:2", "2:3", "3:4", "4:3", "4:5", "5:4", "9:16", "16:9", "21:9", "2:1",
                "1:2", "3:1", "1:3", "9:21",
            ],
            resolutions: &[],
            qualities: &[],
        },
        "wan-2.6" => S {
            upstream: Some("wan-2.6"),
            aspects: &["1:1", "4:3", "3:4", "3:2", "2:3"],
            resolutions: &[],
            qualities: &[],
        },
        // Registry id is disambiguated from the video model of the same upstream name.
        "wan-2.6-image" => S {
            upstream: Some("wan-2.6"),
            aspects: &["1:1", "4:3", "3:4", "3:2", "2:3"],
            resolutions: &[],
            qualities: &[],
        },
        "wan-2.7-text-to-image" => S {
            upstream: None,
            aspects: &["1:1", "16:9", "9:16", "4:3", "3:4", "3:2", "2:3"],
            resolutions: &[],
            qualities: &[],
        },
        "wan-2.7-text-to-image-pro" => S {
            upstream: None,
            aspects: &["1:1", "16:9", "9:16", "4:3", "3:4", "3:2", "2:3"],
            resolutions: &[],
            qualities: &[],
        },
        "prefect-pony-xl" => S {
            upstream: None,
            aspects: &["1:1", "16:9", "9:16", "4:3", "3:4", "3:2", "2:3"],
            resolutions: &[],
            qualities: &[],
        },
        "1.0" | "1.1" | "1.2" | "2.0" | "flux-2-max" | "SDXL" | "realism" | "realism-2"
        | "z-image-turbo" => S {
            upstream: None,
            aspects: ANY_ASPECTS,
            resolutions: &[],
            qualities: &[],
        },
        _ => return None,
    };
    Some(spec)
}

struct VideoModelSpec {
    aspects: &'static [&'static str],
    resolutions: &'static [&'static str],
    seconds: &'static [i64],
    t2v: bool,
    i2v: bool,
    audio: bool,
}

fn video_model_spec(id: &str) -> Option<VideoModelSpec> {
    const DEF: &[&str] = &["auto", "16:9", "9:16", "1:1"];
    const RES: &[&str] = &["480p", "720p"];
    const SEC58: &[i64] = &[5, 8];
    const SEC4812: &[i64] = &[4, 8, 12];
    let spec = match id {
        "default" | "turbo" | "wan-2.2-uncensored-v2" | "wan-2.2-uncensored" => VideoModelSpec {
            aspects: DEF,
            resolutions: RES,
            seconds: SEC58,
            t2v: true,
            i2v: true,
            audio: false,
        },
        "wan-2.6" => VideoModelSpec {
            aspects: DEF,
            resolutions: &[],
            seconds: SEC58,
            t2v: true,
            i2v: true,
            audio: false,
        },
        "wan-2.6-ultra"
        | "wan-2.7"
        | "wan-2.7-pro"
        | "wan-2.7-uncensored"
        | "wan-2.7-uncensored-v2" => VideoModelSpec {
            aspects: &[],
            resolutions: &[],
            seconds: &[],
            t2v: true,
            i2v: true,
            audio: false,
        },
        "wan-3.0" | "wan-3.0-uncensored" | "wan-3.0-uncensored-pro" => VideoModelSpec {
            aspects: &[],
            resolutions: &[],
            seconds: &[],
            t2v: true,
            i2v: true,
            audio: true,
        },
        "sora-2" | "sora-2-pro" => VideoModelSpec {
            aspects: &[],
            resolutions: &[],
            seconds: SEC4812,
            t2v: true,
            i2v: true,
            audio: false,
        },
        "grok-imagine-video"
        | "bytedance-seedance-2.0"
        | "seedance-2.0-fast"
        | "seedance-2.0-uncensored"
        | "seedance-2.0-fast-uncensored"
        | "seedance-2.0-mini-uncensored"
        | "seedance-2.0-mini-turbo"
        | "seedance-v1.5-pro-uncensored"
        | "seedance-2.5-uncensored"
        | "seedance-2.5"
        | "vidu-q3-uncensored" => VideoModelSpec {
            aspects: &[],
            resolutions: &[],
            seconds: &[],
            t2v: true,
            i2v: true,
            audio: id.contains("uncensored")
                || id == "seedance-2.5"
                || id == "seedance-2.5-uncensored"
                || id == "seedance-v1.5-pro-uncensored",
        },
        "av-gen-v2" | "cosmos-predict" | "minimax-h3" | "happyhorse-1.0" | "wan/v2.2-a14b" => {
            VideoModelSpec {
                aspects: &[],
                resolutions: &[],
                seconds: &[],
                t2v: true,
                i2v: true,
                audio: id == "av-gen-v2",
            }
        }
        _ => return None,
    };
    Some(spec)
}

/// One generated media asset (b64 or remote url).
#[derive(Debug)]
pub struct UcMediaAsset {
    pub b64: String,
    pub mime: String,
    /// Set when the upstream handed back a URL rather than bytes
    /// (veoaifree-style); UC always downloads to b64.
    pub url: Option<String>,
}

/// Outcome of a UC media call: assets plus the rotated `__client` cookie
/// header when Clerk issued one (persist via `updateProviderCredentials`).
pub struct UcMediaResult {
    pub assets: Vec<UcMediaAsset>,
    pub rotated_cookie_header: Option<String>,
}

pub struct UcWebExecutor {
    pool: Arc<ClientPool>,
}

impl UcWebExecutor {
    pub fn new(pool: Arc<ClientPool>) -> Self {
        Self { pool }
    }

    /// `generateMedia` — kind "image" (`/v2/image-gen`) or "video"
    /// (`/image_to_video`; an input image is uploaded first and passed as
    /// `media_blob_name`).
    pub async fn generate_media(
        &self,
        prompt: &str,
        kind: &str,
        size: Option<&str>,
        model: Option<&str>,
        duration: Option<f64>,
        resolution: Option<&str>,
        images: &[String],
        credentials: &ProviderConnection,
        proxy: Option<&ProxyTarget>,
    ) -> Result<UcMediaResult, UcWebError> {
        if kind != "image" && kind != "video" {
            return Err(UcWebError::bad_request(
                "uc-web supports image and video generation only",
            ));
        }
        if prompt.trim().is_empty() {
            return Err(UcWebError::bad_request("prompt is required"));
        }
        let cookie_header = credentials.api_key.clone().unwrap_or_default();
        let session = self.mint_session(&cookie_header, proxy).await?;
        let rotated_cookie_header = with_rotated_client(&cookie_header, &session.rotated_client);
        let headers = auth_headers(&session.jwt);
        let client = self
            .pool
            .get("uc-web", proxy)
            .map_err(|e| UcWebError::new(502, e.to_string()))?
            .as_ref()
            .clone();

        if kind == "image" {
            let image_model = model.unwrap_or(UC_DEFAULT_IMAGE_MODEL);
            let spec = image_model_spec(image_model).ok_or_else(|| {
                UcWebError::bad_request(format!(
                    "Unknown UC image model \"{image_model}\" — see the provider's model list"
                ))
            })?;
            let mut body = json!({
                "prompt": prompt,
                "mode": UC_IMAGE_MODE,
                "model_version": spec.upstream.unwrap_or(image_model),
                "m_n_user": true,
                "moderationMode": UC_MODERATION_MODE,
                "country": UC_COUNTRY,
                "vdiscount": false,
            });
            let aspect = aspect_for(size, spec.aspects);
            if let Some(obj) = body.as_object_mut() {
                obj.insert("aspect_ratio".into(), json!(aspect.aspect));
                obj.insert("imageWidth".into(), json!(aspect.width));
                obj.insert("imageHeight".into(), json!(aspect.height));
            }
            if !spec.resolutions.is_empty() {
                let want = resolution.unwrap_or("").to_lowercase();
                let pick = if spec.resolutions.contains(&want.as_str()) {
                    want
                } else {
                    spec.resolutions[0].to_string()
                };
                body["resolution"] = json!(pick);
            }
            // `qualities` only gates which registry models accept a quality
            // param upstream; the /v2/image-gen body does not carry it.
            let _ = spec.qualities;
            let res = client
                .post(UC_IMAGE_URL)
                .headers(headers.clone())
                .json(&body)
                .send()
                .await
                .map_err(|e| UcWebError::new(502, e.to_string()))?;
            if !res.status().is_success() {
                let status = res.status().as_u16();
                let detail: String = res
                    .text()
                    .await
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect();
                return Err(UcWebError::new(
                    status,
                    format!("UC image request failed (HTTP {status}) {detail}"),
                ));
            }
            let json: Value = res
                .json()
                .await
                .map_err(|e| UcWebError::new(502, e.to_string()))?;
            let result_url = json
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if result_url.is_empty() {
                return Err(UcWebError::new(
                    502,
                    "UC image response carried no result url",
                ));
            }
            self.poll_result_url(&client, &result_url, POLL_TIMEOUT_MS)
                .await?;
            let asset = self.download_binary(&client, &result_url).await?;
            return Ok(UcMediaResult {
                assets: vec![asset],
                rotated_cookie_header,
            });
        }

        // kind == "video"
        let video_model = model.unwrap_or(UC_DEFAULT_VIDEO_MODEL);
        let spec = video_model_spec(video_model).ok_or_else(|| {
            UcWebError::bad_request(format!(
                "Unknown UC video model \"{video_model}\" — see the provider's model list"
            ))
        })?;
        let input_image = images.first();
        if input_image.is_some() && !spec.i2v {
            return Err(UcWebError::bad_request(format!(
                "UC video model \"{video_model}\" does not accept an input image"
            )));
        }
        if input_image.is_none() && !spec.t2v {
            return Err(UcWebError::bad_request(format!(
                "UC video model \"{video_model}\" requires an input image"
            )));
        }

        let seconds = duration
            .filter(|d| d.is_finite() && *d > 0.0)
            .unwrap_or_else(|| spec.seconds.first().copied().unwrap_or(5) as f64);
        let video_resolution = if !spec.resolutions.is_empty() {
            let want = resolution.unwrap_or("").to_string();
            if spec.resolutions.contains(&want.as_str()) {
                want
            } else {
                spec.resolutions[0].to_string()
            }
        } else {
            "480p".to_string()
        };
        let mut video_body = json!({
            "prompt": prompt,
            "media_blob_name": Value::Null,
            "num_frames": (seconds * 16.0).round() as i64 + 1,
            "frames_per_second": 16,
            "num_inference_steps": 30,
            "guide_scale": 5,
            "shift": 5,
            "aspect_ratio": if spec.aspects.contains(&"auto") { "auto" } else { "9:16" },
            "pro_mode": video_model == "wan-3.0-uncensored-pro",
            "turbo": video_model == "turbo",
            "resolution": video_resolution,
            "sora_resolution": video_resolution,
            "end_frame_blob_name": Value::Null,
            "model": video_model,
            "seconds": seconds,
            "video_to_video_duration": seconds,
            "vdiscount": false,
        });

        if let Some(input) = input_image {
            // Image-to-video: signed URL → PUT bytes → generate with blob name.
            if session.uid.is_empty() {
                return Err(UcWebError::auth(
                    "Could not derive the account uid — re-paste the full Cookie header",
                ));
            }
            let bytes = self.resolve_image_bytes(&client, input).await?;
            let signed_res = client
                .post(UC_SIGNED_URL)
                .headers(headers.clone())
                .json(&json!({
                    "content_type": "image/png",
                    "user_identifier": session.uid,
                }))
                .send()
                .await
                .map_err(|e| UcWebError::new(502, e.to_string()))?;
            if !signed_res.status().is_success() {
                let status = signed_res.status().as_u16();
                let detail: String = signed_res
                    .text()
                    .await
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect();
                return Err(UcWebError::new(
                    status,
                    format!("UC signed-url request failed (HTTP {status}) {detail}"),
                ));
            }
            let signed: Value = signed_res
                .json()
                .await
                .map_err(|e| UcWebError::new(502, e.to_string()))?;
            let signed_url = signed
                .get("signed_url")
                .and_then(Value::as_str)
                .unwrap_or("");
            let blob_name = signed
                .get("blob_name")
                .and_then(Value::as_str)
                .unwrap_or("");
            if signed_url.is_empty() || blob_name.is_empty() {
                return Err(UcWebError::new(
                    502,
                    "UC signed-url response missing signed_url or blob_name",
                ));
            }
            let put_res = client
                .put(signed_url)
                .header(CONTENT_TYPE, "image/png")
                .body(bytes)
                .send()
                .await
                .map_err(|e| UcWebError::new(502, e.to_string()))?;
            if !put_res.status().is_success() {
                let status = put_res.status().as_u16();
                return Err(UcWebError::new(
                    status,
                    format!("UC input-image upload failed (HTTP {status})"),
                ));
            }
            video_body["media_blob_name"] = json!(blob_name);
        }

        let gen_res = client
            .post(UC_VIDEO_URL)
            .headers(headers.clone())
            .json(&video_body)
            .send()
            .await
            .map_err(|e| UcWebError::new(502, e.to_string()))?;
        if !gen_res.status().is_success() {
            let status = gen_res.status().as_u16();
            let detail: String = gen_res
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect();
            return Err(UcWebError::new(
                status,
                format!("UC video request failed (HTTP {status}) {detail}"),
            ));
        }
        let gen_json: Value = gen_res
            .json()
            .await
            .map_err(|e| UcWebError::new(502, e.to_string()))?;
        let result_url = gen_json
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if result_url.is_empty() {
            return Err(UcWebError::new(
                502,
                "UC video response carried no result url",
            ));
        }
        let timeout_ms = gen_json
            .get("timeout_seconds")
            .and_then(Value::as_f64)
            .filter(|t| t.is_finite() && *t > 0.0)
            .map(|t| (t * 1000.0) as u64)
            .unwrap_or(POLL_TIMEOUT_MS);
        self.poll_result_url(&client, &result_url, timeout_ms)
            .await?;
        let asset = self.download_binary(&client, &result_url).await?;
        Ok(UcMediaResult {
            assets: vec![asset],
            rotated_cookie_header,
        })
    }

    // ── Clerk session ──────────────────────────────────────────────────

    async fn mint_session(
        &self,
        cookie_header: &str,
        proxy: Option<&ProxyTarget>,
    ) -> Result<UcClerkSession, UcWebError> {
        let jar = parse_cookie_jar(cookie_header);
        let client_jwt = jar.get("__client").cloned().unwrap_or_default();
        if client_jwt.is_empty() {
            return Err(UcWebError::auth(
                "UC credential missing the __client cookie — paste the full uncensored.com Cookie header from a logged-in session",
            ));
        }
        let clerk_headers = {
            let mut h = HeaderMap::new();
            if let Ok(v) = HeaderValue::from_str(&build_cookie_header(&jar)) {
                h.insert(reqwest::header::COOKIE, v);
            }
            if let Ok(v) = HeaderValue::from_str(UC_ORIGIN) {
                h.insert(reqwest::header::ORIGIN, v);
            }
            if let Ok(v) = HeaderValue::from_str(&format!("{UC_ORIGIN}/")) {
                h.insert(reqwest::header::REFERER, v);
            }
            if let Ok(v) = HeaderValue::from_str("application/json") {
                h.insert(reqwest::header::ACCEPT, v);
            }
            h
        };
        let resolved = self
            .resolve_clerk_session_id(&client_jwt, &clerk_headers, proxy)
            .await?;
        let sid = resolved.sid;
        if sid.is_empty() {
            return Err(UcWebError::auth(
                "UC Clerk session not found — the __client cookie has no active session. Log in again via Login via Browser",
            ));
        }
        let client = self
            .pool
            .get("uc-web", proxy)
            .map_err(|e| UcWebError::new(502, e.to_string()))?
            .as_ref()
            .clone();
        let res = client
            .post(format!(
                "{UC_CLERK_FAPI}/v1/client/sessions/{}/tokens?_clerk_js_version={UC_CLERK_JS_VERSION}",
                urlencoding_encode(&sid)
            ))
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(reqwest::header::ORIGIN, UC_ORIGIN)
            .header(reqwest::header::REFERER, format!("{UC_ORIGIN}/"))
            .header(reqwest::header::COOKIE, build_cookie_header(&jar))
            .body("")
            .send()
            .await
            .map_err(|e| UcWebError::new(502, e.to_string()))?;
        if !res.status().is_success() {
            let status = res.status().as_u16();
            let detail: String = res
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect();
            return Err(UcWebError::new(
                status,
                format!("UC Clerk token mint failed (HTTP {status}) {detail}"),
            ));
        }
        let rotated_from_list = read_set_cookie_value(res.headers(), "__client");
        let body: Value = res
            .json()
            .await
            .map_err(|e| UcWebError::new(502, e.to_string()))?;
        let jwt = body
            .get("jwt")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if jwt.is_empty() {
            return Err(UcWebError::new(502, "UC Clerk token mint returned no jwt"));
        }
        let uid = decode_jwt(&jwt)
            .and_then(|c| c.get("uid").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        let rotated_client = if !rotated_from_list.is_empty() {
            rotated_from_list
        } else {
            resolved.rotated_client
        };
        Ok(UcClerkSession {
            jwt,
            uid,
            rotated_client,
        })
    }

    /// Resolve the Clerk session id. Legacy `__client` JWTs carry `sid`;
    /// current ones only `{id, rotating_token}`, so fall back to listing the
    /// client's sessions. Clerk rotates `__client` on every `/v1/client`
    /// call; the new value must be carried forward.
    async fn resolve_clerk_session_id(
        &self,
        client_jwt: &str,
        clerk_headers: &HeaderMap,
        proxy: Option<&ProxyTarget>,
    ) -> Result<ClerkSessionRef, UcWebError> {
        if let Some(sid) = decode_jwt(client_jwt)
            .and_then(|c| c.get("sid").and_then(Value::as_str).map(str::to_string))
            .filter(|s| !s.is_empty())
        {
            return Ok(ClerkSessionRef {
                sid,
                rotated_client: String::new(),
            });
        }
        let client = self
            .pool
            .get("uc-web", proxy)
            .map_err(|e| UcWebError::new(502, e.to_string()))?
            .as_ref()
            .clone();
        let res = client
            .get(format!(
                "{UC_CLERK_FAPI}/v1/client?_clerk_js_version={UC_CLERK_JS_VERSION}"
            ))
            .headers(clerk_headers.clone())
            .send()
            .await
            .map_err(|e| UcWebError::new(502, e.to_string()))?;
        if !res.status().is_success() {
            return Ok(ClerkSessionRef {
                sid: String::new(),
                rotated_client: String::new(),
            });
        }
        let rotated_client = read_set_cookie_value(res.headers(), "__client");
        let body: Value = res.json().await.unwrap_or(Value::Null);
        let payload = body
            .get("response")
            .or_else(|| body.get("client"))
            .unwrap_or(&body);
        let sessions = payload.get("sessions").and_then(Value::as_array);
        let active = sessions
            .and_then(|list| {
                list.iter()
                    .find(|s| s.get("status").and_then(Value::as_str) == Some("active"))
                    .or_else(|| list.first())
            })
            .cloned()
            .unwrap_or(Value::Null);
        let sid = active
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        Ok(ClerkSessionRef {
            sid,
            rotated_client,
        })
    }

    // ── Result polling / download ──────────────────────────────────────

    /// HEAD-poll the pre-determined result URL until it serves 200.
    async fn poll_result_url(
        &self,
        client: &reqwest::Client,
        url: &str,
        timeout_ms: u64,
    ) -> Result<(), UcWebError> {
        let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
            let status = match client
                .head(url)
                .header(reqwest::header::REFERER, format!("{UC_ORIGIN}/"))
                .send()
                .await
            {
                Ok(res) => res.status().as_u16(),
                Err(_) => 0, // transient network — keep polling
            };
            if status == 200 {
                return Ok(());
            }
            if (400..500).contains(&status) && status != 429 {
                tracing::warn!("UC-WEB result poll got HTTP {status} — continuing until timeout");
            }
        }
        Err(UcWebError::new(
            502,
            "UC result did not become available in time",
        ))
    }

    async fn download_binary(
        &self,
        client: &reqwest::Client,
        url: &str,
    ) -> Result<UcMediaAsset, UcWebError> {
        let res = client
            .get(url)
            .header(reqwest::header::REFERER, format!("{UC_ORIGIN}/"))
            .send()
            .await
            .map_err(|e| UcWebError::new(502, e.to_string()))?;
        if !res.status().is_success() {
            let status = res.status().as_u16();
            return Err(UcWebError::new(
                status,
                format!("UC result download failed (HTTP {status})"),
            ));
        }
        let mime = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .unwrap_or("application/octet-stream")
            .to_string();
        let bytes = res
            .bytes()
            .await
            .map_err(|e| UcWebError::new(502, e.to_string()))?;
        Ok(UcMediaAsset {
            b64: base64::engine::general_purpose::STANDARD.encode(bytes),
            mime,
            url: None,
        })
    }

    async fn resolve_image_bytes(
        &self,
        client: &reqwest::Client,
        reference: &str,
    ) -> Result<Vec<u8>, UcWebError> {
        if let Some(rest) = reference.strip_prefix("data:") {
            if let Some((_, b64)) = rest.split_once(";base64,") {
                return base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|_| {
                        UcWebError::bad_request(
                            "UC image-to-video could not decode the input image",
                        )
                    });
            }
        }
        if reference.starts_with("http://") || reference.starts_with("https://") {
            let res = client
                .get(reference)
                .header(reqwest::header::ACCEPT, "image/*")
                .header(reqwest::header::USER_AGENT, WEB_UA)
                .send()
                .await
                .map_err(|e| UcWebError::new(502, e.to_string()))?;
            if !res.status().is_success() {
                let status = res.status().as_u16();
                return Err(UcWebError::new(
                    status,
                    format!("input image download failed (HTTP {status})"),
                ));
            }
            return res
                .bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| UcWebError::new(502, e.to_string()));
        }
        base64::engine::general_purpose::STANDARD
            .decode(reference)
            .map_err(|_| {
                UcWebError::bad_request("UC image-to-video could not decode the input image")
            })
    }
}

struct UcClerkSession {
    jwt: String,
    uid: String,
    rotated_client: String,
}

struct ClerkSessionRef {
    sid: String,
    rotated_client: String,
}

struct AspectPick {
    aspect: &'static str,
    width: &'static str,
    height: &'static str,
}

/// `aspectFor`: direct aspect match → nearest ratio for "WxH" sizes →
/// fallback (first allowed or "1:1").
fn aspect_for(size: Option<&str>, allowed: &[&str]) -> AspectPick {
    let raw = size.unwrap_or("").trim();
    let pick = |candidate: &str| -> Option<AspectPick> {
        let (w, h) = aspect_size(candidate)?;
        if !allowed.is_empty() && !allowed.contains(&candidate) {
            return None;
        }
        Some(AspectPick {
            aspect: candidate_leak(candidate),
            width: w,
            height: h,
        })
    };
    // aspect strings are 'static literals from aspect_size's match arms.
    fn candidate_leak(s: &str) -> &'static str {
        // The candidates come from UC_ASPECT_SIZES keys — all 'static.
        match s {
            "1:1" => "1:1",
            "16:9" => "16:9",
            "9:16" => "9:16",
            "4:3" => "4:3",
            "3:4" => "3:4",
            "3:2" => "3:2",
            "2:3" => "2:3",
            "4:5" => "4:5",
            "5:4" => "5:4",
            "21:9" => "21:9",
            _ => "1:1",
        }
    }
    if let Some(p) = pick(raw) {
        return p;
    }
    if raw.contains('x') {
        let mut dims = raw.split('x');
        let w: f64 = dims.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let h: f64 = dims.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        if w > 0.0 && h > 0.0 {
            let mut best: Option<(&'static str, f64)> = None;
            for aspect in [
                "1:1", "16:9", "9:16", "4:3", "3:4", "3:2", "2:3", "4:5", "5:4", "21:9",
            ] {
                if !allowed.is_empty() && !allowed.contains(&aspect) {
                    continue;
                }
                if let Some((aw, ah)) = aspect_size(aspect) {
                    let delta =
                        ((aw.parse::<f64>().unwrap() / ah.parse::<f64>().unwrap()) - (w / h)).abs();
                    if best.map(|(_, d)| delta < d).unwrap_or(true) {
                        best = Some((aspect, delta));
                    }
                }
            }
            if let Some((aspect, _)) = best {
                let (width, height) = aspect_size(aspect).unwrap();
                return AspectPick {
                    aspect,
                    width,
                    height,
                };
            }
        }
    }
    let fallback = if allowed.is_empty() {
        "1:1"
    } else {
        allowed[0]
    };
    let (width, height) = aspect_size(fallback).unwrap_or(("1024", "1024"));
    // `fallback` borrows from `allowed` (non-'static); the pick's aspect is a
    // known literal so resolve it through the same 'static mapping used above.
    AspectPick {
        aspect: candidate_leak(fallback),
        width,
        height,
    }
}

// ── Cookie / JWT helpers ────────────────────────────────────────────────

fn parse_cookie_jar(header: &str) -> BTreeMap<String, String> {
    let mut jar = BTreeMap::new();
    for part in header.split(';') {
        if let Some(eq) = part.find('=') {
            let name = part[..eq].trim();
            if !name.is_empty() {
                jar.insert(name.to_string(), part[eq + 1..].trim().to_string());
            }
        }
    }
    jar
}

fn build_cookie_header(jar: &BTreeMap<String, String>) -> String {
    jar.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Decode a JWT payload (base64url, no signature check — claims only).
fn decode_jwt(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Read a `Set-Cookie` value for `name` from response headers.
fn read_set_cookie_value(headers: &HeaderMap, name: &str) -> String {
    for value in headers.get_all(reqwest::header::SET_COOKIE) {
        if let Ok(entry) = value.to_str() {
            let pair = entry.split(';').next().unwrap_or("").trim();
            if let Some(eq) = pair.find('=') {
                if pair[..eq].trim() == name {
                    return pair[eq + 1..].trim().to_string();
                }
            }
        }
    }
    String::new()
}

/// Swap the `__client` value inside a stored Cookie header; returns the new
/// header, or "" when nothing changed.
fn with_rotated_client(cookie_header: &str, rotated_client: &str) -> Option<String> {
    if rotated_client.is_empty() {
        return None;
    }
    let mut jar = parse_cookie_jar(cookie_header);
    if jar.get("__client").map(String::as_str) == Some(rotated_client) {
        return None;
    }
    jar.insert("__client".to_string(), rotated_client.to_string());
    Some(build_cookie_header(&jar))
}

fn auth_headers(jwt: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&format!("Bearer {jwt}")) {
        headers.insert(reqwest::header::AUTHORIZATION, v);
    }
    headers.insert(reqwest::header::ORIGIN, HeaderValue::from_static(UC_ORIGIN));
    if let Ok(v) = HeaderValue::from_str(&format!("{UC_ORIGIN}/")) {
        headers.insert(reqwest::header::REFERER, v);
    }
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers
}

fn urlencoding_encode(s: &str) -> String {
    // encodeURIComponent-lite: keep unreserved, %XX the rest.
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
