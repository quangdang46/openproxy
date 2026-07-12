//! Kiro OAuth 2.0 authentication methods
//!
//! Supports 6 auth methods (the `KiroAuthMethod` enum):
//!
//! 1. **BuilderId** — AWS SSO Builder ID device code flow (OIDC client registration +
//!    device authorization + token poll).
//! 2. **Idc** — AWS IAM Identity Center device code flow (same protocol, different
//!    `startUrl` / region).
//! 3. **Google** — Social login via Kiro's Cognito-backed identity provider.
//! 4. **Github** — Social login via Kiro's Cognito-backed identity provider.
//! 5. **Imported** — Import an existing Kiro AWS SSO refresh token (`aorAAAAAG...`).
//! 6. **ExternalIdp** — Microsoft Entra external IdP (CLIProxyAPI enterprise import).
//!    Refresh is form-urlencoded against a validated Microsoft token endpoint.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default Kiro auth service base URL (Cognito).
const KIRO_AUTH_SERVICE: &str = "https://prod.us-east-1.auth.desktop.kiro.dev";
/// Pre-encoded version of the social redirect URI for URL building.
const KIRO_SOCIAL_REDIRECT_URI_ENCODED: &str = "kiro%3A%2F%2Fkiro.kiroAgent%2Fauthenticate-success";
/// The raw redirect URI. **Must** be this exact value (Cognito whitelist).
const KIRO_SOCIAL_REDIRECT_URI: &str = "kiro://kiro.kiroAgent/authenticate-success";

// ---------------------------------------------------------------------------
// KiroAuthMethod
// ---------------------------------------------------------------------------

/// The authentication methods supported by Kiro.
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
    /// Microsoft Entra / external IdP (CLIProxyAPI enterprise).
    /// Serialized as `external_idp` (snake_case) for connection PSD parity.
    #[serde(rename = "external_idp", alias = "external-idp")]
    ExternalIdp,
}

