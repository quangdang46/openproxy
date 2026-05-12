//! `openproxy media *` — media provider + media endpoint helpers (PLAN v3 mục
//! 4.15). Wraps `/api/media-providers/*` for CRUD on TTS/STT/embed/image/web
//! providers and the synchronous `/v1/audio/*`, `/v1/embeddings`,
//! `/v1/images/generations`, `/v1/search`, `/v1/web/fetch` endpoints.
//!
//! `media tts speak` is the only command that writes raw bytes (audio) to
//! stdout — everything else emits JSON envelopes via `--robot`.

use std::io::Write;

use clap::Subcommand;
use serde_json::{json, Map, Value};

use crate::cli::config::ResolvedConfig;
use crate::cli::output::{emit_robot, humanln, OutputCtx};
use crate::cli::runtime::{read_input, require_runtime, rt_error_to_exit, Runtime};

#[derive(Debug, Clone, Subcommand)]
pub enum MediaCmd {
    /// Manage media providers (TTS, STT, embed, image, web).
    Providers {
        #[command(subcommand)]
        cmd: ProvidersCmd,
    },
    /// Manage media combos (chained provider fallbacks per kind).
    Combo {
        #[command(subcommand)]
        cmd: ComboCmd,
    },
    /// Text-to-speech commands.
    Tts {
        #[command(subcommand)]
        cmd: TtsCmd,
    },
    /// Speech-to-text commands.
    Stt {
        #[command(subcommand)]
        cmd: SttCmd,
    },
    /// Generate embeddings.
    Embed {
        #[arg(long)]
        provider: String,
        /// Embedding model id.
        #[arg(long, default_value = "")]
        model: String,
        /// Text input or `-` for stdin.
        #[arg(long, default_value = "-")]
        text: String,
    },
    /// Image generation.
    Image {
        #[command(subcommand)]
        cmd: ImageCmd,
    },
    /// Generic web search via `/v1/search`.
    Search {
        #[arg(long)]
        provider: String,
        /// Query string or `-` for stdin.
        #[arg(long, default_value = "-")]
        query: String,
    },
    /// Web fetch (extracted page content).
    Web {
        #[command(subcommand)]
        cmd: WebCmd,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum ProvidersCmd {
    /// List media providers, optionally filtered by kind.
    List {
        /// One of `tts|stt|embed|image|web` (legacy: `embedding`, `search`).
        #[arg(long)]
        kind: Option<String>,
    },
    /// Add a media provider via POST `/api/media-providers`.
    Add {
        /// Provider id (e.g. `elevenlabs`, `openai`, `cohere`, `firecrawl`).
        #[arg(long)]
        provider: String,
        /// Kind: `tts|stt|embedding|image|search|webSearch|webFetch`.
        #[arg(long)]
        kind: String,
        /// Display name for the provider entry.
        #[arg(long)]
        name: String,
        /// JSON body or `-` for stdin (merged on top of `{provider, kind, name}`).
        #[arg(long = "from-file")]
        from_file: Option<String>,
    },
    /// Edit a media provider (PUT via `/api/media-providers/<id>`).
    Edit {
        /// Provider connection id.
        id: String,
        /// JSON body of fields to update, or `-` for stdin.
        #[arg(long = "from-file", default_value = "-")]
        from_file: String,
    },
    /// Delete a media provider.
    Delete {
        /// Provider connection id.
        id: String,
        /// Kind path segment (defaults to `tts`).
        #[arg(long, default_value = "tts")]
        kind: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum ComboCmd {
    /// List media combos.
    List,
    /// Create a media combo (chained providers per kind).
    Create {
        /// Combo kind: `tts|stt|embedding|image|search`.
        #[arg(long)]
        kind: String,
        /// Combo display name.
        #[arg(long)]
        name: String,
        /// Comma-separated list of provider ids.
        #[arg(long, value_delimiter = ',')]
        members: Vec<String>,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum TtsCmd {
    /// List available voices, optionally filtered to one provider.
    Voices {
        #[arg(long)]
        provider: Option<String>,
        /// Optional language filter (e.g. `en`).
        #[arg(long)]
        lang: Option<String>,
    },
    /// Synthesize speech to stdout (raw audio bytes).
    Speak {
        #[arg(long)]
        provider: String,
        #[arg(long, default_value = "")]
        model: String,
        #[arg(long)]
        voice: String,
        /// Text input or `-` for stdin.
        #[arg(long, default_value = "-")]
        text: String,
        /// Output format hint (mp3, wav, ...). Default: `mp3`.
        #[arg(long, default_value = "mp3")]
        format: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum SttCmd {
    /// Transcribe an audio file.
    Transcribe {
        #[arg(long)]
        provider: String,
        #[arg(long, default_value = "")]
        model: String,
        /// Path to the audio file on disk (base64-encoded into the request).
        #[arg(long)]
        file: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum ImageCmd {
    /// Generate an image and print the JSON response.
    Generate {
        #[arg(long)]
        provider: String,
        #[arg(long, default_value = "")]
        model: String,
        /// Prompt text or `-` for stdin.
        #[arg(long, default_value = "-")]
        prompt: String,
        /// Image size (e.g. `1024x1024`).
        #[arg(long, default_value = "1024x1024")]
        size: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum WebCmd {
    /// Fetch a URL via `/v1/web/fetch`.
    Fetch {
        /// Page URL to fetch (positional to avoid colliding with the
        /// global `--url` server-override flag).
        page: String,
        #[arg(long)]
        provider: String,
        /// Output format: markdown (default), html, text.
        #[arg(long, default_value = "markdown")]
        format: String,
        /// Truncate the output to N characters.
        #[arg(long)]
        max_chars: Option<usize>,
    },
}

pub async fn run(cmd: MediaCmd, cfg: &ResolvedConfig, ctx: OutputCtx) -> anyhow::Result<i32> {
    let rt = match require_runtime(cfg).await {
        Ok(rt) => rt,
        Err(e) => return rt_error_to_exit(ctx, e),
    };
    match cmd {
        MediaCmd::Providers { cmd } => match cmd {
            ProvidersCmd::List { kind } => run_providers_list(&rt, ctx, kind).await,
            ProvidersCmd::Add {
                provider,
                kind,
                name,
                from_file,
            } => run_providers_add(&rt, ctx, provider, kind, name, from_file).await,
            ProvidersCmd::Edit { id, from_file } => {
                run_providers_edit(&rt, ctx, id, from_file).await
            }
            ProvidersCmd::Delete { id, kind } => run_providers_delete(&rt, ctx, id, kind).await,
        },
        MediaCmd::Combo { cmd } => match cmd {
            ComboCmd::List => run_combo_list(&rt, ctx).await,
            ComboCmd::Create {
                kind,
                name,
                members,
            } => run_combo_create(&rt, ctx, kind, name, members).await,
        },
        MediaCmd::Tts { cmd } => match cmd {
            TtsCmd::Voices { provider, lang } => run_tts_voices(&rt, ctx, provider, lang).await,
            TtsCmd::Speak {
                provider,
                model,
                voice,
                text,
                format,
            } => run_tts_speak(&rt, ctx, provider, model, voice, text, format).await,
        },
        MediaCmd::Stt { cmd } => match cmd {
            SttCmd::Transcribe {
                provider,
                model,
                file,
            } => run_stt_transcribe(&rt, ctx, provider, model, file).await,
        },
        MediaCmd::Embed {
            provider,
            model,
            text,
        } => run_embed(&rt, ctx, provider, model, text).await,
        MediaCmd::Image { cmd } => match cmd {
            ImageCmd::Generate {
                provider,
                model,
                prompt,
                size,
            } => run_image_generate(&rt, ctx, provider, model, prompt, size).await,
        },
        MediaCmd::Search { provider, query } => run_search(&rt, ctx, provider, query).await,
        MediaCmd::Web { cmd } => match cmd {
            WebCmd::Fetch {
                provider,
                page,
                format,
                max_chars,
            } => run_web_fetch(&rt, ctx, provider, page, format, max_chars).await,
        },
    }
}

async fn run_providers_list(
    rt: &Runtime,
    ctx: OutputCtx,
    kind: Option<String>,
) -> anyhow::Result<i32> {
    let path = match &kind {
        Some(k) => format!("/api/media-providers/{}", encode_kind(k)),
        None => "/api/media-providers".to_string(),
    };
    match rt.get_json(&path).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.media.providers.list", payload)?;
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

async fn run_providers_add(
    rt: &Runtime,
    ctx: OutputCtx,
    provider: String,
    kind: String,
    name: String,
    from_file: Option<String>,
) -> anyhow::Result<i32> {
    let mut body = if let Some(path) = from_file {
        let raw = read_input(&path)?;
        serde_json::from_str(raw.trim()).map_err(|e| anyhow::anyhow!("--from-file JSON: {e}"))?
    } else {
        Value::Object(Map::new())
    };
    if let Some(obj) = body.as_object_mut() {
        obj.insert("provider".to_string(), Value::String(provider));
        obj.insert("mediaType".to_string(), Value::String(server_kind(&kind)));
        obj.insert("name".to_string(), Value::String(name));
    }
    match rt.post_json("/api/media-providers", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.media.providers.add", payload)?;
            } else {
                humanln(
                    ctx,
                    format!(
                        "Added media provider id={}",
                        payload.get("id").and_then(Value::as_str).unwrap_or("?")
                    ),
                );
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_providers_edit(
    rt: &Runtime,
    ctx: OutputCtx,
    id: String,
    from_file: String,
) -> anyhow::Result<i32> {
    let raw = read_input(&from_file)?;
    let body: Value =
        serde_json::from_str(raw.trim()).map_err(|e| anyhow::anyhow!("--from-file JSON: {e}"))?;
    let path = format!("/api/media-providers/{}", urlencoding::encode(&id));
    match rt.put_json(&path, &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.media.providers.edit", payload)?;
            } else {
                humanln(ctx, format!("Edited media provider id={id}"));
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_providers_delete(
    rt: &Runtime,
    ctx: OutputCtx,
    id: String,
    kind: String,
) -> anyhow::Result<i32> {
    // The server route is `/api/media-providers/{kind}` with `?id=` or the
    // kind path acts as the id when no kind is provided. We mirror the
    // shape used by the dashboard delete button.
    let path = format!(
        "/api/media-providers/{}?id={}",
        encode_kind(&kind),
        urlencoding::encode(&id)
    );
    match rt.delete_json(&path).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.media.providers.delete", payload)?;
            } else {
                humanln(ctx, format!("Deleted media provider id={id}"));
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_combo_list(rt: &Runtime, ctx: OutputCtx) -> anyhow::Result<i32> {
    // No dedicated list endpoint; use `/api/combos` filtered to media kinds.
    match rt.get_json("/api/combos").await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.media.combo.list", payload)?;
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

async fn run_combo_create(
    rt: &Runtime,
    ctx: OutputCtx,
    kind: String,
    name: String,
    members: Vec<String>,
) -> anyhow::Result<i32> {
    let body = json!({
        "name": name,
        "kind": server_kind(&kind),
        "providers": members,
        "strategy": "fallback",
    });
    match rt.post_json("/api/combos", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.media.combo.create", payload)?;
            } else {
                humanln(
                    ctx,
                    format!(
                        "Created media combo id={}",
                        payload.get("id").and_then(Value::as_str).unwrap_or("?")
                    ),
                );
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_tts_voices(
    rt: &Runtime,
    ctx: OutputCtx,
    provider: Option<String>,
    lang: Option<String>,
) -> anyhow::Result<i32> {
    let mut path = "/api/media-providers/tts/voices".to_string();
    let mut query = Vec::new();
    if let Some(p) = provider {
        query.push(format!("provider={}", urlencoding::encode(&p)));
    }
    if let Some(l) = lang {
        query.push(format!("lang={}", urlencoding::encode(&l)));
    }
    if !query.is_empty() {
        path = format!("{}?{}", path, query.join("&"));
    }
    match rt.get_json(&path).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.media.tts.voices", payload)?;
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

async fn run_tts_speak(
    rt: &Runtime,
    ctx: OutputCtx,
    provider: String,
    model: String,
    voice: String,
    text: String,
    format: String,
) -> anyhow::Result<i32> {
    let input = read_input(&text)?;
    let body = json!({
        "model": if model.is_empty() { provider.clone() } else { model.clone() },
        "voice": voice,
        "input": input.trim_end(),
        "response_format": format,
        "provider": provider,
    });
    match rt.post_json_bytes("/v1/audio/speech", &body).await {
        Ok((bytes, content_type)) => {
            if ctx.is_robot() {
                emit_robot(
                    "openproxy.v1.media.tts.speak",
                    json!({
                        "bytes": bytes.len(),
                        "content_type": content_type,
                    }),
                )?;
            }
            // Write raw audio to stdout, even in --robot mode (the envelope
            // gives metadata; the bytes are the payload). Agents reading
            // both should split stdout into two channels.
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(&bytes)?;
            stdout.flush()?;
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_stt_transcribe(
    rt: &Runtime,
    ctx: OutputCtx,
    provider: String,
    model: String,
    file: String,
) -> anyhow::Result<i32> {
    let raw = std::fs::read(&file).map_err(|e| anyhow::anyhow!("read --file {file}: {e}"))?;
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    let encoded = B64.encode(&raw);
    let body = json!({
        "provider": provider,
        "model": if model.is_empty() { "whisper-1".to_string() } else { model },
        "file_b64": encoded,
        "file_name": std::path::Path::new(&file)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "audio".to_string()),
    });
    match rt.post_json("/v1/audio/transcriptions", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.media.stt.transcribe", payload)?;
            } else {
                let text = payload.get("text").and_then(Value::as_str).unwrap_or("");
                println!("{text}");
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

async fn run_embed(
    rt: &Runtime,
    ctx: OutputCtx,
    provider: String,
    model: String,
    text: String,
) -> anyhow::Result<i32> {
    let input = read_input(&text)?;
    let body = json!({
        "provider": provider,
        "model": if model.is_empty() { "text-embedding-3-small".to_string() } else { model },
        "input": input.trim_end(),
    });
    match rt.post_json("/v1/embeddings", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.media.embed", payload)?;
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

async fn run_image_generate(
    rt: &Runtime,
    ctx: OutputCtx,
    provider: String,
    model: String,
    prompt: String,
    size: String,
) -> anyhow::Result<i32> {
    let prompt_text = read_input(&prompt)?;
    let body = json!({
        "provider": provider,
        "model": if model.is_empty() { "gpt-image-1".to_string() } else { model },
        "prompt": prompt_text.trim(),
        "size": size,
    });
    match rt.post_json("/v1/images/generations", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.media.image.generate", payload)?;
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

async fn run_search(
    rt: &Runtime,
    ctx: OutputCtx,
    provider: String,
    query: String,
) -> anyhow::Result<i32> {
    let q = read_input(&query)?;
    let body = json!({
        "provider": provider,
        "query": q.trim(),
    });
    match rt.post_json("/v1/search", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.media.search", payload)?;
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

async fn run_web_fetch(
    rt: &Runtime,
    ctx: OutputCtx,
    provider: String,
    url: String,
    format: String,
    max_chars: Option<usize>,
) -> anyhow::Result<i32> {
    let mut body = json!({
        "provider": provider,
        "url": url,
        "format": format,
    });
    if let Some(m) = max_chars {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("maxCharacters".to_string(), json!(m));
        }
    }
    match rt.post_json("/v1/web/fetch", &body).await {
        Ok(payload) => {
            if ctx.is_robot() {
                emit_robot("openproxy.v1.media.web.fetch", payload)?;
            } else {
                let content = payload.get("content").and_then(Value::as_str).unwrap_or("");
                println!("{content}");
            }
            Ok(0)
        }
        Err(e) => rt_error_to_exit(ctx, e),
    }
}

/// Map CLI-friendly kind names to what the server route expects.
fn server_kind(kind: &str) -> String {
    match kind {
        "embed" => "embedding".into(),
        "web-search" | "websearch" => "webSearch".into(),
        "web-fetch" | "webfetch" => "webFetch".into(),
        other => other.to_string(),
    }
}

fn encode_kind(kind: &str) -> String {
    urlencoding::encode(&server_kind(kind)).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_kind_normalizes_aliases() {
        assert_eq!(server_kind("embed"), "embedding");
        assert_eq!(server_kind("web-search"), "webSearch");
        assert_eq!(server_kind("tts"), "tts");
    }

    #[test]
    fn encode_kind_keeps_camel_case() {
        assert_eq!(encode_kind("webSearch"), "webSearch");
    }
}
