//! Per-tool local config readers/writers for `openproxy connect` / `openproxy show`.
//!
//! Port of 9router `cli/src/cli/commands/connectTools.js`. Each tool knows its
//! local config file layout; `plan_apply` computes the `(path, content)` pairs
//! `connect` would write so `--dry-run` can preview them without touching disk,
//! `apply` commits them (one-time `.bak-openproxy` backup + mode 0600), `show`
//! reads the 9router/openproxy settings back offline, and `reset` strips them.
//!
//! Naming: this product writes the `openproxy` provider key (matching
//! `src/server/api/cli_tools*.rs`), while files produced by a real 9router use
//! `9router`. Readers and reset accept BOTH keys so `show`/`--reset` work on
//! files written by either product's tooling.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Map, Value};

/// Provider key this build writes into tool configs.
pub const PROVIDER_KEY: &str = "openproxy";
/// Provider keys recognised as "ours" when reading/resetting (openproxy + 9router).
const PROVIDER_KEYS: &[&str] = &["openproxy", "9router"];
/// Model id prefixes recognised as "ours" (e.g. opencode `model`, pi providers).
const MODEL_PREFIXES: &[&str] = &["openproxy/", "9router/"];
/// Suffix of the one-time pre-write backup (`<file>.bak-openproxy`).
const BACKUP_SUFFIX: &str = ".bak-openproxy";

/// Claude Code tier overrides: `(flag, envKey)` — port of upstream `CLAUDE_MODELS`.
pub const CLAUDE_MODELS: &[(&str, &str)] = &[
    ("fable", "ANTHROPIC_DEFAULT_FABLE_MODEL"),
    ("opus", "ANTHROPIC_DEFAULT_OPUS_MODEL"),
    ("sonnet", "ANTHROPIC_DEFAULT_SONNET_MODEL"),
    ("haiku", "ANTHROPIC_DEFAULT_HAIKU_MODEL"),
];

/// Context handed to every tool's `apply`, mirroring upstream `ctx`.
#[derive(Debug, Clone)]
pub struct ApplyCtx {
    /// Server origin (no `/v1`), e.g. `http://host:4623`.
    pub base_url: String,
    pub api_key: String,
    /// Model for non-Claude tools (`None` only for tools with `needs_model == false`).
    pub model: Option<String>,
    /// Claude tier env values keyed by envKey; `None` removes the key.
    pub claude_models: BTreeMap<String, Option<String>>,
}

/// Normalised view of one tool's local config, as produced by `show`.
#[derive(Debug, Clone, Default)]
pub struct ToolShow {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    /// label -> model (e.g. `main`, `sonnet`, `subagent`, `explorer`).
    pub models: BTreeMap<String, String>,
    /// `Some(false)` = our provider is present but not the active one.
    pub active: Option<bool>,
}

impl ToolShow {
    pub fn configured(&self) -> bool {
        self.base_url.is_some() || self.api_key.is_some() || !self.models.is_empty()
    }
}

/// The CLI tools `connect` can configure — port of upstream `TOOLS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConnectTool {
    Claude,
    Codex,
    OpenCode,
    Droid,
    Crush,
    Kilo,
    Cline,
    Pi,
    Omp,
}

impl ConnectTool {
    pub const ALL: &'static [ConnectTool] = &[
        ConnectTool::Claude,
        ConnectTool::Codex,
        ConnectTool::OpenCode,
        ConnectTool::Droid,
        ConnectTool::Crush,
        ConnectTool::Kilo,
        ConnectTool::Cline,
        ConnectTool::Pi,
        ConnectTool::Omp,
    ];

    pub fn id(&self) -> &'static str {
        match self {
            ConnectTool::Claude => "claude",
            ConnectTool::Codex => "codex",
            ConnectTool::OpenCode => "opencode",
            ConnectTool::Droid => "droid",
            ConnectTool::Crush => "crush",
            ConnectTool::Kilo => "kilo",
            ConnectTool::Cline => "cline",
            ConnectTool::Pi => "pi",
            ConnectTool::Omp => "omp",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            ConnectTool::Claude => "Claude Code",
            ConnectTool::Codex => "OpenAI Codex CLI",
            ConnectTool::OpenCode => "OpenCode",
            ConnectTool::Droid => "Factory Droid",
            ConnectTool::Crush => "Crush",
            ConnectTool::Kilo => "Kilo Code CLI",
            ConnectTool::Cline => "Cline CLI",
            ConnectTool::Pi => "Pi",
            ConnectTool::Omp => "Oh My Pi",
        }
    }

    /// Server route used to inherit this tool's model (`GET /api/cli-tools/<route>`).
    /// `None` = no per-tool model on the server (kilo, omp).
    pub fn route(&self) -> Option<&'static str> {
        match self {
            ConnectTool::Codex => Some("codex-settings"),
            ConnectTool::OpenCode => Some("opencode-settings"),
            ConnectTool::Droid => Some("droid-settings"),
            ConnectTool::Crush => Some("crush-settings"),
            ConnectTool::Cline => Some("cline-settings"),
            ConnectTool::Pi => Some("pi-settings"),
            _ => None,
        }
    }

    /// Tools that discover models themselves don't need `--model`/server model.
    pub fn needs_model(&self) -> bool {
        !matches!(self, ConnectTool::Claude | ConnectTool::Omp)
    }

    /// Resolve a user-supplied tool name/alias to a tool (port of `resolveTools`).
    pub fn resolve(list: &[String]) -> Result<Vec<ConnectTool>> {
        let mut out: Vec<ConnectTool> = Vec::new();
        for raw in list {
            let id = raw.trim().to_ascii_lowercase();
            if id.is_empty() {
                continue;
            }
            if id == "all" {
                for t in ConnectTool::ALL {
                    if !out.contains(t) {
                        out.push(*t);
                    }
                }
                continue;
            }
            let tool = match id.as_str() {
                "claude" | "claude-code" | "claudecode" => ConnectTool::Claude,
                "codex" => ConnectTool::Codex,
                "opencode" => ConnectTool::OpenCode,
                "droid" | "factory" => ConnectTool::Droid,
                "crush" => ConnectTool::Crush,
                "kilo" | "kilocode" => ConnectTool::Kilo,
                "cline" => ConnectTool::Cline,
                "pi" | "pi-coding-agent" => ConnectTool::Pi,
                "omp" | "oh-my-pi" => ConnectTool::Omp,
                _ => {
                    return Err(anyhow!(
                        "Unknown tool \"{raw}\". Supported: {}, all",
                        ConnectTool::ALL
                            .iter()
                            .map(|t| t.id())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                }
            };
            if !out.contains(&tool) {
                out.push(tool);
            }
        }
        Ok(out)
    }

    /// All config file paths this tool may touch (for `show` output).
    pub fn paths(&self, home: &Path) -> Vec<PathBuf> {
        match self {
            ConnectTool::Claude => vec![home.join(".claude").join("settings.json")],
            ConnectTool::Codex => vec![home.join(".codex").join("config.toml")],
            ConnectTool::OpenCode => {
                vec![home.join(".config").join("opencode").join("opencode.json")]
            }
            ConnectTool::Droid => vec![home.join(".factory").join("settings.json")],
            ConnectTool::Crush => vec![xdg_config_home(home).join("crush").join("crush.json")],
            ConnectTool::Kilo => vec![home
                .join(".local")
                .join("share")
                .join("kilo")
                .join("auth.json")],
            ConnectTool::Cline => vec![
                home.join(".cline").join("data").join("globalState.json"),
                home.join(".cline").join("data").join("secrets.json"),
            ],
            ConnectTool::Pi => vec![pi_path(home)],
            ConnectTool::Omp => vec![home.join(".omp").join("agent").join("models.yml")],
        }
    }

    /// Compute the `(path, content)` pairs `apply` would write. Performs no I/O
    /// beyond reading the existing files.
    pub fn plan_apply(&self, home: &Path, ctx: &ApplyCtx) -> Result<Vec<(PathBuf, String)>> {
        match self {
            ConnectTool::Claude => claude::plan(home, ctx),
            ConnectTool::Codex => codex::plan(home, ctx),
            ConnectTool::OpenCode => opencode::plan(home, ctx),
            ConnectTool::Droid => droid::plan(home, ctx),
            ConnectTool::Crush => crush::plan(home, ctx),
            ConnectTool::Kilo => kilo::plan(home, ctx),
            ConnectTool::Cline => cline::plan(home, ctx),
            ConnectTool::Pi => pi::plan(home, ctx),
            ConnectTool::Omp => omp::plan(home, ctx),
        }
    }

    /// Write the planned files (backup + 0600). Returns written paths.
    pub fn apply(&self, home: &Path, ctx: &ApplyCtx) -> Result<Vec<PathBuf>> {
        let mut written = Vec::new();
        for (path, content) in self.plan_apply(home, ctx)? {
            write_file(&path, &content)?;
            written.push(path);
        }
        Ok(written)
    }

    /// Read the tool's local config back (offline; never contacts a server).
    pub fn show(&self, home: &Path) -> Result<ToolShow> {
        match self {
            ConnectTool::Claude => claude::show(home),
            ConnectTool::Codex => codex::show(home),
            ConnectTool::OpenCode => opencode::show(home),
            ConnectTool::Droid => droid::show(home),
            ConnectTool::Crush => crush::show(home),
            ConnectTool::Kilo => kilo::show(home),
            ConnectTool::Cline => cline::show(home),
            ConnectTool::Pi => pi::show(home),
            ConnectTool::Omp => omp::show(home),
        }
    }

    /// Remove our settings from the tool's local config. Returns touched files.
    pub fn reset(&self, home: &Path) -> Result<Vec<PathBuf>> {
        match self {
            ConnectTool::Claude => claude::reset(home),
            ConnectTool::Codex => codex::reset(home),
            ConnectTool::OpenCode => opencode::reset(home),
            ConnectTool::Droid => droid::reset(home),
            ConnectTool::Crush => crush::reset(home),
            ConnectTool::Kilo => kilo::reset(home),
            ConnectTool::Cline => cline::reset(home),
            ConnectTool::Pi => pi::reset(home),
            ConnectTool::Omp => omp::reset(home),
        }
    }
}

// ── shared helpers ──────────────────────────────────────────────────────────

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `$XDG_CONFIG_HOME` or `~/.config` (crush honours XDG).
fn xdg_config_home(home: &Path) -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"))
}

