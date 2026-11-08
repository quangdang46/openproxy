//! `openproxy connect` / `openproxy show` — port of 9router
//! `cli/src/cli/commands/connect.js` + `show.js`.
//!
//! `connect <server-url>` points the CLI tools on THIS machine (Claude Code,
//! Codex, OpenCode, …) at a remote openproxy/9router server: it logs in with
//! the dashboard password (or reuses/creates an API key), reads the server's
//! own cli-tools config for model choices, then writes each tool's local
//! settings file (see `connect_tools`). `--dry-run` computes the same file
//! payloads without writing them. `show` reads those local files back
//! offline — it never contacts a server and never prints a full API key.

use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use clap::Args;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, COOKIE};
use reqwest::{Client, Method, StatusCode};
use serde_json::{json, Value};

use crate::cli::connect_tools::{
    ApplyCtx, ConnectTool, CLAUDE_MODELS,
};
use crate::cli::config::ResolvedConfig;
use crate::cli::output::{emit_error, emit_robot, humanln, OutputCtx};

#[derive(Debug, Clone, Args)]
pub struct ConnectArgs {
    /// Remote server URL, e.g. http://host:4623 (or `host:4623`).
    /// Optional with `--reset`.
    pub url: Option<String>,

    /// Comma-separated tools to configure (default: claude).
    /// Supported: claude, codex, opencode, droid, crush, kilo, cline, pi, omp, all.
    #[arg(long, value_delimiter = ',')]
    pub tools: Option<Vec<String>>,

    /// Dashboard password (or env OPENPROXY_PASSWORD / NINE_ROUTER_PASSWORD;
    /// prompted when omitted in a terminal).
    #[arg(long, hide_env_values = true)]
    pub password: Option<String>,

    /// After a successful login, save the password for this server to
    /// <data-dir>/connect.env (mode 600) so later runs skip the prompt.
    #[arg(long)]
    pub save: bool,

    /// API key name to reuse/create on the server.
    #[arg(long)]
    pub key_name: Option<String>,

    /// Use this API key; skip login + key lookup.
    #[arg(long, hide_env_values = true)]
    pub api_key: Option<String>,

    /// Model for all non-Claude tools.
    #[arg(long)]
    pub model: Option<String>,

    /// Override the Claude Code "fable" tier model.
    #[arg(long)]
    pub fable: Option<String>,
    /// Override the Claude Code "opus" tier model.
    #[arg(long)]
    pub opus: Option<String>,
    /// Override the Claude Code "sonnet" tier model.
    #[arg(long)]
    pub sonnet: Option<String>,
    /// Override the Claude Code "haiku" tier model.
    #[arg(long)]
    pub haiku: Option<String>,

    /// Also print OpenAI-compatible env vars for other CLIs.
    #[arg(long)]
    pub print_env: bool,

    /// Remove our settings from the selected tools and exit.
    #[arg(long)]
    pub reset: bool,

    /// Compute the local config payloads without writing any files.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Args)]
pub struct ShowArgs {
    /// Tools to inspect (positional; same names as `connect --tools`).
    pub tools: Vec<String>,

    /// Comma-separated tools (same as positional names).
    #[arg(long, value_delimiter = ',')]
    pub tools_opt: Option<Vec<String>>,

    /// Machine-readable JSON output (API key still masked).
    #[arg(long)]
    pub json: bool,
}

/// Mask an API key for display: first 6 + last 4 chars (port of `maskKey`).
pub fn mask_key(key: &str) -> String {
    if key.chars().count() < 12 {
        return "****".to_string();
    }
    let head: String = key.chars().take(6).collect();
    let tail: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

/// `http://` added when missing; only the origin is kept (port of
/// `normalizeServerUrl`).
pub fn normalize_server_url(input: &str) -> Result<String> {
    let mut raw = input.trim().to_string();
    if !raw.to_ascii_lowercase().starts_with("http://")
        && !raw.to_ascii_lowercase().starts_with("https://")
    {
        raw = format!("http://{raw}");
    }
    let url = url::Url::parse(&raw).with_context(|| format!("invalid server URL `{input}`"))?;
    Ok(url.origin().ascii_serialization())
}

fn default_key_name() -> String {
    let host = whoami::fallible::hostname().unwrap_or_else(|_| "localhost".to_string());
    format!("cli-{host}").chars().take(64).collect()
}

// ── saved password (<data-dir>/connect.env) ─────────────────────────────────

fn saved_env_path(cfg: &ResolvedConfig) -> PathBuf {
    cfg.data_dir.join("connect.env")
}

fn load_saved_password(cfg: &ResolvedConfig, server: &str) -> Option<String> {
    let text = std::fs::read_to_string(saved_env_path(cfg)).ok()?;
    let mut env = std::collections::HashMap::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        if !k.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
            continue;
        }
        // Values are written JSON-quoted; accept a hand-edited bare value too.
        let parsed = serde_json::from_str::<Value>(v.trim())
            .ok()
            .and_then(|j| j.as_str().map(str::to_string))
            .unwrap_or_else(|| v.trim().to_string());
        env.insert(k.to_string(), parsed);
    }
    (env.get("OPENPROXY_SERVER").map(String::as_str) == Some(server))
        .then(|| env.get("OPENPROXY_PASSWORD").cloned())
        .flatten()
}

