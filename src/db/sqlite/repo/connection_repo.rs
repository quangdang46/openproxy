//! Repository for `providerConnections` table.
//!
//! JSON blob stored in `data` column; secret fields are AES-encrypted
//! via [`crate::db::crypto`] before insertion and decrypted after read.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;

use serde_json::Value;

use crate::types::ProviderConnection;

/// Column list (excluding the dynamic `data` payload) used for all SELECTs.
const COLUMNS: &str =
    "id, provider, authType, name, email, priority, isActive, createdAt, updatedAt";

fn row_to_connection(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProviderConnection> {
    let data_str: String = row.get(8)?;
    let mut data: Value =
        serde_json::from_str(&data_str).unwrap_or(Value::Object(Default::default()));

    if let Some(obj) = data.as_object_mut() {
        obj.insert("id".into(), Value::String(row.get(0)?));
        obj.insert("provider".into(), Value::String(row.get(1)?));
        obj.insert("authType".into(), Value::String(row.get(2)?));
        if let Ok(v) = row.get::<_, Option<String>>(3) {
            if let Some(v) = v {
                obj.insert("name".into(), Value::String(v));
            }
        }
        if let Ok(v) = row.get::<_, Option<String>>(4) {
            if let Some(v) = v {
                obj.insert("email".into(), Value::String(v));
            }
        }
        if let Ok(v) = row.get::<_, Option<i64>>(5) {
            obj.insert("priority".into(), json!(v));
        }
        if let Ok(v) = row.get::<_, Option<bool>>(6) {
            obj.insert("isActive".into(), json!(v));
        }
        obj.insert("createdAt".into(), Value::String(row.get(7)?));
        obj.insert("updatedAt".into(), Value::String(row.get(8)?));
    }

    let encryption_key = crate::db::crypto::encryption_key().unwrap_or_default();
    let mut conn = ProviderConnection::default();
    let data_str = serde_json::to_string(&data).unwrap_or_default();
    let parsed: Value = serde_json::from_str(&data_str).unwrap_or_default();
    let mut app_db = crate::types::AppDb::default();
    app_db.provider_connections.push(conn.clone());
    app_db.provider_connections = vec![];
    // Deserialize from JSON
    let json_val = serde_json::to_value(&data).unwrap_or_default();
    crate::db::crypto::decrypt_connection(&mut conn, &encryption_key);
    // We need a simpler approach — just build from the row directly
    let mut c = ProviderConnection {
        id: row.get(0)?,
        provider: row.get(1)?,
        auth_type: row.get(2)?,
        ..Default::default()
    };
    if let Ok(v) = row.get::<_, Option<String>>(3) {
        c.name = v;
    }
    if let Ok(v) = row.get::<_, Option<String>>(4) {
        c.email = v;
    }
    if let Ok(v) = row.get::<_, Option<i64>>(5) {
        c.priority = v.map(|x| x as u32);
    }
    if let Ok(v) = row.get::<_, Option<bool>>(6) {
        c.is_active = v;
    }
    c.created_at = Some(row.get(7)?);
    c.updated_at = Some(row.get(8)?);

    // Merge data blob
    if let Some(obj) = data.as_object() {
        let data_str = serde_json::to_string(obj).unwrap_or_default();
        if let Ok(mut parsed) = serde_json::from_str::<crate::types::AppDb>(&format!(
            r#"{{"providerConnections":[{}]}}"#,
            data_str
        )) {
            if let Some(pc) = parsed.provider_connections.pop() {
                c.access_token = pc.access_token;
                c.refresh_token = pc.refresh_token;
                c.expires_at = pc.expires_at;
                c.api_key = pc.api_key;
                c.test_status = pc.test_status;
                c.last_error = pc.last_error;
                c.rate_limited_until = pc.rate_limited_until;
                c.backoff_level = pc.backoff_level;
                c.consecutive_errors = pc.consecutive_errors;
                c.consecutive_use_count = pc.consecutive_use_count;
                c.proxy_url = pc.proxy_url;
                c.proxy_label = pc.proxy_label;
                c.use_connection_proxy = pc.use_connection_proxy;
                c.provider_specific_data = pc.provider_specific_data;
                c.extra = pc.extra;
            }
        }
    }

    crate::db::crypto::decrypt_connection(&mut c, &encryption_key);
    Ok(c)
}

/// Build the serializable portion of a providerConnection minus the
/// indexed columns (those are stored as separate columns). Returns the
/// JSON string for the `data` column.
fn connection_to_data(c: &ProviderConnection, encryption_key: &str) -> String {
    let mut clone = c.clone();
    crate::db::crypto::encrypt_connection(&mut clone, encryption_key);
    let json_val = serde_json::to_value(&clone).unwrap_or_default();
    if let Some(mut obj) = json_val.as_object().cloned() {
        obj.remove("id");
        obj.remove("provider");
        obj.remove("auth_type");
        obj.remove("name");
        obj.remove("email");
        obj.remove("priority");
        obj.remove("is_active");
        obj.remove("created_at");
        obj.remove("updated_at");
        serde_json::to_string(&obj).unwrap_or_default()
    } else {
        "{}".to_string()
    }
}

pub fn get_active(conn: &Connection, provider: &str) -> rusqlite::Result<Vec<ProviderConnection>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS}, data FROM providerConnections WHERE provider = ?1 AND isActive IS NOT 0 ORDER BY priority"
    ))?;
    let rows = stmt.query_map(params![provider], row_to_connection)?;
    rows.collect()
}

