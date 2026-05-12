//! `openproxy provider *` — extended provider-connection commands (M3).
//!
//! Complements the existing `provider list/add`. Adds the full CRUD
//! lifecycle, connection test/validate, client-info, and idempotent
//! `apply` with diff output.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use clap::Subcommand;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::cli::apply::{into_items, load_document, ApplyDiff};
use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};
use crate::db::{Db, ProviderConnectionFilter};
use crate::types::ProviderConnection;

#[derive(Debug, Clone, Subcommand)]
pub enum ProviderExtCmd {
    /// Show one provider by id or name.
    Get { id_or_name: String },
    /// Edit a provider connection. Any flag omitted = unchanged.
    Edit {
        id_or_name: String,
        #[arg(long)]
        api_key: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        priority: Option<u32>,
        #[arg(long)]
        default_model: Option<String>,
    },
    /// Delete a provider.
    Delete {
        id_or_name: String,
        #[arg(long)]
        strict: bool,
    },
    /// Mark provider active.
    Enable { id_or_name: String },
    /// Mark provider inactive.
    Disable { id_or_name: String },
    /// Run a real connectivity probe against the provider's `/v1/models`.
    Test { id_or_name: String },
    /// Validate raw credentials (does not require saved connection).
    Validate {
        /// Provider alias (openai, anthropic, ...).
        #[arg(long)]
        provider: String,
        #[arg(long)]
        api_key: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
    },
    /// Report the hostname/version/client identity (mirrors /api/providers/client).
    ClientInfo,
    /// Idempotent upsert from YAML/JSON.
    Apply {
        #[arg(long = "from-file", default_value = "-")]
        from_file: String,
        #[arg(long)]
        prune: bool,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderInput {
    name: String,
    provider: String,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    priority: Option<u32>,
    #[serde(default)]
    is_active: Option<bool>,
    #[serde(default)]
    default_model: Option<String>,
    #[serde(default)]
    auth_type: Option<String>,
}

pub async fn run(cmd: ProviderExtCmd, db: &Db, ctx: OutputCtx) -> anyhow::Result<()> {
    match cmd {
        ProviderExtCmd::Get { id_or_name } => run_get(db, ctx, &id_or_name).await,
        ProviderExtCmd::Edit {
            id_or_name,
            api_key,
            base_url,
            priority,
            default_model,
        } => {
            run_edit(
                db,
                ctx,
                &id_or_name,
                api_key,
                base_url,
                priority,
                default_model,
            )
            .await
        }
        ProviderExtCmd::Delete { id_or_name, strict } => {
            run_delete(db, ctx, &id_or_name, strict).await
        }
        ProviderExtCmd::Enable { id_or_name } => run_set_active(db, ctx, &id_or_name, true).await,
        ProviderExtCmd::Disable { id_or_name } => run_set_active(db, ctx, &id_or_name, false).await,
        ProviderExtCmd::Test { id_or_name } => run_test(db, ctx, &id_or_name).await,
        ProviderExtCmd::Validate {
            provider,
            api_key,
            base_url,
        } => run_validate(ctx, &provider, api_key, base_url).await,
        ProviderExtCmd::ClientInfo => run_client_info(ctx).await,
        ProviderExtCmd::Apply { from_file, prune } => run_apply(db, ctx, &from_file, prune).await,
    }
}

fn find_provider(db: &Db, id_or_name: &str) -> Option<ProviderConnection> {
    db.snapshot()
        .provider_connections
        .iter()
        .find(|c| {
            c.id == id_or_name || c.name.as_deref() == Some(id_or_name) || c.provider == id_or_name
        })
        .cloned()
}

async fn run_get(db: &Db, ctx: OutputCtx, id_or_name: &str) -> anyhow::Result<()> {
    let Some(conn) = find_provider(db, id_or_name) else {
        let exit = emit_error(
            ctx,
            "not_found",
            &format!("provider '{id_or_name}' not found"),
        )?;
        std::process::exit(exit);
    };
    let mut conn_masked = conn.clone();
    if let Some(key) = conn_masked.api_key.as_deref() {
        conn_masked.api_key = Some(crate::cli::output::mask_secret(key));
    }
    if let Some(token) = conn_masked.access_token.as_deref() {
        conn_masked.access_token = Some(crate::cli::output::mask_secret(token));
    }
    if let Some(token) = conn_masked.refresh_token.as_deref() {
        conn_masked.refresh_token = Some(crate::cli::output::mask_secret(token));
    }
    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.provider.get",
            serde_json::to_value(&conn_masked)?,
        )?;
    } else {
        humanln(
            ctx,
            format!(
                "Provider: {} ({})  type={}",
                conn.name.as_deref().unwrap_or("-"),
                conn.id,
                conn.provider
            ),
        );
        humanln(
            ctx,
            format!(
                "  apiKey:  {}",
                conn.api_key
                    .as_deref()
                    .map(crate::cli::output::mask_secret)
                    .unwrap_or_else(|| "-".into())
            ),
        );
        humanln(
            ctx,
            format!("  active:  {}", conn.is_active.unwrap_or(true)),
        );
    }
    Ok(())
}

