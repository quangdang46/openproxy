//! `openproxy key *` — extended API key lifecycle commands (M3).
//!
//! Complements the existing `key list` / `key add` (which now live as
//! variants on `KeyCmd` in `cli::mod`). These commands implement the
//! full CRUD + rotate + idempotent apply.

use std::collections::HashSet;

use clap::Subcommand;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::cli::apply::{into_items, load_document, ApplyDiff};
use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};
use crate::db::Db;
use crate::types::ApiKey;

#[derive(Debug, Clone, Subcommand)]
pub enum KeyExtCmd {
    /// Show one key by id or name.
    Get { id_or_name: String },
    /// Generate a fresh secret for an existing key. Returns the new key.
    Rotate { id_or_name: String },
    /// Delete a key.
    Delete {
        id_or_name: String,
        #[arg(long)]
        strict: bool,
    },
    /// Mark a key active.
    Enable { id_or_name: String },
    /// Mark a key inactive (kept in DB, ignored by auth).
    Disable { id_or_name: String },
    /// Idempotent upsert from a YAML/JSON file or stdin.
    Apply {
        #[arg(long = "from-file", default_value = "-")]
        from_file: String,
        #[arg(long)]
        prune: bool,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyInput {
    name: String,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    is_active: Option<bool>,
}

pub async fn run(cmd: KeyExtCmd, db: &Db, ctx: OutputCtx) -> anyhow::Result<()> {
    match cmd {
        KeyExtCmd::Get { id_or_name } => run_get(db, ctx, &id_or_name).await,
        KeyExtCmd::Rotate { id_or_name } => run_rotate(db, ctx, &id_or_name).await,
        KeyExtCmd::Delete { id_or_name, strict } => run_delete(db, ctx, &id_or_name, strict).await,
        KeyExtCmd::Enable { id_or_name } => run_set_active(db, ctx, &id_or_name, true).await,
        KeyExtCmd::Disable { id_or_name } => run_set_active(db, ctx, &id_or_name, false).await,
        KeyExtCmd::Apply { from_file, prune } => run_apply(db, ctx, &from_file, prune).await,
    }
}

fn find_key(db: &Db, id_or_name: &str) -> Option<ApiKey> {
    db.snapshot()
        .api_keys
        .iter()
        .find(|k| k.id == id_or_name || k.name == id_or_name)
        .cloned()
}

fn mask(secret: &str) -> String {
    crate::cli::output::mask_secret(secret)
}

async fn run_get(db: &Db, ctx: OutputCtx, id_or_name: &str) -> anyhow::Result<()> {
    let Some(key) = find_key(db, id_or_name) else {
        let exit = emit_error(ctx, "not_found", &format!("key '{id_or_name}' not found"))?;
        std::process::exit(exit);
    };
    let payload = json!({
        "id": key.id,
        "name": key.name,
        "keyMasked": mask(&key.key),
        "isActive": key.is_active(),
        "createdAt": key.created_at,
    });
    if ctx.is_robot() {
        emit_robot("openproxy.v1.key.get", payload)?;
    } else {
        humanln(ctx, format!("Key: {} ({})", key.name, key.id));
        humanln(ctx, format!("  key:    {}", mask(&key.key)));
        humanln(ctx, format!("  active: {}", key.is_active()));
    }
    Ok(())
}

async fn run_rotate(db: &Db, ctx: OutputCtx, id_or_name: &str) -> anyhow::Result<()> {
    if find_key(db, id_or_name).is_none() {
        let exit = emit_error(ctx, "not_found", &format!("key '{id_or_name}' not found"))?;
        std::process::exit(exit);
    }
    let machine_id = uuid::Uuid::new_v4().simple().to_string();
    let new_secret = crate::core::auth::generate_api_key_with_machine(&machine_id);
    let new_secret_clone = new_secret.clone();
    db.update(|app| {
        if let Some(key) = app
            .api_keys
            .iter_mut()
            .find(|k| k.id == id_or_name || k.name == id_or_name)
        {
            key.key = new_secret_clone.clone();
            key.machine_id = Some(machine_id.clone());
        }
    })
    .await?;

    let payload = json!({
        "name": id_or_name,
        "newKey": new_secret,
        "newKeyMasked": mask(&new_secret),
    });
    if ctx.is_robot() {
        emit_robot("openproxy.v1.key.rotate", payload)?;
    } else {
        humanln(ctx, format!("rotated key '{id_or_name}'"));
        humanln(ctx, format!("  new key: {new_secret}"));
    }
    Ok(())
}

async fn run_delete(db: &Db, ctx: OutputCtx, id_or_name: &str, strict: bool) -> anyhow::Result<()> {
    if find_key(db, id_or_name).is_none() {
        if strict {
            let exit = emit_error(ctx, "not_found", &format!("key '{id_or_name}' not found"))?;
            std::process::exit(exit);
        }
        if ctx.is_robot() {
            emit_robot(
                "openproxy.v1.key.delete",
                json!({ "key": id_or_name, "deleted": false }),
            )?;
        } else {
            humanln(ctx, format!("key '{id_or_name}' not found (no-op)"));
        }
        return Ok(());
    }
    db.update(|app| {
        app.api_keys
            .retain(|k| k.id != id_or_name && k.name != id_or_name);
    })
    .await?;
    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.key.delete",
            json!({ "key": id_or_name, "deleted": true }),
        )?;
    } else {
        humanln(ctx, format!("deleted key '{id_or_name}'"));
    }
    Ok(())
}

