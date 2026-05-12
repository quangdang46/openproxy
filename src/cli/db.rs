//! `openproxy db *` — database lifecycle + cloud sync (PLAN v3 mục 4.17, 4.18).
//!
//! These commands manage the on-disk `db.json` snapshot the server reads:
//! - `db init` / `db export` / `db import` operate on the snapshot itself.
//! - `db dump --resource <r>` extracts a single collection from the export.
//! - `db migrate` is a no-op stub today (no schema migrations are owned by
//!   the CLI yet; the server normalizes on load).
//! - `db cloud *` talks to `/api/cloud/*` for the bearer-authenticated
//!   cloud-sync surface (auth, credentials update, alias list/set, resolve).
//!
//! Every command emits an `openproxy.v1.db.*` envelope in `--robot` mode and
//! exits with code 6 if the server is unreachable.

use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};
use serde_json::{json, Map, Value};

use crate::cli::config::ResolvedConfig;
use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};
use crate::cli::runtime::{require_runtime, rt_error_to_exit, Runtime};

#[derive(Debug, Clone, Subcommand)]
pub enum DbCmd {
    /// Confirm the server's `db.json` is initialized. Useful as a readiness
    /// probe in scripts that have just called `server init`.
    Init,
    /// Download the full server snapshot. With `--out <path>` writes the
    /// pretty-printed JSON to disk; otherwise the JSON is emitted as the
    /// envelope's `data`.
    Export {
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Upload a snapshot to the server. The server's import is *merge by
    /// default*: only collections explicitly present in the file overwrite
    /// the server-side data. `--replace` is reserved for the future when the
    /// server learns a destructive overwrite mode; today it errors out so
    /// scripts don't silently fall back to merge.
    Import {
        /// Path to the JSON snapshot (or `-` for stdin).
        path: String,
        /// Default. Only overwrite collections present in the import.
        #[arg(long, conflicts_with = "replace")]
        merge: bool,
        /// Reserved for a future destructive overwrite mode. Errors today.
        #[arg(long)]
        replace: bool,
    },
    /// Print a single resource slice from the server's snapshot.
    Dump {
        #[arg(long, value_enum)]
        resource: DumpResource,
    },
    /// Stub. The server normalizes on load; no CLI migration step is
    /// required today. Returns a no-op envelope so scripts can call it
    /// unconditionally.
    Migrate,
    /// Cloud sync helpers (`/api/cloud/*`).
    Cloud {
        #[command(subcommand)]
        cmd: CloudCmd,
    },
}

#[derive(Debug, Clone, ValueEnum)]
pub enum DumpResource {
    Providers,
    Nodes,
    Combos,
    Keys,
    Pools,
    Models,
    Aliases,
    Pricing,
    Settings,
}

impl DumpResource {
    fn key(&self) -> &'static str {
        match self {
            DumpResource::Providers => "providerConnections",
            DumpResource::Nodes => "providerNodes",
            DumpResource::Combos => "combos",
            DumpResource::Keys => "apiKeys",
            DumpResource::Pools => "proxyPools",
            DumpResource::Models => "customModels",
            DumpResource::Aliases => "modelAliases",
            DumpResource::Pricing => "pricing",
            DumpResource::Settings => "settings",
        }
    }

