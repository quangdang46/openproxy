//! Token refresh with deduplication.
//!
//! Provides a `RefreshDedup` concurrent-safe dedup layer plus a set of
//! per-provider refresh functions that each wrap the upstream token-refresh
//! API for that provider.
//!
//! # Dedup guarantees
//!
//! - Only **one** HTTP refresh request is in flight per `(provider, old_token)`
//!   pair at any time. Concurrent callers all await the same `OnceCell`.
//! - The result is cached for `REFRESH_RESULT_TTL_MS` (10 s) so that burst
//!   callers within that window reuse the same response instead of sending
//!   duplicate HTTP requests.
//! - The per-token mutex in the old code prevented `refresh_token_reused`
//!   errors from Auth0; this dedup layer provides the same mutual exclusion
//!   *within process*.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;
use tokio::sync::OnceCell;

use super::TOKEN_EXPIRY_BUFFER_MS;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Result of a successful (or failed) token refresh.
#[derive(Debug, Clone)]
pub struct RefreshResult {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
}

impl RefreshResult {
    /// Wrap an access-token-only response (no refresh_token, no expires_in).
    pub fn access_only(access_token: String) -> Self {
        Self {
            access_token,
            refresh_token: None,
            expires_in: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Refresh lead times (how early before expiry we proactively refresh)
// ---------------------------------------------------------------------------

pub const REFRESH_LEAD_CODEX_MS: u64 = 5 * 24 * 60 * 60 * 1000; // 5 days
pub const REFRESH_LEAD_OPENAI_MS: u64 = 5 * 24 * 60 * 60 * 1000; // 5 days
pub const REFRESH_LEAD_CLAUDE_MS: u64 = 4 * 60 * 60 * 1000; // 4 hours
pub const REFRESH_LEAD_IFLOW_MS: u64 = 24 * 60 * 60 * 1000; // 24 hours
pub const REFRESH_LEAD_QWEN_MS: u64 = 20 * 60 * 1000; // 20 minutes
pub const REFRESH_LEAD_KIMI_CODING_MS: u64 = 5 * 60 * 1000; // 5 minutes
pub const REFRESH_LEAD_ANTIGRAVITY_MS: u64 = 5 * 60 * 1000; // 5 minutes
pub const REFRESH_LEAD_XAI_MS: u64 = 5 * 60 * 1000; // 5 minutes
/// Gemini CLI uses a lead time returned by its own token response, so this is
/// a fallback default.
pub const REFRESH_LEAD_GEMINI_CLI_DEFAULT_MS: u64 = 5 * 60 * 1000;

// ---------------------------------------------------------------------------
// Constants shared by refresh functions
// ---------------------------------------------------------------------------

const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub(crate) const CLAUDE_TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";

const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

const GEMINI_CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";
const GEMINI_CLIENT_SECRET: &str = "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";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

const ANTIGRAVITY_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const ANTIGRAVITY_CLIENT_SECRET: &str = "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";

const IFLOW_CLIENT_ID: &str = "10009311001";
const IFLOW_CLIENT_SECRET: &str = "4Z3YjXycVsQvyGF1etiNlIBB4RsqSDtW";
const IFLOW_TOKEN_URL: &str = "https://iflow.cn/oauth/token";

const QWEN_CLIENT_ID: &str = "f0304373b74a44d2b584a3fb70ca9e56";
const QWEN_TOKEN_URL: &str = "https://chat.qwen.ai/api/v1/oauth2/token";

const XAI_CLIENT_ID: &str = "b1a00492-073a-073a-47ea-816f-4c329264a828";

const CLINE_REFRESH_URL: &str = "https://api.cline.bot/api/v1/auth/refresh";

const KIRO_AUTH_SERVICE: &str = "https://prod.us-east-1.auth.desktop.kiro.dev";

const GITHUB_OAUTH_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GITHUB_COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";

const GITLAB_TOKEN_URL: &str = "https://gitlab.com/oauth/token";

// ---------------------------------------------------------------------------
// Refresh dedup
// ---------------------------------------------------------------------------

/// Number of milliseconds a refresh result is cached for dedup purposes.
const REFRESH_RESULT_TTL_MS: u64 = 10_000;

/// Internal entry for an in-flight or cached refresh.
struct DedupEntry {
    in_flight: Arc<OnceCell<Result<RefreshResult, String>>>,
    cached_result: Option<Result<RefreshResult, String>>,
    expires_at: Instant,
}

/// Concurrent-safe dedup that ensures only one refresh request is in flight
/// per `(provider, old_token)` pair, and caches the result for 10 seconds.
pub struct RefreshDedup {
    cache: Mutex<HashMap<String, DedupEntry>>,
}

impl RefreshDedup {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Return a cached or freshly-fetched `RefreshResult` for
    /// `(provider, old_token)`.
    ///
    /// If another task is already refreshing the same token, this call will
    /// await that in-flight request rather than duplicating it.
    pub async fn dedup<F, Fut>(
        &self,
        provider: &str,
        old_token: &str,
        refresh_fn: F,
    ) -> Result<RefreshResult, String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<RefreshResult, String>>,
    {
        let key = make_dedup_key(provider, old_token);
        let cell: Arc<OnceCell<Result<RefreshResult, String>>>;

        // --- critical section: check cache / get-or-create in_flight cell ---
        {
            let mut cache = self.cache.lock();

            // Fast path: cached result is still warm.
            if let Some(entry) = cache.get(&key) {
                if entry.expires_at > Instant::now() {
                    if let Some(ref cached) = entry.cached_result {
                        return cached.clone();
                    }
                }
            }

            // Get or insert an OnceCell so concurrent callers share the same
            // in-flight future.
            let entry = cache.entry(key.clone()).or_insert_with(|| DedupEntry {
                in_flight: Arc::new(OnceCell::new()),
                cached_result: None,
                expires_at: Instant::now(),
            });
            cell = entry.in_flight.clone();
        }
        // --- lock released ---

        let result = cell
            .get_or_init(|| async move { refresh_fn().await })
            .await
            .clone();

        // --- cache warming ---
        {
            let mut cache = self.cache.lock();
            if let Some(entry) = cache.get_mut(&key) {
                entry.cached_result = Some(result.clone());
                entry.expires_at = Instant::now() + Duration::from_millis(REFRESH_RESULT_TTL_MS);
            }
        }

        result
    }
}

impl Default for RefreshDedup {
    fn default() -> Self {
        Self::new()
    }
}

fn make_dedup_key(provider: &str, old_token: &str) -> String {
    format!("{}:{}", provider, old_token)
}

/// Check whether an access token needs refreshing based on its `expires_at`
/// RFC 3339 timestamp.
pub fn needs_refresh(expires_at: &Option<String>) -> bool {
    let Some(expires_at) = expires_at else {
        return true;
    };

    match chrono::DateTime::parse_from_rfc3339(expires_at) {
        Ok(expires_at) => {
            let expires_at = expires_at.with_timezone(&chrono::Utc);
            let now = chrono::Utc::now();
            let buffer = chrono::Duration::milliseconds(TOKEN_EXPIRY_BUFFER_MS as i64);
            expires_at - buffer < now
        }
        Err(_) => true,
    }
}

// ---------------------------------------------------------------------------
// Per-provider refresh functions
// ---------------------------------------------------------------------------

/// Refresh a Claude OAuth access-token.
///
/// POST JSON to `https://api.anthropic.com/v1/oauth/token`.
pub async fn refresh_claude_oauth_token(refresh_token: &str) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLAUDE_CLIENT_ID,
    });
    let resp = client
        .post(CLAUDE_TOKEN_URL)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Claude refresh request failed: {e}"))?;
    parse_json_refresh_response(resp).await
}

