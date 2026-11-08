//! Per-API-key access control — port of 9router
//! `src/shared/utils/keyAccess.js`, `src/shared/constants/keyAccess.js` and
//! `src/sse/services/keyAccess.js` (v0.5.112).
//!
//! Two modes per key: UNRESTRICTED (default — exactly the behaviour before
//! this feature) or RESTRICTED, where the key may call ONLY the combos and
//! models in its allow list. No wildcards, no deny mode. Matching is exact
//! and case-insensitive, on RESOLVED identities, so every spelling that
//! routes to the same target is treated the same and no spelling can route
//! somewhere the list does not name:
//!
//!   * a request that routes as a combo (`get_combo_models_from_data` — the
//!     lookup routing itself uses) is allowed iff that combo's name is
//!     listed;
//!   * a request that routes to a model is allowed iff its resolved
//!     `provider/model` equals the resolved `provider/model` of a listed
//!     model entry. So `cx/gpt-5`, `codex/gpt-5` and an alias that points
//!     there are one target; a raw string that merely looks like a listed
//!     entry but routes elsewhere is not;
//!   * search/fetch (provider IS the model) are allowed iff the resolved
//!     provider id is listed, or the request is a listed combo.
//!
//! An allowed combo grants its members when they are reached THROUGH that
//! combo (the check runs once, on the requested target, before combo
//! expansion). It does not grant calling a member directly by name — that
//! needs its own entry. The check runs before any credential lookup, so a
//! denied request never touches a provider account.

use std::collections::HashSet;

use serde_json::Value;

use crate::core::combo::get_combo_models_from_data;
use crate::core::model::{get_model_info, resolve_provider_alias};
use crate::types::{ApiKey, AppDb, KeyAccess};

/// Upper bounds for one key's allow list (`KEY_ACCESS_MAX_ENTRIES` /
/// `KEY_ACCESS_MAX_ENTRY_LENGTH`). The API rejects input beyond these (400)
/// rather than truncating it, so a declared list is never silently cut.
pub const KEY_ACCESS_MAX_ENTRIES: usize = 200;
pub const KEY_ACCESS_MAX_ENTRY_LENGTH: usize = 256;

/// `/v1/models` `owned_by` tag for combo entries (`KEY_ACCESS_COMBO_OWNER`).
pub const KEY_ACCESS_COMBO_OWNER: &str = "combo";

/// `/v1/models` entry kinds whose "model" is the provider itself
/// (search/fetch) — `KEY_ACCESS_PROVIDER_AS_MODEL_KINDS`.
pub const KEY_ACCESS_PROVIDER_AS_MODEL_KINDS: &[&str] = &["webSearch", "webFetch"];

/// Client-facing 403 message (`keyAccessDeniedMessage`). Names only what the
/// client itself sent: never the key name or the key's allow list.
pub fn key_access_denied_message(requested: &str) -> String {
    format!("This API key is not allowed to use model \"{requested}\"")
}

/// `normalizeAllowList`: keep strings only, trim, drop empties, de-duplicate
/// case-insensitively (the first spelling wins). Order is preserved.
pub fn normalize_allow_list(list: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in list {
        let entry = item.trim();
        if entry.is_empty() {
            continue;
        }
        let key = entry.to_lowercase();
        if !seen.insert(key) {
            continue;
        }
        out.push(entry.to_string());
    }
    out
}

/// `keyAccessFromColumns`: build the access object from the two apiKeys
/// columns.
///
/// Fails CLOSED: the restricted flag has its own INTEGER column, so a
/// corrupt or unparsable allow list can only ever shrink what a restricted
/// key may call (to nothing); it can never turn a restricted key into an
/// unrestricted one.
pub fn key_access_from_columns(restricted: Option<i64>, allow: Option<&str>) -> KeyAccess {
    // JS: `restrictedCol === 1 || restrictedCol === true`.
    if restricted != Some(1) {
        return KeyAccess::default();
    }
    let parsed: Vec<String> = allow
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|item| item.as_str().map(str::to_string))
        .collect();
    KeyAccess {
        restricted: true,
        allow: normalize_allow_list(&parsed),
    }
}

