//! `openproxy media *` — media provider + media endpoint helpers (PLAN v3 mục
//! 4.15). Wraps `/api/media-providers/*` for CRUD on TTS/STT/embed/image/web
//! providers and the synchronous `/v1/audio/*`, `/v1/embeddings`,
//! `/v1/images/generations`, `/v1/search`, `/v1/web/fetch` endpoints.
//!
//! `media tts speak` is the only command that writes raw bytes (audio) to
//! stdout — everything else emits JSON envelopes via `--robot`. `media video`
//! is the async exception: it writes an MP4 to a file the caller names.

use std::io::Write;

use clap::Subcommand;
use serde_json::{json, Map, Value};

use crate::cli::config::ResolvedConfig;
use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};
use crate::cli::runtime::{read_input, require_runtime, rt_error_to_exit, Runtime};

/// Job states 9router stops polling on (`xaiVideo.js:22` `TERMINAL_STATUSES`).
const VIDEO_TERMINAL_STATUSES: [&str; 6] = [
    "done",
    "failed",
    "completed",
    "error",
    "expired",
    "cancelled",
];

/// The subset of those that will never produce a file
/// (`xaiVideo.js:23` `FAILED_STATUSES`).
const VIDEO_FAILED_STATUSES: [&str; 4] = ["failed", "error", "expired", "cancelled"];

