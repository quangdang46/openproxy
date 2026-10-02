//! Repository for `proxyPools` table.

use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};

use crate::types::ProxyPool;

pub fn get_active(conn: &Connection) -> rusqlite::Result<Vec<ProxyPool>> {
    let mut stmt = conn.prepare(
        "SELECT id, isActive, testStatus, data, createdAt, updatedAt FROM proxyPools WHERE isActive IS NOT 0"
    )?;
    let rows = stmt.query_map([], row_to_pool)?;
    rows.collect()
}

pub fn get_by_id(conn: &Connection, id: &str) -> rusqlite::Result<Option<ProxyPool>> {
    let mut stmt = conn.prepare(
        "SELECT id, isActive, testStatus, data, createdAt, updatedAt FROM proxyPools WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(params![id], row_to_pool)?;
    rows.next().transpose()
}

pub fn create(conn: &Connection, p: &ProxyPool) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO proxyPools(id, isActive, testStatus, data, createdAt, updatedAt) VALUES(?1,?2,?3,?4,?5,?6)",
        params![p.id, p.is_active.map(|v| v as i32).unwrap_or(1), p.test_status, pool_to_data(p), p.created_at.as_deref().unwrap_or(""), p.updated_at.as_deref().unwrap_or("")],
    )?;
    Ok(())
}

pub fn update(conn: &Connection, p: &ProxyPool) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE proxyPools SET isActive=?2, testStatus=?3, data=?4, updatedAt=?5 WHERE id=?1",
        params![
            p.id,
            p.is_active.map(|v| v as i32).unwrap_or(1),
            p.test_status,
            pool_to_data(p),
            p.updated_at.as_deref().unwrap_or("")
        ],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM proxyPools WHERE id = ?1", params![id])?;
    Ok(())
}

