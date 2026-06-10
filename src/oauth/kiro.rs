//! Kiro OAuth 2.0 authentication methods
//!
//! Supports 5 auth methods (the `KiroAuthMethod` enum):
//!
//! 1. **BuilderId** — AWS SSO Builder ID device code flow (OIDC client registration +
//!    device authorization + token poll).
//! 2. **Idc** — AWS IAM Identity Center device code flow (same protocol, different
//!    `startUrl` / region).
//! 3. **Google** — Social login via Kiro's Cognito-backed identity provider.
//! 4. **Github** — Social login via Kiro's Cognito-backed identity provider.
//! 5. **Imported** — Import an existing Kiro AWS SSO refresh token (`aorAAAAAG...`).

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default Kiro auth service base URL (Cognito).
const KIRO_AUTH_SERVICE: &str = "https://prod.us-east-1.auth.desktop.kiro.dev";
/// Pre-encoded version of the social redirect URI for URL building.
const KIRO_SOCIAL_REDIRECT_URI_ENCODED: &str =
    "kiro%3A%2F%2Fkiro.kiroAgent%2Fauthenticate-success";
/// The raw redirect URI. **Must** be this exact value (Cognito whitelist).
const KIRO_SOCIAL_REDIRECT_URI: &str = "kiro://kiro.kiroAgent/authenticate-success";

// ---------------------------------------------------------------------------
// KiroAuthMethod
// ---------------------------------------------------------------------------

/// The five authentication methods supported by Kiro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KiroAuthMethod {
    /// AWS SSO Builder ID (default).
    BuilderId,
    /// AWS IAM Identity Center.
    Idc,
    /// Google social login via Cognito.
    Google,
    /// GitHub social login via Cognito.
    Github,
    /// Imported Kiro AWS SSO refresh token.
    Imported,
}

impl KiroAuthMethod {
    /// Return the canonical string representation (kebab-case).
    pub fn as_str(&self) -> &'static str {
        match self {
            KiroAuthMethod::BuilderId => "builder-id",
            KiroAuthMethod::Idc => "idc",
            KiroAuthMethod::Google => "google",
            KiroAuthMethod::Github => "github",
            KiroAuthMethod::Imported => "imported",
        }
    }

    /// Parse a string into a `KiroAuthMethod`. Accepts common variants.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "builder-id" | "builder_id" => Some(Self::BuilderId),
            "idc" => Some(Self::Idc),
            "google" => Some(Self::Google),
            "github" => Some(Self::Github),
            "imported" => Some(Self::Imported),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Response types (camelCase to match AWS OIDC / Kiro API)
// ---------------------------------------------------------------------------

/// Response from the AWS SSO OIDC `/client/register` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientRegistrationResponse {
    pub client_id: String,
    pub client_secret: String,
    #[serde(default)]
    pub client_secret_expires_at: Option<i64>,
}

/// Response from the AWS SSO OIDC `/device_authorization` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAuthorizationResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub interval: Option<u64>,
}

/// Response from the AWS SSO OIDC `/token` endpoint (also used by Cognito).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenPollResponse {
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub profile_arn: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default, alias = "error_description")]
    pub error_description: Option<String>,
}

/// A Kiro-specific error.
#[derive(Debug, Clone)]
pub struct KiroError {
    pub error: String,
    pub error_description: Option<String>,
}

impl std::fmt::Display for KiroError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Kiro error: {}", self.error)?;
        if let Some(desc) = &self.error_description {
            write!(f, " ({desc})")?;
        }
        Ok(())
    }
}

impl std::error::Error for KiroError {}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Resolve the configurable Kiro auth-service base URL.
fn auth_service_base_url() -> String {
    std::env::var("OPENPROXY_KIRO_AUTH_SERVICE_BASE_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| KIRO_AUTH_SERVICE.to_string())
        .trim_end_matches('/')
        .to_string()
}

/// Resolve the configurable AWS OIDC base URL for a given region.
fn oidc_base_url(region: &str) -> String {
    std::env::var("OPENPROXY_KIRO_OIDC_BASE_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| format!("https://oidc.{region}.amazonaws.com"))
        .trim_end_matches('/')
        .to_string()
}

// ---------------------------------------------------------------------------
// 1. AWS OIDC Client Registration
// ---------------------------------------------------------------------------

/// Register an OAuth client with the AWS SSO OIDC endpoint.
///
/// POST `https://oidc.{region}.amazonaws.com/client/register`
pub async fn register_client(region: &str) -> Result<ClientRegistrationResponse, KiroError> {
    let client = reqwest::Client::new();
    let url = format!("{}/client/register", oidc_base_url(region));

    let body = serde_json::json!({
        "clientName": "kiro-oauth-client",
        "clientType": "public",
        "scopes": [
            "codewhisperer:completions",
            "codewhisperer:analysis",
            "codewhisperer:conversations"
        ],
        "grantTypes": [
            "urn:ietf:params:oauth:grant-type:device_code",
            "refresh_token"
        ],
        "issuerUrl": "https://identitycenter.amazonaws.com/ssoins-722374e8c3c8e6c6"
    });

    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| KiroError {
            error: "request_failed".to_string(),
            error_description: Some(e.to_string()),
        })?;

    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(KiroError {
            error: "client_registration_failed".to_string(),
            error_description: Some(text),
        });
    }

    response.json().await.map_err(|e| KiroError {
        error: "parse_error".to_string(),
        error_description: Some(e.to_string()),
    })
}

