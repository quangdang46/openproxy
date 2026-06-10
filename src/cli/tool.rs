//! `openproxy tool *` — manage CLI-tool integrations (claude, codex, copilot,
//! openclaw, hermes, cowork) via `/api/cli-tools/*` (PLAN v3 mục 4.12).
//!
//! `apply <name>` writes a tool's settings (model/api-key/base-url) by POSTing
//! to the per-tool `/<name>-settings` endpoint. `revert <name>` resets it via
//! DELETE on the same endpoint. `execute` runs an arbitrary shell command via
//! the server. `run` invokes one of the named tools (`provider-list`, etc.).

use clap::Subcommand;
use serde_json::{json, Map, Value};

use crate::cli::config::ResolvedConfig;
use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};
use crate::cli::runtime::{require_runtime, rt_error_to_exit, Runtime};

#[derive(Debug, Clone, Subcommand)]
pub enum ToolCmd {
    /// List available built-in CLI tools (provider-list, key-list, ...).
    List,
    /// Show the saved settings for a per-tool integration.
    Show {
        /// One of `claude`, `codex`, `copilot`, `openclaw`, `hermes`, `cowork`.
        name: String,
    },
    /// Apply settings to a per-tool integration. Use `--dry-run` to preview
    /// the JSON body without sending it.
    Apply {
        /// One of `claude`, `codex`, `copilot`, `openclaw`, `hermes`, `cowork`.
        name: String,
        /// Model id to set (passed as `model` to the server).
        #[arg(long)]
        model: Option<String>,
        /// API key to write (often the OpenProxy key, not a provider key).
        #[arg(long, hide_env_values = true)]
        api_key: Option<String>,
        /// Base URL (defaults to the running server's URL).
        #[arg(long)]
        endpoint: Option<String>,
        /// Print the JSON we would POST without actually sending it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Reset / revert a per-tool integration (DELETE the saved settings).
    Revert { name: String },
    /// Execute an arbitrary command via `/api/cli-tools/execute`.
    Execute {
        /// Command and arguments. Pass `--` before args containing flags.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        argv: Vec<String>,
    },
    /// Run a named built-in tool (`provider-list`, `key-list`, ...).
    Run {
        name: String,
        /// Trailing arguments forwarded to the tool.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        argv: Vec<String>,
    },
    /// Show help index from the server.
    Doc,
    /// Antigravity MITM helpers.
    AntigravityMitm {
        #[command(subcommand)]
        cmd: AntigravityCmd,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum AntigravityCmd {
    /// Enable antigravity MITM (optionally aliased).
    Enable {
        /// Optional alias name to register.
        #[arg(long)]
        alias: Option<String>,
    },
    /// Disable antigravity MITM (and any alias).
    Disable {
        /// Optional alias name to remove.
        #[arg(long)]
        alias: Option<String>,
    },
}

pub async fn run(cmd: ToolCmd, cfg: &ResolvedConfig, ctx: OutputCtx) -> anyhow::Result<i32> {
    let rt = match require_runtime(cfg).await {
        Ok(rt) => rt,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    match cmd {
        ToolCmd::List => run_list(&rt, ctx).await,
        ToolCmd::Show { name } => run_show(&rt, ctx, &name).await,
        ToolCmd::Apply {
            name,
            model,
            api_key,
            endpoint,
            dry_run,
        } => run_apply(&rt, ctx, &name, model, api_key, endpoint, dry_run).await,
        ToolCmd::Revert { name } => run_revert(&rt, ctx, &name).await,
        ToolCmd::Execute { argv } => run_execute(&rt, ctx, argv).await,
        ToolCmd::Run { name, argv } => run_run(&rt, ctx, &name, argv).await,
        ToolCmd::Doc => run_help(&rt, ctx).await,
        ToolCmd::AntigravityMitm { cmd } => match cmd {
            AntigravityCmd::Enable { alias } => run_antigravity(&rt, ctx, true, alias).await,
            AntigravityCmd::Disable { alias } => run_antigravity(&rt, ctx, false, alias).await,
        },
    }
}

/// Supported per-tool integration names. The server endpoint is
/// `/api/cli-tools/<name>-settings`.
const SUPPORTED_TOOLS: &[&str] = &[
    "claude", "codex", "copilot", "openclaw", "hermes", "cowork", "opencode", "droid",
];

fn settings_path(name: &str) -> Option<String> {
    let lowered = name.to_ascii_lowercase();
    if SUPPORTED_TOOLS.contains(&lowered.as_str()) {
        Some(format!("/api/cli-tools/{}-settings", lowered))
    } else {
        None
    }
}

async fn run_list(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.get_json("/api/cli-tools").await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.tool.list", payload)?;
            } else {
                let tools = payload
                    .get("tools")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for tool in &tools {
                    humanln(
                        ctx,
                        format!(
                            "{:24} [{}]  {}",
                            tool.get("name").and_then(Value::as_str).unwrap_or("?"),
                            tool.get("category").and_then(Value::as_str).unwrap_or("-"),
                            tool.get("description")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                        ),
                    );
                }
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_show(rt: &Runtime, ctx: OutputCtx, name: &str) -> anyhow::Result<i32> {
    let Some(path) = settings_path(name) else {
        return Ok(emit_error(
            ctx,
            "usage",
            &format!(
                "unknown tool `{name}` — expected one of: {}",
                SUPPORTED_TOOLS.join(", ")
            ),
        )?);
    };
    match rt.get_json(&path).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.tool.show", payload)?;
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

async fn run_apply(
    rt: &Runtime,
    ctx: OutputCtx,
    name: &str,
    model: Option<String>,
    api_key: Option<String>,
    endpoint: Option<String>,
    dry_run: bool,
) -> anyhow::Result<i32> {
    let Some(path) = settings_path(name) else {
        return Ok(emit_error(
            ctx,
            "usage",
            &format!(
                "unknown tool `{name}` — expected one of: {}",
                SUPPORTED_TOOLS.join(", ")
            ),
        )?);
    };
    let body = build_apply_body(name, &model, api_key.as_deref(), endpoint.as_deref());

    if dry_run {
        let preview = json!({"path": path, "body": body});
        if ctx.is_robot() {
            emit_robot("openproxy.v1.tool.apply.dry_run", preview)?;
        } else {
            humanln(ctx, "[dry-run] POST {path}".replace("{path}", &path));
            humanln(ctx, serde_json::to_string_pretty(&body).unwrap_or_default());
        }
        return Ok(0);
    }

    match rt.post_json(&path, &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.tool.apply", payload)?;
            } else {
                humanln(ctx, format!("Applied settings for `{name}`."));
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

/// Build the per-tool POST body. Each tool has a slightly different shape;
/// we cover the agent-friendly common case (model + api_key + base_url).
pub(crate) fn build_apply_body(
    name: &str,
    model: &Option<String>,
    api_key: Option<&str>,
    endpoint: Option<&str>,
) -> Value {
    let lower = name.to_ascii_lowercase();
    let default_url = "http://127.0.0.1:4623";
    let base_url = endpoint.unwrap_or(default_url);
    match lower.as_str() {
        "claude" => {
            // /api/cli-tools/claude-settings expects {env: {ANTHROPIC_BASE_URL,...}}.
            let mut env = Map::new();
            env.insert(
                "ANTHROPIC_BASE_URL".to_string(),
                Value::String(base_url.to_string()),
            );
            if let Some(key) = api_key {
                env.insert(
                    "ANTHROPIC_AUTH_TOKEN".to_string(),
                    Value::String(key.to_string()),
                );
            }
            if let Some(m) = model.as_deref() {
                env.insert("ANTHROPIC_MODEL".to_string(), Value::String(m.to_string()));
            }
            json!({"env": Value::Object(env)})
        }
        "codex" | "opencode" | "droid" => json!({
            "baseUrl": base_url,
            "apiKey": api_key.unwrap_or(""),
            "model": model.clone().unwrap_or_default(),
        }),
        "copilot" => json!({
            "baseUrl": base_url,
            "apiKey": api_key,
            "models": model
                .as_ref()
                .map(|m| vec![m.clone()])
                .unwrap_or_default(),
        }),
        // hermes / cowork / openclaw — `{baseUrl, apiKey, model|models}`.
        _ => {
            let mut obj = Map::new();
            obj.insert("baseUrl".to_string(), Value::String(base_url.to_string()));
            if let Some(k) = api_key {
                obj.insert("apiKey".to_string(), Value::String(k.to_string()));
            }
            if let Some(m) = model.as_deref() {
                obj.insert("model".to_string(), Value::String(m.to_string()));
                obj.insert(
                    "models".to_string(),
                    Value::Array(vec![Value::String(m.to_string())]),
                );
            }
            Value::Object(obj)
        }
    }
}

async fn run_revert(rt: &Runtime, ctx: OutputCtx, name: &str) -> anyhow::Result<i32> {
    let Some(path) = settings_path(name) else {
        return Ok(emit_error(ctx, "usage", &format!("unknown tool `{name}`"))?);
    };
    match rt.delete_json(&path).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.tool.revert", payload)?;
            } else {
                humanln(ctx, format!("Reverted `{name}` settings."));
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_execute(rt: &Runtime, ctx: OutputCtx, argv: Vec<String>) -> anyhow::Result<i32> {
    if argv.is_empty() {
        return Ok(emit_error(ctx, "usage", "execute requires a command")?);
    }
    let mut iter = argv.into_iter();
    let command = iter.next().unwrap_or_default();
    let rest: Vec<String> = iter.collect();
    let body = json!({
        "command": command,
        "args": rest,
    });
    match rt.post_json("/api/cli-tools/execute", &body).await {
        Ok(payload) => emit_command_result(ctx, "openproxy.v1.tool.execute", payload),
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_run(
    rt: &Runtime,
    ctx: OutputCtx,
    name: &str,
    argv: Vec<String>,
) -> anyhow::Result<i32> {
    let path = format!("/api/cli-tools/run/{}", urlencoding::encode(name));
    let body = json!({
        "command": name,
        "args": argv,
    });
    match rt.post_json(&path, &body).await {
        Ok(payload) => emit_command_result(ctx, "openproxy.v1.tool.run", payload),
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

fn emit_command_result(ctx: OutputCtx, schema: &str, payload: Value) -> anyhow::Result<i32> {
    let exit = payload
        .get("exit_code")
        .or_else(|| payload.get("exitCode"))
        .and_then(Value::as_i64)
        .map(|c| c as i32)
        .unwrap_or(0);
    if ctx.is_robot() {
        emit_robot(schema, payload)?;
    } else {
        let stdout = payload.get("stdout").and_then(Value::as_str).unwrap_or("");
        let stderr = payload.get("stderr").and_then(Value::as_str).unwrap_or("");
        if !stdout.is_empty() {
            println!("{stdout}");
        }
        if !stderr.is_empty() {
            eprintln!("{stderr}");
        }
    }
    Ok(exit)
}

async fn run_help(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.get_json("/api/cli-tools/help").await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.tool.help", payload)?;
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

async fn run_antigravity(
    rt: &Runtime,
    ctx: OutputCtx,
    enable: bool,
    alias: Option<String>,
) -> anyhow::Result<i32> {
    let (alias_method, alias_body) = match (&alias, enable) {
        (Some(name), true) => (Some("put"), json!({"alias": name})),
        (Some(_), false) => (Some("delete"), Value::Null),
        (None, _) => (None, Value::Null),
    };

    // First, toggle the underlying integration.
    let core_result = if enable {
        rt.post_empty("/api/cli-tools/antigravity-mitm").await
    } else {
        rt.delete_json("/api/cli-tools/antigravity-mitm").await
    };
    let core_payload = match core_result {
        Ok(v) => v,
        Err(e) => return rt_error_to_exit(ctx, e),
    };

    // Then, optionally adjust the alias.
    let alias_payload = match alias_method {
        Some("put") => match rt
            .put_json("/api/cli-tools/antigravity-mitm/alias", &alias_body)
            .await
        {
            Ok(v) => Some(v),
            Err(e) => return rt_error_to_exit(ctx, e),
        },
        Some("delete") => match rt
            .delete_json("/api/cli-tools/antigravity-mitm/alias")
            .await
        {
            Ok(v) => Some(v),
            Err(e) => return rt_error_to_exit(ctx, e),
        },
        _ => None,
    };

    let combined = json!({
        "core": core_payload,
        "alias": alias_payload,
        "enabled": enable,
    });
    let schema = if enable {
        "openproxy.v1.tool.antigravity.enable"
    } else {
        "openproxy.v1.tool.antigravity.disable"
    };
    if ctx.is_robot() {
        emit_robot(schema, combined)?;
    } else {
        humanln(
            ctx,
            format!(
                "Antigravity MITM {}{}.",
                if enable { "enabled" } else { "disabled" },
                alias.map(|a| format!(" (alias `{a}`)")).unwrap_or_default(),
            ),
        );
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_path_known_tools() {
        assert_eq!(
            settings_path("Claude"),
            Some("/api/cli-tools/claude-settings".to_string())
        );
        assert_eq!(
            settings_path("hermes"),
            Some("/api/cli-tools/hermes-settings".to_string())
        );
        assert!(settings_path("bogus").is_none());
    }

    #[test]
    fn build_apply_body_claude_uses_env_block() {
        let body = build_apply_body(
            "claude",
            &Some("sonnet-4".to_string()),
            Some("op_key"),
            Some("http://localhost:1234"),
        );
        let env = body.get("env").unwrap();
        assert_eq!(
            env.get("ANTHROPIC_BASE_URL").and_then(Value::as_str),
            Some("http://localhost:1234")
        );
        assert_eq!(
            env.get("ANTHROPIC_AUTH_TOKEN").and_then(Value::as_str),
            Some("op_key")
        );
    }

    #[test]
    fn build_apply_body_codex_uses_flat_shape() {
        let body = build_apply_body(
            "codex",
            &Some("gpt-4o-mini".to_string()),
            Some("op_key"),
            None,
        );
        assert_eq!(
            body.get("model").and_then(Value::as_str),
            Some("gpt-4o-mini")
        );
        assert_eq!(
            body.get("baseUrl").and_then(Value::as_str),
            Some("http://127.0.0.1:4623")
        );
        assert_eq!(body.get("apiKey").and_then(Value::as_str), Some("op_key"));
    }
}
