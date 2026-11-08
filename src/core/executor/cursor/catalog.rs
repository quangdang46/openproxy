//! Port of `open-sse/services/cursorModels.js` — the live Cursor model
//! catalog fetched from AgentService `GetUsableModels`.
//!
//! Unlike the static provider registry, the live catalog includes models
//! newly enabled for the account and omits unavailable ones. It also carries
//! the model-picker flags (`defaultOn` / `isChatOnly` / `supportsAgent` /
//! `isHidden`) that `resolveAutoModelSelection` needs to turn the picker's
//! "Auto (Server Picks)" / "default" entry into a real AgentService model id —
//! sending the alias verbatim makes `Run` come back empty.
//!
//! Transport: `agent.api5.cursor.sh` is HTTP/2-only and the unary
//! GetUsableModels call uses an *unframed* protobuf body
//! (`application/proto`), unlike the chat path's streaming
//! `application/connect+proto`. We reuse the pool's hyper h2 client — the
//! same client `execute_agent` uses for `AgentService/Run`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::http;
use hyper::http::header::HeaderValue;
use sha2::{Digest, Sha256};

use crate::core::executor::DirectHyperClient;
use crate::types::ProviderConnection;

use super::{build_cursor_headers, decode_message, CursorExecutorError};

/// `agent.v1.AgentService/GetUsableModels` path
/// (9router registry cursor.js `oauth.modelsEndpoint`).
const CURSOR_MODELS_PATH: &str = "/agent.v1.AgentService/GetUsableModels";
const CURSOR_MODELS_AUTHORITY: &str = "agent.api5.cursor.sh";
const CURSOR_MODELS_BASE_URL: &str = "https://agent.api5.cursor.sh";

/// 9router cursorModels.js FETCH_TIMEOUT_MS.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// 9router cursorModels.js CACHE_TTL_MS (5 minutes).
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

// agent.v1.ModelDetails protobuf field numbers (response field 1).
const MODEL_ID_FIELD: u32 = 1;
const DISPLAY_MODEL_ID_FIELD: u32 = 3;
const DISPLAY_NAME_FIELD: u32 = 4;
const DISPLAY_NAME_SHORT_FIELD: u32 = 5;
const RESPONSE_MODELS_FIELD: u32 = 1;

// agent.v1.AvailableModels (response field 2) — the model-picker flags.
const RESPONSE_AVAILABLE_MODELS_FIELD: u32 = 2;
const AVAILABLE_MODEL_NAME_FIELD: u32 = 1;
const AVAILABLE_MODEL_DEFAULT_ON_FIELD: u32 = 2;
const AVAILABLE_MODEL_IS_CHAT_ONLY_FIELD: u32 = 4;
const AVAILABLE_MODEL_SUPPORTS_AGENT_FIELD: u32 = 5;
const AVAILABLE_MODEL_IS_HIDDEN_FIELD: u32 = 35;

/// One entry of the live Cursor catalog: the usable-model id/name plus the
/// picker flags merged on from `AvailableModels` (9router
/// `mergeCursorModelFlags`). Flags default to false when the server did not
/// send field 2 for the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorCatalogModel {
    pub id: String,
    pub name: String,
    pub default_on: bool,
    pub is_chat_only: bool,
    pub supports_agent: bool,
    pub is_hidden: bool,
}

/// Picker flags decoded from `agent.v1.AvailableModels` before the merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct AvailableModelFlags {
    default_on: bool,
    is_chat_only: bool,
    supports_agent: bool,
    is_hidden: bool,
}

/// Result of resolving "Auto (Server Picks)" / "default" (9router
/// `resolveAutoModelSelection` return shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoModelSelection {
    pub model_id: String,
    pub matched_by: &'static str,
}

fn first_bytes<'a>(fields: &'a HashMap<u32, Vec<Vec<u8>>>, field_number: u32) -> Option<&'a [u8]> {
    fields.get(&field_number)?.first().map(Vec::as_slice)
}

fn first_string(fields: &HashMap<u32, Vec<Vec<u8>>>, field_number: u32) -> String {
    first_bytes(fields, field_number)
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default()
}

/// Varint fields are stored by `decode_message` as the decoded u32 in
/// little-endian bytes, so a protobuf `true` is `[1, 0, 0, 0]`.
fn first_varint_is_one(fields: &HashMap<u32, Vec<Vec<u8>>>, field_number: u32) -> bool {
    match first_bytes(fields, field_number) {
        Some(bytes) if bytes.len() == 4 => {
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) == 1
        }
        _ => false,
    }
}

