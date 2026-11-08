//! Pure text helpers for editing Hermes `config.yaml` / `.env` in place.
//! Port of 9router `src/app/api/cli-tools/hermes-settings/hermesYaml.js`.
//!
//! Hermes YAML shapes (see https://hermes-agent.nousresearch.com/docs/user-guide/configuration):
//!   model:                      # top-level mapping block (children indented)
//!     provider: "custom"
//!     default: "provider/model"
//!   model: ""                   # fresh install ships this scalar sentinel ("not configured")
//!   delegation: { ... }         # top-level, subagent model
//!   auxiliary: { <task>: { provider, model, base_url, api_key } }
//!   stt:                        # speech-to-text; openai backend honours base_url override

use once_cell::sync::Lazy;
use regex::{Captures, Regex};
use serde_json::{json, Map, Value};

/// Top-level "model:" mapping block — key on its own line, indented children.
/// NOTE: does NOT match the scalar sentinel `model: ""` (no newline right after `model:`).
pub static MODEL_BLOCK_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^model:[ \t]*\r?\n((?:[ \t]+.*\r?\n?|[ \t]*\r?\n)*)").unwrap()
});
/// Single-line form: `model: ""`, `model: ''`, `model:` at EOF. Anchored without indent so
/// only the top-level key matches (`model_aliases:` never does — `:` must follow "model").
pub static MODEL_SCALAR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^model:[ \t]*[^\r\n]*\r?\n?").unwrap());
/// Top-level "delegation:" block (until next non-indented, non-empty line).
pub static DELEGATION_BLOCK_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^delegation:[ \t]*\r?\n((?:[ \t]+.*\r?\n?|[ \t]*\r?\n)*)").unwrap()
});
/// "auxiliary:" block; children are 2-space-indented role keys with 4+-space fields.
pub static AUX_BLOCK_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^auxiliary:[ \t]*\r?\n((?:(?:[ \t]+.*\r?\n?)|(?:[ \t]*\r?\n))*)").unwrap()
});
/// Sub-entries inside the auxiliary body: `  <role>:` followed by 4+-space fields.
static AUX_ROLE_SUB_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?m)^  ([A-Za-z0-9_]+):[ \t]*\r?\n((?:(?:[ \t]{4,}.*\r?\n?)|(?:[ \t]*\r?\n))*)",
    )
    .unwrap()
});
/// Top-level "stt:" block — same capture shape as delegation.
pub static STT_BLOCK_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^stt:[ \t]*\r?\n((?:[ \t]+.*\r?\n?|[ \t]*\r?\n)*)").unwrap()
});

fn aux_role_re(role: &str) -> Regex {
    Regex::new(&format!(
        r"(?m)^  {}:[ \t]*\r?\n(?:(?:[ \t]{{4,}}.*\r?\n?)|(?:[ \t]*\r?\n))*",
        regex::escape(role)
    ))
    .unwrap()
}

fn stt_sub_re(name: &str) -> Regex {
    Regex::new(&format!(
        r"(?m)^  {}:[ \t]*\r?\n((?:(?:[ \t]{{4,}}.*\r?\n?)|(?:[ \t]*\r?\n))*)",
        regex::escape(name)
    ))
    .unwrap()
}

/// The api_key reference our STT wiring writes — the fingerprint that marks a block
/// as ours (a user's own OpenAI audio setup keeps their own key or none).
pub const STT_API_KEY_REF: &str = "${OPENAI_API_KEY}";

pub fn build_model_block(model: &str, base_url: &str) -> String {
    format!(
        "model:\n  default: \"{model}\"\n  provider: \"custom\"\n  base_url: \"{base_url}\"\n  api_key: {STT_API_KEY_REF}\n"
    )
}

pub fn build_delegation_block(model: &str, base_url: &str) -> String {
    format!(
        "delegation:\n  model: \"{model}\"\n  provider: \"custom\"\n  base_url: \"{base_url}\"\n  api_key: {STT_API_KEY_REF}\n"
    )
}

pub fn build_aux_role_block(role: &str, model: &str, base_url: &str) -> String {
    format!(
        "  {role}:\n    provider: \"custom\"\n    model: \"{model}\"\n    base_url: \"{base_url}\"\n    api_key: {STT_API_KEY_REF}\n"
    )
}

