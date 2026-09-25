//! Additive schema sync + settings/custom-model persistence parity.
//!
//! Covers four findings:
//!
//! 1. `sync_schema_from_tables` — 9router runs `syncSchemaFromTables` on every
//!    boot, so a column appended to `TABLES` reaches existing databases.
//!    OpenProxy only ran `CREATE TABLE IF NOT EXISTS` (a no-op on an existing
//!    table) plus one hand-written per-column patch, so every later column was
//!    silently absent.
//! 2. `PATCH /api/settings` — 9router merges the raw body
//!    (`{ ...current, ...updates }`); OpenProxy's closed request struct dropped
//!    every key it did not declare, including the `ccFilterNaming` its own chat
//!    path reads.
//! 3. `POST /api/models/custom` caps — 9router whitelists the capability keys
//!    (`sanitizeCaps`), stores them, and `GET /api/models` spreads them over the
//!    name-derived values for custom rows.
//! 4. `observabilityEnabled` — 9router's `DEFAULT_SETTINGS.enableObservability`
//!    is false, and it persists the flag under the key `enableObservability`.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use openproxy::db::sqlite::migrations::get_schema_version;
use openproxy::db::sqlite::{SqliteDb, SCHEMA_VERSION};
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::{ApiKey, CustomModel};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::util::ServiceExt;

const TEST_KEY: &str = "settings-parity-test-key";

fn active_key() -> ApiKey {
    ApiKey {
        id: "key-1".into(),
        name: "Local".into(),
        key: TEST_KEY.into(),
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
        state.api_keys = vec![active_key()];
        state.settings.require_login = true;
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

fn authorized(method: Method, uri: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {TEST_KEY}"))
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

async fn patch(settings: AppState, body: Value) -> (StatusCode, Value) {
    let app = openproxy::build_app(settings);
    let response = app
        .oneshot(authorized(
            Method::PATCH,
            "/api/settings",
            Body::from(body.to_string()),
        ))
        .await
        .unwrap();
    response_json(response).await
}

async fn get_settings(settings: AppState) -> (StatusCode, Value) {
    let app = openproxy::build_app(settings);
    let response = app
        .oneshot(authorized(Method::GET, "/api/settings", Body::empty()))
        .await
        .unwrap();
    response_json(response).await
}

fn column_names(db: &SqliteDb, table: &str) -> HashSet<String> {
    let conn = db.lock();
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .unwrap();
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<HashSet<String>>>()
        .unwrap();
    rows
}

// ── 1. Additive column sync ───────────────────────────────────────────

/// A column appended to `TABLES_SQL` after a database was created must reach it
/// on the next open — the gap 9router's `syncSchemaFromTables` closes.
#[test]
fn adds_column_declared_later_to_an_existing_database() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("openproxy.sqlite");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        // A database written before `apiKeys` grew `createdAt` and
        // `monthly_budget_usd`. Only the hand-written budget helper knew about
        // the latter, so `createdAt` is the probe that isolates the sync. No
        // rows: SQLite only permits `ADD COLUMN ... NOT NULL` on an empty
        // table, and that refusal is what the next test pins down.
        conn.execute_batch(
            "CREATE TABLE _meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE apiKeys (
                 id   TEXT PRIMARY KEY,
                 key  TEXT UNIQUE NOT NULL,
                 name TEXT
             );
             INSERT INTO _meta(key, value) VALUES('schema_version', '1');",
        )
        .unwrap();
    }

    let db = SqliteDb::open(&path).expect("open legacy database");

    let columns = column_names(&db, "apiKeys");
    assert!(
        columns.contains("createdAt"),
        "createdAt was declared in TABLES_SQL but never reached the database: {columns:?}"
    );
    assert!(columns.contains("monthly_budget_usd"));
    assert_eq!(get_schema_version(&db.lock()).unwrap(), SCHEMA_VERSION);
}

#[test]
fn sync_is_idempotent_and_preserves_rows() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("openproxy.sqlite");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE _meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE apiKeys (id TEXT PRIMARY KEY, key TEXT UNIQUE NOT NULL);
             INSERT INTO apiKeys(id, key) VALUES ('k1', 'a'), ('k2', 'b'), ('k3', 'c');",
        )
        .unwrap();
    }

    // Two boots in a row: the second must find nothing left to do.
    let db = SqliteDb::open(&path).expect("first open");
    let columns_after_first = column_names(&db, "apiKeys");
    drop(db);
    let db = SqliteDb::open(&path).expect("second open");
    assert_eq!(column_names(&db, "apiKeys"), columns_after_first);

    // The ALTER adds a column; it never rebuilds the table out from under the
    // rows already stored.
    let conn = db.lock();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM apiKeys", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 3);
}

