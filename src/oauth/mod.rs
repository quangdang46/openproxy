//! OAuth 2.0 flows implementation
//!
//! Supports:
//! - PKCE Authorization Code Flow (claude, codex, gitlab)
//! - Device Code Flow (github, kiro, kimi-coding, kilocode, codebuddy)
//! - Import Token (cursor)

use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use url::form_urlencoded;

pub const TOKEN_EXPIRY_BUFFER_MS: u64 = 5 * 60 * 1000;

pub mod pending;
#[cfg(test)]
pub mod tests;

pub enum OAuthFlowKind {
    AuthorizationCodePkce,
    DeviceCode,
    ImportToken,
}

pub struct OAuthProviderConfig {
    pub auth_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    pub uses_pkce: bool,
    pub extra_params: BTreeMap<String, String>,
}

impl OAuthProviderConfig {
    pub fn build_auth_url(
        &self,
        client_id: &str,
        redirect_uri: &str,
        state: &str,
        code_challenge: &str,
    ) -> String {
        let mut pairs: Vec<(String, String)> = vec![
            ("client_id".to_string(), client_id.to_string()),
            ("redirect_uri".to_string(), redirect_uri.to_string()),
            ("response_type".to_string(), "code".to_string()),
            ("state".to_string(), state.to_string()),
        ];

        if self.uses_pkce {
            pairs.push(("code_challenge".to_string(), code_challenge.to_string()));
            pairs.push(("code_challenge_method".to_string(), "S256".to_string()));
        }

        if !self.scopes.is_empty() {
            pairs.push(("scope".to_string(), self.scopes.join(" ")));
        }

        for (key, value) in &self.extra_params {
            pairs.push((key.clone(), value.clone()));
        }

        let query_string = pairs
            .iter()
            .map(|(k, v)| format!("{}={}", k, form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>()))
            .collect::<Vec<_>>()
            .join("&");

        format!("{}?{}", self.auth_url, query_string)
    }
}

pub mod pkce {
    use super::*;

    pub fn generate_code_verifier() -> String {
        let mut random_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut random_bytes);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes)
    }

    pub fn generate_code_challenge(verifier: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let hash = hasher.finalize();
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash)
    }

    pub fn generate_verifier_and_challenge() -> (String, String) {
        let verifier = generate_code_verifier();
        let challenge = generate_code_challenge(&verifier);
        (verifier, challenge)
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub interval: u64,
    #[serde(default)]
    pub expires_in: Option<i64>,
}

/// Result of Kiro AWS SSO OIDC device flow initiation.
/// Contains the device code response plus the dynamically registered client credentials.
#[derive(Debug, Clone)]
pub struct KiroDeviceFlow {
    pub device_code: DeviceCodeResponse,
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct OAuthError {
    pub error: String,
    #[serde(default)]
    pub error_description: Option<String>,
}

pub struct RefreshRequest {
    pub refresh_token: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scopes: Vec<String>,
}

pub mod providers {
    use super::*;

    pub fn claude() -> OAuthProviderConfig {
        OAuthProviderConfig {
            auth_url: "https://auth.claude.ai/authorize".to_string(),
            token_url: "https://auth.claude.ai/token".to_string(),
            scopes: vec!["read".to_string(), "connect".to_string()],
            uses_pkce: true,
            extra_params: [("response_type".to_string(), "code".to_string())].into(),
        }
    }

    pub fn codex() -> OAuthProviderConfig {
        OAuthProviderConfig {
            auth_url: "https://codex.ai/oauth/authorize".to_string(),
            token_url: "https://codex.ai/oauth/token".to_string(),
            scopes: vec!["openid".to_string(), "profile".to_string(), "email".to_string()],
            uses_pkce: true,
            extra_params: [
                ("response_type".to_string(), "code".to_string()),
                ("prompt".to_string(), "select_account".to_string()),
            ]
            .into(),
        }
    }

    pub fn gitlab() -> OAuthProviderConfig {
        OAuthProviderConfig {
            auth_url: "https://gitlab.com/oauth/authorize".to_string(),
            token_url: "https://gitlab.com/oauth/token".to_string(),
            scopes: vec!["api".to_string(), "read_user".to_string()],
            uses_pkce: true,
            extra_params: [("response_type".to_string(), "code".to_string())].into(),
        }
    }

