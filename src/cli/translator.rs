//! `openproxy translator *` — request translator pipeline (PLAN v3 mục 4.13).
//!
//! Talks to `/api/translator/*`. The server exposes a 3-step pipeline
//! (`/api/translator/translate?step=1..3`): step 1 detects source/target
//! format, step 2 translates source → OpenAI intermediate, step 3 translates
//! OpenAI intermediate → target plus builds the URL/headers. We expose:
//!
//! - `translator formats` → `/api/translator/formats`
//! - `translator translate --from <src> --to <dst>` → step 2 + step 3 combo.
//! - `translator send --provider <p>` → `/api/translator/send`.
//! - `translator preset list|save|load` — built on top of `/api/translator/save`
//!   + `/api/translator/load` (translations dictionary).

use clap::Subcommand;
use serde_json::{json, Map, Value};

use crate::cli::config::ResolvedConfig;
use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};
use crate::cli::runtime::{read_input, require_runtime, rt_error_to_exit, Runtime};

#[derive(Debug, Clone, Subcommand)]
pub enum TranslatorCmd {
    /// List source/target formats the server supports.
    Formats,
    /// Translate a request body from one format to another.
    Translate {
        /// Source format (e.g. `openai`, `claude`, `gemini`).
        #[arg(long)]
        from: String,
        /// Target format (e.g. `openai`, `claude`, `gemini`).
        #[arg(long)]
        to: String,
        /// Path to a JSON request body, or `-` for stdin.
        #[arg(long = "from-file", default_value = "-")]
        from_file: String,
        /// Model id to pass through to the translator.
        #[arg(long, default_value = "")]
        model: String,
    },
    /// Send a translated request to a provider via `/api/translator/send`.
    Send {
        #[arg(long)]
        provider: String,
        /// Model id for the request.
        #[arg(long, default_value = "")]
        model: String,
        /// Path to a JSON request body (already in target format), or `-`.
        #[arg(long = "from-file", default_value = "-")]
        from_file: String,
    },
    /// Manage named translator presets persisted on the server.
    Preset {
        #[command(subcommand)]
        cmd: PresetCmd,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum PresetCmd {
    /// List saved presets.
    List,
    /// Save a preset under `<name>`. Body is read from `--from-file` (`-` = stdin).
    Save {
        name: String,
        #[arg(long = "from-file", default_value = "-")]
        from_file: String,
    },
    /// Load a preset by name.
    Load { name: String },
}

pub async fn run(cmd: TranslatorCmd, cfg: &ResolvedConfig, ctx: OutputCtx) -> anyhow::Result<i32> {
    let rt = match require_runtime(cfg).await {
        Ok(rt) => rt,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    match cmd {
        TranslatorCmd::Formats => run_formats(&rt, ctx).await,
        TranslatorCmd::Translate {
            from,
            to,
            from_file,
            model,
        } => run_translate(&rt, ctx, from, to, from_file, model).await,
        TranslatorCmd::Send {
            provider,
            model,
            from_file,
        } => run_send(&rt, ctx, provider, model, from_file).await,
        TranslatorCmd::Preset { cmd } => match cmd {
            PresetCmd::List => run_preset_list(&rt, ctx).await,
            PresetCmd::Save { name, from_file } => run_preset_save(&rt, ctx, name, from_file).await,
            PresetCmd::Load { name } => run_preset_load(&rt, ctx, name).await,
        },
    }
}

async fn run_formats(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.get_json("/api/translator/formats").await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.translator.formats", payload)?;
            } else {
                let arr = payload.as_array().cloned().unwrap_or_default();
                for f in &arr {
                    humanln(
                        ctx,
                        format!(
                            "{:20} {}",
                            f.get("id").and_then(Value::as_str).unwrap_or("?"),
                            f.get("description").and_then(Value::as_str).unwrap_or(""),
                        ),
                    );
                }
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_translate(
    rt: &Runtime,
    ctx: OutputCtx,
    from: String,
    to: String,
    from_file: String,
    model: String,
) -> anyhow::Result<i32> {
    let raw = read_input(&from_file)?;
    let mut body: Value = serde_json::from_str(raw.trim())
        .map_err(|e| anyhow::anyhow!("--from-file must be JSON: {e}"))?;

    // Ensure the model is on the body so the server can route it.
    if !model.is_empty() {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("model".to_string(), Value::String(model.clone()));
        }
    }

    // Step 2: translate <from> → OpenAI intermediate.
    let step2 = json!({"step": 2, "from": from, "to": "openai", "body": body});
    let openai_body = match rt.post_json("/api/translator/translate", &step2).await {
        Ok(v) => v
            .get("result")
            .and_then(|r| r.get("body"))
            .cloned()
            .unwrap_or(Value::Null),
        Err(e) => return rt_error_to_exit(ctx, e),
    };

    // Step 3: translate OpenAI intermediate → <to>.
    let step3 = json!({
        "step": 3,
        "provider": &to,
        "model": model,
        "body": openai_body,
    });
    match rt.post_json("/api/translator/translate", &step3).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.translator.translate", payload)?;
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

async fn run_send(
    rt: &Runtime,
    ctx: OutputCtx,
    provider: String,
    model: String,
    from_file: String,
) -> anyhow::Result<i32> {
    let raw = read_input(&from_file)?;
    let body: Value = serde_json::from_str(raw.trim())
        .map_err(|e| anyhow::anyhow!("--from-file must be JSON: {e}"))?;
    let send_body = json!({
        "provider": provider,
        "model": model,
        "body": body,
    });
    match rt.post_json("/api/translator/send", &send_body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.translator.send", payload)?;
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

async fn run_preset_list(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    // Server lacks a list endpoint, but `/api/translator/load` returns all
    // translations when called with an empty body.
    match rt.post_json("/api/translator/load", &json!({})).await {
        Ok(payload) => {
            let presets = payload
                .get("translations")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if ctx.is_robot() {
                emit_robot(
                    "openproxy.v1.translator.preset.list",
                    json!({"presets": presets}),
                )?;
            } else if let Some(obj) = presets.as_object() {
                for name in obj.keys() {
                    humanln(ctx, name);
                }
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_preset_save(
    rt: &Runtime,
    ctx: OutputCtx,
    name: String,
    from_file: String,
) -> anyhow::Result<i32> {
    let raw = read_input(&from_file)?;
    // The translator save endpoint stores opaque strings, so we encode the
    // JSON document as a string keyed by `name`.
    let mut map: Map<String, Value> = Map::new();
    map.insert(name.clone(), Value::String(raw.trim().to_string()));
    let body = json!({"translations": Value::Object(map)});
    match rt.post_json("/api/translator/save", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.translator.preset.save", payload)?;
            } else {
                humanln(ctx, format!("Saved preset `{name}`."));
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_preset_load(rt: &Runtime, ctx: OutputCtx, name: String) -> anyhow::Result<i32> {
    match rt.post_json("/api/translator/load", &json!({})).await {
        Ok(payload) => {
            let presets = payload
                .get("translations")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let raw = presets.get(&name).cloned();
            match raw {
                Some(Value::String(s)) => {
                    let parsed: Value = serde_json::from_str(&s).unwrap_or(Value::String(s));
                    if ctx.is_robot() {
                        emit_robot(
                            "openproxy.v1.translator.preset.load",
                            json!({"name": name, "body": parsed}),
                        )?;
                    } else {
                        humanln(
                            ctx,
                            serde_json::to_string_pretty(&parsed).unwrap_or_default(),
                        );
                    }
                    Ok(0)
                }
                Some(other) => {
                    if ctx.is_robot() {
                        emit_robot(
                            "openproxy.v1.translator.preset.load",
                            json!({"name": name, "body": other}),
                        )?;
                    } else {
                        humanln(
                            ctx,
                            serde_json::to_string_pretty(&other).unwrap_or_default(),
                        );
                    }
                    Ok(0)
                }
                None => Ok(emit_error(
                    ctx,
                    "not_found",
                    &format!("no preset named `{name}`"),
                )?),
            }
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}