/// 9router wraps each `ALTER` in a `try`/`catch` and logs the failure: a column
/// that cannot be added must not stop the boot, and must not stop the columns
/// queued behind it.
#[test]
fn sync_does_not_abort_boot_on_unaddable_column() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("openproxy.sqlite");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE _meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             -- `providerConnections` predates the NOT NULL columns, and holds a
             -- row: SQLite refuses `ADD COLUMN ... NOT NULL` with no default.
             -- The indexed columns are present so TABLES_SQL's CREATE INDEX
             -- statements still apply.
             CREATE TABLE providerConnections (
                 id       TEXT PRIMARY KEY,
                 provider TEXT,
                 isActive INTEGER,
                 priority INTEGER
             );
             INSERT INTO providerConnections(id, provider) VALUES ('c1', 'openai');
             CREATE TABLE apiKeys (id TEXT PRIMARY KEY, key TEXT UNIQUE NOT NULL);",
        )
        .unwrap();
    }

    let db = SqliteDb::open(&path).expect("boot must survive an unaddable column");

    let connections = column_names(&db, "providerConnections");
    assert!(
        !connections.contains("data"),
        "data is NOT NULL with no default — SQLite must have refused it: {connections:?}"
    );
    // The pass kept going after the refusal, both within the table (`name` is
    // declared before `data`, `authType` after it) and past it.
    assert!(connections.contains("authType"));
    assert!(column_names(&db, "apiKeys").contains("createdAt"));

    let rows: i64 = db
        .lock()
        .query_row("SELECT COUNT(*) FROM providerConnections", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1);
}

// ── 2. Settings PATCH is a raw merge ──────────────────────────────────

