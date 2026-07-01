//! `openproxy provider *` — extended provider-connection commands (M3).
//!
//! Complements the existing `provider list/add`. Adds the full CRUD
//! lifecycle, connection test/validate, client-info, and idempotent
//! `apply` with diff output.

use std::collections::{BTreeMap, HashSet};

use clap::Subcommand;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::cli::apply::{into_items, load_document, ApplyDiff};
use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};
use crate::db::Db;
use crate::types::{AppDb, ProviderConnection};

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
        /// Preview changes without modifying the database.
        #[arg(long)]
        dry_run: bool,
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
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    global_priority: Option<u32>,
    #[serde(default)]
    proxy_url: Option<String>,
    #[serde(default)]
    proxy_label: Option<String>,
    #[serde(default)]
    use_connection_proxy: Option<bool>,
    #[serde(default)]
    provider_specific_data: Option<BTreeMap<String, Value>>,
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
        ProviderExtCmd::Apply {
            from_file,
            prune,
            dry_run,
        } => run_apply(db, ctx, &from_file, prune, dry_run).await,
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

async fn run_apply(
    db: &Db,
    ctx: OutputCtx,
    from_file: &str,
    prune: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
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

    // Collect changes to compute diff without committing
    let diff_only = dry_run;

    if !diff_only {
        db.update(|app| {
            apply_items(app, &items, &names_in_doc, &mut diff, &now, prune);
        })
        .await?;
    } else {
        // Dry run: open a snapshot for reading only
        let snap = db.snapshot();
        let mut phantom = (*snap).clone();
        apply_items(&mut phantom, &items, &names_in_doc, &mut diff, &now, prune);
    }

    let summary = diff.summary();
    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.provider.apply",
            json!({ "diff": diff, "summary": summary, "prune": prune, "dry_run": dry_run }),
        )?;
    } else {
        humanln(
            ctx,
            format!(
                "provider apply ({}{}): {summary}",
                if dry_run { "DRY RUN, " } else { "" },
                if prune { "prune" } else { "no prune" },
            ),
        );
    }
    Ok(())
}

