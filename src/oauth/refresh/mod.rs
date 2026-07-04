//! Per-provider token refresh providers.
//!
//! Defines the `RefreshProvider` trait and per-provider implementations.
//! Re-exports shared types and helpers from `token_refresh.rs`.

use async_trait::async_trait;

pub use crate::oauth::token_refresh::{
    dedup_refresh, needs_refresh, needs_refresh_with_lead, refresh_with_retry, RefreshResult,
};

/// Trait for per-provider token refresh.
///
/// Each provider implements this trait to encapsulate its own token-refresh
/// logic (URL, payload format, authentication, response parsing).
#[async_trait]
pub trait RefreshProvider: Send + Sync {
    /// Refresh a token given the refresh_token string.
    async fn refresh(&self, refresh_token: &str) -> Result<RefreshResult, String>;
}

// ---------------------------------------------------------------------------
// Shared helpers (duplicated from token_refresh.rs so that per-provider
// modules can use them without modifying token_refresh.rs).
// ---------------------------------------------------------------------------

/// Send a form-urlencoded POST and parse the JSON response into a RefreshResult.
pub(crate) async fn refresh_form_token(
    url: &str,
    fields: Vec<(&str, &str)>,
) -> Result<RefreshResult, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .header(reqwest::header::ACCEPT, "application/json")
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
pub(crate) async fn parse_json_refresh_response(
    resp: reqwest::Response,
) -> Result<RefreshResult, String> {
    let payload: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse refresh response: {e}"))?;

    let access_token = payload
        .get("access_token")
        .or_else(|| payload.get("accessToken"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "Refresh response did not include access_token".to_string())?;

    Ok(RefreshResult {
        access_token: access_token.to_string(),
        refresh_token: payload
            .get("refresh_token")
            .or_else(|| payload.get("refreshToken"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        expires_in: payload
            .get("expires_in")
            .or_else(|| payload.get("expiresIn"))
            .and_then(serde_json::Value::as_i64),
    })
}

pub mod codex;
pub mod openai;
