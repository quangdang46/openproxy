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

// ─── media combos ──────────────────────────────────────────────────────────
//
// `/api/combos` holds *every* combo — LLM fallback chains included. `media
// combo list` has to narrow that to the media modalities, and `media combo
// create` has to send the field `CreateComboRequest` actually reads.

#[tokio::test(flavor = "multi_thread")]
async fn media_combo_list_excludes_llm_combos() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/combos"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "combos": [
                { "id": "1", "name": "chat-fallback", "kind": null,   "models": ["openai/gpt-4o"] },
                { "id": "2", "name": "code-fallback", "kind": "llm",  "models": ["anthropic/claude"] },
                { "id": "3", "name": "tts-fallback",  "kind": "tts",  "models": ["elevenlabs/s1"] },
                { "id": "4", "name": "img-fallback",  "kind": "image","models": ["fal/flux"] },
            ]
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "media", "combo", "list"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.media.combo.list");

    let names: Vec<&str> = env["data"]["combos"]
        .as_array()
        .expect("combos array")
        .iter()
        .map(|c| c["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        names,
        vec!["tts-fallback", "img-fallback"],
        "media combo list leaked non-media combos (an LLM chain with kind:null \
         or kind:\"llm\" is a chat combo, not a media one)"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn media_combo_create_posts_members_as_models() {
    use wiremock::matchers::body_partial_json;

    let server = boot_server().await;
    Mock::given(method("POST"))
        .and(path("/api/combos"))
        .and(body_partial_json(json!({
            "name": "voice-fallback",
            "kind": "tts",
            "models": ["elevenlabs/s1", "openai/tts-1"],
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "combo-1",
            "name": "voice-fallback",
            "kind": "tts",
            "models": ["elevenlabs/s1", "openai/tts-1"],
        })))
        .mount(&server)
        .await;

    let out = op(
        &server,
        &[
            "--robot",
            "media",
            "combo",
            "create",
            "--kind",
            "tts",
            "--name",
            "voice-fallback",
            "--members",
            "elevenlabs/s1,openai/tts-1",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.media.combo.create");
    // The mock only matches a body carrying `models`, so reaching this assert
    // already proves the members were sent. --members used to be posted as
    // `providers`, which `CreateComboRequest` drops, creating an empty combo.
    assert_eq!(env["data"]["models"][0], "elevenlabs/s1");
}

#[tokio::test(flavor = "multi_thread")]
async fn media_combo_create_normalizes_embed_kind_to_embedding() {
    use wiremock::matchers::body_partial_json;

    let server = boot_server().await;
    Mock::given(method("POST"))
        .and(path("/api/combos"))
        .and(body_partial_json(json!({ "kind": "embedding" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "c" })))
        .mount(&server)
        .await;

    let out = op(
        &server,
        &[
            "--robot",
            "media",
            "combo",
            "create",
            "--kind",
            "embed",
            "--name",
            "vec-fallback",
            "--members",
            "voyage/voyage-3",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.media.combo.create");
}

// ─── antigravity-mitm ──────────────────────────────────────────────────────
//
// `POST /api/cli-tools/antigravity-mitm` and its DELETE twin both extract
// `Json<StartMitmRequest>` / `Json<StopMitmRequest>`. The CLI sent them with
// no body at all, so axum rejected every call with 415 before the handler
// ran — `tool antigravity-mitm enable` could never succeed.

#[tokio::test(flavor = "multi_thread")]
async fn antigravity_mitm_enable_sends_a_json_body() {
    use wiremock::matchers::{body_json, method, path};

    let server = boot_server().await;
    Mock::given(method("POST"))
        .and(path("/api/cli-tools/antigravity-mitm"))
        .and(body_json(json!({})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "enabled": true,
            "message": "antigravity MITM enabled",
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "tool", "antigravity-mitm", "enable"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.tool.antigravity.enable");
    assert_eq!(env["data"]["enabled"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn antigravity_mitm_disable_sends_a_json_body() {
    use wiremock::matchers::{body_json, method, path};

    let server = boot_server().await;
    Mock::given(method("DELETE"))
        .and(path("/api/cli-tools/antigravity-mitm"))
        .and(body_json(json!({})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "enabled": false,
            "message": "antigravity MITM disabled",
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "tool", "antigravity-mitm", "disable"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.tool.antigravity.disable");
    assert_eq!(env["data"]["enabled"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn antigravity_mitm_alias_set_and_delete_send_json_bodies() {
    use wiremock::matchers::{body_json, method, path};

    let server = boot_server().await;
    // The alias endpoint takes `{tool, mappings}` — not a bare `alias` field.
    // It used to 422 with "missing field `tool`" on every `--alias` call.
    Mock::given(method("PUT"))
        .and(path("/api/cli-tools/antigravity-mitm/alias"))
        .and(body_json(
            json!({ "tool": "antigravity", "mappings": { "ag": "ag" } }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "success": true })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/cli-tools/antigravity-mitm/alias"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "success": true })))
        .mount(&server)
        .await;
    // Both core toggles must succeed first, or the alias step is never reached.
    Mock::given(method("POST"))
        .and(path("/api/cli-tools/antigravity-mitm"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "enabled": true })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/cli-tools/antigravity-mitm"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "enabled": false })))
        .mount(&server)
        .await;

    let out = op(
        &server,
        &[
            "--robot",
            "tool",
            "antigravity-mitm",
            "enable",
            "--alias",
            "ag",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.tool.antigravity.enable");
    assert_eq!(env["data"]["alias"]["success"], true);

    let out = op(
        &server,
        &[
            "--robot",
            "tool",
            "antigravity-mitm",
            "disable",
            "--alias",
            "ag",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.tool.antigravity.disable");
}