    fn slug(&self) -> &'static str {
        match self {
            DumpResource::Providers => "providers",
            DumpResource::Nodes => "nodes",
            DumpResource::Combos => "combos",
            DumpResource::Keys => "keys",
            DumpResource::Pools => "pools",
            DumpResource::Models => "models",
            DumpResource::Aliases => "aliases",
            DumpResource::Pricing => "pricing",
            DumpResource::Settings => "settings",
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
pub enum CloudCmd {
    /// Dump cloud-visible connections + model aliases.
    Auth,
    /// Push refreshed OAuth credentials to the server for one provider.
    Sync {
        /// Provider alias whose credentials are being refreshed.
        #[arg(long)]
        provider: String,
        /// New access token.
        #[arg(long, hide_env_values = true)]
        access_token: String,
        /// Optional refresh token.
        #[arg(long, hide_env_values = true)]
        refresh_token: Option<String>,
        /// Token lifetime, in seconds.
        #[arg(long)]
        expires_in: Option<i64>,
    },
    /// Resolve a model alias to its `{provider, model}` target.
    Resolve {
        /// Alias name configured in the server's model-alias table.
        #[arg(long)]
        alias: String,
    },
    /// Model alias management (`/api/cloud/models/alias`).
    Alias {
        #[command(subcommand)]
        cmd: AliasCmd,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum AliasCmd {
    /// List all `alias -> provider/model` mappings.
    List,
    /// Insert or update an alias.
    Set {
        /// Alias name (e.g. `gpt-5`).
        #[arg(long)]
        alias: String,
        /// Target as `provider/model` (e.g. `openai/gpt-5-mini`).
        #[arg(long)]
        model: String,
    },
}

pub async fn run(cmd: DbCmd, cfg: &ResolvedConfig, ctx: OutputCtx) -> anyhow::Result<i32> {
    let rt = match require_runtime(cfg).await {
        Ok(rt) => rt,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    match cmd {
        DbCmd::Init => run_init(&rt, ctx).await,
        DbCmd::Export { out } => run_export(&rt, ctx, out).await,
        DbCmd::Import {
            path,
            merge,
            replace,
        } => run_import(&rt, ctx, path, merge, replace).await,
        DbCmd::Dump { resource } => run_dump(&rt, ctx, resource).await,
        DbCmd::Migrate => run_migrate(ctx),
        DbCmd::Cloud { cmd } => run_cloud(&rt, ctx, cmd).await,
    }
}

async fn run_init(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    // GET /api/init returns plain text "Initialized"; we just confirm 200 OK
    // and emit a normalized envelope. Use `request` + status check rather
    // than `get_json` because the body is not JSON.
    let url = format!("{}/api/init", rt.base_url());
    let res = reqwest::Client::new().get(&url).send().await;
    match res {
        Ok(r) if r.status().is_success() => {
            let body = r.text().await.unwrap_or_default();
            if ctx.is_robot() {
                emit_robot(
                    "openproxy.v1.db.init",
                    json!({"initialized": true, "message": body}),
                )?;
            } else {
                humanln(ctx, format!("db initialized: {body}"));
            }
            Ok(0)
        }
        Ok(r) => Ok(emit_error(
            ctx,
            "other",
            &format!("/api/init returned HTTP {}", r.status()),
        )?),
        Err(e) => Ok(emit_error(ctx, "server_unreachable", &e.to_string())?),
    }
}

async fn run_export(rt: &Runtime, ctx: OutputCtx, out: Option<PathBuf>) -> anyhow::Result<i32> {
    match rt.get_json("/api/db/export").await {
        Ok(payload) => {
            if let Some(path) = out {
                let pretty = serde_json::to_string_pretty(&payload).unwrap_or_default();
                if let Err(e) = std::fs::write(&path, pretty.as_bytes()) {
                    return Ok(emit_error(
                        ctx,
                        "other",
                        &format!("write {}: {}", path.display(), e),
                    )?);
                }
                let bytes = pretty.len();
                if ctx.is_robot() {
                    emit_robot(
                        "openproxy.v1.db.export",
                        json!({"written": path.to_string_lossy(), "bytes": bytes}),
                    )?;
                } else {
                    humanln(ctx, format!("exported {bytes} bytes to {}", path.display()));
                }
            } else if ctx.is_robot() {
                emit_robot("openproxy.v1.db.export", payload)?;
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload).unwrap_or_default()
                );
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_import(
    rt: &Runtime,
    ctx: OutputCtx,
    path: String,
    _merge: bool,
    replace: bool,
) -> anyhow::Result<i32> {
    if replace {
        return Ok(emit_error(
            ctx,
            "usage",
            "--replace is reserved for a future destructive overwrite mode; today only --merge is supported",
        )?);
    }
    let raw = match read_input(&path) {
        Ok(s) => s,
        Err(e) => {
            return Ok(emit_error(
                ctx,
                "usage",
                &format!("cannot read {path}: {e}"),
            )?);
        }
    };
    let body: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            return Ok(emit_error(
                ctx,
                "validation",
                &format!("import file is not valid JSON: {e}"),
            )?);
        }
    };
    if !body.is_object() {
        return Ok(emit_error(
            ctx,
            "validation",
            "import payload must be a JSON object",
        )?);
    }

    match rt.post_json("/api/settings/database", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot(
                    "openproxy.v1.db.import",
                    json!({"mode": "merge", "result": payload}),
                )?;
            } else {
                humanln(ctx, "db import: ok (merge)");
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_dump(rt: &Runtime, ctx: OutputCtx, resource: DumpResource) -> anyhow::Result<i32> {
    match rt.get_json("/api/db/export").await {
        Ok(payload) => {
            let slice = payload.get(resource.key()).cloned().unwrap_or(Value::Null);
            if ctx.is_robot() {
                emit_robot(
                    "openproxy.v1.db.dump",
                    json!({"resource": resource.slug(), "data": slice}),
                )?;
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&slice).unwrap_or_default()
                );
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

fn run_migrate(ctx: OutputCtx) -> anyhow::Result<i32> {
    // No CLI-owned migrations exist yet; the server normalizes settings on
    // load. Emit a deterministic envelope so callers can rely on the
    // command existing and exiting 0.
    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.db.migrate",
            json!({"applied": 0, "note": "server normalizes on load; no CLI migrations registered"}),
        )?;
    } else {
        humanln(ctx, "no migrations to run.");
    }
    Ok(0)
}

async fn run_cloud(rt: &Runtime, ctx: OutputCtx, cmd: CloudCmd) -> anyhow::Result<i32> {
    match cmd {
        CloudCmd::Auth => run_cloud_auth(rt, ctx).await,
        CloudCmd::Sync {
            provider,
            access_token,
            refresh_token,
            expires_in,
        } => run_cloud_sync(rt, ctx, provider, access_token, refresh_token, expires_in).await,
        CloudCmd::Resolve { alias } => run_cloud_resolve(rt, ctx, alias).await,
        CloudCmd::Alias { cmd } => match cmd {
            AliasCmd::List => run_alias_list(rt, ctx).await,
            AliasCmd::Set { alias, model } => run_alias_set(rt, ctx, alias, model).await,
        },
    }
}

async fn run_cloud_auth(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    // Server expects a POST body; an empty object is fine.
    match rt.post_json("/api/cloud/auth", &json!({})).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.db.cloud.auth", payload)?;
            } else {
                let conns = payload
                    .get("connections")
                    .and_then(Value::as_array)
                    .map(|a| a.len())
                    .unwrap_or(0);
                let aliases = payload
                    .get("modelAliases")
                    .and_then(Value::as_object)
                    .map(|o| o.len())
                    .unwrap_or(0);
                humanln(
                    ctx,
                    format!("cloud auth: {conns} active connection(s), {aliases} alias(es)"),
                );
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_cloud_sync(
    rt: &Runtime,
    ctx: OutputCtx,
    provider: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
) -> anyhow::Result<i32> {
    let mut creds = Map::new();
    creds.insert("accessToken".to_string(), Value::String(access_token));
    if let Some(rt_) = refresh_token {
        creds.insert("refreshToken".to_string(), Value::String(rt_));
    }
    if let Some(s) = expires_in {
        creds.insert(
            "expiresIn".to_string(),
            Value::Number(serde_json::Number::from(s)),
        );
    }
    let body = json!({
        "provider": provider,
        "credentials": Value::Object(creds),
    });
    match rt.put_json("/api/cloud/credentials/update", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot(
                    "openproxy.v1.db.cloud.sync",
                    json!({"provider": provider, "result": payload}),
                )?;
            } else {
                humanln(ctx, format!("cloud sync ok for {provider}"));
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_cloud_resolve(rt: &Runtime, ctx: OutputCtx, alias: String) -> anyhow::Result<i32> {
    let body = json!({"alias": alias});
    match rt.post_json("/api/cloud/model/resolve", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.db.cloud.resolve", payload)?;
            } else {
                let provider = payload
                    .get("provider")
                    .and_then(Value::as_str)
                    .unwrap_or("?");
                let model = payload.get("model").and_then(Value::as_str).unwrap_or("?");
                humanln(ctx, format!("{alias} -> {provider}/{model}"));
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_alias_list(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.get_json("/api/cloud/models/alias").await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.db.cloud.alias.list", payload)?;
            } else {
                let aliases = payload.get("aliases").cloned().unwrap_or(Value::Null);
                println!(
                    "{}",
                    serde_json::to_string_pretty(&aliases).unwrap_or_default()
                );
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_alias_set(
    rt: &Runtime,
    ctx: OutputCtx,
    alias: String,
    model: String,
) -> anyhow::Result<i32> {
    let body = json!({"alias": alias, "model": model});
    match rt.put_json("/api/cloud/models/alias", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.db.cloud.alias.set", payload)?;
            } else {
                humanln(ctx, format!("alias set: {alias} -> {model}"));
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

fn read_input(spec: &str) -> std::io::Result<String> {
    use std::io::Read;
    if spec == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        Ok(buf)
    } else {
        std::fs::read_to_string(PathBuf::from(spec))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_resource_keys_match_db_layout() {
        assert_eq!(DumpResource::Providers.key(), "providerConnections");
        assert_eq!(DumpResource::Nodes.key(), "providerNodes");
        assert_eq!(DumpResource::Keys.key(), "apiKeys");
        assert_eq!(DumpResource::Settings.key(), "settings");
    }

    #[test]
    fn dump_resource_slugs_are_lowercase() {
        for r in [
            DumpResource::Providers,
            DumpResource::Nodes,
            DumpResource::Combos,
            DumpResource::Keys,
            DumpResource::Pools,
            DumpResource::Models,
            DumpResource::Aliases,
            DumpResource::Pricing,
            DumpResource::Settings,
        ] {
            let s = r.slug();
            assert!(
                s.chars().all(|c| c.is_ascii_lowercase()),
                "non-lowercase slug: {s}"
            );
        }
    }
}
