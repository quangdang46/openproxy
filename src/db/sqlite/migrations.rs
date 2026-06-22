//! Versioned migration runner for the OpenProxy SQLite schema.
//!
//! Migration files live under `src/db/sqlite/migrations/` and follow the
//! naming convention `NNNN_description.sql`. Each file is wrapped in a
//! transaction by [`apply_pending_migrations`].
//!
//! Currently the schema is initialized via [`crate::db::sqlite::schema::TABLES_SQL`]
//! (DDL is idempotent thanks to `IF NOT EXISTS`), and this module just
//! records the schema version into `_meta`. Future migrations append here.

use rusqlite::{params, Connection, OptionalExtension};

use super::schema::SCHEMA_VERSION;

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
/// This module currently has no incremental migrations (the schema is fully
/// expressed by `TABLES_SQL`). The function exists as the extension point
/// for future schema-evolution scripts; for now it only stamps the version
/// so callers can distinguish "fresh DB" from "DB at current version".
pub fn apply_pending_migrations(conn: &Connection) -> rusqlite::Result<()> {
    let current = get_schema_version(conn)?;
    if current < SCHEMA_VERSION {
        // No-op for now — TABLES_SQL already brings a fresh DB to the
        // current shape. We just bump the recorded version.
        set_schema_version(conn, SCHEMA_VERSION)?;
    }
    Ok(())
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
}
