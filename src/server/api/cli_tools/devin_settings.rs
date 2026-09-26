//! GET /api/cli-tools/devin-settings
//!
//! Install detection only — the Devin CLI keeps its own auth (`devin auth login`),
//! so unlike the sibling `*-settings` routes there is no config to read or write.
//!
//! Port of 9router `src/app/api/cli-tools/devin-settings/route.js`. The candidate
//! list is `devin_bin_candidates()` from the `devin-cli` executor rather than a
//! literal copy: a second copy is how detection drifts from what we actually spawn.

use std::path::Path;
use std::time::Duration;

use axum::{
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use tokio::process::Command;

use crate::core::executor::devin_bin_candidates;
use crate::server::state::AppState;

const INSTALL_URL: &str = "https://cli.devin.ai";

/// `devin --version` is a foreground probe on the request path, so cap it: a
/// hung CLI must not hold the dashboard's status request open.
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/cli-tools/devin-settings", get(get_devin_settings))
}

async fn get_devin_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match locate_devin().await {
        Some(path) => {
            let version = read_devin_version(&path).await;
            Json(json!({
                "installed": true,
                "path": path,
                "version": version,
                "message": "Devin CLI detected. Make sure `devin auth login` has been run.",
            }))
            .into_response()
        }
        None => Json(json!({
            "installed": false,
            "path": Value::Null,
            "version": Value::Null,
            "message": format!(
                "Devin CLI is not installed. Install it from {INSTALL_URL} and run `devin auth login`."
            ),
            "installUrl": INSTALL_URL,
        }))
        .into_response(),
    }
}

/// Resolve the binary the way `devin_cli` will, so a "detected" status cannot be
/// reported for a path the executor would fail to spawn.
///
/// 9router probes `which devin` first; we follow the executor's order instead
/// (env override → installer paths → PATH) because the reported `path` has to be
/// the one that wins at runtime.
async fn locate_devin() -> Option<String> {
    if let Some(path) = devin_bin_env_override() {
        if Path::new(&path).exists() {
            return Some(path);
        }
    }
    for candidate in devin_bin_candidates() {
        if Path::new(&candidate).exists() {
            return Some(candidate);
        }
    }
    which_devin().await
}

fn devin_bin_env_override() -> Option<String> {
    let env_bin = std::env::var("CLI_DEVIN_BIN").ok()?;
    let trimmed = env_bin.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

async fn which_devin() -> Option<String> {
    let finder = if cfg!(windows) { "where" } else { "which" };
    let output = Command::new(finder).arg("devin").output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    // `where` prints one path per line; the first hit is the one PATH resolves.
    // A success with no path is still a hit — report the bare name, which is
    // exactly what the executor falls back to.
    let found = String::from_utf8_lossy(&output.stdout);
    let first = found.lines().map(str::trim).find(|line| !line.is_empty());
    Some(first.unwrap_or("devin").to_string())
}

async fn read_devin_version(bin: &str) -> Option<String> {
    let output = tokio::time::timeout(VERSION_TIMEOUT, Command::new(bin).arg("--version").output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout);
    let first_line = version
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    Some(first_line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `std::env::set_var` is process-wide, so tests that touch it must not
    /// interleave with other threads' env access.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        old_value: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old_value = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, old_value }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.old_value.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn candidate_list_covers_the_documented_installer_layouts() {
        let candidates = devin_bin_candidates();
        assert!(candidates
            .iter()
            .any(|c| c.ends_with("/.local/share/devin/bin/devin")));
        assert!(candidates.iter().any(|c| c.ends_with("/.devin/bin/devin")));
        assert!(candidates.iter().any(|c| c.ends_with("/.local/bin/devin")));
        assert!(candidates.iter().any(|c| c == "/opt/homebrew/bin/devin"));
        assert!(candidates.iter().any(|c| c == "/usr/local/bin/devin"));
        assert!(candidates.iter().any(|c| c == "/usr/bin/devin"));
    }

    #[test]
    fn env_override_is_trimmed_and_ignored_when_blank() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let _set = EnvVarGuard::set("CLI_DEVIN_BIN", "  /opt/custom/devin  ");
        assert_eq!(
            devin_bin_env_override().as_deref(),
            Some("/opt/custom/devin")
        );

        let _blank = EnvVarGuard::set("CLI_DEVIN_BIN", "   ");
        assert_eq!(devin_bin_env_override(), None);
    }
}
