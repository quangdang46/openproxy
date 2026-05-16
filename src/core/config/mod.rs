//! Configuration / static lookup tables ported from 9router's
//! `open-sse/config/`. Each submodule mirrors one upstream JS file.
//!
//! Most entries are static lookup tables exposed as `once_cell::sync::Lazy`
//! values so they are computed once on first use. Behavioural helpers
//! (e.g. `kiro_constants::resolve_kiro_model`) are plain Rust functions.

pub mod app_constants;
pub mod codex_instructions;
pub mod default_thinking_signature;
pub mod error_config;
pub mod google_tts_languages;
pub mod kiro_constants;
pub mod ollama_models;
pub mod provider_models;
pub mod runtime_config;
pub mod tts_models;