/// Decode `agent.v1.GetUsableModelsResponse` field 1: repeated
/// `agent.v1.ModelDetails` → `(id, display name)` pairs (9router
/// `parseCursorUsableModels`).
pub fn parse_cursor_usable_models(payload: &[u8]) -> Vec<(String, String)> {
    let Ok(response) = decode_message(payload) else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut models = Vec::new();

    for entry in response.get(&RESPONSE_MODELS_FIELD).into_iter().flatten() {
        let Ok(detail) = decode_message(entry) else {
            continue;
        };
        let id = first_string(&detail, MODEL_ID_FIELD).trim().to_string();
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        let name = [
            DISPLAY_NAME_FIELD,
            DISPLAY_NAME_SHORT_FIELD,
            DISPLAY_MODEL_ID_FIELD,
        ]
        .iter()
        .map(|&field| first_string(&detail, field))
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| id.clone())
        .trim()
        .to_string();
        models.push((id, name));
    }

    models
}

/// Decode response field 2, `agent.v1.AvailableModels`: canonical model name
/// plus picker flags (9router `parseCursorAvailableModels`).
pub fn parse_cursor_available_models(payload: &[u8]) -> Vec<(String, AvailableModelFlags)> {
    let Ok(response) = decode_message(payload) else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut models = Vec::new();

    for entry in response
        .get(&RESPONSE_AVAILABLE_MODELS_FIELD)
        .into_iter()
        .flatten()
    {
        let Ok(fields) = decode_message(entry) else {
            continue;
        };
        let id = first_string(&fields, AVAILABLE_MODEL_NAME_FIELD)
            .trim()
            .to_string();
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        models.push((
            id,
            AvailableModelFlags {
                default_on: first_varint_is_one(&fields, AVAILABLE_MODEL_DEFAULT_ON_FIELD),
                is_chat_only: first_varint_is_one(&fields, AVAILABLE_MODEL_IS_CHAT_ONLY_FIELD),
                supports_agent: first_varint_is_one(&fields, AVAILABLE_MODEL_SUPPORTS_AGENT_FIELD),
                is_hidden: first_varint_is_one(&fields, AVAILABLE_MODEL_IS_HIDDEN_FIELD),
            },
        ));
    }

    models
}

/// Merge picker flags onto the usable-models list (9router
/// `mergeCursorModelFlags`). Only models present in field 2 get flags; the
/// rest keep the all-false default, matching the JS spread of `undefined`.
pub fn merge_cursor_model_flags(
    models: Vec<(String, String)>,
    payload: &[u8],
) -> Vec<CursorCatalogModel> {
    let flags_by_id: HashMap<String, AvailableModelFlags> =
        parse_cursor_available_models(payload).into_iter().collect();
    models
        .into_iter()
        .map(|(id, name)| {
            let flags = flags_by_id.get(&id).copied().unwrap_or_default();
            CursorCatalogModel {
                id,
                name,
                default_on: flags.default_on,
                is_chat_only: flags.is_chat_only,
                supports_agent: flags.supports_agent,
                is_hidden: flags.is_hidden,
            }
        })
        .collect()
}

/// Parse the full catalog payload into merged models (usable models +
/// picker flags) — the Rust equivalent of `parseCursorUsableModels` +
/// `mergeCursorModelFlags` in one step.
pub fn parse_cursor_catalog(payload: &[u8]) -> Vec<CursorCatalogModel> {
    merge_cursor_model_flags(parse_cursor_usable_models(payload), payload)
}

/// "Auto (Server Picks)" / "default" is not a real AgentService model id —
/// sending it verbatim makes `Run` come back empty. Resolve it to the
/// account's default agent-capable model. Prefers the picker's `defaultOn`
/// model, then the first usable agent model (9router
/// `resolveAutoModelSelection`).
pub fn resolve_auto_model_selection(models: &[CursorCatalogModel]) -> Option<AutoModelSelection> {
    let usable = models
        .iter()
        .filter(|model| model.supports_agent && !model.is_chat_only && !model.is_hidden);
    let preferred = usable
        .clone()
        .find(|model| model.default_on)
        .or_else(|| usable.clone().next())?;
    Some(AutoModelSelection {
        model_id: preferred.id.clone(),
        matched_by: "auto-default",
    })
}

/// True for the picker aliases that must be resolved through the live
/// catalog before `AgentService/Run` (9router cursor.js:664 `model ===
/// "default" || model === "auto"`). Callers pass the already
/// `parse_cursor_model`-stripped name.
pub fn is_auto_model_selection(model: &str) -> bool {
    model == "default" || model == "auto"
}

