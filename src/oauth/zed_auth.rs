//! Zed hosted LLM auth helpers — port of 9router `open-sse/shared/zedAuth.js`
//! (RSA native-app keypair, access-token decrypt, short-lived LLM token).
//!
//! Flow (bead .102 login + executor EXEC-13 share these primitives):
//! 1. `create_native_auth_data()` mints an RSA-2048 keypair and the
//!    `https://zed.dev/native_app_signin?native_app_port=…&native_app_public_key=…`
//!    URL. The private key travels through the OAuth codeVerifier slot as an
//!    opaque `zed-rsa-pkcs1:<base64url>` verifier.
//! 2. Zed redirects back to the local proxy with
//!    `?user_id=…&access_token=<RSA-encrypted>`; `parse_callback_payload()`
//!    + `decrypt_access_token()` (OAEP-SHA256, PKCS1-v1.5 fallback) recover the plaintext access token.
//! 3. `fetch_llm_token()` POSTs `{organization_id}` to
//!    `cloud.zed.dev/client/llm_tokens` with `${userId} ${accessToken}` auth
//!    to mint a 50-minute LLM bearer used by the executor.

use base64::Engine as _;
use once_cell::sync::Lazy;
use rand::rngs::OsRng;
use reqwest::header::AUTHORIZATION;
use rsa::pkcs1::{DecodeRsaPrivateKey, EncodeRsaPrivateKey, EncodeRsaPublicKey, LineEnding};
use rsa::{Oaep, Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use serde_json::Value;
use sha2_rsa_compat::Sha256;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const ZED_WEB_BASE_URL: &str = "https://zed.dev";
pub const ZED_CLOUD_BASE_URL: &str = "https://cloud.zed.dev";
/// JS ZED_HOSTED_CONFIG.defaultNativeAppPort.
pub const ZED_DEFAULT_NATIVE_APP_PORT: u16 = 58443;

/// JS ZED_HEADERS (zedAuth.js:19-28) — one definition so the completions path,
/// the /models path and the token-expiry check cannot drift apart.
pub const ZED_HEADER_EXPIRED_TOKEN: &str = "x-zed-expired-token";
pub const ZED_HEADER_OUTDATED_TOKEN: &str = "x-zed-outdated-token";
pub const ZED_HEADER_CLIENT_SUPPORTS_STATUS: &str = "x-zed-client-supports-status-messages";
pub const ZED_HEADER_CLIENT_SUPPORTS_STREAM_ENDED: &str =
    "x-zed-client-supports-stream-ended-request-completion-status";
pub const ZED_HEADER_SERVER_SUPPORTS_STATUS: &str = "x-zed-server-supports-status-messages";
pub const ZED_HEADER_CLIENT_SUPPORTS_XAI: &str = "x-zed-client-supports-x-ai";
pub const ZED_HEADER_SYSTEM_ID: &str = "x-zed-system-id";

const PRIVATE_KEY_PREFIX: &str = "zed-rsa-pkcs1:";
const LLM_TOKEN_TTL_SECS: u64 = 50 * 60;
const MODEL_CACHE_TTL_SECS: u64 = 60 * 60;

/// JS llmTokenCache (zedAuth.js:33). Zed bearers are good for 50 minutes, so
/// re-minting one per request is a wasted round-trip against cloud.zed.dev.
static LLM_TOKEN_CACHE: Lazy<Mutex<HashMap<String, (String, Instant)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// JS modelCache (zedAuth.js:34) — the live catalog, never hardcoded.
static MODEL_CACHE: Lazy<Mutex<HashMap<String, (ZedModelCatalog, Instant)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// JS modelInflight (zedAuth.js:35) — collapses a burst of concurrent catalog
/// fetches for one account into a single upstream request. A waiter that loses
/// the race re-checks the TTL cache and finds the winner's entry.
static MODEL_FLIGHT_LOCKS: Lazy<Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone)]
pub struct NativeAuthData {
    /// `https://zed.dev/native_app_signin?…` — open in a browser.
    pub auth_url: String,
    /// Opaque verifier carrying the encoded private key (codeVerifier slot).
    pub private_key_verifier: String,
    pub native_app_port: u16,
    pub system_id: String,
    /// Base64url DER public key handed to zed.dev.
    pub public_key: String,
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn from_b64url(value: &str) -> Vec<u8> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .unwrap_or_default()
}