async fn run_edit(
    db: &Db,
    ctx: OutputCtx,
    id_or_name: &str,
    api_key: Option<String>,
    base_url: Option<String>,
    priority: Option<u32>,
    default_model: Option<String>,
) -> anyhow::Result<()> {
    if find_provider(db, id_or_name).is_none() {
        let exit = emit_error(
            ctx,
            "not_found",
            &format!("provider '{id_or_name}' not found"),
        )?;
        std::process::exit(exit);
    }
    let mut updated: Option<ProviderConnection> = None;
    db.update(|app| {
        if let Some(conn) = app.provider_connections.iter_mut().find(|c| {
            c.id == id_or_name || c.name.as_deref() == Some(id_or_name) || c.provider == id_or_name
        }) {
            if let Some(key) = &api_key {
                conn.api_key = Some(key.clone());
            }
            if let Some(url) = &base_url {
                conn.provider_specific_data
                    .insert("baseUrl".into(), Value::String(url.clone()));
            }
            if let Some(p) = priority {
                conn.priority = Some(p);
            }
            if let Some(model) = &default_model {
                conn.default_model = Some(model.clone());
            }
            conn.updated_at = Some(chrono::Utc::now().to_rfc3339());
            updated = Some(conn.clone());
        }
    })
    .await?;

    let mut conn = updated.expect("provider existed");
    if let Some(key) = conn.api_key.as_deref() {
        conn.api_key = Some(crate::cli::output::mask_secret(key));
    }
    if ctx.is_robot() {
        emit_robot("openproxy.v1.provider.edit", serde_json::to_value(&conn)?)?;
    } else {
        humanln(ctx, format!("updated provider '{id_or_name}'"));
    }
    Ok(())
}

async fn run_delete(db: &Db, ctx: OutputCtx, id_or_name: &str, strict: bool) -> anyhow::Result<()> {
    if find_provider(db, id_or_name).is_none() {
        if strict {
            let exit = emit_error(
                ctx,
                "not_found",
                &format!("provider '{id_or_name}' not found"),
            )?;
            std::process::exit(exit);
        }
        if ctx.is_robot() {
            emit_robot(
                "openproxy.v1.provider.delete",
                json!({ "key": id_or_name, "deleted": false }),
            )?;
        } else {
            humanln(ctx, format!("provider '{id_or_name}' not found (no-op)"));
        }
        return Ok(());
    }
    db.update(|app| {
        app.provider_connections.retain(|c| {
            c.id != id_or_name && c.name.as_deref() != Some(id_or_name) && c.provider != id_or_name
        });
    })
    .await?;
    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.provider.delete",
            json!({ "key": id_or_name, "deleted": true }),
        )?;
    } else {
        humanln(ctx, format!("deleted provider '{id_or_name}'"));
    }
    Ok(())
}