// ==================== CACHE ====================

struct CachedCatalog {
    expires_at: Instant,
    models: Vec<CursorCatalogModel>,
}

fn catalog_cache() -> &'static Mutex<HashMap<String, CachedCatalog>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedCatalog>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 9router cursorModels.js `cacheKey`: sha256 over `cursor:{machineId}:{accessToken}`
/// so the raw token never sits in the cache key.
fn cache_key(access_token: &str, machine_id: &str) -> String {
    let seed = format!("cursor:{machine_id}:{access_token}");
    hex::encode(Sha256::digest(seed.as_bytes()))
}

/// Test hook: drop the whole catalog cache (9router `clearCursorModelCache`).
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn clear_cursor_model_cache() {
    if let Ok(mut cache) = catalog_cache().lock() {
        cache.clear();
    }
}

// ==================== FETCH ====================

/// Unary `GetUsableModels` over the h2 client: empty unframed protobuf body,
/// `application/proto` in both directions, connect-* headers removed (9router
/// `fetchCursorCatalog`).
pub async fn fetch_cursor_catalog(
    client: &DirectHyperClient,
    access_token: &str,
    machine_id: &str,
    ghost_mode: bool,
) -> Result<Vec<CursorCatalogModel>, CursorExecutorError> {
    let url = format!("{CURSOR_MODELS_BASE_URL}{CURSOR_MODELS_PATH}");

    let mut headers = build_cursor_headers(access_token, Some(machine_id), ghost_mode)?;
    headers.remove("connect-accept-encoding");
    headers.remove("connect-protocol-version");
    headers.insert("accept", HeaderValue::from_static("application/proto"));
    headers.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/proto"),
    );

    let mut builder = http::Request::builder()
        .method(http::Method::POST)
        .uri(url)
        .header(":authority", CURSOR_MODELS_AUTHORITY)
        .header(":scheme", "https");
    for (name, value) in headers.iter() {
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    let request = builder
        .body(Full::new(Bytes::new()))
        .map_err(CursorExecutorError::InvalidRequest)?;

    let response = tokio::time::timeout(FETCH_TIMEOUT, client.request(request))
        .await
        .map_err(|_| CursorExecutorError::Timeout("Cursor GetUsableModels timed out".into()))?
        .map_err(CursorExecutorError::Hyper)?;

    let status = response.status();
    if status != http::StatusCode::OK {
        return Err(CursorExecutorError::StreamError(format!(
            "Cursor GetUsableModels returned {status}"
        )));
    }

    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|e| CursorExecutorError::StreamError(format!("GetUsableModels body: {e}")))?
        .to_bytes();

    Ok(parse_cursor_catalog(&body))
}

/// Resolve the live catalog for an account with the 5-minute cache and
/// fail-open semantics of 9router `resolveCursorModels`: any failure or an
/// empty catalog returns `None` so callers fall back to static models.
pub async fn resolve_cursor_models(
    client: &DirectHyperClient,
    access_token: &str,
    machine_id: &str,
    ghost_mode: bool,
    force_refresh: bool,
) -> Option<Vec<CursorCatalogModel>> {
    let key = cache_key(access_token, machine_id);
    if !force_refresh {
        if let Ok(cache) = catalog_cache().lock() {
            if let Some(entry) = cache.get(&key) {
                if entry.expires_at > Instant::now() {
                    return Some(entry.models.clone());
                }
            }
        }
    }

    match fetch_cursor_catalog(client, access_token, machine_id, ghost_mode).await {
        Ok(models) if !models.is_empty() => {
            if let Ok(mut cache) = catalog_cache().lock() {
                cache.insert(
                    key,
                    CachedCatalog {
                        expires_at: Instant::now() + CACHE_TTL,
                        models: models.clone(),
                    },
                );
            }
            Some(models)
        }
        Ok(_) => None,
        Err(error) => {
            tracing::warn!("CURSOR_MODELS: live model fetch failed: {error}");
            None
        }
    }
}

/// Convenience wrapper over [`resolve_cursor_models`] that pulls
/// `accessToken` / `machineId` / `ghostMode` out of a stored connection
/// (9router `resolveCursorModels` credential shape).
pub async fn resolve_cursor_models_for_connection(
    client: &DirectHyperClient,
    connection: &ProviderConnection,
    force_refresh: bool,
) -> Option<Vec<CursorCatalogModel>> {
    let access_token = connection.access_token.as_deref()?;
    let machine_id = connection
        .provider_specific_data
        .get("machineId")
        .and_then(|v| v.as_str())?;
    let ghost_mode = connection
        .provider_specific_data
        .get("ghostMode")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if access_token.is_empty() || machine_id.is_empty() {
        return None;
    }
    resolve_cursor_models(client, access_token, machine_id, ghost_mode, force_refresh).await
}

