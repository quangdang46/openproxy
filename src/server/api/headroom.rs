//! Headroom proxy management API
//!
//! Provides endpoints for managing the Headroom compression proxy:
//! - `GET /api/headroom/status`              — check whether the Headroom proxy is reachable
//! - `POST /api/headroom/start`              — attempt to start the local Headroom proxy
//! - `POST /api/headroom/stop`               — attempt to stop the local Headroom proxy
//! - `POST /api/headroom/restart`            — restart managed proxy with current extras flags
//! - `GET /api/headroom/extras`              — query installed compression extras
//! - `POST /api/headroom/extras`             — install compression extras
//! - `DELETE /api/headroom/extras`           — uninstall compression extras
//! - `* /api/headroom/proxy/{*path}`         — reverse-proxy catch-all to configured Headroom URL
//!
//! The Headroom URL and extras flags are read from settings, so everything is
//! configurable at runtime without a server restart.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Mutex;

use axum::body::Body;
use axum::extract::{Path, Query, RawQuery, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::api::require_dashboard_or_management_api_key;
use crate::server::state::AppState;

// ── Headroom extras constants ────────────────────────────────────────────

/// Recognized compression extras (closed set — not arbitrary strings).
const HEADROOM_EXTRAS: &[&str] = &["code", "ml"];

/// Marker packages that each extra pulls in.
const EXTRA_MARKERS: &[(&str, &[&str])] = &[
    ("code", &["tree-sitter", "tree-sitter-language-pack"]),
    ("ml", &["torch", "huggingface-hub"]),
];

/// Python candidates in priority order.
const PYTHON_CANDIDATES: &[&str] = &[
    "python3.13",
    "python3.12",
    "python3.11",
    "python3.10",
    "python3",
    "python",
];

/// Extra bin directories often missing from PATH when run as a service.
const EXTRA_BINS: &[&str] = &[
    "/usr/local/bin",
    "/opt/homebrew/bin",
    "/Library/Frameworks/Python.framework/Versions/3.13/bin",
    "/Library/Frameworks/Python.framework/Versions/3.12/bin",
    "/Library/Frameworks/Python.framework/Versions/3.11/bin",
    "/Library/Frameworks/Python.framework/Versions/3.10/bin",
];

// ── Managed PID tracking (best-effort) ───────────────────────────────────

static MANAGED_PID: Lazy<Mutex<Option<u32>>> = Lazy::new(|| Mutex::new(None));

fn set_managed_pid(pid: Option<u32>) {
    if let Ok(mut guard) = MANAGED_PID.lock() {
        *guard = pid;
    }
}

fn get_managed_pid() -> Option<u32> {
    MANAGED_PID.lock().ok().and_then(|g| *g)
}

fn is_pid_alive(pid: u32) -> bool {
    // Sending signal 0 checks whether the process exists (Unix).
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        // Best-effort on non-Unix: try `tasklist` / assume alive if we still track it.
        let _ = pid;
        true
    }
}

