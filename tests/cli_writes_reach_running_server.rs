//! A CLI write that lands in SQLite must be visible to the *running* server
//! without a restart.
//!
//! `provider add` / `pool create` / `combo create` / `key add` / the `*_apply`
//! commands all write through `Db::update`, which commits to SQLite. A running
//! server keeps its own `ArcSwap` `AppDb` and only re-reads on the paths that
//! already call `reload_snapshot()` (auth, the OAuth device poll, the chat
//! dispatcher) — so every one of those commands reported success while
//! `GET /api/providers` (and therefore the dashboard) kept serving the
//! pre-write list.
//!
//! These tests drive the real `openproxy` binary against a real server over a
//! shared `DATA_DIR`, then assert through the *HTTP* API rather than the
//! in-process snapshot: only a round trip through the running server catches
//! a stale snapshot, because the writer's own `db.update` already updated it.
#![cfg(test)]

use assert_cmd::prelude::*;
use serde_json::json;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn op(dir: &Path, args: &[&str]) -> serde_json::Value {
    let out = Command::cargo_bin("openproxy")
        .expect("locate openproxy binary")
        .env("DATA_DIR", dir)
        .args(["--robot"])
        .args(args)
        .output()
        .expect("run openproxy");
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "invalid envelope for {args:?}: {e}\nraw: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// Kills the spawned server when dropped so a test run leaves no listener
/// behind for the next one to collide with.
struct ServerGuard(std::process::Child);

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spin up a server on a free port over `dir`, and return `(url, api_key)`.
///
/// The key is minted through the server itself so the assertions below are
/// authenticated the same way a real client would be.
fn boot(dir: &Path, port: u16) -> (String, String, ServerGuard) {
    let url = format!("http://127.0.0.1:{port}");
    let child = Command::cargo_bin("openproxy")
        .expect("locate openproxy binary")
        .env("DATA_DIR", dir)
        .arg("--port")
        .arg(port.to_string())
        .arg("--no-open")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(
            std::fs::File::create(dir.join("server.err")).expect("server.err"),
        ))
        .spawn()
        .expect("spawn server");
    // Kill the server when the test ends. Leaking it makes the next run fail
    // with "Address already in use", which reads as a product bug rather than
    // test hygiene.
    let _guard = ServerGuard(child);
    // Wait for readiness on the health route, not just the TCP listener —
    // `key add` needs the server to be serving before it can install the key
    // through HTTP, and minting it too early would fall back to the local-DB
    // path the very bug these tests are about.
    for _ in 0..200 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            let probe = get(&url, "/api/health", "");
            if probe.get("ok").is_some() || probe.get("status").is_some() {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let key = op(dir, &["key", "add", "sweep-test-key", "--auto"])["data"]["key"]
        .as_str()
        .expect("minted key")
        .to_string();
    (url, key, _guard)
}

/// Minimal std-only HTTP GET — avoids pulling `reqwest`'s blocking feature in
/// for a dev-dependency that is only used here.
fn get(url: &str, path: &str, key: &str) -> serde_json::Value {
    use std::io::{Read, Write};
    let host_port = url.trim_start_matches("http://");
    let mut stream = std::net::TcpStream::connect(host_port)
        .unwrap_or_else(|e| panic!("connect {host_port}: {e}"));
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_port}\r\nAuthorization: Bearer {key}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).expect("write request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let text = String::from_utf8_lossy(&raw).to_string();
    let body = match text.find("\r\n\r\n") {
        Some(i) => text[i + 4..].to_string(),
        None => panic!("malformed response for {path}: {text}"),
    };
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("{path}: {e}\nraw: {body}"))
}

/// Count the entries in a list response. `auth` is the bearer key; `field` is
/// the response's own array key — the two are unrelated, and passing the auth
/// key as the field silently yields 0.
fn count(url: &str, path: &str, auth: &str, field: &str) -> usize {
    get(url, path, auth)[field]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0)
}

fn test_dir(tag: &str) -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    // The endpoint sidecar is what `openproxy` reads to find the running
    // server, so it has to point at the port we actually bound.
    std::thread::sleep(std::time::Duration::from_millis(50));
    let _ = tag;
    dir
}