// ==================== TESTS ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a protobuf field (LEN wire type). Tag and length are varints —
    /// field 35's tag does not fit in a single byte.
    fn pb_field(field: u32, data: &[u8]) -> Vec<u8> {
        fn varint(mut value: u32, out: &mut Vec<u8>) {
            loop {
                let mut b = (value & 0x7F) as u8;
                value >>= 7;
                if value > 0 {
                    b |= 0x80;
                }
                out.push(b);
                if value == 0 {
                    break;
                }
            }
        }
        let mut out = Vec::new();
        varint((field << 3) | 2, &mut out);
        varint(data.len() as u32, &mut out);
        out.extend_from_slice(data);
        out
    }

    fn pb_varint_field(field: u32, value: u32) -> Vec<u8> {
        fn push_varint(mut v: u32, out: &mut Vec<u8>) {
            loop {
                let mut b = (v & 0x7F) as u8;
                v >>= 7;
                if v > 0 {
                    b |= 0x80;
                }
                out.push(b);
                if v == 0 {
                    break;
                }
            }
        }
        let mut out = Vec::new();
        push_varint(field << 3, &mut out);
        push_varint(value, &mut out);
        out
    }

    fn model_details(id: &str, name: &str) -> Vec<u8> {
        let mut msg = pb_field(MODEL_ID_FIELD, id.as_bytes());
        msg.extend(pb_field(DISPLAY_NAME_FIELD, name.as_bytes()));
        msg
    }

    fn usable_models_response(details: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for detail in details {
            out.extend(pb_field(RESPONSE_MODELS_FIELD, detail));
        }
        out
    }

    fn available_models_response(entries: &[(&str, &[(u32, u32)])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, flags) in entries {
            let mut msg = pb_field(AVAILABLE_MODEL_NAME_FIELD, name.as_bytes());
            for (field, value) in *flags {
                msg.extend(pb_varint_field(field, value));
            }
            out.extend(pb_field(RESPONSE_AVAILABLE_MODELS_FIELD, &msg));
        }
        out
    }

    #[test]
    fn parse_usable_models_extracts_id_and_name() {
        let payload = usable_models_response(&[
            model_details("claude-4.6-opus-max", "Claude 4.6 Opus Max"),
            model_details("gpt-5.3-codex", "GPT 5.3 Codex"),
            // duplicate id is dropped
            model_details("claude-4.6-opus-max", "Claude 4.6 Opus Max"),
        ]);
        let models = parse_cursor_usable_models(&payload);
        assert_eq!(
            models,
            vec![
                (
                    "claude-4.6-opus-max".to_string(),
                    "Claude 4.6 Opus Max".to_string()
                ),
                ("gpt-5.3-codex".to_string(), "GPT 5.3 Codex".to_string()),
            ]
        );
    }

    #[test]
    fn parse_usable_models_falls_back_through_name_fields() {
        // No display name fields → id is the name; display_model_id used when
        // display_name is absent.
        let mut detail = pb_field(MODEL_ID_FIELD, b"kimi-k2.5");
        detail.extend(pb_field(DISPLAY_MODEL_ID_FIELD, b"Kimi K2.5"));
        let payload = usable_models_response(&[detail]);
        let models = parse_cursor_usable_models(&payload);
        assert_eq!(
            models[0],
            ("kimi-k2.5".to_string(), "Kimi K2.5".to_string())
        );
    }

    #[test]
    fn parse_available_models_reads_picker_flags() {
        let payload = available_models_response(&[(
            "claude-4.6-opus-max",
            &[
                (AVAILABLE_MODEL_DEFAULT_ON_FIELD, 1),
                (AVAILABLE_MODEL_SUPPORTS_AGENT_FIELD, 1),
            ],
        )]);
        let models = parse_cursor_available_models(&payload);
        assert_eq!(models.len(), 1);
        let (id, flags) = &models[0];
        assert_eq!(id, "claude-4.6-opus-max");
        assert!(flags.default_on);
        assert!(flags.supports_agent);
        assert!(!flags.is_chat_only);
        assert!(!flags.is_hidden);
    }

    #[test]
    fn parse_available_models_reads_high_field_numbers() {
        // isHidden is field 35 — its tag needs two varint bytes.
        let payload = available_models_response(&[(
            "stealth-model",
            &[
                (AVAILABLE_MODEL_SUPPORTS_AGENT_FIELD, 1),
                (AVAILABLE_MODEL_IS_HIDDEN_FIELD, 1),
            ],
        )]);
        let models = parse_cursor_available_models(&payload);
        assert!(models[0].1.supports_agent);
        assert!(models[0].1.is_hidden);
    }

    #[test]
    fn catalog_merge_attaches_flags_by_id() {
        let mut payload = usable_models_response(&[model_details("auto-model", "Auto Model")]);
        let flags = available_models_response(&[(
            "auto-model",
            &[
                (AVAILABLE_MODEL_DEFAULT_ON_FIELD, 1),
                (AVAILABLE_MODEL_SUPPORTS_AGENT_FIELD, 1),
            ],
        )]);
        // field-2 entries live at top level alongside field-1 entries
        payload.extend(flags);
        let models = parse_cursor_catalog(&payload);
        assert_eq!(models.len(), 1);
        assert!(models[0].default_on);
        assert!(models[0].supports_agent);
        assert_eq!(models[0].name, "Auto Model");
    }

    #[test]
    fn auto_resolution_prefers_default_on_agent_model() {
        let models = vec![
            CursorCatalogModel {
                id: "chat-only".into(),
                name: "Chat".into(),
                default_on: true,
                is_chat_only: true,
                supports_agent: true,
                is_hidden: false,
            },
            CursorCatalogModel {
                id: "hidden".into(),
                name: "Hidden".into(),
                default_on: false,
                is_chat_only: false,
                supports_agent: true,
                is_hidden: true,
            },
            CursorCatalogModel {
                id: "no-agent".into(),
                name: "NoAgent".into(),
                default_on: false,
                is_chat_only: false,
                supports_agent: false,
                is_hidden: false,
            },
            CursorCatalogModel {
                id: "agent-a".into(),
                name: "Agent A".into(),
                default_on: false,
                is_chat_only: false,
                supports_agent: true,
                is_hidden: false,
            },
            CursorCatalogModel {
                id: "agent-default".into(),
                name: "Agent Default".into(),
                default_on: true,
                is_chat_only: false,
                supports_agent: true,
                is_hidden: false,
            },
        ];
        let selection = resolve_auto_model_selection(&models).unwrap();
        assert_eq!(selection.model_id, "agent-default");
        assert_eq!(selection.matched_by, "auto-default");
    }

    #[test]
    fn auto_resolution_falls_back_to_first_usable_agent_model() {
        let models = vec![
            CursorCatalogModel {
                id: "chat-only".into(),
                name: "Chat".into(),
                default_on: true,
                is_chat_only: true,
                supports_agent: true,
                is_hidden: false,
            },
            CursorCatalogModel {
                id: "agent-a".into(),
                name: "Agent A".into(),
                default_on: false,
                is_chat_only: false,
                supports_agent: true,
                is_hidden: false,
            },
            CursorCatalogModel {
                id: "agent-b".into(),
                name: "Agent B".into(),
                default_on: false,
                is_chat_only: false,
                supports_agent: true,
                is_hidden: false,
            },
        ];
        let selection = resolve_auto_model_selection(&models).unwrap();
        assert_eq!(selection.model_id, "agent-a");
    }

    #[test]
    fn auto_resolution_returns_none_without_usable_agent_models() {
        assert!(resolve_auto_model_selection(&[]).is_none());
        let chat_only = vec![CursorCatalogModel {
            id: "chat".into(),
            name: "Chat".into(),
            default_on: true,
            is_chat_only: true,
            supports_agent: true,
            is_hidden: false,
        }];
        assert!(resolve_auto_model_selection(&chat_only).is_none());
    }

    #[test]
    fn auto_model_aliases_detected() {
        assert!(is_auto_model_selection("default"));
        assert!(is_auto_model_selection("auto"));
        assert!(!is_auto_model_selection("claude-4.6-opus-max"));
        assert!(!is_auto_model_selection("cursor/default"));
    }

    #[test]
    fn cache_key_does_not_contain_raw_token() {
        let key = cache_key("secret-token", "machine-1");
        assert_eq!(key.len(), 64);
        assert!(!key.contains("secret-token"));
        assert!(!key.contains("machine-1"));
    }

    #[test]
    fn malformed_payload_returns_empty() {
        assert!(parse_cursor_catalog(&[0xFF, 0xFF, 0xFF]).is_empty());
        assert!(parse_cursor_usable_models(&[]).is_empty());
    }
}
