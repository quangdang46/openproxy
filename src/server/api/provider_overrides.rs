//! Per-provider custom header overrides — 9router
//! `src/app/api/providers/[id]/overrides/route.js`.
//!
//! The dashboard pre-fills the registry's built-in headers, stores only the
//! **user-changed** rows in `settings.providerOverrides`, and the executor
//! merges them **last** at dispatch (open-sse/executors/base.js:131). The
//! blocked-header list below is enforced here, at the API boundary — the
//! executor trusts what it is given.

use std::collections::BTreeMap;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::server::state::AppState;
use crate::types::ProviderHeaderOverride;

/// Validation at the trust boundary — the UI also validates, but this is the
/// gate (route.js `MAX_HEADERS` / `MAX_HEADER_VALUE_LENGTH`).
const MAX_HEADERS: usize = 20;
const MAX_HEADER_VALUE_LENGTH: usize = 8192;
/// RFC 7230 token subset: letters, digits, hyphen (no spaces, no unicode).
const HEADER_NAME_RE: &str = "^[A-Za-z0-9-]+$";
/// Request-structure / auth headers a user override must never touch
/// (route.js `BLOCKED_HEADERS`).
const BLOCKED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "content-type",
    "connection",
    "transfer-encoding",
    "authorization",
    "cookie",
];

fn header_name_valid(name: &str) -> bool {
    // Regex-free equivalent of /^[A-Za-z0-9-]+$/ (no regex dep in this crate).
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Validate + normalize an override payload. Returns `Ok(headers)` — the
/// cleaned map, empty meaning "delete the override" — or `Err(message)`.
/// Mirrors `normalizeOverride` in the 9router route.
fn normalize_override_headers(
    headers: Option<&Map<String, Value>>,
) -> Result<BTreeMap<String, String>, String> {
    let mut clean = BTreeMap::new();
    let Some(headers) = headers else {
        return Ok(clean);
    };
    let entries: Vec<(&String, &Value)> = headers
        .iter()
        .filter(|(_, v)| !v.is_null() && v.as_str() != Some(""))
        .collect();
    if entries.len() > MAX_HEADERS {
        return Err(format!("Too many headers (max {MAX_HEADERS})"));
    }
    for (name, value) in entries {
        if !header_name_valid(name) {
            return Err(format!("Invalid header name: {name}"));
        }
        let Some(value) = value.as_str() else {
            return Err(format!("Invalid value for header {name}"));
        };
        if value.contains('\r') || value.contains('\n') {
            return Err(format!("Invalid value for header {name}"));
        }
        if value.len() > MAX_HEADER_VALUE_LENGTH {
            return Err(format!(
                "Header {name} value too long (max {MAX_HEADER_VALUE_LENGTH})"
            ));
        }
        if BLOCKED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(format!("Header {name} cannot be overridden"));
        }
        clean.insert(name.clone(), value.to_string());
    }
    Ok(clean)
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// GET /api/providers/{id}/overrides — user override for this provider.
///
/// `{headers, builtinHeaders}` — built-in headers come straight from the
/// provider registry config so the UI pre-fills exactly what this provider
/// sends upstream.
pub(super) async fn get_provider_overrides(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }
    let canonical = crate::core::model::resolve_provider_alias(&id);
    let snapshot = state.db.snapshot();
    let headers_out = snapshot
        .settings
        .provider_overrides
        .get(&canonical)
        .map(|o| o.headers.clone())
        .unwrap_or_default();
    let builtin: BTreeMap<String, String> =
        crate::core::executor::provider_config_headers(&canonical)
            .into_iter()
            .collect();
    Json(json!({
        "headers": headers_out,
        "builtinHeaders": builtin,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PutProviderOverridesRequest {
    /// `{name: value}` map; absent/empty clears the override.
    #[serde(default)]
    headers: Option<Value>,
}

/// PUT /api/providers/{id}/overrides — body `{headers: {name: value}}`.
/// An empty payload clears the override.
pub(super) async fn put_provider_overrides(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<PutProviderOverridesRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }
    let canonical = crate::core::model::resolve_provider_alias(&id);

    // `headers` may arrive as a JSON object; anything else is a 400.
    let header_map = match body.headers {
        None | Some(Value::Null) => None,
        Some(Value::Object(map)) => Some(map),
        Some(_) => {
            return json_error(StatusCode::BAD_REQUEST, "headers must be an object");
        }
    };
    let clean = match normalize_override_headers(header_map.as_ref()) {
        Ok(clean) => clean,
        Err(message) => return json_error(StatusCode::BAD_REQUEST, &message),
    };

    let canonical_for_update = canonical.clone();
    let result = state
        .db
        .update_settings(move |settings| {
            if clean.is_empty() {
                settings.provider_overrides.remove(&canonical_for_update);
            } else {
                settings.provider_overrides.insert(
                    canonical_for_update.clone(),
                    ProviderHeaderOverride {
                        headers: clean.clone(),
                    },
                );
            }
        })
        .await;

    match result {
        Ok(_) => {
            let headers_out: BTreeMap<String, String> = state
                .db
                .snapshot()
                .settings
                .provider_overrides
                .get(&canonical)
                .map(|o| o.headers.clone())
                .unwrap_or_default();
            Json(json!({ "headers": headers_out })).into_response()
        }
        Err(_) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to save overrides",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
            .collect()
    }

    #[test]
    fn override_replaces_and_adds_headers() {
        let map = obj(&[("X-Custom", "1"), ("X-Other", "2")]);
        let clean = normalize_override_headers(Some(&map)).unwrap();
        assert_eq!(clean.get("X-Custom").unwrap(), "1");
        assert_eq!(clean.get("X-Other").unwrap(), "2");
    }

    #[test]
    fn blocked_headers_rejected() {
        for blocked in [
            "host",
            "Host",
            "authorization",
            "Authorization",
            "cookie",
            "content-type",
            "content-length",
            "connection",
            "transfer-encoding",
        ] {
            let map = obj(&[(blocked, "x")]);
            let err = normalize_override_headers(Some(&map)).unwrap_err();
            assert!(
                err.contains("cannot be overridden"),
                "{blocked} should be blocked, got {err}"
            );
        }
    }

    #[test]
    fn invalid_names_and_crlf_values_rejected() {
        let bad_name = obj(&[("Bad Header", "x")]);
        assert!(normalize_override_headers(Some(&bad_name)).is_err());
        let bad_value = obj(&[("X-Ok", "a\r\nb")]);
        assert!(normalize_override_headers(Some(&bad_value)).is_err());
        let non_string = {
            let mut m = Map::new();
            m.insert("X-Num".into(), json!(42));
            m
        };
        assert!(normalize_override_headers(Some(&non_string)).is_err());
    }

    #[test]
    fn empty_and_absent_headers_normalize_to_empty() {
        assert!(normalize_override_headers(None).unwrap().is_empty());
        let map = obj(&[("X-Empty", "")]);
        assert!(normalize_override_headers(Some(&map)).unwrap().is_empty());
    }

    #[test]
    fn too_many_headers_rejected() {
        let pairs: Vec<(String, String)> = (0..=MAX_HEADERS)
            .map(|i| (format!("X-H{i}"), "v".to_string()))
            .collect();
        let map: Map<String, Value> = pairs
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect();
        assert!(normalize_override_headers(Some(&map)).is_err());
    }
}