/// 9router's `xaiVideo.js:18` host default.
const VIDEO_DEFAULT_HOST: &str = "127.0.0.1";
/// 9router defaults to 20128; OpenProxy's own listen port is 4623, so an
/// unconfigured `--port` has to mean something a user can actually curl.
const VIDEO_DEFAULT_PORT: u16 = 4623;
const VIDEO_DEFAULT_MODEL: &str = "xai/grok-imagine-video";
const VIDEO_DEFAULT_OUTPUT: &str = "video.mp4";
const VIDEO_DEFAULT_TIMEOUT_SEC: u64 = 600;
const VIDEO_DEFAULT_POLL_INTERVAL_MS: u64 = 5_000;
/// Per-request ceiling. A single create POST or poll GET is quick; the job's
/// own `--timeout` governs the loop, not any one request.
const VIDEO_HTTP_TIMEOUT_SEC: u64 = 120;

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
    /// Generate a video: submit the job, poll it, save the MP4.
    ///
    /// 9router's `xai video` (cli/src/cli/commands/xaiVideo.js) bypasses the
    /// CLI launcher and drives the gateway over a raw socket. Auth comes from
    /// the global `--api-key` / `$OPENPROXY_API_KEY`, never from a flag here,
    /// so the key cannot be echoed out of `--help` or a shell history.
    Video {
        /// Video description.
        #[arg(long)]
        prompt: String,
        /// Where to write the finished MP4.
        #[arg(long, short = 'o', default_value = VIDEO_DEFAULT_OUTPUT)]
        output: String,
        /// Video model id.
        #[arg(long, default_value = VIDEO_DEFAULT_MODEL)]
        model: String,
        /// Clip length in seconds.
        #[arg(long)]
        duration: Option<i64>,
        /// Aspect ratio, e.g. `16:9`, `9:16`, `1:1`.
        #[arg(long = "aspect-ratio")]
        aspect_ratio: Option<String>,
        /// `480p` | `720p` | `1080p`.
        #[arg(long)]
        resolution: Option<String>,
        /// Image path or URL, for image-to-video.
        #[arg(long)]
        image: Option<String>,
        /// Give up after this many seconds.
        #[arg(long, default_value_t = VIDEO_DEFAULT_TIMEOUT_SEC)]
        timeout: u64,
        /// Seconds between poll requests.
        #[arg(long, default_value_t = VIDEO_DEFAULT_POLL_INTERVAL_MS)]
        poll_interval_ms: u64,
        /// Gateway host. Omit to follow the resolved runtime (`--url`).
        #[arg(long)]
        host: Option<String>,
        /// Gateway port. Omit to follow the resolved runtime.
        #[arg(long)]
        port: Option<u16>,
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
        MediaCmd::Video {
            prompt,
            output,
            model,
            duration,
            aspect_ratio,
            resolution,
            image,
            timeout,
            poll_interval_ms,
            host,
            port,
        } => {
            run_video(
                cfg,
                &rt,
                ctx,
                VideoJob {
                    prompt,
                    output,
                    model,
                    duration,
                    aspect_ratio,
                    resolution,
                    image,
                    timeout_sec: timeout,
                    poll_interval_ms,
                    host,
                    port,
                },
            )
            .await
        }
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
    // `/v1/search` (generic_media_handler) requires `model`; the provider
    // alone is not enough. Send `<provider>/search` so parse_model resolves
    // the provider and select_media_connection matches the stored row.
    let body = json!({
        "provider": provider,
        "model": format!("{provider}/search"),
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

// ---------------------------------------------------------------------------
// `media video` — the async job flow behind `/v1/videos/*`.
// ---------------------------------------------------------------------------

/// Everything `media video` needs, gathered from the parsed subcommand so the
/// job runner takes one argument and stays readable.
struct VideoJob {
    prompt: String,
    output: String,
    model: String,
    duration: Option<i64>,
    aspect_ratio: Option<String>,
    resolution: Option<String>,
    image: Option<String>,
    timeout_sec: u64,
    poll_interval_ms: u64,
    host: Option<String>,
    port: Option<u16>,
}

/// Raw gateway client for the video job.
///
/// `Runtime`'s helpers cannot drive this flow: the create response carries the
/// job's account in a header, every poll has to pin that account back, and the
/// finished file is fetched from the provider's own host. 9router reaches for
/// `http.request` for the same three reasons (xaiVideo.js:100-118, :155-176,
/// :184-215); this is that with reqwest, reusing the gateway's auth contract
/// (`x-api-key` plus a bearer, exactly as `Runtime::auth_headers` sends it).
struct VideoClient {
    http: reqwest::Client,
    base: String,
    api_key: Option<String>,
}

/// One gateway answer, with the job's account already lifted out of the
/// headers.
struct VideoReply {
    status: reqwest::StatusCode,
    /// `x-9router-connection-id` (9router's own spelling) or the
    /// `x-openproxy-connection-id` this build also writes.
    connection_id: Option<String>,
    body: String,
}

impl VideoReply {
    fn json(&self) -> Option<Value> {
        serde_json::from_str(&self.body).ok()
    }
}

impl VideoClient {
    fn new(base: String, api_key: Option<String>) -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(VIDEO_HTTP_TIMEOUT_SEC))
                .build()?,
            base,
            api_key,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base.trim_end_matches('/'), path)
    }

    async fn send(
        &self,
        builder: reqwest::RequestBuilder,
        pin: Option<&str>,
    ) -> anyhow::Result<VideoReply> {
        let mut builder = builder.header(reqwest::header::ACCEPT, "application/json");
        if let Some(key) = &self.api_key {
            if let Ok(value) = reqwest::header::HeaderValue::from_str(key) {
                builder = builder.header("x-api-key", value);
                if let Ok(bearer) = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
                {
                    builder = builder.header(reqwest::header::AUTHORIZATION, bearer);
                }
            }
        }
        if let Some(id) = pin {
            builder = builder.header("x-connection-id", id);
        }
        let response = builder.send().await?;
        let connection_id = ["x-9router-connection-id", "x-openproxy-connection-id"]
            .into_iter()
            .find_map(|name| {
                response
                    .headers()
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
            });
        let status = response.status();
        let body = response.text().await?;
        Ok(VideoReply {
            status,
            connection_id,
            body,
        })
    }

    async fn create(&self, body: &Value) -> anyhow::Result<VideoReply> {
        self.send(
            self.http
                .post(self.url("/v1/videos/generations"))
                .json(body),
            None,
        )
        .await
    }

    async fn poll(&self, request_id: &str, pin: Option<&str>) -> anyhow::Result<VideoReply> {
        self.send(
            self.http
                .get(self.url(&format!("/v1/videos/{}", urlencoding::encode(request_id)))),
            pin,
        )
        .await
    }
}

/// Where the video job should be submitted.
///
/// `--host`/`--port` exist because 9router's `xaiVideo.js` is a standalone
/// script that dials a host and a port. Inside OpenProxy the resolved runtime
/// already knows better than a compiled-in default — it honours `--url` and
/// the endpoint sidecar `server start --detach` writes — so it wins unless the
/// caller points somewhere else explicitly.
fn video_base_url(rt: &Runtime, host: Option<&str>, port: Option<u16>) -> String {
    match (host, port) {
        (None, None) => rt.base_url().to_string(),
        (host, port) => format!(
            "http://{}:{}",
            host.unwrap_or(VIDEO_DEFAULT_HOST),
            port.unwrap_or(VIDEO_DEFAULT_PORT)
        ),
    }
}

