#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationFormat {
    OpenAi,
    Claude,
    Gemini,
}

pub mod helpers;
pub mod registry;
pub mod request;
pub mod response;
pub mod response_transform;
