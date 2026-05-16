//! Shared error type for media-route handlers.
//!
//! All four media-route adapters (image, tts, embeddings, search) report
//! failures through this enum so the HTTP layer has one converter.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("upstream: {0}")]
    Upstream(String),
}

impl MediaError {
    /// Best-fit HTTP status for this error.
    pub fn status(&self) -> u16 {
        match self {
            MediaError::Validation(_) => 400,
            MediaError::Http { status, .. } => *status,
            MediaError::Upstream(_) => 502,
        }
    }

    /// User-facing message body.
    pub fn message(&self) -> String {
        match self {
            MediaError::Validation(m) | MediaError::Upstream(m) => m.clone(),
            MediaError::Http { message, .. } => message.clone(),
        }
    }
}

impl From<crate::core::media::image::ImageHandlerError> for MediaError {
    fn from(e: crate::core::media::image::ImageHandlerError) -> Self {
        use crate::core::media::image::ImageHandlerError as I;
        match e {
            I::Validation(m) | I::UnsupportedProvider { provider: m } => MediaError::Validation(m),
            I::Http(status, message) => MediaError::Http { status, message },
            I::Upstream(m) => MediaError::Upstream(m),
        }
    }
}

impl From<crate::core::media::embeddings::EmbeddingsHandlerError> for MediaError {
    fn from(e: crate::core::media::embeddings::EmbeddingsHandlerError) -> Self {
        use crate::core::media::embeddings::EmbeddingsHandlerError as E;
        match e {
            E::Validation(m) | E::UnsupportedProvider(m) => MediaError::Validation(m),
            E::Http(status, message) => MediaError::Http { status, message },
            E::Upstream(m) => MediaError::Upstream(m),
        }
    }
}

impl From<crate::core::media::search::SearchHandlerError> for MediaError {
    fn from(e: crate::core::media::search::SearchHandlerError) -> Self {
        use crate::core::media::search::SearchHandlerError as S;
        match e {
            S::Validation(m) | S::UnsupportedProvider(m) => MediaError::Validation(m),
            S::Http(status, message) => MediaError::Http { status, message },
            S::Upstream(m) => MediaError::Upstream(m),
        }
    }
}

impl From<crate::core::media::tts::base::TtsError> for MediaError {
    fn from(e: crate::core::media::tts::base::TtsError) -> Self {
        use crate::core::media::tts::base::TtsError as T;
        match e {
            T::MissingCredentials(p) => MediaError::Http {
                status: 401,
                message: format!("Provider '{p}' is missing credentials"),
            },
            T::Upstream { status, message } => MediaError::Http { status, message },
            T::Parse(m) => MediaError::Upstream(m),
            T::Network(m) => MediaError::Upstream(m),
        }
    }
}