fn clear_dead_pid() {
    let pid = get_managed_pid();
    if let Some(p) = pid {
        if !is_pid_alive(p) {
            set_managed_pid(None);
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Find a Python interpreter >= 3.10 by probing candidates.
fn find_python() -> Option<String> {
    // Try HEADROOM_PYTHON env var first
    if let Ok(py) = std::env::var("HEADROOM_PYTHON") {
        if check_python_version(&py) {
            return Some(py);
        }
    }

    for candidate in PYTHON_CANDIDATES {
        // Prefer full paths from EXTRA_BINS, then bare names on PATH.
        for dir in EXTRA_BINS {
            let full_path = format!("{}/{}", dir, candidate);
            if check_python_version(&full_path) {
                return Some(full_path);
            }
        }
        if check_python_version(candidate) {
            return Some(candidate.to_string());
        }
    }

    None
}

fn check_python_version(python: &str) -> bool {
    let output = match std::process::Command::new(python).arg("--version").output() {
        Ok(o) if o.status.success() => o,
        // Some Pythons write version to stderr
        Ok(o) => o,
        Err(_) => return false,
    };
    let ver = {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stdout.trim().is_empty() {
            stdout.to_string()
        } else {
            stderr.to_string()
        }
    };
    // Parse "Python X.Y..."
    let re = match regex::Regex::new(r"(\d+)\.(\d+)") {
        Ok(r) => r,
        Err(_) => return false,
    };
    let caps = match re.captures(&ver) {
        Some(c) => c,
        None => return false,
    };
    let major: u32 = match caps.get(1).and_then(|m| m.as_str().parse().ok()) {
        Some(v) => v,
        None => return false,
    };
    let minor: u32 = match caps.get(2).and_then(|m| m.as_str().parse().ok()) {
        Some(v) => v,
        None => return false,
    };
    major > 3 || (major == 3 && minor >= 10)
}

fn find_headroom_binary() -> Option<String> {
    let current_path = std::env::var_os("PATH").unwrap_or_default();
    let mut parts: Vec<std::path::PathBuf> =
        EXTRA_BINS.iter().map(std::path::PathBuf::from).collect();
    parts.extend(std::env::split_paths(&current_path));
    let extended_path = std::env::join_paths(&parts).ok()?;

    let output = std::process::Command::new("which")
        .arg("headroom")
        .env("PATH", &extended_path)
        .output()
        .ok()?;
    if output.status.success() {
        String::from_utf8(output.stdout)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        None
    }
}

/// Check if a headroom URL is loopback/localhost.
fn is_loopback_url(url: &str) -> bool {
    if let Ok(parsed) = url::Url::parse(url) {
        let host = parsed.host_str().unwrap_or("");
        host == "localhost"
            || host == "127.0.0.1"
            || host == "::1"
            || host == "[::1]"
            || host == "0.0.0.0"
    } else {
        url.contains("localhost") || url.contains("127.0.0.1") || url.contains("::1")
    }
}

/// Parse port from a headroom URL, defaulting to 8787.
fn parse_port(url: &str) -> u16 {
    if let Ok(parsed) = url::Url::parse(url) {
        parsed.port().unwrap_or(8787)
    } else {
        8787
    }
}

/// Build proxy CLI args from extras flags.
fn extras_proxy_args(code_aware: bool, kompress: bool) -> Vec<String> {
    let mut args = Vec::new();
    if code_aware {
        args.push("--code-aware".to_string());
    }
    if !kompress {
        args.push("--disable-kompress".to_string());
    }
    args
}

/// Read the install/uninstall log file from DATA_DIR.
fn read_install_log() -> String {
    let log_path = headroom_dir().join("install.log");
    match std::fs::read_to_string(&log_path) {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
            let start = lines.len().saturating_sub(15);
            lines[start..].join("\n")
        }
        Err(_) => String::new(),
    }
}

fn headroom_dir() -> std::path::PathBuf {
    let data_dir = std::env::var_os("DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            home.join(".openproxy")
        });
    data_dir.join("headroom")
}

/// Truncate the install log so progress polling only shows the current op.
fn reset_install_log() {
    let dir = headroom_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("install.log"), b"");
}

/// Write a line to the install log.
fn append_install_log(line: &str) {
    let dir = headroom_dir();
    let _ = std::fs::create_dir_all(&dir);
    let log_path = dir.join("install.log");
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = writeln!(f, "{}", line);
    }
}

/// Detect installed headroom extras via `pip list --format=json`.
fn get_installed_extras(python: &str) -> Value {
    let empty = || {
        json!({
            "installed": false,
            "version": null,
            "extras": { "code": false, "ml": false },
        })
    };
    let output = match std::process::Command::new(python)
        .args([
            "-m",
            "pip",
            "list",
            "--format=json",
            "--disable-pip-version-check",
        ])
        .output()
    {
        Ok(out) if out.status.success() => out,
        _ => return empty(),
    };
    let packages: Vec<Value> = match serde_json::from_slice(&output.stdout) {
        Ok(p) => p,
        Err(_) => return empty(),
    };
    let names: HashSet<String> = packages
        .iter()
        .filter_map(|p| p.get("name").and_then(|n| n.as_str()))
        .map(|n| n.to_lowercase())
        .collect();
    let installed = names.contains("headroom-ai");
    if !installed {
        return empty();
    }
    let version = packages
        .iter()
        .find(|p| {
            p.get("name")
                .and_then(|n| n.as_str())
                .map(|n| n.eq_ignore_ascii_case("headroom-ai"))
                .unwrap_or(false)
        })
        .and_then(|p| p.get("version").and_then(|v| v.as_str()))
        .map(|s| s.to_string());
    let mut extras = BTreeMap::new();
    for &(extra, markers) in EXTRA_MARKERS {
        extras.insert(
            extra.to_string(),
            Value::Bool(markers.iter().any(|m| names.contains(*m))),
        );
    }
    json!({
        "installed": true,
        "version": version,
        "extras": extras,
    })
}

