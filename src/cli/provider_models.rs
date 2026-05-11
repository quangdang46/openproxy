//! `openproxy provider models *` — model registry per provider.
//!
//! Wraps the in-DB model alias map (`modelAliases`), custom models
//! (`customModels`), and disabled-model list (in `extra.disabledModels`).
//! All writes go through `Db::update`; the server file watcher picks up
//! the change.

use std::collections::{BTreeMap, BTreeSet};

use clap::Subcommand;
use serde_json::{json, Value};

use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};
use crate::db::Db;
use crate::types::{CustomModel, ModelAliasTarget, ProviderConnection, ProviderModelRef};

#[derive(Debug, Clone, Subcommand)]
pub enum ModelsCmd {
    /// List models registered against a provider (built-in + custom + aliases).
    List { provider: String },
    /// Run a real `/v1/models` probe against the provider connection.
    Test {
        provider: String,
        /// Specific model to ping (defaults to first available).
        #[arg(long)]
        model: Option<String>,
    },
    /// Alias management subgroup.
    #[command(subcommand)]
    Alias(AliasCmd),
    /// Disable a model for a provider (hidden from `/v1/models`).
    Disable {
        provider: String,
        #[arg(long)]
        model: String,
    },
    /// Re-enable a previously disabled model.
    Enable {
        provider: String,
        #[arg(long)]
        model: String,
    },
    /// Custom model management subgroup.
    #[command(subcommand)]
    Custom(CustomCmd),
}

#[derive(Debug, Clone, Subcommand)]
pub enum AliasCmd {
    /// List all aliases.
    List,
    /// Set an alias mapping `<alias> -> <provider>/<model>`.
    Set {
        provider: String,
        model: String,
        alias: String,
    },
    /// Remove an alias by name.
    Unset { alias: String },
}