/// Local file → base64 data URL; an http(s) or `data:` URL passes through
/// untouched (9router `imageInputToUrl`, xaiVideo.js:88-94).
fn video_image_input(input: &str) -> anyhow::Result<String> {
    if input.starts_with("http://") || input.starts_with("https://") || input.starts_with("data:") {
        return Ok(input.to_string());
    }
    let raw = std::fs::read(input).map_err(|e| anyhow::anyhow!("read --image {input}: {e}"))?;
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    let mime = match std::path::Path::new(input)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        _ => "image/jpeg",
    };
    Ok(format!("data:{mime};base64,{}", B64.encode(&raw)))
}

/// The download URL 9router accepts from a finished job (xaiVideo.js:265).
fn video_result_url(job: &Value) -> Option<&str> {
    job.get("video")
        .and_then(|v| {
            v.get("url")
                .or_else(|| v.get("file_output").and_then(|f| f.get("public_url")))
        })
        .and_then(Value::as_str)
}

/// Stream `url` into `<output>.part`, then rename it over `output`.
///
/// A partially written MP4 is worse than no MP4: a player that opens the file
/// while the job is still downloading sees a truncated stream. 9router
/// unlinks the `.part` on every failure path (xaiVideo.js:184-215) and so does
/// this, including on Ctrl+C.
async fn download_video(url: &str, output: &str) -> anyhow::Result<()> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let part = format!("{output}.part");
    let fetch = async {
        let response = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(VIDEO_HTTP_TIMEOUT_SEC))
            .build()?
            .get(url)
            .send()
            .await?;
        if !response.status().is_success() {
            anyhow::bail!("download failed: HTTP {}", response.status());
        }
        let mut file = tokio::fs::File::create(&part).await?;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            file.write_all(&chunk?).await?;
        }
        file.flush().await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(e) = fetch {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, output)?;
    Ok(())
}

/// The first line worth showing a human when the gateway refuses the job.
fn video_error_detail(reply: &VideoReply) -> String {
    reply
        .json()
        .and_then(|body| {
            body.pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            if reply.body.trim().is_empty() {
                format!("HTTP {}", reply.status.as_u16())
            } else {
                reply.body.chars().take(500).collect()
            }
        })
}

