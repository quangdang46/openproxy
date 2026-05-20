//! HTTP routes for managing `db.json` snapshots (auto-backup, manual
//! snapshot, restore, prune, export, import). See `crate::db::backups`
//! for the underlying file management.

use axum::body::Body;
use axum::extract::{Multipart, Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use chrono::{SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::backups::{BackupManager, BackupReason};
use crate::server::state::AppState;
use crate::types::AppDb;

use super::require_dashboard_or_management_api_key;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/db-backups", get(list_handler))
        .route("/api/db-backups", put(create_handler))
        .route("/api/db-backups", delete(cleanup_handler))
        .route("/api/db-backups/restore", post(restore_handler))
        .route("/api/db-backups/export", get(export_handler))
        .route("/api/db-backups/import", post(import_handler))
        .route("/api/db-backups/{id}", delete(delete_one_handler))
}

fn manager(state: &AppState) -> BackupManager {
    BackupManager::new(&state.db.data_dir)
}

async fn list_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match manager(&state).list().await {
        Ok(backups) => Json(json!({
            "backups": backups,
            "maxFiles": BackupManager::max_files(),
            "retentionDays": BackupManager::retention_days(),
            "autoDisabled": BackupManager::is_auto_disabled(),
        }))
        .into_response(),
        Err(err) => internal_error(err),
    }
}

async fn create_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match manager(&state).create(BackupReason::Manual).await {
        Ok(Some(info)) => Json(json!({ "created": true, "backup": info })).into_response(),
        Ok(None) => {
            Json(json!({ "created": false, "message": "Backup skipped (db missing or invalid)" }))
                .into_response()
        }
        Err(err) => internal_error(err),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RestoreBody {
    backup_id: String,
}

async fn restore_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<RestoreBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let Json(payload) = match body {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Missing or invalid backupId" })),
            )
                .into_response()
        }
    };

    let mgr = manager(&state);

    let next_db = match mgr.read_backup(&payload.backup_id).await {
        Ok(db) => db,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": err.to_string() })),
            )
                .into_response();
        }
    };

    // Safety snapshot of the current db before we swap.
    if let Err(err) = mgr.create(BackupReason::PreRestore).await {
        tracing::warn!(
            target: "openproxy::db::backups",
            error = %err,
            "pre-restore backup failed; aborting restore"
        );
        return internal_error(err);
    }

    match state.db.replace_app_db(move || next_db).await {
        Ok(snapshot) => Json(json!({
            "restored": true,
            "backupId": payload.backup_id,
            "providerCount": snapshot.provider_connections.len(),
            "comboCount": snapshot.combos.len(),
            "apiKeyCount": snapshot.api_keys.len(),
        }))
        .into_response(),
        Err(err) => internal_error(err),
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CleanupBody {
    keep_latest: Option<usize>,
    retention_days: Option<u64>,
}

async fn cleanup_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<CleanupBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let body = body.map(|Json(b)| b).unwrap_or_default();
    let max_files = body.keep_latest.unwrap_or_else(BackupManager::max_files);
    let retention_days = body
        .retention_days
        .unwrap_or_else(BackupManager::retention_days);

    match manager(&state).cleanup(max_files, retention_days).await {
        Ok(result) => Json(json!({ "cleaned": true, "result": result })).into_response(),
        Err(err) => internal_error(err),
    }
}

async fn delete_one_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match manager(&state).delete(&id).await {
        Ok(()) => Json(json!({ "deleted": true, "id": id })).into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

async fn export_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let snapshot = state.db.snapshot();
    let bytes = match serde_json::to_vec_pretty(snapshot.as_ref()) {
        Ok(b) => b,
        Err(err) => return internal_error(anyhow::anyhow!(err)),
    };

    let timestamp = Utc::now()
        .to_rfc3339_opts(SecondsFormat::Millis, true)
        .replace([':', '.'], "-");
    let filename = format!("openproxy-backup-{}.json", timestamp);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .header(header::CACHE_CONTROL, "no-cache, no-store")
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        .body(Body::from(bytes))
        .unwrap_or_else(|err| internal_error(anyhow::anyhow!(err)))
}

const MAX_IMPORT_BYTES: usize = 100 * 1024 * 1024; // 100 MB

async fn import_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Response {
    if let Err(response) = require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let bytes = match collect_multipart_payload(multipart).await {
        Ok(bytes) => bytes,
        Err(err) => return err.into_response(),
    };

    if bytes.len() > MAX_IMPORT_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({
                "error": format!(
                    "File too large. Maximum allowed size is {} MB.",
                    MAX_IMPORT_BYTES / (1024 * 1024)
                )
            })),
        )
            .into_response();
    }

    let parsed: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("Invalid JSON: {err}") })),
            )
                .into_response();
        }
    };

    if !parsed.is_object() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Import payload must be a JSON object" })),
        )
            .into_response();
    }

    // Pre-import safety snapshot.
    if let Err(err) = manager(&state).create(BackupReason::PreImport).await {
        tracing::warn!(
            target: "openproxy::db::backups",
            error = %err,
            "pre-import backup failed; aborting import"
        );
        return internal_error(err);
    }

    let next = AppDb::from_json_value(parsed);
    match state.db.replace_app_db(move || next).await {
        Ok(snapshot) => Json(json!({
            "imported": true,
            "providerCount": snapshot.provider_connections.len(),
            "comboCount": snapshot.combos.len(),
            "apiKeyCount": snapshot.api_keys.len(),
        }))
        .into_response(),
        Err(err) => internal_error(err),
    }
}

enum ImportError {
    BadRequest(String),
}

impl ImportError {
    fn into_response(self) -> Response {
        match self {
            ImportError::BadRequest(msg) => {
                (StatusCode::BAD_REQUEST, Json(json!({ "error": msg }))).into_response()
            }
        }
    }
}

async fn collect_multipart_payload(mut multipart: Multipart) -> Result<Vec<u8>, ImportError> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ImportError::BadRequest(e.to_string()))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name != "file" {
            continue;
        }
        let bytes = field
            .bytes()
            .await
            .map_err(|e| ImportError::BadRequest(e.to_string()))?;
        return Ok(bytes.to_vec());
    }
    Err(ImportError::BadRequest(
        "No 'file' field in multipart upload".into(),
    ))
}

fn internal_error(err: anyhow::Error) -> Response {
    tracing::error!(
        target: "openproxy::db::backups",
        error = %err,
        "db-backup operation failed"
    );
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": err.to_string() })),
    )
        .into_response()
}