fn save_password(cfg: &ResolvedConfig, server: &str, password: &str) -> Result<PathBuf> {
    let file = saved_env_path(cfg);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = format!(
        "OPENPROXY_SERVER={}\nOPENPROXY_PASSWORD={}\n",
        serde_json::to_string(server)?,
        serde_json::to_string(password)?
    );
    std::fs::write(&file, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn password_from_env() -> Option<String> {
    std::env::var("OPENPROXY_PASSWORD")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("NINE_ROUTER_PASSWORD")
                .ok()
                .filter(|s| !s.is_empty())
        })
}

/// Prompt for the dashboard password without echoing it (TTY only).
fn prompt_password() -> Result<String> {
    if !std::io::stdin().is_terminal() {
        return Err(anyhow!(
            "Password required: pass --password or set OPENPROXY_PASSWORD"
        ));
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        eprint!("openproxy dashboard password: ");
        let _ = std::io::stderr().flush();
        return read_password_no_echo();
    }
    #[cfg(not(unix))]
    {
        use std::io::Write;
        eprint!("openproxy dashboard password: ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .context("read password")?;
        Ok(line.trim_end_matches(['\r', '\n']).to_string())
    }
}

/// Read a line from the TTY with echo disabled (unix termios via libc).
#[cfg(unix)]
fn read_password_no_echo() -> Result<String> {
    use std::io::Write;
    // Prefer /dev/tty so piping stdin still works; fall back to stdin.
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty");
    let mut input: Box<dyn std::io::Read> = match tty {
        Ok(f) => Box::new(f),
        Err(_) => Box::new(std::io::stdin()),
    };
    // SAFETY: termios calls on fd 0/tty; we restore before returning.
    unsafe {
        let fd = libc::STDIN_FILENO;
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut termios) == 0 {
            let orig = termios;
            termios.c_lflag &= !libc::ECHO;
            libc::tcsetattr(fd, libc::TCSANOW, &termios);
            let mut buf = String::new();
            // read_line needs BufRead; wrap manually.
            let mut byte = [0u8; 1];
            loop {
                match std::io::Read::read(&mut input, &mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if byte[0] == b'\n' || byte[0] == b'\r' {
                            break;
                        }
                        buf.push(byte[0] as char);
                    }
                }
            }
            let _ = std::io::stderr().write_all(b"\n");
            libc::tcsetattr(fd, libc::TCSANOW, &orig);
            return Ok(buf);
        }
    }
    // Fallback: plain stdin read.
    let mut line = String::new();
    std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line)
        .context("read password")?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

// ── HTTP helpers (cookie session OR api-key auth) ───────────────────────────

struct ServerClient {
    client: Client,
    base: String,
    cookie: Option<String>,
    api_key: Option<String>,
}

impl ServerClient {
    fn new(base: String) -> Result<Self> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("build http client")?;
        Ok(Self {
            client,
            base,
            cookie: None,
            api_key: None,
        })
    }

    fn with_auth(mut self, cookie: Option<String>, api_key: Option<String>) -> Self {
        self.cookie = cookie;
        self.api_key = api_key;
        self
    }

    fn headers(&self, json_body: bool) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(ACCEPT, HeaderValue::from_static("application/json"));
        if json_body {
            h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }
        if let Some(c) = &self.cookie {
            if let Ok(v) = HeaderValue::from_str(c) {
                h.insert(COOKIE, v);
            }
        }
        if let Some(k) = &self.api_key {
            if let Ok(v) = HeaderValue::from_str(k) {
                h.insert("x-api-key", v.clone());
                if let Ok(b) = HeaderValue::from_str(&format!("Bearer {k}")) {
                    h.insert(AUTHORIZATION, b);
                }
            }
        }
        h
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(StatusCode, HeaderMap, Option<Value>), String> {
        let url = format!("{}{}", self.base, path);
        let mut req = self
            .client
            .request(method, &url)
            .headers(self.headers(body.is_some()));
        if let Some(b) = body {
            req = req.json(b);
        }
        let res = req.send().await.map_err(|e| {
            format!(
                "Cannot reach {}: {}",
                url::Url::parse(&url)
                    .map(|u| u.origin().ascii_serialization())
                    .unwrap_or_else(|_| url.clone()),
                e
            )
        })?;
        let status = res.status();
        let headers = res.headers().clone();
        let data = res.json::<Value>().await.ok();
        Ok((status, headers, data))
    }

    async fn get_json(&self, path: &str) -> Result<Option<Value>, String> {
        let (status, _, data) = self.request(Method::GET, path, None).await?;
        if !status.is_success() {
            return Err(format!("GET {path} → HTTP {status}"));
        }
        Ok(data)
    }
}