async fn run_video(
    cfg: &ResolvedConfig,
    rt: &Runtime,
    ctx: OutputCtx,
    job: VideoJob,
) -> anyhow::Result<i32> {
    let client = VideoClient::new(
        video_base_url(rt, job.host.as_deref(), job.port),
        cfg.api_key
            .clone()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty()),
    )?;

    let mut body = json!({ "model": job.model, "prompt": job.prompt });
    if let Some(obj) = body.as_object_mut() {
        if let Some(duration) = job.duration {
            obj.insert("duration".to_string(), json!(duration));
        }
        if let Some(ratio) = &job.aspect_ratio {
            obj.insert("aspect_ratio".to_string(), json!(ratio));
        }
        if let Some(resolution) = &job.resolution {
            obj.insert("resolution".to_string(), json!(resolution));
        }
        if let Some(image) = &job.image {
            obj.insert(
                "image".to_string(),
                json!({ "url": video_image_input(image)? }),
            );
        }
    }

    humanln(ctx, format!("Requesting video ({})…", job.model));
    let create = client.create(&body).await?;
    if !create.status.is_success() {
        let detail = video_error_detail(&create);
        let hint = if create.status == reqwest::StatusCode::BAD_REQUEST
            && detail.to_ascii_lowercase().contains("no credentials")
        {
            " — connect an account first: dashboard → Providers."
        } else {
            ""
        };
        return Ok(emit_error(
            ctx,
            "other",
            &format!("video create failed: {detail}{hint}"),
        )?);
    }
    let Some(request_id) = create
        .json()
        .as_ref()
        .and_then(|body| body.get("request_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Ok(emit_error(
            ctx,
            "decode",
            "video create succeeded but returned no request_id",
        )?);
    };
    // The job is account-bound upstream. Without this every poll could land on
    // a different account and the provider would answer "unknown request_id".
    let pin = create.connection_id.as_deref();
    humanln(ctx, format!("Job accepted: {request_id}"));

    let interval = std::time::Duration::from_millis(job.poll_interval_ms.max(1));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(job.timeout_sec);
    let mut announced = String::new();
    let result = loop {
        let reply = tokio::select! {
            polled = client.poll(&request_id, pin) => polled?,
            _ = tokio::signal::ctrl_c() => {
                let _ = std::fs::remove_file(format!("{}.part", job.output));
                return Ok(130);
            }
        };
        if reply.status.is_success() {
            if let Some(payload) = reply.json() {
                let status = payload
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if VIDEO_FAILED_STATUSES.contains(&status.as_str()) {
                    return Ok(emit_error(
                        ctx,
                        "other",
                        &format!("job {request_id} failed: {}", video_error_detail(&reply)),
                    )?);
                }
                if VIDEO_TERMINAL_STATUSES.contains(&status.as_str()) {
                    break payload;
                }
                // A ten-minute job with no output reads as a hang, so announce
                // each state once instead of every poll (xaiVideo.js:150).
                let progress = payload
                    .get("progress")
                    .and_then(Value::as_f64)
                    .map(|p| format!(" {p}%"))
                    .unwrap_or_default();
                let line = format!(
                    "{}{progress}",
                    if status.is_empty() {
                        "pending"
                    } else {
                        &status
                    }
                );
                if line != announced {
                    announced = line.clone();
                    humanln(ctx, line);
                }
            }
        } else if reply.status.as_u16() != 429 && reply.status.as_u16() != 503 {
            // 429/503 mean "try again"; anything else will not get better.
            return Ok(emit_error(
                ctx,
                "other",
                &format!("polling failed: {}", video_error_detail(&reply)),
            )?);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(emit_error(
                ctx,
                "other",
                &format!(
                    "timed out after {}s waiting for {request_id}",
                    job.timeout_sec
                ),
            )?);
        }
        tokio::time::sleep(interval).await;
    };

    let Some(url) = video_result_url(&result) else {
        return Ok(emit_error(
            ctx,
            "other",
            "job finished but returned no video URL",
        )?);
    };
    humanln(ctx, "Downloading…");
    let downloaded = tokio::select! {
        result = download_video(url, &job.output) => result,
        _ = tokio::signal::ctrl_c() => {
            let _ = std::fs::remove_file(format!("{}.part", job.output));
            return Ok(130);
        }
    };
    if let Err(e) = downloaded {
        return Ok(emit_error(ctx, "network", &format!("{e}"))?);
    }

    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.media.video",
            json!({ "request_id": request_id, "output": job.output }),
        )?;
    } else {
        humanln(ctx, format!("Saved {}", job.output));
    }
    Ok(0)
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

    /// Parse `openproxy media video …` far enough to read the job out. The
    /// top-level `Cli` is what the route table is built from, and `Media` is
    /// its own subcommand enum, so the same two-step walk exercises the real
    /// wiring rather than a hand-rolled sub-parser.
    fn parse_video(argv: &[&str]) -> MediaCmd {
        use clap::Parser;
        let mut full = vec!["openproxy", "media", "video"];
        full.extend_from_slice(argv);
        match crate::cli::Cli::try_parse_from(&full)
            .expect("media video should parse")
            .cmd
            .expect("a subcommand is required")
        {
            crate::cli::Command::Media { cmd } => cmd,
            other => panic!("expected a media command, got {other:?}"),
        }
    }

    fn video_fields(cmd: MediaCmd) -> (String, String, String, u64, Option<String>, Option<u16>) {
        match cmd {
            MediaCmd::Video {
                prompt,
                output,
                model,
                timeout,
                host,
                port,
                ..
            } => (prompt, output, model, timeout, host, port),
            other => panic!("expected a video command, got {other:?}"),
        }
    }

    #[test]
    fn media_video_parses_the_9router_option_set() {
        let cmd = parse_video(&[
            "--prompt",
            "a neon city",
            "--output",
            "v.mp4",
            "--model",
            "xai/grok-imagine-video",
            "--duration",
            "8",
            "--aspect-ratio",
            "16:9",
            "--resolution",
            "720p",
            "--image",
            "a.png",
            "--timeout",
            "120",
        ]);
        match cmd {
            MediaCmd::Video {
                prompt,
                output,
                model,
                duration,
                aspect_ratio,
                resolution,
                image,
                timeout,
                poll_interval_ms,
                ..
            } => {
                assert_eq!(prompt, "a neon city");
                assert_eq!(output, "v.mp4");
                assert_eq!(model, "xai/grok-imagine-video");
                assert_eq!(duration, Some(8));
                assert_eq!(aspect_ratio.as_deref(), Some("16:9"));
                assert_eq!(resolution.as_deref(), Some("720p"));
                assert_eq!(image.as_deref(), Some("a.png"));
                assert_eq!(timeout, 120);
                assert_eq!(poll_interval_ms, VIDEO_DEFAULT_POLL_INTERVAL_MS);
            }
            other => panic!("expected a video command, got {other:?}"),
        }
    }

    #[test]
    fn media_video_requires_a_prompt() {
        use clap::Parser;
        let err = crate::cli::Cli::try_parse_from(["openproxy", "media", "video"])
            .expect_err("media video without --prompt is a usage error");
        assert!(
            err.kind() == clap::error::ErrorKind::MissingRequiredArgument,
            "expected a missing-argument error, got: {err}"
        );
    }

    /// 9router's `xaiVideo.js:18-20` defaults, with the port moved onto
    /// OpenProxy's own 4623 — 9router's 20128 is a launcher convention, not a
    /// contract, and copying it would point every user at a dead socket.
    #[test]
    fn media_video_defaults_match_xai_video_js() {
        let (prompt, output, model, timeout, host, port) =
            video_fields(parse_video(&["--prompt", "x"]));
        assert_eq!(prompt, "x");
        assert_eq!(output, "video.mp4");
        assert_eq!(model, "xai/grok-imagine-video");
        assert_eq!(timeout, 600);
        assert_eq!(host, None, "an unset --host follows the resolved runtime");
        assert_eq!(port, None, "an unset --port follows the resolved runtime");
        assert_eq!(VIDEO_DEFAULT_HOST, "127.0.0.1");
        assert_eq!(VIDEO_DEFAULT_PORT, 4623);
    }

    #[test]
    fn media_video_host_and_port_are_opt_in_overrides() {
        let (_, _, _, _, host, port) = video_fields(parse_video(&[
            "--prompt", "x", "--host", "10.0.0.5", "--port", "9999",
        ]));
        assert_eq!(host.as_deref(), Some("10.0.0.5"));
        assert_eq!(port, Some(9999));
    }

    #[test]
    fn media_video_terminal_statuses_match_xai_video_js() {
        assert_eq!(
            VIDEO_TERMINAL_STATUSES,
            [
                "done",
                "failed",
                "completed",
                "error",
                "expired",
                "cancelled"
            ]
        );
        assert_eq!(
            VIDEO_FAILED_STATUSES,
            ["failed", "error", "expired", "cancelled"]
        );
    }

    /// The finished job may name the file either way; 9router reads both
    /// (xaiVideo.js:265), so a provider that answers with the OpenRouter
    /// `file_output.public_url` shape still yields a download.
    #[test]
    fn video_result_url_reads_both_finished_job_shapes() {
        let direct = json!({"video": {"url": "https://cdn.test/a.mp4"}});
        assert_eq!(video_result_url(&direct), Some("https://cdn.test/a.mp4"));
        let file_output =
            json!({"video": {"file_output": {"public_url": "https://cdn.test/b.mp4"}}});
        assert_eq!(
            video_result_url(&file_output),
            Some("https://cdn.test/b.mp4")
        );
        assert_eq!(video_result_url(&json!({"status": "done"})), None);
    }

    /// A local path is inlined as a data URL; a URL the caller already has is
    /// forwarded untouched, so the caller keeps control of remote fetches.
    #[test]
    fn video_image_input_passes_urls_through_and_encodes_files() {
        assert_eq!(
            video_image_input("https://cdn.test/a.png").unwrap(),
            "https://cdn.test/a.png"
        );
        assert_eq!(
            video_image_input("data:image/png;base64,AA==").unwrap(),
            "data:image/png;base64,AA=="
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("frame.webp");
        std::fs::write(&path, b"RIFF....WEBP").expect("write frame");
        let inlined = video_image_input(path.to_str().unwrap()).expect("encode frame");
        assert!(
            inlined.starts_with("data:image/webp;base64,"),
            "a .webp must be typed image/webp, got: {inlined}"
        );
        assert!(video_image_input("/nonexistent/frame.png").is_err());
    }

    mod flow {
        use super::*;
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn client(server: &MockServer) -> VideoClient {
            VideoClient::new(server.uri(), Some("sk-test".into())).expect("client")
        }

        /// The create answer names the account in a header and the polls have
        /// to carry it back, or a multi-account gateway hands the poll to a
        /// provider that has never heard of the job. 9router writes
        /// `x-9router-connection-id` (videoGeneration.js:97-104); OpenProxy
        /// also writes `x-openproxy-connection-id`, so a client has to accept
        /// whichever one it is handed.
        #[tokio::test]
        async fn video_client_reads_either_connection_header_spelling() {
            for name in ["x-9router-connection-id", "x-openproxy-connection-id"] {
                let server = MockServer::start().await;
                Mock::given(method("POST"))
                    .and(path("/v1/videos/generations"))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .insert_header(name, "conn-a")
                            .set_body_json(json!({"request_id": "job-1"})),
                    )
                    .mount(&server)
                    .await;

                let create = client(&server)
                    .create(&json!({"model": "xai/grok-imagine-video", "prompt": "hi"}))
                    .await
                    .expect("create");
                assert_eq!(create.connection_id.as_deref(), Some("conn-a"));

                // The poll must go out with the pin, and the gateway's own
                // bearer — the key is never echoed into the request path.
                Mock::given(method("GET"))
                    .and(path("/v1/videos/job-1"))
                    .and(header("x-connection-id", "conn-a"))
                    .and(header("authorization", "Bearer sk-test"))
                    .respond_with(
                        ResponseTemplate::new(200).set_body_json(json!({"status": "queued"})),
                    )
                    .expect(1)
                    .mount(&server)
                    .await;
                let poll = client(&server)
                    .poll("job-1", create.connection_id.as_deref())
                    .await
                    .expect("poll");
                assert_eq!(poll.status, reqwest::StatusCode::OK);
                assert_eq!(poll.json().unwrap()["status"], "queued");
            }
        }

        /// A job id is caller-visible, so it has to be escaped rather than
        /// pasted into the path.
        #[tokio::test]
        async fn video_client_escapes_the_request_id() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/v1/videos/a%2Fb"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "done"})))
                .expect(1)
                .mount(&server)
                .await;
            let reply = client(&server).poll("a/b", None).await.expect("poll");
            assert!(reply.status.is_success());
        }

        /// The download must land whole: bytes stream into `<output>.part` and
        /// only a finished file is renamed over the target, so a player never
        /// opens a half-written MP4 (xaiVideo.js:184-215).
        #[tokio::test]
        async fn download_writes_the_part_file_then_renames() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/clip.mp4"))
                .respond_with(ResponseTemplate::new(200).set_body_raw(b"ID3mp4bytes", "video/mp4"))
                .mount(&server)
                .await;

            let dir = tempfile::tempdir().expect("tempdir");
            let output = dir.path().join("clip.mp4");
            download_video(
                &format!("{}/clip.mp4", server.uri()),
                output.to_str().unwrap(),
            )
            .await
            .expect("download");

            assert_eq!(std::fs::read(&output).unwrap(), b"ID3mp4bytes");
            assert!(
                !dir.path().join("clip.mp4.part").exists(),
                "the temp file must not survive a successful download"
            );
        }

        /// A failed download must not leave a partial file where the caller
        /// expects a whole one.
        #[tokio::test]
        async fn download_removes_the_part_file_on_failure() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/clip.mp4"))
                .respond_with(ResponseTemplate::new(503))
                .mount(&server)
                .await;

            let dir = tempfile::tempdir().expect("tempdir");
            let output = dir.path().join("clip.mp4");
            let err = download_video(
                &format!("{}/clip.mp4", server.uri()),
                output.to_str().unwrap(),
            )
            .await
            .expect_err("a 503 is not a video");
            assert!(err.to_string().contains("503"), "got: {err}");
            assert!(!output.exists());
            assert!(!dir.path().join("clip.mp4.part").exists());
        }
    }
}