    pub fn github() -> OAuthProviderConfig {
        OAuthProviderConfig {
            auth_url: "https://github.com/login/device/code".to_string(),
            token_url: "https://github.com/login/oauth/access_token".to_string(),
            scopes: vec!["read:user".to_string(), "repo".to_string()],
            uses_pkce: false,
            extra_params: [("scope".to_string(), "read:user repo".to_string())].into(),
        }
    }

    pub fn kiro() -> OAuthProviderConfig {
        OAuthProviderConfig {
            auth_url: "https://kiro.ai/oauth/device/code".to_string(),
            token_url: "https://kiro.ai/oauth/token".to_string(),
            scopes: vec!["openid".to_string(), "profile".to_string()],
            uses_pkce: false,
            extra_params: [
                ("scope".to_string(), "openid profile".to_string()),
                ("oauth_extension".to_string(), "pkce".to_string()),
            ]
            .into(),
        }
    }

    pub fn kimi_coding() -> OAuthProviderConfig {
        OAuthProviderConfig {
            auth_url: "https://api.moonshot.cn/kimi-device/oauth/device/code".to_string(),
            token_url: "https://api.moonshot.cn/kimi-device/oauth/token".to_string(),
            scopes: vec!["kimi:read".to_string()],
            uses_pkce: false,
            extra_params: [
                ("client_id".to_string(), "kimi-coding-openproxy".to_string()),
            ]
            .into(),
        }
    }

    pub fn kilocode() -> OAuthProviderConfig {
        OAuthProviderConfig {
            auth_url: "https://api.kilo.ai/oauth/device/code".to_string(),
            token_url: "https://api.kilo.ai/oauth/token".to_string(),
            scopes: vec!["read".to_string()],
            uses_pkce: false,
            extra_params: [
                ("scope".to_string(), "read".to_string()),
                ("client_id".to_string(), "kilocode-openproxy".to_string()),
            ]
            .into(),
        }
    }

    pub fn codebuddy() -> OAuthProviderConfig {
        OAuthProviderConfig {
            auth_url: "https://copilot.tencent.com/oauth/device/code".to_string(),
            token_url: "https://copilot.tencent.com/oauth/token".to_string(),
            scopes: vec!["read".to_string()],
            uses_pkce: false,
            extra_params: [
                ("scope".to_string(), "read".to_string()),
                ("client_id".to_string(), "codebuddy-openproxy".to_string()),
            ]
            .into(),
        }
    }

    pub fn gitlab_with_baseurl(base_url: &str) -> OAuthProviderConfig {
        OAuthProviderConfig {
            auth_url: format!("{}/oauth/authorize", base_url.trim_end_matches('/')),
            token_url: format!("{}/oauth/token", base_url.trim_end_matches('/')),
            scopes: vec!["api".to_string(), "read_user".to_string()],
            uses_pkce: true,
            extra_params: [("response_type".to_string(), "code".to_string())].into(),
        }
    }

    pub fn get_config(provider: &str) -> Option<OAuthProviderConfig> {
        match provider {
            "claude" => Some(claude()),
            "codex" => Some(codex()),
            "gitlab" => Some(gitlab()),
            "github" => Some(github()),
            "kiro" => Some(kiro()),
            "kimi-coding" => Some(kimi_coding()),
            "kilocode" => Some(kilocode()),
            "codebuddy" => Some(codebuddy()),
            _ => None,
        }
    }
}

pub mod device_code {
    use super::*;

    pub async fn start_device_flow(
        provider_config: &OAuthProviderConfig,
        client_id: &str,
    ) -> Result<DeviceCodeResponse, OAuthError> {
        let client = reqwest::Client::new();
        let params = [
            ("client_id", client_id),
            ("scope", &provider_config.scopes.join(" ")),
        ];
        let response = client
            .post(&provider_config.auth_url)
            .form(&params)
            .send()
            .await
            .map_err(|e| OAuthError {
                error: "request_failed".to_string(),
                error_description: Some(e.to_string()),
            })?;

        if !response.status().is_success() {
            let error: OAuthError = response.json().await.unwrap_or(OAuthError {
                error: "unknown_error".to_string(),
                error_description: None,
            });
            return Err(error);
        }

        response.json().await.map_err(|e| OAuthError {
            error: "parse_error".to_string(),
            error_description: Some(e.to_string()),
        })
    }

