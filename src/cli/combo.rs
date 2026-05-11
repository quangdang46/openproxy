//! `openproxy combo *` — fallback chains and round-robin combos.
//!
//! Combos are one of OpenProxy's two core concepts: a named list of models
//! the router walks through on failure (or rotates across, depending on
//! strategy). They are stored in `db.json` as the `combos` Vec.

use std::collections::BTreeMap;
use std::collections::HashSet;

use clap::Subcommand;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::cli::apply::{into_items, load_document, ApplyDiff};
use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};
use crate::db::Db;
use crate::types::Combo;

#[derive(Debug, Clone, Subcommand)]
pub enum ComboCmd {
    /// List all combos.
    List,
    /// Show one combo by name.
    Get { name: String },
    /// Create a new combo.
    Create {
        /// Combo name (letters, digits, `_`, `-`, `.`).
        #[arg(long)]
        name: String,
        /// Models in priority order, comma-separated (e.g. `openai/gpt-4o,anthropic/claude-3-5-sonnet`).
        #[arg(long, value_delimiter = ',')]
        models: Vec<String>,
        /// Strategy: `fallback` (default), `round-robin`, or `sticky-round-robin`.
        #[arg(long, default_value = "fallback")]
        strategy: String,
    },
    /// Edit an existing combo. Any flag omitted is left unchanged.
    Edit {
        name: String,
        #[arg(long, value_delimiter = ',')]
        models: Option<Vec<String>>,
        #[arg(long)]
        strategy: Option<String>,
    },
    /// Delete a combo. Exit 0 if it does not exist (use `--strict` to fail).
    Delete {
        name: String,
        #[arg(long)]
        strict: bool,
    },
    /// Mark combo active (sets `isActive=true` in extras).
    Enable { name: String },
    /// Mark combo inactive.
    Disable { name: String },
    /// Dry-run a combo's expansion to verify membership and reachability.
    Test {
        name: String,
        /// Optional prompt; ignored unless `--live`.
        #[arg(long)]
        prompt: Option<String>,
    },
    /// Idempotent upsert from a YAML/JSON file or stdin (`-`).
    Apply {
        /// Path or `-` for stdin.
        #[arg(long = "from-file", default_value = "-")]
        from_file: String,
        /// Delete combos that exist in DB but are missing from the input.
        #[arg(long)]
        prune: bool,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComboInput {
    name: String,
    #[serde(default)]
    models: Vec<String>,
    #[serde(default)]
    strategy: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    is_active: Option<bool>,
}

pub async fn run(cmd: ComboCmd, db: &Db, ctx: OutputCtx) -> anyhow::Result<()> {
    match cmd {
        ComboCmd::List => run_list(db, ctx).await,
        ComboCmd::Get { name } => run_get(db, ctx, &name).await,
        ComboCmd::Create {
            name,
            models,
            strategy,
        } => run_create(db, ctx, name, models, strategy).await,
        ComboCmd::Edit {
            name,
            models,
            strategy,
        } => run_edit(db, ctx, &name, models, strategy).await,
        ComboCmd::Delete { name, strict } => run_delete(db, ctx, &name, strict).await,
        ComboCmd::Enable { name } => run_set_active(db, ctx, &name, true).await,
        ComboCmd::Disable { name } => run_set_active(db, ctx, &name, false).await,
        ComboCmd::Test { name, prompt } => run_test(db, ctx, &name, prompt).await,
        ComboCmd::Apply { from_file, prune } => run_apply(db, ctx, &from_file, prune).await,
    }
}

async fn run_list(db: &Db, ctx: OutputCtx) -> anyhow::Result<()> {
    let snapshot = db.snapshot();
    let combos = snapshot.combos.clone();
    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.combo.list",
            json!({ "combos": combos, "count": combos.len() }),
        )?;
    } else {
        humanln(ctx, format!("Combos ({}):", combos.len()));
        for combo in &combos {
            humanln(
                ctx,
                format!(
                    "  {}  [{}]  {} model(s)",
                    combo.name,
                    combo.kind.as_deref().unwrap_or("fallback"),
                    combo.models.len()
                ),
            );
        }
    }
    Ok(())
}

