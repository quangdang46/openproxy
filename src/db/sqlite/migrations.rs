//! Versioned migration runner for the OpenProxy SQLite schema.
//!
//! Migration files live under `src/db/sqlite/migrations/` and follow the
//! naming convention `NNNN_description.sql`. Each file is wrapped in a
//! transaction by [`apply_pending_migrations`].
//!
//! Currently the schema is initialized via [`crate::db::sqlite::schema::TABLES_SQL`]
//! (DDL is idempotent thanks to `IF NOT EXISTS`), and this module records the
//! schema version into `_meta`. Because `CREATE TABLE IF NOT EXISTS` is a no-op
//! on an existing table, [`sync_schema_from_tables`] runs first to backfill
//! columns added to `TABLES_SQL` after a database was created — the version
//! stamp follows that sync, it does not stand in for it.

use std::collections::HashSet;

use rusqlite::{params, Connection, OptionalExtension};

use super::schema::{DECLARED_COLUMNS, SCHEMA_VERSION};

/// Get the current schema version stored in `_meta`. Returns 0 if the row
/// is missing (fresh DB).
pub fn get_schema_version(conn: &Connection) -> rusqlite::Result<i32> {
    conn.query_row(
        "SELECT value FROM _meta WHERE key = 'schema_version'",
        [],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map(|opt| opt.and_then(|s| s.parse::<i32>().ok()).unwrap_or(0))
}

/// Stamp the active schema version into `_meta`. Idempotent.
pub fn set_schema_version(conn: &Connection, version: i32) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO _meta(key, value) VALUES('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![version.to_string()],
    )?;
    Ok(())
}

/// Run any pending migrations between `current` and [`SCHEMA_VERSION`].
///
/// This module currently has no version-gated migrations (the schema is fully
/// expressed by `TABLES_SQL`). The function exists as the extension point
/// for future schema-evolution scripts; for now it applies the additive column
/// sync and then stamps the version so callers can distinguish "fresh DB" from
/// "DB at current version".
pub fn apply_pending_migrations(conn: &Connection) -> rusqlite::Result<()> {
    // Bring databases created before a column was added up to date. This
    // replaced the hand-written `add_api_keys_budget_column` patch: one registry
    // in `schema.rs` now covers every table, so the next added column needs no
    // new helper here.
    sync_schema_from_tables(conn)?;

    // Repair the combos whose `kind` column was written with a dispatch
    // strategy by `openproxy combo create --strategy` (bead openproxy-63hh).
    // `kind` means media modality, so a value like "round-robin" hides the
    // combo from the Combos page and from GET /v1/models. The write path is
    // fixed, but existing databases still hold the corrupted rows, so the
    // repair has to run on open — otherwise the fix only ever helps newly
    // created databases. Idempotent: a no-op once no rows match.
    crate::db::sqlite::patch::clear_combo_kind_strategy_leak(conn)?;

    let current = get_schema_version(conn)?;
    if current < SCHEMA_VERSION {
        set_schema_version(conn, SCHEMA_VERSION)?;
    }
    Ok(())
}

/// Additive-only column sync — 9router `syncSchemaFromTables`
/// (`9router/src/lib/db/migrate.js:79`).
///
/// `CREATE TABLE IF NOT EXISTS` leaves an existing table untouched, so a column
/// appended to `TABLES_SQL` after a database was created would never reach it.
/// This diffs each declared table's live `PRAGMA table_info` against
/// [`DECLARED_COLUMNS`] and `ALTER TABLE … ADD COLUMN`s whatever is missing.
///
/// Never drops, renames, or retypes. A column that cannot be added is logged
/// and skipped rather than failing the boot, matching 9router's per-column
/// `try`/`catch` — a refused additive change must not stop the router.
pub fn sync_schema_from_tables(conn: &Connection) -> rusqlite::Result<()> {
    for (table, columns) in DECLARED_COLUMNS {
        if !table_exists(conn, table)? {
            continue;
        }
        let present = column_names(conn, table)?;
        for (name, def) in *columns {
            if present.contains(*name) {
                continue;
            }
            // Table names and defs are compile-time constants, so the
            // interpolated SQL below is not caller-controlled.
            match conn.execute(
                &format!(
                    "ALTER TABLE \"{table}\" ADD COLUMN \"{name}\" {}",
                    strip_add_column_constraints(def)
                ),
                [],
            ) {
                Ok(_) => tracing::info!(target: "openproxy::db", "added column {table}.{name}"),
                Err(err) => tracing::warn!(
                    target: "openproxy::db",
                    "add column {table}.{name} failed: {err}"
                ),
            }
        }
    }
    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
}

