//! `openproxy chat *` — interact with the proxy from the CLI.

use clap::Subcommand;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::io::Write;

use crate::cli::config::ResolvedConfig;
use crate::cli::output::{emit_robot, humanln, OutputCtx};
use crate::cli::runtime::{read_input, require_runtime, rt_error_to_exit, Runtime};

#[derive(Debug, Clone, Subcommand)]
pub enum ChatCmd {
    /// List models the proxy currently exposes (`/v1/models`).
    Models,
    /// List user-defined tags (`/api/tags`).
    Tags,
    /// Send a single non-streaming chat request and print the assistant
    /// message. Reads the prompt from `--prompt` or stdin (`--prompt -`).
    Send {
        #[arg(long)]
        model: String,
        /// Prompt string, or `-` to read from stdin.
        #[arg(long, default_value = "-")]
        prompt: String,
        /// Optional system message.
        #[arg(long)]
        system: Option<String>,
    },
    /// Stream a chat completion, one NDJSON event per chunk, until Ctrl+C
    /// or the server closes the stream.
    Stream {
        #[arg(long)]
        model: String,
        #[arg(long, default_value = "-")]
        prompt: String,
        #[arg(long)]
        system: Option<String>,
    },
}

pub async fn run(cmd: ChatCmd, cfg: &ResolvedConfig, ctx: OutputCtx) -> anyhow::Result<i32> {
    let rt = match require_runtime(cfg).await {
        Ok(rt) => rt,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    match cmd {
        ChatCmd::Models => run_models(&rt, ctx).await,
        ChatCmd::Tags => run_tags(&rt, ctx).await,
        ChatCmd::Send {
            model,
            prompt,
            system,
        } => run_send(&rt, ctx, model, prompt, system).await,
        ChatCmd::Stream {
            model,
            prompt,
            system,
        } => run_stream(&rt, ctx, model, prompt, system).await,
    }
}

async fn run_models(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.get_json("/v1/models").await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.chat.models", payload)?;
            } else {
                let data = payload.get("data").and_then(Value::as_array);
                let count = data.map(|a| a.len()).unwrap_or(0);
                humanln(ctx, format!("{} models", count));
                if let Some(arr) = data {
                    for m in arr.iter().take(20) {
                        humanln(
                            ctx,
                            format!("  {}", m.get("id").and_then(Value::as_str).unwrap_or("?")),
                        );
                    }
                }
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_tags(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    match rt.get_json("/api/tags").await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.chat.tags", payload)?;
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

fn build_chat_body(model: &str, prompt: &str, system: Option<&str>, stream: bool) -> Value {
    let mut messages = Vec::new();
    if let Some(sys) = system {
        messages.push(json!({"role": "system", "content": sys}));
    }
    messages.push(json!({"role": "user", "content": prompt}));
    json!({
        "model": model,
        "messages": messages,
        "stream": stream,
    })
}

async fn run_send(
    rt: &Runtime,
    ctx: OutputCtx,
    model: String,
    prompt: String,
    system: Option<String>,
) -> anyhow::Result<i32> {
    let prompt_text = read_input(&prompt)?;
    let body = build_chat_body(&model, prompt_text.trim_end(), system.as_deref(), false);
    match rt.post_json("/api/dashboard/chat/completions", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.chat.response", payload)?;
            } else {
                let content = payload
                    .get("choices")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .and_then(|c| c.get("message"))
                    .and_then(|m| m.get("content"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                println!("{}", content);
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_stream(
    rt: &Runtime,
    ctx: OutputCtx,
    model: String,
    prompt: String,
    system: Option<String>,
) -> anyhow::Result<i32> {
    let prompt_text = read_input(&prompt)?;
    let body = build_chat_body(&model, prompt_text.trim_end(), system.as_deref(), true);

    let mut stream = match rt
        .post_stream_sse("/api/dashboard/chat/completions", &body)
        .await
    {
        Ok(s) => s,
        Err(e) => return rt_error_to_exit(ctx, e),
    };

    let mut stdout = std::io::stdout().lock();
    while let Some(chunk) = stream.next().await {
        let bytes = match chunk {
            Ok(b) => b,
            Err(e) => return rt_error_to_exit(ctx, e),
        };
        if bytes.starts_with(b"[DONE]") || bytes.as_ref() == b"[DONE]" {
            break;
        }
        let event: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        if ctx.is_robot() {
            let envelope = json!({
                "schema": "openproxy.v1.chat.event",
                "ok": true,
                "data": event,
                "error": null,
                "meta": {},
            });
            writeln!(stdout, "{}", envelope)?;
        } else if let Some(delta) = event
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
        {
            write!(stdout, "{}", delta)?;
        }
        stdout.flush()?;
    }
    if !ctx.is_robot() {
        writeln!(stdout)?;
    }
    Ok(0)
}
