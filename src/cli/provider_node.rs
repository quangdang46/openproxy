//! `openproxy provider node *` — register custom provider instances.
//!
//! A `ProviderNode` is the "what is this server's API shape?" half. It
//! pairs with one or more `ProviderConnection` entries that hold actual
//! credentials. Combo entries that reference a custom node use the node's
//! UUID as the provider prefix (`<node-uuid>/gpt-4o`).

use std::collections::BTreeMap;
use std::time::Duration;

use clap::Subcommand;
use serde_json::{json, Value};

use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};
use crate::db::Db;
use crate::types::ProviderNode;

#[derive(Debug, Clone, Subcommand)]
pub enum NodeCmd {
    /// List registered provider nodes.
    List {
        /// Optional filter by node type (e.g. `openai-compatible`).
        #[arg(long)]
        r#type: Option<String>,
    },
    /// Show one node by id or name.
    Get { id_or_name: String },
    /// Register a new provider node.
    Add {
        #[arg(long)]
        name: String,
        /// Node type, e.g. `openai-compatible`, `anthropic-compatible`.
        #[arg(long, default_value = "openai-compatible")]
        r#type: String,
        /// Base URL of the provider's API.
        #[arg(long)]
        base_url: String,
        /// Optional prefix used to namespace this node's model ids.
        #[arg(long)]
        prefix: Option<String>,
        /// Optional API type override (e.g. `openai`, `anthropic`).
        #[arg(long)]
        api_type: Option<String>,
    },
    /// Edit an existing node. Any flag omitted is left unchanged.
    Edit {
        id_or_name: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        prefix: Option<String>,
        #[arg(long)]
        api_type: Option<String>,
    },
    /// Delete a node.
    Delete {
        id_or_name: String,
        /// Fail with exit code 3 if the node does not exist.
        #[arg(long)]
        strict: bool,
    },
    /// Probe `<baseUrl>/models` (or `/embeddings` for embedding nodes) to
    /// confirm the node is reachable. Does NOT touch DB.
    Validate {
        id_or_name: String,
        /// Bearer token to send (defaults to "test" for unauthenticated probes).
        #[arg(long)]
        api_key: Option<String>,
        /// Model id for embedding-style probes.
        #[arg(long)]
        model_id: Option<String>,
    },
}

pub async fn run(cmd: NodeCmd, db: &Db, ctx: OutputCtx) -> anyhow::Result<()> {
    match cmd {
        NodeCmd::List { r#type } => run_list(db, ctx, r#type.as_deref()).await,
        NodeCmd::Get { id_or_name } => run_get(db, ctx, &id_or_name).await,
        NodeCmd::Add {
            name,
            r#type,
            base_url,
            prefix,
            api_type,
        } => run_add(db, ctx, name, r#type, base_url, prefix, api_type).await,
        NodeCmd::Edit {
            id_or_name,
            name,
            base_url,
            prefix,
            api_type,
        } => run_edit(db, ctx, &id_or_name, name, base_url, prefix, api_type).await,
        NodeCmd::Delete { id_or_name, strict } => run_delete(db, ctx, &id_or_name, strict).await,
        NodeCmd::Validate {
            id_or_name,
            api_key,
            model_id,
        } => run_validate(db, ctx, &id_or_name, api_key, model_id).await,
    }
}

fn find_node(db: &Db, id_or_name: &str) -> Option<ProviderNode> {
    db.snapshot()
        .provider_nodes
        .iter()
        .find(|n| n.id == id_or_name || n.name == id_or_name)
        .cloned()
}

async fn run_list(db: &Db, ctx: OutputCtx, node_type: Option<&str>) -> anyhow::Result<()> {
    let nodes = db.provider_nodes(node_type);
    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.provider-node.list",
            json!({ "nodes": nodes, "count": nodes.len() }),
        )?;
    } else {
        humanln(ctx, format!("Provider nodes ({}):", nodes.len()));
        for node in &nodes {
            humanln(
                ctx,
                format!(
                    "  {} ({})  type={}  baseUrl={}",
                    node.name,
                    node.id,
                    node.r#type,
                    node.base_url.as_deref().unwrap_or("-"),
                ),
            );
        }
    }
    Ok(())
}