async fn run_get(db: &Db, ctx: OutputCtx, name: &str) -> anyhow::Result<()> {
    let Some(combo) = db.combo_by_name(name) else {
        let exit = emit_error(ctx, "not_found", &format!("combo '{name}' not found"))?;
        std::process::exit(exit);
    };
    if ctx.is_robot() {
        emit_robot("openproxy.v1.combo.get", serde_json::to_value(&combo)?)?;
    } else {
        humanln(ctx, format!("Combo: {}", combo.name));
        humanln(
            ctx,
            format!("  kind: {}", combo.kind.as_deref().unwrap_or("fallback")),
        );
        humanln(ctx, format!("  models ({}):", combo.models.len()));
        for m in &combo.models {
            humanln(ctx, format!("    - {m}"));
        }
    }
    Ok(())
}

async fn run_create(
    db: &Db,
    ctx: OutputCtx,
    name: String,
    models: Vec<String>,
    strategy: String,
) -> anyhow::Result<()> {
    if !is_valid_combo_name(&name) {
        let exit = emit_error(
            ctx,
            "validation",
            &format!("invalid combo name '{name}'. Use letters, digits, '_', '-', or '.'."),
        )?;
        std::process::exit(exit);
    }
    if models.is_empty() {
        let exit = emit_error(ctx, "validation", "--models requires at least one model id")?;
        std::process::exit(exit);
    }
    if db.combo_by_name(&name).is_some() {
        let exit = emit_error(
            ctx,
            "conflict",
            &format!("combo '{name}' already exists. Use `combo edit` or `combo apply`."),
        )?;
        std::process::exit(exit);
    }

    let now = chrono::Utc::now().to_rfc3339();
    let combo = Combo {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.clone(),
        models,
        kind: normalize_strategy(&strategy),
        created_at: Some(now.clone()),
        updated_at: Some(now),
        extra: BTreeMap::new(),
    };

    db.update(|db| db.combos.push(combo.clone())).await?;

    if ctx.is_robot() {
        emit_robot("openproxy.v1.combo.create", serde_json::to_value(&combo)?)?;
    } else {
        humanln(ctx, format!("created combo '{}'", combo.name));
    }
    Ok(())
}

async fn run_edit(
    db: &Db,
    ctx: OutputCtx,
    name: &str,
    models: Option<Vec<String>>,
    strategy: Option<String>,
) -> anyhow::Result<()> {
    if db.combo_by_name(name).is_none() {
        let exit = emit_error(ctx, "not_found", &format!("combo '{name}' not found"))?;
        std::process::exit(exit);
    }
    let mut updated: Option<Combo> = None;
    db.update(|db| {
        if let Some(combo) = db.combos.iter_mut().find(|c| c.name == name) {
            if let Some(m) = &models {
                combo.models = m.clone();
            }
            if let Some(s) = &strategy {
                combo.kind = normalize_strategy(s);
            }
            combo.updated_at = Some(chrono::Utc::now().to_rfc3339());
            updated = Some(combo.clone());
        }
    })
    .await?;

    let combo = updated.expect("combo existed before update");
    if ctx.is_robot() {
        emit_robot("openproxy.v1.combo.edit", serde_json::to_value(&combo)?)?;
    } else {
        humanln(ctx, format!("updated combo '{}'", combo.name));
    }
    Ok(())
}

