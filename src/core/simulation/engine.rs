//! Simulation engine dispatch (bead sim-06).
//!
//! [`ProviderSimulator`] is the per-format abstraction; [`SimulationEngine`]
//! routes a [`SimContext`] to the registered simulator. Phase 2 adds a
//! replayer behind the same trait — no refactor.
//!
//! Mock requested for a format with no simulator → [`SimulationError::Unsupported`].

use std::collections::HashMap;
use std::sync::Arc;

use super::error::SimulationError;
use crate::core::executor::ProviderFormat;

/// Execution input for one simulated request.
pub struct SimContext<'a> {
    /// Provider name as configured (`"openai"`, `"openrouter"`, …).
    /// Passthrough for logging/attribution; provider-specific behavior
    /// overrides arrive in Phase 3 (plan §2.2: no extra abstraction in MVP).
    pub provider: &'a str,
    /// Model id from the request.
    pub model: &'a str,
    /// Request body (OpenAI chat / Anthropic messages / Gemini content).
    pub body: &'a serde_json::Value,
    /// Whether the client asked for streaming.
    pub stream: bool,
}

/// One protocol-format simulator.
#[async_trait::async_trait]
pub trait ProviderSimulator: Send + Sync {
    /// Wire format this simulator covers.
    fn format(&self) -> ProviderFormat;

    /// Synthesize a complete provider response body (JSON `Value`).
    /// Streaming framing (SSE) is applied by the caller from this body;
    /// fault injection (beads sim-12+) wraps the result afterwards.
    async fn execute(&self, ctx: &SimContext<'_>) -> Result<serde_json::Value, SimulationError>;
}

/// Registry of format → simulator.
pub struct SimulationEngine {
    simulators: HashMap<ProviderFormat, Arc<dyn ProviderSimulator>>,
}

impl SimulationEngine {
    /// Empty engine (tests register fakes; production registers sim-06+).
    pub fn new() -> Self {
        Self {
            simulators: HashMap::new(),
        }
    }

    /// Register one simulator (builder style).
    pub fn register(mut self, sim: Arc<dyn ProviderSimulator>) -> Self {
        self.simulators.insert(sim.format(), sim);
        self
    }

    /// Whether `format` can execute in mock mode (plan §3.6).
    pub fn supports(&self, format: ProviderFormat) -> bool {
        self.simulators.contains_key(&format)
    }

    /// Dispatch, or [`SimulationError::Unsupported`] when unregistered.
    pub async fn execute(
        &self,
        format: ProviderFormat,
        provider: &str,
        ctx: &SimContext<'_>,
    ) -> Result<serde_json::Value, SimulationError> {
        match self.simulators.get(&format) {
            Some(sim) => sim.execute(ctx).await,
            None => Err(SimulationError::Unsupported {
                provider: provider.to_string(),
                format: format.as_str().to_string(),
            }),
        }
    }

    /// MVP production engine: OpenAI (+compatible) registered here;
    /// Anthropic/Gemini arrive in beads sim-09/sim-10.
    pub fn mvp() -> Self {
        Self::new()
            .register(Arc::new(super::openai::OpenAiSimulator))
            .register(Arc::new(super::openai::OpenAiCompatibleSimulator))
    }
}

impl Default for SimulationEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Deterministic 8-hex id from canonical request bytes (plan §5:
/// same request + same config → same bytes; no wall-clock, no randomness).
pub fn hash8(bytes: &[u8]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    format!("{:08x}", h.finish() & 0xffff_ffff)
}

/// `created` timestamp derived from the same hash (stable, not wall-clock).
pub fn hash_created(bytes: &[u8]) -> i64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    b"created:".hash(&mut h);
    bytes.hash(&mut h);
    // Fixed base (2025-01-01) + hash-derived offset: deterministic, plausible.
    1_735_689_600 + (h.finish() % 30_000_000) as i64
}

/// Last user-message text from an OpenAI chat body (for `Echo:` default).
pub fn last_user_text(body: &serde_json::Value) -> String {
    body.get("messages")
        .and_then(|m| m.as_array())
        .map(|msgs| {
            msgs.iter()
                .rev()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                .and_then(|m| m.get("content"))
                .map(|c| match c {
                    serde_json::Value::String(s) => s.clone(),
                    // content blocks array → concat text parts
                    serde_json::Value::Array(parts) => parts
                        .iter()
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join(" "),
                    other => other.to_string(),
                })
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// Whitespace-split token estimate (documented approximation).
pub fn estimate_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    text.split_whitespace().count() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSim;
    #[async_trait::async_trait]
    impl ProviderSimulator for FakeSim {
        fn format(&self) -> ProviderFormat {
            ProviderFormat::OpenAI
        }
        async fn execute(
            &self,
            _ctx: &SimContext<'_>,
        ) -> Result<serde_json::Value, SimulationError> {
            Ok(serde_json::json!({"ok": true}))
        }
    }

    #[tokio::test]
    async fn dispatch_and_supports() {
        let engine = SimulationEngine::new().register(Arc::new(FakeSim));
        assert!(engine.supports(ProviderFormat::OpenAI));
        assert!(!engine.supports(ProviderFormat::Gemini));
        let ctx = SimContext {
            provider: "openai",
            model: "gpt-4o",
            body: &serde_json::json!({}),
            stream: false,
        };
        let v = engine
            .execute(ProviderFormat::OpenAI, "openai", &ctx)
            .await
            .unwrap();
        assert_eq!(v, serde_json::json!({"ok": true}));
    }

    #[tokio::test]
    async fn unregistered_format_errors_loudly() {
        let engine = SimulationEngine::new();
        let ctx = SimContext {
            provider: "my-custom",
            model: "m",
            body: &serde_json::json!({}),
            stream: false,
        };
        let err = engine
            .execute(ProviderFormat::Gemini, "my-custom", &ctx)
            .await
            .unwrap_err();
        match &err {
            SimulationError::Unsupported { provider, format } => {
                assert_eq!(provider, "my-custom");
                assert_eq!(format, "gemini");
            }
            other => panic!("wrong error: {other:?}"),
        }
        assert!(err.message().contains("mock unavailable"));
    }

    #[test]
    fn hash_helpers_deterministic() {
        let b = b"{\"model\":\"gpt-4o\"}";
        assert_eq!(hash8(b), hash8(b));
        assert_eq!(hash_created(b), hash_created(b));
        assert_eq!(hash8(b).len(), 8);
        assert!(hash_created(b) > 1_700_000_000);
    }

    #[test]
    fn last_user_text_picks_last_user() {
        let body = serde_json::json!({"messages": [
            {"role": "user", "content": "first"},
            {"role": "assistant", "content": "hi"},
            {"role": "user", "content": "second"},
        ]});
        assert_eq!(last_user_text(&body), "second");
    }

    #[test]
    fn estimate_tokens_counts_words() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("hello world"), 2);
    }
}
