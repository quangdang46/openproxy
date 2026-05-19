//! End-to-end tests for `openproxy sync`. These exercise the binary against
//! a real `DATA_DIR` and parse the `--robot` envelope, mirroring the style
//! of the M4/M5 CLI tests.

#![cfg(test)]

use assert_cmd::prelude::*;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::process::{Command, Output};
use tempfile::TempDir;

fn write_fixture(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    let mut f = fs::File::create(&p).expect("create fixture");
    f.write_all(body.as_bytes()).expect("write fixture");
    p
}

fn op(data_dir: &std::path::Path, args: &[&str]) -> Output {
    Command::cargo_bin("openproxy")
        .expect("locate openproxy binary")
        .env("DATA_DIR", data_dir)
        .args(args)
        .output()
        .expect("spawn openproxy")
}

/// Minimal fixture mimicking the schema emitted by
/// `scripts/sync/normalize-sources.mjs`.
fn fixture_snapshot(source: &str, alias: &str, model_id: &str) -> String {
    format!(
        r#"{{
            "source": "{source}",
            "ref": "v0.0.1",
            "generatedAt": "2026-05-19T14:00:00Z",
            "providerIdToAlias": {{"provider-id": "{alias}"}},
            "providers": [
                {{
                    "id": "provider-id",
                    "alias": "{alias}",
                    "format": "openai",
                    "authType": "apikey",
                    "baseUrl": "https://example.com/v1/chat/completions",
                    "models": [
                        {{
                            "id": "{model_id}",
                            "name": "Sample model",
                            "kind": "llm",
                            "contextLength": 128000
                        }}
                    ]
                }}
            ]
        }}"#,
        source = source,
        alias = alias,
        model_id = model_id,
    )
}

fn parse_robot_envelope(stdout: &[u8]) -> Value {
    let s = std::str::from_utf8(stdout).expect("utf8 stdout");
    let line = s
        .lines()
        .find(|l| l.contains("openproxy.v1.sync.apply"))
        .unwrap_or_else(|| panic!("no sync envelope found in stdout:\n{s}"));
    serde_json::from_str(line).expect("envelope is valid JSON")
}

#[test]
fn sync_dry_run_emits_envelope_and_does_not_write() {
    let dir = TempDir::new().unwrap();
    let fixture_path = write_fixture(
        dir.path(),
        "fixture-9router.json",
        &fixture_snapshot("9router", "testprov", "testprov/unique-id-1"),
    );

    let out = op(
        dir.path(),
        &[
            "--robot",
            "sync",
            "9router",
            "--dry-run",
            "--source-file",
            fixture_path.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "sync should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot_envelope(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.sync.apply");
    let data = &env["data"];
    assert_eq!(data["source"], "9router");
    assert_eq!(data["ref"], "v0.0.1");
    assert_eq!(data["dryRun"], true);
    let created = data["diff"]["created"].as_array().expect("array");
    assert_eq!(created.len(), 1);
    assert_eq!(created[0]["provider_alias"], "testprov");
    assert_eq!(created[0]["model_id"], "testprov/unique-id-1");
    // db.json may be auto-initialized by Db::load, but the dry-run must
    // not have added the synced model.
    let db_path = dir.path().join("db.json");
    if db_path.exists() {
        let db: Value = serde_json::from_str(&fs::read_to_string(&db_path).unwrap()).unwrap();
        let models = db.get("customModels").and_then(|m| m.as_array());
        if let Some(models) = models {
            assert!(
                !models.iter().any(|m| m["id"] == "testprov/unique-id-1"),
                "dry-run must not have persisted the synced model"
            );
        }
    }
}

#[test]
fn sync_apply_then_reapply_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let fixture_path = write_fixture(
        dir.path(),
        "fixture-9router.json",
        &fixture_snapshot("9router", "testprov", "testprov/unique-id-1"),
    );

    // First apply — should create.
    let out1 = op(
        dir.path(),
        &[
            "--robot",
            "sync",
            "9router",
            "--source-file",
            fixture_path.to_str().unwrap(),
        ],
    );
    assert!(out1.status.success());
    let env1 = parse_robot_envelope(&out1.stdout);
    assert_eq!(env1["data"]["diff"]["created"].as_array().unwrap().len(), 1);

    // db.json should now exist and contain the synced model.
    let db_path = dir.path().join("db.json");
    assert!(db_path.exists(), "apply should have created db.json");
    let raw = fs::read_to_string(&db_path).unwrap();
    let db: Value = serde_json::from_str(&raw).unwrap();
    let models = db["customModels"].as_array().expect("customModels array");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["id"], "testprov/unique-id-1");
    assert_eq!(models[0]["providerAlias"], "testprov");
    assert_eq!(models[0]["source"], "9router");
    assert_eq!(models[0]["sourceRef"], "v0.0.1");
    assert_eq!(models[0]["contextLength"], 128000);

    // Second apply — same snapshot. Should be all unchanged.
    let out2 = op(
        dir.path(),
        &[
            "--robot",
            "sync",
            "9router",
            "--source-file",
            fixture_path.to_str().unwrap(),
        ],
    );
    assert!(out2.status.success());
    let env2 = parse_robot_envelope(&out2.stdout);
    assert_eq!(env2["data"]["diff"]["created"].as_array().unwrap().len(), 0);
    assert_eq!(env2["data"]["diff"]["updated"].as_array().unwrap().len(), 0);
    assert_eq!(
        env2["data"]["diff"]["unchanged"].as_array().unwrap().len(),
        1
    );
}