async fn run_delete(db: &Db, ctx: OutputCtx, name: &str, strict: bool) -> anyhow::Result<()> {
    if db.combo_by_name(name).is_none() {
        if strict {
            let exit = emit_error(ctx, "not_found", &format!("combo '{name}' not found"))?;
            std::process::exit(exit);
        }
        if ctx.is_robot() {
            emit_robot(
                "openproxy.v1.combo.delete",
                json!({ "name": name, "deleted": false }),
            )?;
        } else {
            humanln(ctx, format!("combo '{name}' not found (no-op)"));
        }
        return Ok(());
    }

    db.update(|db| db.combos.retain(|c| c.name != name)).await?;

    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.combo.delete",
            json!({ "name": name, "deleted": true }),
        )?;
    } else {
        humanln(ctx, format!("deleted combo '{name}'"));
    }
    Ok(())
}

async fn run_set_active(db: &Db, ctx: OutputCtx, name: &str, active: bool) -> anyhow::Result<()> {
    if db.combo_by_name(name).is_none() {
        let exit = emit_error(ctx, "not_found", &format!("combo '{name}' not found"))?;
        std::process::exit(exit);
    }
    let mut updated: Option<Combo> = None;
    db.update(|db| {
        if let Some(combo) = db.combos.iter_mut().find(|c| c.name == name) {
            combo.extra.insert("isActive".into(), Value::Bool(active));
            combo.updated_at = Some(chrono::Utc::now().to_rfc3339());
            updated = Some(combo.clone());
        }
    })
    .await?;

    let combo = updated.expect("combo existed");
    let schema = if active {
        "openproxy.v1.combo.enable"
    } else {
        "openproxy.v1.combo.disable"
    };
    if ctx.is_robot() {
        emit_robot(schema, serde_json::to_value(&combo)?)?;
    } else {
        humanln(
            ctx,
            format!(
                "{} combo '{}'",
                if active { "enabled" } else { "disabled" },
                combo.name
            ),
        );
    }
    Ok(())
}