/// Pi prefers `~/.pi/agent/models.json`; a legacy `~/.pi/models.json` is used
/// only when the agent file does not exist and the legacy one does.
fn pi_path(home: &Path) -> PathBuf {
    let agent = home.join(".pi").join("agent").join("models.json");
    let root = home.join(".pi").join("models.json");
    if !agent.exists() && root.exists() {
        root
    } else {
        agent
    }
}

/// `baseUrl` → `{base}/v1` (upstream `v1()`).
fn v1(base_url: &str) -> String {
    let b = base_url.trim_end_matches('/');
    if b.ends_with("/v1") {
        b.to_string()
    } else {
        format!("{b}/v1")
    }
}

/// First provider key (of ours) present in `obj`, if any.
fn our_provider_key<'a>(obj: &'a Map<String, Value>) -> Option<&'a str> {
    PROVIDER_KEYS.iter().copied().find(|k| obj.contains_key(*k))
}

/// Does `model` carry one of our provider prefixes (`openproxy/`, `9router/`)?
fn is_our_model(model: &str) -> bool {
    MODEL_PREFIXES.iter().any(|p| model.starts_with(p))
}

/// Strip a recognised provider prefix, returning the bare model id.
fn strip_model_prefix(model: &str) -> &str {
    for p in MODEL_PREFIXES {
        if let Some(rest) = model.strip_prefix(p) {
            return rest;
        }
    }
    model
}

/// Drop trailing commas (JSONC) outside string literals — port of upstream
/// `stripTrailingCommas`.
fn strip_trailing_commas(text: &str) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r#"("(?:\\.|[^"\\])*")|,(\s*[}\]])"#).expect("jsonc regex")
    });
    re.replace_all(text, |caps: &regex::Captures| {
        if let Some(s) = caps.get(1) {
            s.as_str().to_string()
        } else {
            caps.get(2).map(|m| m.as_str()).unwrap_or("").to_string()
        }
    })
    .into_owned()
}

/// Read a JSON/JSONC file; `Ok(None)` when missing, `Err` on parse failure.
fn read_json_file(path: &Path) -> Result<Option<Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let parsed: Value = serde_json::from_str(&strip_trailing_commas(&text))
        .with_context(|| format!("Cannot parse {}", path.display()))?;
    Ok(Some(parsed))
}

fn read_json_or_empty(path: &Path) -> Result<Value> {
    Ok(read_json_file(path)?.unwrap_or(Value::Object(Map::new())))
}

fn obj_mut<'a>(v: &'a mut Value) -> &'a mut Map<String, Value> {
    if !v.is_object() {
        *v = Value::Object(Map::new());
    }
    v.as_object_mut().expect("object")
}

/// Files hold the API key → owner-only (0600) on POSIX.
fn set_secret_mode(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// One-time backup of the user's pre-existing file, then write.
fn write_file(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let backup = path.with_file_name(format!(
        "{}{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config"),
        BACKUP_SUFFIX
    ));
    if path.exists() && !backup.exists() {
        std::fs::copy(path, &backup).with_context(|| format!("backup {}", path.display()))?;
        let _ = set_secret_mode(&backup);
    }
    std::fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
    let _ = set_secret_mode(path);
    Ok(())
}

/// Rewrite an existing file on reset (no backup).
fn rewrite_file(path: &Path, content: &str) -> Result<()> {
    std::fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
    let _ = set_secret_mode(path);
    Ok(())
}

// ── TOML helpers (codex config.toml) ────────────────────────────────────────

fn read_toml_file(path: &Path) -> Result<Option<toml::Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let parsed: toml::Value =
        toml::from_str(&text).with_context(|| format!("Cannot parse {}", path.display()))?;
    Ok(Some(parsed))
}

fn tget<'a>(v: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    v.as_table()?.get(key)
}

fn toml_table(v: &mut toml::Value) -> &mut toml::map::Map<String, toml::Value> {
    if !v.is_table() {
        *v = toml::Value::Table(toml::map::Map::new());
    }
    v.as_table_mut().expect("table")
}

fn ensure_toml_table<'a>(
    parent: &'a mut toml::map::Map<String, toml::Value>,
    key: &str,
) -> &'a mut toml::map::Map<String, toml::Value> {
    let entry = parent
        .entry(key.to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    if !entry.is_table() {
        *entry = toml::Value::Table(toml::map::Map::new());
    }
    entry.as_table_mut().expect("table")
}