async fn run_get(db: &Db, ctx: OutputCtx, id_or_name: &str) -> anyhow::Result<()> {
    let Some(node) = find_node(db, id_or_name) else {
        let exit = emit_error(
            ctx,
            "not_found",
            &format!("provider node '{id_or_name}' not found"),
        )?;
        std::process::exit(exit);
    };
    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.provider-node.get",
            serde_json::to_value(&node)?,
        )?;
    } else {
        humanln(ctx, format!("Node: {} ({})", node.name, node.id));
        humanln(ctx, format!("  type: {}", node.r#type));
        humanln(
            ctx,
            format!("  baseUrl: {}", node.base_url.as_deref().unwrap_or("-")),
        );
        if let Some(prefix) = &node.prefix {
            humanln(ctx, format!("  prefix: {prefix}"));
        }
        if let Some(api_type) = &node.api_type {
            humanln(ctx, format!("  apiType: {api_type}"));
        }
    }
    Ok(())
}

async fn run_add(
    db: &Db,
    ctx: OutputCtx,
    name: String,
    r#type: String,
    base_url: String,
    prefix: Option<String>,
    api_type: Option<String>,
) -> anyhow::Result<()> {
    if name.trim().is_empty() {
        let exit = emit_error(ctx, "validation", "--name is required")?;
        std::process::exit(exit);
    }
    if base_url.trim().is_empty() {
        let exit = emit_error(ctx, "validation", "--base-url is required")?;
        std::process::exit(exit);
    }
    if db.snapshot().provider_nodes.iter().any(|n| n.name == name) {
        let exit = emit_error(
            ctx,
            "conflict",
            &format!("provider node '{name}' already exists"),
        )?;
        std::process::exit(exit);
    }

    let now = chrono::Utc::now().to_rfc3339();
    let node = ProviderNode {
        id: uuid::Uuid::new_v4().to_string(),
        r#type,
        name,
        prefix,
        api_type,
        base_url: Some(base_url),
        created_at: Some(now.clone()),
        updated_at: Some(now),
        extra: BTreeMap::new(),
    };

    db.update(|db| db.provider_nodes.push(node.clone())).await?;

    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.provider-node.add",
            serde_json::to_value(&node)?,
        )?;
    } else {
        humanln(
            ctx,
            format!("created provider node '{}' ({})", node.name, node.id),
        );
    }
    Ok(())
}

async fn run_edit(
    db: &Db,
    ctx: OutputCtx,
    id_or_name: &str,
    name: Option<String>,
    base_url: Option<String>,
    prefix: Option<String>,
    api_type: Option<String>,
) -> anyhow::Result<()> {
    if find_node(db, id_or_name).is_none() {
        let exit = emit_error(
            ctx,
            "not_found",
            &format!("provider node '{id_or_name}' not found"),
        )?;
        std::process::exit(exit);
    }
    let mut updated: Option<ProviderNode> = None;
    db.update(|app| {
        if let Some(node) = app
            .provider_nodes
            .iter_mut()
            .find(|n| n.id == id_or_name || n.name == id_or_name)
        {
            if let Some(v) = &name {
                node.name = v.clone();
            }
            if let Some(v) = &base_url {
                node.base_url = Some(v.clone());
            }
            if let Some(v) = &prefix {
                node.prefix = Some(v.clone());
            }
            if let Some(v) = &api_type {
                node.api_type = Some(v.clone());
            }
            node.updated_at = Some(chrono::Utc::now().to_rfc3339());
            updated = Some(node.clone());
        }
    })
    .await?;

    let node = updated.expect("node existed");
    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.provider-node.edit",
            serde_json::to_value(&node)?,
        )?;
    } else {
        humanln(ctx, format!("updated provider node '{}'", node.name));
    }
    Ok(())
}

