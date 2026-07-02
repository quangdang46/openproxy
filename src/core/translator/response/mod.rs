//! Response translators: provider format → OpenAI SSE chunks

pub mod claude_to_openai;
pub mod commandcode_to_openai;
pub mod cursor_to_openai;
pub mod gemini_to_openai;
pub mod kiro_to_openai;
pub mod ollama_to_openai;
pub mod openai_responses;
pub mod openai_to_antigravity;
pub mod non_streaming;
pub mod openai_to_claude;
pub mod openai_to_gemini;