// ---------------------------------------------------------------------------
// 2. Device Authorization
// ---------------------------------------------------------------------------

/// Start a device authorization flow with AWS SSO OIDC.
///
/// POST `https://oidc.{region}.amazonaws.com/device_authorization`
pub async fn start_device_authorization(
    client_id: &str,
    client_secret: &str,
    start_url: &str,
    region: &str,
) -> Result<DeviceAuthorizationResponse, KiroError> {
    let client = reqwest::Client::new();
    let url = format!("{}/device_authorization", oidc_base_url(region));

    let body = serde_json::json!({
        "clientId": client_id,
        "clientSecret": client_secret,
        "startUrl": start_url,
    });

    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| KiroError {
            error: "request_failed".to_string(),
            error_description: Some(e.to_string()),
        })?;

    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(KiroError {
            error: "device_authorization_failed".to_string(),
            error_description: Some(text),
        });
    }

    response.json().await.map_err(|e| KiroError {
        error: "parse_error".to_string(),
        error_description: Some(e.to_string()),
    })
}

// ---------------------------------------------------------------------------
// 3. Poll Device Token
// ---------------------------------------------------------------------------

/// Poll the AWS SSO OIDC token endpoint for device code completion.
///
/// POST `https://oidc.{region}.amazonaws.com/token`
///
/// Known protocol errors returned from the API (not thrown):
/// - `authorization_pending` — user has not yet approved
/// - `slow_down` — reduce polling frequency
/// - `expired_token` — the device code has expired
/// - `access_denied` — the user denied the request
pub async fn poll_device_token(
    client_id: &str,
    client_secret: &str,
    device_code: &str,
    region: &str,
) -> Result<TokenPollResponse, KiroError> {
    let client = reqwest::Client::new();
    let url = format!("{}/token", oidc_base_url(region));

    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&serde_json::json!({
            "clientId": client_id,
            "clientSecret": client_secret,
            "deviceCode": device_code,
            "grantType": "urn:ietf:params:oauth:grant-type:device_code",
        }))
        .send()
        .await
        .map_err(|e| KiroError {
            error: "request_failed".to_string(),
            error_description: Some(e.to_string()),
        })?;

    response.json().await.map_err(|e| KiroError {
        error: "parse_error".to_string(),
        error_description: Some(e.to_string()),
    })
}

// ---------------------------------------------------------------------------
// 4. Social login
// ---------------------------------------------------------------------------

/// Build the Kiro social login URL for the given identity provider.
///
/// `idp` should be `"Google"` or `"Github"`.
///
/// The `redirect_uri` **MUST** be `kiro://kiro.kiroAgent/authenticate-success`
/// — it is whitelisted in Cognito and other values will be rejected.
pub fn build_social_login_url(idp: &str, code_challenge: &str, state: &str) -> String {
    format!(
        "{}/login?idp={idp}&redirect_uri={encoded_redirect}&code_challenge={code_challenge}&code_challenge_method=S256&state={state}&prompt=select_account",
        auth_service_base_url(),
        encoded_redirect = KIRO_SOCIAL_REDIRECT_URI_ENCODED,
    )
}

/// Exchange an authorization code obtained from the social login flow for tokens.
///
/// POST `{auth_service_base}/oauth/token`
pub async fn exchange_social_code(
    code: &str,
    code_verifier: &str,
) -> Result<TokenPollResponse, KiroError> {
    let client = reqwest::Client::new();
    let url = format!("{}/oauth/token", auth_service_base_url());

    let body = serde_json::json!({
        "code": code,
        "code_verifier": code_verifier,
        "redirect_uri": KIRO_SOCIAL_REDIRECT_URI,
    });

    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| KiroError {
            error: "request_failed".to_string(),
            error_description: Some(e.to_string()),
        })?;

    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(KiroError {
            error: "token_exchange_failed".to_string(),
            error_description: Some(text),
        });
    }

    response.json().await.map_err(|e| KiroError {
        error: "parse_error".to_string(),
        error_description: Some(e.to_string()),
    })
}

// ---------------------------------------------------------------------------
// 5. Import token validation
// ---------------------------------------------------------------------------