/// `keyAccessToColumns`: the `(accessRestricted, accessAllow)` column pair
/// to persist for an access object (already validated/normalized).
pub fn key_access_to_columns(access: &KeyAccess) -> (i64, String) {
    let allow = if access.restricted {
        normalize_allow_list(&access.allow)
    } else {
        Vec::new()
    };
    (
        i64::from(access.restricted),
        serde_json::to_string(&allow).unwrap_or_else(|_| "[]".to_string()),
    )
}

/// `validateKeyAccessInput`: validate client input for an access update
/// (`{ restricted, allow }`). Rejects rather than repairs: wrong types, too
/// many entries, overlong entries.
///
/// Returns the normalized `KeyAccess` or a client-facing error string.
pub fn validate_key_access_input(input: &Value) -> Result<KeyAccess, String> {
    let Some(obj) = input.as_object() else {
        return Err("access must be an object { restricted, allow }".to_string());
    };
    for key in obj.keys() {
        if key != "restricted" && key != "allow" {
            return Err(format!("access: unexpected field(s) {key}"));
        }
    }
    let Some(restricted) = obj.get("restricted").and_then(Value::as_bool) else {
        return Err("access.restricted must be a boolean".to_string());
    };
    let allow_value = obj.get("allow").cloned().unwrap_or(Value::Array(vec![]));
    let Some(allow_array) = allow_value.as_array() else {
        return Err("access.allow must be an array of strings".to_string());
    };
    if allow_array.iter().any(|e| !e.is_string()) {
        return Err("access.allow must be an array of strings".to_string());
    }
    if allow_array.len() > KEY_ACCESS_MAX_ENTRIES {
        return Err(format!(
            "access.allow has more than {KEY_ACCESS_MAX_ENTRIES} entries"
        ));
    }
    if allow_array.iter().any(|e| {
        e.as_str()
            .is_some_and(|s| s.trim().chars().count() > KEY_ACCESS_MAX_ENTRY_LENGTH)
    }) {
        return Err(format!(
            "access.allow entries must be at most {KEY_ACCESS_MAX_ENTRY_LENGTH} characters"
        ));
    }
    let allow: Vec<String> = allow_array
        .iter()
        .filter_map(|e| e.as_str().map(str::to_string))
        .collect();
    Ok(KeyAccess {
        restricted,
        allow: normalize_allow_list(&allow),
    })
}

/// Access context for the request's key, or `None` when the request is
/// unrestricted (no key, unknown key, or a key whose access is unrestricted).
/// Keys are only authenticated elsewhere; this only decides what an
/// identified key may call. Mirrors `getKeyAccessContext`.
#[derive(Debug, Clone)]
pub struct KeyAccessContext {
    pub key_id: String,
    /// `record.name || record.id` upstream — the name shown in deny logs.
    pub key_name: String,
    pub allow: Vec<String>,
}

/// Build the context from the apiKeys row the presented secret resolved to.
pub fn key_access_context(api_key: Option<&ApiKey>) -> Option<KeyAccessContext> {
    let record = api_key?;
    if !record.access.restricted {
        return None;
    }
    Some(KeyAccessContext {
        key_id: record.id.clone(),
        key_name: if record.name.is_empty() {
            record.id.clone()
        } else {
            record.name.clone()
        },
        allow: record.access.allow.clone(),
    })
}

/// Resolve the presented secret (`extract_api_key` output) against the
/// snapshot's key index — the Rust equivalent of upstream's
/// `getApiKeyByKey(extractClientApiKey(request))`.
pub fn context_for_presented_key(
    presented_key: Option<&str>,
    snapshot: &AppDb,
) -> Option<KeyAccessContext> {
    let key = presented_key?;
    snapshot
        .api_key_map
        .get(key)
        .and_then(|record| key_access_context(Some(record)))
}

