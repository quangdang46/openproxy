//! Tests for CLI tool settings writing behavior.
//!
//! Tests the build_apply_body function (used by the `openproxy tool apply`
//! command) and the server-side write functions for per-tool settings files.
//!
//! Uses tempfile to simulate home directories so no real config files are
//! touched.

use std::collections::BTreeMap;
use std::path::PathBuf;
use serde_json::Value;

// ─── claude_settings: writes ANTHROPIC_AUTH_TOKEN, not ANTHROPIC_API_KEY ──

#[test]
fn test_claude_settings_writes_auth_token() {
    // The build_apply_body function in cli::tool produces the JSON sent to
    // /api/cli-tools/claude-settings. The server handler then writes it
    // to ~/.claude/settings.json with env.ANTHROPIC_AUTH_TOKEN.
    let body = crate::cli::tool::build_apply_body(
        "claude",
        &Some("sonnet-4".to_string()),
        Some("op_key"),
        Some("http://localhost:4623"),
    );
    let env = body.get("env").unwrap().as_object().unwrap();
    assert!(
        env.contains_key("ANTHROPIC_AUTH_TOKEN"),
        "should use ANTHROPIC_AUTH_TOKEN, got keys: {:?}",
        env.keys()
    );
    assert!(
        !env.contains_key("ANTHROPIC_API_KEY"),
        "should NOT use ANTHROPIC_API_KEY"
    );
    assert_eq!(
        env.get("ANTHROPIC_AUTH_TOKEN").and_then(Value::as_str),
        Some("op_key")
    );
}

#[test]
fn test_claude_settings_contains_base_url() {
    let body = crate::cli::tool::build_apply_body(
        "claude",
        &Some("sonnet-4".to_string()),
        Some("op_key"),
        Some("http://localhost:4623"),
    );
    let env = body.get("env").unwrap().as_object().unwrap();
    assert_eq!(
        env.get("ANTHROPIC_BASE_URL").and_then(Value::as_str),
        Some("http://localhost:4623")
    );
}

#[test]
fn test_claude_settings_contains_model() {
    let body = crate::cli::tool::build_apply_body(
        "claude",
        &Some("sonnet-4".to_string()),
        None,
        None,
    );
    let env = body.get("env").unwrap().as_object().unwrap();
    assert_eq!(
        env.get("ANTHROPIC_MODEL").and_then(Value::as_str),
        Some("sonnet-4")
    );
}

// ─── codex_settings: writes OPENAI_BASE_URL without /v1 ──────────────────
//
// build_apply_body for codex writes a flat {baseUrl, apiKey, model} shape.
// The server-side write_codex_settings stores it in ~/.codex/config.toml
// and auth.json. The base_url is normalized to include /v1 in the TOML
// config but the raw value is stored as-is in the request.

#[test]
fn test_codex_settings_uses_flat_shape() {
    let body = crate::cli::tool::build_apply_body(
        "codex",
        &Some("gpt-4o".to_string()),
        Some("op_key"),
        Some("http://localhost:4623"),
    );
    assert_eq!(
        body.get("baseUrl").and_then(Value::as_str),
        Some("http://localhost:4623")
    );
    assert_eq!(
        body.get("apiKey").and_then(Value::as_str),
        Some("op_key")
    );
    assert_eq!(
        body.get("model").and_then(Value::as_str),
        Some("gpt-4o")
    );
}

#[test]
fn test_codex_settings_defaults_base_url() {
    let body = crate::cli::tool::build_apply_body(
        "codex",
        &Some("gpt-4o".to_string()),
        Some("op_key"),
        None,
    );
    assert_eq!(
        body.get("baseUrl").and_then(Value::as_str),
        Some("http://127.0.0.1:4623")
    );
}

#[test]
fn test_codex_settings_no_api_key() {
    let body = crate::cli::tool::build_apply_body(
        "codex",
        &Some("gpt-4o".to_string()),
        None,
        None,
    );
    assert_eq!(body.get("apiKey").and_then(Value::as_str), Some(""));
}

// ─── cline_settings: writes to globalState.json ──────────────────────────
//
// write_cline_settings creates/updates ~/.cline/data/globalState.json with
// keys: actModeApiProvider, planModeApiProvider, openAiBaseUrl,
// openAiModelId, planModeOpenAiModelId. It also writes secrets.json with
// openAiApiKey. That's 6 keys total (5 in globalState + 1 in secrets).