#[test]
fn sync_prune_removes_only_same_source_entries() {
    let dir = TempDir::new().unwrap();
    // Pre-seed db.json with: (a) a 9router-tagged stale entry, (b) a
    // user-added entry without source. The fixture only contains a *different*
    // model id — the stale entry should be removed, the user one preserved.
    let preseed = serde_json::json!({
        "customModels": [
            {
                "providerAlias": "testprov",
                "id": "testprov/legacy-id",
                "type": "chat",
                "name": "Old",
                "source": "9router",
                "sourceRef": "v0.0.0",
                "kind": "llm"
            },
            {
                "providerAlias": "testprov",
                "id": "testprov/user-custom",
                "type": "chat",
                "name": "Mine"
            }
        ]
    });
    fs::write(
        dir.path().join("db.json"),
        serde_json::to_string_pretty(&preseed).unwrap(),
    )
    .unwrap();

    let fixture_path = write_fixture(
        dir.path(),
        "fixture-9router.json",
        &fixture_snapshot("9router", "testprov", "testprov/unique-id-1"),
    );

    // --prune should drop the 9router-tagged entry and keep the user one.
    let out = op(
        dir.path(),
        &[
            "--robot",
            "sync",
            "9router",
            "--prune",
            "--source-file",
            fixture_path.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot_envelope(&out.stdout);
    let deleted = env["data"]["diff"]["deleted"].as_array().expect("deleted");
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0]["model_id"], "testprov/legacy-id");

    // Re-read db.
    let db: Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join("db.json")).unwrap()).unwrap();
    let models = db["customModels"].as_array().unwrap();
    let ids: Vec<&str> = models.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert!(
        ids.contains(&"testprov/user-custom"),
        "user model should survive"
    );
    assert!(
        ids.contains(&"testprov/unique-id-1"),
        "synced model should be added"
    );
    assert!(
        !ids.contains(&"testprov/legacy-id"),
        "stale model should be pruned"
    );
}

#[test]
fn sync_omniroute_runs_against_embedded_snapshot() {
    let dir = TempDir::new().unwrap();
    let out = op(dir.path(), &["--robot", "sync", "omniroute", "--dry-run"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot_envelope(&out.stdout);
    assert_eq!(env["data"]["source"], "omniroute");
    // Embedded omniroute snapshot has many models — confirm we get a
    // non-trivial number of creates.
    let created = env["data"]["diff"]["created"].as_array().unwrap();
    assert!(
        created.len() >= 50,
        "expected omniroute snapshot to add lots of models, got {}",
        created.len()
    );
}