/// The allow list resolved once per request: combo names, resolved model
/// ids, and resolved provider ids (for provider-as-model kinds). All keys
/// are lowercase, matching upstream's `lower()` sets.
#[derive(Debug, Default)]
pub struct KeyAccessSets {
    pub combos: HashSet<String>,
    pub models: HashSet<String>,
    pub providers: HashSet<String>,
}

fn canonical_model(provider: &str, model: &str) -> String {
    format!("{}/{}", provider.to_lowercase(), model.to_lowercase())
}

/// `getAllowSets`: for each allow entry — a combo name goes to `combos`; a
/// model entry resolves through `get_model_info` into `models`; a slash-less
/// entry additionally resolves its provider into `providers`.
pub fn allow_sets(ctx: &KeyAccessContext, snapshot: &AppDb) -> KeyAccessSets {
    let combo_names: HashSet<String> = snapshot
        .combos
        .iter()
        .map(|combo| combo.name.to_lowercase())
        .collect();
    let mut sets = KeyAccessSets::default();
    for entry in &ctx.allow {
        let key = entry.to_lowercase();
        if combo_names.contains(&key) {
            sets.combos.insert(key);
            continue;
        }
        let info = get_model_info(entry, snapshot);
        if let Some(provider) = info.provider {
            sets.models.insert(canonical_model(&provider, &info.model));
        }
        if !entry.contains('/') {
            sets.providers
                .insert(resolve_provider_alias(entry).to_lowercase());
        }
    }
    sets
}

/// `isModelStringAllowed`: a requested model string is allowed iff its
/// resolved `provider/model` is in the models set; a string that resolves to
/// no provider (a combo with no members) is allowed iff it names a listed
/// combo.
pub fn is_model_string_allowed(sets: &KeyAccessSets, requested: &str, snapshot: &AppDb) -> bool {
    let info = get_model_info(requested, snapshot);
    match info.provider {
        None => sets.combos.contains(&requested.to_lowercase()),
        Some(provider) => sets
            .models
            .contains(&canonical_model(&provider, &info.model)),
    }
}

/// `enforceKeyAccess`: gate for endpoints whose `model` may be a combo or a
/// model (chat, image, tts). Call with the requested string BEFORE combo
/// expansion.
///
/// Returns `Err(client-facing message)` when denied; `Ok(())` when the
/// request is unrestricted or allowed.
pub fn enforce_key_access(
    ctx: Option<&KeyAccessContext>,
    requested: &str,
    snapshot: &AppDb,
) -> Result<(), String> {
    let Some(ctx) = ctx else {
        return Ok(());
    };
    if requested.is_empty() {
        return Err(key_access_denied_message(requested));
    }
    let sets = allow_sets(ctx, snapshot);
    let allowed = if get_combo_models_from_data(requested, &snapshot.combos).is_some() {
        sets.combos.contains(&requested.to_lowercase())
    } else {
        is_model_string_allowed(&sets, requested, snapshot)
    };
    if allowed {
        Ok(())
    } else {
        tracing::warn!(
            target: "openproxy::auth",
            "key-access: key \"{}\" denied \"{}\"",
            ctx.key_name,
            requested
        );
        Err(key_access_denied_message(requested))
    }
}

/// `enforceKeyAccessResolved`: gate for endpoints that resolve a single
/// provider/model themselves (embeddings, stt, video). A missing model is
/// denied for restricted keys: an unverifiable target is not an allowed one.
pub fn enforce_key_access_resolved(
    ctx: Option<&KeyAccessContext>,
    requested: &str,
    provider: Option<&str>,
    model: Option<&str>,
    snapshot: &AppDb,
) -> Result<(), String> {
    let Some(ctx) = ctx else {
        return Ok(());
    };
    let label = if !requested.is_empty() {
        requested.to_string()
    } else {
        match provider {
            Some(provider) if !provider.is_empty() => {
                format!("{provider}/{}", model.unwrap_or(""))
            }
            _ => String::new(),
        }
    };
    let (Some(provider), Some(model)) = (provider, model) else {
        return deny(ctx, &label);
    };
    if provider.is_empty() || model.is_empty() {
        return deny(ctx, &label);
    }
    let sets = allow_sets(ctx, snapshot);
    if sets.models.contains(&canonical_model(provider, model)) {
        Ok(())
    } else {
        deny(ctx, &label)
    }
}