#[tokio::test]
async fn patch_settings_persists_undeclared_keys() {
    let state = app_state().await;
    let body = json!({
        "ccFilterNaming": true,
        "quotaVisibility": { "openai": { "hidden": ["gpt-4"] } },
    });

    let (status, _) = patch(state.clone(), body).await;
    assert_eq!(status, StatusCode::OK);

    let snapshot = state.db.snapshot();
    assert_eq!(
        snapshot.settings.extra.get("ccFilterNaming"),
        Some(&json!(true))
    );
    assert_eq!(
        snapshot
            .settings
            .extra
            .get("quotaVisibility")
            .and_then(|value| value.get("openai"))
            .and_then(|value| value.get("hidden"))
            .and_then(Value::as_array),
        Some(&vec![json!("gpt-4")])
    );

    // The read-back serialises `Settings`, whose `#[serde(flatten)] extra`
    // surfaces both at the top level.
    let (status, payload) = get_settings(state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(payload["ccFilterNaming"], true);
    assert_eq!(payload["quotaVisibility"]["openai"]["hidden"][0], "gpt-4");
}

#[tokio::test]
async fn patch_settings_declared_field_does_not_duplicate_into_extra() {
    let state = app_state().await;

    let (status, payload) = patch(
        state.clone(),
        json!({ "observabilityEnabled": false, "comboStrategy": "latency" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(payload["observabilityEnabled"], false);
    assert_eq!(payload["comboStrategy"], "latency");

    // `Settings` has its own `#[serde(flatten)] extra`; a key that a declared
    // field already owns must not land in both places, or the next load would
    // meet the same name twice.
    let snapshot = state.db.snapshot();
    assert!(!snapshot.settings.observability_enabled);
    assert_eq!(snapshot.settings.combo_strategy, "latency");
    assert!(!snapshot.settings.extra.contains_key("observabilityEnabled"));
    assert!(!snapshot.settings.extra.contains_key("comboStrategy"));
}

#[tokio::test]
async fn patch_settings_does_not_persist_password_fields() {
    let state = app_state().await;

    let (status, _) = patch(state.clone(), json!({ "someUnknownKey": 1 })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        state.db.snapshot().settings.extra.get("someUnknownKey"),
        Some(&json!(1))
    );

    // The password branch still short-circuits, so neither the credentials nor
    // the keys travelling alongside them are written.
    let (status, _) = patch(
        state.clone(),
        json!({ "newPassword": "x", "anotherUnknownKey": 2 }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    let snapshot = state.db.snapshot();
    assert!(!snapshot.settings.extra.contains_key("newPassword"));
    assert!(!snapshot.settings.extra.contains_key("anotherUnknownKey"));
}

// ── 3. Custom-model capabilities ──────────────────────────────────────

#[tokio::test]
async fn post_custom_model_persists_and_returns_sanitized_caps() {
    let state = app_state().await;
    let app = openproxy::build_app(state.clone());

    let response = app
        .oneshot(authorized(
            Method::POST,
            "/api/models/custom",
            Body::from(
                json!({
                    "providerAlias": "oa",
                    "id": "gpt-v",
                    "caps": { "vision": true, "reasoning": false, "bogus": true, "search": true },
                })
                .to_string(),
            ),
        ))
        .await
        .unwrap();
    let (status, _) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);

    // `search` is not in CAPACITY_META and `bogus` is not a key at all, so
    // sanitizeCaps keeps only the two whitelisted booleans.
    let app = openproxy::build_app(state.clone());
    let response = app
        .oneshot(authorized(Method::GET, "/api/models/custom", Body::empty()))
        .await
        .unwrap();
    let (status, payload) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    let model = &payload["models"][0];
    assert_eq!(model["id"], "gpt-v");
    assert_eq!(model["caps"], json!({ "vision": true, "reasoning": false }));
}

#[tokio::test]
async fn post_custom_model_omits_caps_when_nothing_whitelisted() {
    let state = app_state().await;
    let app = openproxy::build_app(state.clone());

    let response = app
        .oneshot(authorized(
            Method::POST,
            "/api/models/custom",
            Body::from(
                json!({ "providerAlias": "oa", "id": "gpt-nocaps", "caps": { "search": true } })
                    .to_string(),
            ),
        ))
        .await
        .unwrap();
    let (status, _) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(authorized(Method::GET, "/api/models/custom", Body::empty()))
        .await
        .unwrap();
    let (_, payload) = response_json(response).await;
    let model = &payload["models"][0];
    assert!(
        model.get("caps").is_none(),
        "a caps map with no whitelisted key must not be stored at all: {model}"
    );
}

#[tokio::test]
async fn get_api_models_spreads_custom_caps_over_derived() {
    let state = app_state().await;
    state
        .db
        .update(|db| {
            let mut extra = BTreeMap::new();
            // The id says "reasoning", so the name heuristic derives true; the
            // operator said otherwise and the override has to win.
            extra.insert(
                "caps".to_string(),
                json!({ "vision": true, "reasoning": false }),
            );
            db.custom_models.push(CustomModel {
                provider_alias: "myproxy".into(),
                id: "house-reasoning-v2".into(),
                r#type: "llm".into(),
                name: Some("House Reasoning".into()),
                extra,
            });
            // A non-LLM custom model stays out of the LLM list.
            db.custom_models.push(CustomModel {
                provider_alias: "myproxy".into(),
                id: "house-embed".into(),
                r#type: "embedding".into(),
                name: None,
                extra: BTreeMap::new(),
            });
        })
        .await
        .unwrap();

    let app = openproxy::build_app(state);
    let response = app
        .oneshot(authorized(Method::GET, "/api/models", Body::empty()))
        .await
        .unwrap();
    let (status, payload) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);

    let row = payload["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["fullModel"] == "myproxy/house-reasoning-v2")
        .expect("custom model row missing from GET /api/models");
    assert_eq!(row["caps"]["vision"], true);
    assert_eq!(row["caps"]["reasoning"], false);

    assert!(!payload["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|model| model["fullModel"] == "myproxy/house-embed"));
}

// ── 4. Observability default + 9router key alias ──────────────────────

#[tokio::test]
async fn observability_defaults_to_off_on_a_fresh_install() {
    let state = app_state().await;
    let (status, payload) = get_settings(state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(payload["observabilityEnabled"], false);
}

#[tokio::test]
async fn imported_9router_settings_honour_enable_observability() {
    let temp = tempdir().expect("tempdir");
    {
        // Only materialise the database, then drop the handle so the raw
        // connection below is the sole writer.
        Db::load_from(temp.path()).await.expect("db");
    }
    {
        let conn = rusqlite::Connection::open(temp.path().join("openproxy.sqlite")).unwrap();
        conn.execute(
            "INSERT INTO settings(id, data) VALUES(1, ?1)
             ON CONFLICT(id) DO UPDATE SET data = excluded.data",
            [json!({ "enableObservability": false, "observabilityMaxRecords": 10 }).to_string()],
        )
        .unwrap();
    }

    let db = Db::load_from(temp.path()).await.expect("reload");
    let snapshot = db.snapshot();
    assert!(
        !snapshot.settings.observability_enabled,
        "enableObservability: false must land on the typed field, not extra"
    );
    assert_eq!(snapshot.settings.observability_max_records, 10);
    assert!(!snapshot.settings.extra.contains_key("enableObservability"));

    // The inverse: an explicit `true` still wins over the now-false default.
    {
        let conn = rusqlite::Connection::open(temp.path().join("openproxy.sqlite")).unwrap();
        conn.execute(
            "UPDATE settings SET data = ?1 WHERE id = 1",
            [json!({ "enableObservability": true }).to_string()],
        )
        .unwrap();
    }
    let db = Db::load_from(temp.path()).await.expect("reload");
    assert!(db.snapshot().settings.observability_enabled);
}

#[tokio::test]
async fn patch_settings_accepts_enable_observability_alias() {
    let state = app_state().await;

    let (status, payload) = patch(state.clone(), json!({ "enableObservability": true })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(payload["observabilityEnabled"], true);

    let snapshot = state.db.snapshot();
    assert!(snapshot.settings.observability_enabled);
    assert!(!snapshot.settings.extra.contains_key("enableObservability"));
}