// ── Query params ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ExtrasQuery {
    log: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ExtrasBody {
    extras: Option<Vec<String>>,
}

// ── Handlers ─────────────────────────────────────────────────────────────

/// GET /api/headroom/status
pub async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }

    let snapshot = state.db.snapshot();
    let url = snapshot.settings.headroom_url.clone();

    if url.is_empty() {
        return Json(json!({
            "installed": false,
            "running": false,
            "python": null,
            "loading": false,
            "localUrl": false,
            "canStart": false,
            "managedPid": false,
        }))
        .into_response();
    }

    // Probe the headroom health endpoint
    let health_url = format!("{}/health", url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let running = match client.get(&health_url).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    };

    let local_url = is_loopback_url(&url);

    // Check for managed PID
    clear_dead_pid();
    let managed_pid = get_managed_pid().is_some();

    Json(json!({
        "installed": running || local_url,
        "running": running,
        "python": std::env::var("HEADROOM_PYTHON").ok(),
        "loading": false,
        "localUrl": local_url,
        "canStart": local_url && !running,
        "managedPid": managed_pid,
    }))
    .into_response()
}

/// POST /api/headroom/start
pub async fn start(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }

    let snapshot = state.db.snapshot();
    let url = snapshot.settings.headroom_url.clone();

    if !is_loopback_url(&url) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Headroom is configured for a remote URL; start it externally."
            })),
        )
            .into_response();
    }

    // Check for Python
    let python_path = match find_python() {
        Some(p) if !p.is_empty() => p,
        _ => {
            return (
                StatusCode::PRECONDITION_FAILED,
                Json(json!({
                    "error": "Python >= 3.10 is required to start Headroom locally."
                })),
            )
                .into_response();
        }
    };

    // Check for headroom binary
    if find_headroom_binary().is_none() {
        return (
            StatusCode::PRECONDITION_FAILED,
            Json(json!({
                "error": "Headroom CLI not found. Run: pip install \"headroom-ai[proxy]\""
            })),
        )
            .into_response();
    }

    // Kill existing managed process if alive
    clear_dead_pid();
    if let Some(old_pid) = get_managed_pid() {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &old_pid.to_string()])
            .output();
        set_managed_pid(None);
    }

    let port = parse_port(&url);
    let code_aware = snapshot.settings.headroom_code_aware;
    let kompress = snapshot.settings.headroom_kompress;

    let args = {
        let mut a = vec![
            "-m".to_string(),
            "headroom".to_string(),
            "proxy".to_string(),
            "--port".to_string(),
            port.to_string(),
        ];
        a.extend(extras_proxy_args(code_aware, kompress));
        a
    };

    let mut child = match std::process::Command::new(&python_path)
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to spawn Headroom proxy: {e}")})),
            )
                .into_response();
        }
    };

    let pid = child.id();
    set_managed_pid(Some(pid));

    // Detach — we don't await the child.
    tokio::task::spawn_blocking(move || {
        let _ = child.wait();
        // On exit, clear the managed PID if it's still ours.
        let current = get_managed_pid();
        if current == Some(pid) {
            set_managed_pid(None);
        }
    });

    Json(json!({
        "started": true,
        "pid": pid,
        "message": "Headroom proxy starting…"
    }))
    .into_response()
}

