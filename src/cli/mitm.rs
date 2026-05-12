//! `openproxy mitm *` — manage the embedded MITM router (PLAN v3 mục 4.10).
//!
//! All commands talk to the running server's `/api/mitm/*` and
//! `/api/mitm-config` endpoints. The server is the source of truth for MITM
//! state (cert metadata, active routes); the CLI just adds the agent-friendly
//! envelope and bulk-apply ergonomics.

use std::path::PathBuf;

use clap::Subcommand;
use serde_json::{json, Map, Value};

use crate::cli::config::ResolvedConfig;
use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};
use crate::cli::runtime::{read_input, require_runtime, rt_error_to_exit, Runtime};

#[derive(Debug, Clone, Subcommand)]
pub enum MitmCmd {
    /// Show whether MITM is enabled, route count, and cert status.
    Status,
    /// Activate every configured MITM route (server-side reload).
    Start {
        /// Currently ignored — the server uses the configured router port.
        #[arg(long, default_value_t = 8080)]
        port: u16,
    },
    /// Deactivate all MITM routes (drops `mitm_alias`).
    Stop,
    /// Certificate management.
    Cert {
        #[command(subcommand)]
        cmd: CertCmd,
    },
    /// Read/write MITM configuration (routes + per-tool settings).
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum CertCmd {
    /// Generate a fresh CA certificate stored on the server.
    Generate,
    /// Print the local path where the CLI stores its cert copy.
    Path,
}

#[derive(Debug, Clone, Subcommand)]
pub enum ConfigCmd {
    /// Fetch the current MITM config (router + routes + per-tool).
    Get,
    /// Set a single key on a single route. `key` is a dotted path under the
    /// route, e.g. `routes.claude.upstreamUrl`.
    Set {
        #[arg(long)]
        key: String,
        #[arg(long)]
        value: String,
    },
    /// Bulk-apply a JSON document matching the PUT `/api/mitm-config` shape.
    Apply {
        /// Path to a JSON file, or `-` to read stdin.
        #[arg(long = "from-file", default_value = "-")]
        from_file: String,
    },
}

pub async fn run(cmd: MitmCmd, cfg: &ResolvedConfig, ctx: OutputCtx) -> anyhow::Result<i32> {
    let rt = match require_runtime(cfg).await {
        Ok(rt) => rt,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    match cmd {
        MitmCmd::Status => run_status(&rt, ctx).await,
        MitmCmd::Start { .. } => run_start(&rt, ctx).await,
        MitmCmd::Stop => run_stop(&rt, ctx).await,
        MitmCmd::Cert { cmd } => match cmd {
            CertCmd::Generate => run_cert_generate(&rt, ctx).await,
            CertCmd::Path => run_cert_path(cfg, ctx),
        },
        MitmCmd::Config { cmd } => match cmd {
            ConfigCmd::Get => run_config_get(&rt, ctx).await,
            ConfigCmd::Set { key, value } => run_config_set(&rt, ctx, key, value).await,
            ConfigCmd::Apply { from_file } => run_config_apply(&rt, ctx, &from_file).await,
        },
    }
}

async fn run_status(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.get_json("/api/mitm-config").await {
        Ok(payload) => {
            let enabled = payload
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let routes = payload
                .get("routes")
                .and_then(Value::as_object)
                .map(|m| m.len())
                .unwrap_or(0);
            let cert = payload
                .get("certStatus")
                .or_else(|| payload.get("cert_status"))
                .cloned()
                .unwrap_or(Value::Null);
            if ctx.is_robot() {
                emit_robot(
                    "openproxy.v1.mitm.status",
                    json!({
                        "enabled": enabled,
                        "routes": routes,
                        "cert": cert,
                    }),
                )?;
            } else {
                humanln(
                    ctx,
                    format!(
                        "MITM: {} ({} route{})",
                        if enabled { "enabled" } else { "disabled" },
                        routes,
                        if routes == 1 { "" } else { "s" }
                    ),
                );
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_start(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.post_empty("/api/mitm/start").await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.mitm.start", payload)?;
            } else {
                humanln(ctx, "MITM proxy started.");
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_stop(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.post_empty("/api/mitm/stop").await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.mitm.stop", payload)?;
            } else {
                humanln(ctx, "MITM proxy stopped.");
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_cert_generate(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.post_empty("/api/mitm/cert/generate").await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.mitm.cert.generate", payload)?;
            } else {
                let fp = payload
                    .get("fingerprint")
                    .and_then(Value::as_str)
                    .unwrap_or("?");
                humanln(ctx, format!("New MITM cert generated. fingerprint={fp}"));
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

fn run_cert_path(cfg: &ResolvedConfig, ctx: OutputCtx) -> anyhow::Result<i32> {
    let path = local_cert_path(&cfg.data_dir);
    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.mitm.cert.path",
            json!({"path": path.to_string_lossy()}),
        )?;
    } else {
        println!("{}", path.display());
    }
    Ok(0)
}

/// Local path where `openproxy mitm cert` reads/writes the CA bundle copy.
/// The server owns the canonical cert; this is the agent-friendly export
/// location.
fn local_cert_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("mitm-ca.pem")
}

async fn run_config_get(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.get_json("/api/mitm-config").await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.mitm.config", payload)?;
            } else {
                humanln(
                    ctx,
                    serde_json::to_string_pretty(&payload).unwrap_or_default(),
                );
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_config_set(
    rt: &Runtime,
    ctx: OutputCtx,
    key: String,
    value: String,
) -> anyhow::Result<i32> {
    // The server PUT shape is `{routerBaseUrl?, routes?: {name: route}}`.
    // We expose two kinds of writes:
    //   --key routerBaseUrl --value http://...   (top-level)
    //   --key routes.<name>.<field> --value ...  (per-route field)
    let parts: Vec<&str> = key.split('.').collect();
    let body = if parts.len() == 1 && parts[0] == "routerBaseUrl" {
        json!({"routerBaseUrl": value})
    } else if parts.len() == 3 && parts[0] == "routes" {
        let name = parts[1].to_string();
        let field = parts[2].to_string();
        let mut entry = Map::new();
        // upstreamUrl is required by the server PUT — accept it as the
        // primary write and forward any other field as-is.
        if field == "upstreamUrl" {
            entry.insert("upstreamUrl".to_string(), Value::String(value));
        } else {
            // The server requires upstreamUrl on every route entry; load
            // the current one first to avoid clearing it.
            let current = rt.get_json("/api/mitm-config").await;
            let upstream = match &current {
                Ok(v) => v
                    .get("routes")
                    .and_then(|r| r.get(&name))
                    .and_then(|r| r.get("upstreamUrl").or_else(|| r.get("upstream_url")))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                Err(_) => String::new(),
            };
            entry.insert("upstreamUrl".to_string(), Value::String(upstream));
            let coerced = coerce_value(&field, &value);
            entry.insert(field, coerced);
        }
        let mut routes = Map::new();
        routes.insert(name, Value::Object(entry));
        json!({ "routes": routes })
    } else {
        return Ok(emit_error(
            ctx,
            "usage",
            "key must be `routerBaseUrl` or `routes.<name>.<field>`",
        )?);
    };

    match rt.put_json("/api/mitm-config", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.mitm.config.set", payload)?;
            } else {
                humanln(ctx, format!("Updated `{key}`."));
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_config_apply(rt: &Runtime, ctx: OutputCtx, from: &str) -> anyhow::Result<i32> {
    let text = read_input(from)?;
    let body: Value = serde_json::from_str(text.trim()).map_err(|e| {
        anyhow::anyhow!("--from-file must be JSON matching the PUT /api/mitm-config shape: {e}")
    })?;
    match rt.put_json("/api/mitm-config", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.mitm.config.apply", payload)?;
            } else {
                humanln(ctx, "MITM config applied.");
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

/// Best-effort coercion of `--value` strings into typed JSON values for the
/// fields the server understands (`enabled`, `requestTransform`,
/// `responseTransform` are booleans).
fn coerce_value(field: &str, value: &str) -> Value {
    let lower = value.to_ascii_lowercase();
    let is_bool_field = matches!(
        field,
        "enabled"
            | "requestTransform"
            | "responseTransform"
            | "request_transform"
            | "response_transform"
    );
    if is_bool_field {
        match lower.as_str() {
            "true" | "yes" | "1" | "on" => return Value::Bool(true),
            "false" | "no" | "0" | "off" => return Value::Bool(false),
            _ => {}
        }
    }
    Value::String(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coerce_value_handles_booleans() {
        assert_eq!(coerce_value("enabled", "true"), Value::Bool(true));
        assert_eq!(coerce_value("requestTransform", "0"), Value::Bool(false));
        assert_eq!(
            coerce_value("pathPrefix", "/api"),
            Value::String("/api".to_string())
        );
    }

    #[test]
    fn local_cert_path_lives_in_data_dir() {
        let p = local_cert_path(std::path::Path::new("/tmp/op"));
        assert!(p.ends_with("mitm-ca.pem"));
    }
}