fn toml_key(key: &str) -> String {
    let bare = !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if bare {
        key.to_string()
    } else {
        format!("{}", toml::Value::String(key.to_string()))
    }
}

/// Render a `toml::Value` document. Sub-tables become `[section]` headers;
/// every leaf (including arrays, which Display renders inline) is emitted as
/// `key = <value>` so we never hit toml's values-after-table ordering trap.
fn toml_emit_document(root: &toml::map::Map<String, toml::Value>) -> String {
    let mut out = String::new();
    emit_toml_table(root, "", &mut out);
    out
}

fn emit_toml_table(table: &toml::map::Map<String, toml::Value>, prefix: &str, out: &mut String) {
    // Pass 1: scalar / array entries (arrays of tables render as inline
    // `[{...}]`, which is valid TOML).
    for (k, v) in table {
        if v.is_table() {
            continue;
        }
        out.push_str(&format!("{} = {}\n", toml_key(k), v));
    }
    // Pass 2: sub-tables as `[path]` sections.
    for (k, v) in table {
        let Some(sub) = v.as_table() else { continue };
        if !prefix.is_empty() {
            out.push('\n');
        }
        let path = if prefix.is_empty() {
            toml_key(k)
        } else {
            format!("{}.{}", prefix, toml_key(k))
        };
        out.push_str(&format!("[{path}]\n"));
        emit_toml_table(sub, &path, out);
    }
}

// ── YAML helpers (omp models.yml) ───────────────────────────────────────────

fn read_yaml_file(path: &Path) -> Result<Option<Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let yml: serde_yml::Value =
        serde_yml::from_str(&text).with_context(|| format!("Cannot parse {}", path.display()))?;
    let bytes = serde_json::to_vec(&yml).context("convert YAML to JSON")?;
    let json: Value = serde_json::from_slice(&bytes).context("normalise YAML payload")?;
    Ok(Some(json))
}

fn yaml_string(v: &Value) -> Result<String> {
    serde_yml::to_string(v).context("serialise YAML")
}

// ── Claude Code ─────────────────────────────────────────────────────────────
mod claude {
    use super::*;

    const RESET_KEYS: &[&str] = &[
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_DEFAULT_FABLE_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    ];

    fn path(home: &Path) -> PathBuf {
        home.join(".claude").join("settings.json")
    }

    pub fn plan(home: &Path, ctx: &ApplyCtx) -> Result<Vec<(PathBuf, String)>> {
        let file = path(home);
        let mut cur = read_json_or_empty(&file)?;
        let root = obj_mut(&mut cur);
        root.insert("hasCompletedOnboarding".into(), Value::Bool(true));
        let env = root
            .entry("env".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        let env = obj_mut(env);
        env.insert(
            "ANTHROPIC_BASE_URL".into(),
            Value::String(v1(&ctx.base_url)),
        );
        env.insert(
            "ANTHROPIC_AUTH_TOKEN".into(),
            Value::String(ctx.api_key.clone()),
        );
        // A tier with no model (null) is removed so Claude Code uses its own
        // default instead of a stale value from an earlier run.
        for &(_, env_key) in CLAUDE_MODELS {
            match ctx.claude_models.get(env_key).and_then(|m| m.as_deref()) {
                Some(m) => {
                    env.insert((*env_key).to_string(), Value::String(m.to_string()));
                }
                None => {
                    env.remove(env_key);
                }
            }
        }
        Ok(vec![(file, serde_json::to_string_pretty(&cur)?)])
    }

    pub fn show(home: &Path) -> Result<ToolShow> {
        let env = read_json_file(&path(home))?
            .and_then(|v| v.get("env").cloned())
            .unwrap_or(Value::Null);
        let env = env.as_object().cloned().unwrap_or_default();
        let mut models = BTreeMap::new();
        if let Some(m) = env.get("ANTHROPIC_MODEL").and_then(Value::as_str) {
            if !m.is_empty() {
                models.insert("main".to_string(), m.to_string());
            }
        }
        for &(flag, env_key) in CLAUDE_MODELS {
            if let Some(m) = env.get(env_key).and_then(Value::as_str) {
                if !m.is_empty() {
                    models.insert(flag.to_string(), m.to_string());
                }
            }
        }
        Ok(ToolShow {
            base_url: env
                .get("ANTHROPIC_BASE_URL")
                .and_then(Value::as_str)
                .map(str::to_string),
            api_key: env
                .get("ANTHROPIC_AUTH_TOKEN")
                .and_then(Value::as_str)
                .map(str::to_string),
            models,
            active: None,
        })
    }

    pub fn reset(home: &Path) -> Result<Vec<PathBuf>> {
        let file = path(home);
        let Some(mut cur) = read_json_file(&file)? else {
            return Ok(vec![]);
        };
        let mut removed = false;
        if let Some(env) = cur.get_mut("env").and_then(Value::as_object_mut) {
            for k in RESET_KEYS {
                removed |= env.remove(*k).is_some();
            }
            if env.is_empty() {
                let _ = cur.as_object_mut().map(|o| o.remove("env"));
            }
        }
        // Nothing of ours was configured — like the sibling tools' resets,
        // report no touched file rather than rewriting a no-op.
        if !removed {
            return Ok(vec![]);
        }
        rewrite_file(&file, &serde_json::to_string_pretty(&cur)?)?;
        Ok(vec![file])
    }
}

// ── OpenAI Codex CLI (~/.codex/config.toml) ─────────────────────────────────
mod codex {
    use super::*;

    fn path(home: &Path) -> PathBuf {
        home.join(".codex").join("config.toml")
    }

    /// model id from the server's codex-settings payload: `data.config` is raw
    /// TOML text; only trust `model` when the provider is ours.
    pub fn server_model(data: &Value) -> Option<String> {
        let text = data.get("config")?.as_str()?;
        let cfg: toml::Value = toml::from_str(text).ok()?;
        let provider = tget(&cfg, "model_provider")?.as_str()?;
        if PROVIDER_KEYS.contains(&provider) {
            tget(&cfg, "model")?.as_str().map(str::to_string)
        } else {
            None
        }
    }

    pub fn plan(home: &Path, ctx: &ApplyCtx) -> Result<Vec<(PathBuf, String)>> {
        let file = path(home);
        let mut cfg = read_toml_file(&file)?.unwrap_or(toml::Value::Table(toml::map::Map::new()));
        let root = toml_table(&mut cfg);
        let model = ctx.model.clone().unwrap_or_default();
        root.insert("model".into(), toml::Value::String(model.clone()));
        root.insert(
            "model_provider".into(),
            toml::Value::String(PROVIDER_KEY.to_string()),
        );
        // Custom providers ignore auth.json — key travels as a static header.
        let providers = ensure_toml_table(root, "model_providers");
        let mut p = toml::map::Map::new();
        p.insert("name".into(), toml::Value::String("OpenProxy".to_string()));
        p.insert("base_url".into(), toml::Value::String(v1(&ctx.base_url)));
        p.insert(
            "wire_api".into(),
            toml::Value::String("responses".to_string()),
        );
        let mut headers = toml::map::Map::new();
        headers.insert(
            "Authorization".into(),
            toml::Value::String(format!("Bearer {}", ctx.api_key)),
        );
        p.insert("http_headers".into(), toml::Value::Table(headers));
        providers.insert(PROVIDER_KEY.to_string(), toml::Value::Table(p));
        // Also drop a stale `9router` provider entry if present.
        providers.remove("9router");

        let agents = ensure_toml_table(root, "agents");
        agents.remove("subagent");
        agents.insert("default_subagent_model".into(), toml::Value::String(model));

        let text = toml_emit_document(match &cfg {
            toml::Value::Table(t) => t,
            _ => unreachable!(),
        });
        Ok(vec![(file, text)])
    }

