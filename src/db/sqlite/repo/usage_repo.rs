//! Repository for `usageHistory` and `usageDaily` tables.

use std::collections::BTreeMap;

use rusqlite::{params, Connection};
use serde_json::Value;

use crate::types::{DailySummary, UsageEntry};

pub fn get_history(
    conn: &Connection,
    limit: i64,
    offset: i64,
) -> rusqlite::Result<Vec<UsageEntry>> {
    let mut stmt = conn.prepare(
        "SELECT timestamp, provider, model, connectionId, apiKey, endpoint,
                promptTokens, completionTokens, cost, status, tokens, meta,
                bytesBefore, bytesAfter, bytesSaved, imagePrompts
         FROM usageHistory ORDER BY timestamp DESC LIMIT ?1 OFFSET ?2",
    )?;
    let rows = stmt.query_map(params![limit, offset], row_to_usage)?;
    rows.collect()
}

pub fn insert(conn: &Connection, entry: &UsageEntry) -> rusqlite::Result<()> {
    let tokens_json = entry
        .tokens
        .as_ref()
        .map(|t| serde_json::to_string(t).unwrap_or_default());
    conn.execute(
        "INSERT INTO usageHistory(timestamp, provider, model, connectionId, apiKey, endpoint,
                promptTokens, completionTokens, cost, status, tokens, meta,
                bytesBefore, bytesAfter, bytesSaved, imagePrompts)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
        params![
            entry.timestamp.as_deref().unwrap_or(""),
            entry.provider.as_deref(),
            entry.model,
            entry.connection_id.as_deref(),
            entry.api_key.as_deref(),
            entry.endpoint.as_deref(),
            entry
                .tokens
                .as_ref()
                .and_then(|t| t.prompt_tokens.or(t.input_tokens))
                .unwrap_or(0) as i64,
            entry
                .tokens
                .as_ref()
                .and_then(|t| t.completion_tokens.or(t.output_tokens))
                .unwrap_or(0) as i64,
            entry.cost,
            entry.status.as_deref(),
            tokens_json,
            None::<String>,
            entry.bytes_before as i64,
            entry.bytes_after as i64,
            entry.bytes_saved as i64,
            entry.image_prompts as i64,
        ],
    )?;
    Ok(())
}

pub fn get_daily(conn: &Connection, date_key: &str) -> rusqlite::Result<Option<DailySummary>> {
    let mut stmt = conn.prepare("SELECT data FROM usageDaily WHERE dateKey = ?1")?;
    let mut rows = stmt.query_map(params![date_key], |row| {
        let s: String = row.get(0)?;
        serde_json::from_str(&s).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
    })?;
    rows.next().transpose()
}

pub fn upsert_daily(
    conn: &Connection,
    date_key: &str,
    summary: &DailySummary,
) -> rusqlite::Result<()> {
    let json_str = serde_json::to_string(summary).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "INSERT INTO usageDaily(dateKey, data) VALUES(?1,?2) ON CONFLICT(dateKey) DO UPDATE SET data = excluded.data",
        params![date_key, json_str],
    )?;
    Ok(())
}

/// Every durable day rollup, keyed by `dateKey`. `usageDaily` is retained for
/// 90 days while `usageHistory` is pruned at 30, so this is the only place the
/// days in between still exist.
pub fn all_daily(conn: &Connection) -> rusqlite::Result<BTreeMap<String, DailySummary>> {
    let mut stmt = conn.prepare("SELECT dateKey, data FROM usageDaily")?;
    let rows = stmt.query_map([], |row| {
        let date_key: String = row.get(0)?;
        let data: String = row.get(1)?;
        let day = serde_json::from_str(&data)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        Ok((date_key, day))
    })?;
    rows.collect()
}

/// The `usageDaily` bucket an entry belongs to. Mirrors the key
/// `UsageDb::normalize` derives when it folds history into days — the durable
/// rollup and the in-memory summary have to agree, or the two drift apart.
/// Unparseable or missing timestamps land in `"unknown"`, as they do there.
pub fn date_key_for(entry: &UsageEntry) -> String {
    entry
        .timestamp
        .as_deref()
        .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
        .map(|timestamp| timestamp.date_naive().to_string())
        .or_else(|| {
            entry
                .timestamp
                .as_ref()
                .map(|timestamp| timestamp.chars().take(10).collect())
        })
        .unwrap_or_else(|| "unknown".into())
}

fn row_to_usage(row: &rusqlite::Row<'_>) -> rusqlite::Result<UsageEntry> {
    let timestamp: Option<String> = row.get(0)?;
    let provider: Option<String> = row.get(1)?;
    let model: String = row.get(2)?;
    let _conn_id: Option<String> = row.get(3)?;
    let _api_key: Option<String> = row.get(4)?;
    let _endpoint: Option<String> = row.get(5)?;
    let prompt_tokens: Option<i64> = row.get(6)?;
    let completion_tokens: Option<i64> = row.get(7)?;
    let cost: Option<f64> = row.get(8)?;
    let status: Option<String> = row.get(9)?;
    let tokens_str: Option<String> = row.get(10)?;
    let bytes_before: u64 = row.get::<_, i64>(12).unwrap_or(0) as u64;
    let bytes_after: u64 = row.get::<_, i64>(13).unwrap_or(0) as u64;
    let bytes_saved: u64 = row.get::<_, i64>(14).unwrap_or(0) as u64;
    let image_prompts: u64 = row.get::<_, i64>(15).unwrap_or(0) as u64;

    let tokens = tokens_str.and_then(|s| serde_json::from_str(&s).ok());

    Ok(UsageEntry {
        timestamp,
        provider,
        model,
        tokens,
        cost,
        status,
        bytes_before,
        bytes_after,
        bytes_saved,
        image_prompts,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqliteDb;
    use crate::types::UsageDb;
    use serde_json::json;

    #[test]
    fn roundtrip() {
        let db = SqliteDb::open_in_memory().unwrap();
        let entry = UsageEntry {
            model: "gpt-4o".into(),
            provider: Some("openai".into()),
            timestamp: Some("2026-01-01T00:00:00Z".into()),
            ..Default::default()
        };
        db.with_transaction(|tx| insert(tx, &entry)).unwrap();
        let history = db.with_conn(|c| get_history(c, 10, 0)).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].model, "gpt-4o");
    }

    #[test]
    fn daily_rollup_roundtrips() {
        let db = SqliteDb::open_in_memory().unwrap();
        let entry = UsageEntry {
            model: "gpt-4o".into(),
            provider: Some("openai".into()),
            timestamp: Some("2026-01-01T12:00:00Z".into()),
            cost: Some(0.25),
            ..Default::default()
        };
        assert_eq!(date_key_for(&entry), "2026-01-01");

        // Fold the entry the way `Db::update_usage` does, then persist.
        let mut folded = UsageDb {
            history: vec![entry],
            ..Default::default()
        };
        folded.normalize();
        db.with_transaction(|tx| {
            upsert_daily(tx, "2026-01-01", &folded.daily_summary["2026-01-01"])
        })
        .unwrap();

        let days = db.with_conn(|conn| all_daily(conn)).unwrap();
        assert_eq!(days["2026-01-01"].requests, 1);
        assert_eq!(days["2026-01-01"].cost, 0.25);
    }
}
