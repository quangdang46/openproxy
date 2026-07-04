//! Codex and Claude token refresh providers.
//!
//! Implements `RefreshProvider` for:
//! - **Claude** (Anthropic): POST JSON to Anthropic's OAuth token endpoint
//! - **Codex** (OpenAI / ChatGPT): POST form-urlencoded to OpenAI's Auth0 endpoint

use async_trait::async_trait;
use reqwest::header::{ACCEPT, CONTENT_TYPE};

use crate::oauth::token_refresh::{dedup_refresh, RefreshResult};
use super::RefreshProvider;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Claude OAuth client ID.
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Claude OAuth token URL.
const CLAUDE_TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";

/// Codex / ChatGPT OAuth client ID.
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Codex / ChatGPT token endpoint (default).
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the codex token URL (allows env-override).
fn codex_token_url() -> String {
    std::env::var("OPENPROXY_CODEX_TOKEN_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| CODEX_TOKEN_URL.to_string())
}

// ---------------------------------------------------------------------------
// ClaudeRefreshProvider
// ---------------------------------------------------------------------------

/// Claude (Anthropic) OAuth token refresh provider.
///
/// Refreshes a Claude access token via JSON POST to the Anthropic OAuth
/// token endpoint.
pub struct ClaudeRefreshProvider;

#[async_trait]
impl RefreshProvider for ClaudeRefreshProvider {
    async fn refresh(&self, refresh_token: &str) -> Result<RefreshResult, String> {
        let rt = refresh_token.to_string();
        dedup_refresh("claude", refresh_token, move || {
            let rt = rt.clone();
            async move { refresh_claude_token_impl(&rt).await }
        })
        .await
    }
}

/// The actual HTTP refresh call for Claude.
async fn refresh_claude_token_impl(refresh_token: &str) -> Result<RefreshResult, String> {
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
    super::parse_json_refresh_response(resp).await
}

// ---------------------------------------------------------------------------
// CodexRefreshProvider
// ---------------------------------------------------------------------------

/// Codex / ChatGPT OAuth token refresh provider.
///
/// Refreshes a Codex access token via form-urlencoded POST to the OpenAI
/// Auth0 token endpoint.
pub struct CodexRefreshProvider;

#[async_trait]
impl RefreshProvider for CodexRefreshProvider {
    async fn refresh(&self, refresh_token: &str) -> Result<RefreshResult, String> {
        let rt = refresh_token.to_string();
        dedup_refresh("codex", refresh_token, move || {
            let rt = rt.clone();
            async move { refresh_codex_token_impl(&rt).await }
        })
        .await
    }
}

/// The actual HTTP refresh call for Codex.
async fn refresh_codex_token_impl(refresh_token: &str) -> Result<RefreshResult, String> {
    super::refresh_form_token(
        &codex_token_url(),
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CODEX_CLIENT_ID),
        ],
    )
    .await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_codex_token_url_default() {
        let url = codex_token_url();
        assert_eq!(url, CODEX_TOKEN_URL);
    }

    #[test]
    fn test_claude_constants() {
        assert!(!CLAUDE_CLIENT_ID.is_empty());
        assert!(!CLAUDE_TOKEN_URL.is_empty());
    }

    #[test]
    fn test_codex_constants() {
        assert!(!CODEX_CLIENT_ID.is_empty());
        assert!(!CODEX_TOKEN_URL.is_empty());
    }
}