fn deny(ctx: &KeyAccessContext, label: &str) -> Result<(), String> {
    tracing::warn!(
        target: "openproxy::auth",
        "key-access: key \"{}\" denied \"{}\"",
        ctx.key_name,
        label
    );
    Err(key_access_denied_message(label))
}

/// `enforceKeyAccessProvider`: gate for search/fetch, where the provider IS
/// the model.
pub fn enforce_key_access_provider(
    ctx: Option<&KeyAccessContext>,
    requested: &str,
    snapshot: &AppDb,
) -> Result<(), String> {
    let Some(ctx) = ctx else {
        return Ok(());
    };
    if requested.is_empty() {
        return Err(key_access_denied_message(requested));
    }
    let sets = allow_sets(ctx, snapshot);
    let allowed = if get_combo_models_from_data(requested, &snapshot.combos).is_some() {
        sets.combos.contains(&requested.to_lowercase())
    } else {
        sets.providers
            .contains(&resolve_provider_alias(requested).to_lowercase())
    };
    if allowed {
        Ok(())
    } else {
        deny(ctx, requested)
    }
}

/// `filterAdapterModels`: capacity-adapter models are appended by the
/// gateway, not chosen by the client. For a restricted key keep only the
/// adapter models the key may call directly; the original targets (already
/// gated) are always kept. Dropping a model just shortens the fallback
/// list, so combo failover is unaffected.
pub fn filter_adapter_models(
    ctx: Option<&KeyAccessContext>,
    augmented: Vec<String>,
    original: &[String],
    snapshot: &AppDb,
) -> Vec<String> {
    let Some(ctx) = ctx else {
        return augmented;
    };
    let sets = allow_sets(ctx, snapshot);
    let keep: HashSet<&str> = original.iter().map(String::as_str).collect();
    augmented
        .into_iter()
        .filter(|model| {
            if keep.contains(model.as_str()) {
                return true;
            }
            let allowed = is_model_string_allowed(&sets, model, snapshot);
            if !allowed {
                tracing::info!(
                    target: "openproxy::auth",
                    "key-access: key \"{}\" skips adapter model \"{}\" (not allowed)",
                    ctx.key_name,
                    model
                );
            }
            allowed
        })
        .collect()
}