    fn provider_table<'a>(cfg: &'a toml::Value) -> Option<&'a toml::map::Map<String, toml::Value>> {
        let providers = cfg.get("model_providers")?.as_table()?;
        PROVIDER_KEYS
            .iter()
            .find_map(|k| providers.get(*k).and_then(|v| v.as_table()))
    }

    pub fn show(home: &Path) -> Result<ToolShow> {
        let Some(cfg) = read_toml_file(&path(home))? else {
            return Ok(ToolShow::default());
        };
        let p = provider_table(&cfg);
        let auth = p
            .and_then(|t| t.get("http_headers"))
            .and_then(|h| h.get("Authorization"))
            .and_then(|v| v.as_str());
        let api_key = auth.map(|a| {
            a.strip_prefix("Bearer ")
                .or_else(|| a.strip_prefix("bearer "))
                .unwrap_or(a)
                .to_string()
        });
        let mut models = BTreeMap::new();
        if let Some(m) = tget(&cfg, "model").and_then(|v| v.as_str()) {
            if !m.is_empty() {
                models.insert("main".to_string(), m.to_string());
            }
        }
        if let Some(m) = tget(&cfg, "agents")
            .and_then(|a| a.as_table())
            .and_then(|a| a.get("default_subagent_model"))
            .and_then(|v| v.as_str())
        {
            if !m.is_empty() {
                models.insert("subagent".to_string(), m.to_string());
            }
        }
        let provider = tget(&cfg, "model_provider").and_then(|v| v.as_str());
        Ok(ToolShow {
            base_url: p
                .and_then(|t| t.get("base_url"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
            api_key,
            models,
            active: provider.map(|p| PROVIDER_KEYS.contains(&p)),
        })
    }

    pub fn reset(home: &Path) -> Result<Vec<PathBuf>> {
        let file = path(home);
        let Some(mut cfg) = read_toml_file(&file)? else {
            return Ok(vec![]);
        };
        let root = toml_table(&mut cfg);
        let provider = root
            .get("model_provider")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if provider
            .as_deref()
            .is_some_and(|p| PROVIDER_KEYS.contains(&p))
        {
            root.remove("model");
            root.remove("model_provider");
        }
        if let Some(providers) = root
            .get_mut("model_providers")
            .and_then(|v| v.as_table_mut())
        {
            for k in PROVIDER_KEYS {
                providers.remove(*k);
            }
            if providers.is_empty() {
                root.remove("model_providers");
            }
        }
        if let Some(agents) = root.get_mut("agents").and_then(|v| v.as_table_mut()) {
            agents.remove("default_subagent_model");
            agents.remove("subagent");
            if agents.is_empty() {
                root.remove("agents");
            }
        }
        let text = toml_emit_document(match &cfg {
            toml::Value::Table(t) => t,
            _ => unreachable!(),
        });
        rewrite_file(&file, &text)?;
        Ok(vec![file])
    }
}

// ── OpenCode (~/.config/opencode/opencode.json) ─────────────────────────────
mod opencode {
    use super::*;

    fn path(home: &Path) -> PathBuf {
        home.join(".config").join("opencode").join("opencode.json")
    }

    /// `data.config.model` like `openproxy/<m>` / `9router/<m>` → bare id.
    pub fn server_model(data: &Value) -> Option<String> {
        let m = data.get("config")?.get("model")?.as_str()?;
        if is_our_model(m) {
            Some(strip_model_prefix(m).to_string())
        } else {
            // openproxy's GET also exposes a pre-stripped `opencode.activeModel`.
            data.get("opencode")
                .and_then(|o| o.get("activeModel"))
                .and_then(Value::as_str)
                .map(str::to_string)
        }
    }

    fn provider<'a>(cfg: &'a mut Value) -> &'a mut Map<String, Value> {
        let root = obj_mut(cfg);
        let providers = root
            .entry("provider".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        let providers = obj_mut(providers);
        // Drop a stale 9router entry when (re)writing under openproxy.
        providers.remove("9router");
        let p = providers.entry(PROVIDER_KEY.to_string()).or_insert_with(
            || json!({"npm": "@ai-sdk/openai-compatible", "options": {}, "models": {}}),
        );
        obj_mut(p)
    }

    pub fn plan(home: &Path, ctx: &ApplyCtx) -> Result<Vec<(PathBuf, String)>> {
        let file = path(home);
        let mut cfg = read_json_or_empty(&file)?;
        let model = ctx.model.clone().unwrap_or_default();
        let model_ref = format!("{}/{}", PROVIDER_KEY, model);
        {
            let p = provider(&mut cfg);
            let options = p
                .entry("options".to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            let options = obj_mut(options);
            options.insert("baseURL".into(), Value::String(v1(&ctx.base_url)));
            options.insert("apiKey".into(), Value::String(ctx.api_key.clone()));
            let models = p
                .entry("models".to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            let models = obj_mut(models);
            models.insert(
                model.clone(),
                json!({
                    "name": model,
                    "modalities": {"input": ["text", "image"], "output": ["text"]},
                }),
            );
        }
        let root = obj_mut(&mut cfg);
        root.insert("model".into(), Value::String(model_ref.clone()));
        let agent = root
            .entry("agent".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        let agent = obj_mut(agent);
        agent.insert(
            "explorer".into(),
            json!({
                "description": "Fast explorer subagent for codebase exploration",
                "mode": "subagent",
                "model": model_ref,
            }),
        );
        Ok(vec![(file, serde_json::to_string_pretty(&cfg)?)])
    }

    fn ours_provider<'a>(cfg: &'a Value) -> Option<&'a Map<String, Value>> {
        let providers = cfg.get("provider")?.as_object()?;
        PROVIDER_KEYS
            .iter()
            .find_map(|k| providers.get(*k).and_then(Value::as_object))
    }

    pub fn show(home: &Path) -> Result<ToolShow> {
        let cfg = read_json_or_empty(&path(home))?;
        let p = ours_provider(&cfg);
        let mut models = BTreeMap::new();
        if let Some(m) = cfg.get("model").and_then(Value::as_str) {
            if !m.is_empty() {
                models.insert("main".to_string(), m.to_string());
            }
        }
        if let Some(m) = cfg
            .get("agent")
            .and_then(|a| a.get("explorer"))
            .and_then(|e| e.get("model"))
            .and_then(Value::as_str)
        {
            if !m.is_empty() {
                models.insert("explorer".to_string(), m.to_string());
            }
        }
        let options = p.and_then(|p| p.get("options")).and_then(Value::as_object);
        let listed: Vec<String> = p
            .and_then(|p| p.get("models"))
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        if !listed.is_empty() {
            models.insert("available".to_string(), listed.join(", "));
        }
        let active = cfg
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(is_our_model);
        Ok(ToolShow {
            base_url: options
                .and_then(|o| o.get("baseURL"))
                .and_then(Value::as_str)
                .map(str::to_string),
            api_key: options
                .and_then(|o| o.get("apiKey"))
                .and_then(Value::as_str)
                .map(str::to_string),
            models,
            active: Some(active),
        })
    }

    pub fn reset(home: &Path) -> Result<Vec<PathBuf>> {
        let file = path(home);
        let Some(mut cfg) = read_json_file(&file)? else {
            return Ok(vec![]);
        };
        if let Some(providers) = cfg.get_mut("provider").and_then(Value::as_object_mut) {
            for k in PROVIDER_KEYS {
                providers.remove(*k);
            }
        }
        if let Some(m) = cfg.get("model").and_then(Value::as_str) {
            if is_our_model(m) {
                let _ = cfg.as_object_mut().map(|o| o.remove("model"));
            }
        }
        let explorer_ours = cfg
            .get("agent")
            .and_then(|a| a.get("explorer"))
            .and_then(|e| e.get("model"))
            .and_then(Value::as_str)
            .is_some_and(is_our_model);
        if explorer_ours {
            if let Some(agent) = cfg.get_mut("agent").and_then(Value::as_object_mut) {
                agent.remove("explorer");
                if agent.is_empty() {
                    let _ = cfg.as_object_mut().map(|o| o.remove("agent"));
                }
            }
        }
        rewrite_file(&file, &serde_json::to_string_pretty(&cfg)?)?;
        Ok(vec![file])
    }
}

// ── Factory Droid (~/.factory/settings.json) ────────────────────────────────
mod droid {
    use super::*;

    fn path(home: &Path) -> PathBuf {
        home.join(".factory").join("settings.json")
    }

    fn is_ours(m: &Value) -> bool {
        m.get("id").and_then(Value::as_str).is_some_and(|id| {
            id.starts_with("custom:OpenProxy") || id.starts_with("custom:9Router")
        })
    }

    /// First `customModels[]` entry whose id is ours → its `model`.
    pub fn server_model(data: &Value) -> Option<String> {
        data.get("settings")?
            .get("customModels")?
            .as_array()?
            .iter()
            .find(|m| is_ours(m))?
            .get("model")?
            .as_str()
            .map(str::to_string)
    }

    pub fn plan(home: &Path, ctx: &ApplyCtx) -> Result<Vec<(PathBuf, String)>> {
        let file = path(home);
        let mut cfg = read_json_or_empty(&file)?;
        let model = ctx.model.clone().unwrap_or_default();
        let others: Vec<Value> = cfg
            .get("customModels")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|m| !is_ours(m))
            .collect();
        let mut custom: Vec<Value> = vec![json!({
            "model": model,
            "id": "custom:OpenProxy-0",
            "index": 0,
            "baseUrl": v1(&ctx.base_url),
            "apiKey": ctx.api_key.clone(),
            "displayName": model,
            "maxOutputTokens": 131072,
            "noImageSupport": false,
            "provider": "openai",
        })];
        custom.extend(others);
        for (i, m) in custom.iter_mut().enumerate() {
            if let Some(obj) = m.as_object_mut() {
                obj.insert("index".into(), Value::from(i as u64));
            }
        }
        obj_mut(&mut cfg).insert("customModels".into(), Value::Array(custom));
        Ok(vec![(file, serde_json::to_string_pretty(&cfg)?)])
    }

    pub fn show(home: &Path) -> Result<ToolShow> {
        let ours: Vec<Value> = read_json_file(&path(home))?
            .and_then(|c| c.get("customModels").and_then(Value::as_array).cloned())
            .unwrap_or_default()
            .into_iter()
            .filter(is_ours)
            .collect();
        let first = ours.first();
        let mut models = BTreeMap::new();
        if let Some(m) = first.and_then(|m| m.get("model")).and_then(Value::as_str) {
            if !m.is_empty() {
                models.insert("main".to_string(), m.to_string());
            }
        }
        if ours.len() > 1 {
            let ids: Vec<&str> = ours
                .iter()
                .filter_map(|m| m.get("model").and_then(Value::as_str))
                .collect();
            if !ids.is_empty() {
                models.insert("available".to_string(), ids.join(", "));
            }
        }
        Ok(ToolShow {
            base_url: first
                .and_then(|m| m.get("baseUrl"))
                .and_then(Value::as_str)
                .map(str::to_string),
            api_key: first
                .and_then(|m| m.get("apiKey"))
                .and_then(Value::as_str)
                .map(str::to_string),
            models,
            active: None,
        })
    }

    pub fn reset(home: &Path) -> Result<Vec<PathBuf>> {
        let file = path(home);
        let Some(mut cfg) = read_json_file(&file)? else {
            return Ok(vec![]);
        };
        let removed = cfg
            .get("customModels")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(is_ours));
        if !removed {
            return Ok(vec![]);
        }
        if let Some(list) = cfg.get_mut("customModels").and_then(Value::as_array_mut) {
            list.retain(|m| !is_ours(m));
        }
        if cfg
            .get("customModels")
            .and_then(Value::as_array)
            .is_some_and(|a| a.is_empty())
        {
            let _ = cfg.as_object_mut().map(|o| o.remove("customModels"));
        }
        rewrite_file(&file, &serde_json::to_string_pretty(&cfg)?)?;
        Ok(vec![file])
    }
}