/// POST /api/headroom/stop
pub async fn stop(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }

    let snapshot = state.db.snapshot();
    let url = snapshot.settings.headroom_url.clone();

    // Try killing managed PID first
    clear_dead_pid();
    if let Some(pid) = get_managed_pid() {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .output();
        set_managed_pid(None);
        // Best-effort graceful wait, then force
        for _ in 0..30 {
            if !is_pid_alive(pid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if is_pid_alive(pid) {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .output();
        }
        return Json(json!({
            "stopped": true,
            "message": "Managed Headroom proxy stopped."
        }))
        .into_response();
    }

    if !url.is_empty() {
        // Try the shutdown endpoint
        let shutdown_url = format!("{}/shutdown", url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        let _ = client.post(&shutdown_url).send().await;

        // Also try pkill
        if is_loopback_url(&url) {
            let _ = std::process::Command::new("pkill")
                .arg("-f")
                .arg("headroom")
                .output();
        }
    }

    Json(json!({
        "stopped": true,
        "message": "Headroom proxy stopped."
    }))
    .into_response()
}

/// POST /api/headroom/restart
pub async fn restart(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }

    let snapshot = state.db.snapshot();
    let url = snapshot.settings.headroom_url.clone();

    if !is_loopback_url(&url) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "External Headroom proxies must be restarted outside OpenProxy.",
                "code": "EXTERNAL_PROXY"
            })),
        )
            .into_response();
    }

    // Stop managed proxy if running
    clear_dead_pid();
    if let Some(pid) = get_managed_pid() {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .output();
        for _ in 0..30 {
            if !is_pid_alive(pid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if is_pid_alive(pid) {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .output();
        }
        set_managed_pid(None);
    }

    // Start again with current settings
    let python_path = match find_python() {
        Some(p) => p,
        None => {
            return (
                StatusCode::PRECONDITION_FAILED,
                Json(json!({
                    "error": "Python >= 3.10 not found",
                    "code": "NO_PYTHON"
                })),
            )
                .into_response();
        }
    };

    if find_headroom_binary().is_none() {
        return (
            StatusCode::PRECONDITION_FAILED,
            Json(json!({
                "error": "Headroom CLI not installed",
                "code": "NOT_INSTALLED"
            })),
        )
            .into_response();
    }

    let port = parse_port(&url);
    let code_aware = snapshot.settings.headroom_code_aware;
    let kompress = snapshot.settings.headroom_kompress;

    let args = {
        let mut a = vec![
            "-m".to_string(),
            "headroom".to_string(),
            "proxy".to_string(),
            "--port".to_string(),
            port.to_string(),
        ];
        a.extend(extras_proxy_args(code_aware, kompress));
        a
    };

    let mut child = match std::process::Command::new(&python_path)
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to spawn Headroom proxy: {e}"), "code": "SPAWN_FAILED"})),
            )
                .into_response();
        }
    };

    let pid = child.id();
    set_managed_pid(Some(pid));

    tokio::task::spawn_blocking(move || {
        let _ = child.wait();
        let current = get_managed_pid();
        if current == Some(pid) {
            set_managed_pid(None);
        }
    });

    Json(json!({
        "success": true,
        "pid": pid,
        "message": "Headroom proxy restarted with current extras flags."
    }))
    .into_response()
}

/// GET /api/headroom/extras
///
/// Returns the available extras, installed status, and optionally the install log.
/// Pass `?log=1` to get the live install/uninstall log tail.
pub async fn extras_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ExtrasQuery>,
) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }

    if params.log.as_deref() == Some("1") {
        return Json(json!({ "log": read_install_log() })).into_response();
    }

    let python = find_python();
    let extras_status = python
        .as_ref()
        .map(|py| get_installed_extras(py))
        .unwrap_or_else(|| {
            json!({
                "installed": false,
                "version": null,
                "extras": {"code": false, "ml": false},
            })
        });

    let response = json!({
        "available": HEADROOM_EXTRAS,
        "installed": extras_status["installed"],
        "version": extras_status["version"],
        "extras": extras_status["extras"],
    });

    Json(response).into_response()
}

