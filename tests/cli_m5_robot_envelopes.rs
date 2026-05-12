//! M5 CLI integration tests — mitm / tunnel (runtime) / tool / translator / media.
//!
//! Exercises the `openproxy` binary against a wiremock server and asserts the
//! `--robot` JSON envelopes. We hit one happy-path per subcommand group; the
//! detailed handler tests live in unit tests inside each `cli/*.rs` module.

#![cfg(test)]

use assert_cmd::prelude::*;
use serde_json::{json, Value};
use std::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API_KEY: &str = "test-cli-key";

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

fn op_stdin(server: &MockServer, args: &[&str], stdin: &str) -> std::process::Output {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::cargo_bin("openproxy")
        .expect("locate openproxy binary")
        .env("OPENPROXY_URL", server.uri())
        .env("OPENPROXY_API_KEY", API_KEY)
        .env(
            "DATA_DIR",
            tempfile::tempdir()
                .expect("tempdir")
                .path()
                .to_string_lossy()
                .to_string(),
        )
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn openproxy");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait")
}

fn parse_robot(stdout: &[u8]) -> Value {
    let s = std::str::from_utf8(stdout).expect("utf8 stdout");
    serde_json::from_str(s.trim()).unwrap_or_else(|e| {
        panic!("invalid robot envelope: {e}\nraw: {s}");
    })
}

// ─── mitm ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn mitm_status_emits_envelope() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/mitm-config"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "enabled": true,
            "routes": {"claude": {"upstreamUrl": "https://api.anthropic.com"}},
            "certStatus": {"fingerprint": "abc"},
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "mitm", "status"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.mitm.status");
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["enabled"], true);
    assert_eq!(env["data"]["routes"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn mitm_start_emits_envelope() {
    let server = boot_server().await;
    Mock::given(method("POST"))
        .and(path("/api/mitm/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"started": true})))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "mitm", "start"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.mitm.start");
    assert_eq!(env["data"]["started"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn mitm_cert_generate_emits_envelope() {
    let server = boot_server().await;
    Mock::given(method("POST"))
        .and(path("/api/mitm/cert/generate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"fingerprint": "deadbeef"})))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "mitm", "cert", "generate"]);
    assert!(out.status.success());
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.mitm.cert.generate");
    assert_eq!(env["data"]["fingerprint"], "deadbeef");
}

#[tokio::test(flavor = "multi_thread")]
async fn mitm_config_apply_reads_stdin() {
    let server = boot_server().await;
    Mock::given(method("PUT"))
        .and(path("/api/mitm-config"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;

    let body = r#"{"routerBaseUrl":"http://router.example/"}"#;
    let out = op_stdin(
        &server,
        &["--robot", "mitm", "config", "apply", "--from-file", "-"],
        body,
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.mitm.config.apply");
}

// ─── tunnel (runtime) ───────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn tunnel_enable_emits_envelope() {
    let server = boot_server().await;
    Mock::given(method("POST"))
        .and(path("/api/tunnel/enable"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"enabled": true})))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "tunnel", "enable", "cloudflare"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.tunnel.enable");
    assert_eq!(env["data"]["enabled"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn tunnel_tailscale_check_emits_envelope() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/tunnel/tailscale-check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "installed": true,
            "loggedIn": false,
            "daemonRunning": true,
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "tunnel", "tailscale", "check"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.tunnel.tailscale.check");
    assert_eq!(env["data"]["installed"], true);
    assert_eq!(env["data"]["loggedIn"], false);
}

// ─── tool ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn tool_list_emits_envelope() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/cli-tools"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tools": [
                {"name": "provider-list", "description": "List providers", "category": "provider"},
                {"name": "key-list",      "description": "List keys",      "category": "key"},
            ],
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "tool", "list"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.tool.list");
    assert_eq!(env["data"]["tools"].as_array().map(Vec::len), Some(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_apply_dry_run_does_not_call_server() {
    // No mock for POST /api/cli-tools/claude-settings — if the binary
    // tries to hit it, wiremock will return 404 and we'll see a failure.
    let server = boot_server().await;
    let out = op(
        &server,
        &[
            "--robot",
            "tool",
            "apply",
            "claude",
            "--model",
            "claude-sonnet-4",
            "--api-key",
            "op_test",
            "--endpoint",
            "http://router.example",
            "--dry-run",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.tool.apply.dry_run");
    assert_eq!(env["data"]["path"], "/api/cli-tools/claude-settings");
    assert_eq!(
        env["data"]["body"]["env"]["ANTHROPIC_BASE_URL"],
        "http://router.example"
    );
    assert_eq!(
        env["data"]["body"]["env"]["ANTHROPIC_AUTH_TOKEN"],
        "op_test"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_revert_calls_delete() {
    let server = boot_server().await;
    Mock::given(method("DELETE"))
        .and(path("/api/cli-tools/codex-settings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"reverted": true})))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "tool", "revert", "codex"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.tool.revert");
}

// ─── translator ─────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn translator_formats_emits_envelope() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/translator/formats"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "openai", "name": "OpenAI", "description": "Chat Completions"},
            {"id": "claude", "name": "Claude", "description": "Messages"},
        ])))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "translator", "formats"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.translator.formats");
    assert_eq!(env["data"].as_array().map(Vec::len), Some(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn translator_preset_save_posts_to_translator_save() {
    let server = boot_server().await;
    Mock::given(method("POST"))
        .and(path("/api/translator/save"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
        .mount(&server)
        .await;

    let out = op_stdin(
        &server,
        &["--robot", "translator", "preset", "save", "my-preset"],
        r#"{"foo": "bar"}"#,
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.translator.preset.save");
}

// ─── media ──────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn media_providers_list_emits_envelope() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/media-providers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tts": [],
            "stt": [],
            "embedding": [],
            "image": [],
            "search": [],
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "media", "providers", "list"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.media.providers.list");
}

#[tokio::test(flavor = "multi_thread")]
async fn media_tts_voices_emits_envelope() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/media-providers/tts/voices"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "voices": [{"id": "alloy", "name": "Alloy"}],
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "media", "tts", "voices"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.media.tts.voices");
}

#[tokio::test(flavor = "multi_thread")]
async fn media_tts_speak_writes_bytes_to_stdout() {
    let server = boot_server().await;
    let audio_bytes = b"FAKE_MP3_BYTES_PAYLOAD";
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(audio_bytes.to_vec())
                .insert_header("content-type", "audio/mpeg"),
        )
        .mount(&server)
        .await;

    let out = op_stdin(
        &server,
        &[
            "media",
            "tts",
            "speak",
            "--provider",
            "elevenlabs",
            "--model",
            "eleven_turbo_v2",
            "--voice",
            "alice",
        ],
        "Hello world",
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(&out.stdout[..], &audio_bytes[..]);
}

#[tokio::test(flavor = "multi_thread")]
async fn media_web_fetch_emits_envelope() {
    let server = boot_server().await;
    Mock::given(method("POST"))
        .and(path("/v1/web/fetch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": "# Page title\n",
            "format": "markdown",
        })))
        .mount(&server)
        .await;

    let out = op(
        &server,
        &[
            "--robot",
            "media",
            "web",
            "fetch",
            "https://example.com",
            "--provider",
            "firecrawl",
        ],
    );
    assert!(
        out.status.success(),
        "status: {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.media.web.fetch");
    assert_eq!(env["data"]["content"], "# Page title\n");
}
