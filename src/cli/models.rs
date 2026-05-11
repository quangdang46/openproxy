//! `openproxy models *` — top-level model registry view.
//!
//! Reads the built-in provider catalog merged with the user's custom
//! models, aliases, and disabled lists from `db.json`. Exposes a
//! pricing view that mirrors the dashboard's pricing editor.

use std::collections::BTreeMap;

use clap::Subcommand;
use serde_json::{json, Value};

use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};
use crate::core::model::catalog::provider_catalog;
use crate::db::Db;
use crate::types::ModelAliasTarget;

#[derive(Debug, Clone, Subcommand)]
pub enum ModelsCmd {
    /// List models across all providers (built-in + custom).
    List {
        /// Filter to a single provider alias.
        #[arg(long)]
        provider: Option<String>,
    },
    /// Show info for a single model id (`<provider>/<model>` or alias).
    Info { model: String },
    /// Probe the underlying provider connection for the model.
    Test { model: String },
    /// Show the pricing table for a model (or all models if not specified).
    Pricing {
        #[arg(long)]
        model: Option<String>,
    },
}

pub async fn run(cmd: ModelsCmd, db: &Db, ctx: OutputCtx) -> anyhow::Result<()> {
    match cmd {
        ModelsCmd::List { provider } => run_list(db, ctx, provider.as_deref()).await,
        ModelsCmd::Info { model } => run_info(db, ctx, &model).await,
        ModelsCmd::Test { model } => run_test(db, ctx, &model).await,
        ModelsCmd::Pricing { model } => run_pricing(db, ctx, model.as_deref()).await,
    }
}

async fn run_list(db: &Db, ctx: OutputCtx, provider: Option<&str>) -> anyhow::Result<()> {
    let catalog = provider_catalog();
    let snapshot = db.snapshot();

    let mut groups: Vec<Value> = Vec::new();
    for entry in catalog.iter_provider_models() {
        if let Some(p) = provider {
            if entry.alias != p {
                continue;
            }
        }
        let custom_for_provider: Vec<&str> = snapshot
            .custom_models
            .iter()
            .filter(|m| m.provider_alias == entry.alias)
            .map(|m| m.id.as_str())
            .collect();
        let mut models: Vec<Value> = entry
            .models
            .iter()
            .map(|m| {
                json!({
                    "id": m.id,
                    "name": m.name,
                    "kind": m.kind,
                    "source": "builtin",
                })
            })
            .collect();
        for id in &custom_for_provider {
            models.push(json!({ "id": id, "source": "custom" }));
        }
        groups.push(json!({
            "provider": entry.alias,
            "count": models.len(),
            "models": models,
        }));
    }

    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.models.list",
            json!({ "providers": groups, "count": groups.len() }),
        )?;
    } else {
        for group in &groups {
            humanln(
                ctx,
                format!(
                    "{} ({})",
                    group.get("provider").and_then(Value::as_str).unwrap_or(""),
                    group.get("count").and_then(Value::as_u64).unwrap_or(0)
                ),
            );
            if let Some(models) = group.get("models").and_then(Value::as_array) {
                for m in models {
                    humanln(
                        ctx,
                        format!(
                            "  {}  {}  [{}]",
                            m.get("id").and_then(Value::as_str).unwrap_or(""),
                            m.get("kind").and_then(Value::as_str).unwrap_or(""),
                            m.get("source").and_then(Value::as_str).unwrap_or(""),
                        ),
                    );
                }
            }
        }
    }
    Ok(())
}