async fn run_set_active(
    db: &Db,
    ctx: OutputCtx,
    id_or_name: &str,
    active: bool,
) -> anyhow::Result<()> {
    if find_provider(db, id_or_name).is_none() {
        let exit = emit_error(
            ctx,
            "not_found",
            &format!("provider '{id_or_name}' not found"),
        )?;
        std::process::exit(exit);
    }
    db.update(|app| {
        if let Some(conn) = app.provider_connections.iter_mut().find(|c| {
            c.id == id_or_name || c.name.as_deref() == Some(id_or_name) || c.provider == id_or_name
        }) {
            conn.is_active = Some(active);
            conn.updated_at = Some(chrono::Utc::now().to_rfc3339());
        }
    })
    .await?;
    let schema = if active {
        "openproxy.v1.provider.enable"
    } else {
        "openproxy.v1.provider.disable"
    };
    if ctx.is_robot() {
        emit_robot(schema, json!({ "provider": id_or_name, "active": active }))?;
    } else {
        humanln(
            ctx,
            format!(
                "{} provider '{id_or_name}'",
                if active { "enabled" } else { "disabled" }
            ),
        );
    }
    Ok(())
}

async fn run_test(db: &Db, ctx: OutputCtx, id_or_name: &str) -> anyhow::Result<()> {
    let Some(conn) = find_provider(db, id_or_name) else {
        let exit = emit_error(
            ctx,
            "not_found",
            &format!("provider '{id_or_name}' not found"),
        )?;
        std::process::exit(exit);
    };

    let base_url = conn
        .provider_specific_data
        .get("baseUrl")
        .and_then(Value::as_str)
        .map(String::from);
    let api_key = conn.api_key.as_deref();
    let start = std::time::Instant::now();
    let (valid, error, latency_ms) = crate::server::api::providers::test_provider_api(
        conn.provider.as_str(),
        api_key,
        base_url.as_deref(),
    )
    .await;
    let measured_ms = start.elapsed().as_millis() as u64;
    let latency_ms = latency_ms.unwrap_or(measured_ms);

    // Persist last-tested metadata
    let now = chrono::Utc::now().to_rfc3339();
    let valid_clone = valid;
    let error_clone = error.clone();
    let now_clone = now.clone();
    db.update(|app| {
        if let Some(c) = app
            .provider_connections
            .iter_mut()
            .find(|c| c.id == conn.id)
        {
            c.test_status = Some(if valid_clone { "ok" } else { "failed" }.to_string());
            c.last_tested = Some(now_clone.clone());
            c.last_error = error_clone.clone();
        }
    })
    .await?;

    let payload = json!({
        "providerId": conn.id,
        "provider": conn.provider,
        "valid": valid,
        "latencyMs": latency_ms,
        "error": error,
    });
    if ctx.is_robot() {
        emit_robot("openproxy.v1.provider.test", payload)?;
    } else if valid {
        humanln(ctx, format!("OK   {} ({}ms)", conn.provider, latency_ms));
    } else {
        humanln(
            ctx,
            format!(
                "FAIL {} ({}ms) — {}",
                conn.provider,
                latency_ms,
                error.as_deref().unwrap_or("unknown")
            ),
        );
    }
    Ok(())
}

async fn run_validate(
    ctx: OutputCtx,
    provider: &str,
    api_key: Option<String>,
    base_url: Option<String>,
) -> anyhow::Result<()> {
    let start = std::time::Instant::now();
    let (valid, error, latency_ms) = crate::server::api::providers::test_provider_api(
        provider,
        api_key.as_deref(),
        base_url.as_deref(),
    )
    .await;
    let measured_ms = start.elapsed().as_millis() as u64;
    let latency_ms = latency_ms.unwrap_or(measured_ms);

    let payload = json!({
        "provider": provider,
        "baseUrl": base_url,
        "valid": valid,
        "latencyMs": latency_ms,
        "error": error,
    });
    if ctx.is_robot() {
        emit_robot("openproxy.v1.provider.validate", payload)?;
    } else if valid {
        humanln(ctx, format!("OK   {provider} ({latency_ms}ms)"));
    } else {
        humanln(
            ctx,
            format!(
                "FAIL {provider} ({latency_ms}ms) — {}",
                error.as_deref().unwrap_or("unknown")
            ),
        );
    }
    Ok(())
}

