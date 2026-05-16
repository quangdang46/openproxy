//! Port of `open-sse/config/ollamaModels.js` — static stub list returned
//! by the local-Ollama executor when an upstream Ollama server is not
//! reachable. The actual models are advertised verbatim from this list,
//! so the shape must match what `GET /api/tags` returns.

use once_cell::sync::Lazy;
use serde_json::{json, Value};

/// Stub catalogue of Ollama-compatible model entries.
///
/// JSON shape matches Ollama's `/api/tags` response so the executor can
/// pass it through unchanged.
pub static OLLAMA_MODELS: Lazy<Value> = Lazy::new(|| {
    json!({
        "models": [
            {
                "name": "llama3.2",
                "modified_at": "2025-12-26T00:00:00Z",
                "size": 2_000_000_000u64,
                "digest": "abc123def456",
                "details": {
                    "format": "gguf",
                    "family": "llama",
                    "parameter_size": "3B",
                    "quantization_level": "Q4_K_M"
                }
            },
            {
                "name": "qwen2.5",
                "modified_at": "2025-12-26T00:00:00Z",
                "size": 4_000_000_000u64,
                "digest": "def456abc123",
                "details": {
                    "format": "gguf",
                    "family": "qwen",
                    "parameter_size": "7B",
                    "quantization_level": "Q4_K_M"
                }
            }
        ]
    })
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_models_has_two_entries() {
        let arr = OLLAMA_MODELS["models"].as_array().expect("array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["name"], "llama3.2");
    }
}
