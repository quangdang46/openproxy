//! Simulation persistence (bead sim-02).
//!
//! Storage design (no new tables — plan §3.4, adapted to the real schema):
//!
//! - Per-provider **configured** mode lives in the generic `kv` table under
//!   scope `"simulationMode"`, key = provider name, value = `"real" | "mock"`.
//!   Read: unknown key → `Real` (backward compatible). Write: validated.
//! - Global force-all lives on [`crate::types::Settings`] as
//!   `dev_mock_all: bool`, persisted in the existing single-row `settings`
//!   table — no schema change, flows through `Db::update_settings`.
//! - Env override `OPENPROXY_DEV_MOCK=1` is read at resolve time (bead sim-03),
//!   never persisted.
//!
//! The server reads modes from its in-memory `AppDb` snapshot; the CLI writes
//! through the same `Db` layer, and the server picks changes up via the
//! existing `reload_snapshot()` path (same mechanism as CLI combo writes).

use rusqlite::Connection;
use serde_json::Value;

use crate::core::simulation::ProviderExecutionMode;

/// `kv` scope holding per-provider configured simulation modes.
pub const SIM_MODE_SCOPE: &str = "simulationMode";

/// Read the configured mode for one provider. Unknown/missing/invalid → Real.
pub fn get_provider_mode(conn: &Connection, provider: &str) -> ProviderExecutionMode {
    let raw: Option<Value> =
        crate::db::sqlite::repo::kv_repo::get(conn, SIM_MODE_SCOPE, provider).unwrap_or_default();
    match raw.as_ref().and_then(Value::as_str) {
        Some("mock") => ProviderExecutionMode::Mock,
        _ => ProviderExecutionMode::Real,
    }
}

/// Persist the configured mode for one provider. Only `"real"`/`"mock"` are
/// valid — anything else is a caller bug and is rejected loudly.
pub fn set_provider_mode(
    conn: &Connection,
    provider: &str,
    mode: ProviderExecutionMode,
) -> rusqlite::Result<()> {
    let provider = provider.trim();
    if provider.is_empty() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let value = Value::String(mode.to_string());
    crate::db::sqlite::repo::kv_repo::set(conn, SIM_MODE_SCOPE, provider, &value)
}

/// Remove a provider's mode override (falls back to default Real).
pub fn clear_provider_mode(conn: &Connection, provider: &str) -> rusqlite::Result<()> {
    crate::db::sqlite::repo::kv_repo::delete(conn, SIM_MODE_SCOPE, provider)
}

/// All provider mode overrides in one map (for `/api/mock/status`, bead 19).
pub fn all_provider_modes(
    conn: &Connection,
) -> rusqlite::Result<std::collections::HashMap<String, ProviderExecutionMode>> {
    let raw = crate::db::sqlite::repo::kv_repo::get_all(conn, SIM_MODE_SCOPE)?;
    Ok(raw
        .into_iter()
        .map(|(k, v)| {
            let mode = match v.as_str() {
                Some("mock") => ProviderExecutionMode::Mock,
                _ => ProviderExecutionMode::Real,
            };
            (k, mode)
        })
        .collect())
}

/// `OPENPROXY_DEV_MOCK` env force — the safety boundary (plan §3.2).
/// Truthy values: `1`, `true`, `yes` (case-insensitive). Never persisted.
pub fn env_force_all() -> bool {
    match std::env::var("OPENPROXY_DEV_MOCK") {
        Ok(v) => matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqliteDb;

    #[test]
    fn unknown_provider_defaults_real() {
        let db = SqliteDb::open_in_memory().unwrap();
        let mode = db
            .with_conn(|c| Ok(get_provider_mode(c, "openai")))
            .unwrap();
        assert_eq!(mode, ProviderExecutionMode::Real);
    }

    #[test]
    fn set_get_roundtrip() {
        let db = SqliteDb::open_in_memory().unwrap();
        db.with_transaction(|tx| set_provider_mode(tx, "openai", ProviderExecutionMode::Mock))
            .unwrap();
        let mode = db
            .with_conn(|c| Ok(get_provider_mode(c, "openai")))
            .unwrap();
        assert_eq!(mode, ProviderExecutionMode::Mock);
        // Other providers unaffected.
        let other = db
            .with_conn(|c| Ok(get_provider_mode(c, "anthropic")))
            .unwrap();
        assert_eq!(other, ProviderExecutionMode::Real);
    }

    #[test]
    fn clear_falls_back_to_real() {
        let db = SqliteDb::open_in_memory().unwrap();
        db.with_transaction(|tx| set_provider_mode(tx, "openai", ProviderExecutionMode::Mock))
            .unwrap();
        db.with_transaction(|tx| clear_provider_mode(tx, "openai"))
            .unwrap();
        let mode = db
            .with_conn(|c| Ok(get_provider_mode(c, "openai")))
            .unwrap();
        assert_eq!(mode, ProviderExecutionMode::Real);
    }

    #[test]
    fn invalid_stored_value_defaults_real() {
        let db = SqliteDb::open_in_memory().unwrap();
        db.with_transaction(|tx| {
            crate::db::sqlite::repo::kv_repo::set(
                tx,
                SIM_MODE_SCOPE,
                "openai",
                &Value::String("bogus".into()),
            )
        })
        .unwrap();
        let mode = db
            .with_conn(|c| Ok(get_provider_mode(c, "openai")))
            .unwrap();
        assert_eq!(mode, ProviderExecutionMode::Real);
    }

    #[test]
    fn set_overwrites() {
        let db = SqliteDb::open_in_memory().unwrap();
        db.with_transaction(|tx| set_provider_mode(tx, "x", ProviderExecutionMode::Mock))
            .unwrap();
        db.with_transaction(|tx| set_provider_mode(tx, "x", ProviderExecutionMode::Real))
            .unwrap();
        let mode = db.with_conn(|c| Ok(get_provider_mode(c, "x"))).unwrap();
        assert_eq!(mode, ProviderExecutionMode::Real);
    }

    #[test]
    fn env_force_parsing() {
        // Save/restore to avoid leaking env into other tests (serial by mutex
        // in practice; values chosen to not collide with CI).
        let prev = std::env::var("OPENPROXY_DEV_MOCK").ok();
        for (val, expected) in [
            ("1", true),
            ("true", true),
            ("YES", true),
            ("0", false),
            ("", false),
        ] {
            std::env::set_var("OPENPROXY_DEV_MOCK", val);
            assert_eq!(env_force_all(), expected, "value {val:?}");
        }
        match prev {
            Some(v) => std::env::set_var("OPENPROXY_DEV_MOCK", v),
            None => std::env::remove_var("OPENPROXY_DEV_MOCK"),
        }
    }
}