fn extract_auth_cookie(headers: &HeaderMap) -> Option<String> {
    for v in headers.get_all(reqwest::header::SET_COOKIE) {
        if let Ok(s) = v.to_str() {
            for part in s.split(',') {
                let part = part.trim();
                if let Some(rest) = part.strip_prefix("auth_token=") {
                    let token = rest.split(';').next().unwrap_or("");
                    if !token.is_empty() {
                        return Some(format!("auth_token={token}"));
                    }
                }
            }
        }
    }
    None
}

async fn login(sc: &ServerClient, password: &str) -> Result<String> {
    let (status, headers, data) = sc
        .request(
            Method::POST,
            "/api/auth/login",
            Some(&json!({"password": password})),
        )
        .await
        .map_err(|e| anyhow!(e))?;
    let ok = status == StatusCode::OK
        && data
            .as_ref()
            .and_then(|d| d.get("success"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if !ok {
        let msg = data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        // Strip control chars (terminal escape injection) like upstream.
        let msg: String = msg.chars().filter(|c| !c.is_control()).collect();
        return Err(anyhow!("Login failed ({status}): {msg}"));
    }
    extract_auth_cookie(&headers).ok_or_else(|| {
        anyhow!("Login succeeded but server returned no session cookie")
    })
}

struct ApiKeyResult {
    key: String,
    created: bool,
}

async fn get_or_create_api_key(sc: &ServerClient, key_name: &str) -> Result<ApiKeyResult> {
    match sc.request(Method::GET, "/api/keys", None).await {
        Ok((status, _, Some(data))) if status.is_success() => {
        if let Some(keys) = data.get("keys").and_then(Value::as_array) {
            let existing = keys.iter().find(|k| {
                k.get("name").and_then(Value::as_str) == Some(key_name)
                    && k.get("isActive").and_then(Value::as_bool) != Some(false)
            });
            if let Some(k) = existing {
                if let Some(key) = k.get("key").and_then(Value::as_str) {
                    return Ok(ApiKeyResult {
                        key: key.to_string(),
                        created: false,
                    });
                }
            }
        }
        }
        Ok((status, _, _)) => {
            return Err(anyhow!("Failed to list API keys (HTTP {status})"));
        }
        Err(e) => return Err(anyhow!(e)),
    }
    let (status, _, data) = sc
        .request(
            Method::POST,
            "/api/keys",
            Some(&json!({"name": key_name})),
        )
        .await
        .map_err(|e| anyhow!(e))?;
    let key = data
        .as_ref()
        .and_then(|d| d.get("key"))
        .and_then(Value::as_str)
        .map(str::to_string);
    match (status, key) {
        (StatusCode::CREATED, Some(key)) | (StatusCode::OK, Some(key)) => Ok(ApiKeyResult {
            key,
            created: true,
        }),
        _ => {
            let msg = data
                .as_ref()
                .and_then(|d| d.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("");
            Err(anyhow!(
                "Failed to create API key ({status}): {msg}"
            ))
        }
    }
}

// ── server model inheritance (fetchServerModels) ────────────────────────────

/// Extract the tool's own model from a `GET /api/cli-tools/<route>` payload.
fn server_model_for(tool: ConnectTool, data: &Value) -> Option<String> {
    match tool {
        ConnectTool::Codex => codex_server_model(data),
        ConnectTool::OpenCode => opencode_server_model(data),
        ConnectTool::Droid => droid_server_model(data),
        ConnectTool::Crush => crush_server_model(data),
        ConnectTool::Cline => cline_server_model(data),
        ConnectTool::Pi => pi_server_model(data),
        _ => None,
    }
}

fn tv_get<'a>(v: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    v.as_table()?.get(key)
}

fn codex_server_model(data: &Value) -> Option<String> {
    let text = data.get("config")?.as_str()?;
    let cfg: toml::Value = toml::from_str(text).ok()?;
    let provider = tv_get(&cfg, "model_provider")?.as_str()?;
    if ["openproxy", "9router"].contains(&provider) {
        tv_get(&cfg, "model")?.as_str().map(str::to_string)
    } else {
        None
    }
}

fn opencode_server_model(data: &Value) -> Option<String> {
    if let Some(m) = data
        .get("config")
        .and_then(|c| c.get("model"))
        .and_then(Value::as_str)
    {
        for p in ["openproxy/", "9router/"] {
            if let Some(rest) = m.strip_prefix(p) {
                return Some(rest.to_string());
            }
        }
    }
    data.get("opencode")
        .and_then(|o| o.get("activeModel"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn droid_server_model(data: &Value) -> Option<String> {
    data.get("settings")?
        .get("customModels")?
        .as_array()?
        .iter()
        .find(|m| {
            m.get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| {
                    id.starts_with("custom:OpenProxy") || id.starts_with("custom:9Router")
                })
        })?
        .get("model")?
        .as_str()
        .map(str::to_string)
}

fn crush_server_model(data: &Value) -> Option<String> {
    let providers = data.get("config")?.get("providers")?.as_object()?;
    let p = ["openproxy", "9router"]
        .iter()
        .find_map(|k| providers.get(*k))?;
    p.get("models")?
        .as_array()?
        .first()?
        .get("id")?
        .as_str()
        .map(str::to_string)
}

fn cline_server_model(data: &Value) -> Option<String> {
    let st = data.get("settings")?;
    if st.get("actModeApiProvider")?.as_str()? == "openai" {
        st.get("openAiModelId")?.as_str().map(str::to_string)
    } else {
        None
    }
}

fn pi_server_model(data: &Value) -> Option<String> {
    let providers = data.get("config")?.get("providers")?.as_object()?;
    let p = ["openproxy", "9router"]
        .iter()
        .find_map(|k| providers.get(*k))?;
    let m = p.get("models")?.as_array()?.first()?;
    match m {
        Value::String(s) => Some(s.clone()),
        other => other.get("id")?.as_str().map(str::to_string),
    }
}

struct ServerModels {
    /// claude flag -> model (only set tiers).
    claude: BTreeMap<String, String>,
    /// tool id -> that tool's own server model.
    by_tool: BTreeMap<String, String>,
    /// Fallback shared model (server's opencode model).
    shared: Option<String>,
}

/// Best-effort: any failure returns empty maps and built-in defaults apply.
async fn fetch_server_models(
    sc: &ServerClient,
    tools: &[ConnectTool],
) -> ServerModels {
    let mut out = ServerModels {
        claude: BTreeMap::new(),
        by_tool: BTreeMap::new(),
        shared: None,
    };
    // Claude tiers live in the server's claude-settings env block.
    if tools.contains(&ConnectTool::Claude) {
        if let Ok(Some(data)) = sc.get_json("/api/cli-tools/claude-settings").await {
            if let Some(env) = data
                .get("settings")
                .and_then(|s| s.get("env"))
                .and_then(Value::as_object)
            {
                for &(flag, env_key) in CLAUDE_MODELS {
                    if let Some(m) = env.get(env_key).and_then(Value::as_str) {
                        if !m.is_empty() {
                            out.claude.insert(flag.to_string(), m.to_string());
                        }
                    }
                }
            }
        }
    }
    // Each tool's own config; opencode doubles as the shared fallback, so fetch
    // it even when opencode itself wasn't selected.
    let mut wanted: Vec<ConnectTool> = tools.iter().copied().filter(|t| t.route().is_some()).collect();
    let opencode_selected = wanted.contains(&ConnectTool::OpenCode);
    if !opencode_selected && tools.iter().any(|t| t.needs_model()) {
        wanted.push(ConnectTool::OpenCode);
    }
    wanted.sort();
    wanted.dedup();
    for tool in wanted {
        let Some(route) = tool.route() else { continue };
        let path = format!("/api/cli-tools/{route}");
        let Ok(Some(data)) = sc.get_json(&path).await else { continue };
        if let Some(model) = server_model_for(tool, &data) {
            out.by_tool.insert(tool.id().to_string(), model);
        }
    }
    out.shared = out.by_tool.get("opencode").cloned();
    out
}

/// `GET /v1/models` → set of advertised ids (None when unavailable).
async fn list_models(sc: &ServerClient) -> Option<BTreeSet<String>> {
    let (status, _, data) = sc
        .request(Method::GET, "/v1/models", None)
        .await
        .ok()?;
    if status == StatusCode::UNAUTHORIZED {
        return None;
    }
    if !status.is_success() {
        return None;
    }
    Some(
        data.and_then(|d| d.get("data").and_then(Value::as_array).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string))
            .collect(),
    )
}

// ── connect ─────────────────────────────────────────────────────────────────

pub async fn run_connect(
    args: ConnectArgs,
    cfg: &ResolvedConfig,
    ctx: OutputCtx,
) -> Result<i32> {
    // Pick tools before any network call: bad names fail fast.
    let tools = select_tools(&args, ctx)?;

    if args.reset {
        return run_reset(&tools, ctx);
    }

    let Some(url) = args.url.as_deref() else {
        return Ok(emit_error(
            ctx,
            "usage",
            "connect requires <server-url> (or pass --reset)",
        )?);
    };
    let server = normalize_server_url(url)?;
    let is_remote_http = server.starts_with("http://")
        && !server.starts_with("http://localhost")
        && !server.starts_with("http://127.0.0.1")
        && !server.starts_with("http://[::1]");
    if is_remote_http {
        humanln(
            ctx,
            "⚠ Plain HTTP: password and API key travel unencrypted. Use only on a trusted LAN/VPN.",
        );
    }

    // Auth: provided key, else dashboard login (password flag / saved / prompt).
    let mut api_key: Option<String> = args.api_key.clone();
    let mut cookie: Option<String> = None;
    if api_key.is_some() {
        humanln(ctx, "• Using provided API key");
    } else {
        let saved = if args.password.is_none() {
            load_saved_password(cfg, &server)
        } else {
            None
        };
        if saved.is_some() {
            humanln(
                ctx,
                format!("• Using saved password ({})", saved_env_path(cfg).display()),
            );
        }
        let password = match args.password.clone().or(saved.clone()) {
            Some(p) => p,
            None => prompt_password()?,
        };
        humanln(ctx, format!("• Logging in to {server}"));
        let login_client = ServerClient::new(server.clone())?.with_auth(None, None);
        match login(&login_client, &password).await {
            Ok(c) => {
                cookie = Some(c);
                if args.save && Some(&password) != saved.as_ref() {
                    match save_password(cfg, &server, &password) {
                        Ok(f) => humanln(
                            ctx,
                            format!(
                                "• Saved password to {} (plain text, mode 600)",
                                f.display()
                            ),
                        ),
                        Err(e) => humanln(ctx, format!("• Could not save password: {e}")),
                    }
                }
            }
            Err(e) => {
                let msg = e.to_string();
                if saved.is_some() {
                    return Err(anyhow!(
                        "{msg} — saved password may be stale; delete {} or pass --password",
                        saved_env_path(cfg).display()
                    ));
                }
                return Err(e);
            }
        }
        let authed = ServerClient::new(server.clone())?
            .with_auth(cookie.clone(), None);
        let key_name = args
            .key_name
            .clone()
            .unwrap_or_else(default_key_name);
        match get_or_create_api_key(&authed, &key_name).await {
            Ok(r) => {
                api_key = Some(r.key.clone());
                humanln(
                    ctx,
                    format!(
                        "• {} API key \"{key_name}\" ({})",
                        if r.created { "Created" } else { "Reusing" },
                        mask_key(&r.key)
                    ),
                );
            }
            Err(e) => return Err(e),
        }
    }

    let api_key = api_key.expect("api key resolved");
    let sc = ServerClient::new(server.clone())?.with_auth(cookie.clone(), Some(api_key.clone()));

    // Models come from the server's own cli-tools config; flags override.
    let server_models = fetch_server_models(&sc, &tools).await;
    if cookie.is_none() {
        humanln(
            ctx,
            "• No dashboard login: server model config may be unreadable — pass --model if needed",
        );
    }

    let available = list_models(&sc).await;
    if let (Some(set), Some(m)) = (&available, args.model.as_deref()) {
        if !set.contains(m) {
            humanln(
                ctx,
                format!("⚠ model: \"{m}\" not listed by server — override with --model <model>"),
            );
        }
    }

    // Claude tiers: flag > server value > unset.
    let mut claude_models: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut claude_unset: Vec<&str> = Vec::new();
    if tools.contains(&ConnectTool::Claude) {
        for &(flag, env_key) in CLAUDE_MODELS {
            let flag_model = match flag {
                "fable" => args.fable.as_deref(),
                "opus" => args.opus.as_deref(),
                "sonnet" => args.sonnet.as_deref(),
                "haiku" => args.haiku.as_deref(),
                _ => None,
            };
            let model = flag_model
                .map(str::to_string)
                .or_else(|| server_models.claude.get(flag).cloned());
            if let Some(m) = &model {
                if let Some(set) = &available {
                    let bare = m.strip_suffix("[1m]").unwrap_or(m);
                    if !set.contains(bare) {
                        humanln(
                            ctx,
                            format!(
                                "⚠ claude {flag}: \"{m}\" not listed by server — override with --{flag} <model>"
                            ),
                        );
                    }
                }
            } else {
                claude_unset.push(flag);
            }
            claude_models.insert(env_key.to_string(), model);
        }
    }

    // Other tools: --model > tool's own server model > server's opencode model.
    let mut tool_models: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut from_opencode: Vec<String> = Vec::new();
    for t in &tools {
        if !t.needs_model() {
            continue;
        }
        let own = server_models.by_tool.get(t.id()).cloned();
        let model = args
            .model
            .clone()
            .or(own.clone())
            .or_else(|| server_models.shared.clone());
        if args.model.is_none() && own.is_none() && server_models.shared.is_some() {
            from_opencode.push(t.id().to_string());
        }
        if let Some(m) = &model {
            if let Some(set) = &available {
                if !set.contains(m.as_str()) {
                    humanln(
                        ctx,
                        format!("⚠ model: \"{m}\" not listed by server — override with --model <model>"),
                    );
                }
            }
        }
        tool_models.insert(t.id().to_string(), model);
    }
    if !from_opencode.is_empty() {
        humanln(
            ctx,
            format!(
                "• {}: no own model on the server — using its OpenCode model",
                from_opencode.join(", ")
            ),
        );
    }

    let home = crate::cli::connect_tools::home_dir();
    let mut failed = 0usize;
    let mut skipped = 0usize;
    let mut dry_payload: Vec<Value> = Vec::new();
    let mut applied: Vec<Value> = Vec::new();

    for t in &tools {
        if t.needs_model() && tool_models.get(t.id()).and_then(|m| m.as_ref()).is_none() {
            skipped += 1;
            humanln(
                ctx,
                format!(
                    "⏭  {}: no model configured on the server — skipped (pass --model <model>)",
                    t.name()
                ),
            );
            continue;
        }
        let actx = ApplyCtx {
            base_url: server.clone(),
            api_key: api_key.clone(),
            model: tool_models.get(t.id()).cloned().flatten(),
            claude_models: claude_models.clone(),
        };
        if args.dry_run {
            match t.plan_apply(&home, &actx) {
                Ok(files) => {
                    dry_payload.push(json!({
                        "tool": t.id(),
                        "name": t.name(),
                        "files": files.iter().map(|(p, c)| json!({
                            "path": p.display().to_string(),
                            "content": c,
                        })).collect::<Vec<_>>(),
                    }));
                    humanln(
                        ctx,
                        format!(
                            "[dry-run] {} → {}",
                            t.name(),
                            files.iter().map(|(p, _)| p.display().to_string()).collect::<Vec<_>>().join(", ")
                        ),
                    );
                }
                Err(e) => {
                    failed += 1;
                    humanln(ctx, format!("❌ {}: {e}", t.name()));
                }
            }
            continue;
        }
        match t.apply(&home, &actx) {
            Ok(files) => {
                humanln(
                    ctx,
                    format!(
                        "✅ {} → {}",
                        t.name(),
                        files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
                    ),
                );
                applied.push(json!({
                    "tool": t.id(),
                    "name": t.name(),
                    "files": files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                }));
            }
            Err(e) => {
                failed += 1;
                humanln(ctx, format!("❌ {}: {e}", t.name()));
            }
        }
    }

    humanln(ctx, format!("   Base URL: {}/v1", server.trim_end_matches('/')));
    if tools.contains(&ConnectTool::Claude) {
        for &(flag, env_key) in CLAUDE_MODELS {
            if let Some(Some(m)) = claude_models.get(env_key) {
                humanln(ctx, format!("   {env_key}={m}"));
            }
        }
        if !claude_unset.is_empty() {
            humanln(
                ctx,
                format!(
                    "   Claude {}: not set on the server — Claude Code's own default applies (or pass --{} <model>)",
                    claude_unset.join(", "),
                    claude_unset[0]
                ),
            );
        }
    }
    for (id, m) in &tool_models {
        if let Some(m) = m {
            humanln(ctx, format!("   {id} model: {m}"));
        }
    }
    if tools.contains(&ConnectTool::Omp) {
        humanln(
            ctx,
            "   omp models: every server model, listed under openproxy in /model",
        );
    }
    humanln(
        ctx,
        format!(
            "   Restart the tools to apply. Undo: openproxy connect --reset --tools {}",
            tools.iter().map(|t| t.id()).collect::<Vec<_>>().join(",")
        ),
    );

    if args.print_env {
        humanln(ctx, "\nOpenAI-compatible CLIs (codex, opencode, aider, …):");
        humanln(ctx, format!("   OPENAI_BASE_URL={}/v1", server.trim_end_matches('/')));
        humanln(ctx, format!("   OPENAI_API_KEY={api_key}"));
    }

    if ctx.is_robot() {
        let data = if args.dry_run {
            json!({
                "server": server,
                "dryRun": true,
                "tools": dry_payload,
            })
        } else {
            json!({
                "server": server,
                "tools": applied,
                "skipped": skipped,
                "failed": failed,
            })
        };
        emit_robot("openproxy.v1.connect.run", data)?;
    } else if args.dry_run {
        // Human dry-run already printed per-tool lines; also dump the payload
        // as JSON so it can be piped.
        println!("{}", serde_json::to_string_pretty(&json!({"tools": dry_payload}))?);
    }

    Ok(if failed > 0 || skipped > 0 { 1 } else { 0 })
}

fn select_tools(args: &ConnectArgs, ctx: OutputCtx) -> Result<Vec<ConnectTool>> {
    if let Some(list) = &args.tools {
        return ConnectTool::resolve(list);
    }
    if std::io::stdin().is_terminal() && !ctx.is_robot() {
        // Simple prompt (upstream uses a multi-select; comma list is equivalent).
        eprintln!("Select CLI tools to configure (comma-separated, empty = claude)");
        eprintln!(
            "Supported: {}",
            ConnectTool::ALL.iter().map(|t| t.id()).collect::<Vec<_>>().join(", ")
        );
        eprint!("tools> ");
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line)
            .context("read tool selection")?;
        let line = line.trim();
        if line.is_empty() {
            return ConnectTool::resolve(&["claude".to_string()]);
        }
        let list: Vec<String> = line.split(',').map(|s| s.trim().to_string()).collect();
        return ConnectTool::resolve(&list);
    }
    ConnectTool::resolve(&["claude".to_string()])
}

fn run_reset(tools: &[ConnectTool], ctx: OutputCtx) -> Result<i32> {
    let home = crate::cli::connect_tools::home_dir();
    let mut failed = 0usize;
    for t in tools {
        match t.reset(&home) {
            Ok(files) if !files.is_empty() => {
                humanln(
                    ctx,
                    format!(
                        "✅ {}: removed openproxy settings ({})",
                        t.name(),
                        files
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
            Ok(_) => humanln(ctx, format!("• {}: nothing to reset", t.name())),
            Err(e) => {
                failed += 1;
                humanln(ctx, format!("❌ {}: {e}", t.name()));
            }
        }
    }
    if ctx.is_robot() {
        emit_robot(
            "openproxy.v1.connect.reset",
            json!({"tools": tools.iter().map(|t| t.id()).collect::<Vec<_>>(), "failed": failed}),
        )?;
    }
    Ok(if failed > 0 { 1 } else { 0 })
}

// ── show (offline) ──────────────────────────────────────────────────────────

pub fn run_show(args: ShowArgs, ctx: OutputCtx) -> Result<i32> {
    let mut names: Vec<String> = args.tools.clone();
    if let Some(extra) = &args.tools_opt {
        names.extend(extra.iter().cloned());
    }
    let tools = if names.is_empty() {
        ConnectTool::ALL.to_vec()
    } else {
        ConnectTool::resolve(&names)?
    };
    let home = crate::cli::connect_tools::home_dir();

    let mut records = Vec::new();
    let mut any_error = false;
    for t in &tools {
        let files: Vec<String> = t
            .paths(&home)
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        let mut rec = json!({
            "id": t.id(),
            "name": t.name(),
            "files": files,
        });
        match t.show(&home) {
            Ok(s) => {
                rec["configured"] = Value::Bool(s.configured());
                rec["baseUrl"] = s
                    .base_url
                    .map(Value::String)
                    .unwrap_or(Value::Null);
                rec["apiKey"] = s
                    .api_key
                    .map(|k| Value::String(mask_key(&k)))
                    .unwrap_or(Value::Null);
                rec["models"] = Value::Object(
                    s.models
                        .into_iter()
                        .map(|(k, v)| (k, Value::String(v)))
                        .collect(),
                );
                if let Some(active) = s.active {
                    rec["active"] = Value::Bool(active);
                }
            }
            Err(e) => {
                any_error = true;
                rec["error"] = Value::String(e.to_string());
            }
        }
        records.push(rec);
    }

    if ctx.is_robot() {
        emit_robot("openproxy.v1.connect.show", json!(records))?;
    } else if args.json {
        println!("{}", serde_json::to_string_pretty(&records)?);
    } else {
        for (i, rec) in records.iter().enumerate() {
            if i > 0 {
                println!();
            }
            print_record(rec, ctx);
        }
    }
    Ok(if any_error { 1 } else { 0 })
}

fn print_record(rec: &Value, ctx: OutputCtx) {
    let name = rec.get("name").and_then(Value::as_str).unwrap_or("?");
    if let Some(err) = rec.get("error").and_then(Value::as_str) {
        humanln(ctx, format!("❌ {name}: {err}"));
        return;
    }
    if rec.get("configured").and_then(Value::as_bool) != Some(true) {
        let file = rec
            .get("files")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .unwrap_or("?");
        humanln(ctx, format!("• {name}: not configured for openproxy ({file})"));
        return;
    }
    let inactive = rec.get("active").and_then(Value::as_bool) == Some(false);
    humanln(
        ctx,
        format!(
            "✅ {name}{}",
            if inactive {
                "  (openproxy present but not the active provider)"
            } else {
                ""
            }
        ),
    );
    let files = rec
        .get("files")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    humanln(ctx, format!("   File:     {files}"));
    humanln(
        ctx,
        format!(
            "   Base URL: {}",
            rec.get("baseUrl")
                .and_then(Value::as_str)
                .unwrap_or("(none)")
        ),
    );
    humanln(
        ctx,
        format!(
            "   API key:  {}",
            rec.get("apiKey")
                .and_then(Value::as_str)
                .unwrap_or("(none)")
        ),
    );
    let models = rec.get("models").and_then(Value::as_object);
    match models {
        Some(m) if !m.is_empty() => {
            let width = m.keys().map(|k| k.len()).max().unwrap_or(0);
            humanln(ctx, "   Models:");
            for (k, v) in m {
                humanln(
                    ctx,
                    format!(
                        "     {:width$}  {}",
                        k,
                        v.as_str().unwrap_or(""),
                        width = width
                    ),
                );
            }
        }
        _ => humanln(ctx, "   Models:   (none set — the tool's own default applies)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::config::CliOverrides;
    use crate::cli::test_lock::ENV_LOCK;

    #[test]
    fn normalize_url_keeps_origin_only() {
        assert_eq!(
            normalize_server_url("http://host:4623/dashboard/providers").unwrap(),
            "http://host:4623/"
        );
        assert_eq!(
            normalize_server_url("https://example.com:8443/x?y=1").unwrap(),
            "https://example.com:8443/"
        );
        assert_eq!(
            normalize_server_url("192.168.1.5:4623").unwrap(),
            "http://192.168.1.5:4623/"
        );
    }

    #[test]
    fn mask_key_hides_middle() {
        assert_eq!(mask_key("op-abcdef1234567890"), "op-abc…7890");
        assert_eq!(mask_key("short"), "****");
    }

    #[tokio::test]
    async fn connect_dry_run_produces_payload_without_writing() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);
        #[cfg(windows)]
        std::env::set_var("USERPROFILE", &home);

        let resolved = ResolvedConfig::resolve(CliOverrides {
            data_dir: Some(tmp.path().join("data")),
            ..Default::default()
        })
        .unwrap();
        let args = ConnectArgs {
            url: Some("http://127.0.0.1:9".to_string()), // unreachable: dry-run must tolerate
            tools: Some(vec!["claude".to_string(), "opencode".to_string()]),
            password: None,
            save: false,
            key_name: None,
            api_key: Some("op-dryrunkey1234567890".to_string()),
            model: Some("test/model".to_string()),
            fable: None,
            opus: None,
            sonnet: None,
            haiku: None,
            print_env: false,
            reset: false,
            dry_run: true,
        };
        let code = run_connect(args, &resolved, OutputCtx::robot())
            .await
            .unwrap();
        assert_eq!(code, 0);
        // Dry-run computed payloads but wrote nothing.
        assert!(!home.join(".claude").join("settings.json").exists());
        assert!(!home
            .join(".config")
            .join("opencode")
            .join("opencode.json")
            .exists());

        std::env::remove_var("HOME");
        #[cfg(windows)]
        std::env::remove_var("USERPROFILE");
    }

    #[tokio::test]
    async fn connect_requires_url_or_reset() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let resolved = ResolvedConfig::resolve(CliOverrides {
            data_dir: Some(tmp.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();
        let args = ConnectArgs {
            url: None,
            tools: Some(vec!["claude".to_string()]),
            password: None,
            save: false,
            key_name: None,
            api_key: None,
            model: None,
            fable: None,
            opus: None,
            sonnet: None,
            haiku: None,
            print_env: false,
            reset: false,
            dry_run: false,
        };
        let code = run_connect(args, &resolved, OutputCtx::robot())
            .await
            .unwrap();
        assert_ne!(code, 0);
    }
}