/// Refresh a Codex / ChatGPT access token.
///
/// POST form-urlencoded to the OpenAI Auth0 token endpoint.
pub async fn refresh_codex_token(refresh_token: &str) -> Result<RefreshResult, String> {
    refresh_form_token(
        &codex_token_url(),
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CODEX_CLIENT_ID),
        ],
    )
    .await
}

/// Resolve the codex token URL (allows env-override).
fn codex_token_url() -> String {
    std::env::var("OPENPROXY_CODEX_TOKEN_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| CODEX_TOKEN_URL.to_string())
}

/// Refresh a Google OAuth token (used by both gemini-cli and antigravity).
pub async fn refresh_google_token(
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<RefreshResult, String> {
    refresh_form_token(
        GOOGLE_TOKEN_URL,
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ],
    )
    .await
}

/// Refresh a Qwen access token.
pub async fn refresh_qwen_token(refresh_token: &str) -> Result<RefreshResult, String> {
    refresh_form_token(
        QWEN_TOKEN_URL,
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", QWEN_CLIENT_ID),
        ],
    )
    .await
}

/// Refresh an iFlow access token (uses Basic Auth).
pub async fn refresh_iflow_token(refresh_token: &str) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let basic = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        format!("{}:{}", IFLOW_CLIENT_ID, IFLOW_CLIENT_SECRET),
    );
    let resp = client
        .post(IFLOW_TOKEN_URL)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(ACCEPT, "application/json")
        .header(AUTHORIZATION, format!("Basic {basic}"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .map_err(|e| format!("iFlow refresh request failed: {e}"))?;
    parse_json_refresh_response(resp).await
}

/// Refresh a GitHub OAuth token.
///
/// Note: GitHub's token refresh response does *not* include a `refresh_token`
/// field. The `refresh_token` in the returned `RefreshResult` will be `None`.
pub async fn refresh_github_token(refresh_token: &str) -> Result<RefreshResult, String> {
    refresh_form_token(
        GITHUB_OAUTH_TOKEN_URL,
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", "openproxy"),
        ],
    )
    .await
}