/// `filterModelsListForKey` decision for one `/v1/models` card:
/// `owned_by == "combo"` entries match the combos set; provider-as-model
/// kinds (`webSearch`/`webFetch`) match the providers set; everything else
/// matches the resolved `provider/model`.
pub fn model_card_allowed(
    sets: &KeyAccessSets,
    id: &str,
    owned_by: &str,
    kind: Option<&str>,
    snapshot: &AppDb,
) -> bool {
    if owned_by == KEY_ACCESS_COMBO_OWNER {
        return sets.combos.contains(&id.to_lowercase());
    }
    let info = get_model_info(id, snapshot);
    let Some(provider) = info.provider else {
        return false;
    };
    if kind.is_some_and(|k| KEY_ACCESS_PROVIDER_AS_MODEL_KINDS.contains(&k)) {
        return sets.providers.contains(&provider.to_lowercase());
    }
    sets.models
        .contains(&canonical_model(&provider, &info.model))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalize_trims_dedupes_case_insensitive() {
        let list = vec![
            "  Combo-A ".to_string(),
            "combo-a".to_string(),
            "".to_string(),
            "openai/gpt-4o".to_string(),
            "  ".to_string(),
        ];
        assert_eq!(
            normalize_allow_list(&list),
            vec!["Combo-A".to_string(), "openai/gpt-4o".to_string()]
        );
    }

    #[test]
    fn columns_roundtrip_and_fail_closed() {
        // Unrestricted ignores the allow column entirely.
        let access = key_access_from_columns(Some(0), Some("[\"x\"]"));
        assert!(!access.restricted);
        assert!(access.allow.is_empty());

        // Restricted with corrupt JSON → empty allow (can call nothing).
        let access = key_access_from_columns(Some(1), Some("not json"));
        assert!(access.restricted);
        assert!(access.allow.is_empty());

        let access = key_access_from_columns(Some(1), Some("[\"Main\", \"main\", \"cx/gpt-5\"]"));
        assert_eq!(
            access.allow,
            vec!["Main".to_string(), "cx/gpt-5".to_string()]
        );

        let (restricted, allow) = key_access_to_columns(&access);
        assert_eq!(restricted, 1);
        let back = key_access_from_columns(Some(restricted), Some(&allow));
        assert_eq!(back, access);

        let (restricted, allow) = key_access_to_columns(&KeyAccess::default());
        assert_eq!(restricted, 0);
        assert_eq!(allow, "[]");
    }

    #[test]
    fn validate_rejects_rather_than_repairs() {
        assert!(validate_key_access_input(&json!("nope")).is_err());
        assert!(validate_key_access_input(&json!({"restricted": "yes", "allow": []})).is_err());
        assert!(validate_key_access_input(&json!({"restricted": true, "allow": "x"})).is_err());
        assert!(validate_key_access_input(&json!({"restricted": true, "allow": [1]})).is_err());
        assert!(
            validate_key_access_input(&json!({"restricted": true, "allow": [], "extra": 1}))
                .is_err()
        );
        let too_many = vec!["a"; KEY_ACCESS_MAX_ENTRIES + 1];
        assert!(
            validate_key_access_input(&json!({"restricted": true, "allow": too_many})).is_err()
        );
        let too_long = "x".repeat(KEY_ACCESS_MAX_ENTRY_LENGTH + 1);
        assert!(
            validate_key_access_input(&json!({"restricted": true, "allow": [too_long]})).is_err()
        );

        let ok =
            validate_key_access_input(&json!({"restricted": true, "allow": [" Combo ", "combo"]}))
                .unwrap();
        assert!(ok.restricted);
        assert_eq!(ok.allow, vec!["Combo".to_string()]);
    }

    #[test]
    fn unrestricted_context_is_none() {
        let key = ApiKey {
            id: "k1".into(),
            name: "n".into(),
            key: "sk-x".into(),
            ..Default::default()
        };
        assert!(key_access_context(Some(&key)).is_none());
        assert!(key_access_context(None).is_none());
    }

    #[test]
    fn restricted_key_denied_and_allowed() {
        use crate::types::Combo;
        let mut db = AppDb::default();
        db.combos.push(Combo {
            id: "c1".into(),
            name: "Main".into(),
            ..Default::default()
        });
        let key = ApiKey {
            id: "k1".into(),
            name: "restricted".into(),
            key: "sk-x".into(),
            access: KeyAccess {
                restricted: true,
                allow: vec!["Main".to_string(), "cx/gpt-5".to_string()],
            },
            ..Default::default()
        };
        let ctx = key_access_context(Some(&key)).unwrap();

        // Listed combo allowed under its exact routing name; the resolved
        // `provider/model` of a listed entry accepts every spelling that
        // routes there (cx is an alias of codex). A wrong-case combo name
        // routes to `openai/MAIN`, not the combo — denied, matching upstream.
        assert!(enforce_key_access(Some(&ctx), "Main", &db).is_ok());
        assert!(enforce_key_access(Some(&ctx), "MAIN", &db).is_err());
        assert!(enforce_key_access(Some(&ctx), "codex/gpt-5", &db).is_ok());
        assert!(enforce_key_access(Some(&ctx), "cx/gpt-5", &db).is_ok());
        assert!(enforce_key_access(Some(&ctx), "openai/gpt-5", &db).is_err());
        assert!(enforce_key_access(Some(&ctx), "", &db).is_err());
        // No context → unrestricted.
        assert!(enforce_key_access(None, "anything", &db).is_ok());
    }
}
