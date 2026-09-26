//! Repository for `combos` table.

use rusqlite::{params, Connection};

use crate::types::Combo;

/// Columns that get their own SQL column; everything else a `Combo` carries
/// (its `extra` keys and `disabled_models`) lives in the `data` blob.
const CANONICAL_KEYS: [&str; 6] = ["id", "name", "kind", "models", "createdAt", "updatedAt"];

/// Serialize a combo into the `data` blob: the whole struct minus the columns
/// the table already stores.
///
/// Serializing `extra` alone used to drop `disabled_models`, so a member the
/// operator muted through `PUT /api/combos/{id}` was dispatched again after
/// every restart while `kind` / `strategy` / `isActive` — which do live in
/// `extra` — persisted. The shape matches `import.rs::combo_data_json`, which
/// the restore path already produces.
pub fn combo_data_json(c: &Combo) -> String {
    let mut blob = match serde_json::to_value(c) {
        Ok(serde_json::Value::Object(fields)) => fields,
        _ => serde_json::Map::new(),
    };
    blob.retain(|key, _| !CANONICAL_KEYS.contains(&key.as_str()));
    serde_json::to_string(&serde_json::Value::Object(blob)).unwrap_or_else(|_| "{}".into())
}

pub fn get_all(conn: &Connection) -> rusqlite::Result<Vec<Combo>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, kind, models, data, createdAt, updatedAt FROM combos ORDER BY name",
    )?;
    let rows = stmt.query_map([], row_to_combo)?;
    rows.collect()
}

pub fn get_by_name(conn: &Connection, name: &str) -> rusqlite::Result<Option<Combo>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, kind, models, data, createdAt, updatedAt FROM combos WHERE name = ?1",
    )?;
    let mut rows = stmt.query_map(params![name], row_to_combo)?;
    rows.next().transpose()
}

pub fn create(conn: &Connection, c: &Combo) -> rusqlite::Result<()> {
    let models_json = serde_json::to_string(&c.models).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "INSERT INTO combos(id, name, kind, models, data, createdAt, updatedAt) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![c.id, c.name, c.kind, models_json, combo_data_json(c), c.created_at.as_deref().unwrap_or(""), c.updated_at.as_deref().unwrap_or("")],
    )?;
    Ok(())
}

pub fn update(conn: &Connection, c: &Combo) -> rusqlite::Result<()> {
    let models_json = serde_json::to_string(&c.models).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "UPDATE combos SET kind=?2, models=?3, data=?4, updatedAt=?5 WHERE id=?1",
        params![
            c.id,
            c.kind,
            models_json,
            combo_data_json(c),
            c.updated_at.as_deref().unwrap_or("")
        ],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM combos WHERE id = ?1", params![id])?;
    Ok(())
}