/// Refresh a GitHub Copilot session token via the internal v2 token endpoint.
///
/// Unlike the other refresh functions this takes a *GitHub OAuth access token*
/// (not a refresh token) and performs a **GET** request.
pub async fn refresh_copilot_token(access_token: &str) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let resp = client
        .get(GITHUB_COPILOT_TOKEN_URL)
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .header("User-Agent", "GitHubCopilotChat/0.38.0")
        .header("Editor-Version", "vscode/1.110.0")
        .header("Editor-Plugin-Version", "copilot-chat/0.38.0")
        .header(ACCEPT, "application/json")
        .header("x-github-api-version", "2025-04-01")
        .send()
        .await
        .map_err(|e| format!("Copilot token refresh request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Copilot token refresh returned HTTP {}",
            resp.status().as_u16()
        ));
    }

    let payload: Value = resp
        .json()
        .await
        .map_err(|e| format!("Copilot refresh parse failed: {e}"))?;

    let token = payload
        .get("token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "Copilot refresh response missing 'token'".to_string())?;

    Ok(RefreshResult {
        access_token: token.to_string(),
        refresh_token: None,
        expires_in: None,
    })
}

/// Refresh a Kiro access token.
///
/// Branches between AWS Cognito OIDC and Kiro's own social auth service
/// depending on whether `provider_specific_data` contains `clientId` /
/// `clientSecret`.
pub async fn refresh_kiro_token(
    refresh_token: &str,
    provider_specific_data: &std::collections::BTreeMap<String, Value>,
) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let (url, body) = if let (Some(client_id), Some(client_secret)) = (
        provider_specific_data
            .get("clientId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty()),
        provider_specific_data
            .get("clientSecret")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty()),
    ) {
        let region = provider_specific_data
            .get("region")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("us-east-1");
        (
            format!("https://oidc.{region}.amazonaws.com/token"),
            serde_json::json!({
                "clientId": client_id,
                "clientSecret": client_secret,
                "refreshToken": refresh_token,
                "grantType": "refresh_token",
            }),
        )
    } else {
        (
            format!("{KIRO_AUTH_SERVICE}/refreshToken"),
            serde_json::json!({ "refreshToken": refresh_token }),
        )
    };

    let resp = client
        .post(&url)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Kiro refresh request failed: {e}"))?;

    let payload: Value = resp
        .json()
        .await
        .map_err(|e| format!("Kiro refresh parse failed: {e}"))?;

    let access_token = payload
        .get("accessToken")
        .or_else(|| payload.get("access_token"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "Kiro refresh response missing access token".to_string())?;

    Ok(RefreshResult {
        access_token: access_token.to_string(),
        refresh_token: payload
            .get("refreshToken")
            .or_else(|| payload.get("refresh_token"))
            .and_then(Value::as_str)
            .map(str::to_string),
        expires_in: payload
            .get("expiresIn")
            .or_else(|| payload.get("expires_in"))
            .and_then(Value::as_i64),
    })
}

/// Refresh an xAI access token via form-urlencoded request.
///
/// Uses the standard xAI auth endpoint.
pub async fn refresh_xai_token(refresh_token: &str) -> Result<RefreshResult, String> {
    let token_url = resolve_xai_token_url();
    refresh_form_token(
        &token_url,
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", XAI_CLIENT_ID),
        ],
    )
    .await
}