#[derive(Debug, Clone, Subcommand)]
pub enum CustomCmd {
    /// Register a custom model under a provider alias.
    Add {
        provider: String,
        model: String,
        /// Model kind, e.g. `chat`, `embedding`, `image`.
        #[arg(long, default_value = "chat")]
        r#type: String,
        /// Optional human-readable name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Remove a custom model.
    Remove { provider: String, model: String },
    /// List custom models.
    List,
}

pub async fn run(cmd: ModelsCmd, db: &Db, ctx: OutputCtx) -> anyhow::Result<()> {
    match cmd {
        ModelsCmd::List { provider } => run_list(db, ctx, &provider).await,
        ModelsCmd::Test { provider, model } => run_test(db, ctx, &provider, model).await,
        ModelsCmd::Alias(cmd) => run_alias(db, ctx, cmd).await,
        ModelsCmd::Disable { provider, model } => {
            run_set_disabled(db, ctx, &provider, &model, true).await
        }
        ModelsCmd::Enable { provider, model } => {
            run_set_disabled(db, ctx, &provider, &model, false).await
        }
        ModelsCmd::Custom(cmd) => run_custom(db, ctx, cmd).await,
    }
}

async fn run_list(db: &Db, ctx: OutputCtx, provider: &str) -> anyhow::Result<()> {
    let snapshot = db.snapshot();
    let custom: Vec<&CustomModel> = snapshot
        .custom_models
        .iter()
        .filter(|m| m.provider_alias == provider)
        .collect();
    let disabled = disabled_models_for(&snapshot.extra, provider);
    let aliases: BTreeMap<String, ModelAliasTarget> = snapshot
        .model_aliases
        .iter()
        .filter(
            |(_, target)| matches!(target, ModelAliasTarget::Mapping(r) if r.provider == provider),
        )
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let payload = json!({
        "provider": provider,
        "custom": custom,
        "disabled": disabled,
        "aliases": aliases,
    });

    if ctx.is_robot() {
        emit_robot("openproxy.v1.provider-models.list", payload)?;
    } else {
        humanln(ctx, format!("Provider models — {provider}"));
        humanln(ctx, format!("  custom ({}):", custom.len()));
        for m in &custom {
            humanln(
                ctx,
                format!(
                    "    {} ({}) {}",
                    m.id,
                    m.r#type,
                    m.name.as_deref().unwrap_or("")
                ),
            );
        }
        humanln(ctx, format!("  disabled ({}):", disabled.len()));
        for id in &disabled {
            humanln(ctx, format!("    {id}"));
        }
        humanln(ctx, format!("  aliases ({}):", aliases.len()));
        for (alias, target) in &aliases {
            if let ModelAliasTarget::Mapping(r) = target {
                humanln(ctx, format!("    {alias} -> {}/{}", r.provider, r.model));
            }
        }
    }
    Ok(())
}

async fn run_test(
    db: &Db,
    ctx: OutputCtx,
    provider: &str,
    _model: Option<String>,
) -> anyhow::Result<()> {
    let snapshot = db.snapshot();
    let conn = snapshot
        .provider_connections
        .iter()
        .find(|c| c.provider == provider || c.name.as_deref() == Some(provider))
        .cloned();
    let Some(conn) = conn else {
        let exit = emit_error(
            ctx,
            "not_found",
            &format!("no provider connection for '{provider}'"),
        )?;
        std::process::exit(exit);
    };

    let base_url = conn
        .provider_specific_data
        .get("baseUrl")
        .and_then(Value::as_str)
        .map(String::from);
    let api_key = conn.api_key.as_deref();
    let provider_alias = conn.provider.as_str();

    let start = std::time::Instant::now();
    let (valid, error, latency_ms) = crate::server::api::providers::test_provider_api(
        provider_alias,
        api_key,
        base_url.as_deref(),
    )
    .await;
    let measured_ms = start.elapsed().as_millis() as u64;
    let latency_ms = latency_ms.unwrap_or(measured_ms);

    let payload = json!({
        "provider": provider_alias,
        "connectionId": conn.id,
        "valid": valid,
        "latencyMs": latency_ms,
        "error": error,
    });

    if ctx.is_robot() {
        emit_robot("openproxy.v1.provider-models.test", payload)?;
    } else if valid {
        humanln(ctx, format!("OK   {provider_alias} ({latency_ms}ms)"));
    } else {
        humanln(
            ctx,
            format!(
                "FAIL {provider_alias} ({latency_ms}ms) — {}",
                error.as_deref().unwrap_or("unknown")
            ),
        );
    }

    let _ = ProviderConnection::default();
    Ok(())
}

async fn run_alias(db: &Db, ctx: OutputCtx, cmd: AliasCmd) -> anyhow::Result<()> {
    match cmd {
        AliasCmd::List => {
            let snapshot = db.snapshot();
            let aliases = snapshot.model_aliases.clone();
            if ctx.is_robot() {
                emit_robot(
                    "openproxy.v1.provider-models.alias.list",
                    json!({ "aliases": aliases, "count": aliases.len() }),
                )?;
            } else {
                humanln(ctx, format!("Model aliases ({}):", aliases.len()));
                for (alias, target) in &aliases {
                    let target = match target {
                        ModelAliasTarget::Path(s) => s.clone(),
                        ModelAliasTarget::Mapping(r) => format!("{}/{}", r.provider, r.model),
                    };
                    humanln(ctx, format!("  {alias} -> {target}"));
                }
            }
            Ok(())
        }
        AliasCmd::Set {
            provider,
            model,
            alias,
        } => {
            if alias.trim().is_empty() {
                let exit = emit_error(ctx, "validation", "alias cannot be empty")?;
                std::process::exit(exit);
            }
            let target = ModelAliasTarget::Mapping(ProviderModelRef {
                provider: provider.clone(),
                model: model.clone(),
                extra: BTreeMap::new(),
            });
            db.update(|app| {
                app.model_aliases.insert(alias.clone(), target.clone());
            })
            .await?;

            let payload = json!({
                "alias": alias,
                "target": { "provider": provider, "model": model }
            });
            if ctx.is_robot() {
                emit_robot("openproxy.v1.provider-models.alias.set", payload)?;
            } else {
                humanln(ctx, format!("set alias {alias} -> {provider}/{model}"));
            }
            Ok(())
        }
        AliasCmd::Unset { alias } => {
            let existed = db.snapshot().model_aliases.contains_key(&alias);
            if !existed {
                if ctx.is_robot() {
                    emit_robot(
                        "openproxy.v1.provider-models.alias.unset",
                        json!({ "alias": alias, "removed": false }),
                    )?;
                } else {
                    humanln(ctx, format!("alias '{alias}' not found (no-op)"));
                }
                return Ok(());
            }
            db.update(|app| {
                app.model_aliases.remove(&alias);
            })
            .await?;
            if ctx.is_robot() {
                emit_robot(
                    "openproxy.v1.provider-models.alias.unset",
                    json!({ "alias": alias, "removed": true }),
                )?;
            } else {
                humanln(ctx, format!("removed alias '{alias}'"));
            }
            Ok(())
        }
    }
}

async fn run_set_disabled(
    db: &Db,
    ctx: OutputCtx,
    provider: &str,
    model: &str,
    disable: bool,
) -> anyhow::Result<()> {
    let mut result_disabled: Vec<String> = Vec::new();
    db.update(|app| {
        let mut map = disabled_map(&app.extra);
        let entry = map.entry(provider.to_string()).or_default();
        let mut set: BTreeSet<String> = entry.iter().cloned().collect();
        if disable {
            set.insert(model.to_string());
        } else {
            set.remove(model);
        }
        *entry = set.into_iter().collect();
        if entry.is_empty() {
            map.remove(provider);
        }
        result_disabled = map.get(provider).cloned().unwrap_or_default();
        write_disabled_map(&mut app.extra, &map);
    })
    .await?;

    let schema = if disable {
        "openproxy.v1.provider-models.disable"
    } else {
        "openproxy.v1.provider-models.enable"
    };
    if ctx.is_robot() {
        emit_robot(
            schema,
            json!({
                "provider": provider,
                "model": model,
                "disabled": result_disabled,
            }),
        )?;
    } else {
        humanln(
            ctx,
            format!(
                "{} {provider}/{model}",
                if disable { "disabled" } else { "enabled" }
            ),
        );
    }
    Ok(())
}

async fn run_custom(db: &Db, ctx: OutputCtx, cmd: CustomCmd) -> anyhow::Result<()> {
    match cmd {
        CustomCmd::List => {
            let snapshot = db.snapshot();
            let models = snapshot.custom_models.clone();
            if ctx.is_robot() {
                emit_robot(
                    "openproxy.v1.provider-models.custom.list",
                    json!({ "customModels": models, "count": models.len() }),
                )?;
            } else {
                humanln(ctx, format!("Custom models ({}):", models.len()));
                for m in &models {
                    humanln(
                        ctx,
                        format!(
                            "  {}/{}  type={}  {}",
                            m.provider_alias,
                            m.id,
                            m.r#type,
                            m.name.as_deref().unwrap_or(""),
                        ),
                    );
                }
            }
            Ok(())
        }
        CustomCmd::Add {
            provider,
            model,
            r#type,
            name,
        } => {
            let snapshot = db.snapshot();
            if snapshot
                .custom_models
                .iter()
                .any(|m| m.provider_alias == provider && m.id == model)
            {
                let exit = emit_error(
                    ctx,
                    "conflict",
                    &format!("custom model '{provider}/{model}' already exists"),
                )?;
                std::process::exit(exit);
            }
            let entry = CustomModel {
                provider_alias: provider.clone(),
                id: model.clone(),
                r#type,
                name,
                extra: BTreeMap::new(),
            };
            db.update(|app| app.custom_models.push(entry.clone()))
                .await?;
            if ctx.is_robot() {
                emit_robot(
                    "openproxy.v1.provider-models.custom.add",
                    serde_json::to_value(&entry)?,
                )?;
            } else {
                humanln(ctx, format!("added custom model {}/{}", provider, model));
            }
            Ok(())
        }
        CustomCmd::Remove { provider, model } => {
            let existed = db
                .snapshot()
                .custom_models
                .iter()
                .any(|m| m.provider_alias == provider && m.id == model);
            if !existed {
                if ctx.is_robot() {
                    emit_robot(
                        "openproxy.v1.provider-models.custom.remove",
                        json!({
                            "provider": provider,
                            "model": model,
                            "removed": false,
                        }),
                    )?;
                } else {
                    humanln(ctx, format!("{provider}/{model} not found (no-op)"));
                }
                return Ok(());
            }
            db.update(|app| {
                app.custom_models
                    .retain(|m| !(m.provider_alias == provider && m.id == model));
            })
            .await?;
            if ctx.is_robot() {
                emit_robot(
                    "openproxy.v1.provider-models.custom.remove",
                    json!({
                        "provider": provider,
                        "model": model,
                        "removed": true,
                    }),
                )?;
            } else {
                humanln(ctx, format!("removed custom model {provider}/{model}"));
            }
            Ok(())
        }
    }
}

fn disabled_map(extra: &BTreeMap<String, Value>) -> BTreeMap<String, Vec<String>> {
    extra
        .get("disabledModels")
        .cloned()
        .and_then(|v| serde_json::from_value::<BTreeMap<String, Vec<String>>>(v).ok())
        .unwrap_or_default()
}

fn write_disabled_map(extra: &mut BTreeMap<String, Value>, map: &BTreeMap<String, Vec<String>>) {
    if map.is_empty() {
        extra.remove("disabledModels");
    } else {
        extra.insert(
            "disabledModels".to_string(),
            serde_json::to_value(map).unwrap_or_else(|_| Value::Object(Default::default())),
        );
    }
}

fn disabled_models_for(extra: &BTreeMap<String, Value>, provider: &str) -> Vec<String> {
    disabled_map(extra).remove(provider).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn disabled_map_handles_missing_key() {
        let extra = BTreeMap::new();
        assert!(disabled_map(&extra).is_empty());
    }

    #[test]
    fn disabled_map_parses_existing_entries() {
        let mut extra = BTreeMap::new();
        extra.insert(
            "disabledModels".to_string(),
            json!({"openai": ["gpt-4o", "gpt-4o-mini"]}),
        );
        let map = disabled_map(&extra);
        assert_eq!(map.get("openai").unwrap(), &vec!["gpt-4o", "gpt-4o-mini"]);
    }

    #[test]
    fn write_disabled_map_removes_empty() {
        let mut extra = BTreeMap::new();
        extra.insert("disabledModels".to_string(), json!({"x": ["y"]}));
        write_disabled_map(&mut extra, &BTreeMap::new());
        assert!(!extra.contains_key("disabledModels"));
    }

    #[test]
    fn disabled_models_for_returns_provider_subset() {
        let mut extra = BTreeMap::new();
        extra.insert(
            "disabledModels".to_string(),
            json!({"openai": ["a"], "anthropic": ["b"]}),
        );
        assert_eq!(disabled_models_for(&extra, "openai"), vec!["a"]);
        assert!(disabled_models_for(&extra, "missing").is_empty());
    }
}
