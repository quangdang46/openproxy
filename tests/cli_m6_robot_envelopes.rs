//! M6 CLI integration tests — `settings`, `db`, `db cloud`, and `schema
//! stability` envelopes. Pattern matches `cli_m5_robot_envelopes.rs`: spin
//! up a wiremock server, drive the `openproxy` binary against it with
//! `OPENPROXY_URL` + `OPENPROXY_API_KEY`, and assert the resulting
//! `openproxy.v1.*` envelopes on stdout.

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

fn parse_robot(stdout: &[u8]) -> Value {
    let s = std::str::from_utf8(stdout).expect("utf8 stdout");
    serde_json::from_str(s.trim()).unwrap_or_else(|e| {
        panic!("invalid robot envelope: {e}\nraw: {s}");
    })
}

// ─── settings ─────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn settings_get_emits_envelope() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/settings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "comboStrategy": "fallback",
            "rtkEnabled": true,
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "settings", "get"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_robot(&out.stdout);
    assert_eq!(v["schema"], "openproxy.v1.settings.get");
    assert_eq!(v["data"]["comboStrategy"], "fallback");
    assert_eq!(v["data"]["rtkEnabled"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_get_with_key_extracts_dotted_field() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/settings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "comboStrategy": "fallback",
            "outboundProxy": {"url": "http://proxy.example.com"},
        })))
        .mount(&server)
        .await;

    let out = op(
        &server,
        &["--robot", "settings", "get", "--key", "outboundProxy.url"],
    );
    assert!(out.status.success());
    let v = parse_robot(&out.stdout);
    assert_eq!(v["schema"], "openproxy.v1.settings.get");
    assert_eq!(v["data"]["key"], "outboundProxy.url");
    assert_eq!(v["data"]["value"], "http://proxy.example.com");
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_set_patches_and_emits_envelope() {
    let server = boot_server().await;
    Mock::given(method("PATCH"))
        .and(path("/api/settings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "comboStrategy": "round-robin",
        })))
        .mount(&server)
        .await;

    let out = op(
        &server,
        &[
            "--robot",
            "settings",
            "set",
            "--key",
            "comboStrategy",
            "--value",
            "round-robin",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_robot(&out.stdout);
    assert_eq!(v["schema"], "openproxy.v1.settings.set");
    assert_eq!(v["data"]["updated"][0], "comboStrategy");
    assert_eq!(v["data"]["settings"]["comboStrategy"], "round-robin");
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_proxy_test_passes_through_be_response() {
    let server = boot_server().await;
    Mock::given(method("POST"))
        .and(path("/api/settings/proxy-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "status": 200,
            "elapsedMs": 42,
            "url": "https://google.com/",
        })))
        .mount(&server)
        .await;

    let out = op(
        &server,
        &[
            "--robot",
            "settings",
            "proxy-test",
            "--proxy-url",
            "http://proxy.example.com:8080",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_robot(&out.stdout);
    assert_eq!(v["schema"], "openproxy.v1.settings.proxy_test");
    assert_eq!(v["data"]["ok"], true);
    assert_eq!(v["data"]["elapsedMs"], 42);
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_locale_set_emits_envelope() {
    let server = boot_server().await;
    Mock::given(method("POST"))
        .and(path("/api/locale"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "locale": "vi",
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "settings", "locale", "set", "vi"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_robot(&out.stdout);
    assert_eq!(v["schema"], "openproxy.v1.settings.locale.set");
    assert_eq!(v["data"]["locale"], "vi");
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_version_emits_envelope() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "currentVersion": "1.0.0",
            "latestVersion": "1.0.1",
            "hasUpdate": true,
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "settings", "version"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_robot(&out.stdout);
    assert_eq!(v["schema"], "openproxy.v1.settings.version");
    assert_eq!(v["data"]["currentVersion"], "1.0.0");
    assert_eq!(v["data"]["hasUpdate"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_update_check_uses_version_endpoint() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "currentVersion": "1.0.0",
            "latestVersion": "1.0.0",
            "hasUpdate": false,
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "settings", "update", "--check"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_robot(&out.stdout);
    assert_eq!(v["schema"], "openproxy.v1.settings.update.check");
    assert_eq!(v["data"]["hasUpdate"], false);
}

// ─── db ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn db_export_emits_full_snapshot() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/db/export"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "providerConnections": [{"name": "openai-main"}],
            "settings": {"comboStrategy": "fallback"},
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "db", "export"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_robot(&out.stdout);
    assert_eq!(v["schema"], "openproxy.v1.db.export");
    assert_eq!(v["data"]["providerConnections"][0]["name"], "openai-main");
}

#[tokio::test(flavor = "multi_thread")]
async fn db_export_with_out_writes_file_and_reports_bytes() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/db/export"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"a": 1})))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out_path = dir.path().join("snap.json");
    let out = op(
        &server,
        &[
            "--robot",
            "db",
            "export",
            "--out",
            out_path.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_robot(&out.stdout);
    assert_eq!(v["schema"], "openproxy.v1.db.export");
    assert!(v["data"]["bytes"].as_u64().unwrap() > 0);

    let written = std::fs::read_to_string(&out_path).unwrap();
    let parsed: Value = serde_json::from_str(&written).unwrap();
    assert_eq!(parsed["a"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn db_dump_extracts_single_resource() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/db/export"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "providerConnections": [{"name": "openai-main"}, {"name": "anthropic"}],
            "settings": {"comboStrategy": "fallback"},
        })))
        .mount(&server)
        .await;

    let out = op(
        &server,
        &["--robot", "db", "dump", "--resource", "providers"],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_robot(&out.stdout);
    assert_eq!(v["schema"], "openproxy.v1.db.dump");
    assert_eq!(v["data"]["resource"], "providers");
    assert_eq!(v["data"]["data"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn db_import_merge_posts_to_settings_database() {
    let server = boot_server().await;
    Mock::given(method("POST"))
        .and(path("/api/settings/database"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let in_path = dir.path().join("in.json");
    std::fs::write(&in_path, r#"{"providerConnections": []}"#).unwrap();

    let out = op(
        &server,
        &["--robot", "db", "import", in_path.to_str().unwrap()],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_robot(&out.stdout);
    assert_eq!(v["schema"], "openproxy.v1.db.import");
    assert_eq!(v["data"]["mode"], "merge");
    assert_eq!(v["data"]["result"]["success"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn db_migrate_is_noop_but_emits_envelope() {
    let server = boot_server().await;
    let out = op(&server, &["--robot", "db", "migrate"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_robot(&out.stdout);
    assert_eq!(v["schema"], "openproxy.v1.db.migrate");
    assert_eq!(v["data"]["applied"], 0);
}

// ─── db cloud ─────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn db_cloud_auth_returns_connections_and_aliases() {
    let server = boot_server().await;
    Mock::given(method("POST"))
        .and(path("/api/cloud/auth"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "connections": [{"id": "openai-main"}],
            "modelAliases": {"fast": "openai/gpt-4o-mini"},
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "db", "cloud", "auth"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_robot(&out.stdout);
    assert_eq!(v["schema"], "openproxy.v1.db.cloud.auth");
    assert_eq!(v["data"]["modelAliases"]["fast"], "openai/gpt-4o-mini");
}

#[tokio::test(flavor = "multi_thread")]
async fn db_cloud_resolve_maps_alias_to_provider_model() {
    let server = boot_server().await;
    Mock::given(method("POST"))
        .and(path("/api/cloud/model/resolve"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alias": "fast",
            "provider": "openai",
            "model": "gpt-4o-mini",
        })))
        .mount(&server)
        .await;

    let out = op(
        &server,
        &["--robot", "db", "cloud", "resolve", "--alias", "fast"],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_robot(&out.stdout);
    assert_eq!(v["schema"], "openproxy.v1.db.cloud.resolve");
    assert_eq!(v["data"]["provider"], "openai");
    assert_eq!(v["data"]["model"], "gpt-4o-mini");
}

#[tokio::test(flavor = "multi_thread")]
async fn db_cloud_alias_list_passes_through_be_payload() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/cloud/models/alias"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "aliases": {"fast": "openai/gpt-4o-mini"},
        })))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "db", "cloud", "alias", "list"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_robot(&out.stdout);
    assert_eq!(v["schema"], "openproxy.v1.db.cloud.alias.list");
    assert_eq!(v["data"]["aliases"]["fast"], "openai/gpt-4o-mini");
}

#[tokio::test(flavor = "multi_thread")]
async fn db_cloud_alias_set_puts_to_be_and_echoes() {
    let server = boot_server().await;
    Mock::given(method("PUT"))
        .and(path("/api/cloud/models/alias"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "alias": "fast",
            "model": "openai/gpt-4o-mini",
        })))
        .mount(&server)
        .await;

    let out = op(
        &server,
        &[
            "--robot",
            "db",
            "cloud",
            "alias",
            "set",
            "--alias",
            "fast",
            "--model",
            "openai/gpt-4o-mini",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_robot(&out.stdout);
    assert_eq!(v["schema"], "openproxy.v1.db.cloud.alias.set");
    assert_eq!(v["data"]["alias"], "fast");
}

// ─── schema stability ──────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn schema_stability_emits_v1_promise() {
    let server = boot_server().await;
    let out = op(&server, &["--robot", "schema", "stability"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_robot(&out.stdout);
    assert_eq!(v["schema"], "openproxy.v1.schema.stability");
    assert_eq!(v["data"]["namespace"], "openproxy.v1");
    assert_eq!(v["data"]["stability"], "stable");
}

#[tokio::test(flavor = "multi_thread")]
async fn schema_list_includes_namespace_and_stability() {
    let server = boot_server().await;
    let out = op(&server, &["--robot", "schema", "list"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_robot(&out.stdout);
    assert_eq!(v["schema"], "openproxy.v1.schema.list");
    assert_eq!(v["data"]["namespace"], "openproxy.v1");
    assert_eq!(v["data"]["stability"], "stable");
    assert!(!v["data"]["resources"].as_array().unwrap().is_empty());
}
