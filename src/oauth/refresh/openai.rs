//! OpenAI token refresh provider.
//!
//! Implements `RefreshProvider` for the OpenAI OAuth token refresh flow.
//! Uses the same Auth0 endpoint as Codex with form-urlencoded payload.

use async_trait::async_trait;

use super::RefreshProvider;
use crate::oauth::token_refresh::{dedup_refresh, RefreshResult};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// OpenAI / Codex OAuth client ID.
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// OpenAI / Codex token endpoint (default).
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
// OpenAiRefreshProvider
// ---------------------------------------------------------------------------

/// OpenAI token refresh provider.
///
/// Refreshes an OpenAI access token via form-urlencoded POST to the OpenAI
/// Auth0 token endpoint.
pub struct OpenAiRefreshProvider;

#[async_trait]
impl RefreshProvider for OpenAiRefreshProvider {
    async fn refresh(&self, refresh_token: &str) -> Result<RefreshResult, String> {
        let rt = refresh_token.to_string();
        dedup_refresh("openai", refresh_token, move || {
            let rt = rt.clone();
            async move { refresh_openai_token_impl(&rt).await }
        })
        .await
    }
}

/// The actual HTTP refresh call for OpenAI.
async fn refresh_openai_token_impl(refresh_token: &str) -> Result<RefreshResult, String> {
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
    fn test_codex_client_id_constants() {
        assert!(!CODEX_CLIENT_ID.is_empty());
    }
}
