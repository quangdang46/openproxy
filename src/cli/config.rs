//! CLI configuration and profile resolution.
//!
//! Most users never need this — by default the CLI uses `DATA_DIR` (or `~/.openproxy`)
//! and writes the local DB directly. Config profiles are only useful when:
//!
//! - You manage multiple OpenProxy instances at different `DATA_DIR`s.
//! - You remote-manage a server at another host (`--url https://...`).
//!
//! The config file lives at:
//! - Linux/macOS: `~/.config/openproxy/config.toml`
//! - Windows:     `%APPDATA%\openproxy\config.toml`
//!
//! Resolution precedence (highest first):
//! 1. Explicit CLI flags (`--data-dir`, `--url`, `--api-key`)
//! 2. `OPENPROXY_*` environment variables
//! 3. Selected profile (`--profile <name>` or `default_profile`)
//! 4. Built-in defaults

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// Resolved per-invocation configuration the CLI works against.
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub data_dir: PathBuf,
    /// `Some(url)` enables remote management mode (CLI talks HTTPS to a remote
    /// `openproxy` server). `None` means local DB-direct mode.
    pub remote_url: Option<String>,
    /// API key for the remote server (read once at resolve time so we never
    /// touch the env again later).
    pub api_key: Option<String>,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigFile {
    #[serde(default)]
    pub default_profile: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub data_dir: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// Name of the env var holding the API key (preferred over `api_key`).
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
}

/// CLI flag overrides supplied by clap.
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub data_dir: Option<PathBuf>,
    pub url: Option<String>,
    pub api_key: Option<String>,
    pub profile: Option<String>,
}

impl ResolvedConfig {
    pub fn resolve(overrides: CliOverrides) -> anyhow::Result<Self> {
        let file = load_config_file().unwrap_or_default();

        let profile_name = overrides
            .profile
            .clone()
            .or_else(|| std::env::var("OPENPROXY_PROFILE").ok())
            .or_else(|| file.default_profile.clone());

        let profile = profile_name
            .as_deref()
            .and_then(|name| file.profiles.get(name).cloned())
            .unwrap_or_default();

        let data_dir = overrides
            .data_dir
            .or_else(|| std::env::var_os("DATA_DIR").map(PathBuf::from))
            .or_else(|| std::env::var_os("OPENPROXY_DATA_DIR").map(PathBuf::from))
            .or_else(|| profile.data_dir.as_deref().map(PathBuf::from))
            .unwrap_or_else(default_data_dir);

        let remote_url = overrides
            .url
            .or_else(|| std::env::var("OPENPROXY_URL").ok())
            .or(profile.url);

        let api_key = overrides
            .api_key
            .or_else(|| std::env::var("OPENPROXY_API_KEY").ok())
            .or_else(|| {
                profile
                    .api_key_env
                    .as_deref()
                    .and_then(|name| std::env::var(name).ok())
            })
            .or(profile.api_key);

        Ok(Self {
            data_dir,
            remote_url,
            api_key,
            profile: profile_name,
        })
    }

    pub fn is_remote(&self) -> bool {
        self.remote_url.is_some()
    }
}

fn default_data_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".openproxy"))
        .unwrap_or_else(|| PathBuf::from(".openproxy"))
}

fn config_file_path() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("OPENPROXY_CONFIG") {
        return Some(PathBuf::from(custom));
    }
    let dirs = directories::ProjectDirs::from("", "", "openproxy")?;
    Some(dirs.config_dir().join("config.toml"))
}

fn load_config_file() -> anyhow::Result<ConfigFile> {
    let Some(path) = config_file_path() else {
        return Ok(ConfigFile::default());
    };
    if !path.exists() {
        return Ok(ConfigFile::default());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("read config file at {}", path.display()))?;
    let parsed: ConfigFile = toml::from_str(&text)
        .with_context(|| format!("parse config file at {}", path.display()))?;
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_env() {
        std::env::remove_var("DATA_DIR");
        std::env::remove_var("OPENPROXY_DATA_DIR");
        std::env::remove_var("OPENPROXY_URL");
        std::env::remove_var("OPENPROXY_API_KEY");
        std::env::remove_var("OPENPROXY_PROFILE");
        std::env::remove_var("OPENPROXY_CONFIG");
    }

    #[test]
    fn flags_override_env_and_defaults() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("DATA_DIR", "/tmp/from-env");
        let resolved = ResolvedConfig::resolve(CliOverrides {
            data_dir: Some(PathBuf::from("/tmp/from-flag")),
            url: Some("http://x".into()),
            api_key: Some("k".into()),
            profile: None,
        })
        .unwrap();
        assert_eq!(resolved.data_dir, PathBuf::from("/tmp/from-flag"));
        assert_eq!(resolved.remote_url.as_deref(), Some("http://x"));
        assert_eq!(resolved.api_key.as_deref(), Some("k"));
        assert!(resolved.is_remote());
        clear_env();
    }

    #[test]
    fn env_used_when_no_flag() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("DATA_DIR", "/tmp/env-only");
        let resolved = ResolvedConfig::resolve(CliOverrides::default()).unwrap();
        assert_eq!(resolved.data_dir, PathBuf::from("/tmp/env-only"));
        assert!(resolved.remote_url.is_none());
        clear_env();
    }

    #[test]
    fn default_data_dir_when_nothing_set() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        let resolved = ResolvedConfig::resolve(CliOverrides::default()).unwrap();
        assert!(resolved.data_dir.ends_with(".openproxy"));
        clear_env();
    }
}
