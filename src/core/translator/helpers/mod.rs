//! Translator helpers ported from `open-sse/translator/helpers/`.
//!
//! Each helper is a small pure function the request/response translators
//! use to enforce shared invariants (max-tokens floor, tool-call id
//! shape, image-URL fetch, etc.).

pub mod image_helper;
pub mod max_tokens_helper;
pub mod openai_helper;
pub mod tool_call_helper;
