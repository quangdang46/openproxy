//! TTS handler — orchestrates one synthesize request.

use reqwest::Client;
use thiserror::Error;

use super::base::{TtsError, TtsRequest, TtsResult};
use super::{get_tts_adapter, GenericFormat, GenericTtsRequest};

#[derive(Debug, Error)]
pub enum TtsHandlerError {
    #[error("HTTP {0}: {1}")]
    Http(u16, String),
    #[error("validation: {0}")]
    Validation(String),
    #[error("provider {0} not supported for TTS")]
    UnsupportedProvider(String),
    #[error("upstream: {0}")]
    Upstream(String),
}

impl From<TtsError> for TtsHandlerError {
    fn from(e: TtsError) -> Self {
        match e {
            TtsError::MissingCredentials(p) => TtsHandlerError::Validation(format!(
                "Provider '{p}' is missing credentials"
            )),
            TtsError::Upstream { status, message } => TtsHandlerError::Http(status, message),
            TtsError::Parse(m) | TtsError::Network(m) => TtsHandlerError::Upstream(m),
        }
    }
}

/// Run the TTS pipeline. Either dispatches to a special adapter or to
/// the generic format-driven path. `format` should match the upstream
/// provider's `ttsConfig.format` (used only when no special adapter
/// exists).
pub async fn handle_tts(
    client: &Client,
    provider: &str,
    request: TtsRequest<'_>,
    generic_request: Option<GenericTtsRequest<'_>>,
) -> Result<TtsResult, TtsHandlerError> {
    if request.text.trim().is_empty() {
        return Err(TtsHandlerError::Validation(
            "Missing required field: input".into(),
        ));
    }
    if let Some(adapter) = get_tts_adapter(provider) {
        return Ok(adapter.synthesize(client, &request).await?);
    }
    if let Some(generic) = generic_request {
        return Ok(super::synthesize_via_format(client, generic).await?);
    }
    Err(TtsHandlerError::UnsupportedProvider(provider.to_string()))
}

#[allow(dead_code)]
fn _phantom_format_use(f: GenericFormat) -> GenericFormat {
    f
}