/// Check whether a token is a valid Kiro import token.
///
/// Valid Kiro import tokens start with `"aorAAAAAG"`.
pub fn validate_import_token(token: &str) -> bool {
    token.starts_with("aorAAAAAG")
}

// ---------------------------------------------------------------------------
// 6. Refresh
// ---------------------------------------------------------------------------

/// Refresh a Kiro token, switching between AWS OIDC and Cognito refresh
/// depending on the `auth_method`.
///
/// - **BuilderId / Idc**: POST `https://oidc.{region}.amazonaws.com/token`
///   with `clientId`, `clientSecret`, and `grantType=refresh_token`.
/// - **Google / Github / Imported**: POST `{auth_service_base}/refreshToken`
///   with only the refresh token.
pub async fn refresh(
    auth_method: KiroAuthMethod,
    refresh_token: &str,
    client_id: Option<&str>,
    client_secret: Option<&str>,
    region: Option<&str>,
) -> Result<TokenPollResponse, KiroError> {
    let inner = reqwest::Client::new();

    match auth_method {
        KiroAuthMethod::BuilderId | KiroAuthMethod::Idc => {
            let region = region.unwrap_or("us-east-1");
            let client_id = client_id.ok_or_else(|| KiroError {
                error: "missing_client_id".to_string(),
                error_description: Some(
                    "client_id is required for AWS OIDC refresh".to_string(),
                ),
            })?;
            let client_secret = client_secret.ok_or_else(|| KiroError {
                error: "missing_client_secret".to_string(),
                error_description: Some(
                    "client_secret is required for AWS OIDC refresh".to_string(),
                ),
            })?;

            let url = format!("{}/token", oidc_base_url(region));
            let body = serde_json::json!({
                "clientId": client_id,
                "clientSecret": client_secret,
                "refreshToken": refresh_token,
                "grantType": "refresh_token",
            });

            let response = inner
                .post(&url)
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| KiroError {
                    error: "request_failed".to_string(),
                    error_description: Some(e.to_string()),
                })?;

            if !response.status().is_success() {
                let text = response.text().await.unwrap_or_default();
                return Err(KiroError {
                    error: "token_refresh_failed".to_string(),
                    error_description: Some(text),
                });
            }

            response.json().await.map_err(|e| KiroError {
                error: "parse_error".to_string(),
                error_description: Some(e.to_string()),
            })
        }
        KiroAuthMethod::Google | KiroAuthMethod::Github | KiroAuthMethod::Imported => {
            let url = format!("{}/refreshToken", auth_service_base_url());
            let body = serde_json::json!({
                "refreshToken": refresh_token,
            });

            let response = inner
                .post(&url)
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| KiroError {
                    error: "request_failed".to_string(),
                    error_description: Some(e.to_string()),
                })?;

            if !response.status().is_success() {
                let text = response.text().await.unwrap_or_default();
                return Err(KiroError {
                    error: "token_refresh_failed".to_string(),
                    error_description: Some(text),
                });
            }

            response.json().await.map_err(|e| KiroError {
                error: "parse_error".to_string(),
                error_description: Some(e.to_string()),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- KiroAuthMethod ----------------------------------------------------

    #[test]
    fn test_kiro_auth_method_as_str() {
        assert_eq!(KiroAuthMethod::BuilderId.as_str(), "builder-id");
        assert_eq!(KiroAuthMethod::Idc.as_str(), "idc");
        assert_eq!(KiroAuthMethod::Google.as_str(), "google");
        assert_eq!(KiroAuthMethod::Github.as_str(), "github");
        assert_eq!(KiroAuthMethod::Imported.as_str(), "imported");
    }

    #[test]
    fn test_kiro_auth_method_from_str() {
        assert_eq!(
            KiroAuthMethod::from_str("builder-id"),
            Some(KiroAuthMethod::BuilderId)
        );
        assert_eq!(
            KiroAuthMethod::from_str("builder_id"),
            Some(KiroAuthMethod::BuilderId)
        );
        assert_eq!(KiroAuthMethod::from_str("idc"), Some(KiroAuthMethod::Idc));
        assert_eq!(
            KiroAuthMethod::from_str("google"),
            Some(KiroAuthMethod::Google)
        );
        assert_eq!(
            KiroAuthMethod::from_str("github"),
            Some(KiroAuthMethod::Github)
        );
        assert_eq!(
            KiroAuthMethod::from_str("imported"),
            Some(KiroAuthMethod::Imported)
        );
        assert_eq!(KiroAuthMethod::from_str("unknown"), None);
    }

    // -- import token validation -------------------------------------------

    #[test]
    fn test_validate_import_token() {
        assert!(validate_import_token("aorAAAAAG-some-token"));
        assert!(validate_import_token("aorAAAAAG"));
        assert!(!validate_import_token("invalid-token"));
        assert!(!validate_import_token(""));
    }

    // -- social login URL builder ------------------------------------------

    #[test]
    fn test_build_social_login_url_google() {
        let url = build_social_login_url("Google", "challenge123", "state456");
        assert!(url.contains("idp=Google"));
        assert!(url.contains(
            "redirect_uri=kiro%3A%2F%2Fkiro.kiroAgent%2Fauthenticate-success"
        ));
        assert!(url.contains("code_challenge=challenge123"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=state456"));
        assert!(url.contains("prompt=select_account"));
    }

    #[test]
    fn test_build_social_login_url_github() {
        let url = build_social_login_url("Github", "ch789", "state012");
        assert!(url.contains("idp=Github"));
    }

    // -- deserialization tests ---------------------------------------------

    #[test]
    fn test_client_registration_response_deserialize() {
        let json =
            r#"{"clientId":"c1","clientSecret":"s1","clientSecretExpiresAt":12345}"#;
        let resp: ClientRegistrationResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.client_id, "c1");
        assert_eq!(resp.client_secret, "s1");
        assert_eq!(resp.client_secret_expires_at, Some(12345));
    }

    #[test]
    fn test_client_registration_response_minimal() {
        let json = r#"{"clientId":"c1","clientSecret":"s1"}"#;
        let resp: ClientRegistrationResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.client_id, "c1");
        assert_eq!(resp.client_secret, "s1");
        assert!(resp.client_secret_expires_at.is_none());
    }

    #[test]
    fn test_device_authorization_response_full() {
        let json = r#"{"deviceCode":"dc1","userCode":"UC1","verificationUri":"https://x.com","verificationUriComplete":"https://x.com/uc","expiresIn":600,"interval":5}"#;
        let resp: DeviceAuthorizationResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.device_code, "dc1");
        assert_eq!(resp.user_code, "UC1");
        assert_eq!(resp.verification_uri, "https://x.com");
        assert_eq!(
            resp.verification_uri_complete,
            Some("https://x.com/uc".to_string())
        );
        assert_eq!(resp.expires_in, Some(600));
        assert_eq!(resp.interval, Some(5));
    }

    #[test]
    fn test_device_authorization_response_minimal() {
        let json = r#"{"deviceCode":"d","userCode":"u","verificationUri":"https://x.com"}"#;
        let resp: DeviceAuthorizationResponse = serde_json::from_str(json).unwrap();
        assert!(resp.verification_uri_complete.is_none());
        assert!(resp.expires_in.is_none());
        assert!(resp.interval.is_none());
    }

    #[test]
    fn test_token_poll_response_success() {
        let json = r#"{"accessToken":"at1","refreshToken":"rt1","expiresIn":7200,"profileArn":"arn:aws:iam::123:role/Kiro"}"#;
        let resp: TokenPollResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.access_token, Some("at1".to_string()));
        assert_eq!(resp.refresh_token, Some("rt1".to_string()));
        assert_eq!(resp.expires_in, Some(7200));
        assert_eq!(
            resp.profile_arn,
            Some("arn:aws:iam::123:role/Kiro".to_string())
        );
    }

    #[test]
    fn test_token_poll_response_pending() {
        let json = r#"{"error":"authorization_pending","error_description":"Waiting"}"#;
        let resp: TokenPollResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.error, Some("authorization_pending".to_string()));
        assert_eq!(resp.error_description, Some("Waiting".to_string()));
        assert!(resp.access_token.is_none());
    }

    #[test]
    fn test_token_poll_response_slow_down() {
        let json = r#"{"error":"slow_down","error_description":"Slow down"}"#;
        let resp: TokenPollResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.error, Some("slow_down".to_string()));
    }

    #[test]
    fn test_token_poll_response_expired() {
        let json = r#"{"error":"expired_token","error_description":"Expired"}"#;
        let resp: TokenPollResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.error, Some("expired_token".to_string()));
    }

    #[test]
    fn test_token_poll_response_access_denied() {
        let json = r#"{"error":"access_denied","error_description":"Denied"}"#;
        let resp: TokenPollResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.error, Some("access_denied".to_string()));
    }

    // -- KiroError display -------------------------------------------------

    #[test]
    fn test_kiro_error_display_with_description() {
        let err = KiroError {
            error: "test_error".to_string(),
            error_description: Some("something went wrong".to_string()),
        };
        let msg = format!("{err}");
        assert!(msg.contains("test_error"));
        assert!(msg.contains("something went wrong"));
    }

    #[test]
    fn test_kiro_error_display_no_description() {
        let err = KiroError {
            error: "bare_error".to_string(),
            error_description: None,
        };
        let msg = format!("{err}");
        assert!(msg.contains("bare_error"));
    }
}