async fn run_info(db: &Db, ctx: OutputCtx, model: &str) -> anyhow::Result<()> {
    let snapshot = db.snapshot();
    let resolved = crate::core::model::get_model_info(model, &snapshot);
    let alias_target = snapshot.model_aliases.get(model).cloned();
    let provider = resolved.provider.as_deref().unwrap_or("");
    let pricing = snapshot.pricing.get(provider).cloned();
    let route_kind = match resolved.route_kind {
        crate::core::model::ModelRouteKind::Direct => "direct",
        crate::core::model::ModelRouteKind::Combo => "combo",
    };
    let payload = json!({
        "input": model,
        "provider": resolved.provider,
        "modelId": resolved.model,
        "routeKind": route_kind,
        "alias": alias_target.map(|t| match t {
            ModelAliasTarget::Path(s) => json!({"kind":"path","value":s}),
            ModelAliasTarget::Mapping(r) => json!({"kind":"mapping","provider":r.provider,"model":r.model}),
        }),
        "pricing": pricing,
    });
    if ctx.is_robot() {
        emit_robot("openproxy.v1.models.info", payload)?;
    } else {
        humanln(ctx, format!("Model: {model}"));
        humanln(
            ctx,
            format!(
                "  provider: {}",
                resolved.provider.as_deref().unwrap_or("?"),
            ),
        );
        humanln(ctx, format!("  modelId:  {}", resolved.model));
        humanln(ctx, format!("  route:    {route_kind}"));
    }
    Ok(())
}

async fn run_test(db: &Db, ctx: OutputCtx, model: &str) -> anyhow::Result<()> {
    let snapshot = db.snapshot();
    let resolved = crate::core::model::get_model_info(model, &snapshot);
    let Some(provider_alias) = resolved.provider.as_deref() else {
        let exit = emit_error(
            ctx,
            "not_found",
            &format!("cannot resolve provider for '{model}'"),
        )?;
        std::process::exit(exit);
    };
    let conn = snapshot
        .provider_connections
        .iter()
        .find(|c| c.provider == provider_alias || c.name.as_deref() == Some(provider_alias))
        .cloned();
    let Some(conn) = conn else {
        let exit = emit_error(
            ctx,
            "not_found",
            &format!("no provider connection configured for '{provider_alias}'"),
        )?;
        std::process::exit(exit);
    };
    let base_url = conn
        .provider_specific_data
        .get("baseUrl")
        .and_then(Value::as_str)
        .map(String::from);
    let start = std::time::Instant::now();
    let (valid, error, latency_ms) = crate::server::api::providers::test_provider_api(
        provider_alias,
        conn.api_key.as_deref(),
        base_url.as_deref(),
    )
    .await;
    let latency_ms = latency_ms.unwrap_or(start.elapsed().as_millis() as u64);
    let payload = json!({
        "model": model,
        "providerAlias": provider_alias,
        "valid": valid,
        "latencyMs": latency_ms,
        "error": error,
    });
    if ctx.is_robot() {
        emit_robot("openproxy.v1.models.test", payload)?;
    } else if valid {
        humanln(
            ctx,
            format!("OK   {model} via {provider_alias} ({latency_ms}ms)"),
        );
    } else {
        humanln(
            ctx,
            format!(
                "FAIL {model} via {provider_alias} ({latency_ms}ms) — {}",
                error.as_deref().unwrap_or("unknown")
            ),
        );
    }
    Ok(())
}

async fn run_pricing(db: &Db, ctx: OutputCtx, model: Option<&str>) -> anyhow::Result<()> {
    let snapshot = db.snapshot();
    let pricing = if let Some(m) = model {
        let mut bucket: BTreeMap<String, Value> = BTreeMap::new();
        for (provider, rows) in &snapshot.pricing {
            if let Some(entry) = rows.get(m) {
                bucket.insert(provider.clone(), entry.clone());
            }
        }
        json!({ "model": m, "entries": bucket })
    } else {
        serde_json::to_value(&snapshot.pricing)?
    };
    if ctx.is_robot() {
        emit_robot("openproxy.v1.models.pricing", pricing)?;
    } else {
        let pretty = serde_json::to_string_pretty(&pricing).unwrap_or_default();
        println!("{pretty}");
    }
    Ok(())
}