/// `PRAGMA table_info` takes no bind parameters, hence the interpolated name.
fn column_names(conn: &Connection, table: &str) -> rusqlite::Result<HashSet<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect()
}

/// SQLite's `ADD COLUMN` rejects `PRIMARY KEY` and `UNIQUE`; 9router strips
/// both before issuing the ALTER (`migrate.js:85`).
fn strip_add_column_constraints(def: &str) -> String {
    let mut out = def.to_string();
    for keyword in ["PRIMARY KEY", "UNIQUE"] {
        while let Some(pos) = out.to_ascii_uppercase().find(keyword) {
            let mut rest = &out[pos + keyword.len()..];
            if keyword == "PRIMARY KEY" {
                // `AUTOINCREMENT` only ever follows `PRIMARY KEY`, and only
                // there does leaving it behind produce invalid DDL.
                let tail = rest.trim_start();
                if tail.len() >= "AUTOINCREMENT".len()
                    && tail[.."AUTOINCREMENT".len()].eq_ignore_ascii_case("AUTOINCREMENT")
                {
                    rest = &tail["AUTOINCREMENT".len()..];
                }
            }
            out = format!("{}{}", &out[..pos], rest);
        }
    }
    // Each removal leaves the surrounding spaces behind; the DDL is only ever
    // handed to SQLite, so collapsing the runs is free and keeps it readable.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn fresh() -> Connection {
        Connection::open_in_memory().expect("in-memory sqlite")
    }

    #[test]
    fn fresh_db_has_version_zero() {
        let conn = fresh();
        // Pre-create _meta since apply_pending_migrations needs it.
        conn.execute_batch("CREATE TABLE _meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        assert_eq!(get_schema_version(&conn).unwrap(), 0);
    }

    #[test]
    fn stamping_sets_version() {
        let conn = fresh();
        conn.execute_batch("CREATE TABLE _meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        set_schema_version(&conn, 5).unwrap();
        assert_eq!(get_schema_version(&conn).unwrap(), 5);
    }

    #[test]
    fn apply_pending_brings_to_target() {
        let conn = fresh();
        conn.execute_batch("CREATE TABLE _meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        set_schema_version(&conn, SCHEMA_VERSION - 1).unwrap();
        apply_pending_migrations(&conn).unwrap();
        assert_eq!(get_schema_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn add_column_defs_lose_only_the_constraints_sqlite_rejects() {
        assert_eq!(
            strip_add_column_constraints("INTEGER PRIMARY KEY AUTOINCREMENT"),
            "INTEGER"
        );
        assert_eq!(
            strip_add_column_constraints("TEXT UNIQUE NOT NULL"),
            "TEXT NOT NULL"
        );
        assert_eq!(
            strip_add_column_constraints("INTEGER PRIMARY KEY CHECK (id = 1)"),
            "INTEGER CHECK (id = 1)"
        );
        assert_eq!(
            strip_add_column_constraints("TEXT NOT NULL DEFAULT '{}'"),
            "TEXT NOT NULL DEFAULT '{}'"
        );
    }

    #[test]
    fn sync_adds_missing_columns_and_is_idempotent() {
        let conn = fresh();
        // A database written before `createdAt` existed on apiKeys. Empty, so
        // SQLite permits the NOT NULL column — the refusal path has its own
        // test in the integration suite.
        conn.execute_batch(
            "CREATE TABLE apiKeys (
                id   TEXT PRIMARY KEY,
                key  TEXT UNIQUE NOT NULL,
                name TEXT,
                machineId TEXT,
                isActive INTEGER NOT NULL DEFAULT 1
             );",
        )
        .unwrap();

        sync_schema_from_tables(&conn).unwrap();
        let first = column_names(&conn, "apiKeys").unwrap();
        assert!(first.contains("createdAt"));
        assert!(first.contains("monthly_budget_usd"));

        // A second pass finds nothing left to do.
        sync_schema_from_tables(&conn).unwrap();
        assert_eq!(column_names(&conn, "apiKeys").unwrap(), first);
    }

    #[test]
    fn sync_skips_tables_the_database_never_created() {
        let conn = fresh();
        // Only `_meta` exists; the sync must not fail trying to alter the rest.
        conn.execute_batch("CREATE TABLE _meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        sync_schema_from_tables(&conn).unwrap();
    }
}
