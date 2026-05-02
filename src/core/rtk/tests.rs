//! Unit tests for RTK compression functionality.
//!
//! Tests cover:
//! - CompressionLevel enum parsing and values
//! - Caveman prompt injection for different request shapes
//! - Content token estimation
//! - Request preprocessing with caveman settings
//! - Edge cases for injection

use serde_json::{json, Value};

use crate::core::rtk::*;
use crate::types::Settings;

// =============================================================================
// Tests for CompressionLevel
// =============================================================================

#[test]
fn test_compression_level_parse_lite() {
    assert_eq!("lite".parse::<CompressionLevel>(), Ok(CompressionLevel::Lite));
}

#[test]
fn test_compression_level_parse_full() {
    assert_eq!("full".parse::<CompressionLevel>(), Ok(CompressionLevel::Full));
}

#[test]
fn test_compression_level_parse_ultra() {
    assert_eq!("ultra".parse::<CompressionLevel>(), Ok(CompressionLevel::Ultra));
}

#[test]
fn test_compression_level_parse_invalid() {
    assert_eq!("invalid".parse::<CompressionLevel>(), Err(()));
}

#[test]
fn test_compression_level_parse_case_insensitive() {
    assert_eq!("LITE".parse::<CompressionLevel>(), Ok(CompressionLevel::Lite));
    assert_eq!("FULL".parse::<CompressionLevel>(), Ok(CompressionLevel::Full));
    assert_eq!("ULTRA".parse::<CompressionLevel>(), Ok(CompressionLevel::Ultra));
    assert_eq!("LiTe".parse::<CompressionLevel>(), Ok(CompressionLevel::Lite));
}

#[test]
fn test_compression_level_parse_or_default() {
    assert_eq!(CompressionLevel::parse_or_default("lite"), CompressionLevel::Lite);
    assert_eq!(CompressionLevel::parse_or_default("invalid"), CompressionLevel::Full);
    assert_eq!(CompressionLevel::parse_or_default("ultra"), CompressionLevel::Ultra);
}

#[test]
fn test_compression_level_as_str() {
    assert_eq!(CompressionLevel::Lite.as_str(), "lite");
    assert_eq!(CompressionLevel::Full.as_str(), "full");
    assert_eq!(CompressionLevel::Ultra.as_str(), "ultra");
}

#[test]
fn test_compression_level_prompt_not_empty() {
    assert!(!CompressionLevel::Lite.prompt().is_empty());
    assert!(!CompressionLevel::Full.prompt().is_empty());
    assert!(!CompressionLevel::Ultra.prompt().is_empty());
}

#[test]
fn test_compression_level_all_prompts_different() {
    let lite = CompressionLevel::Lite.prompt();
    let full = CompressionLevel::Full.prompt();
    let ultra = CompressionLevel::Ultra.prompt();
    assert_ne!(lite, full);
    assert_ne!(full, ultra);
    assert_ne!(lite, ultra);
}

#[test]
fn test_compression_level_ultra_prompt_contains_abbreviation_hint() {
    let ultra_prompt = CompressionLevel::Ultra.prompt();
    assert!(ultra_prompt.contains("abbreviate") || ultra_prompt.contains("terse"));
}

#[test]
fn test_compression_level_full_prompt_contains_caveman_hint() {
    let full_prompt = CompressionLevel::Full.prompt();
    assert!(full_prompt.contains("caveman"));
}

// =============================================================================
// Tests for normalize_caveman_level
// =============================================================================

#[test]
fn test_normalize_caveman_level_valid() {
    assert_eq!(normalize_caveman_level("lite"), "lite");
    assert_eq!(normalize_caveman_level("full"), "full");
    assert_eq!(normalize_caveman_level("ultra"), "ultra");
}

#[test]
fn test_normalize_caveman_level_invalid_defaults_to_full() {
    assert_eq!(normalize_caveman_level("invalid"), "full");
    assert_eq!(normalize_caveman_level(""), "full");
}

// =============================================================================
// Tests for inject_caveman_prompt - OpenAI shape
// =============================================================================

