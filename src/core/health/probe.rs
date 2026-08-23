//! Cheap health probe: `GET <base>/models` with a 5 s timeout.
//!
//! A model listing is the cheapest reliable liveness signal — it consumes no
//! tokens and no free-tier request quota on every provider tested against
//! OmniRoute's `discoverProviderModels`. No chat completion is issued, so a
//! degraded free-tier account is never billed for being probed.

use std::time::Duration;

use serde_json::Value;

use crate::core::executor::provider_config_base_url;
use crate::types::ProviderConnection;

/// Hard ceiling for a single probe. Anything slower is treated as a transport
/// failure (→ `server_error`, 5 min degrade).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Fully resolved probe request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeTarget {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

/// Result of a single probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutcome {
    /// Observed HTTP status. `None` on transport failure (DNS/TLS/timeout).
    pub http_status: Option<u16>,
    /// Human-readable failure reason, when the probe did not return `2xx`.
    pub error: Option<String>,
    /// Probed URL, for logging / diagnostics.
    pub url: String,
}

/// Which auth dialect a provider speaks. Derived from the configured upstream
/// URL so no separate provider table has to be kept in sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthDialect {
    /// `Authorization: Bearer <key>` (OpenAI-compatible).
    Bearer,
    /// `x-api-key` + `anthropic-version` (Anthropic-compatible).
    Anthropic,
    /// `x-goog-api-key` (Gemini / Generative Language).
    Google,
}

/// Build the probe request for a connection, or `None` when the connection
/// cannot be probed cheaply (unknown base URL or no API key).
pub fn probe_target(connection: &ProviderConnection) -> Option<ProbeTarget> {
    let api_key = api_key_for(connection)?;
    let base_url = base_url_for(connection)?;
    let dialect = auth_dialect(&connection.provider, &base_url);
    let url = models_url(&base_url, dialect);

    let headers = match dialect {
        AuthDialect::Bearer => vec![("authorization".into(), format!("Bearer {api_key}"))],
        AuthDialect::Anthropic => vec![
            ("x-api-key".into(), api_key),
            ("anthropic-version".into(), ANTHROPIC_VERSION.into()),
        ],
        AuthDialect::Google => vec![("x-goog-api-key".into(), api_key)],
    };

    Some(ProbeTarget { url, headers })
}

/// Execute the probe. Never panics and never returns an error type: every
/// failure mode maps onto a [`ProbeOutcome`] the registry can classify.
pub async fn probe_connection(
    client: &reqwest::Client,
    connection: &ProviderConnection,
) -> Option<ProbeOutcome> {
    let target = probe_target(connection)?;

    let mut request = client.get(&target.url).timeout(PROBE_TIMEOUT);
    for (name, value) in &target.headers {
        request = request.header(name, value);
    }

    match request.send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            // Drain so the pooled connection stays reusable.
            let _ = response.bytes().await;
            Some(ProbeOutcome {
                http_status: Some(status),
                error: if (200..300).contains(&status) {
                    None
                } else {
                    Some(format!("probe HTTP {status}"))
                },
                url: target.url,
            })
        }
        Err(error) => Some(ProbeOutcome {
            http_status: None,
            error: Some(format!("probe transport error: {error}")),
            url: target.url,
        }),
    }
}

/// Turn a configured chat/messages endpoint into its `/models` sibling.
fn models_url(base_url: &str, dialect: AuthDialect) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    // Gemini's configured base already points at `/v1beta/models`.
    if dialect == AuthDialect::Google || trimmed.ends_with("/models") {
        return trimmed.to_string();
    }

    for suffix in [
        "/chat/completions",
        "/messages",
        "/responses",
        "/completions",
    ] {
        if let Some(root) = trimmed.strip_suffix(suffix) {
            return format!("{}/models", root.trim_end_matches('/'));
        }
    }

    format!("{trimmed}/models")
}

fn auth_dialect(provider: &str, base_url: &str) -> AuthDialect {
    let provider = provider.to_ascii_lowercase();
    let base = base_url.to_ascii_lowercase();

    if base.contains("generativelanguage")
        || base.contains("aiplatform")
        || provider.starts_with("gemini")
        || provider.starts_with("antigravity")
    {
        return AuthDialect::Google;
    }
    if base.contains("/anthropic") || base.ends_with("/messages") || base.contains("/v1/messages") {
        return AuthDialect::Anthropic;
    }
    if provider.starts_with("anthropic") || provider == "claude" {
        return AuthDialect::Anthropic;
    }
    AuthDialect::Bearer
}

/// Operator-supplied base URL wins over the compiled provider table so custom
/// OpenAI/Anthropic-compatible endpoints are probed at the right host.
fn base_url_for(connection: &ProviderConnection) -> Option<String> {
    for key in ["baseUrl", "base_url", "apiBaseUrl", "endpoint"] {
        if let Some(value) = connection_string(connection, key) {
            return Some(value);
        }
    }
    provider_config_base_url(&connection.provider)
}

fn api_key_for(connection: &ProviderConnection) -> Option<String> {
    connection
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| connection_string(connection, "apiKey"))
}

fn connection_string(connection: &ProviderConnection, key: &str) -> Option<String> {
    connection
        .provider_specific_data
        .get(key)
        .or_else(|| connection.extra.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