/** Generate a fresh RSA keypair + the zed.dev native_app_signin URL for it. */
pub fn create_native_auth_data(
    native_app_port: Option<u16>,
    system_id: Option<String>,
) -> NativeAuthData {
    let mut rng = OsRng;
    let private_key =
        RsaPrivateKey::new(&mut rng, 2048).expect("RSA-2048 keygen cannot fail on OsRng");
    let public_key_der = private_key
        .to_public_key()
        .to_pkcs1_der()
        .expect("RSA pkcs1 DER encoding of a valid key cannot fail");

    let port = native_app_port.unwrap_or(ZED_DEFAULT_NATIVE_APP_PORT);
    let system_id = system_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let public_key_string = base64::engine::general_purpose::STANDARD
        .encode(public_key_der.as_bytes())
        .replace('+', "-")
        .replace('/', "_");

    let auth_url = format!(
        "{ZED_WEB_BASE_URL}/native_app_signin?native_app_port={port}&native_app_public_key={public_key_string}&system_id={system_id}"
    );

    let private_key_pem = private_key
        .to_pkcs1_pem(LineEnding::LF)
        .unwrap_or_default()
        .to_string();

    NativeAuthData {
        auth_url,
        private_key_verifier: encode_private_key_verifier(&private_key_pem),
        native_app_port: port,
        system_id,
        public_key: public_key_string,
    }
}

/** Encode a PEM private key as an opaque verifier (codeVerifier slot). */
pub fn encode_private_key_verifier(private_key_pem: &str) -> String {
    format!("{PRIVATE_KEY_PREFIX}{}", b64url(private_key_pem.as_bytes()))
}

pub fn decode_private_key_verifier(verifier: &str) -> Result<RsaPrivateKey, String> {
    let value = verifier.trim();
    let encoded = value
        .strip_prefix(PRIVATE_KEY_PREFIX)
        .ok_or("Missing Zed private key verifier; restart the login flow")?;
    let pem = String::from_utf8(from_b64url(encoded))
        .map_err(|_| "Zed private key verifier is not valid UTF-8")?;
    RsaPrivateKey::from_pkcs1_pem(&pem).map_err(|e| format!("invalid Zed private key: {e}"))
}

/// Parse the pasted native-app callback URL/JSON/query into userId +
/// encrypted token (JS parseZedCallbackPayload).
pub fn parse_callback_payload(input: &str) -> Result<(String, String), String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("Missing Zed callback URL".to_string());
    }

    let mut user_id: Option<String> = None;
    let mut encrypted: Option<String> = None;

    if let Ok(data) = serde_json::from_str::<Value>(raw) {
        user_id = data
            .get("user_id")
            .or_else(|| data.get("userId"))
            .and_then(Value::as_str)
            .map(String::from);
        encrypted = data
            .get("access_token")
            .or_else(|| data.get("accessToken"))
            .or_else(|| data.get("token"))
            .and_then(Value::as_str)
            .map(String::from);
    } else {
        // Query-string / path?query / partial-query form.
        let query_part = raw.split_once('?').map(|(_, q)| q).unwrap_or(raw);
        for pair in query_part.trim_start_matches('/').split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            match key {
                "user_id" | "userId" => user_id = Some(value.to_string()),
                "access_token" | "accessToken" | "token" => encrypted = Some(value.to_string()),
                _ => {}
            }
        }
    }

    match (user_id, encrypted) {
        (Some(u), Some(t)) if !u.is_empty() && !t.is_empty() => Ok((u, t)),
        _ => Err("Zed callback must include user_id and access_token".to_string()),
    }
}

/// Decrypt the RSA-encrypted access token (OAEP-SHA256, PKCS1-v1.5 fallback).
pub fn decrypt_access_token(
    encrypted_access_token: &str,
    private_key_verifier: &str,
) -> Result<String, String> {
    let private_key = decode_private_key_verifier(private_key_verifier)?;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
    // JS uses base64url for the encrypted blob.
    let encrypted = B64URL
        .decode(encrypted_access_token.trim())
        .or_else(|_| {
            base64::engine::general_purpose::STANDARD.decode(encrypted_access_token.trim())
        })
        .map_err(|e| format!("Zed access token is not valid base64: {e}"))?;

    if let Ok(plain) = private_key.decrypt(Oaep::new::<Sha256>(), &encrypted) {
        return String::from_utf8(plain)
            .map_err(|_| "decrypted Zed token is not UTF-8".to_string());
    }
    let plain = private_key
        .decrypt(Pkcs1v15Encrypt, &encrypted)
        .map_err(|e| format!("Failed to decrypt Zed access token: {e}"))?;
    String::from_utf8(plain).map_err(|_| "decrypted Zed token is not UTF-8".to_string())
}