/// `^[ \t]+<key>:[ \t]*["']?([^"'\r\n]+)["']?` — indented `key: value`, value required.
fn extract_field(body: &str, key: &str) -> Option<String> {
    let pattern = format!(
        r"(?m)^[ \t]+{}:[ \t]*[\"']?([^\"'\r\n]+)[\"']?",
        regex::escape(key)
    );
    Regex::new(&pattern)
        .ok()
        .and_then(|re| re.captures(body))
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().trim().to_string())
}

/// Parse the top-level model block back to fields (best-effort, simple key:value).
/// Returns None when there is no mapping block — i.e. unset or the `model: ""` sentinel.
pub fn parse_model_block(yaml: &str) -> Option<Value> {
    let captures = MODEL_BLOCK_RE.captures(yaml)?;
    let body = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
    Some(json!({
        "default": extract_field(body, "default"),
        "provider": extract_field(body, "provider"),
        "base_url": extract_field(body, "base_url"),
        "api_key": extract_field(body, "api_key"),
    }))
}

pub fn parse_delegation_block(yaml: &str) -> Option<Value> {
    let captures = DELEGATION_BLOCK_RE.captures(yaml)?;
    let body = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
    Some(json!({
        "model": extract_field(body, "model"),
        "provider": extract_field(body, "provider"),
        "base_url": extract_field(body, "base_url"),
    }))
}

/// role -> { model, provider, base_url } for every entry under "auxiliary:".
pub fn parse_aux_roles(yaml: &str) -> Map<String, Value> {
    let mut roles = Map::new();
    let Some(captures) = AUX_BLOCK_RE.captures(yaml) else {
        return roles;
    };
    let body = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
    for sub in AUX_ROLE_SUB_RE.captures_iter(body) {
        let role = sub.get(1).map(|m| m.as_str()).unwrap_or_default();
        let sub_body = sub.get(2).map(|m| m.as_str()).unwrap_or_default();
        roles.insert(
            role.to_string(),
            json!({
                "model": extract_field(sub_body, "model"),
                "provider": extract_field(sub_body, "provider"),
                "base_url": extract_field(sub_body, "base_url"),
            }),
        );
    }
    roles
}