#[test]
fn test_inject_caveman_openai_system_message() {
    let mut body = json!({
        "messages": [
            { "role": "system", "content": "You are helpful." },
            { "role": "user", "content": "Hello" }
        ]
    });

    let result = inject_caveman_prompt(&mut body, CompressionLevel::Lite);
    assert!(result);

    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert!(messages[0]["content"].as_str().unwrap().contains("helpful"));
    assert!(messages[0]["content"].as_str().unwrap().contains("Respond terse"));
}

#[test]
fn test_inject_caveman_openai_no_system_message() {
    let mut body = json!({
        "messages": [
            { "role": "user", "content": "Hello" }
        ]
    });

    let result = inject_caveman_prompt(&mut body, CompressionLevel::Full);
    assert!(result);

    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], CompressionLevel::Full.prompt());
}

#[test]
fn test_inject_caveman_openai_instructions() {
    let mut body = json!({
        "instructions": "Some existing instructions."
    });

    let result = inject_caveman_prompt(&mut body, CompressionLevel::Ultra);
    assert!(result);

    assert!(body["instructions"].as_str().unwrap().contains("Some existing"));
    assert!(body["instructions"].as_str().unwrap().contains("Respond ultra-terse"));
}

// =============================================================================
// Tests for inject_caveman_prompt - Claude shape
// =============================================================================

#[test]
fn test_inject_caveman_claude_system() {
    let mut body = json!({
        "system": "You are Claude."
    });

    let result = inject_caveman_prompt(&mut body, CompressionLevel::Lite);
    assert!(result);

    assert!(body["system"].as_str().unwrap().contains("Claude"));
    assert!(body["system"].as_str().unwrap().contains("Respond terse"));
}

#[test]
fn test_inject_caveman_claude_system_blocks() {
    let mut body = json!({
        "system": [
            { "type": "text", "text": "You are Claude." }
        ]
    });

    let result = inject_caveman_prompt(&mut body, CompressionLevel::Full);
    assert!(result);

    let blocks = body["system"].as_array().unwrap();
    assert_eq!(blocks[0]["text"], "You are Claude.");
    // First block should be text, second block should have prompt
    assert!(blocks[blocks.len() - 1]["text"].as_str().unwrap().contains("caveman"));
}

// =============================================================================
// Tests for inject_caveman_prompt - Gemini shape
// =============================================================================

#[test]
fn test_inject_caveman_gemini_system_instruction_object() {
    let mut body = json!({
        "request": {
            "systemInstruction": { "parts": [{ "text": "You are Gemini." }] },
            "contents": [{ "parts": [{ "text": "Hello" }] }]
        }
    });

    let result = inject_caveman_prompt(&mut body, CompressionLevel::Lite);
    assert!(result);

    let parts = body["request"]["systemInstruction"]["parts"].as_array().unwrap();
    assert_eq!(parts[0]["text"], "You are Gemini.");
    assert!(parts[parts.len() - 1]["text"].as_str().unwrap().contains("Respond terse"));
}

#[test]
fn test_inject_caveman_gemini_system_instruction_string() {
    let mut body = json!({
        "request": {
            "systemInstruction": "You are Gemini.",
            "contents": [{ "parts": [{ "text": "Hello" }] }]
        }
    });

    let result = inject_caveman_prompt(&mut body, CompressionLevel::Full);
    assert!(result);

    let parts = body["request"]["systemInstruction"]["parts"].as_array().unwrap();
    assert_eq!(parts[0]["text"], "You are Gemini.");
    assert!(parts[parts.len() - 1]["text"].as_str().unwrap().contains("caveman"));
}

#[test]
fn test_inject_caveman_gemini_flat_shape() {
    let mut body = json!({
        "systemInstruction": { "parts": [{ "text": "System prompt" }] },
        "contents": [{ "parts": [{ "text": "Hello" }] }]
    });

    let result = inject_caveman_prompt(&mut body, CompressionLevel::Ultra);
    assert!(result);

    let parts = body["systemInstruction"]["parts"].as_array().unwrap();
    assert!(parts.len() >= 2);
}

// =============================================================================
// Tests for inject_caveman_prompt - Responses API shape
// =============================================================================

