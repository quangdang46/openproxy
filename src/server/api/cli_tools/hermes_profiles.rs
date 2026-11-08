//! Hermes profile discovery + path resolution.
//! Port of 9router `src/app/api/cli-tools/hermes-settings/hermesProfiles.js`.
//!
//! A Hermes profile is a separate home directory:
//!   ~/.hermes                      → the "default" profile (the install root)
//!   ~/.hermes/profiles/<name>      → named profile
//! Hermes only treats a directory under profiles/ as a profile when it carries one of the
//! identity files below; a bare directory left by logging/cron is ignored.
//! Everything is derived from files on disk — no `hermes` subprocess is spawned, so there
//! is no shell interpolation surface at all.

use std::env;
use std::path::PathBuf;

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};
use tokio::fs;

use super::hermes_yaml;

/// Profile names become directory names and command aliases — keep them inert.
static PROFILE_NAME_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$").unwrap());
const IDENTITY_FILES: [&str; 6] = [
    "config.yaml",
    ".env",
    "SOUL.md",
    "profile.yaml",
    "auth.json",
    "state.db",
];

pub struct HermesHome {
    pub name: String,
    pub dir: PathBuf,
    pub config_path: PathBuf,
    pub env_path: PathBuf,
    pub is_default: bool,
}

impl HermesHome {
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "dir": self.dir.to_string_lossy().to_string(),
            "isDefault": self.is_default,
        })
    }
}

pub fn hermes_root() -> PathBuf {
    home_dir().join(".hermes")
}

pub fn profiles_root() -> PathBuf {
    hermes_root().join("profiles")
}

pub fn is_valid_profile_name(name: &str) -> bool {
    PROFILE_NAME_RE.is_match(name) && !name.eq_ignore_ascii_case("default")
}

/// Resolve a profile name to its home directory.
/// - empty / "default" → the install root
/// - otherwise → <root>/profiles/<name>, with strict name validation + containment check
/// Returns Err(msg) on an invalid name so the route can answer 400 without touching the fs.
pub fn resolve_hermes_home(profile: Option<&str>) -> Result<HermesHome, String> {
    let base = hermes_root();
    let name = profile.unwrap_or_default().trim();
    if name.is_empty() || name == "default" {
        return Ok(HermesHome {
            name: "default".to_string(),
            config_path: base.join("config.yaml"),
            env_path: base.join(".env"),
            dir: base,
            is_default: true,
        });
    }
    if !is_valid_profile_name(name) {
        return Err(
            "Invalid Hermes profile name (letters, digits, - and _ only, max 64 chars)".to_string(),
        );
    }
    let profiles_root = profiles_root();
    let dir = profiles_root.join(name);
    // Containment check: the joined path must stay inside profiles/ (defence in depth —
    // the name regex already excludes separators, but never trust it alone).
    let dir_str = dir.to_string_lossy().to_string();
    let root_str = profiles_root.to_string_lossy().to_string();
    if dir != profiles_root
        && !dir_str.starts_with(&format!("{root_str}{}", std::path::MAIN_SEPARATOR))
    {
        return Err("Invalid Hermes profile path".to_string());
    }
    Ok(HermesHome {
        name: name.to_string(),
        config_path: dir.join("config.yaml"),
        env_path: dir.join(".env"),
        dir,
        is_default: false,
    })
}

pub async fn dir_exists(dir: &std::path::Path) -> bool {
    fs::metadata(dir).await.map(|m| m.is_dir()).unwrap_or(false)
}

/// Recognised as a profile only when one identity file is present (Hermes ignores bare dirs).
async fn is_profile_home(dir: &std::path::Path) -> bool {
    for file in IDENTITY_FILES {
        if fs::metadata(dir.join(file)).await.is_ok() {
            return true;
        }
    }
    false
}

async fn read_text_file(path: &std::path::Path) -> anyhow::Result<String> {
    match fs::read_to_string(path).await {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

/// `display_name` in profile.yaml — presentation only, the canonical id stays the dir name.
fn parse_display_name(yaml: &str) -> Option<String> {
    static RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"(?m)^display_name:[ \t]*["']?([^"'\r\n]+)["']?"#).unwrap());
    RE.captures(yaml)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
}

async fn build_entry(name: &str, dir: PathBuf, is_default: bool) -> anyhow::Result<Value> {
    let config_path = dir.join("config.yaml");
    let yaml = read_text_file(&config_path).await?;
    let model = hermes_yaml::parse_model_block(&yaml);
    let delegation = hermes_yaml::parse_delegation_block(&yaml);
    let auxiliary = hermes_yaml::parse_aux_roles(&yaml);
    let profile_yaml = read_text_file(&dir.join("profile.yaml")).await?;
    let display_name = parse_display_name(&profile_yaml);

    let has_router = hermes_yaml::has_openproxy_config(model.as_ref())
        || hermes_yaml::has_openproxy_config(delegation.as_ref())
        || auxiliary
            .values()
            .any(|cfg| hermes_yaml::has_openproxy_config(Some(cfg)));

    Ok(json!({
        "name": name,
        "dir": dir.to_string_lossy().to_string(),
        "isDefault": is_default,
        "displayName": display_name,
        "command": if is_default { "hermes".to_string() } else { format!("hermes -p {name}") },
        "alias": if is_default { Value::Null } else { json!(name) },
        "model": model.as_ref().and_then(|m| m.get("default")).cloned().unwrap_or(Value::Null),
        "baseUrl": model.as_ref().and_then(|m| m.get("base_url")).cloned().unwrap_or(Value::Null),
        "hasOpenProxy": has_router,
    }))
}

/// List every Hermes profile on this machine: the default home plus each recognised
/// directory under profiles/. Never throws for a missing install — returns just [default].
pub async fn list_profiles() -> anyhow::Result<Vec<Value>> {
    let base = hermes_root();
    let mut entries = vec![build_entry("default", base.clone(), true).await?];

    let mut names: Vec<String> = Vec::new();
    match fs::read_dir(profiles_root()).await {
        Ok(mut dirents) => {
            while let Ok(Some(entry)) = dirents.next_entry().await {
                let Ok(file_type) = entry.file_type().await else {
                    continue;
                };
                if !file_type.is_dir() {
                    continue;
                }
                let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
                    continue;
                };
                if is_valid_profile_name(&name) {
                    names.push(name);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(error) => return Err(error.into()),
    }
    names.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));

    for name in names {
        let dir = base.join("profiles").join(&name);
        if !is_profile_home(&dir).await {
            continue; // bare dir — Hermes ignores it too
        }
        entries.push(build_entry(&name, dir, false).await?);
    }
    Ok(entries)
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}
