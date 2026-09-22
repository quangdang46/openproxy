//! Simulated model registry (bead sim-11).
//!
//! Hand-maintained headline models per format (plan §10 Q5 decision: NOT
//! imported from the provider catalog — mock must work offline/keyless and
//! the registry only needs to be plausible, not exhaustive).
//!
//! Values are approximate and documented as such. Each simulator validates
//! the requested model id via [`is_known`]; unknown ids → provider-correct
//! 404 WITHOUT consulting any real catalog (offline/keyless invariant).
//!
//! Model names were checked against the repo provider catalog
//! (`provider_catalog` / `PROVIDER_CONFIGS`) at time of writing; drift is
//! acceptable — the registry gates 404-vs-200, not capability truth.

use crate::core::executor::ProviderFormat;

/// One simulated model entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulatedModel {
    /// Model id as clients send it (e.g. `"gpt-4o"`).
    pub id: &'static str,
    /// Advertised context window (approximate, informational only).
    pub context_window: Option<u64>,
    /// Whether `stream: true` is accepted (all MVP entries: true).
    pub supports_streaming: bool,
    /// Whether tool/function calling is accepted.
    pub supports_tools: bool,
}

const OPENAI_MODELS: &[SimulatedModel] = &[
    SimulatedModel {
        id: "gpt-5",
        context_window: Some(400_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "gpt-5-mini",
        context_window: Some(400_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "gpt-5-nano",
        context_window: Some(400_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "gpt-4o",
        context_window: Some(128_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "gpt-4o-mini",
        context_window: Some(128_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "gpt-4.1",
        context_window: Some(1_000_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "gpt-4.1-mini",
        context_window: Some(1_000_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "gpt-4-turbo",
        context_window: Some(128_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "gpt-4",
        context_window: Some(8_192),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "gpt-3.5-turbo",
        context_window: Some(16_385),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "o1",
        context_window: Some(200_000),
        supports_streaming: false,
        supports_tools: false,
    },
    SimulatedModel {
        id: "o1-mini",
        context_window: Some(128_000),
        supports_streaming: false,
        supports_tools: false,
    },
    SimulatedModel {
        id: "o3",
        context_window: Some(200_000),
        supports_streaming: false,
        supports_tools: false,
    },
    SimulatedModel {
        id: "o3-mini",
        context_window: Some(200_000),
        supports_streaming: false,
        supports_tools: false,
    },
    SimulatedModel {
        id: "o4-mini",
        context_window: Some(200_000),
        supports_streaming: false,
        supports_tools: false,
    },
];

const ANTHROPIC_MODELS: &[SimulatedModel] = &[
    SimulatedModel {
        id: "claude-opus-4-6",
        context_window: Some(200_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "claude-opus-4-1",
        context_window: Some(200_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "claude-sonnet-4-6",
        context_window: Some(200_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "claude-sonnet-4-5",
        context_window: Some(200_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "claude-haiku-4-5",
        context_window: Some(200_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "claude-3-7-sonnet-latest",
        context_window: Some(200_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "claude-3-5-sonnet-latest",
        context_window: Some(200_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "claude-3-5-haiku-latest",
        context_window: Some(200_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "claude-3-opus-latest",
        context_window: Some(200_000),
        supports_streaming: true,
        supports_tools: true,
    },
];

const GEMINI_MODELS: &[SimulatedModel] = &[
    SimulatedModel {
        id: "gemini-2.5-pro",
        context_window: Some(1_000_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "gemini-2.5-flash",
        context_window: Some(1_000_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "gemini-2.5-flash-lite",
        context_window: Some(1_000_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "gemini-2.0-flash",
        context_window: Some(1_000_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "gemini-2.0-flash-lite",
        context_window: Some(1_000_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "gemini-1.5-pro",
        context_window: Some(2_000_000),
        supports_streaming: true,
        supports_tools: true,
    },
    SimulatedModel {
        id: "gemini-1.5-flash",
        context_window: Some(1_000_000),
        supports_streaming: true,
        supports_tools: true,
    },
];

/// Models for one wire format. Compatible formats share the base list
/// (protocol reuse, plan §2.2); `ClaudeCompatible` maps to Anthropic.
pub fn models_for(format: ProviderFormat) -> &'static [SimulatedModel] {
    use ProviderFormat::*;
    match format {
        OpenAI | OpenAICompatible => OPENAI_MODELS,
        Anthropic | AnthropicCompatible | ClaudeCompatible => ANTHROPIC_MODELS,
        Gemini => GEMINI_MODELS,
    }
}

/// Whether `id` is a known simulated model for `format`.
/// Unknown → provider-correct 404 (offline/keyless: never consults real catalog).
pub fn is_known(format: ProviderFormat, id: &str) -> bool {
    models_for(format).iter().any(|m| m.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_non_empty_per_format() {
        use ProviderFormat::*;
        for f in [
            OpenAI,
            OpenAICompatible,
            Anthropic,
            AnthropicCompatible,
            ClaudeCompatible,
            Gemini,
        ] {
            assert!(!models_for(f).is_empty(), "empty list for {f:?}");
        }
    }

    #[test]
    fn known_headlines_pass() {
        assert!(is_known(ProviderFormat::OpenAI, "gpt-4o"));
        assert!(is_known(ProviderFormat::OpenAICompatible, "gpt-4o"));
        assert!(is_known(ProviderFormat::Anthropic, "claude-sonnet-4-6"));
        assert!(is_known(
            ProviderFormat::AnthropicCompatible,
            "claude-sonnet-4-6"
        ));
        assert!(is_known(
            ProviderFormat::ClaudeCompatible,
            "claude-sonnet-4-6"
        ));
        assert!(is_known(ProviderFormat::Gemini, "gemini-2.5-flash"));
    }

    #[test]
    fn unknown_fails_all_formats() {
        use ProviderFormat::*;
        for f in [OpenAI, Anthropic, Gemini] {
            assert!(!is_known(f, "nope-999"), "bogus known for {f:?}");
        }
    }

    #[test]
    fn reasoning_models_flagged() {
        // o-series: no streaming/tools in this registry (informational only;
        // simulators don't gate on these flags in MVP).
        let o1 = OPENAI_MODELS.iter().find(|m| m.id == "o1").unwrap();
        assert!(!o1.supports_streaming && !o1.supports_tools);
    }
}