impl KiroAuthMethod {
    /// Return the canonical string representation.
    ///
    /// Most methods are kebab-case; `ExternalIdp` uses snake_case `external_idp`
    /// because that is the wire value stored in connection `providerSpecificData`.
    pub fn as_str(&self) -> &'static str {
        match self {
            KiroAuthMethod::BuilderId => "builder-id",
            KiroAuthMethod::Idc => "idc",
            KiroAuthMethod::Google => "google",
            KiroAuthMethod::Github => "github",
            KiroAuthMethod::Imported => "imported",
            KiroAuthMethod::ExternalIdp => "external_idp",
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
            "external_idp" | "external-idp" | "externalidp" => Some(Self::ExternalIdp),
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

// ---------------------------------------------------------------------------
// External IdP (Microsoft Entra) — CLIProxyAPI enterprise import
// ---------------------------------------------------------------------------

/// Allowed Microsoft login hosts for external_idp token endpoints.
const MICROSOFT_TOKEN_ENDPOINT_HOSTS: &[&str] = &[
    "login.microsoftonline.com",
    "login.microsoft.com",
    "login.windows.net",
];

const EXTERNAL_IDP_DEFAULT_EXPIRES_IN: i64 = 3600;

/// Validated parameters for an external_idp form-urlencoded refresh.
#[derive(Debug, Clone)]
pub struct ExternalIdpRefreshParams {
    pub token_endpoint: String,
    pub client_id: String,
    pub scope: String,
}

/// Validate that `raw_endpoint` is an https Microsoft login token URL.
///
/// Mirrors 9router `validateMicrosoftTokenEndpoint`.
pub fn validate_microsoft_token_endpoint(raw_endpoint: &str) -> Result<String, KiroError> {
    let token_endpoint = raw_endpoint.trim();
    if token_endpoint.is_empty() {
        return Err(KiroError {
            error: "token_endpoint_required".to_string(),
            error_description: Some("token_endpoint is required".to_string()),
        });
    }

    let parsed = url::Url::parse(token_endpoint).map_err(|_| KiroError {
        error: "invalid_token_endpoint".to_string(),
        error_description: Some("token_endpoint must be a valid URL".to_string()),
    })?;

    if parsed.scheme() != "https" {
        return Err(KiroError {
            error: "invalid_token_endpoint".to_string(),
            error_description: Some("token_endpoint must use https".to_string()),
        });
    }

    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    if !MICROSOFT_TOKEN_ENDPOINT_HOSTS.iter().any(|h| *h == host) {
        return Err(KiroError {
            error: "invalid_token_endpoint".to_string(),
            error_description: Some(
                "token_endpoint must be a Microsoft login endpoint".to_string(),
            ),
        });
    }

    Ok(parsed.to_string())
}

/// Normalize scopes from a string or JSON array into a space-joined string.
pub fn normalize_external_idp_scope(scopes: &serde_json::Value) -> String {
    match scopes {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        serde_json::Value::String(s) => s.trim().to_string(),
        _ => String::new(),
    }
}

/// Build validated external_idp refresh parameters from connection PSD.
///
/// Mirrors 9router `buildExternalIdpRefreshParams`.
pub fn build_external_idp_refresh_params(
    provider_specific_data: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<ExternalIdpRefreshParams, KiroError> {
    let client_id = provider_specific_data
        .get("clientId")
        .or_else(|| provider_specific_data.get("client_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| KiroError {
            error: "missing_client_id".to_string(),
            error_description: Some("clientId is required for external_idp refresh".to_string()),
        })?
        .to_string();

    let token_endpoint_raw = provider_specific_data
        .get("tokenEndpoint")
        .or_else(|| provider_specific_data.get("token_endpoint"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| KiroError {
            error: "missing_token_endpoint".to_string(),
            error_description: Some(
                "tokenEndpoint is required for external_idp refresh".to_string(),
            ),
        })?;
    let token_endpoint = validate_microsoft_token_endpoint(token_endpoint_raw)?;

    let scope = provider_specific_data
        .get("scope")
        .or_else(|| provider_specific_data.get("scopes"))
        .map(normalize_external_idp_scope)
        .unwrap_or_default();
    if scope.is_empty() {
        return Err(KiroError {
            error: "missing_scope".to_string(),
            error_description: Some("scope is required for external_idp refresh".to_string()),
        });
    }

    Ok(ExternalIdpRefreshParams {
        token_endpoint,
        client_id,
        scope,
    })
}

/// Detect whether connection PSD identifies an external_idp auth method.
pub fn is_external_idp_auth(
    provider_specific_data: &std::collections::BTreeMap<String, serde_json::Value>,
) -> bool {
    provider_specific_data
        .get("authMethod")
        .or_else(|| provider_specific_data.get("auth_method"))
        .and_then(serde_json::Value::as_str)
        .map(|s| matches!(s, "external_idp" | "external-idp" | "externalidp"))
        .unwrap_or(false)
}

/// Normalize CLIProxyAPI / external_idp auth JSON into tokens + PSD.
///
/// Mirrors 9router `normalizeKiroExternalIdpAuth`.
pub fn normalize_kiro_external_idp_auth(
    raw: &serde_json::Value,
) -> Result<NormalizedExternalIdpAuth, KiroError> {
    let input = if let Some(s) = raw.as_str() {
        serde_json::from_str::<serde_json::Value>(s).map_err(|_| KiroError {
            error: "invalid_auth_json".to_string(),
            error_description: Some("CLIProxyAPI auth JSON is invalid".to_string()),
        })?
    } else {
        raw.clone()
    };

    if !input.is_object() {
        return Err(KiroError {
            error: "invalid_auth_json".to_string(),
            error_description: Some("CLIProxyAPI auth JSON is required".to_string()),
        });
    }

    let auth_method = input
        .get("auth_method")
        .or_else(|| input.get("authMethod"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if !auth_method.is_empty() && auth_method != "external_idp" {
        return Err(KiroError {
            error: "unsupported_auth_method".to_string(),
            error_description: Some(
                "Only external_idp Kiro auth is supported by this importer".to_string(),
            ),
        });
    }

    let access_token = input
        .get("access_token")
        .or_else(|| input.get("accessToken"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| KiroError {
            error: "missing_access_token".to_string(),
            error_description: Some("access_token is required".to_string()),
        })?
        .to_string();
    let refresh_token = input
        .get("refresh_token")
        .or_else(|| input.get("refreshToken"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| KiroError {
            error: "missing_refresh_token".to_string(),
            error_description: Some("refresh_token is required".to_string()),
        })?
        .to_string();
    let client_id = input
        .get("client_id")
        .or_else(|| input.get("clientId"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| KiroError {
            error: "missing_client_id".to_string(),
            error_description: Some("client_id is required".to_string()),
        })?
        .to_string();
    let token_endpoint = validate_microsoft_token_endpoint(
        input
            .get("token_endpoint")
            .or_else(|| input.get("tokenEndpoint"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(""),
    )?;
    let profile_arn = input
        .get("profile_arn")
        .or_else(|| input.get("profileArn"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| KiroError {
            error: "missing_profile_arn".to_string(),
            error_description: Some("profile_arn is required".to_string()),
        })?
        .to_string();
    let region = input
        .get("region")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("us-east-1")
        .to_string();
    let scope = input
        .get("scopes")
        .or_else(|| input.get("scope"))
        .map(normalize_external_idp_scope)
        .unwrap_or_default();
    if scope.is_empty() {
        return Err(KiroError {
            error: "missing_scope".to_string(),
            error_description: Some("scopes is required".to_string()),
        });
    }

    let expires_at = resolve_external_idp_expires_at(&input, &access_token);
    let payload = decode_jwt_payload(&access_token);
    let email = input
        .get("email")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            payload.as_ref().and_then(|p| {
                p.get("email")
                    .or_else(|| p.get("preferred_username"))
                    .or_else(|| p.get("upn"))
                    .or_else(|| p.get("sub"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
        });

    let mut provider_specific_data = std::collections::BTreeMap::new();
    provider_specific_data.insert(
        "profileArn".to_string(),
        serde_json::Value::String(profile_arn),
    );
    provider_specific_data.insert("region".to_string(), serde_json::Value::String(region));
    provider_specific_data.insert(
        "authMethod".to_string(),
        serde_json::Value::String("external_idp".to_string()),
    );
    provider_specific_data.insert(
        "provider".to_string(),
        serde_json::Value::String("CLIProxyAPI".to_string()),
    );
    provider_specific_data.insert("clientId".to_string(), serde_json::Value::String(client_id));
    provider_specific_data.insert(
        "tokenEndpoint".to_string(),
        serde_json::Value::String(token_endpoint),
    );
    provider_specific_data.insert("scope".to_string(), serde_json::Value::String(scope));

    Ok(NormalizedExternalIdpAuth {
        access_token,
        refresh_token,
        expires_at,
        email,
        provider_specific_data,
    })
}

/// Tokens + PSD produced by [`normalize_kiro_external_idp_auth`].
#[derive(Debug, Clone)]
pub struct NormalizedExternalIdpAuth {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: String,
    pub email: Option<String>,
    pub provider_specific_data: std::collections::BTreeMap<String, serde_json::Value>,
}

fn resolve_external_idp_expires_at(input: &serde_json::Value, access_token: &str) -> String {
    for key in ["expired", "expires_at", "expiresAt"] {
        if let Some(explicit) = input.get(key).and_then(serde_json::Value::as_str) {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(explicit) {
                return dt.with_timezone(&chrono::Utc).to_rfc3339();
            }
            if let Ok(dt) = chrono::DateTime::parse_from_str(explicit, "%+") {
                return dt.with_timezone(&chrono::Utc).to_rfc3339();
            }
        }
    }

    let expires_in = input
        .get("expires_in")
        .or_else(|| input.get("expiresIn"))
        .and_then(serde_json::Value::as_i64)
        .filter(|v| *v > 0);
    if let Some(secs) = expires_in {
        return (chrono::Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339();
    }

    if let Some(payload) = decode_jwt_payload(access_token) {
        if let Some(exp) = payload.get("exp").and_then(serde_json::Value::as_i64) {
            return chrono::DateTime::from_timestamp(exp, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| {
                    (chrono::Utc::now()
                        + chrono::Duration::seconds(EXTERNAL_IDP_DEFAULT_EXPIRES_IN))
                    .to_rfc3339()
                });
        }
    }

    (chrono::Utc::now() + chrono::Duration::seconds(EXTERNAL_IDP_DEFAULT_EXPIRES_IN)).to_rfc3339()
}

/// Decode the middle segment of a JWT into a JSON object (no signature verify).
fn decode_jwt_payload(jwt: &str) -> Option<serde_json::Value> {
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    use base64::Engine;
    let mut padded = parts[1].replace('-', "+").replace('_', "/");
    let rem = padded.len() % 4;
    if rem != 0 {
        padded.push_str(&"=".repeat(4 - rem));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(padded)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Refresh a Kiro token, switching between AWS OIDC and Cognito refresh
/// depending on the `auth_method`.
///
/// - **BuilderId / Idc**: POST `https://oidc.{region}.amazonaws.com/token`
///   with `clientId`, `clientSecret`, and `grantType=refresh_token`.
/// - **Google / Github / Imported**: POST `{auth_service_base}/refreshToken`
///   with only the refresh token.
/// - **ExternalIdp**: form-urlencoded POST to Microsoft `tokenEndpoint`
///   (`grant_type=refresh_token&client_id&refresh_token&scope`).
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
                error_description: Some("client_id is required for AWS OIDC refresh".to_string()),
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
        KiroAuthMethod::ExternalIdp => {
            // External IdP needs clientId / tokenEndpoint / scope from PSD;
            // use the dedicated form-urlencoded helper via token_refresh.
            Err(KiroError {
                error: "external_idp_requires_psd".to_string(),
                error_description: Some(
                    "use refresh_external_idp_token with provider_specific_data".to_string(),
                ),
            })
        }
    }
}

/// Refresh an external_idp token via Microsoft Entra form-urlencoded POST.
///
/// Mirrors 9router:
/// `POST tokenEndpoint` with
/// `grant_type=refresh_token&client_id&refresh_token&scope`.
pub async fn refresh_external_idp_token(
    refresh_token: &str,
    provider_specific_data: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<TokenPollResponse, KiroError> {
    if refresh_token.trim().is_empty() {
        return Err(KiroError {
            error: "missing_refresh_token".to_string(),
            error_description: Some("refresh token is required".to_string()),
        });
    }

    let params = build_external_idp_refresh_params(provider_specific_data)?;
    let client = reqwest::Client::new();
    let response = client
        .post(&params.token_endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", params.client_id.as_str()),
            ("refresh_token", refresh_token),
            ("scope", params.scope.as_str()),
        ])
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

    // Microsoft returns snake_case access_token / refresh_token / expires_in.
    let payload: serde_json::Value = response.json().await.map_err(|e| KiroError {
        error: "parse_error".to_string(),
        error_description: Some(e.to_string()),
    })?;

    Ok(TokenPollResponse {
        access_token: payload
            .get("access_token")
            .or_else(|| payload.get("accessToken"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        refresh_token: payload
            .get("refresh_token")
            .or_else(|| payload.get("refreshToken"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        expires_in: payload
            .get("expires_in")
            .or_else(|| payload.get("expiresIn"))
            .and_then(serde_json::Value::as_i64),
        profile_arn: None,
        error: payload
            .get("error")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        error_description: payload
            .get("error_description")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    })
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
        assert_eq!(KiroAuthMethod::ExternalIdp.as_str(), "external_idp");
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
        assert_eq!(
            KiroAuthMethod::from_str("external_idp"),
            Some(KiroAuthMethod::ExternalIdp)
        );
        assert_eq!(
            KiroAuthMethod::from_str("external-idp"),
            Some(KiroAuthMethod::ExternalIdp)
        );
        assert_eq!(KiroAuthMethod::from_str("unknown"), None);
    }

    #[test]
    fn test_validate_microsoft_token_endpoint_ok() {
        let url = validate_microsoft_token_endpoint(
            "https://login.microsoftonline.com/tenant/oauth2/v2.0/token",
        )
        .unwrap();
        assert!(url.starts_with("https://login.microsoftonline.com/"));
    }

    #[test]
    fn test_validate_microsoft_token_endpoint_rejects_http() {
        assert!(validate_microsoft_token_endpoint(
            "http://login.microsoftonline.com/tenant/oauth2/v2.0/token"
        )
        .is_err());
    }

    #[test]
    fn test_validate_microsoft_token_endpoint_rejects_other_host() {
        assert!(
            validate_microsoft_token_endpoint("https://evil.example.com/oauth2/v2.0/token")
                .is_err()
        );
    }

    #[test]
    fn test_build_external_idp_refresh_params() {
        let mut pdata = std::collections::BTreeMap::new();
        pdata.insert(
            "clientId".into(),
            serde_json::Value::String("client-123".into()),
        );
        pdata.insert(
            "tokenEndpoint".into(),
            serde_json::Value::String(
                "https://login.microsoftonline.com/t/oauth2/v2.0/token".into(),
            ),
        );
        pdata.insert(
            "scope".into(),
            serde_json::Value::String("openid profile offline_access".into()),
        );
        let params = build_external_idp_refresh_params(&pdata).unwrap();
        assert_eq!(params.client_id, "client-123");
        assert_eq!(params.scope, "openid profile offline_access");
        assert!(params.token_endpoint.contains("login.microsoftonline.com"));
    }

    #[test]
    fn test_build_external_idp_refresh_params_missing_scope() {
        let mut pdata = std::collections::BTreeMap::new();
        pdata.insert(
            "clientId".into(),
            serde_json::Value::String("client-123".into()),
        );
        pdata.insert(
            "tokenEndpoint".into(),
            serde_json::Value::String(
                "https://login.microsoftonline.com/t/oauth2/v2.0/token".into(),
            ),
        );
        assert!(build_external_idp_refresh_params(&pdata).is_err());
    }

    #[test]
    fn test_normalize_kiro_external_idp_auth() {
        let raw = serde_json::json!({
            "auth_method": "external_idp",
            "access_token": "at-1",
            "refresh_token": "rt-1",
            "client_id": "cid",
            "token_endpoint": "https://login.microsoftonline.com/t/oauth2/v2.0/token",
            "profile_arn": "arn:aws:codewhisperer:us-east-1:123:profile/p",
            "scopes": ["openid", "profile", "offline_access"],
            "region": "us-west-2",
            "expires_in": 1800
        });
        let norm = normalize_kiro_external_idp_auth(&raw).unwrap();
        assert_eq!(norm.access_token, "at-1");
        assert_eq!(norm.refresh_token, "rt-1");
        assert_eq!(
            norm.provider_specific_data
                .get("authMethod")
                .and_then(|v| v.as_str()),
            Some("external_idp")
        );
        assert_eq!(
            norm.provider_specific_data
                .get("clientId")
                .and_then(|v| v.as_str()),
            Some("cid")
        );
        assert_eq!(
            norm.provider_specific_data
                .get("scope")
                .and_then(|v| v.as_str()),
            Some("openid profile offline_access")
        );
        assert!(norm.expires_at.len() > 10);
    }

    #[test]
    fn test_is_external_idp_auth() {
        let mut pdata = std::collections::BTreeMap::new();
        assert!(!is_external_idp_auth(&pdata));
        pdata.insert(
            "authMethod".into(),
            serde_json::Value::String("external_idp".into()),
        );
        assert!(is_external_idp_auth(&pdata));
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
        assert!(url.contains("redirect_uri=kiro%3A%2F%2Fkiro.kiroAgent%2Fauthenticate-success"));
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
        let json = r#"{"clientId":"c1","clientSecret":"s1","clientSecretExpiresAt":12345}"#;
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