    pub async fn poll_for_token(
        provider_config: &OAuthProviderConfig,
        device_code: &str,
        _user_code: &str,
        interval_secs: u64,
    ) -> Result<TokenResponse, OAuthError> {
        let client = reqwest::Client::new();
        let mut current_interval = interval_secs;

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(current_interval)).await;

            let params = [
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", provider_config.extra_params.get("client_id").map(|s| s.as_str()).unwrap_or("openproxy")),
                ("device_code", device_code),
            ];
            let response = client
                .post(&provider_config.token_url)
                .form(&params)
                .send()
                .await
                .map_err(|e| OAuthError {
                    error: "request_failed".to_string(),
                    error_description: Some(e.to_string()),
                })?;

            let body: serde_json::Value = response.json().await.unwrap_or_default();
            let error = body.get("error").and_then(|e| e.as_str());

            match error {
                Some("authorization_pending") => continue,
                Some("slow_down") => {
                    current_interval = (current_interval * 2).min(60);
                    continue;
                }
                Some("access_denied") => {
                    return Err(OAuthError {
                        error: "access_denied".to_string(),
                        error_description: Some("User denied the authorization request".to_string()),
                    });
                }
                Some("expired_token") => {
                    return Err(OAuthError {
                        error: "expired_token".to_string(),
                        error_description: Some("The device code has expired".to_string()),
                    });
                }
                _ => {
                    if body.get("access_token").is_some() {
                        let token_response: TokenResponse = serde_json::from_value(body).map_err(|e| OAuthError {
                            error: "parse_error".to_string(),
                            error_description: Some(e.to_string()),
                        })?;
                        return Ok(token_response);
                    }
                    continue;
                }
            }
        }
    }

    pub async fn exchange_code_for_token(
        provider_config: &OAuthProviderConfig,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
        client_id: &str,
    ) -> Result<TokenResponse, OAuthError> {
        let client = reqwest::Client::new();
        let params = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", code_verifier),
        ];

        let response = client
            .post(&provider_config.token_url)
            .form(&params)
            .send()
            .await
            .map_err(|e| OAuthError {
                error: "request_failed".to_string(),
                error_description: Some(e.to_string()),
            })?;

        if !response.status().is_success() {
            let error: OAuthError = response.json().await.unwrap_or(OAuthError {
                error: "token_exchange_failed".to_string(),
                error_description: None,
            });
            return Err(error);
        }

        response.json().await.map_err(|e| OAuthError {
            error: "parse_error".to_string(),
            error_description: Some(e.to_string()),
        })
    }

    /// GitHub Copilot special: exchange OAuth token for Copilot token
    pub async fn exchange_github_copilot_token(oauth_token: &str) -> Result<TokenResponse, OAuthError> {
        let client = reqwest::Client::new();
        let response = client
            .post("https://github.com/copilot_internal/v1/token")
            .header("Authorization", format!("Bearer {}", oauth_token))
            .send()
            .await
            .map_err(|e| OAuthError {
                error: "request_failed".to_string(),
                error_description: Some(e.to_string()),
            })?;

        if !response.status().is_success() {
            let error: OAuthError = response.json().await.unwrap_or(OAuthError {
                error: "copilot_token_exchange_failed".to_string(),
                error_description: None,
            });
            return Err(error);
        }

        response.json().await.map_err(|e| OAuthError {
            error: "parse_error".to_string(),
            error_description: Some(e.to_string()),
        })
    }

    /// Kiro AWS SSO OIDC flow: combined client registration + device code start.
    /// Step 1: Register client with Kiro's OIDC endpoint.
    /// Step 2: Start device authorization using the registered client credentials.
    pub async fn kiro_start_device_flow() -> Result<super::KiroDeviceFlow, OAuthError> {
        let (client_id, client_secret) = kiro_register_client().await?;

        let kiro_config = super::providers::kiro();
        let device_resp = start_device_flow(&kiro_config, &client_id).await?;

        Ok(super::KiroDeviceFlow {
            device_code: device_resp,
            client_id,
            client_secret,
        })
    }

    /// Kiro AWS SSO OIDC flow - register client first, then standard device code flow
    pub async fn kiro_register_client() -> Result<(String, String), OAuthError> {
        let client = reqwest::Client::new();
        let client_id = format!("openproxy-{}", uuid::Uuid::new_v4());
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let expires_at = now_secs + 3600;

        let registration = serde_json::json!({
            "client_id": client_id,
            "client_name": "OpenProxy Device Client",
            "client_type": "public",
            "grant_types": ["urn:ietf:params:oauth:grant-type:device_code"],
            "redirect_uris": ["http://localhost:20128/oauth/callback"],
            "token_endpoint_auth_method": "none",
            "expires_at": expires_at
        });

        let response = client
            .post("https://kiro.ai/auth/oidc/register")
            .json(&registration)
            .send()
            .await
            .map_err(|e| OAuthError {
                error: "request_failed".to_string(),
                error_description: Some(e.to_string()),
            })?;

        if !response.status().is_success() {
            let error: OAuthError = response.json().await.unwrap_or(OAuthError {
                error: "client_registration_failed".to_string(),
                error_description: None,
            });
            return Err(error);
        }

        let resp_body: serde_json::Value = response.json().await.map_err(|e| OAuthError {
            error: "parse_error".to_string(),
            error_description: Some(e.to_string()),
        })?;

        let registered_client_id = resp_body.get("client_id")
            .and_then(|v| v.as_str())
            .unwrap_or(&client_id)
            .to_string();
        let client_secret = resp_body.get("client_secret")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();

        Ok((registered_client_id, client_secret))
    }
}