/// Resolve xAI's token URL (env override or default).
fn resolve_xai_token_url() -> String {
    std::env::var("OPENPROXY_XAI_TOKEN_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "https://auth.x.ai/oauth2/token".to_string())
}

/// Refresh an OpenAI access token (same flow as codex).
pub async fn refresh_openai_token(refresh_token: &str) -> Result<RefreshResult, String> {
    refresh_form_token(
        &codex_token_url(),
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CODEX_CLIENT_ID),
        ],
    )
    .await
}

/// Refresh a Kimi Coding access token.
pub async fn refresh_kimi_coding_token(refresh_token: &str) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post("https://api.moonshot.cn/kimi-device/oauth/token")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(ACCEPT, "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", "kimi-coding-openproxy"),
        ])
        .send()
        .await
        .map_err(|e| format!("Kimi Coding refresh request failed: {e}"))?;
    parse_json_refresh_response(resp).await
}

/// Refresh a KiloCode access token.
pub async fn refresh_kilocode_token(refresh_token: &str) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post("https://api.kilo.ai/oauth/token")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(ACCEPT, "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", "kilocode-openproxy"),
        ])
        .send()
        .await
        .map_err(|e| format!("KiloCode refresh request failed: {e}"))?;
    parse_json_refresh_response(resp).await
}

/// Refresh a Cline access token.
pub async fn refresh_cline_token(refresh_token: &str) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(CLINE_REFRESH_URL)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .json(&serde_json::json!({
            "refreshToken": refresh_token,
            "grantType": "refresh_token",
            "clientType": "extension"
        }))
        .send()
        .await
        .map_err(|e| format!("Cline refresh request failed: {e}"))?;

    let payload: Value = resp
        .json()
        .await
        .map_err(|e| format!("Cline refresh parse failed: {e}"))?;

    let data = payload.get("data").unwrap_or(&payload);
    let access_token = data
        .get("accessToken")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "Cline refresh response missing access token".to_string())?;

    let expires_in = data
        .get("expiresAt")
        .and_then(Value::as_str)
        .and_then(|expires_at| {
            chrono::DateTime::parse_from_rfc3339(expires_at).ok()
        })
        .map(|expires_at| (expires_at.timestamp() - chrono::Utc::now().timestamp()).max(1));

    Ok(RefreshResult {
        access_token: access_token.to_string(),
        refresh_token: data
            .get("refreshToken")
            .and_then(Value::as_str)
            .map(str::to_string),
        expires_in,
    })
}

/// Refresh a GitLab access token.
pub async fn refresh_gitlab_token(refresh_token: &str) -> Result<RefreshResult, String> {
    refresh_form_token(
        GITLAB_TOKEN_URL,
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", "openproxy"),
        ],
    )
    .await
}

/// Refresh a CodeBuddy access token.
pub async fn refresh_codebuddy_token(refresh_token: &str) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post("https://copilot.tencent.com/oauth/token")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(ACCEPT, "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", "codebuddy-openproxy"),
        ])
        .send()
        .await
        .map_err(|e| format!("CodeBuddy refresh request failed: {e}"))?;
    parse_json_refresh_response(resp).await
}

/// Qoder does not support token refresh. This function always returns an error.
pub async fn refresh_qoder_token(_refresh_token: &str) -> Result<RefreshResult, String> {
    Err("Qoder does not support token refresh".to_string())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Send a form-urlencoded POST and parse the JSON response into a RefreshResult.
async fn refresh_form_token(
    url: &str,
    fields: Vec<(&str, &str)>,
) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(ACCEPT, "application/json")
        .form(&fields)
        .send()
        .await
        .map_err(|e| format!("Refresh request failed: {e}"))?;
    parse_json_refresh_response(resp).await
}

/// Parse a JSON token-refresh response into a RefreshResult.
///
/// Handles both camelCase and snake_case field names for cross-provider
/// compatibility.
async fn parse_json_refresh_response(resp: reqwest::Response) -> Result<RefreshResult, String> {
    let payload: Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse refresh response: {e}"))?;

    let access_token = payload
        .get("access_token")
        .or_else(|| payload.get("accessToken"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "Refresh response did not include access_token".to_string())?;

    Ok(RefreshResult {
        access_token: access_token.to_string(),
        refresh_token: payload
            .get("refresh_token")
            .or_else(|| payload.get("refreshToken"))
            .and_then(Value::as_str)
            .map(str::to_string),
        expires_in: payload
            .get("expires_in")
            .or_else(|| payload.get("expiresIn"))
            .and_then(Value::as_i64),
    })
}
