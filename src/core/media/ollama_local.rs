//! Shared helpers for the `ollama-local` provider's media adapters
//! (TTS + embeddings), ported from 9router
//! `open-sse/config/providers.js` `resolveOllamaLocalHost` /
//! `OLLAMA_LOCAL_DEFAULT_HOST`.
//!
//! The same resolver backs the chat executor
//! (`core::executor::ollama`), which appends `/api/chat`; the media
//! adapters append their own paths (`/v1/audio/speech`,
//! `/v1/embeddings`, `/api/tags`).

use crate::types::ProviderConnection;

/// `OLLAMA_LOCAL_DEFAULT_HOST` in 9router providers.js.
pub(crate) const OLLAMA_LOCAL_DEFAULT_HOST: &str = "http://localhost:11434";

/// Resolve the Ollama daemon base URL for a connection.
///
/// Mirrors `resolveOllamaLocalHost`: `providerSpecificData.baseUrl`
/// (trimmed) wins, else `http://localhost:11434`; trailing slashes are
/// stripped so callers can append `/v1/...` or `/api/...` paths without
/// doubling separators. A saved value that already ends in `/v1` or the
/// full endpoint path is tolerated — the media adapters strip those
/// suffixes themselves before appending (see `tts/ollama_local.rs` and
/// `embeddings/base.rs`).
pub(crate) fn resolve_host(credentials: &ProviderConnection) -> String {
    let raw = credentials
        .provider_specific_data
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(OLLAMA_LOCAL_DEFAULT_HOST);
    raw.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn conn_with_base_url(url: &str) -> ProviderConnection {
        ProviderConnection {
            provider_specific_data: BTreeMap::from([("baseUrl".into(), json!(url))]),
            ..Default::default()
        }
    }

    #[test]
    fn defaults_to_localhost_11434() {
        assert_eq!(
            resolve_host(&ProviderConnection::default()),
            "http://localhost:11434"
        );
    }

    #[test]
    fn honours_provider_specific_data_base_url() {
        let conn = conn_with_base_url("http://192.168.1.100:11434/");
        assert_eq!(resolve_host(&conn), "http://192.168.1.100:11434");
    }

    #[test]
    fn empty_base_url_falls_back_to_default() {
        let conn = conn_with_base_url("   ");
        assert_eq!(resolve_host(&conn), "http://localhost:11434");
    }
}