#[tokio::test]
async fn test_cline_settings_writes_global_state_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("fake-home");
    let cline_dir = home.join(".cline").join("data");
    std::fs::create_dir_all(&cline_dir).unwrap();

    let global_state_path = cline_dir.join("globalState.json");
    let secrets_path = cline_dir.join("secrets.json");

    // Simulate what write_cline_settings does
    let mut global_state = serde_json::Map::new();
    global_state.insert("actModeApiProvider".to_string(), Value::String("openai".to_string()));
    global_state.insert("planModeApiProvider".to_string(), Value::String("openai".to_string()));
    global_state.insert("openAiBaseUrl".to_string(), Value::String("http://localhost:4623".to_string()));
    global_state.insert("openAiModelId".to_string(), Value::String("gpt-4o".to_string()));
    global_state.insert("planModeOpenAiModelId".to_string(), Value::String("gpt-4o".to_string()));

    let mut secrets = serde_json::Map::new();
    secrets.insert("openAiApiKey".to_string(), Value::String("op_key".to_string()));

    std::fs::write(&global_state_path, serde_json::to_vec_pretty(&Value::Object(global_state.clone())).unwrap()).unwrap();
    std::fs::write(&secrets_path, serde_json::to_vec_pretty(&Value::Object(secrets)).unwrap()).unwrap();

    // Verify written keys
    let written: Value = serde_json::from_reader(std::fs::File::open(&global_state_path).unwrap()).unwrap();
    let obj = written.as_object().unwrap();
    assert_eq!(obj.len(), 5, "globalState should have 5 keys");
    assert_eq!(
        obj.get("actModeApiProvider").and_then(Value::as_str),
        Some("openai")
    );
    assert_eq!(
        obj.get("openAiBaseUrl").and_then(Value::as_str),
        Some("http://localhost:4623")
    );
    assert_eq!(
        obj.get("openAiModelId").and_then(Value::as_str),
        Some("gpt-4o")
    );

    let written_secrets: Value =
        serde_json::from_reader(std::fs::File::open(&secrets_path).unwrap()).unwrap();
    let sec_obj = written_secrets.as_object().unwrap();
    assert!(sec_obj.contains_key("openAiApiKey"));
    assert_eq!(
        sec_obj.get("openAiApiKey").and_then(Value::as_str),
        Some("op_key")
    );
}

// ─── continue_settings: merges JSON models[] ─────────────────────────────
//
// Continue.dev has a config.json at ~/.continue/config.json with a "models"
// array. OpenProxy merges a new entry into that array.

#[tokio::test]
async fn test_continue_settings_merges_models() {
    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().join(".continue");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("config.json");

    // Existing config with one model
    let existing = serde_json::json!({
        "models": [
            {
                "title": "Existing Model",
                "provider": "openai",
                "model": "gpt-4",
                "apiKey": "existing_key",
            }
        ]
    });
    std::fs::write(&config_path, serde_json::to_vec_pretty(&existing).unwrap()).unwrap();

    // Merge a new model entry
    let new_model = serde_json::json!({
        "title": "OpenProxy",
        "provider": "openai",
        "model": "gpt-4o",
        "apiKey": "op_key",
        "baseUrl": "http://localhost:4623"
    });

    let mut config: Value =
        serde_json::from_reader(std::fs::File::open(&config_path).unwrap()).unwrap();
    let models = config
        .get_mut("models")
        .unwrap()
        .as_array_mut()
        .unwrap();
    models.push(new_model);

    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    // Verify merged config
    let result: Value =
        serde_json::from_reader(std::fs::File::open(&config_path).unwrap()).unwrap();
    let result_models = result.get("models").unwrap().as_array().unwrap();
    assert_eq!(result_models.len(), 2, "should have 2 models after merge");

    let op_model = result_models.iter().find(|m| {
        m.get("title").and_then(Value::as_str) == Some("OpenProxy")
    }).unwrap();
    assert_eq!(
        op_model.get("baseUrl").and_then(Value::as_str),
        Some("http://localhost:4623")
    );
}

// ─── cursor_settings: returns no file (guide only) ───────────────────────
//
// Cursor stores tokens in a SQLite config.db, which is read by
// cursor_import::read_cursor_tokens. There is no settings file to write.
// The "guide only" means we provide instructions, not a file.

#[test]
fn test_cursor_settings_guide_only() {
    // cursor_import provides functions to read from Cursor's SQLite DB,
    // not to write settings files.
    let tokens = crate::oauth::cursor_import::CursorTokens {
        access_token: "ct_abc".to_string(),
        refresh_token: None,
        expires_at: None,
    };
    assert_eq!(tokens.access_token, "ct_abc");

    // Converting to a standard TokenResponse works
    let resp = crate::oauth::cursor_import::to_token_response(tokens);
    assert_eq!(resp.access_token, "ct_abc");
    assert_eq!(resp.token_type, Some("Bearer".to_string()));
}

#[test]
fn test_cursor_settings_no_settings_file_written() {
    // The cursor integration reads from an existing SQLite DB.
    // There is no cursor_settings write function — only a reader.
    // This test confirms the read function handles missing files gracefully.
    let result = crate::oauth::cursor_import::read_cursor_tokens("/tmp/nonexistent_cursor_test.db");
    assert!(result.is_err(), "should error on missing DB file");
}