pub mod token_refresh {
    use super::*;

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
}

pub fn needs_refresh(expires_at: &Option<String>) -> bool {
    token_refresh::needs_refresh(expires_at)
}

pub fn expires_at_from_seconds(expires_in: i64) -> String {
    let expires = chrono::Utc::now() + chrono::Duration::seconds(expires_in);
    expires.to_rfc3339()
}


// Cursor import module - for importing tokens from Cursor's SQLite config.db
pub mod cursor_import {
    use crate::oauth::{TokenResponse, expires_at_from_seconds};

    #[derive(Clone)]
    pub struct CursorTokens {
        pub access_token: String,
        pub refresh_token: Option<String>,
        pub expires_at: Option<String>,
    }

    /// Read tokens from Cursor's SQLite config.db
    pub fn read_cursor_tokens(config_path: &str) -> Result<CursorTokens, String> {
        let conn = rusqlite::Connection::open(config_path)
            .map_err(|e| format!("Failed to open SQLite: {}", e))?;

        let result = conn.query_row(
            "SELECT access_token, refresh_token, expires_at FROM user_authentication LIMIT 1",
            [],
            |row| {
                let access_token: String = row.get(0)?;
                let refresh_token: Option<String> = row.get(1)?;
                let expires_at_raw: Option<i64> = row.get(2)?;
                Ok((access_token, refresh_token, expires_at_raw))
            },
        ).map_err(|e| format!("Failed to query: {}", e))?;

        let (access_token, refresh_token, expires_at_raw) = result;
        let expires_at = expires_at_raw.map(|secs| expires_at_from_seconds(secs));

        Ok(CursorTokens {
            access_token,
            refresh_token,
            expires_at,
        })
    }

    /// Convert CursorTokens to TokenResponse
    pub fn to_token_response(cursor: CursorTokens) -> TokenResponse {
        TokenResponse {
            access_token: cursor.access_token,
            refresh_token: cursor.refresh_token,
            expires_in: None,
            id_token: None,
            token_type: Some("Bearer".to_string()),
            scope: None,
        }
    }
}

// GitLab PAT (Personal Access Token) support
pub mod gitlab_pat {
    use crate::oauth::TokenResponse;

    pub fn create_token_response(pat: &str) -> TokenResponse {
        TokenResponse {
            access_token: pat.to_string(),
            refresh_token: None,
            expires_in: None,
            id_token: None,
            token_type: Some("Bearer".to_string()),
            scope: Some("api read_user".to_string()),
        }
    }

    pub fn is_valid_pat(pat: &str) -> bool {
        !pat.is_empty() && pat.len() >= 20
    }
}
