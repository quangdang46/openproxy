//! Media providers ported from 9router's `open-sse/handlers/`.
//!
//! Subdirectories:
//!   - `image/`    — image generation (12 providers)
//!   - `tts/`      — text-to-speech (9 providers + STT helpers)
//!   - `embeddings/` — embedding providers (3)
//!   - `search/`   — web search providers
//!   - `responses/` — OpenAI Responses API
//!
//! Each provider lives in its own module and implements a small adapter
//! trait that the public handler dispatches to.

mod error;
pub use error::MediaError;

pub mod embeddings;
pub mod image;
pub mod responses;
pub mod search;
pub mod stt;
pub mod tts;
