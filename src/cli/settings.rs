//! `openproxy settings *` — manage server settings, locale, version, and self-update.
//!
//! These commands all run against the live server and emit the
//! `openproxy.v1.settings.*` envelopes in `--robot` mode. They are the M6
//! follow-on to the M5 tooling integration commands.
//!
//! Endpoints exercised here:
//!
//! | Subcommand                              | Route                            | Method |
//! | --------------------------------------- | -------------------------------- | ------ |
//! | `settings get [--key <path>]`           | `/api/settings`                  | GET    |
//! | `settings set --key <k> --value <v>`    | `/api/settings`                  | PATCH  |
//! | `settings apply --from-file <path\|->`  | `/api/settings`                  | PUT    |
//! | `settings proxy-test --proxy-url <url>` | `/api/settings/proxy-test`       | POST   |
//! | `settings locale set <lang>`            | `/api/locale`                    | POST   |
//! | `settings version`                      | `/api/version`                   | GET    |
//! | `settings update [--check] [--apply]`   | `/api/version`/`/api/version/update` | GET/POST |

use std::path::PathBuf;

use clap::Subcommand;
use serde_json::{json, Map, Value};

use crate::cli::config::ResolvedConfig;
use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};
use crate::cli::runtime::{require_runtime, rt_error_to_exit, Runtime};