#[test]
fn test_inject_caveman_responses_api_input() {
    let mut body = json!({
        "input": [
            { "role": "user", "content": [{ "type": "input_text", "text": "Hello" }] }
        ]
    });

    let result = inject_caveman_prompt(&mut body, CompressionLevel::Lite);
    assert!(result);

    let input = body["input"].as_array().unwrap();
    assert!(input[0]["content"][0]["text"].as_str().unwrap().contains("Hello"));
}

// =============================================================================
// Tests for inject_caveman_prompt - idempotency
// =============================================================================

#[test]
fn test_inject_caveman_is_idempotent() {
    let mut body = json!({
        "messages": [
            { "role": "system", "content": CompressionLevel::Full.prompt() },
            { "role": "user", "content": "Hello" }
        ]
    });

    let result1 = inject_caveman_prompt(&mut body, CompressionLevel::Full);
    let result2 = inject_caveman_prompt(&mut body, CompressionLevel::Full);

    assert!(result1); // First injection succeeds
    assert!(!result2); // Second injection returns false (already present)
}

// =============================================================================
// Tests for should_auto_apply_caveman
// =============================================================================

#[test]
fn test_should_auto_apply_caveman_small_prompt() {
    let body = json!({
        "messages": [
            { "role": "user", "content": "Hello" }
        ]
    });

    assert!(!should_auto_apply_caveman(&body, "gpt-4o-mini"));
}

#[test]
fn test_should_auto_apply_caveman_large_prompt() {
    let body = json!({
        "messages": [
            { "role": "user", "content": "x".repeat(10000) }
        ]
    });

    assert!(should_auto_apply_caveman(&body, "gpt-4o-mini"));
}

#[test]
fn test_should_auto_apply_caveman_claude_large() {
    let body = json!({
        "messages": [
            { "role": "user", "content": "x".repeat(10000) }
        ]
    });

    // Claude has larger context window
    assert!(should_auto_apply_caveman(&body, "claude-sonnet-4-20250514"));
}

#[test]
fn test_should_auto_apply_caveman_gemini_large() {
    let body = json!({
        "messages": [
            { "role": "user", "content": "x".repeat(10000) }
        ]
    });

    // Gemini 2.0 has larger context window
    assert!(should_auto_apply_caveman(&body, "gemini-2.0-flash"));
}

// =============================================================================
// Tests for apply_request_preprocessing
// =============================================================================

#[test]
fn test_apply_request_preprocessing_disabled() {
    let settings = Settings {
        caveman_enabled: false,
        caveman_level: "ultra".into(),
        rtk_enabled: false,
        ..Settings::default()
    };

    let mut body = json!({
        "messages": [
            { "role": "user", "content": "x".repeat(10000) }
        ]
    });

    let result = apply_request_preprocessing(&mut body, &settings, "gpt-4o-mini");
    assert!(!result);

    // Body unchanged
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["content"], "x".repeat(10000));
}

#[test]
fn test_apply_request_preprocessing_small_request() {
    let settings = Settings {
        caveman_enabled: true,
        caveman_level: "ultra".into(),
        rtk_enabled: false,
        ..Settings::default()
    };

    let mut body = json!({
        "messages": [
            { "role": "user", "content": "short" }
        ]
    });

    let result = apply_request_preprocessing(&mut body, &settings, "gpt-4o-mini");
    assert!(!result);
}

#[test]
fn test_apply_request_preprocessing_large_request() {
    let settings = Settings {
        caveman_enabled: true,
        caveman_level: "full".into(),
        rtk_enabled: false,
        ..Settings::default()
    };

    let mut body = json!({
        "messages": [
            { "role": "user", "content": "x".repeat(10000) }
        ]
    });

    let result = apply_request_preprocessing(&mut body, &settings, "gpt-4o-mini");
    assert!(result);

    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
}

#[test]
fn test_apply_request_preprocessing_rtk_toggle_ignored() {
    // RTK toggle should not affect caveman injection
    let settings = Settings {
        caveman_enabled: true,
        caveman_level: "lite".into(),
        rtk_enabled: false, // RTK disabled
        ..Settings::default()
    };

    let mut body = json!({
        "messages": [
            { "role": "user", "content": "x".repeat(10000) }
        ]
    });

    let result = apply_request_preprocessing(&mut body, &settings, "gpt-4o-mini");
    assert!(result);
}