// ── Crush ($XDG_CONFIG_HOME/crush/crush.json) ───────────────────────────────
mod crush {
    use super::*;

    fn path(home: &Path) -> PathBuf {
        xdg_config_home(home).join("crush").join("crush.json")
    }

    /// `data.config.providers[<ours>].models[0].id`
    pub fn server_model(data: &Value) -> Option<String> {
        let providers = data.get("config")?.get("providers")?.as_object()?;
        let p = PROVIDER_KEYS.iter().find_map(|k| providers.get(*k))?;
        p.get("models")?
            .as_array()?
            .first()?
            .get("id")?
            .as_str()
            .map(str::to_string)
    }

    pub fn plan(home: &Path, ctx: &ApplyCtx) -> Result<Vec<(PathBuf, String)>> {
        let file = path(home);
        let mut cfg = read_json_or_empty(&file)?;
        let model = ctx.model.clone().unwrap_or_default();
        let root = obj_mut(&mut cfg);
        let providers = root
            .entry("providers".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        let providers = obj_mut(providers);
        providers.remove("9router");
        providers.insert(
            PROVIDER_KEY.to_string(),
            json!({
                "type": "openai-compat",
                "base_url": v1(&ctx.base_url),
                "api_key": ctx.api_key.clone(),
                "models": [{"id": model, "name": model, "context_window": 128000}],
            }),
        );
        Ok(vec![(file, serde_json::to_string_pretty(&cfg)?)])
    }

    fn ours<'a>(cfg: &'a Value) -> Option<&'a Map<String, Value>> {
        let providers = cfg.get("providers")?.as_object()?;
        PROVIDER_KEYS
            .iter()
            .find_map(|k| providers.get(*k).and_then(Value::as_object))
    }

    pub fn show(home: &Path) -> Result<ToolShow> {
        let cfg = read_json_or_empty(&path(home))?;
        let p = ours(&cfg);
        let ids: Vec<&str> = p
            .and_then(|p| p.get("models"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|m| m.get("id").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        let mut models = BTreeMap::new();
        if let Some(first) = ids.first() {
            models.insert("main".to_string(), (*first).to_string());
        }
        Ok(ToolShow {
            base_url: p
                .and_then(|p| p.get("base_url"))
                .and_then(Value::as_str)
                .map(str::to_string),
            api_key: p
                .and_then(|p| p.get("api_key"))
                .and_then(Value::as_str)
                .map(str::to_string),
            models,
            active: None,
        })
    }

    pub fn reset(home: &Path) -> Result<Vec<PathBuf>> {
        let file = path(home);
        let Some(mut cfg) = read_json_file(&file)? else {
            return Ok(vec![]);
        };
        let present = cfg
            .get("providers")
            .and_then(Value::as_object)
            .is_some_and(|p| PROVIDER_KEYS.iter().any(|k| p.contains_key(*k)));
        if !present {
            return Ok(vec![]);
        }
        if let Some(providers) = cfg.get_mut("providers").and_then(Value::as_object_mut) {
            for k in PROVIDER_KEYS {
                providers.remove(*k);
            }
            if providers.is_empty() {
                let _ = cfg.as_object_mut().map(|o| o.remove("providers"));
            }
        }
        rewrite_file(&file, &serde_json::to_string_pretty(&cfg)?)?;
        Ok(vec![file])
    }
}

// ── Kilo Code CLI (~/.local/share/kilo/auth.json) ───────────────────────────
mod kilo {
    use super::*;

    fn path(home: &Path) -> PathBuf {
        home.join(".local")
            .join("share")
            .join("kilo")
            .join("auth.json")
    }

    // No route upstream: the dashboard GET exposes only auth key names, so kilo
    // falls back to the shared model.

    pub fn plan(home: &Path, ctx: &ApplyCtx) -> Result<Vec<(PathBuf, String)>> {
        let file = path(home);
        let mut auth = read_json_or_empty(&file)?;
        let root = obj_mut(&mut auth);
        // Also drop provider-keyed leftovers so only `openai-compatible` remains.
        for k in PROVIDER_KEYS {
            root.remove(*k);
        }
        root.insert(
            "openai-compatible".to_string(),
            json!({
                "type": "api-key",
                "apiKey": ctx.api_key.clone(),
                "baseUrl": v1(&ctx.base_url),
                "model": ctx.model.clone(),
            }),
        );
        Ok(vec![(file, serde_json::to_string_pretty(&auth)?)])
    }

    pub fn show(home: &Path) -> Result<ToolShow> {
        let auth = read_json_or_empty(&path(home))?;
        // `openai-compatible` is what we write; provider keys cover files
        // written by other tooling.
        let a = ["openai-compatible"]
            .iter()
            .chain(PROVIDER_KEYS.iter())
            .find_map(|k| auth.get(*k));
        let mut models = BTreeMap::new();
        if let Some(m) = a.and_then(|a| a.get("model")).and_then(Value::as_str) {
            if !m.is_empty() {
                models.insert("main".to_string(), m.to_string());
            }
        }
        Ok(ToolShow {
            base_url: a
                .and_then(|a| a.get("baseUrl"))
                .and_then(Value::as_str)
                .map(str::to_string),
            api_key: a
                .and_then(|a| a.get("apiKey"))
                .and_then(Value::as_str)
                .map(str::to_string),
            models,
            active: None,
        })
    }

    pub fn reset(home: &Path) -> Result<Vec<PathBuf>> {
        let file = path(home);
        let Some(mut auth) = read_json_file(&file)? else {
            return Ok(vec![]);
        };
        let mut touched = false;
        if let Some(root) = auth.as_object_mut() {
            for k in ["openai-compatible"].iter().chain(PROVIDER_KEYS.iter()) {
                touched |= root.remove(*k).is_some();
            }
        }
        if !touched {
            return Ok(vec![]);
        }
        rewrite_file(&file, &serde_json::to_string_pretty(&auth)?)?;
        Ok(vec![file])
    }
}

// ── Cline CLI (~/.cline/data/{globalState,secrets}.json) ────────────────────
mod cline {
    use super::*;

    fn state_path(home: &Path) -> PathBuf {
        home.join(".cline").join("data").join("globalState.json")
    }
    fn secrets_path(home: &Path) -> PathBuf {
        home.join(".cline").join("data").join("secrets.json")
    }

    /// `data.settings` → model when provider is openai.
    pub fn server_model(data: &Value) -> Option<String> {
        let st = data.get("settings")?;
        if st.get("actModeApiProvider")?.as_str()? == "openai" {
            st.get("openAiModelId")?.as_str().map(str::to_string)
        } else {
            None
        }
    }

    pub fn plan(home: &Path, ctx: &ApplyCtx) -> Result<Vec<(PathBuf, String)>> {
        let state_file = state_path(home);
        let secrets_file = secrets_path(home);
        let mut state = read_json_or_empty(&state_file)?;
        let root = obj_mut(&mut state);
        root.insert("actModeApiProvider".into(), Value::String("openai".into()));
        root.insert("planModeApiProvider".into(), Value::String("openai".into()));
        // Cline expects base WITHOUT /v1.
        root.insert(
            "openAiBaseUrl".into(),
            Value::String(ctx.base_url.trim_end_matches('/').to_string()),
        );
        let model = ctx.model.clone().unwrap_or_default();
        root.insert("openAiModelId".into(), Value::String(model.clone()));
        root.insert("planModeOpenAiModelId".into(), Value::String(model));

        let mut secrets = read_json_or_empty(&secrets_file)?;
        obj_mut(&mut secrets).insert("openAiApiKey".into(), Value::String(ctx.api_key.clone()));

        Ok(vec![
            (state_file, serde_json::to_string_pretty(&state)?),
            (secrets_file, serde_json::to_string_pretty(&secrets)?),
        ])
    }

    pub fn show(home: &Path) -> Result<ToolShow> {
        let st = read_json_or_empty(&state_path(home))?;
        let mut models = BTreeMap::new();
        if let Some(m) = st.get("openAiModelId").and_then(Value::as_str) {
            if !m.is_empty() {
                models.insert("act".to_string(), m.to_string());
            }
        }
        if let Some(m) = st.get("planModeOpenAiModelId").and_then(Value::as_str) {
            if !m.is_empty() {
                models.insert("plan".to_string(), m.to_string());
            }
        }
        let api_key = read_json_file(&secrets_path(home))?.and_then(|s| {
            s.get("openAiApiKey")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        Ok(ToolShow {
            base_url: st
                .get("openAiBaseUrl")
                .and_then(Value::as_str)
                .map(str::to_string),
            api_key,
            models,
            active: st
                .get("actModeApiProvider")
                .and_then(Value::as_str)
                .map(|p| p == "openai"),
        })
    }

    pub fn reset(home: &Path) -> Result<Vec<PathBuf>> {
        let state_file = state_path(home);
        let secrets_file = secrets_path(home);
        let mut touched = Vec::new();
        if let Some(mut state) = read_json_file(&state_file)? {
            let is_openai =
                state.get("actModeApiProvider").and_then(Value::as_str) == Some("openai");
            if is_openai {
                if let Some(root) = state.as_object_mut() {
                    for k in ["openAiBaseUrl", "openAiModelId", "planModeOpenAiModelId"] {
                        root.remove(k);
                    }
                    root.insert("actModeApiProvider".into(), Value::String("cline".into()));
                    root.insert("planModeApiProvider".into(), Value::String("cline".into()));
                }
                rewrite_file(&state_file, &serde_json::to_string_pretty(&state)?)?;
                touched.push(state_file.clone());
            }
        }
        if let Some(mut secrets) = read_json_file(&secrets_file)? {
            if let Some(root) = secrets.as_object_mut() {
                if root.remove("openAiApiKey").is_some() {
                    rewrite_file(&secrets_file, &serde_json::to_string_pretty(&secrets)?)?;
                    touched.push(secrets_file);
                }
            }
        }
        Ok(touched)
    }
}

// ── Pi (pi-coding-agent) (~/.pi/agent/models.json) ──────────────────────────
mod pi {
    use super::*;

    /// `data.config.providers[<ours>].models[0]` — string or `{id}`.
    pub fn server_model(data: &Value) -> Option<String> {
        let providers = data.get("config")?.get("providers")?.as_object()?;
        let p = PROVIDER_KEYS.iter().find_map(|k| providers.get(*k))?;
        let models = p.get("models")?.as_array()?.first()?;
        match models {
            Value::String(s) => Some(s.clone()),
            other => other.get("id")?.as_str().map(str::to_string),
        }
    }

    pub fn plan(home: &Path, ctx: &ApplyCtx) -> Result<Vec<(PathBuf, String)>> {
        let file = pi_path(home);
        let mut cfg = read_json_or_empty(&file)?;
        let model = ctx.model.clone().unwrap_or_default();
        let root = obj_mut(&mut cfg);
        let providers = root
            .entry("providers".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        let providers = obj_mut(providers);
        providers.remove("9router");
        let mut p = providers
            .get(PROVIDER_KEY)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        p.insert("baseUrl".into(), Value::String(v1(&ctx.base_url)));
        p.insert("apiKey".into(), Value::String(ctx.api_key.clone()));
        p.insert("api".into(), Value::String("openai-completions".into()));
        p.insert(
            "models".into(),
            json!([{"id": model, "name": model, "contextWindow": 128000, "maxTokens": 16384}]),
        );
        providers.insert(PROVIDER_KEY.to_string(), Value::Object(p));
        Ok(vec![(file, serde_json::to_string_pretty(&cfg)?)])
    }

    fn ours<'a>(cfg: &'a Value) -> Option<&'a Map<String, Value>> {
        let providers = cfg.get("providers")?.as_object()?;
        PROVIDER_KEYS
            .iter()
            .find_map(|k| providers.get(*k).and_then(Value::as_object))
    }

    pub fn show(home: &Path) -> Result<ToolShow> {
        let cfg = read_json_or_empty(&pi_path(home))?;
        let p = ours(&cfg);
        let ids: Vec<String> = p
            .and_then(|p| p.get("models"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|m| match m {
                        Value::String(s) => Some(s.clone()),
                        other => other.get("id").and_then(Value::as_str).map(str::to_string),
                    })
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let mut models = BTreeMap::new();
        if let Some(first) = ids.first() {
            models.insert("main".to_string(), first.clone());
            if ids.len() > 1 {
                models.insert("available".to_string(), ids.join(", "));
            }
        }
        Ok(ToolShow {
            base_url: p
                .and_then(|p| p.get("baseUrl"))
                .and_then(Value::as_str)
                .map(str::to_string),
            api_key: p
                .and_then(|p| p.get("apiKey"))
                .and_then(Value::as_str)
                .map(str::to_string),
            models,
            active: None,
        })
    }

    pub fn reset(home: &Path) -> Result<Vec<PathBuf>> {
        let file = pi_path(home);
        let Some(mut cfg) = read_json_file(&file)? else {
            return Ok(vec![]);
        };
        let present = cfg
            .get("providers")
            .and_then(Value::as_object)
            .is_some_and(|p| PROVIDER_KEYS.iter().any(|k| p.contains_key(*k)));
        if !present {
            return Ok(vec![]);
        }
        if let Some(providers) = cfg.get_mut("providers").and_then(Value::as_object_mut) {
            for k in PROVIDER_KEYS {
                providers.remove(*k);
            }
            if providers.is_empty() {
                let _ = cfg.as_object_mut().map(|o| o.remove("providers"));
            }
        }
        rewrite_file(&file, &serde_json::to_string_pretty(&cfg)?)?;
        Ok(vec![file])
    }
}

// ── Oh My Pi (~/.omp/agent/models.yml, YAML) ────────────────────────────────
// Proxy discovery lists every server model under /model, so no model is
// written (upstream `needsModel: false`).
mod omp {
    use super::*;

    fn path(home: &Path) -> PathBuf {
        home.join(".omp").join("agent").join("models.yml")
    }

    fn ours<'a>(cfg: &'a Value) -> Option<&'a Map<String, Value>> {
        let providers = cfg.get("providers")?.as_object()?;
        PROVIDER_KEYS
            .iter()
            .find_map(|k| providers.get(*k).and_then(Value::as_object))
    }

    pub fn plan(home: &Path, ctx: &ApplyCtx) -> Result<Vec<(PathBuf, String)>> {
        let file = path(home);
        let mut cfg = read_yaml_file(&file)?.unwrap_or(json!({}));
        let root = obj_mut(&mut cfg);
        let providers = root
            .entry("providers".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        let providers = obj_mut(providers);
        providers.remove("9router");
        providers.insert(
            PROVIDER_KEY.to_string(),
            json!({
                "baseUrl": v1(&ctx.base_url),
                "apiKey": ctx.api_key.clone(),
                "api": "openai-completions",
                "authHeader": true,
                "disableStrictTools": true,
                "discovery": {"type": "proxy"},
            }),
        );
        // plan returns (path, content); yaml-serialise now so dry-run sees text.
        Ok(vec![(file, yaml_string(&cfg)?)])
    }

    pub fn show(home: &Path) -> Result<ToolShow> {
        let cfg = read_yaml_file(&path(home))?.unwrap_or(json!({}));
        let p = ours(&cfg);
        let proxy = p
            .and_then(|p| p.get("discovery"))
            .and_then(|d| d.get("type"))
            .and_then(Value::as_str)
            == Some("proxy");
        let mut models = BTreeMap::new();
        if proxy {
            models.insert(
                "main".to_string(),
                "(all server models, via discovery)".to_string(),
            );
        }
        Ok(ToolShow {
            base_url: p
                .and_then(|p| p.get("baseUrl"))
                .and_then(Value::as_str)
                .map(str::to_string),
            api_key: p
                .and_then(|p| p.get("apiKey"))
                .and_then(Value::as_str)
                .map(str::to_string),
            models,
            active: None,
        })
    }

    pub fn reset(home: &Path) -> Result<Vec<PathBuf>> {
        let file = path(home);
        let Some(mut cfg) = read_yaml_file(&file)? else {
            return Ok(vec![]);
        };
        let present = cfg
            .get("providers")
            .and_then(Value::as_object)
            .is_some_and(|p| PROVIDER_KEYS.iter().any(|k| p.contains_key(*k)));
        if !present {
            return Ok(vec![]);
        }
        if let Some(providers) = cfg.get_mut("providers").and_then(Value::as_object_mut) {
            for k in PROVIDER_KEYS {
                providers.remove(*k);
            }
            if providers.is_empty() {
                let _ = cfg.as_object_mut().map(|o| o.remove("providers"));
            }
        }
        // Empty doc → empty file (upstream writes "").
        let content = if cfg.as_object().is_some_and(|o| o.is_empty()) {
            String::new()
        } else {
            yaml_string(&cfg)?
        };
        rewrite_file(&file, &content)?;
        Ok(vec![file])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ApplyCtx {
        let mut claude_models = BTreeMap::new();
        claude_models.insert(
            "ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(),
            Some("cc/claude-sonnet-5-5".to_string()),
        );
        claude_models.insert("ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(), None);
        ApplyCtx {
            base_url: "http://server.local:4623".to_string(),
            api_key: "op-test-secret".to_string(),
            model: Some("ocg/deepseek-flash".to_string()),
            claude_models,
        }
    }

    #[test]
    fn resolve_tools_aliases_and_all() {
        let tools =
            ConnectTool::resolve(&["claude-code".into(), "oh-my-pi".into(), "KiloCode".into()])
                .unwrap();
        assert_eq!(
            tools,
            vec![ConnectTool::Claude, ConnectTool::Omp, ConnectTool::Kilo]
        );
        assert!(ConnectTool::resolve(&["bogus".into()]).is_err());
        assert_eq!(ConnectTool::resolve(&["all".into()]).unwrap().len(), 9);
    }

    #[test]
    fn strip_trailing_commas_jsonc() {
        assert_eq!(strip_trailing_commas("{\"a\":1,}"), "{\"a\":1}");
        assert_eq!(strip_trailing_commas("{\"a\":\"x,}\"}"), "{\"a\":\"x,}\"}");
        assert_eq!(strip_trailing_commas("[1, 2,]"), "[1, 2]");
    }

    #[test]
    fn claude_plan_writes_expected_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let files = ConnectTool::Claude.plan_apply(home, &ctx()).unwrap();
        assert_eq!(files.len(), 1);
        let (path, content) = &files[0];
        assert_eq!(path, &home.join(".claude").join("settings.json"));
        let v: Value = serde_json::from_str(content).unwrap();
        assert_eq!(v["hasCompletedOnboarding"], json!(true));
        let env = &v["env"];
        assert_eq!(
            env["ANTHROPIC_BASE_URL"],
            json!("http://server.local:4623/v1")
        );
        assert_eq!(env["ANTHROPIC_AUTH_TOKEN"], json!("op-test-secret"));
        assert_eq!(
            env["ANTHROPIC_DEFAULT_SONNET_MODEL"],
            json!("cc/claude-sonnet-5-5")
        );
        // Unset tier is removed, not written as null.
        assert!(env.get("ANTHROPIC_DEFAULT_OPUS_MODEL").is_none());
        assert!(env.get("ANTHROPIC_DEFAULT_FABLE_MODEL").is_none());
        assert!(env.get("ANTHROPIC_DEFAULT_HAIKU_MODEL").is_none());
    }

    #[test]
    fn claude_apply_then_show_then_reset_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let written = ConnectTool::Claude.apply(home, &ctx()).unwrap();
        assert_eq!(written.len(), 1);
        // Backup was created on first write of a pre-existing file only —
        // file did not exist, so no backup.
        assert!(!home.join(".claude/settings.json.bak-openproxy").exists());

        let shown = ConnectTool::Claude.show(home).unwrap();
        assert_eq!(
            shown.base_url.as_deref(),
            Some("http://server.local:4623/v1")
        );
        assert_eq!(shown.api_key.as_deref(), Some("op-test-secret"));
        assert_eq!(
            shown.models.get("sonnet").map(String::as_str),
            Some("cc/claude-sonnet-5-5")
        );
        assert!(shown.configured());

        let touched = ConnectTool::Claude.reset(home).unwrap();
        assert_eq!(touched.len(), 1);
        let after = ConnectTool::Claude.show(home).unwrap();
        assert!(!after.configured());
        // Second reset touches nothing.
        assert!(ConnectTool::Claude.reset(home).unwrap().is_empty());
    }

    #[test]
    fn opencode_plan_uses_openproxy_provider_key() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let files = ConnectTool::OpenCode.plan_apply(home, &ctx()).unwrap();
        let (_, content) = &files[0];
        let v: Value = serde_json::from_str(content).unwrap();
        let p = &v["provider"]["openproxy"];
        assert_eq!(
            p["options"]["baseURL"],
            json!("http://server.local:4623/v1")
        );
        assert_eq!(p["options"]["apiKey"], json!("op-test-secret"));
        assert!(p["models"]["ocg/deepseek-flash"].is_object());
        assert_eq!(v["model"], json!("openproxy/ocg/deepseek-flash"));
        assert_eq!(
            v["agent"]["explorer"]["model"],
            json!("openproxy/ocg/deepseek-flash")
        );
        // 9router key must not be written by this build.
        assert!(v["provider"].get("9router").is_none());
    }

    #[test]
    fn codex_plan_emits_valid_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let files = ConnectTool::Codex.plan_apply(home, &ctx()).unwrap();
        let (path, content) = &files[0];
        assert!(path.ends_with("config.toml"));
        // Round-trip: the emitted document must parse and carry our provider.
        let parsed: toml::Value = toml::from_str(content).unwrap();
        assert_eq!(
            parsed.get("model_provider").and_then(|v| v.as_str()),
            Some("openproxy")
        );
        assert_eq!(
            parsed
                .get("model_providers")
                .and_then(|m| m.get("openproxy"))
                .and_then(|p| p.get("base_url"))
                .and_then(|v| v.as_str()),
            Some("http://server.local:4623/v1")
        );
        assert!(content.contains("[model_providers.openproxy.http_headers]"));
        assert!(content.contains("Authorization = \"Bearer op-test-secret\""));
    }

    #[test]
    fn omp_plan_emits_yaml_provider() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let files = ConnectTool::Omp.plan_apply(home, &ctx()).unwrap();
        let (path, content) = &files[0];
        assert!(path.ends_with("models.yml"));
        let yml: serde_yml::Value = serde_yml::from_str(content).unwrap();
        let json_bytes = serde_json::to_vec(&yml).unwrap();
        let v: Value = serde_json::from_slice(&json_bytes).unwrap();
        let p = &v["providers"]["openproxy"];
        assert_eq!(p["api"], json!("openai-completions"));
        assert_eq!(p["discovery"]["type"], json!("proxy"));
        assert_eq!(p["baseUrl"], json!("http://server.local:4623/v1"));
    }

    #[test]
    fn kilo_and_cline_and_droid_plans() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        for tool in [
            ConnectTool::Kilo,
            ConnectTool::Cline,
            ConnectTool::Droid,
            ConnectTool::Pi,
            ConnectTool::Crush,
        ] {
            let files = tool.plan_apply(home, &ctx()).unwrap();
            assert!(!files.is_empty(), "{} produced no files", tool.id());
            for (path, content) in &files {
                // Every planned payload must be non-empty valid-ish text.
                assert!(
                    !content.trim().is_empty(),
                    "{} empty content",
                    path.display()
                );
            }
        }
        // Spot-check kilo auth shape.
        let files = ConnectTool::Kilo.plan_apply(home, &ctx()).unwrap();
        let v: Value = serde_json::from_str(&files[0].1).unwrap();
        assert_eq!(v["openai-compatible"]["type"], json!("api-key"));
        assert_eq!(v["openai-compatible"]["model"], json!("ocg/deepseek-flash"));
    }
}
