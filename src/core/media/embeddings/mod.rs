//! Embedding-provider adapters ported from
//! `open-sse/handlers/embeddingProviders/`.
//!
//! Three adapter implementations cover the supported providers:
//!   - [`OpenAiCompatAdapter`] for openai/openrouter/mistral/voyage-ai/
//!     fireworks/together/nebius/github/nvidia/jina-ai
//!   - [`GeminiAdapter`] for Google AI Studio (single + batch)
//!   - [`OpenAiCompatNodeAdapter`] for runtime-configured custom nodes
//!     (`openai-compatible-*`, `custom-embedding-*`)
//!
//! The handler in [`handler::handle_embeddings`] orchestrates the
//! upstream call and normalises the response to OpenAI's
//! `{ object: "list", data: [...] }` shape.

mod base;
pub mod handler;

pub use base::{EmbeddingAdapter, EmbeddingRequest, EmbeddingResponse};
pub use handler::{handle_embeddings, EmbeddingsHandlerError};

/// Look up the embedding adapter for a provider id. Falls back to the
/// runtime-configured node adapter when the provider name matches the
/// `openai-compatible-*` / `custom-embedding-*` namespaces.
pub fn get_embedding_adapter(provider: &str) -> Option<&'static dyn EmbeddingAdapter> {
    match provider {
        "openai" => Some(&base::OPENAI),
        "openrouter" => Some(&base::OPENROUTER),
        "mistral" => Some(&base::MISTRAL),
        "voyage-ai" => Some(&base::VOYAGE_AI),
        "fireworks" => Some(&base::FIREWORKS),
        "together" => Some(&base::TOGETHER),
        "nebius" => Some(&base::NEBIUS),
        "github" => Some(&base::GITHUB),
        "nvidia" => Some(&base::NVIDIA),
        "jina-ai" => Some(&base::JINA_AI),
        "gemini" | "google_ai_studio" => Some(&base::GEMINI),
        _ if provider.starts_with("openai-compatible-")
            || provider.starts_with("custom-embedding-") =>
        {
            Some(&base::OPENAI_COMPAT_NODE)
        }
        _ => None,
    }
}

pub fn is_embedding_provider(provider: &str) -> bool {
    get_embedding_adapter(provider).is_some()
}
