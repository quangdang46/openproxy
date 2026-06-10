use std::env;
use std::path::{Path, PathBuf};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use tokio::process::Command;

use crate::server::state::AppState;

/// Cursor IDE is a **guide-only** tool — no file is written.
///
/// The user must manually configure Cursor's built-in OpenAI provider:
/// 1. Open Cursor Settings → Models
/// 2. Set OpenAI API Base URL to the proxy endpoint
/// 3. Set API Key
/// 4. Add custom model(s)
///
/// **IMPORTANT**: Cursor requires a Pro subscription and a tunnel / cloud-exposed
/// endpoint. Localhost (127.0.0.1, ::1, 0.0.0.0) **will not work** because Cursor
/// runs its AI requests in a subprocess that cannot reach the host's loopback on
/// many platforms. See the OpenProxy docs for recommended tunnels (bore, ngrok,
/// Cloudflare Tunnel, etc.).

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/cli-tools/cursor-settings",
        get(get_cursor_settings)
            .post(save_cursor_settings)
            .delete(delete_cursor_settings),
    )
}

async fn get_cursor_settings(State(state): State<AppState>, _headers: HeaderMap) -> Response {
    let installed = check_cursor_installed().await;

    let guide_steps = json!([
        {
            "step": 1,
            "action": "Open Cursor Settings",
            "detail": "Open Cursor IDE → Cmd/Ctrl+Shift+P → 'Preferences: Open Settings (UI)' → navigate to 'Models' section."
        },
        {
            "step": 2,
            "action": "Set OpenAI API Base URL",
            "detail": "Under 'OpenAI API Base URL', enter: {{baseUrl}}"
        },
        {
            "step": 3,
            "action": "Set API Key",
            "detail": "Under 'OpenAI API Key', enter: {{apiKey}}"
        },
        {
            "step": 4,
            "action": "Add Custom Model",
            "detail": "Click 'Add Custom Model' and enter the model ID (e.g. {{model}}). You can add multiple models."
        }
    ]);

    Json(json!({
        "installed": installed,
        "configType": "guide",
        "guideSteps": guide_steps,
        "settingsPath": Value::Null,
        "hasOpenProxy": false,
        "warning": "Cursor requires a Pro subscription. The endpoint must be a tunnel / cloud URL (e.g. https://your-tunnel.example.com) — localhost will NOT work because Cursor runs AI requests in a subprocess that cannot reach the host loopback. Users on Cursor Free plan cannot use custom endpoints.",
        "settings": Value::Null,
    }))
    .into_response()
}

async fn save_cursor_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let guide_steps = json!([
        {
            "step": 1,
            "action": "Open Cursor Settings",
            "detail": "Open Cursor IDE → Cmd/Ctrl+Shift+P → 'Preferences: Open Settings (UI)' → navigate to 'Models' section."
        },
        {
            "step": 2,
            "action": "Set OpenAI API Base URL",
            "detail": "Under 'OpenAI API Base URL', enter: {{baseUrl}}"
        },
        {
            "step": 3,
            "action": "Set API Key",
            "detail": "Under 'OpenAI API Key', enter: {{apiKey}}"
        },
        {
            "step": 4,
            "action": "Add Custom Model",
            "detail": "Click 'Add Custom Model' and enter the model ID (e.g. {{model}}). You can add multiple models."
        }
    ]);

    Json(json!({
        "success": true,
        "configType": "guide",
        "message": "Cursor does not support automatic file configuration. Follow the guide steps below to configure manually.",
        "guideSteps": guide_steps,
        "note": "No file was written. Cursor settings are configured entirely through its UI.",
        "warning": "Cursor requires a Pro subscription. The endpoint must be a tunnel / cloud URL (e.g. https://your-tunnel.example.com) — localhost will NOT work.",
    }))
    .into_response()
}

async fn delete_cursor_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = super::super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    // No-op — we never write files for Cursor.
    Json(json!({
        "success": true,
        "message": "No settings file to reset. Cursor is guide-only."
    }))
    .into_response()
}

async fn check_cursor_installed() -> bool {
    let home = home_dir();

    // macOS
    if Path::new("/Applications/Cursor.app").exists() {
        return true;
    }

    // Linux AppImage / binary
    if command_exists("cursor", false).await {
        return true;
    }

    if home.join(".local/share/cursor").exists() {
        return true;
    }

    // Linux Snap
    if Path::new("/snap/bin/cursor").exists() {
        return true;
    }

    // Windows — check common paths
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        let cursor_exe = PathBuf::from(local_app_data)
            .join("Programs")
            .join("Cursor")
            .join("Cursor.exe");
        if cursor_exe.exists() {
            return true;
        }
    }

    // Windows via Scoop
    if let Some(user_profile) = env::var_os("USERPROFILE") {
        let scoop_path = PathBuf::from(user_profile)
            .join("scoop")
            .join("apps")
            .join("cursor")
            .join("current")
            .join("Cursor.exe");
        if scoop_path.exists() {
            return true;
        }
    }

    false
}

async fn command_exists(program: &str, _inject_windows_npm_path: bool) -> bool {
    let finder = if cfg!(windows) { "where" } else { "which" };
    let mut command = Command::new(finder);
    command.arg(program);

    command
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}