#[derive(Debug, Clone, Subcommand)]
pub enum SettingsCmd {
    /// Show the server's settings document. With `--key <dot.path>` print a
    /// single field's value (string in human mode, `{key, value}` envelope
    /// in robot mode).
    Get {
        /// Dotted path inside the settings document (e.g. `comboStrategy`).
        #[arg(long)]
        key: Option<String>,
    },
    /// Update a single field on the server's settings document.
    /// Values are auto-coerced: `true`/`false` ⇒ bool, integers ⇒ number,
    /// otherwise pass-through as a string.
    Set {
        /// camelCase field name on the settings document
        /// (e.g. `comboStrategy`, `rtkEnabled`, `outboundProxyUrl`).
        #[arg(long)]
        key: String,
        /// Value to write. Use `--value-json` for arrays/objects.
        #[arg(long, conflicts_with = "value_json")]
        value: Option<String>,
        /// Raw JSON value (for arrays/objects/null literals).
        #[arg(long = "value-json")]
        value_json: Option<String>,
    },
    /// Replace the settings document with the JSON in `--from-file <path|->`.
    Apply {
        /// File path or `-` for stdin.
        #[arg(long = "from-file", default_value = "-")]
        from_file: String,
    },
    /// Test connectivity through an outbound proxy URL by HEADing a probe URL.
    ProxyTest {
        /// Outbound proxy URL (`http(s)://...` or `socks5://...`).
        #[arg(long)]
        proxy_url: String,
        /// URL the server should probe (default: `https://google.com/`).
        #[arg(long)]
        test_url: Option<String>,
        /// Per-request timeout, in milliseconds (default: 8000, max: 30000).
        #[arg(long)]
        timeout_ms: Option<u64>,
    },
    /// Locale management. Currently `set` only — `get` is read from the
    /// settings document via `settings get --key locale` once the server
    /// exposes it.
    Locale {
        #[command(subcommand)]
        cmd: LocaleCmd,
    },
    /// Print the dashboard package version reported by the server.
    Version,
    /// Check for an upstream upgrade (`--check`, default) or apply it
    /// (`--apply`).
    Update {
        /// Just probe — never write. This is the default if no flag is set.
        #[arg(long, conflicts_with = "apply")]
        check: bool,
        /// Trigger the server-side self-update.
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum LocaleCmd {
    /// Set the server-side locale cookie.
    Set {
        /// Locale code (e.g. `en`, `vi`, `zh-CN`).
        lang: String,
    },
}

pub async fn run(cmd: SettingsCmd, cfg: &ResolvedConfig, ctx: OutputCtx) -> anyhow::Result<i32> {
    let rt = match require_runtime(cfg).await {
        Ok(rt) => rt,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    match cmd {
        SettingsCmd::Get { key } => run_get(&rt, ctx, key.as_deref()).await,
        SettingsCmd::Set {
            key,
            value,
            value_json,
        } => run_set(&rt, ctx, &key, value, value_json).await,
        SettingsCmd::Apply { from_file } => run_apply(&rt, ctx, &from_file).await,
        SettingsCmd::ProxyTest {
            proxy_url,
            test_url,
            timeout_ms,
        } => run_proxy_test(&rt, ctx, proxy_url, test_url, timeout_ms).await,
        SettingsCmd::Locale { cmd } => match cmd {
            LocaleCmd::Set { lang } => run_locale_set(&rt, ctx, lang).await,
        },
        SettingsCmd::Version => run_version(&rt, ctx).await,
        SettingsCmd::Update { check, apply } => run_update(&rt, ctx, check, apply).await,
    }
}

async fn run_get(rt: &Runtime, ctx: OutputCtx, key: Option<&str>) -> anyhow::Result<i32> {
    match rt.get_json("/api/settings").await {
        Ok(payload) => {
            if let Some(path) = key {
                let value = walk_dotted_path(&payload, path);
                if value.is_none() {
                    return Ok(emit_error(
                        ctx,
                        "not_found",
                        &format!("settings key not found: {path}"),
                    )?);
                }
                let value = value.unwrap().clone();
                if ctx.is_robot() {
                    emit_robot(
                        "openproxy.v1.settings.get",
                        json!({"key": path, "value": value}),
                    )?;
                } else {
                    let pretty = match &value {
                        Value::String(s) => s.clone(),
                        other => serde_json::to_string(other).unwrap_or_default(),
                    };
                    println!("{pretty}");
                }
            } else if ctx.is_robot() {
                emit_robot("openproxy.v1.settings.get", payload)?;
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

async fn run_set(
    rt: &Runtime,
    ctx: OutputCtx,
    key: &str,
    value: Option<String>,
    value_json: Option<String>,
) -> anyhow::Result<i32> {
    let raw = match (value, value_json) {
        (Some(_), Some(_)) => {
            return Ok(emit_error(
                ctx,
                "usage",
                "pass exactly one of --value or --value-json",
            )?);
        }
        (Some(v), None) => coerce_value_str(&v),
        (None, Some(j)) => match serde_json::from_str::<Value>(&j) {
            Ok(v) => v,
            Err(e) => {
                return Ok(emit_error(
                    ctx,
                    "usage",
                    &format!("invalid --value-json: {e}"),
                )?);
            }
        },
        (None, None) => {
            return Ok(emit_error(
                ctx,
                "usage",
                "pass --value <v> or --value-json <json>",
            )?);
        }
    };

    let mut body = Map::new();
    body.insert(key.to_string(), raw);
    let body = Value::Object(body);

    match rt.patch_json("/api/settings", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot(
                    "openproxy.v1.settings.set",
                    json!({"updated": [key], "settings": payload}),
                )?;
            } else {
                humanln(ctx, format!("settings updated: {key}"));
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_apply(rt: &Runtime, ctx: OutputCtx, from_file: &str) -> anyhow::Result<i32> {
    let raw = match read_input(from_file) {
        Ok(s) => s,
        Err(e) => {
            return Ok(emit_error(
                ctx,
                "usage",
                &format!("cannot read {from_file}: {e}"),
            )?);
        }
    };
    let body: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            return Ok(emit_error(
                ctx,
                "validation",
                &format!("settings document is not valid JSON: {e}"),
            )?);
        }
    };
    if !body.is_object() {
        return Ok(emit_error(
            ctx,
            "validation",
            "settings document must be an object",
        )?);
    }

    // PATCH so the server merges field-by-field; the BE handler treats PUT
    // and PATCH the same but PATCH is the friendlier verb for a merge.
    match rt.patch_json("/api/settings", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.settings.apply", json!({"settings": payload}))?;
            } else {
                humanln(ctx, "settings applied");
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_proxy_test(
    rt: &Runtime,
    ctx: OutputCtx,
    proxy_url: String,
    test_url: Option<String>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<i32> {
    let mut body = Map::new();
    body.insert("proxyUrl".to_string(), Value::String(proxy_url));
    if let Some(t) = test_url {
        body.insert("testUrl".to_string(), Value::String(t));
    }
    if let Some(t) = timeout_ms {
        body.insert(
            "timeoutMs".to_string(),
            Value::Number(serde_json::Number::from(t)),
        );
    }
    let body = Value::Object(body);
    match rt.post_json("/api/settings/proxy-test", &body).await {
        Ok(payload) => {
            let ok = payload.get("ok").and_then(Value::as_bool).unwrap_or(false);
            if ctx.is_robot() {
                emit_robot("openproxy.v1.settings.proxy_test", payload.clone())?;
            } else {
                let elapsed = payload
                    .get("elapsedMs")
                    .and_then(Value::as_u64)
                    .map(|n| format!("{n}ms"))
                    .unwrap_or_default();
                let status = payload
                    .get("status")
                    .and_then(Value::as_u64)
                    .map(|n| format!(" status={n}"))
                    .unwrap_or_default();
                humanln(
                    ctx,
                    format!(
                        "proxy_test: {} {elapsed}{status}",
                        if ok { "ok" } else { "fail" }
                    ),
                );
            }
            Ok(if ok { 0 } else { 7 })
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_locale_set(rt: &Runtime, ctx: OutputCtx, lang: String) -> anyhow::Result<i32> {
    let body = json!({"locale": lang});
    match rt.post_json("/api/locale", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.settings.locale.set", payload)?;
            } else {
                let locale = payload
                    .get("locale")
                    .and_then(Value::as_str)
                    .unwrap_or(&lang);
                humanln(ctx, format!("locale set: {locale}"));
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_version(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.get_json("/api/version").await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.settings.version", payload)?;
            } else {
                let cur = payload
                    .get("currentVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("?");
                let latest = payload
                    .get("latestVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let has = payload
                    .get("hasUpdate")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                humanln(
                    ctx,
                    format!(
                        "openproxy {cur} (latest: {latest}){}",
                        if has { " — update available" } else { "" }
                    ),
                );
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_update(
    rt: &Runtime,
    ctx: OutputCtx,
    _check: bool,
    apply: bool,
) -> anyhow::Result<i32> {
    if apply {
        match rt.post_empty("/api/version/update").await {
            Ok(payload) => {
                if ctx.is_robot() {
                    emit_robot("openproxy.v1.settings.update.apply", payload)?;
                } else {
                    let msg = payload
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("update requested");
                    humanln(ctx, msg);
                }
                Ok(0)
            }
            Err(e) => rt_error_to_exit(ctx, e),
        }
    } else {
        // default = `--check` semantics
        match rt.get_json("/api/version").await {
            Ok(payload) => {
                if ctx.is_robot() {
                    emit_robot("openproxy.v1.settings.update.check", payload)?;
                } else {
                    let has = payload
                        .get("hasUpdate")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let cur = payload
                        .get("currentVersion")
                        .and_then(Value::as_str)
                        .unwrap_or("?");
                    let latest = payload
                        .get("latestVersion")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    humanln(
                        ctx,
                        if has {
                            format!("update available: {cur} → {latest}")
                        } else {
                            format!("up to date: {cur}")
                        },
                    );
                }
                Ok(0)
            }
            Err(e) => rt_error_to_exit(ctx, e),
        }
    }
}

fn walk_dotted_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = value;
    for part in path.split('.') {
        cur = cur.get(part)?;
    }
    Some(cur)
}

fn coerce_value_str(raw: &str) -> Value {
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("true") {
        return Value::Bool(true);
    }
    if trimmed.eq_ignore_ascii_case("false") {
        return Value::Bool(false);
    }
    if trimmed.eq_ignore_ascii_case("null") {
        return Value::Null;
    }
    if let Ok(n) = trimmed.parse::<i64>() {
        return Value::Number(serde_json::Number::from(n));
    }
    if let Ok(n) = trimmed.parse::<f64>() {
        if let Some(num) = serde_json::Number::from_f64(n) {
            return Value::Number(num);
        }
    }
    Value::String(raw.to_string())
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
    fn coerce_picks_bools_then_numbers_then_strings() {
        assert_eq!(coerce_value_str("true"), Value::Bool(true));
        assert_eq!(coerce_value_str("False"), Value::Bool(false));
        assert_eq!(coerce_value_str("null"), Value::Null);
        assert_eq!(
            coerce_value_str("42"),
            Value::Number(serde_json::Number::from(42))
        );
        assert_eq!(
            coerce_value_str("hello"),
            Value::String("hello".to_string())
        );
    }

    #[test]
    fn walk_traverses_nested_objects() {
        let v = json!({"a": {"b": {"c": 1}}});
        assert_eq!(walk_dotted_path(&v, "a.b.c"), Some(&json!(1)));
        assert!(walk_dotted_path(&v, "a.x").is_none());
    }
}