async fn run_client_info(ctx: OutputCtx) -> anyhow::Result<()> {
    let client_id = whoami::fallible::hostname().unwrap_or_else(|_| "unknown".to_string());
    let client_name = whoami::username();
    let payload = json!({
        "clientId": client_id,
        "clientName": client_name,
        "version": env!("CARGO_PKG_VERSION"),
    });
    if ctx.is_robot() {
        emit_robot("openproxy.v1.provider.client-info", payload)?;
    } else {
        humanln(ctx, format!("Client ID:   {client_id}"));
        humanln(ctx, format!("Client name: {client_name}"));
        humanln(ctx, format!("Version:     {}", env!("CARGO_PKG_VERSION")));
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
    let items: Vec<ProviderInput> = match into_items(doc) {
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
            if let Some(existing) = app
                .provider_connections
                .iter_mut()
                .find(|c| c.name.as_deref() == Some(item.name.as_str()))
            {
                let mut changed = false;
                if existing.provider != item.provider {
                    existing.provider = item.provider.clone();
                    changed = true;
                }
                if let Some(ak) = item.api_key.as_deref().filter(|k| !k.is_empty()) {
                    if existing.api_key.as_deref() != Some(ak) {
                        existing.api_key = Some(ak.to_string());
                        changed = true;
                    }
                }
                if let Some(url) = item.base_url.as_deref().filter(|u| !u.is_empty()) {
                    let prev = existing
                        .provider_specific_data
                        .get("baseUrl")
                        .and_then(Value::as_str);
                    if prev != Some(url) {
                        existing
                            .provider_specific_data
                            .insert("baseUrl".into(), Value::String(url.to_string()));
                        changed = true;
                    }
                }
                if let Some(p) = item.priority {
                    if existing.priority != Some(p) {
                        existing.priority = Some(p);
                        changed = true;
                    }
                }
                if let Some(active) = item.is_active {
                    if existing.is_active.unwrap_or(true) != active {
                        existing.is_active = Some(active);
                        changed = true;
                    }
                }
                if let Some(model) = item.default_model.as_deref() {
                    if existing.default_model.as_deref() != Some(model) {
                        existing.default_model = Some(model.to_string());
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
                let mut psd = BTreeMap::new();
                if let Some(url) = item.base_url.as_deref().filter(|u| !u.is_empty()) {
                    psd.insert("baseUrl".to_string(), Value::String(url.to_string()));
                }
                app.provider_connections.push(ProviderConnection {
                    id: uuid::Uuid::new_v4().to_string(),
                    provider: item.provider.clone(),
                    auth_type: item
                        .auth_type
                        .clone()
                        .unwrap_or_else(|| "apiKey".to_string()),
                    name: Some(item.name.clone()),
                    priority: item.priority,
                    is_active: Some(item.is_active.unwrap_or(true)),
                    created_at: Some(now.clone()),
                    updated_at: Some(now.clone()),
                    api_key: item.api_key.clone(),
                    default_model: item.default_model.clone(),
                    provider_specific_data: psd,
                    ..Default::default()
                });
                diff.created.push(item.name.clone());
            }
        }
        if prune {
            let to_delete: Vec<String> = app
                .provider_connections
                .iter()
                .filter(|c| {
                    c.name
                        .as_deref()
                        .map(|n| !names_in_doc.contains(n))
                        .unwrap_or(true)
                })
                .map(|c| c.name.clone().unwrap_or_else(|| c.id.clone()))
                .collect();
            for name in &to_delete {
                diff.deleted.push(name.clone());
            }
            app.provider_connections.retain(|c| {
                c.name
                    .as_deref()
                    .map(|n| names_in_doc.contains(n))
                    .unwrap_or(false)
            });
        }
    })
    .await?;

    let summary = diff.summary();
    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.provider.apply",
            json!({ "diff": diff, "summary": summary, "prune": prune }),
        )?;
    } else {
        humanln(ctx, format!("provider apply: {summary}"));
    }
    let _ = ProviderConnectionFilter::default();
    let _ = Duration::from_secs(0);
    Ok(())
}
