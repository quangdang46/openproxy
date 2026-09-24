//! Bead openproxy-7dwb — `opencode.jsonc` is JSONC, not strict JSON.
//!
//! The file is written by the Apply path. A single `//` comment used to make
//! the strict parse fail, and the failure path then wrote a freshly built
//! config: the user's model, MCP servers, permissions and comments were all
//! gone. These tests pin the two halves of the fix: a JSONC file survives an
//! Apply intact, and a file that genuinely cannot be parsed is reported and
//! left untouched.
#![allow(clippy::await_holding_lock)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use once_cell::sync::Lazy;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use serde_json::Value;
use tempfile::tempdir;
use tower::util::ServiceExt;

static ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

fn opencode_config_path(home: &Path) -> PathBuf {
    home.join(".config").join("opencode").join("opencode.jsonc")
}

struct EnvVarGuard {
    key: &'static str,
    old_value: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set_path(key: &'static str, value: &Path) -> Self {
        let old_value = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, old_value }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = self.old_value.take() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn active_key(key: &str) -> ApiKey {
    ApiKey {
        id: format!("{key}-id"),
        name: "Local".into(),
        key: key.into(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        extra: BTreeMap::new(),
        monthly_budget_usd: None,
    }
}

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

fn authorized_request(method: Method, uri: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer valid-bearer")
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

async fn response_json(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

const APPLY_BODY: &str = r#"{"baseUrl":"https://proxy.example.com","apiKey":"sk-openproxy","models":["oa/gpt-4.1","oa/gpt-4.1-mini"],"activeModel":"oa/gpt-4.1-mini","subagentModel":"oa/gpt-4.1-nano"}"#;

/// Apply must read `opencode.jsonc` as JSONC: comments and trailing commas are
/// legal there, and the user's own keys must come back out intact.
#[tokio::test]
async fn opencode_apply_preserves_jsonc_comments_and_existing_keys() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    let config_path = opencode_config_path(home.path());
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    let original = r#"// opencode config - hand written, keep these notes
{
  "$schema": "https://opencode.ai/config.json",
  /* block comment about my own default model */
  "model": "anthropic/claude-sonnet",
  // permissions I tuned by hand
  "permission": { "edit": "ask", "bash": { "*": "allow" } },
  "mcp": {
    // context7 is my own MCP server
    "context7": { "type": "remote", "url": "https://mcp.example.com/mcp" }
  },
  "provider": {
    // never drop my provider
    "myprovider": {
      "npm": "@ai-sdk/openai-compatible",
      "options": { "baseURL": "https://mine.example.com/v1", },
    }, // my provider, keep it
  },
}
// footer note
"#;
    std::fs::write(&config_path, original).unwrap();

    let app = openproxy::build_app(app_state().await);
    let post = app
        .clone()
        .oneshot(authorized_request(
            Method::POST,
            "/api/cli-tools/opencode-settings",
            Body::from(APPLY_BODY),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(post).await;
    assert_eq!(status, StatusCode::OK, "apply failed: {json}");
    assert_eq!(json["success"], true);

    // (a) the resulting file still parses, through the endpoint that reads it,
    //     and (c) no pre-existing key was dropped.
    let get = app
        .clone()
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/opencode-settings",
            Body::empty(),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(get).await;
    assert_eq!(status, StatusCode::OK, "read back failed: {json}");
    let saved = &json["config"];
    assert_eq!(saved["$schema"], "https://opencode.ai/config.json");
    assert_eq!(saved["permission"]["edit"], "ask");
    assert_eq!(saved["permission"]["bash"]["*"], "allow");
    assert_eq!(saved["mcp"]["context7"]["type"], "remote");
    assert_eq!(
        saved["mcp"]["context7"]["url"],
        "https://mcp.example.com/mcp"
    );
    assert_eq!(
        saved["provider"]["myprovider"]["npm"],
        "@ai-sdk/openai-compatible"
    );
    assert_eq!(
        saved["provider"]["myprovider"]["options"]["baseURL"],
        "https://mine.example.com/v1"
    );

    // The Apply path still did its job.
    assert_eq!(saved["model"], "openproxy/oa/gpt-4.1-mini");
    assert_eq!(
        saved["provider"]["openproxy"]["options"]["baseURL"],
        "https://proxy.example.com/v1"
    );
    assert_eq!(
        saved["provider"]["openproxy"]["options"]["apiKey"],
        "sk-openproxy"
    );
    assert_eq!(
        saved["provider"]["openproxy"]["models"]["oa/gpt-4.1"]["name"],
        "oa/gpt-4.1"
    );
    assert_eq!(
        saved["agent"]["explorer"]["model"],
        "openproxy/oa/gpt-4.1-nano"
    );

    // (b) the comments survive, in the file on disk.
    let on_disk = std::fs::read_to_string(&config_path).unwrap();
    for comment in [
        "// opencode config - hand written, keep these notes",
        "/* block comment about my own default model */",
        "// permissions I tuned by hand",
        "// my provider, keep it",
        "// footer note",
        "// context7 is my own MCP server",
        "// never drop my provider",
    ] {
        assert!(
            on_disk.contains(comment),
            "comment lost after Apply: {comment}\n--- file ---\n{on_disk}"
        );
    }
}

/// A file that is not valid JSONC must be reported, never replaced.
#[tokio::test]
async fn opencode_apply_refuses_to_overwrite_unparseable_jsonc() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    let config_path = opencode_config_path(home.path());
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    let broken = "// a comment I will not lose\n{\n  \"model\": \"mine\",\n  \"broken\"\n}\n";
    std::fs::write(&config_path, broken).unwrap();

    let app = openproxy::build_app(app_state().await);
    let post = app
        .oneshot(authorized_request(
            Method::POST,
            "/api/cli-tools/opencode-settings",
            Body::from(APPLY_BODY),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(post).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "broken config must not be reported as success: {json}"
    );
    let error = json["error"].as_str().expect("error message").to_string();
    assert!(
        error.contains("opencode.jsonc"),
        "error must name the file: {error}"
    );
    assert!(
        error.contains("not valid JSON/JSONC") && error.contains("line "),
        "error must say what and where: {error}"
    );

    // The file is byte-for-byte what the user left behind.
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), broken);
}
