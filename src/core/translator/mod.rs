#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationFormat {
    OpenAi,
    Claude,
    Gemini,
}

pub mod response_transform;