fn apply_items(
    app: &mut AppDb,
    items: &[ProviderInput],
    names_in_doc: &HashSet<String>,
    diff: &mut ApplyDiff,
    now: &str,
    prune: bool,
) {
    for item in items {
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
            if let Some(dn) = item.display_name.as_deref().filter(|s| !s.is_empty()) {
                if existing.display_name.as_deref() != Some(dn) {
                    existing.display_name = Some(dn.to_string());
                    changed = true;
                }
            }
            if let Some(email) = item.email.as_deref().filter(|s| !s.is_empty()) {
                if existing.email.as_deref() != Some(email) {
                    existing.email = Some(email.to_string());
                    changed = true;
                }
            }
            if let Some(gp) = item.global_priority {
                if existing.global_priority != Some(gp) {
                    existing.global_priority = Some(gp);
                    changed = true;
                }
            }
            if let Some(pu) = item.proxy_url.as_deref().filter(|s| !s.is_empty()) {
                if existing.proxy_url.as_deref() != Some(pu) {
                    existing.proxy_url = Some(pu.to_string());
                    changed = true;
                }
            }
            if let Some(pl) = item.proxy_label.as_deref().filter(|s| !s.is_empty()) {
                if existing.proxy_label.as_deref() != Some(pl) {
                    existing.proxy_label = Some(pl.to_string());
                    changed = true;
                }
            }
            if let Some(ucp) = item.use_connection_proxy {
                if existing.use_connection_proxy != Some(ucp) {
                    existing.use_connection_proxy = Some(ucp);
                    changed = true;
                }
            }
            if let Some(psd) = &item.provider_specific_data {
                if &existing.provider_specific_data != psd {
                    existing.provider_specific_data = psd.clone();
                    changed = true;
                }
            }
            if let Some(at) = item.auth_type.as_deref().filter(|s| !s.is_empty()) {
                if existing.auth_type != at {
                    existing.auth_type = at.to_string();
                    changed = true;
                }
            }
            if changed {
                existing.updated_at = Some(now.to_string());
                diff.updated.push(item.name.clone());
            } else {
                diff.unchanged.push(item.name.clone());
            }
        } else {
            let mut psd = BTreeMap::new();
            if let Some(url) = item.base_url.as_deref().filter(|u| !u.is_empty()) {
                psd.insert("baseUrl".to_string(), Value::String(url.to_string()));
            }
            if let Some(explicit_psd) = &item.provider_specific_data {
                for (k, v) in explicit_psd {
                    psd.insert(k.clone(), v.clone());
                }
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
                created_at: Some(now.to_string()),
                updated_at: Some(now.to_string()),
                api_key: item.api_key.clone(),
                default_model: item.default_model.clone(),
                display_name: item.display_name.clone(),
                email: item.email.clone(),
                global_priority: item.global_priority,
                proxy_url: item.proxy_url.clone(),
                proxy_label: item.proxy_label.clone(),
                use_connection_proxy: item.use_connection_proxy,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AppDb;

    #[test]
    fn test_apply_items_creates_new_providers() {
        let mut app = AppDb::default();
        let items = vec![ProviderInput {
            name: "test-openai".into(),
            provider: "openai".into(),
            api_key: Some("sk-test".into()),
            base_url: None,
            priority: Some(1),
            is_active: Some(true),
            default_model: Some("gpt-4".into()),
            auth_type: Some("apiKey".into()),
            display_name: None,
            email: None,
            global_priority: None,
            proxy_url: None,
            proxy_label: None,
            use_connection_proxy: None,
            provider_specific_data: None,
        }];
        let names: HashSet<_> = items.iter().map(|i| i.name.clone()).collect();
        let mut diff = ApplyDiff::default();
        let now = "2026-01-01T00:00:00Z";

        apply_items(&mut app, &items, &names, &mut diff, now, false);

        assert_eq!(diff.created.len(), 1);
        assert_eq!(diff.created[0], "test-openai");
        assert!(diff.updated.is_empty());
        assert!(diff.unchanged.is_empty());
        assert_eq!(app.provider_connections.len(), 1);
        assert_eq!(app.provider_connections[0].api_key, Some("sk-test".into()));
    }

    #[test]
    fn test_apply_items_updates_existing_provider() {
        let mut app = AppDb::default();
        app.provider_connections.push(ProviderConnection {
            id: "id-1".into(),
            provider: "openai".into(),
            auth_type: "apiKey".into(),
            name: Some("test-openai".into()),
            priority: Some(1),
            is_active: Some(true),
            api_key: Some("sk-old".into()),
            default_model: Some("gpt-3.5".into()),
            ..Default::default()
        });

        let items = vec![ProviderInput {
            name: "test-openai".into(),
            provider: "openai".into(),
            api_key: Some("sk-new".into()),
            base_url: None,
            priority: Some(2),
            is_active: Some(true),
            default_model: Some("gpt-4".into()),
            auth_type: None,
            display_name: None,
            email: None,
            global_priority: None,
            proxy_url: None,
            proxy_label: None,
            use_connection_proxy: None,
            provider_specific_data: None,
        }];
        let names: HashSet<_> = items.iter().map(|i| i.name.clone()).collect();
        let mut diff = ApplyDiff::default();
        let now = "2026-06-01T00:00:00Z";

        apply_items(&mut app, &items, &names, &mut diff, now, false);

        assert_eq!(diff.updated.len(), 1);
        assert_eq!(diff.updated[0], "test-openai");
        assert_eq!(app.provider_connections[0].api_key, Some("sk-new".into()));
        assert_eq!(app.provider_connections[0].priority, Some(2));
        assert_eq!(
            app.provider_connections[0].default_model,
            Some("gpt-4".into())
        );
        assert_eq!(
            app.provider_connections[0].updated_at,
            Some("2026-06-01T00:00:00Z".into())
        );
    }

    #[test]
    fn test_apply_items_unchanged() {
        let mut app = AppDb::default();
        app.provider_connections.push(ProviderConnection {
            id: "id-1".into(),
            provider: "openai".into(),
            auth_type: "apiKey".into(),
            name: Some("test-openai".into()),
            priority: Some(1),
            is_active: Some(true),
            api_key: Some("sk-test".into()),
            default_model: Some("gpt-4".into()),
            ..Default::default()
        });

        let items = vec![ProviderInput {
            name: "test-openai".into(),
            provider: "openai".into(),
            api_key: Some("sk-test".into()),
            base_url: None,
            priority: Some(1),
            is_active: Some(true),
            default_model: Some("gpt-4".into()),
            auth_type: None,
            display_name: None,
            email: None,
            global_priority: None,
            proxy_url: None,
            proxy_label: None,
            use_connection_proxy: None,
            provider_specific_data: None,
        }];
        let names: HashSet<_> = items.iter().map(|i| i.name.clone()).collect();
        let mut diff = ApplyDiff::default();
        let now = "2026-06-01T00:00:00Z";

        apply_items(&mut app, &items, &names, &mut diff, now, false);

        assert_eq!(diff.unchanged.len(), 1);
        assert!(diff.created.is_empty());
        assert!(diff.updated.is_empty());
    }

    #[test]
    fn test_apply_items_prune_deletes_unnamed() {
        let mut app = AppDb::default();
        app.provider_connections.push(ProviderConnection {
            id: "keep".into(),
            provider: "openai".into(),
            auth_type: "apiKey".into(),
            name: Some("keep-me".into()),
            ..Default::default()
        });
        app.provider_connections.push(ProviderConnection {
            id: "delete".into(),
            provider: "anthropic".into(),
            auth_type: "apiKey".into(),
            name: Some("delete-me".into()),
            ..Default::default()
        });

        let items = vec![ProviderInput {
            name: "keep-me".into(),
            provider: "openai".into(),
            api_key: None,
            base_url: None,
            priority: None,
            is_active: None,
            default_model: None,
            auth_type: None,
            display_name: None,
            email: None,
            global_priority: None,
            proxy_url: None,
            proxy_label: None,
            use_connection_proxy: None,
            provider_specific_data: None,
        }];
        let names: HashSet<_> = items.iter().map(|i| i.name.clone()).collect();
        let mut diff = ApplyDiff::default();
        let now = "2026-06-01T00:00:00Z";

        apply_items(&mut app, &items, &names, &mut diff, now, true);

        assert_eq!(diff.deleted.len(), 1);
        assert_eq!(diff.deleted[0], "delete-me");
        assert_eq!(app.provider_connections.len(), 1);
        assert_eq!(app.provider_connections[0].name, Some("keep-me".into()));
    }

    #[test]
    fn test_apply_items_with_new_fields() {
        let mut app = AppDb::default();
        let items = vec![ProviderInput {
            name: "full-provider".into(),
            provider: "custom".into(),
            api_key: Some("sk-custom".into()),
            base_url: Some("https://custom.api.com".into()),
            priority: Some(5),
            is_active: Some(true),
            default_model: Some("custom-model".into()),
            auth_type: Some("bearer".into()),
            display_name: Some("My Custom Provider".into()),
            email: Some("admin@custom.com".into()),
            global_priority: Some(10),
            proxy_url: Some("http://proxy:8080".into()),
            proxy_label: Some("corp-proxy".into()),
            use_connection_proxy: Some(true),
            provider_specific_data: Some(BTreeMap::from([(
                "extraField".into(),
                Value::String("extra".into()),
            )])),
        }];
        let names: HashSet<_> = items.iter().map(|i| i.name.clone()).collect();
        let mut diff = ApplyDiff::default();
        let now = "2026-06-01T00:00:00Z";

        apply_items(&mut app, &items, &names, &mut diff, now, false);

        assert_eq!(diff.created.len(), 1);
        let p = &app.provider_connections[0];
        assert_eq!(p.display_name, Some("My Custom Provider".into()));
        assert_eq!(p.email, Some("admin@custom.com".into()));
        assert_eq!(p.global_priority, Some(10));
        assert_eq!(p.proxy_url, Some("http://proxy:8080".into()));
        assert_eq!(p.proxy_label, Some("corp-proxy".into()));
        assert_eq!(p.use_connection_proxy, Some(true));
        assert_eq!(
            p.provider_specific_data.get("extraField"),
            Some(&Value::String("extra".into()))
        );
    }
}