// =============================================================================
// Tests for edge cases
// =============================================================================

#[test]
fn test_inject_caveman_empty_body() {
    let mut body = json!({});
    let result = inject_caveman_prompt(&mut body, CompressionLevel::Ultra);
    assert!(!result);
}

#[test]
fn test_inject_caveman_null_fields() {
    let mut body = json!({
        "messages": null,
        "instructions": null
    });
    let result = inject_caveman_prompt(&mut body, CompressionLevel::Lite);
    assert!(!result);
}

#[test]
fn test_inject_caveman_nested_gemini_request() {
    let mut body = json!({
        "request": {
            "contents": [
                {
                    "role": "user",
                    "parts": [{ "text": "What is the meaning of life?" }]
                }
            ]
        }
    });

    let result = inject_caveman_prompt(&mut body, CompressionLevel::Ultra);
    assert!(result);
}

#[test]
fn test_should_auto_apply_caveman_empty_messages() {
    let body = json!({
        "messages": []
    });
    assert!(!should_auto_apply_caveman(&body, "gpt-4o-mini"));
}

#[test]
fn test_should_auto_apply_caveman_null_content() {
    let body = json!({
        "messages": [
            { "role": "user", "content": null }
        ]
    });
    // Should not panic, just return false
    assert!(!should_auto_apply_caveman(&body, "gpt-4o-mini"));
}

#[test]
fn test_compression_level_parse_with_whitespace() {
    assert_eq!(" lite ".parse::<CompressionLevel>(), Ok(CompressionLevel::Lite));
    assert_eq!("  full  ".parse::<CompressionLevel>(), Ok(CompressionLevel::Full));
}

// =============================================================================
// Tests for context window inference
// =============================================================================

#[test]
fn test_context_window_inference_claude() {
    let models = vec![
        "claude-3-5-sonnet",
        "claude-sonnet-4-20250514",
        "claude-opus-4",
        "claude-haiku-4",
    ];
    for model in models {
        let body = json!({
            "messages": [
                { "role": "user", "content": "x".repeat(10000) }
            ]
        });
        assert!(should_auto_apply_caveman(&body, model), "Failed for {}", model);
    }
}

#[test]
fn test_context_window_inference_gemini_large() {
    let models = vec![
        "gemini-1.5-pro",
        "gemini-2.0-flash",
        "gemini-2.5-pro",
    ];
    for model in models {
        let body = json!({
            "messages": [
                { "role": "user", "content": "x".repeat(10000) }
            ]
        });
        assert!(should_auto_apply_caveman(&body, model), "Failed for {}", model);
    }
}

#[test]
fn test_context_window_inference_openai() {
    let models = vec![
        "gpt-4o",
        "gpt-4.1",
        "o1-preview",
        "o3",
        "o4",
    ];
    for model in models {
        let body = json!({
            "messages": [
                { "role": "user", "content": "x".repeat(10000) }
            ]
        });
        assert!(should_auto_apply_caveman(&body, model), "Failed for {}", model);
    }
}

#[test]
fn test_context_window_inference_default() {
    // Unknown model should use default 16k context
    let body = json!({
        "messages": [
            { "role": "user", "content": "x".repeat(10000) }
        ]
    });
    assert!(should_auto_apply_caveman(&body, "unknown-model-v1"));
}

// =============================================================================
// Tests for Claude cache_control block handling
// =============================================================================

#[test]
fn test_inject_caveman_preserves_cache_control() {
    let mut body = json!({
        "system": [
            { "type": "text", "text": "You are helpful." },
            { "type": "cache_control", "index": 0 }
        ]
    });

    let result = inject_caveman_prompt(&mut body, CompressionLevel::Lite);
    assert!(result);

    let system = body["system"].as_array().unwrap();
    // Should have cache_control block preserved
    let has_cache_control = system.iter().any(|b| b["type"] == "cache_control");
    assert!(has_cache_control);
}