/// POST /api/headroom/extras
///
/// Install (or reinstall) the requested compression extras.
/// Body: `{ "extras": ["code", "ml"] }`
pub async fn extras_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ExtrasBody>,
) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }

    let requested: Vec<&str> = body
        .extras
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|e| HEADROOM_EXTRAS.contains(&e.as_str()))
        .map(|s| s.as_str())
        .collect();

    let python = match find_python() {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Python >= 3.10 not found", "code": "NO_PYTHON"})),
            )
                .into_response();
        }
    };

    if find_headroom_binary().is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "headroom-ai not installed (run `pip install \"headroom-ai[proxy]\"` first)",
                "code": "NOT_INSTALLED"
            })),
        )
            .into_response();
    }

    let extras_list = {
        let mut all = vec!["proxy".to_string()];
        all.extend(requested.iter().map(|e| e.to_string()));
        all.join(",")
    };
    let spec = format!("headroom-ai[{}]", extras_list);

    reset_install_log();
    append_install_log(&format!("Installing: {}", spec));

    let child = match std::process::Command::new(&python)
        .args(["-m", "pip", "install", "--upgrade", &spec])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to run pip install: {e}"), "code": "SPAWN_FAILED"})),
            )
                .into_response();
        }
    };

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    json!({"error": format!("pip install failed: {e}"), "code": "INSTALL_FAILED"}),
                ),
            )
                .into_response();
        }
    };

    let stdout_log = String::from_utf8_lossy(&output.stdout);
    let stderr_log = String::from_utf8_lossy(&output.stderr);
    for line in stdout_log.lines() {
        append_install_log(line);
    }
    for line in stderr_log.lines() {
        append_install_log(line);
    }

    if !output.status.success() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": format!("pip install exited with code={:?}", output.status.code()),
                "code": "INSTALL_FAILED"
            })),
        )
            .into_response();
    }

    let status = get_installed_extras(&python);
    append_install_log("Install complete");

    // Match 9router shape: top-level extras is the installed map (status overwrites list).
    Json(json!({
        "success": true,
        "code": output.status.code(),
        "spec": spec,
        "installed": status["installed"],
        "version": status["version"],
        "extras": status["extras"],
    }))
    .into_response()
}

/// DELETE /api/headroom/extras
///
/// Uninstall marker packages for the requested extras.
/// Body: `{ "extras": ["code"] }`
pub async fn extras_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ExtrasBody>,
) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }

    let requested: Vec<&str> = body
        .extras
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|e| HEADROOM_EXTRAS.contains(&e.as_str()))
        .map(|s| s.as_str())
        .collect();

    if requested.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "No valid extras to remove", "code": "INVALID_EXTRAS"})),
        )
            .into_response();
    }

    let python = match find_python() {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Python >= 3.10 not found", "code": "NO_PYTHON"})),
            )
                .into_response();
        }
    };

    let pkgs: Vec<String> = EXTRA_MARKERS
        .iter()
        .filter(|(extra, _)| requested.contains(extra))
        .flat_map(|(_, markers)| markers.iter().map(|m| m.to_string()))
        .collect();

    let unique_pkgs: BTreeSet<String> = pkgs.into_iter().collect();
    if unique_pkgs.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "No packages to remove for given extras", "code": "INVALID_EXTRAS"})),
        )
            .into_response();
    }

    let pkg_list: Vec<String> = unique_pkgs.iter().cloned().collect();
    reset_install_log();
    append_install_log(&format!("Uninstalling: {}", pkg_list.join(" ")));

    let mut args: Vec<String> = vec!["-m".into(), "pip".into(), "uninstall".into(), "-y".into()];
    args.extend(pkg_list.iter().cloned());

    let child = match std::process::Command::new(&python)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to run pip uninstall: {e}"), "code": "SPAWN_FAILED"})),
            )
                .into_response();
        }
    };

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("pip uninstall failed: {e}"), "code": "UNINSTALL_FAILED"})),
            )
                .into_response();
        }
    };

    let stdout_log = String::from_utf8_lossy(&output.stdout);
    let stderr_log = String::from_utf8_lossy(&output.stderr);
    for line in stdout_log.lines() {
        append_install_log(line);
    }
    for line in stderr_log.lines() {
        append_install_log(line);
    }

    if !output.status.success() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": format!("pip uninstall exited with code={:?}", output.status.code()),
                "code": "UNINSTALL_FAILED"
            })),
        )
            .into_response();
    }

    let status = get_installed_extras(&python);
    append_install_log("Uninstall complete");

    Json(json!({
        "success": true,
        "code": output.status.code(),
        "removed": pkg_list,
        "installed": status["installed"],
        "version": status["version"],
        "extras": status["extras"],
    }))
    .into_response()
}


