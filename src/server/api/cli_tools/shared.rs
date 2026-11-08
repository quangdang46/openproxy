//! Shared helpers for the newer "generic" CLI-tool settings routes
//! (pi, omp, crush, forge, smelt, codewhale) ported from 9router v0.5.111+.
//!
//! These routes all follow the same shape: detect the tool binary or its
//! config file, GET returns `{installed, config, has9Router, configPath}`,
//! POST merges an OpenAI-compatible provider block into the tool's config,
//! DELETE removes it again.

use std::env;
use std::path::PathBuf;

use tokio::process::Command;

use crate::server::state::AppState;

/// `which <program>` / `where <program>` — true when the binary is on PATH.
pub(super) async fn command_exists(program: &str) -> bool {
    let finder = if cfg!(windows) { "where" } else { "which" };
    Command::new(finder)
        .arg(program)
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

pub(super) fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// XDG config home, falling back to `~/.config` (mirrors upstream's
/// `process.env.XDG_CONFIG_HOME || ~/.config`).
pub(super) fn config_home_dir() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"))
}

/// Port of upstream `resolveCliApiKey.js`: use the caller-supplied key when
/// non-empty (and not the historical `sk_9router` placeholder), otherwise the
/// first active API key in the database, otherwise an empty string. The
/// `sk_9router` placeholder is NEVER written — it was never a real key (#4399).
pub(super) fn resolve_cli_api_key(state: &AppState, caller_key: Option<&str>) -> String {
    if let Some(key) = caller_key
        .map(str::trim)
        .filter(|k| !k.is_empty() && *k != "sk_9router")
    {
        return key.to_string();
    }
    state
        .db
        .snapshot()
        .api_keys
        .iter()
        .find(|k| k.is_active())
        .map(|k| k.key.clone())
        .unwrap_or_default()
}

/// `baseUrl` normalization shared by every route: ensure a trailing `/v1`.
pub(super) fn normalize_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1")
    }
}
