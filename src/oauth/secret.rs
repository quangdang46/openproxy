//! OAuth client-secret resolution.
//!
//! Client secrets for OAuth providers are resolved exclusively from
//! environment variables at runtime. No static fallback values are hardcoded
//! in the repository. Operators MUST set the appropriate environment variables
//! before starting the server, otherwise OAuth flows for those providers
//! will not be available.
//!
//! Required environment variables:
//! - `IFLOW_CLIENT_SECRET`
//! - `QODER_CLIENT_SECRET`
//! - `ANTIGRAVITY_CLIENT_SECRET`
//! - `GEMINI_CLIENT_SECRET`

/// Get the iFlow client secret from environment variables.
/// Returns `None` if the environment variable is not set.
pub fn iflow_client_secret() -> Option<String> {
    std::env::var("IFLOW_CLIENT_SECRET").ok()
}

/// Get the qoder client secret from environment variables.
/// Returns `None` if the environment variable is not set.
pub fn qoder_client_secret() -> Option<String> {
    std::env::var("QODER_CLIENT_SECRET").ok()
}

/// Get the antigravity client secret from environment variables.
/// Returns `None` if the environment variable is not set.
pub fn antigravity_client_secret() -> Option<String> {
    std::env::var("ANTIGRAVITY_CLIENT_SECRET").ok()
}

/// Get the gemini-cli client secret from environment variables.
/// Returns `None` if the environment variable is not set.
pub fn gemini_cli_client_secret() -> Option<String> {
    std::env::var("GEMINI_CLIENT_SECRET").ok()
}