// ── Headroom reverse proxy (dashboard access) ────────────────────────

/// Hop-by-hop headers that must not be forwarded (RFC 9110 §7.6.1).
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// GET/POST/PUT/PATCH/DELETE/HEAD/OPTIONS `/api/headroom/proxy/{*path}`
///
/// Minimal reverse proxy to the configured Headroom URL + remaining path.
/// Strips hop-by-hop headers; returns the upstream response as-is
/// (no HTML rewrite — dashboard assets use relative paths when proxied).
pub async fn proxy_handler(
    State(state): State<AppState>,
    Path(path): Path<Vec<String>>,
    headers: HeaderMap,
    method: Method,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Response {
    if let Err(resp) = require_dashboard_or_management_api_key(&headers, &state) {
        return resp;
    }

    let snapshot = state.db.snapshot();
    let base = snapshot.settings.headroom_url.trim();
    let base = if base.is_empty() {
        "http://localhost:8787"
    } else {
        base.trim_end_matches('/')
    };

    let mut target = match url::Url::parse(base) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("invalid headroom URL: {e}") })),
            )
                .into_response();
        }
    };
    if target.scheme() != "http" && target.scheme() != "https" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Headroom URL must use http or https" })),
        )
            .into_response();
    }

    let path_joined = path.join("/");
    target.set_path(&format!("/{path_joined}"));
    if let Some(q) = query.as_deref().filter(|q| !q.is_empty()) {
        target.set_query(Some(q));
    }

    let is_loopback = is_loopback_url(target.as_str());
    let has_body = !matches!(method, Method::GET | Method::HEAD);

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("failed to build HTTP client: {e}") })),
            )
                .into_response();
        }
    };

    let reqwest_method = match method {
        Method::GET => reqwest::Method::GET,
        Method::POST => reqwest::Method::POST,
        Method::PUT => reqwest::Method::PUT,
        Method::PATCH => reqwest::Method::PATCH,
        Method::DELETE => reqwest::Method::DELETE,
        Method::HEAD => reqwest::Method::HEAD,
        Method::OPTIONS => reqwest::Method::OPTIONS,
        other => other,
    };

    let mut req_builder = client.request(reqwest_method, target);
    if has_body {
        req_builder = req_builder.body(body);
    }

    for (name, value) in headers.iter() {
        let name_lower = name.as_str().to_ascii_lowercase();
        if HOP_BY_HOP.contains(&name_lower.as_str()) || name_lower == "host" {
            continue;
        }
        // Never leak viewer credentials to a non-loopback Headroom host.
        if !is_loopback && (name_lower == "cookie" || name_lower == "authorization") {
            continue;
        }
        if let Ok(v) = value.to_str() {
            req_builder = req_builder.header(name.as_str(), v);
        }
    }

    match req_builder.send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::BAD_GATEWAY);
            let resp_headers = resp.headers().clone();
            let resp_body = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(json!({ "error": format!("upstream read error: {e}") })),
                    )
                        .into_response();
                }
            };

            let mut out = Response::builder().status(status);
            for (name, value) in resp_headers.iter() {
                let name_lower = name.as_str().to_ascii_lowercase();
                if HOP_BY_HOP.contains(&name_lower.as_str()) {
                    continue;
                }
                out = out.header(name.as_str(), value);
            }
            out.body(Body::from(resp_body)).unwrap_or_else(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "error": format!("response build error: {e}") })),
                )
                    .into_response()
            })
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("proxy error: {e}") })),
        )
            .into_response(),
    }
}

