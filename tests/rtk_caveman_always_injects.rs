//! 9router open-sse/handlers/chatCore.js:278 injects the caveman prompt on
//! every request once the toggle is on — `if (tokenSaverEnabled && cavemanEnabled
//! && cavemanLevel)`. The only caller-side opt-out is the per-request
//! `x-9router-token-saver: off` header, which the chat handler applies before it
//! ever reaches `apply_request_preprocessing`. There is no context-pressure
//! heuristic anywhere in the caveman path.

use openproxy::core::rtk::{apply_request_preprocessing, CompressionLevel};
use openproxy::types::Settings;
use serde_json::json;

#[test]
fn caveman_injects_on_a_one_word_prompt() {
    let mut body = json!({ "messages": [{ "role": "user", "content": "hi" }] });
    let settings = Settings {
        caveman_enabled: true,
        caveman_level: "lite".into(),
        ..Settings::default()
    };

    // `claude-sonnet-4-20250514` is the worst case for a token-count gate: a
    // 200k context window divided by 8 saturates any threshold far above "hi".
    assert!(apply_request_preprocessing(
        &mut body,
        &settings,
        "claude-sonnet-4-20250514"
    ));
    assert_eq!(
        body["messages"][0]["content"],
        CompressionLevel::Lite.prompt().as_str()
    );
    assert_eq!(body["messages"][0]["role"], "system");
}
