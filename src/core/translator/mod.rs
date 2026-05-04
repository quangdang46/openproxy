#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationFormat {
    OpenAi,
    Claude,
    Gemini,
}

pub mod registry;
pub mod response_transform;
