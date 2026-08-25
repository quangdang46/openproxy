use crate::oauth::{DeviceCodeResponse, OAuthError, OAuthProviderConfig, TokenResponse};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct KiloCodeInitiateResponse {
    code: String,
    verification_url: String,
    #[serde(rename = "expiresIn")]
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct KiloCodePollResponse {
    status: String,
    token: Option<String>,
    user_email: Option<String>,
    #[serde(rename = "userEmail")]
    user_email_alt: Option<String>,
}

pub async fn kilocode_start_device_flow(
    provider_config: &OAuthProviderConfig,
) -> Result<DeviceCodeResponse, OAuthError> {
    let client = Client::new();

    let initiate_url = provider_config
        .get_param("initiate_url")
        .ok_or_else(|| OAuthError {
            error: "missing_initiate_url".to_string(),
            error_description: Some("KiloCode initiate_url not configured".to_string()),
        })?;

    let response = client
        .post(initiate_url)
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .map_err(|e| OAuthError {
            error: "request_failed".to_string(),
            error_description: Some(e.to_string()),
        })?;

    if !response.status().is_success() {
        let error_text = response.text().await.unwrap_or_default();
        return Err(OAuthError {
            error: "initiation_failed".to_string(),
            error_description: Some(format!("Device auth initiation failed: {}", error_text)),
        });
    }

    let data: KiloCodeInitiateResponse = response.json().await.map_err(|e| OAuthError {
        error: "parse_error".to_string(),
        error_description: Some(e.to_string()),
    })?;

    Ok(DeviceCodeResponse {
        device_code: data.code.clone(),
        user_code: data.code,
        verification_uri: data.verification_url.clone(),
        verification_uri_complete: Some(data.verification_url),
        interval: 3,
        expires_in: data.expires_in.map(|v| v as i64),
    })
}

pub async fn kilocode_poll_for_token(
    provider_config: &OAuthProviderConfig,
    device_code: &str,
) -> Result<TokenResponse, OAuthError> {
    let client = Client::new();

    let poll_url_base = provider_config
        .get_param("poll_url_base")
        .ok_or_else(|| OAuthError {
            error: "missing_poll_url_base".to_string(),
            error_description: Some("KiloCode poll_url_base not configured".to_string()),
        })?;

    let poll_url = format!("{}/{}", poll_url_base.trim_end_matches('/'), device_code);

    let response = client.get(&poll_url).send().await.map_err(|e| OAuthError {
        error: "request_failed".to_string(),
        error_description: Some(e.to_string()),
    })?;

    let status = response.status();

    if status.as_u16() == 202 {
        return Err(OAuthError {
            error: "authorization_pending".to_string(),
            error_description: None,
        });
    }

    if status.as_u16() == 403 {
        return Err(OAuthError {
            error: "access_denied".to_string(),
            error_description: Some("Authorization denied by user".to_string()),
        });
    }

    if status.as_u16() == 410 {
        return Err(OAuthError {
            error: "expired_token".to_string(),
            error_description: Some("Authorization code expired".to_string()),
        });
    }

    if !status.is_success() {
        return Err(OAuthError {
            error: "poll_failed".to_string(),
            error_description: Some(format!("Poll failed: {}", status)),
        });
    }

    let data: KiloCodePollResponse = response.json().await.map_err(|e| OAuthError {
        error: "parse_error".to_string(),
        error_description: Some(e.to_string()),
    })?;

    if data.status == "approved" {
        if let Some(token) = data.token {
            let email = data.user_email.or(data.user_email_alt);
            return Ok(TokenResponse {
                access_token: token,
                refresh_token: None,
                expires_in: None,
                id_token: None,
                token_type: Some("Bearer".to_string()),
                scope: None,
            });
        }
    }

    Err(OAuthError {
        error: "authorization_pending".to_string(),
        error_description: None,
    })
}