#[test]
fn provider_add_is_visible_to_the_running_server() {
    let dir = test_dir("provider");
    let (url, key, _server) = boot(dir.path(), 47811);
    let before = count(&url, "/api/providers", &key, "connections");

    let v = op(
        dir.path(),
        &[
            "provider",
            "add",
            "sweep-prov",
            r#"{"provider":"openai","apiKey":"sk-x"}"#,
        ],
    );
    assert_eq!(v["ok"], true, "{v}");

    let after = get(&url, "/api/providers", &key);
    assert_eq!(
        count(&url, "/api/providers", &key, "connections"),
        before + 1,
        "provider add wrote to SQLite but the running server still served the \
         pre-write list: {after}\nserver stderr: {}",
        std::fs::read_to_string(dir.path().join("server.err")).unwrap_or_default()
    );
}

#[test]
fn pool_create_is_visible_to_the_running_server() {
    let dir = test_dir("pool");
    let (url, key, _server) = boot(dir.path(), 47812);
    let before = count(&url, "/api/proxy-pools", &key, "proxyPools");

    let v = op(
        dir.path(),
        &[
            "pool",
            "create",
            "sweep-pool",
            "http://proxy.example.com:8080",
        ],
    );
    assert_eq!(v["ok"], true, "{v}");

    let pools = get(&url, "/api/proxy-pools", &key);
    assert_eq!(
        pools["proxyPools"].as_array().map(|a| a.len()).unwrap_or(0),
        before + 1,
        "pool create wrote to SQLite but the running server still served the \
         pre-write list: {pools}"
    );
}

#[test]
fn combo_create_is_visible_to_the_running_server() {
    let dir = test_dir("combo");
    let (url, key, _server) = boot(dir.path(), 47813);
    let before = count(&url, "/api/combos", &key, "combos");

    let v = op(
        dir.path(),
        &[
            "combo",
            "create",
            "--name",
            "sweep-combo",
            "--models",
            "openai/gpt-4o",
        ],
    );
    assert_eq!(v["ok"], true, "{v}");

    let combos = get(&url, "/api/combos", &key);
    assert_eq!(
        count(&url, "/api/combos", &key, "combos"),
        before + 1,
        "combo create wrote to SQLite but the running server still served the \
         pre-write list: {combos}"
    );
}

#[test]
fn provider_apply_is_visible_to_the_running_server() {
    let dir = test_dir("apply");
    let (url, key, _server) = boot(dir.path(), 47814);
    let before = count(&url, "/api/providers", &key, "connections");

    let doc = json!({"name": "sweep-applied", "provider": "openai", "apiKey": "sk-y"});
    let out = Command::cargo_bin("openproxy")
        .expect("locate openproxy binary")
        .env("DATA_DIR", dir.path())
        .args(["--robot", "provider", "apply"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin
                .as_mut()
                .ok_or_else(|| std::io::Error::other("no stdin"))?
                .write_all(doc.to_string().as_bytes())?;
            c.wait_with_output()
        })
        .expect("run provider apply");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid envelope: {e}\nraw: {stdout}"));
    assert_eq!(v["ok"], true, "{v}");

    let providers = get(&url, "/api/providers", &key);
    assert_eq!(
        count(&url, "/api/providers", &key, "connections"),
        before + 1,
        "provider apply reported {v} but the running server still served the \
         pre-write list: {providers}"
    );
}

/// The `data` blob is the only place `name` and `proxyUrl` live — the table
/// has no column for either. `pool_to_data` used to serialise only `p.extra`,
/// so `pool create` inserted `data = '{}'` and the pool vanished on read-back:
/// `pool list` showed an empty list and `pool get` said "not found" for a pool
/// that had just been created.
#[test]
fn a_created_pool_keeps_its_name_and_url_across_a_fresh_read() {
    let dir = test_dir("persist");
    let v = op(
        dir.path(),
        &["pool", "create", "us-east", "http://proxy.example.com:8080"],
    );
    assert_eq!(v["ok"], true, "{v}");

    // A separate process re-reads SQLite from scratch — no in-memory state.
    let listed = op(dir.path(), &["pool", "list"]);
    let pools = listed["data"]["pools"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(pools.len(), 1, "pool create did not persist: {listed}");
    assert_eq!(pools[0]["name"], "us-east", "{listed}");
    assert_eq!(
        pools[0]["proxyUrl"], "http://proxy.example.com:8080",
        "{listed}"
    );
}
