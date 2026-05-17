//! M4 CLI integration tests.
//!
//! These exercise the *real* `openproxy` binary (via assert_cmd) against a
//! local wiremock server, then compare the `--robot` stdout against golden
//! JSON envelopes. They cover the happy path for every M4 subcommand:
//! usage / logs / quota / chat / provider oauth.
//!
//! Streaming commands are tested separately at the lib level (see
//! `src/cli/runtime.rs` tests for `SseFrames`) — driving an indefinite SSE
//! stream from assert_cmd is brittle and adds little signal over the
//! envelope assertions.

#![cfg(test)]

use assert_cmd::prelude::*;
use serde_json::{json, Value};
use std::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API_KEY: &str = "test-cli-key";

/// Boot a wiremock that always answers `/api/health` with 200 OK, plus the
/// per-test mocks attached by the caller.
async fn boot_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;
    server
}

fn op(server: &MockServer, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("openproxy")
        .expect("locate openproxy binary")
        .env("OPENPROXY_URL", server.uri())
        .env("OPENPROXY_API_KEY", API_KEY)
        // Force a clean data dir so the CLI does not fall back to a real
        // local install on the test host.
        .env(
            "DATA_DIR",
            tempfile::tempdir()
                .expect("tempdir")
                .path()
                .to_string_lossy()
                .to_string(),
        )
        .args(args)
        .output()
        .expect("run openproxy")
}

fn parse_robot(stdout: &[u8]) -> Value {
    let s = std::str::from_utf8(stdout).expect("utf8 stdout");
    serde_json::from_str(s.trim()).unwrap_or_else(|e| {
        panic!("invalid robot envelope: {e}\nraw: {s}");
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn usage_summary_emits_robot_envelope() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/usage/summary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total_requests": 42,
            "total_prompt_tokens": 1000,
            "total_completion_tokens": 500,
            "total_cost": 0.12345,
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "usage", "summary"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.usage.summary");
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["total_requests"], 42);
    assert_eq!(env["data"]["total_cost"], 0.12345);
}

#[tokio::test(flavor = "multi_thread")]
async fn usage_providers_emits_robot_envelope() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/usage/providers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "providers": [
                {"provider": "openai",    "requests": 10, "tokens": 1234, "cost": 0.05},
                {"provider": "anthropic", "requests":  3, "tokens":  234, "cost": 0.01},
            ],
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "usage", "providers"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.usage.providers");
    assert_eq!(env["data"]["providers"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_stats_emits_robot_envelope() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/observability/stats"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "logBufferLines": 17,
            "totalRequestsLifetime": 123,
            "levelCounts": {"info": 10, "warn": 5, "error": 2},
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "logs", "stats"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.log.stats");
    assert_eq!(env["data"]["logBufferLines"], 17);
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_clear_posts_and_envelopes() {
    let server = boot_server().await;
    Mock::given(method("POST"))
        .and(path("/api/observability/clear"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"cleared": true})))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "logs", "clear"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.log.clear");
    assert_eq!(env["data"]["cleared"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn quota_list_uses_usage_providers() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/usage/providers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "providers": [
                {"provider": "openai", "requests": 1, "tokens": 100, "cost": 0.01},
            ],
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "quota", "list"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.quota.list");
    let quotas = env["data"]["quotas"].as_array().expect("quotas array");
    assert_eq!(quotas.len(), 1);
    assert_eq!(quotas[0]["provider"], "openai");
}

#[tokio::test(flavor = "multi_thread")]
async fn quota_get_returns_not_found_for_missing_provider() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/usage/providers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "providers": [],
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "quota", "get", "openai"]);
    assert!(!out.status.success(), "should fail with not_found");
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.error");
    assert_eq!(env["error"]["code"], "not_found");
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_models_envelopes_v1_models() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "gpt-4o-mini"},
                {"id": "claude-3-5-sonnet"},
            ],
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "chat", "models"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.chat.models");
    assert_eq!(env["data"]["data"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_oauth_status_envelopes() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/oauth/claude/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "provider": "claude",
            "status": "linked",
            "url": null,
            "expires_at": 1735689600u64,
        })))
        .mount(&server)
        .await;

    let out = op(
        &server,
        &["--robot", "provider", "oauth", "status", "claude"],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.oauth.status");
    assert_eq!(env["data"]["status"], "linked");
    assert_eq!(env["data"]["provider"], "claude");
}

#[tokio::test(flavor = "multi_thread")]
async fn server_down_exits_with_code_6() {
    // Bind to an unused port and immediately drop the listener so connection
    // is refused. `openproxy` must exit 6 with a `server_unreachable` envelope.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listen");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);

    let out = Command::cargo_bin("openproxy")
        .expect("openproxy binary")
        .env("OPENPROXY_URL", format!("http://127.0.0.1:{port}"))
        .env("OPENPROXY_API_KEY", API_KEY)
        .env(
            "DATA_DIR",
            tempfile::tempdir()
                .expect("tempdir")
                .path()
                .to_string_lossy()
                .to_string(),
        )
        .args(["--robot", "usage", "summary"])
        .output()
        .expect("run openproxy");

    assert_eq!(
        out.status.code(),
        Some(6),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.error");
    assert_eq!(env["error"]["code"], "server_unreachable");
}

#[tokio::test(flavor = "multi_thread")]
async fn schema_list_includes_m4_resources() {
    let out = Command::cargo_bin("openproxy")
        .expect("openproxy binary")
        .env(
            "DATA_DIR",
            tempfile::tempdir()
                .expect("tempdir")
                .path()
                .to_string_lossy()
                .to_string(),
        )
        .args(["--robot", "schema", "list"])
        .output()
        .expect("run openproxy");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    let resources = env["data"]["resources"]
        .as_array()
        .expect("resources array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>();
    for required in [
        "usage-event",
        "log-event",
        "chat-event",
        "quota",
        "oauth-status",
    ] {
        assert!(
            resources.contains(&required),
            "schema list missing '{required}': {resources:?}"
        );
    }
}
