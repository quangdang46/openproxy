//! Repository for `apiKeys` table.

use rusqlite::{params, Connection};

use crate::core::key_access::{key_access_from_columns, key_access_to_columns};
use crate::types::ApiKey;

const COLUMNS: &str =
    "id, key, name, machineId, isActive, createdAt, monthly_budget_usd, accessRestricted, accessAllow";

pub fn get_active(conn: &Connection) -> rusqlite::Result<Vec<ApiKey>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM apiKeys WHERE isActive IS NOT 0"
    ))?;
    let rows = stmt.query_map([], row_to_api_key)?;
    rows.collect()
}

pub fn get_all(conn: &Connection) -> rusqlite::Result<Vec<ApiKey>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM apiKeys ORDER BY createdAt DESC"
    ))?;
    let rows = stmt.query_map([], row_to_api_key)?;
    rows.collect()
}

pub fn get_by_id(conn: &Connection, id: &str) -> rusqlite::Result<Option<ApiKey>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM apiKeys WHERE id = ?1"))?;
    let mut rows = stmt.query_map(params![id], row_to_api_key)?;
    rows.next().transpose()
}

pub fn create(conn: &Connection, k: &ApiKey) -> rusqlite::Result<()> {
    let (access_restricted, access_allow) = key_access_to_columns(&k.access);
    conn.execute(
        "INSERT INTO apiKeys(id, key, name, machineId, isActive, createdAt, monthly_budget_usd, accessRestricted, accessAllow) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![k.id, k.key, k.name, k.machine_id, k.is_active.map(|v| v as i32).unwrap_or(1), k.created_at.as_deref().unwrap_or(""), k.monthly_budget_usd, access_restricted, access_allow],
    )?;
    Ok(())
}

pub fn update(conn: &Connection, k: &ApiKey) -> rusqlite::Result<()> {
    let (access_restricted, access_allow) = key_access_to_columns(&k.access);
    conn.execute(
        "UPDATE apiKeys SET key=?2, name=?3, machineId=?4, isActive=?5, monthly_budget_usd=?6, accessRestricted=?7, accessAllow=?8 WHERE id=?1",
        params![
            k.id,
            k.key,
            k.name,
            k.machine_id,
            k.is_active.map(|v| v as i32).unwrap_or(1),
            k.monthly_budget_usd,
            access_restricted,
            access_allow
        ],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM apiKeys WHERE id = ?1", params![id])?;
    Ok(())
}

fn row_to_api_key(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApiKey> {
    let id: String = row.get(0)?;
    let key: String = row.get(1)?;
    let name: Option<String> = row.get(2)?;
    let machine_id: Option<String> = row.get(3)?;
    let is_active: Option<i32> = row.get(4)?;
    let created_at: String = row.get(5)?;
    let monthly_budget_usd: Option<f64> = row.get(6)?;
    let access_restricted: Option<i64> = row.get(7)?;
    let access_allow: Option<String> = row.get(8)?;

    Ok(ApiKey {
        id,
        key,
        name: name.unwrap_or_default(),
        machine_id,
        is_active: is_active.map(|v| v != 0),
        created_at: Some(created_at),
        monthly_budget_usd,
        access: key_access_from_columns(access_restricted, access_allow.as_deref()),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqliteDb;
    use serde_json::json;

    #[test]
    fn roundtrip() {
        let db = SqliteDb::open_in_memory().unwrap();
        let key = ApiKey {
            id: "k1".into(),
            key: "sk-test-machineid-12345678".into(),
            name: "test".into(),
            machine_id: Some("machine123".into()),
            is_active: Some(true),
            created_at: Some("2026-01-01".into()),
            ..Default::default()
        };
        db.with_transaction(|tx| create(tx, &key)).unwrap();
        let read = db.with_conn(|c| get_by_id(c, "k1")).unwrap().unwrap();
        assert_eq!(read.key, "sk-test-machineid-12345678");
    }

    #[test]
    fn access_columns_roundtrip() {
        let db = SqliteDb::open_in_memory().unwrap();
        let key = ApiKey {
            id: "k1".into(),
            key: "sk-restricted".into(),
            name: "restricted".into(),
            is_active: Some(true),
            created_at: Some("2026-01-01".into()),
            access: crate::types::KeyAccess {
                restricted: true,
                allow: vec!["Main".into(), "cx/gpt-5".into()],
            },
            ..Default::default()
        };
        db.with_transaction(|tx| create(tx, &key)).unwrap();
        let read = db.with_conn(|c| get_by_id(c, "k1")).unwrap().unwrap();
        assert!(read.access.restricted);
        assert_eq!(read.access.allow, vec!["Main".to_string(), "cx/gpt-5".to_string()]);

        // Clearing the restriction persists as unrestricted.
        let mut cleared = read.clone();
        cleared.access = Default::default();
        db.with_transaction(|tx| update(tx, &cleared)).unwrap();
        let read = db.with_conn(|c| get_by_id(c, "k1")).unwrap().unwrap();
        assert!(!read.access.restricted);
        assert!(read.access.allow.is_empty());
    }

    #[test]
    fn unique_key_constraint() {
        let db = SqliteDb::open_in_memory().unwrap();
        let k1 = ApiKey {
            id: "k1".into(),
            key: "same".into(),
            is_active: Some(true),
            created_at: Some("2026-01-01".into()),
            ..Default::default()
        };
        let mut k2 = k1.clone();
        k2.id = "k2".into();
        db.with_transaction(|tx| create(tx, &k1)).unwrap();
        let result = db.with_transaction(|tx| create(tx, &k2));
        assert!(result.is_err());
    }
}