pub fn upsert_model_block(yaml: &str, new_block: &str) -> String {
    if MODEL_BLOCK_RE.is_match(yaml) {
        MODEL_BLOCK_RE
            .replace(yaml, |_: &Captures<'_>| new_block.to_string())
            .into_owned()
    } else if MODEL_SCALAR_RE.is_match(yaml) {
        // Scalar sentinel would otherwise survive the upsert and produce a duplicate
        // `model:` key (last-wins in most YAML loaders → the block is silently ignored).
        MODEL_SCALAR_RE
            .replace(yaml, |_: &Captures<'_>| new_block.to_string())
            .into_owned()
    } else if yaml.is_empty() {
        new_block.to_string()
    } else {
        format!("{new_block}\n{yaml}")
    }
}

pub fn upsert_delegation_block(yaml: &str, new_block: &str) -> String {
    if DELEGATION_BLOCK_RE.is_match(yaml) {
        DELEGATION_BLOCK_RE
            .replace(yaml, |_: &Captures<'_>| new_block.to_string())
            .into_owned()
    } else if yaml.is_empty() || yaml.ends_with('\n') {
        format!("{yaml}{new_block}")
    } else {
        format!("{yaml}\n{new_block}")
    }
}

pub fn remove_delegation_block(yaml: &str) -> String {
    DELEGATION_BLOCK_RE.replace(yaml, "").into_owned()
}

pub fn upsert_aux_role(yaml: &str, role: &str, role_block: &str) -> String {
    let re = aux_role_re(role);
    let Some(captures) = AUX_BLOCK_RE.captures(yaml) else {
        let block = format!("auxiliary:\n{role_block}");
        return if yaml.is_empty() || yaml.ends_with('\n') {
            format!("{yaml}{block}")
        } else {
            format!("{yaml}\n{block}")
        };
    };
    let body = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
    let new_body = if re.is_match(body) {
        re.replace(body, |_: &Captures<'_>| role_block.to_string())
            .into_owned()
    } else {
        format!("{body}{role_block}")
    };
    splice_match(yaml, &captures, &format!("auxiliary:\n{new_body}"))
}

pub fn remove_aux_role(yaml: &str, role: &str) -> String {
    let Some(captures) = AUX_BLOCK_RE.captures(yaml) else {
        return yaml.to_string();
    };
    let body = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
    let new_body = aux_role_re(role).replace(body, "").into_owned();
    let replacement = if new_body.trim().is_empty() {
        String::new()
    } else {
        format!("auxiliary:\n{new_body}")
    };
    splice_match(yaml, &captures, &replacement)
}

/// Replace the text covered by `captures` group 0 with `replacement`.
fn splice_match(yaml: &str, captures: &Captures<'_>, replacement: &str) -> String {
    let whole = captures.get(0).unwrap();
    format!("{}{}{}", &yaml[..whole.start()], replacement, &yaml[whole.end()..])
}

pub fn remove_model_block(yaml: &str) -> String {
    let mut out = yaml.to_string();
    if MODEL_BLOCK_RE.is_match(&out) {
        out = MODEL_BLOCK_RE.replace(&out, "").into_owned();
    } else if MODEL_SCALAR_RE.is_match(&out) {
        out = MODEL_SCALAR_RE.replace(&out, "").into_owned();
    }
    out.trim_start_matches('\n').to_string()
}

// ── .env helpers — upsert single KEY=VALUE line ──────────────────────────────

pub fn upsert_env_var(env_text: &str, key: &str, value: &str) -> String {
    let line = format!("{key}={value}");
    let pattern = format!(r"(?m)^{}=.*$", regex::escape(key));
    let re = Regex::new(&pattern).unwrap();
    if re.is_match(env_text) {
        re.replace(env_text, line).into_owned()
    } else if env_text.is_empty() {
        format!("{line}\n")
    } else if env_text.ends_with('\n') {
        format!("{env_text}{line}\n")
    } else {
        format!("{env_text}\n{line}\n")
    }
}

// ── 9router/openproxy detection ──────────────────────────────────────────────

/// Detect openproxy by base_url containing localhost/127.0.0.1 or matching tunnel URL.
pub fn has_openproxy_config(model_cfg: Option<&Value>) -> bool {
    let Some(cfg) = model_cfg else { return false };
    let Some(base_url) = cfg.get("base_url").and_then(Value::as_str) else {
        return false;
    };
    let is_custom = cfg.get("provider").and_then(Value::as_str) == Some("custom");
    is_custom
        && (base_url.contains("localhost")
            || base_url.contains("127.0.0.1")
            || base_url.contains("0.0.0.0"))
}

/// A block openproxy wrote (provider "custom") regardless of endpoint — covers tunnel
/// URLs, so reset/bulk logic can find them even when base_url is not localhost.
pub fn is_custom_block(cfg: Option<&Value>) -> bool {
    cfg.and_then(|c| c.get("provider"))
        .and_then(Value::as_str)
        == Some("custom")
}

// ── stt: audio transcription via openproxy ───────────────────────────────────
// Hermes runs voice messages / audio attachments through `stt.provider`; the "openai"
// backend is OpenAI-SDK-shaped and honours stt.openai.base_url + stt.openai.api_key as
// endpoint overrides, and load_config() expands the ${OPENAI_API_KEY} reference from the
// profile's .env. Pointing it at openproxy's /v1/audio/transcriptions routes every
// transcription through the proxy:
//
//   stt:
//     enabled: true
//     provider: "openai"
//     openai:
//       model: "groq/whisper-large-v3"   # provider-qualified — openproxy routes on it
//       base_url: "http://127.0.0.1:4623/v1"
//       api_key: ${OPENAI_API_KEY}

fn strip_yaml_quotes(value: &str) -> String {
    let mut t = value.trim();
    if let Some(rest) = t.strip_prefix('"').or_else(|| t.strip_prefix('\'')) {
        t = rest;
    }
    if let Some(rest) = t.strip_suffix('"').or_else(|| t.strip_suffix('\'')) {
        t = rest;
    }
    t.trim().to_string()
}

/// Current stt wiring back to fields (best-effort, simple key:value). Returns None when
/// there is no top-level stt block. `model`/`base_url`/`api_key` read the "openai"
/// sub-block — the backend our override targets; sibling settings are ignored.
pub fn parse_stt_block(yaml: &str) -> Option<Value> {
    let captures = STT_BLOCK_RE.captures(yaml)?;
    let body = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
    let direct_re = Regex::new(r"^  ([A-Za-z0-9_]+):[ \t]*(.*)$").unwrap();
    let openai_re = Regex::new(r"^ {4,}([A-Za-z0-9_]+):[ \t]*(.*)$").unwrap();
    let mut direct: Map<String, Value> = Map::new();
    let mut openai: Map<String, Value> = Map::new();
    let mut current: Option<String> = None;

    for line in body.lines() {
        // `^  \S` — exactly two-space indent (a 4+-space line belongs to a sub-block).
        if line.starts_with("  ") && !line.starts_with("   ") && line[2..].starts_with(|c: char| !c.is_whitespace()) {
            if let Some(kv) = direct_re.captures(line) {
                let key = kv.get(1).map(|m| m.as_str()).unwrap_or_default();
                let val = kv.get(2).map(|m| m.as_str()).unwrap_or_default();
                current = None;
                if !val.is_empty() {
                    direct.insert(key.to_string(), json!(strip_yaml_quotes(val)));
                } else {
                    // valueless key = sub-block head
                    current = Some(key.to_string());
                }
            }
        } else if current.as_deref() == Some("openai") {
            // `^ {4,}\S` — at least 4 leading spaces followed by a non-space char.
            let stripped = line.trim_start_matches(' ');
            let indent = line.len() - stripped.len();
            if indent >= 4 && stripped.starts_with(|c: char| !c.is_whitespace()) {
                if let Some(kv) = openai_re.captures(line) {
                    let key = kv.get(1).map(|m| m.as_str()).unwrap_or_default();
                    let val = kv.get(2).map(|m| m.as_str()).unwrap_or_default();
                    if !val.is_empty() {
                        openai.insert(key.to_string(), json!(strip_yaml_quotes(val)));
                    }
                }
            }
        }
    }

    Some(json!({
        "enabled": direct.get("enabled").cloned().unwrap_or(Value::Null),
        "provider": direct.get("provider").cloned().unwrap_or(Value::Null),
        "model": openai.get("model").cloned().unwrap_or(Value::Null),
        "base_url": openai.get("base_url").cloned().unwrap_or(Value::Null),
        "api_key": openai.get("api_key").cloned().unwrap_or(Value::Null),
    }))
}

/// The stt wiring this card wrote: openai backend + the profile key reference. The api_key
/// reference is the fingerprint — a user's own OpenAI audio setup keeps their own key (or
/// none), while ours always points at the OPENAI_API_KEY line we upsert in .env.
pub fn is_openproxy_stt(cfg: Option<&Value>) -> bool {
    let Some(cfg) = cfg else { return false };
    cfg.get("provider").and_then(Value::as_str) == Some("openai")
        && cfg.get("base_url").and_then(Value::as_str).is_some_and(|v| !v.is_empty())
        && cfg.get("api_key").and_then(Value::as_str) == Some(STT_API_KEY_REF)
}

pub fn build_stt_block(model: &str, base_url: &str) -> String {
    format!(
        "stt:\n  enabled: true\n  provider: \"openai\"\n  openai:\n    model: \"{model}\"\n    base_url: \"{base_url}\"\n    api_key: {STT_API_KEY_REF}\n"
    )
}

/// Replace-or-append one `  key: value` direct child of the stt block body.
fn upsert_stt_direct(body: &str, key: &str, value: &str) -> String {
    let re = Regex::new(&format!(r"(?m)^  {}:[^\r\n]*", regex::escape(key))).unwrap();
    if re.is_match(body) {
        re.replace(body, format!("  {key}: {value}")).into_owned()
    } else {
        let head = format!("  {key}: {value}\n");
        if !body.is_empty() && !body.ends_with('\n') {
            format!("{body}\n{head}")
        } else {
            format!("{body}{head}")
        }
    }
}

/// Replace-or-append `    key: value` inside the stt "openai" sub-block; creates the
/// sub-block when absent. Scoped to the sub-block so sibling sections (stt.local.model)
/// can never be touched by the replace.
fn upsert_stt_openai(body: &str, key: &str, value: &str) -> String {
    let re = stt_sub_re("openai");
    let Some(captures) = re.captures(body) else {
        let block = format!("  openai:\n    {key}: {value}\n");
        return if !body.is_empty() && !body.ends_with('\n') {
            format!("{body}\n{block}")
        } else {
            format!("{body}{block}")
        };
    };
    let sub_body = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
    let kv_re = Regex::new(&format!(r"(?m)^ {{4}}{}:[^\r\n]*", regex::escape(key))).unwrap();
    let new_sub_body = if kv_re.is_match(sub_body) {
        kv_re
            .replace(sub_body, format!("    {key}: {value}"))
            .into_owned()
    } else {
        let line = format!("    {key}: {value}\n");
        if sub_body.is_empty() || sub_body.ends_with('\n') {
            format!("{sub_body}{line}")
        } else {
            format!("{sub_body}\n{line}")
        }
    };
    splice_match(body, &captures, &format!("  openai:\n{new_sub_body}"))
}

/// Write the openproxy STT wiring, preserving every sibling setting the user already has
/// (language, local model, echo_transcripts, ...). Upserts the openai backend keys only.
pub fn upsert_stt_openproxy(yaml: &str, model: &str, base_url: &str) -> String {
    let Some(captures) = STT_BLOCK_RE.captures(yaml) else {
        let prefix = if yaml.is_empty() {
            String::new()
        } else if yaml.ends_with('\n') {
            yaml.to_string()
        } else {
            format!("{yaml}\n")
        };
        return format!("{prefix}{}", build_stt_block(model, base_url));
    };
    let body = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
    let mut new_body = body.to_string();
    new_body = upsert_stt_direct(&new_body, "enabled", "true");
    new_body = upsert_stt_direct(&new_body, "provider", "\"openai\"");
    new_body = upsert_stt_openai(&new_body, "model", &format!("\"{model}\""));
    new_body = upsert_stt_openai(&new_body, "base_url", &format!("\"{base_url}\""));
    new_body = upsert_stt_openai(&new_body, "api_key", STT_API_KEY_REF);
    splice_match(yaml, &captures, &format!("stt:\n{new_body}"))
}

/// Drop only the keys the wiring above writes — enabled (only when true, the default),
/// provider (only when "openai") and the openai model/base_url/api_key trio. Anything the
/// user owns (language, local, an explicit provider choice) survives.
pub fn remove_stt_openproxy(yaml: &str) -> String {
    let Some(captures) = STT_BLOCK_RE.captures(yaml) else {
        return yaml.to_string();
    };
    let body = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
    let enabled_re =
        Regex::new(r"(?m)^  enabled:[ \t]*[\"']?true[\"']?[ \t]*\r?\n?").unwrap();
    let provider_re =
        Regex::new(r"(?m)^  provider:[ \t]*[\"']?openai[\"']?[ \t]*\r?\n?").unwrap();
    let mut new_body = enabled_re.replace(body, "").into_owned();
    new_body = provider_re.replace(&new_body, "").into_owned();

    let sub = stt_sub_re("openai").captures(&new_body);
    if let Some(sub_caps) = sub {
        let sub_body = sub_caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        let mut new_sub_body = sub_body.to_string();
        for key in ["model", "base_url", "api_key"] {
            let kv_re =
                Regex::new(&format!(r"(?m)^ {{4}}{}:[^\r\n]*\r?\n?", regex::escape(key)))
                    .unwrap();
            new_sub_body = kv_re.replace(&new_sub_body, "").into_owned();
        }
        let replacement = if new_sub_body.trim().is_empty() {
            String::new()
        } else {
            format!("  openai:\n{new_sub_body}")
        };
        new_body = splice_match(&new_body, &sub_caps, &replacement);
    }

    if new_body.trim().is_empty() {
        let out = STT_BLOCK_RE.replace(yaml, "").into_owned();
        out.trim_start_matches('\n').to_string()
    } else {
        splice_match(yaml, &captures, &format!("stt:\n{new_body}"))
    }
}
