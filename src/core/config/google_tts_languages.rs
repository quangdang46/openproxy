//! Port of `open-sse/config/googleTtsLanguages.js`. Static list of language
//! codes accepted by Google TTS, exposed as JSON values for direct relay
//! to the dashboard.

use crate::core::config::provider_models::google_tts_languages as raw_languages;
use serde_json::Value;

/// Returns the full list of `{ id, name, type: "tts" }` entries known to
/// Google TTS.
pub fn languages() -> &'static [Value] {
    raw_languages()
}

/// Look up a language entry by id (e.g. `"en"`, `"vi"`, `"zh-CN"`).
pub fn find(id: &str) -> Option<&'static Value> {
    raw_languages()
        .iter()
        .find(|v| v.get("id").and_then(|x| x.as_str()) == Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_common_languages() {
        for code in ["en", "vi", "ja", "zh-CN"] {
            assert!(find(code).is_some(), "missing language: {code}");
        }
    }

    #[test]
    fn entries_are_well_formed() {
        for entry in languages() {
            assert!(entry.get("id").and_then(|v| v.as_str()).is_some());
            assert!(entry.get("name").and_then(|v| v.as_str()).is_some());
            assert_eq!(entry.get("type").and_then(|v| v.as_str()), Some("tts"));
        }
    }
}
