//! Repository for the free-form `_meta` key/value table.
//!
//! `_meta` holds cross-cutting state that belongs to no single feature table:
//! the schema version stamped by [`crate::db::sqlite::migrations`] and the
//! durable request counter 9router keeps there.

use rusqlite::{params, Connection, OptionalExtension};

/// Key for the lifetime request count. 9router writes this exact name
/// (usageRepo.js `saveRequestUsage`), so a backup taken by either side stays
/// readable by the other.
pub const TOTAL_REQUESTS_LIFETIME: &str = "totalRequestsLifetime";

/// Read a numeric `_meta` value. `None` when the key was never written or
/// holds something that isn't a number.
pub fn get(conn: &Connection, key: &str) -> rusqlite::Result<Option<u64>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM _meta WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw.and_then(|value| value.parse::<u64>().ok()))
}

/// Write a numeric `_meta` value. Idempotent.
pub fn set(conn: &Connection, key: &str, value: u64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO _meta(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value.to_string()],
    )?;
    Ok(())
}

/// Add one to a counter and return the new value. Callers run this inside the
/// same transaction as the write it counts, so the total can never drift away
/// from the rows that make it up.
pub fn increment(conn: &Connection, key: &str) -> rusqlite::Result<u64> {
    let next = get(conn, key)?.unwrap_or(0) + 1;
    set(conn, key, next)?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqliteDb;

    #[test]
    fn increments_from_absent_key() {
        let db = SqliteDb::open_in_memory().unwrap();
        let (first, second) = db
            .with_transaction(|conn| Ok((increment(conn, "hits")?, increment(conn, "hits")?)))
            .unwrap();
        assert_eq!((first, second), (1, 2));
        assert_eq!(db.with_conn(|conn| get(conn, "hits")).unwrap(), Some(2));
    }

    #[test]
    fn get_is_none_for_unset_key() {
        let db = SqliteDb::open_in_memory().unwrap();
        assert_eq!(db.with_conn(|conn| get(conn, "nope")).unwrap(), None);
    }
}