pub fn get_by_id(conn: &Connection, id: &str) -> rusqlite::Result<Option<ProviderConnection>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS}, data FROM providerConnections WHERE id = ?1"
    ))?;
    let mut rows = stmt.query_map(params![id], row_to_connection)?;
    Ok(rows.next().transpose()?)
}

pub fn create(conn: &Connection, c: &ProviderConnection) -> rusqlite::Result<()> {
    let encryption_key = crate::db::crypto::encryption_key().unwrap_or_default();
    let data = connection_to_data(c, &encryption_key);
    conn.execute(
        "INSERT INTO providerConnections(id, provider, authType, name, email, priority, isActive, data, createdAt, updatedAt)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            c.id, c.provider, c.auth_type, c.name, c.email,
            c.priority.map(|v| v as i64), c.is_active.map(|v| v as i32),
            data, c.created_at.as_deref().unwrap_or(""), c.updated_at.as_deref().unwrap_or(""),
        ],
    )?;
    Ok(())
}

pub fn update(conn: &Connection, c: &ProviderConnection) -> rusqlite::Result<()> {
    let encryption_key = crate::db::crypto::encryption_key().unwrap_or_default();
    let data = connection_to_data(c, &encryption_key);
    conn.execute(
        "UPDATE providerConnections SET provider=?2, authType=?3, name=?4, email=?5, priority=?6, isActive=?7, data=?8, updatedAt=?9 WHERE id=?1",
        params![
            c.id, c.provider, c.auth_type, c.name, c.email,
            c.priority.map(|v| v as i64), c.is_active.map(|v| v as i32),
            data, c.updated_at.as_deref().unwrap_or(""),
        ],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM providerConnections WHERE id = ?1", params![id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (crate::db::sqlite::SqliteDb, ProviderConnection) {
        let db = crate::db::sqlite::SqliteDb::open_in_memory().unwrap();
        let c = ProviderConnection {
            id: "test-1".into(),
            provider: "openai".into(),
            auth_type: "apikey".into(),
            name: Some("test".into()),
            email: Some("test@example.com".into()),
            priority: Some(1),
            is_active: Some(true),
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
            api_key: Some("sk-test123".into()),
            ..Default::default()
        };
        (db, c)
    }

    #[test]
    fn roundtrip_create_and_read() {
        let (db, c) = setup();
        db.with_transaction(|tx| create(tx, &c)).unwrap();
        let read = db.with_conn(|c| get_by_id(c, "test-1")).unwrap().unwrap();
        assert_eq!(read.provider, "openai");
        assert_eq!(read.name.as_deref(), Some("test"));
    }

    #[test]
    fn get_active_filters_by_provider() {
        let (db, mut c1) = setup();
        let mut c2 = c1.clone();
        c2.id = "test-2".into();
        c2.provider = "anthropic".into();
        c1.id = "test-1".into();

        db.with_transaction(|tx| {
            create(tx, &c1)?;
            create(tx, &c2)
        })
        .unwrap();

        let openai = db.with_conn(|c| get_active(c, "openai")).unwrap();
        assert_eq!(openai.len(), 1);
        assert_eq!(openai[0].id, "test-1");
    }

    #[test]
    fn update_modifies_row() {
        let (db, mut c) = setup();
        db.with_transaction(|tx| create(tx, &c)).unwrap();
        c.name = Some("updated".into());
        db.with_transaction(|tx| update(tx, &c)).unwrap();
        let read = db.with_conn(|c| get_by_id(c, "test-1")).unwrap().unwrap();
        assert_eq!(read.name.as_deref(), Some("updated"));
    }

    #[test]
    fn delete_removes_row() {
        let (db, c) = setup();
        db.with_transaction(|tx| create(tx, &c)).unwrap();
        db.with_transaction(|tx| delete(tx, "test-1")).unwrap();
        let read = db.with_conn(|c| get_by_id(c, "test-1")).unwrap();
        assert!(read.is_none());
    }
}