async fn run_test(
    db: &Db,
    ctx: OutputCtx,
    name: &str,
    _prompt: Option<String>,
) -> anyhow::Result<()> {
    let Some(combo) = db.combo_by_name(name) else {
        let exit = emit_error(ctx, "not_found", &format!("combo '{name}' not found"))?;
        std::process::exit(exit);
    };

    let snapshot = db.snapshot();
    let known_providers: HashSet<String> = snapshot
        .provider_connections
        .iter()
        .map(|c| c.provider.clone())
        .collect();
    let known_node_ids: HashSet<String> = snapshot
        .provider_nodes
        .iter()
        .map(|n| n.id.clone())
        .collect();

    let mut members = Vec::with_capacity(combo.models.len());
    for model in &combo.models {
        let provider_part = model.split('/').next().unwrap_or("");
        let resolved = !provider_part.is_empty()
            && (known_providers.contains(provider_part) || known_node_ids.contains(provider_part));
        members.push(json!({
            "model": model,
            "provider": provider_part,
            "resolved": resolved,
        }));
    }

    let payload = json!({
        "name": combo.name,
        "kind": combo.kind.clone().unwrap_or_else(|| "fallback".into()),
        "members": members,
        "reachable": members.iter().all(|m| m.get("resolved") == Some(&Value::Bool(true))),
    });

    if ctx.is_robot() {
        emit_robot("openproxy.v1.combo.test", payload)?;
    } else {
        humanln(ctx, format!("combo '{}' resolution:", combo.name));
        for member in &members {
            let resolved = member.get("resolved") == Some(&Value::Bool(true));
            humanln(
                ctx,
                format!(
                    "  {} {} ({})",
                    if resolved { "OK " } else { "MISS" },
                    member.get("model").and_then(Value::as_str).unwrap_or(""),
                    member.get("provider").and_then(Value::as_str).unwrap_or(""),
                ),
            );
        }
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
    let items: Vec<ComboInput> = match into_items(doc) {
        Ok(items) => items,
        Err(e) => {
            let exit = emit_error(ctx, "validation", &e.to_string())?;
            std::process::exit(exit);
        }
    };

    for item in &items {
        if !is_valid_combo_name(&item.name) {
            let exit = emit_error(
                ctx,
                "validation",
                &format!("invalid combo name '{}'", item.name),
            )?;
            std::process::exit(exit);
        }
    }

    let mut diff = ApplyDiff::default();
    let names_in_doc: HashSet<String> = items.iter().map(|i| i.name.clone()).collect();
    let now = chrono::Utc::now().to_rfc3339();

    db.update(|app| {
        for item in &items {
            let target_kind = item
                .kind
                .clone()
                .or_else(|| item.strategy.as_ref().and_then(|s| normalize_strategy(s)));
            if let Some(existing) = app.combos.iter_mut().find(|c| c.name == item.name) {
                let mut changed = false;
                if existing.models != item.models {
                    existing.models = item.models.clone();
                    changed = true;
                }
                if existing.kind != target_kind {
                    existing.kind = target_kind.clone();
                    changed = true;
                }
                if let Some(active) = item.is_active {
                    let prev = existing.extra.get("isActive").and_then(Value::as_bool);
                    if prev != Some(active) {
                        existing
                            .extra
                            .insert("isActive".into(), Value::Bool(active));
                        changed = true;
                    }
                }
                if changed {
                    existing.updated_at = Some(now.clone());
                    diff.updated.push(item.name.clone());
                } else {
                    diff.unchanged.push(item.name.clone());
                }
            } else {
                let mut extra = BTreeMap::new();
                if let Some(active) = item.is_active {
                    extra.insert("isActive".into(), Value::Bool(active));
                }
                app.combos.push(Combo {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: item.name.clone(),
                    models: item.models.clone(),
                    kind: target_kind,
                    created_at: Some(now.clone()),
                    updated_at: Some(now.clone()),
                    extra,
                });
                diff.created.push(item.name.clone());
            }
        }
        if prune {
            let to_remove: Vec<String> = app
                .combos
                .iter()
                .filter(|c| !names_in_doc.contains(&c.name))
                .map(|c| c.name.clone())
                .collect();
            for name in &to_remove {
                diff.deleted.push(name.clone());
            }
            app.combos.retain(|c| names_in_doc.contains(&c.name));
        }
    })
    .await?;

    let summary = diff.summary();
    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.combo.apply",
            json!({
                "diff": diff,
                "summary": summary,
                "prune": prune,
            }),
        )?;
    } else {
        humanln(ctx, format!("combo apply: {summary}"));
    }
    Ok(())
}

fn is_valid_combo_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

fn normalize_strategy(s: &str) -> Option<String> {
    let trimmed = s.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return None;
    }
    let canonical = match trimmed.as_str() {
        "fallback" | "fallback-chain" => "fallback",
        "rr" | "round-robin" | "round_robin" | "roundrobin" => "round-robin",
        "sticky-rr" | "sticky-round-robin" | "sticky_round_robin" => "sticky-round-robin",
        other => return Some(other.to_string()),
    };
    Some(canonical.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_combo_name() {
        assert!(is_valid_combo_name("combo-1"));
        assert!(is_valid_combo_name("combo_1.v2"));
        assert!(!is_valid_combo_name(""));
        assert!(!is_valid_combo_name("combo name"));
        assert!(!is_valid_combo_name("café"));
    }

    #[test]
    fn normalizes_strategy_aliases() {
        assert_eq!(normalize_strategy("FALLBACK").as_deref(), Some("fallback"));
        assert_eq!(normalize_strategy("rr").as_deref(), Some("round-robin"));
        assert_eq!(
            normalize_strategy("sticky-rr").as_deref(),
            Some("sticky-round-robin")
        );
        assert_eq!(normalize_strategy("").as_deref(), None);
        // Unknown values pass through (lowercased).
        assert_eq!(normalize_strategy("custom").as_deref(), Some("custom"));
    }
}