async fn run_set_active(
    db: &Db,
    ctx: OutputCtx,
    id_or_name: &str,
    active: bool,
) -> anyhow::Result<()> {
    if find_key(db, id_or_name).is_none() {
        let exit = emit_error(ctx, "not_found", &format!("key '{id_or_name}' not found"))?;
        std::process::exit(exit);
    }
    db.update(|app| {
        if let Some(key) = app
            .api_keys
            .iter_mut()
            .find(|k| k.id == id_or_name || k.name == id_or_name)
        {
            key.is_active = Some(active);
        }
    })
    .await?;
    let schema = if active {
        "openproxy.v1.key.enable"
    } else {
        "openproxy.v1.key.disable"
    };
    if ctx.is_robot() {
        emit_robot(schema, json!({ "key": id_or_name, "active": active }))?;
    } else {
        humanln(
            ctx,
            format!(
                "{} key '{id_or_name}'",
                if active { "enabled" } else { "disabled" }
            ),
        );
    }
    Ok(())
}

async fn run_apply(db: &Db, ctx: OutputCtx, from_file: &str, prune: bool) -> anyhow::Result<()> {
    let (doc, _) = match load_document(from_file) {
        Ok(d) => d,
        Err(e) => {
            let exit = emit_error(ctx, "validation", &e.to_string())?;
            std::process::exit(exit);
        }
    };
    let items: Vec<KeyInput> = match into_items(doc) {
        Ok(items) => items,
        Err(e) => {
            let exit = emit_error(ctx, "validation", &e.to_string())?;
            std::process::exit(exit);
        }
    };

    let names_in_doc: HashSet<String> = items.iter().map(|i| i.name.clone()).collect();
    let mut diff = ApplyDiff::default();
    let now = chrono::Utc::now().to_rfc3339();

    db.update(|app| {
        for item in &items {
            if let Some(existing) = app.api_keys.iter_mut().find(|k| k.name == item.name) {
                let mut changed = false;
                if let Some(active) = item.is_active {
                    if existing.is_active() != active {
                        existing.is_active = Some(active);
                        changed = true;
                    }
                }
                if let Some(key) = item.key.as_deref().filter(|k| !k.is_empty()) {
                    if existing.key != key {
                        existing.key = key.to_string();
                        changed = true;
                    }
                }
                if changed {
                    diff.updated.push(item.name.clone());
                } else {
                    diff.unchanged.push(item.name.clone());
                }
            } else {
                let machine_id = uuid::Uuid::new_v4().simple().to_string();
                let key_secret = item
                    .key
                    .clone()
                    .filter(|k| !k.is_empty())
                    .unwrap_or_else(|| {
                        crate::core::auth::generate_api_key_with_machine(&machine_id)
                    });
                app.api_keys.push(ApiKey {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: item.name.clone(),
                    key: key_secret,
                    machine_id: Some(machine_id),
                    is_active: Some(item.is_active.unwrap_or(true)),
                    created_at: Some(now.clone()),
                    extra: std::collections::BTreeMap::new(),
                });
                diff.created.push(item.name.clone());
            }
        }
        if prune {
            let to_delete: Vec<String> = app
                .api_keys
                .iter()
                .filter(|k| !names_in_doc.contains(&k.name))
                .map(|k| k.name.clone())
                .collect();
            for name in &to_delete {
                diff.deleted.push(name.clone());
            }
            app.api_keys.retain(|k| names_in_doc.contains(&k.name));
        }
    })
    .await?;

    let summary = diff.summary();
    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.key.apply",
            json!({ "diff": diff, "summary": summary, "prune": prune }),
        )?;
    } else {
        humanln(ctx, format!("key apply: {summary}"));
    }
    let _ = Value::Null;
    Ok(())
}