/// The `data` blob holds every field that has no dedicated column. `export.rs`
/// merges it back over the row, so a field missing here is a field that
/// silently vanishes on the next read — which is exactly what happened to
/// `name` and `proxyUrl` (see `pool_to_data`).
fn row_to_pool(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProxyPool> {
    let id: String = row.get(0)?;
    let is_active: Option<i32> = row.get(1)?;
    let test_status: Option<String> = row.get(2)?;
    let data_str: Option<String> = row.get(3)?;
    let created_at: String = row.get(4)?;
    let updated_at: String = row.get(5)?;

    let mut pool = ProxyPool {
        id,
        is_active: is_active.map(|v| v != 0),
        test_status,
        created_at: Some(created_at),
        updated_at: Some(updated_at),
        ..Default::default()
    };

    if let Some(data_str) = data_str {
        if let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(&data_str) {
            for (key, value) in fields {
                match key.as_str() {
                    "name" => pool.name = value.as_str().unwrap_or_default().to_string(),
                    "proxyUrl" => pool.proxy_url = value.as_str().unwrap_or_default().to_string(),
                    "noProxy" => pool.no_proxy = value.as_str().unwrap_or_default().to_string(),
                    "type" => pool.r#type = value.as_str().unwrap_or_default().to_string(),
                    "strictProxy" => pool.strict_proxy = value.as_bool(),
                    "lastTestedAt" => pool.last_tested_at = value.as_str().map(str::to_string),
                    "lastError" => pool.last_error = value.as_str().map(str::to_string),
                    "successRate" => pool.success_rate = value.as_f64(),
                    "rttMs" => {
                        pool.rtt_ms = value.as_u64().or_else(|| {
                            value
                                .as_f64()
                                .and_then(|v| v.is_finite().then_some(v as u64))
                        });
                    }
                    _ => {
                        pool.extra.insert(key, value);
                    }
                }
            }
        }
    }

    Ok(pool)
}

/// Serialise the non-column fields into the `data` blob.
///
/// This used to write only `p.extra`, so `pool create` / `pool apply` inserted
/// rows with `data = '{}'` and the pool's name and proxy URL were lost the
/// moment the row was read back — `pool list` showed an empty list and
/// `pool get` reported "not found" for a pool that had just been created.
/// `node_repo::node_to_data` is the reference shape for this.
fn pool_to_data(p: &ProxyPool) -> String {
    let mut fields = Map::new();
    fields.insert("name".into(), json!(p.name));
    fields.insert("proxyUrl".into(), json!(p.proxy_url));
    if !p.no_proxy.is_empty() {
        fields.insert("noProxy".into(), json!(p.no_proxy));
    }
    fields.insert("type".into(), json!(p.r#type));
    if let Some(strict) = p.strict_proxy {
        fields.insert("strictProxy".into(), json!(strict));
    }
    if let Some(tested) = &p.last_tested_at {
        fields.insert("lastTestedAt".into(), json!(tested));
    }
    if let Some(err) = &p.last_error {
        fields.insert("lastError".into(), json!(err));
    }
    if let Some(rate) = p.success_rate {
        fields.insert("successRate".into(), json!(rate));
    }
    if let Some(rtt) = p.rtt_ms {
        fields.insert("rttMs".into(), json!(rtt));
    }
    for (k, v) in &p.extra {
        fields.insert(k.clone(), v.clone());
    }
    serde_json::to_string(&fields).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqliteDb;
    use serde_json::json;

    #[test]
    fn roundtrip() {
        let db = SqliteDb::open_in_memory().unwrap();
        let pool = ProxyPool {
            id: "p1".into(),
            is_active: Some(true),
            test_status: Some("active".into()),
            created_at: Some("2026-01-01".into()),
            updated_at: Some("2026-01-01".into()),
            ..Default::default()
        };
        db.with_transaction(|tx| create(tx, &pool)).unwrap();
        let read = db.with_conn(|c| get_by_id(c, "p1")).unwrap().unwrap();
        assert_eq!(read.test_status.as_deref(), Some("active"));
    }

    /// `name` and `proxyUrl` have no dedicated column, so they live in the
    /// `data` blob. `pool_to_data` used to serialise only `extra`, which wrote
    /// `data = '{}'` and lost both fields on read-back — `pool list` came back
    /// empty and `pool get` reported "not found" for a pool just created.
    #[test]
    fn roundtrip_preserves_name_and_proxy_url() {
        let db = SqliteDb::open_in_memory().unwrap();
        let pool = ProxyPool {
            id: "p2".into(),
            name: "us-east".into(),
            proxy_url: "http://proxy.example.com:8080".into(),
            no_proxy: "localhost,127.0.0.1".into(),
            r#type: "http".into(),
            is_active: Some(true),
            strict_proxy: Some(true),
            created_at: Some("2026-01-01".into()),
            updated_at: Some("2026-01-01".into()),
            ..Default::default()
        };
        db.with_transaction(|tx| create(tx, &pool)).unwrap();

        let read = db.with_conn(|c| get_by_id(c, "p2")).unwrap().unwrap();
        assert_eq!(read.name, "us-east");
        assert_eq!(read.proxy_url, "http://proxy.example.com:8080");
        assert_eq!(read.no_proxy, "localhost,127.0.0.1");
        assert_eq!(read.r#type, "http");
        assert_eq!(read.strict_proxy, Some(true));

        // `get_active` is what the API list path reads through.
        let active = db.with_conn(|c| get_active(c)).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "us-east");
    }

    /// `update` must rewrite the blob too, not just the columns — otherwise
    /// editing a pool's URL leaves the old one in place.
    #[test]
    fn update_rewrites_the_data_blob() {
        let db = SqliteDb::open_in_memory().unwrap();
        let pool = ProxyPool {
            id: "p3".into(),
            name: "us-east".into(),
            proxy_url: "http://old.example.com:8080".into(),
            created_at: Some("2026-01-01".into()),
            updated_at: Some("2026-01-01".into()),
            ..Default::default()
        };
        db.with_transaction(|tx| create(tx, &pool)).unwrap();

        let mut edited = pool.clone();
        edited.proxy_url = "http://new.example.com:8080".into();
        edited.name = "eu-west".into();
        db.with_transaction(|tx| update(tx, &edited)).unwrap();

        let read = db.with_conn(|c| get_by_id(c, "p3")).unwrap().unwrap();
        assert_eq!(read.proxy_url, "http://new.example.com:8080");
        assert_eq!(read.name, "eu-west");
    }
}