fn row_to_combo(row: &rusqlite::Row<'_>) -> rusqlite::Result<Combo> {
    let id: String = row.get(0)?;
    let name: String = row.get(1)?;
    let kind: Option<String> = row.get(2)?;
    let models_json: String = row.get(3)?;
    let data_json: String = row.get(4)?;
    let created_at: String = row.get(5)?;
    let updated_at: String = row.get(6)?;

    let models: Vec<String> = serde_json::from_str(&models_json).unwrap_or_default();
    // `data` is the durable home of everything the columns do not carry:
    // `disabled_models` plus `Combo.extra` (strategy, isActive, fusionConfig,
    // judgeModel, …). It used to be bound to `let _data` and dropped, so every
    // restart silently lost the whole blob. Re-assemble the full struct — the
    // canonical columns always win — so `#[serde(flatten)] extra` absorbs
    // exactly the leftover keys. Rows written before this change carry `extra`
    // alone and still round-trip.
    let mut full = match serde_json::from_str(&data_json) {
        Ok(serde_json::Value::Object(fields)) => fields,
        _ => serde_json::Map::new(),
    };
    full.insert("id".into(), serde_json::Value::String(id));
    full.insert("name".into(), serde_json::Value::String(name));
    full.insert(
        "models".into(),
        serde_json::to_value(&models).unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
    );
    full.insert("createdAt".into(), serde_json::Value::String(created_at));
    full.insert("updatedAt".into(), serde_json::Value::String(updated_at));

    let mut combo: Combo =
        serde_json::from_value(serde_json::Value::Object(full)).unwrap_or_default();
    combo.kind = kind;
    Ok(combo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqliteDb;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn seeded() -> Combo {
        Combo {
            id: "c1".into(),
            name: "mycombo".into(),
            kind: Some("fallback".into()),
            models: vec!["openai/gpt-4o".into(), "anthropic/claude-sonnet".into()],
            disabled_models: vec!["anthropic/claude-sonnet".into()],
            created_at: Some("2026-01-01".into()),
            updated_at: Some("2026-01-01".into()),
            extra: BTreeMap::from([("strategy".into(), json!("round-robin"))]),
        }
    }

    #[test]
    fn roundtrip() {
        let db = SqliteDb::open_in_memory().unwrap();
        let combo = seeded();
        db.with_transaction(|tx| create(tx, &combo)).unwrap();
        let read = db
            .with_conn(|c| get_by_name(c, "mycombo"))
            .unwrap()
            .unwrap();
        assert_eq!(read.models.len(), 2);
        assert_eq!(read.extra.get("strategy"), Some(&json!("round-robin")));
    }

    // `disabled_models` is a declared field, so serializing only `extra` kept
    // it out of the blob: the dispatcher honoured a muted member until the
    // process restarted, then dispatched it again.
    #[test]
    fn disabled_models_survive_restart() {
        let db = SqliteDb::open_in_memory().unwrap();
        db.with_transaction(|tx| create(tx, &seeded())).unwrap();

        // Read back through the same path the daemon uses after a reload.
        let all = db.with_conn(|c| get_all(c)).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].disabled_models, vec!["anthropic/claude-sonnet"]);
        assert_eq!(all[0].extra.get("strategy"), Some(&json!("round-robin")));
    }

    #[test]
    fn update_preserves_disabled_models() {
        let db = SqliteDb::open_in_memory().unwrap();
        db.with_transaction(|tx| create(tx, &seeded())).unwrap();

        let mut next = db
            .with_conn(|c| get_by_name(c, "mycombo"))
            .unwrap()
            .unwrap();
        next.disabled_models = vec!["openai/gpt-4o".into()];
        db.with_transaction(|tx| update(tx, &next)).unwrap();

        let read = db
            .with_conn(|c| get_by_name(c, "mycombo"))
            .unwrap()
            .unwrap();
        assert_eq!(read.disabled_models, vec!["openai/gpt-4o"]);
        assert_eq!(read.extra.get("strategy"), Some(&json!("round-robin")));
    }

    /// A blob written before `disabled_models` joined it — `extra` only —
    /// still reads back, because `extra` is the `#[serde(flatten)]` remainder.
    #[test]
    fn legacy_extra_only_blob_still_round_trips() {
        let db = SqliteDb::open_in_memory().unwrap();
        let combo = seeded();
        let legacy = serde_json::to_string(&combo.extra).unwrap();
        db.with_transaction(|tx| {
            tx.execute(
                "INSERT INTO combos(id, name, kind, models, data, createdAt, updatedAt) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    combo.id,
                    combo.name,
                    combo.kind,
                    serde_json::to_string(&combo.models).unwrap(),
                    legacy,
                    "2026-01-01",
                    "2026-01-01"
                ],
            )
        })
        .unwrap();

        let read = db
            .with_conn(|c| get_by_name(c, "mycombo"))
            .unwrap()
            .unwrap();
        assert_eq!(read.extra.get("strategy"), Some(&json!("round-robin")));
        assert!(read.disabled_models.is_empty());
    }

    #[test]
    fn unique_name_constraint() {
        let db = SqliteDb::open_in_memory().unwrap();
        let c1 = Combo {
            id: "c1".into(),
            name: "same".into(),
            models: vec!["a".into()],
            created_at: Some("2026-01-01".into()),
            updated_at: Some("2026-01-01".into()),
            ..Default::default()
        };
        let mut c2 = c1.clone();
        c2.id = "c2".into();
        db.with_transaction(|tx| create(tx, &c1)).unwrap();
        let result = db.with_transaction(|tx| create(tx, &c2));
        assert!(result.is_err());
    }
}