/// Build the `${userId} ${accessToken}` cloud auth header (JS buildZedUserAuthHeader).
pub fn build_user_auth_header(user_id: &str, access_token: &str) -> Result<String, String> {
    if user_id.is_empty() || access_token.is_empty() {
        return Err("Zed credential is missing userId or accessToken".to_string());
    }
    Ok(format!("{user_id} {access_token}"))
}

/// Exchange the decrypted access token for a short-lived LLM bearer
/// (JS fetchZedLlmToken): POST /client/llm_tokens with the organization id.
pub async fn fetch_llm_token(
    client: &reqwest::Client,
    user_id: &str,
    access_token: &str,
    organization_id: &str,
    system_id: Option<&str>,
) -> Result<String, String> {
    if organization_id.is_empty() {
        return Err("No Zed organization selected".to_string());
    }
    let auth = build_user_auth_header(user_id, access_token)?;
    let mut request = client
        .post(format!("{ZED_CLOUD_BASE_URL}/client/llm_tokens"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("Authorization", auth)
        .json(&serde_json::json!({"organization_id": organization_id}));
    if let Some(sid) = system_id.filter(|s| !s.is_empty()) {
        request = request.header(ZED_HEADER_SYSTEM_ID, sid);
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("Zed llm_tokens request failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Zed llm_tokens returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let data: Value = response
        .json()
        .await
        .map_err(|e| format!("Zed llm_tokens response was not JSON: {e}"))?;
    let token = data
        .get("token")
        .and_then(|t| {
            t.as_str()
                .map(String::from)
                .or_else(|| t.get(0).and_then(Value::as_str).map(String::from))
                .or_else(|| t.get("value").and_then(Value::as_str).map(String::from))
        })
        .ok_or("Zed did not return an LLM token")?;
    Ok(token)
}

/// Cache key for one account's LLM bearer (JS zedUserCacheKey): user id,
/// organization, and the last 16 characters of the access token so a rotated
/// token never reads a stale entry.
pub fn zed_llm_cache_key(user_id: &str, organization_id: &str, access_token: &str) -> String {
    let org = if organization_id.is_empty() {
        "default"
    } else {
        organization_id
    };
    // JS `token.slice(-16)` indexes UTF-16 units; access tokens are ASCII, so a
    // byte tail is equivalent and cannot split a character.
    let tail = access_token
        .get(access_token.len().saturating_sub(16)..)
        .unwrap_or(access_token);
    format!("{user_id}:{org}:{tail}")
}

/// Cached form of `fetch_llm_token` (JS fetchZedLlmToken, which memoises for
/// `LLM_TOKEN_TTL_MS`). `force_refresh` skips the read *and* replaces the
/// entry — that is the path the 401 / expired-token retry takes.
pub async fn fetch_llm_token_cached(
    client: &reqwest::Client,
    user_id: &str,
    access_token: &str,
    organization_id: &str,
    system_id: Option<&str>,
    force_refresh: bool,
) -> Result<String, String> {
    let key = zed_llm_cache_key(user_id, organization_id, access_token);
    if !force_refresh {
        let fresh = LLM_TOKEN_CACHE
            .lock()
            .ok()
            .and_then(|cache| cache.get(&key).cloned())
            .filter(|(_, minted)| minted.elapsed() < Duration::from_secs(LLM_TOKEN_TTL_SECS))
            .map(|(token, _)| token);
        if let Some(token) = fresh {
            return Ok(token);
        }
    }
    let token = fetch_llm_token(client, user_id, access_token, organization_id, system_id).await?;
    if let Ok(mut cache) = LLM_TOKEN_CACHE.lock() {
        cache.insert(key, (token.clone(), Instant::now()));
    }
    Ok(token)
}

/// One resolved Zed model catalog (JS the `resolveZedModels` entry object).
#[derive(Debug, Clone)]
pub struct ZedModelCatalog {
    /// Mapped, non-disabled models only — this is what `/v1/models` lists.
    pub models: Vec<Value>,
    /// Every upstream entry keyed by normalized id, disabled ones included:
    /// resolving a model the user disabled still needs its `provider` field.
    pub raw_by_id: HashMap<String, Value>,
    pub default_model: String,
    pub default_fast_model: String,
    pub recommended_models: Vec<String>,
}

/// JS normalizeZedModelId — ids arrive as a string, a one-element array, or a
/// wrapper object depending on which upstream surface produced them.
pub fn normalize_zed_model_id(id: Option<&Value>) -> String {
    let Some(id) = id else { return String::new() };
    match id {
        Value::Null => String::new(),
        Value::String(s) if s.is_empty() => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(items) => match items.first().and_then(Value::as_str) {
            Some(first) => first.to_string(),
            None => id.to_string(),
        },
        Value::Object(map) => match map.get("id").and_then(Value::as_str) {
            Some(inner) => inner.to_string(),
            None => id.to_string(),
        },
        other => other.to_string(),
    }
}

/// JS mapZedModel — flatten one catalog entry into the shape the dashboard and
/// `/v1/models` consume. Returns `None` for an entry with no usable id.
pub fn map_zed_model(raw: &Value) -> Option<Value> {
    let id = normalize_zed_model_id(raw.get("id"));
    if id.is_empty() {
        return None;
    }
    let flag = |snake: &str, camel: &str| -> bool {
        raw.get(snake)
            .or_else(|| raw.get(camel))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    let field = |snake: &str, camel: &str| -> Value {
        raw.get(snake)
            .cloned()
            .unwrap_or_else(|| raw.get(camel).cloned().unwrap_or(Value::Null))
    };
    let name = raw
        .get("display_name")
        .or_else(|| raw.get("displayName"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(&id)
        .to_string();
    Some(serde_json::json!({
        "id": id,
        "name": name,
        "provider": raw.get("provider").cloned().unwrap_or(Value::Null),
        "isLatest": flag("is_latest", "isLatest"),
        "contextLength": field("max_token_count", "maxTokenCount"),
        "contextLengthInMaxMode": field("max_token_count_in_max_mode", "maxTokenCountInMaxMode"),
        "maxOutputTokens": field("max_output_tokens", "maxOutputTokens"),
        "supportsTools": flag("supports_tools", "supportsTools"),
        "supportsImages": flag("supports_images", "supportsImages"),
        "supportsThinking": flag("supports_thinking", "supportsThinking"),
        "supportsDisablingThinking": flag("supports_disabling_thinking", "supportsDisablingThinking"),
        "supportsFastMode": flag("supports_fast_mode", "supportsFastMode"),
        "supportsServerSideCompaction": flag(
            "supports_server_side_compaction", "supportsServerSideCompaction"),
        "supportedEffortLevels": field("supported_effort_levels", "supportedEffortLevels"),
        "supportsStreamingTools": flag("supports_streaming_tools", "supportsStreamingTools"),
        "supportsParallelToolCalls": flag("supports_parallel_tool_calls", "supportsParallelToolCalls"),
        "isDisabled": flag("is_disabled", "isDisabled"),
        "disabledReason": raw.get("disabled_reason").cloned().unwrap_or(Value::Null),
    }))
}

/// JS zedModelCacheKey — the catalog is per account, not per organization.
pub fn zed_model_cache_key(user_id: &str, organization_id: &str, access_token: &str) -> String {
    zed_llm_cache_key(user_id, organization_id, access_token)
}

/// Fetch the live model catalog (JS the inner `resolveZedModels` promise).
/// The bearer is minted through the same cache the completions path uses, so a
/// catalog read never costs an extra token mint.
async fn fetch_zed_models_catalog(
    client: &reqwest::Client,
    user_id: &str,
    access_token: &str,
    organization_id: &str,
    system_id: Option<&str>,
) -> Result<ZedModelCatalog, String> {
    if access_token.is_empty() {
        return Err("Zed credential is missing an access token".to_string());
    }
    let token = fetch_llm_token_cached(
        client,
        user_id,
        access_token,
        organization_id,
        system_id,
        false,
    )
    .await?;
    let mut request = client
        .get(format!("{ZED_CLOUD_BASE_URL}/models"))
        .header("Accept", "application/json")
        .header(AUTHORIZATION, format!("Bearer {token}"));
    if let Some(sid) = system_id.filter(|s| !s.is_empty()) {
        request = request.header(ZED_HEADER_SYSTEM_ID, sid);
    }
    // Zed only advertises the xAI-backed models when the client opts in
    // (zedAuth.js:369), so this header is required for the catalog to be
    // complete — unlike /completions, where 9router omits it.
    request = request.header(ZED_HEADER_CLIENT_SUPPORTS_XAI, "true");

    let response = request
        .send()
        .await
        .map_err(|e| format!("Zed models request failed: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(format!("Zed models failed: {} {text}", status.as_u16()));
    }
    let data: Value = response
        .json()
        .await
        .map_err(|e| format!("Zed models response was not JSON: {e}"))?;

    let raw_models = data
        .get("models")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let models: Vec<Value> = raw_models
        .iter()
        .filter_map(map_zed_model)
        .filter(|m| !m["isDisabled"].as_bool().unwrap_or(false))
        .collect();
    let mut raw_by_id = HashMap::new();
    for raw in &raw_models {
        let id = normalize_zed_model_id(raw.get("id"));
        if !id.is_empty() {
            raw_by_id.insert(id, raw.clone());
        }
    }
    let recommended = data
        .get("recommended_models")
        .or_else(|| data.get("recommendedModels"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| normalize_zed_model_id(Some(item)))
                .filter(|id| !id.is_empty())
                .collect()
        })
        .unwrap_or_default();

    Ok(ZedModelCatalog {
        models,
        raw_by_id,
        default_model: normalize_zed_model_id(
            data.get("default_model")
                .or_else(|| data.get("defaultModel")),
        ),
        default_fast_model: normalize_zed_model_id(
            data.get("default_fast_model")
                .or_else(|| data.get("defaultFastModel")),
        ),
        recommended_models: recommended,
    })
}

/// Resolve (and cache) the live Zed model catalog (JS resolveZedModels) —
/// "never hardcoded, always a live fetch". `force_refresh` bypasses both the
/// TTL cache and the de-duplication, so a caller that just saw a miss upstream
/// does not read the entry another request is still writing.
pub async fn resolve_zed_models(
    client: &reqwest::Client,
    user_id: &str,
    access_token: &str,
    organization_id: &str,
    system_id: Option<&str>,
    force_refresh: bool,
) -> Result<ZedModelCatalog, String> {
    let key = zed_model_cache_key(user_id, organization_id, access_token);
    if force_refresh {
        return fetch_and_cache_zed_models(
            client,
            &key,
            user_id,
            access_token,
            organization_id,
            system_id,
        )
        .await;
    }
    if let Some(catalog) = read_model_cache(&key) {
        return Ok(catalog);
    }

    let flight = {
        let mut locks = MODEL_FLIGHT_LOCKS
            .lock()
            .map_err(|_| "Zed model cache lock was poisoned")?;
        locks
            .entry(key.clone())
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let _guard = flight.lock().await;
    if let Some(catalog) = read_model_cache(&key) {
        return Ok(catalog);
    }
    fetch_and_cache_zed_models(
        client,
        &key,
        user_id,
        access_token,
        organization_id,
        system_id,
    )
    .await
}

fn read_model_cache(key: &str) -> Option<ZedModelCatalog> {
    MODEL_CACHE
        .lock()
        .ok()
        .and_then(|cache| cache.get(key).cloned())
        .filter(|(_, fetched)| fetched.elapsed() < Duration::from_secs(MODEL_CACHE_TTL_SECS))
        .map(|(catalog, _)| catalog)
}

async fn fetch_and_cache_zed_models(
    client: &reqwest::Client,
    key: &str,
    user_id: &str,
    access_token: &str,
    organization_id: &str,
    system_id: Option<&str>,
) -> Result<ZedModelCatalog, String> {
    let catalog =
        fetch_zed_models_catalog(client, user_id, access_token, organization_id, system_id).await?;
    if let Ok(mut cache) = MODEL_CACHE.lock() {
        cache.insert(key.to_string(), (catalog.clone(), Instant::now()));
    }
    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Seed a bearer under `key` with an explicit age, so the TTL boundary can
    /// be exercised without sleeping.
    fn seed_llm_token(key: &str, token: &str, age: Duration) {
        LLM_TOKEN_CACHE
            .lock()
            .expect("cache lock")
            .insert(key.to_string(), (token.to_string(), Instant::now() - age));
    }

    #[tokio::test]
    async fn cached_llm_token_is_served_without_a_network_call() {
        let key = zed_llm_cache_key("cache-user", "org", "abcdefghijklmnopqrstuvwxyz");
        seed_llm_token(&key, "sentinel-llm-token", Duration::from_secs(0));

        // A real mint would have to reach cloud.zed.dev; returning the sentinel
        // proves the cache short-circuited the request.
        assert_eq!(
            fetch_llm_token_cached(
                &reqwest::Client::new(),
                "cache-user",
                "abcdefghijklmnopqrstuvwxyz",
                "org",
                None,
                false,
            )
            .await,
            Ok("sentinel-llm-token".to_string())
        );
    }

    #[tokio::test]
    async fn llm_token_past_the_50_minute_ttl_is_not_served() {
        let key = zed_llm_cache_key("stale-user", "org", "tok");
        seed_llm_token(&key, "stale-llm-token", Duration::from_secs(51 * 60));

        // Past the TTL the cache must miss. The mint then fails on the empty
        // organization, which is a different failure from replaying the stale
        // bearer — that is what distinguishes "missed" from "served".
        assert_eq!(
            fetch_llm_token_cached(
                &reqwest::Client::new(),
                "stale-user",
                "tok",
                "",
                None,
                false,
            )
            .await,
            Err("No Zed organization selected".to_string())
        );
    }

    #[tokio::test]
    async fn force_refresh_bypasses_a_fresh_cache_entry() {
        let key = zed_llm_cache_key("force-user", "org", "tok");
        seed_llm_token(&key, "stale-llm-token", Duration::from_secs(0));

        assert_eq!(
            fetch_llm_token_cached(&reqwest::Client::new(), "force-user", "tok", "", None, true,)
                .await,
            Err("No Zed organization selected".to_string())
        );
    }

    #[test]
    fn keypair_roundtrip_and_decrypt_oaep() {
        let auth = create_native_auth_data(Some(58443), None);
        assert!(auth.private_key_verifier.starts_with(PRIVATE_KEY_PREFIX));
        assert!(auth.auth_url.contains("native_app_port=58443"));
        assert!(auth.auth_url.contains("native_app_public_key="));

        // Encrypt with the public half, decrypt via the verifier.
        let private_key = decode_private_key_verifier(&auth.private_key_verifier).unwrap();
        let public_key = private_key.to_public_key();
        use rand::rngs::OsRng;
        let mut rng = OsRng;
        let msg = b"zed-access-token-123";
        let encrypted = public_key
            .encrypt(&mut rng, Oaep::new::<Sha256>(), msg)
            .unwrap();
        let blob = b64url(&encrypted);

        let plain = decrypt_access_token(&blob, &auth.private_key_verifier).unwrap();
        assert_eq!(plain, "zed-access-token-123");
    }

    #[test]
    fn callback_payload_parses_query_json_and_bare_token() {
        let (uid, tok) = parse_callback_payload("/?user_id=u1&access_token=abc").unwrap();
        assert_eq!(uid, "u1");
        assert_eq!(tok, "abc");

        let (uid2, tok2) = parse_callback_payload(r#"{"userId": "u2", "token": "t2"}"#).unwrap();
        assert_eq!(uid2, "u2");
        assert_eq!(tok2, "t2");

        assert!(parse_callback_payload("").is_err());
        assert!(parse_callback_payload("?user_id=only").is_err());
    }

    #[test]
    fn pkcs1_fallback_decrypt_works() {
        let auth = create_native_auth_data(None, None);
        let private_key = decode_private_key_verifier(&auth.private_key_verifier).unwrap();
        let public_key = private_key.to_public_key();
        use rand::rngs::OsRng;
        let mut rng = OsRng;
        let encrypted = public_key
            .encrypt(&mut rng, Pkcs1v15Encrypt, b"legacy-token")
            .unwrap();
        let blob = b64url(&encrypted);
        assert_eq!(
            decrypt_access_token(&blob, &auth.private_key_verifier).unwrap(),
            "legacy-token"
        );
    }
}