async fn run_delete(db: &Db, ctx: OutputCtx, id_or_name: &str, strict: bool) -> anyhow::Result<()> {
    if find_node(db, id_or_name).is_none() {
        if strict {
            let exit = emit_error(
                ctx,
                "not_found",
                &format!("provider node '{id_or_name}' not found"),
            )?;
            std::process::exit(exit);
        }
        if ctx.is_robot() {
            emit_robot(
                "openproxy.v1.provider-node.delete",
                json!({ "key": id_or_name, "deleted": false }),
            )?;
        } else {
            humanln(
                ctx,
                format!("provider node '{id_or_name}' not found (no-op)"),
            );
        }
        return Ok(());
    }

    db.update(|app| {
        app.provider_nodes
            .retain(|n| n.id != id_or_name && n.name != id_or_name);
    })
    .await?;

    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.provider-node.delete",
            json!({ "key": id_or_name, "deleted": true }),
        )?;
    } else {
        humanln(ctx, format!("deleted provider node '{id_or_name}'"));
    }
    Ok(())
}

async fn run_validate(
    db: &Db,
    ctx: OutputCtx,
    id_or_name: &str,
    api_key: Option<String>,
    model_id: Option<String>,
) -> anyhow::Result<()> {
    let Some(node) = find_node(db, id_or_name) else {
        let exit = emit_error(
            ctx,
            "not_found",
            &format!("provider node '{id_or_name}' not found"),
        )?;
        std::process::exit(exit);
    };
    let Some(base_url) = node.base_url.as_deref() else {
        let exit = emit_error(
            ctx,
            "validation",
            &format!("node '{id_or_name}' has no baseUrl"),
        )?;
        std::process::exit(exit);
    };

    let probe_url = if node.r#type == "custom-embedding" {
        format!("{}/embeddings", base_url.trim_end_matches('/'))
    } else {
        format!("{}/models", base_url.trim_end_matches('/'))
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let mut req = client.get(&probe_url);
    if let Some(key) = api_key.as_deref().filter(|k| !k.is_empty()) {
        req = req.header("Authorization", format!("Bearer {key}"));
    }
    if let Some(model) = model_id.as_deref().filter(|m| !m.is_empty()) {
        req = req.query(&[("model", model)]);
    }

    let start = std::time::Instant::now();
    let result = req.send().await;
    let latency_ms = start.elapsed().as_millis() as u64;

    let (valid, error, status) = match result {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let ok = resp.status().is_success();
            let err = if ok {
                None
            } else {
                Some(format!("HTTP {status}"))
            };
            (ok, err, Some(status))
        }
        Err(e) => (false, Some(e.to_string()), None),
    };

    let payload = json!({
        "id": node.id,
        "name": node.name,
        "type": node.r#type,
        "baseUrl": base_url,
        "probeUrl": probe_url,
        "valid": valid,
        "status": status,
        "latencyMs": latency_ms,
        "error": error,
    });

    if ctx.is_robot() {
        emit_robot("openproxy.v1.provider-node.validate", payload)?;
    } else if valid {
        humanln(
            ctx,
            format!("OK  {} ({}ms) — {probe_url}", node.name, latency_ms),
        );
    } else {
        humanln(
            ctx,
            format!(
                "FAIL {} ({}ms) — {} — {}",
                node.name,
                latency_ms,
                probe_url,
                error.as_deref().unwrap_or("unknown error"),
            ),
        );
    }

    // We intentionally always return Ok here — the failure is encoded in
    // the envelope. Use `--robot` and parse `valid` to fail in scripts.
    let _ = ctx;
    let _ = Value::Null;
    Ok(())
}
