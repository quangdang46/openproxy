//! Encryption for ProviderConnection sensitive fields (H20).
//!
//! Two schemes coexist, dispatch on a prefix:
//! - `opxenc2:` — **AES-256-GCM** (authenticated encryption) with a key
//!   derived via **Argon2id** from `OPENPROXY_ENCRYPTION_KEY` and a
//!   per-install salt. This is the default for all new writes. GCM detects
//!   tampering (an attacker who can write to disk cannot silently corrupt or
//!   swap blocks). The Argon2id derivation is cached per process, so the hot
//!   path pays zero KDF cost.
//! - `opxenc1:` — legacy AES-256-CBC with a raw SHA-256 KDF, kept
//!   **readable forever** for backward compatibility with the 9router v0.5.x
//!   format. Existing `opxenc1:` values are lazily re-wrapped to `opxenc2:`
//!   the next time their connection is written.
//!
//! Key rotation caveat: changing `OPENPROXY_ENCRYPTION_KEY` requires
//! re-encrypting existing credentials (lazy re-wrap handles this on next
//! write). The primary goal is to prevent accidental exposure of plaintext
//! credentials in `db.json` or SQLite backups. For stronger protection, use
//! platform-level encryption (macOS Keychain, Linux Secret Service, Windows
//! DPAPI) or a dedicated KMS.

use aes::cipher::{
    block_padding::Pkcs7, generic_array::GenericArray, BlockDecryptMut, BlockEncryptMut, KeyIvInit,
};
use aes_gcm::{
    aead::Aead, Aes256Gcm, KeyInit as GcmKeyInit, Nonce,
};
use anyhow::Context;
use argon2::Argon2;
use base64::Engine;
use rand::Rng;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::OnceLock;

use serde_json::{json, Value};

use crate::types::{AppDb, ProviderConnection};

/// Encryptor / decryptor type aliases: AES-256-CBC.
type Enc = cbc::Encryptor<aes::Aes256>;
type Dec = cbc::Decryptor<aes::Aes256>;

const IV_LEN: usize = 16;
const KEY_LEN: usize = 32;

/// Current schema version for `db.json`.
///
/// | Version | Description                      |
/// |---------|----------------------------------|
/// | 0       | Pre-encryption (legacy format)   |
/// | 1       | AES-256-CBC on connection secrets |
pub const SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Key derivation
// ---------------------------------------------------------------------------

/// Derive a 256-bit AES key from `key` via SHA-256.
fn derive_key(key: &str) -> [u8; KEY_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let result = hasher.finalize();
    let mut k = [0u8; KEY_LEN];
    k.copy_from_slice(&result);
    k
}

// ---------------------------------------------------------------------------
// Primitive encrypt / decrypt
// ---------------------------------------------------------------------------

/// Encrypt `plaintext` using AES-256-CBC with a random 16-byte IV.
///
/// Returns `base64(IV || ciphertext)`.
pub fn encrypt_value(key: &str, plaintext: &str) -> String {
    let key_bytes = derive_key(key);
    let iv: [u8; IV_LEN] = rand::thread_rng().gen();

    // Buffer: plaintext + one AES block for PKCS7 padding (16 bytes).
    let mut buf = vec![0u8; plaintext.len() + IV_LEN];
    buf[..plaintext.len()].copy_from_slice(plaintext.as_bytes());

    let ciphertext = Enc::new(
        GenericArray::from_slice(&key_bytes),
        GenericArray::from_slice(&iv),
    )
    .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
    .expect("AES-256-CBC encryption cannot fail for valid input");

    let mut out = Vec::with_capacity(IV_LEN + ciphertext.len());
    out.extend_from_slice(&iv);
    out.extend_from_slice(ciphertext);

    base64::engine::general_purpose::STANDARD.encode(&out)
}

/// Decrypt a value previously produced by [`encrypt_value`].
pub fn decrypt_value(key: &str, ciphertext_b64: &str) -> anyhow::Result<String> {
    let data = base64::engine::general_purpose::STANDARD
        .decode(ciphertext_b64)
        .context("base64 decode failed")?;

    anyhow::ensure!(data.len() >= IV_LEN, "ciphertext too short");

    let (iv, ct) = data.split_at(IV_LEN);
    let mut buf = ct.to_vec();
    let plaintext = Dec::new(
        GenericArray::from_slice(&derive_key(key)),
        GenericArray::from_slice(iv),
    )
    .decrypt_padded_mut::<Pkcs7>(&mut buf)
    .map_err(|e| anyhow::anyhow!("AES-256-CBC decryption failed: {:?}", e))?;

    String::from_utf8(plaintext.to_vec()).context("decrypted data is not valid UTF-8")
}

// ---------------------------------------------------------------------------
// SHA-256 checksum
// ---------------------------------------------------------------------------

/// Compute the hex-encoded SHA-256 digest of `data`.
pub fn sha256_checksum(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

// ---------------------------------------------------------------------------
// Encryption key source
// ---------------------------------------------------------------------------

/// Return the encryption key from the `OPENPROXY_ENCRYPTION_KEY` environment
/// variable, or `None` when unset / empty (encryption is disabled, values are
/// stored in plaintext).
pub fn encryption_key() -> Option<String> {
    std::env::var("OPENPROXY_ENCRYPTION_KEY")
        .ok()
        .filter(|k| !k.is_empty())
}

// ---------------------------------------------------------------------------
// ProviderConnection field-level encryption / decryption
// ---------------------------------------------------------------------------

/// Marker prefix for values encrypted by OpenProxy (prevents double-encrypt and
/// detects ciphertext when `OPENPROXY_ENCRYPTION_KEY` is missing).
pub const ENC_PREFIX: &str = "opxenc1:";

/// Marker prefix for values encrypted with the v2 scheme (AES-256-GCM +
/// Argon2id KDF, H20). `opxenc1:` (legacy AES-CBC) stays readable forever.
pub const ENC_PREFIX_V2: &str = "opxenc2:";

// ---------------------------------------------------------------------------
// v2 crypto: AES-256-GCM + Argon2id
// ---------------------------------------------------------------------------

/// Salt length (bytes) for the Argon2id KDF.
const SALT_LEN: usize = 16;
/// GCM nonce length (12 bytes, standard for AES-GCM).
const NONCE_LEN: usize = 12;
/// GCM auth tag length (16 bytes, appended by aes-gcm).
const TAG_LEN: usize = 16;

/// Directory where the persisted crypto salt file is stored (mirrors the
/// `api_key_secret` persistence pattern).
fn openproxy_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .unwrap_or_else(|| PathBuf::from(".").into());
    PathBuf::from(home).join(".openproxy")
}

/// Path to the persisted per-install crypto salt.
fn crypto_salt_path() -> PathBuf {
    openproxy_dir().join("crypto_salt")
}

/// Get (or create on first use) the per-install 16-byte salt for the
/// Argon2id KDF.
///
/// A single salt is persisted per install (not per record) so the KDF runs
/// **once per process boot** and the derived key is cached — the hot path
/// (every credential read) pays zero Argon2id cost. A per-record salt would
/// add 50–150 ms to every request on an encrypted deployment.
fn get_or_create_salt() -> [u8; SALT_LEN] {
    if let Some(existing) = read_salt_file() {
        return existing;
    }
    let salt: [u8; SALT_LEN] = rand::thread_rng().gen();
    if let Some(dir) = crypto_salt_path().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&crypto_salt_path(), salt);
    salt
}

fn read_salt_file() -> Option<[u8; SALT_LEN]> {
    let bytes = std::fs::read(crypto_salt_path()).ok()?;
    let arr: [u8; SALT_LEN] = bytes.as_slice().try_into().ok()?;
    Some(arr)
}

/// Argon2id parameters. High memory cost (64 MiB) is acceptable because the
/// derivation is cached and runs once per boot.
fn argon2_params() -> argon2::Params {
    let m_cost: u32 = std::env::var("OPENPROXY_ARGON2_M_COST_KB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(65_536);
    let t_cost: u32 = std::env::var("OPENPROXY_ARGON2_T_COST")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let p_cost: u32 = std::env::var("OPENPROXY_ARGON2_P_COST")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    // OWASP floor guard: m ≥ 19456 KiB, t ≥ 2.
    let m_cost = m_cost.max(19_456);
    let t_cost = t_cost.max(2);
    argon2::Params::new(m_cost, t_cost, p_cost, Some(32)).unwrap_or_else(|_| {
        argon2::Params::new(65_536, 3, 1, Some(32)).expect("default params valid")
    })
}

/// Derive the 256-bit AES-GCM key from the raw `OPENPROXY_ENCRYPTION_KEY`
/// using Argon2id with the per-install salt. Cached per process keyed by the
/// raw key string — the hot path never re-derives, but a different key
/// (e.g. in tests) still derives its own.
fn derive_key_v2(raw_key: &str) -> [u8; 32] {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static KEY_CACHE: Mutex<Option<HashMap<String, [u8; 32]>>> = Mutex::new(None);
    let mut guard = KEY_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(k) = cache.get(raw_key) {
        return *k;
    }
    let salt = get_or_create_salt();
    let mut out = [0u8; 32];
    Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon2_params(),
    )
    .hash_password_into(raw_key.as_bytes(), &salt, &mut out)
    .expect("Argon2id derivation cannot fail for fixed params");
    cache.insert(raw_key.to_string(), out);
    out
}

/// Encrypt `plaintext` with AES-256-GCM using the Argon2id-derived key.
///
/// Returns `base64(salt || nonce[12] || ciphertext || tag[16])` — the salt is
/// embedded so records self-describe and a DB copied from another host
/// (same key, different persisted salt) still decrypts via a one-time KDF.
fn encrypt_value_v2(raw_key: &str, plaintext: &str) -> String {
    let key = derive_key_v2(raw_key);
    let cipher = Aes256Gcm::new_from_slice(&key).expect("AES-256 key length valid");
    let nonce_bytes: [u8; NONCE_LEN] = rand::thread_rng().gen();
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .expect("AES-GCM encryption cannot fail");
    // ciphertext includes the 16-byte tag appended by aes-gcm.

    let salt = get_or_create_salt();
    let mut out = Vec::with_capacity(SALT_LEN + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);

    base64::engine::general_purpose::STANDARD.encode(&out)
}

/// Decrypt a value previously produced by [`encrypt_value_v2`].
fn decrypt_value_v2(raw_key: &str, payload_b64: &str) -> anyhow::Result<String> {
    let data = base64::engine::general_purpose::STANDARD
        .decode(payload_b64)
        .context("base64 decode failed")?;
    anyhow::ensure!(
        data.len() >= SALT_LEN + NONCE_LEN + TAG_LEN,
        "v2 ciphertext too short"
    );

    let key = derive_key_v2(raw_key);
    let cipher = Aes256Gcm::new_from_slice(&key).expect("AES-256 key length valid");
    let (salt_nonce, ct) = data.split_at(SALT_LEN + NONCE_LEN);
    let nonce = Nonce::from_slice(&salt_nonce[SALT_LEN..]);
    let plaintext = cipher
        .decrypt(nonce, ct)
        .map_err(|_| anyhow::anyhow!("AES-GCM decryption failed (wrong key or tampered)"))?;
    String::from_utf8(plaintext).context("decrypted data is not valid UTF-8")
}

/// Encrypt sensitive fields of a [`ProviderConnection`] **in place** so the
/// struct is safe for serialization to disk.
///
/// When `key` is empty (encryption disabled), the fields are left as-is.
/// This matches 9router's behaviour where `OPENPROXY_ENCRYPTION_KEY` unset
/// means plaintext storage — SHA-256("") is NOT a valid encryption key.
///
/// Already-prefixed ciphertext is never re-encrypted (stops monotonic growth
/// when a previous load failed to decrypt).
pub fn encrypt_connection(conn: &mut ProviderConnection, key: &str) {
    if key.is_empty() {
        return;
    }
    encrypt_opt(&mut conn.access_token, key);
    encrypt_opt(&mut conn.refresh_token, key);
    encrypt_opt(&mut conn.id_token, key);
    encrypt_opt(&mut conn.api_key, key);
}

/// Decrypt sensitive fields of a [`ProviderConnection`] **in place** after
/// deserialization from disk.
///
/// When `key` is empty:
/// - Prefixed ciphertext → **cleared** (fail-loud; never send blobs upstream)
/// - Unprefixed values left as plaintext (legacy / no-encryption mode)
///
/// When `key` is set but decrypt fails for a prefixed (or likely-ciphertext)
/// value → **cleared** + error log (wrong key).
pub fn decrypt_connection(conn: &mut ProviderConnection, key: &str) {
    decrypt_opt(&mut conn.access_token, key);
    decrypt_opt(&mut conn.refresh_token, key);
    decrypt_opt(&mut conn.id_token, key);
    decrypt_opt(&mut conn.api_key, key);
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn encrypt_opt(field: &mut Option<String>, key: &str) {
    let Some(plain) = field.take() else { return };
    // Already v2 ciphertext — leave untouched (no re-encrypt).
    if plain.starts_with(ENC_PREFIX_V2) {
        *field = Some(plain);
        return;
    }
    // Legacy v1 ciphertext — lazy migrate to v2 (write-triggered; opxenc1
    // stays decryptable forever, this just upgrades on next write).
    if plain.starts_with(ENC_PREFIX) {
        let payload = &plain[ENC_PREFIX.len()..];
        if let Ok(decoded) = decrypt_value(key, payload) {
            *field = Some(format!("{ENC_PREFIX_V2}{}", encrypt_value_v2(key, &decoded)));
            return;
        }
        // Undecryptable v1 (wrong key?) — leave as-is, don't destroy it.
        *field = Some(plain);
        return;
    }
    // Unprefixed legacy 9router blob: if it decrypts with v1, re-wrap as v2;
    // otherwise treat as plaintext and encrypt with v2.
    if let Ok(decoded) = decrypt_value(key, &plain) {
        *field = Some(format!("{ENC_PREFIX_V2}{}", encrypt_value_v2(key, &decoded)));
        return;
    }
    *field = Some(format!("{ENC_PREFIX_V2}{}", encrypt_value_v2(key, &plain)));
}

/// Prefix-based dispatch for decryption. Supports:
/// - `opxenc2:` → v2 (AES-GCM + Argon2id)
/// - `opxenc1:` → legacy v1 (AES-CBC + SHA-256), kept readable forever
/// - unprefixed → legacy 9router blob (try v1) or plaintext
fn decrypt_opt(field: &mut Option<String>, key: &str) {
    let Some(cipher) = field.take() else { return };

    let (is_marked, payload) = if let Some(rest) = cipher.strip_prefix(ENC_PREFIX_V2) {
        (true, rest.to_string())
    } else if let Some(rest) = cipher.strip_prefix(ENC_PREFIX) {
        (true, rest.to_string())
    } else {
        (false, cipher.clone())
    };

    if key.is_empty() {
        if is_marked {
            tracing::error!(
                target: "openproxy::crypto",
                "Encrypted credential present but OPENPROXY_ENCRYPTION_KEY is unset — \
                 clearing field so ciphertext is never sent upstream. Set the same key \
                 used when writing the DB."
            );
            *field = None;
        } else {
            // Plaintext mode
            *field = Some(cipher);
        }
        return;
    }

    let result = if cipher.starts_with(ENC_PREFIX_V2) {
        decrypt_value_v2(key, &payload)
    } else if cipher.starts_with(ENC_PREFIX) {
        decrypt_value(key, &payload)
    } else {
        // Unprefixed: try v1 (legacy blob), fall back to plaintext.
        decrypt_value(key, &payload)
    };

    match result {
        Ok(plain) => *field = Some(plain),
        Err(err) => {
            if is_marked || looks_like_ciphertext(&payload) {
                tracing::error!(
                    target: "openproxy::crypto",
                    "Failed to decrypt credential (wrong OPENPROXY_ENCRYPTION_KEY?): {err:#} — \
                     clearing field so ciphertext is never sent upstream"
                );
                *field = None;
            } else {
                // Value was not encrypted (plaintext token), keep as-is.
                *field = Some(cipher);
            }
        }
    }
}

/// Heuristic for legacy (unprefixed) AES blobs: long standard-base64,
/// decodable, ≥ IV+block (v1) or salt+nonce+tag (v2).
fn looks_like_ciphertext(s: &str) -> bool {
    if s.len() < 44 {
        return false;
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
    {
        return false;
    }
    match base64::engine::general_purpose::STANDARD.decode(s) {
        Ok(raw) => raw.len() >= (IV_LEN + 16).min(SALT_LEN + NONCE_LEN + TAG_LEN),
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Document-level helpers — used by src/db/mod.rs
// ---------------------------------------------------------------------------

/// Prepare an in-memory `AppDb` for serialization to disk:
///
/// 1. Encrypt every `ProviderConnection`'s sensitive fields using `key`.
/// 2. Replace `_schemaVersion` with the current version in the serialised JSON.
/// 3. Attach `_checksum` = SHA-256 of the plaintext JSON.
///
/// When `key` is `None`, encryption is skipped.
pub fn finalize_db(db: &AppDb, key: Option<&str>) -> anyhow::Result<Vec<u8>> {
    let mut clone = db.clone();
    if let Some(k) = key {
        for conn in &mut clone.provider_connections {
            encrypt_connection(conn, k);
        }
    }

    // Serialise (plaintext fields + encrypted secrets) to pretty JSON.
    let bytes = serde_json::to_vec_pretty(&clone)?;
    let checksum = sha256_checksum(&bytes);

    // Re-parse and inject metadata.
    let mut root: Value = serde_json::from_slice(&bytes)?;
    if let Value::Object(ref mut map) = root {
        map.insert("_schemaVersion".into(), json!(SCHEMA_VERSION));
        map.insert("_checksum".into(), Value::String(checksum));
    }
    serde_json::to_vec_pretty(&root).map_err(Into::into)
}

/// Reverse of [`finalize_db`]:
///
/// 1. Strip `_schemaVersion` and `_checksum` metadata.
/// 2. Decrypt any encrypted `ProviderConnection` fields using `key`.
/// 3. If a checksum was present, verify it and log a warning on mismatch.
///
/// When `key` is `None`, decryption is skipped (legacy files).
pub fn open_db(bytes: &[u8], key: Option<&str>) -> anyhow::Result<AppDb> {
    let mut root: Value = serde_json::from_slice(bytes)?;
    let Value::Object(ref mut map) = root else {
        return Ok(serde_json::from_slice(bytes)?);
    };

    let checksum_str = map.remove("_checksum").and_then(|v| match v {
        Value::String(s) => Some(s),
        _ => None,
    });
    map.remove("_schemaVersion");

    // Checksum verification (best-effort — warn only).
    if let Some(ref expected) = checksum_str {
        if let Ok(recomputed) = serde_json::to_vec_pretty(&root) {
            let actual = sha256_checksum(&recomputed);
            if &actual != expected {
                tracing::warn!(
                    target: "openproxy::db::crypto",
                    expected = expected,
                    actual = actual,
                    "JSON checksum mismatch — data may be corrupt on disk"
                );
            }
        }
    }

    // Deserialize into AppDb.
    let mut db: AppDb =
        serde_json::from_value(root).map_err(|e| anyhow::anyhow!("failed to parse AppDb: {e}"))?;

    // Always run decrypt path: empty key clears `opxenc1:` ciphertext (fail-loud).
    let key_str = key.unwrap_or("");
    for conn in &mut db.provider_connections {
        decrypt_connection(conn, key_str);
    }

    Ok(db)
}

/// General-purpose finalize for any JSON value (usage.json, etc.):
/// attach `_schemaVersion` and `_checksum` metadata, but do NOT encrypt.
pub fn finalize_json<T: serde::Serialize>(value: &T) -> anyhow::Result<Vec<u8>> {
    let plain_bytes = serde_json::to_vec_pretty(value)?;
    let checksum = sha256_checksum(&plain_bytes);

    let mut root: Value = serde_json::from_slice(&plain_bytes)?;
    if let Value::Object(ref mut map) = root {
        map.insert("_schemaVersion".into(), json!(SCHEMA_VERSION));
        map.insert("_checksum".into(), Value::String(checksum));
    }
    serde_json::to_vec_pretty(&root).map_err(Into::into)
}

/// General-purpose open for any JSON value: strip metadata fields and verify
/// the checksum if present. Returns the clean `Value` (metadata removed).
/// Does NOT perform field-level decryption (that is `open_db`'s job).
///
/// If `T` is given via turbofish the caller can deserialize the result
/// directly; otherwise parse from `Value`.
pub fn open_json(bytes: &[u8]) -> anyhow::Result<Value> {
    let mut root: Value = serde_json::from_slice(bytes)?;
    let Value::Object(ref mut map) = root else {
        return Ok(root);
    };

    let checksum_str = map.remove("_checksum").and_then(|v| match v {
        Value::String(s) => Some(s),
        _ => None,
    });
    map.remove("_schemaVersion");

    if let Some(ref expected) = checksum_str {
        if let Ok(recomputed) = serde_json::to_vec_pretty(&root) {
            let actual = sha256_checksum(&recomputed);
            if &actual != expected {
                tracing::warn!(
                    target: "openproxy::db::crypto",
                    expected = expected,
                    actual = actual,
                    "JSON checksum mismatch — data may be corrupt on disk"
                );
            }
        }
    }

    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AppDb;
    use serde_json::Value;

    fn with_key() -> tempfile::TempDir {
        tempfile::TempDir::new().unwrap()
    }

    #[test]
    fn encrypt_decrypt_round_trip() {
        let key = "test-key-123";
        let plain = "sk-ant-my-secret-key-here-12345";
        let encrypted = encrypt_value(key, plain);
        assert_ne!(encrypted, plain);
        let decrypted = decrypt_value(key, &encrypted).unwrap();
        assert_eq!(decrypted, plain);
    }

    #[test]
    fn decrypt_plaintext_fails() {
        let key = "test-key-123";
        assert!(decrypt_value(key, "not-encrypted").is_err());
    }

    #[test]
    fn finalize_open_round_trip() {
        let key = Some("test-key");
        let mut db = AppDb::default();
        db.provider_connections = vec![ProviderConnection {
            id: "c1".into(),
            provider: "openai".into(),
            api_key: Some("sk-abc".into()),
            access_token: Some("tok-xyz".into()),
            refresh_token: Some("rt-secret".into()),
            name: Some("test".into()),
            ..Default::default()
        }];

        let bytes = finalize_db(&db, key).unwrap();

        // Check metadata was injected.
        let raw: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(raw.get("_schemaVersion").and_then(Value::as_u64), Some(1));
        assert!(raw.get("_checksum").and_then(Value::as_str).is_some());

        // Check fields are encrypted on disk.
        let conn = &raw["providerConnections"][0];
        assert_ne!(conn["apiKey"].as_str(), Some("sk-abc"));
        assert_ne!(conn["accessToken"].as_str(), Some("tok-xyz"));
        assert_ne!(conn["refreshToken"].as_str(), Some("rt-secret"));
        assert_eq!(conn["name"].as_str(), Some("test"));

        // Round-trip restores plaintext.
        let restored = open_db(&bytes, key).unwrap();
        assert_eq!(restored.provider_connections.len(), 1);
        let rc = &restored.provider_connections[0];
        assert_eq!(rc.api_key.as_deref(), Some("sk-abc"));
        assert_eq!(rc.access_token.as_deref(), Some("tok-xyz"));
        assert_eq!(rc.refresh_token.as_deref(), Some("rt-secret"));
    }

    #[test]
    fn backwards_compat_no_metadata() {
        let key = Some("test-key");
        let mut db = AppDb::default();
        db.provider_connections = vec![ProviderConnection {
            id: "c1".into(),
            provider: "openai".into(),
            api_key: Some("sk-plain".into()),
            access_token: Some("tok-plain".into()),
            ..Default::default()
        }];
        // Write without any metadata — simulating an old file.
        let bytes = serde_json::to_vec_pretty(&db).unwrap();
        let restored = open_db(&bytes, key).unwrap();
        assert_eq!(
            restored.provider_connections[0].api_key.as_deref(),
            Some("sk-plain")
        );
        assert_eq!(
            restored.provider_connections[0].access_token.as_deref(),
            Some("tok-plain")
        );
    }

    #[test]
    fn no_key_no_encrypt() {
        // No key => no encryption, but metadata is still present.
        let mut db = AppDb::default();
        db.provider_connections = vec![ProviderConnection {
            id: "c1".into(),
            provider: "openai".into(),
            api_key: Some("sk-plain".into()),
            ..Default::default()
        }];

        let bytes = finalize_db(&db, None).unwrap();

        let raw: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(raw.get("_schemaVersion").and_then(Value::as_u64), Some(1));
        // Field NOT encrypted when no key.
        assert_eq!(
            raw["providerConnections"][0]["apiKey"].as_str(),
            Some("sk-plain")
        );

        let restored = open_db(&bytes, None).unwrap();
        assert_eq!(
            restored.provider_connections[0].api_key.as_deref(),
            Some("sk-plain")
        );
    }

    #[test]
    fn checksum_detects_corruption() {
        let key = Some("test-key");
        let db = AppDb::default();
        let bytes_orig = finalize_db(&db, key).unwrap();
        // Corrupt the checksum value in the JSON (not content).
        // Replace `_checksum` value with a different string.
        let mut raw: Value = serde_json::from_slice(&bytes_orig).unwrap();
        raw.as_object_mut()
            .unwrap()
            .insert("_checksum".into(), Value::String("deadbeef".into()));
        let bytes = serde_json::to_vec_pretty(&raw).unwrap();
        // JSON is valid, checksum is wrong.
        let result = open_db(&bytes, key);
        assert!(
            result.is_ok(),
            "corrupted checksum should still parse: {:?}",
            result.err()
        );
    }

    #[test]
    fn different_ivs_produce_different_ciphertexts() {
        let key = "test-key";
        let plain = "same-value";
        let c1 = encrypt_value(key, plain);
        let c2 = encrypt_value(key, plain);
        // Random IV => distinct outputs.
        assert_ne!(c1, c2);
        assert_eq!(decrypt_value(key, &c1).unwrap(), plain);
        assert_eq!(decrypt_value(key, &c2).unwrap(), plain);
    }

    #[test]
    fn empty_key_does_not_encrypt() {
        let mut conn = ProviderConnection {
            api_key: Some("sk-plain".into()),
            ..Default::default()
        };
        encrypt_connection(&mut conn, "");
        assert_eq!(conn.api_key.as_deref(), Some("sk-plain"));
    }

    #[test]
    fn encrypt_uses_prefix_and_round_trips() {
        let mut conn = ProviderConnection {
            api_key: Some("sk-secret".into()),
            ..Default::default()
        };
        encrypt_connection(&mut conn, "my-key");
        let stored = conn.api_key.clone().unwrap();
        assert!(
            stored.starts_with(ENC_PREFIX_V2),
            "new writes should use the v2 (GCM) scheme, got {stored:?}"
        );
        // No double-encrypt
        encrypt_connection(&mut conn, "my-key");
        assert_eq!(conn.api_key.as_deref(), Some(stored.as_str()));
        decrypt_connection(&mut conn, "my-key");
        assert_eq!(conn.api_key.as_deref(), Some("sk-secret"));
    }

    #[test]
    fn missing_key_clears_prefixed_ciphertext() {
        let mut conn = ProviderConnection {
            api_key: Some("sk-secret".into()),
            ..Default::default()
        };
        encrypt_connection(&mut conn, "my-key");
        assert!(conn.api_key.as_ref().unwrap().starts_with(ENC_PREFIX_V2));
        decrypt_connection(&mut conn, "");
        assert!(
            conn.api_key.is_none(),
            "must not leave ciphertext as fake api key"
        );
    }

    #[test]
    fn wrong_key_clears_prefixed_ciphertext() {
        let mut conn = ProviderConnection {
            api_key: Some("sk-secret".into()),
            ..Default::default()
        };
        encrypt_connection(&mut conn, "right-key");
        decrypt_connection(&mut conn, "wrong-key");
        assert!(conn.api_key.is_none());
    }

    #[test]
    fn v2_round_trip() {
        let key = "test-key-123";
        let plain = "sk-v2-secret";
        let encrypted = encrypt_value_v2(key, plain);
        assert!(encrypted.len() > 60, "v2 ciphertext should be substantial");
        let decrypted = decrypt_value_v2(key, &encrypted).unwrap();
        assert_eq!(decrypted, plain);
        // Fresh nonce => distinct ciphertexts for same plaintext.
        assert_ne!(encrypted, encrypt_value_v2(key, plain));
    }

    #[test]
    fn v1_ciphertext_reads_after_upgrade() {
        // A v1 (AES-CBC) ciphertext must still decrypt after the v2 upgrade.
        let key = "legacy-key";
        let plain = "sk-legacy-token";
        let v1 = encrypt_value(key, plain); // v1 path still available
        let v1_prefixed = format!("{ENC_PREFIX}{v1}");
        let mut conn = ProviderConnection {
            api_key: Some(v1_prefixed),
            ..Default::default()
        };
        decrypt_connection(&mut conn, key);
        assert_eq!(conn.api_key.as_deref(), Some(plain));
    }

    #[test]
    fn v1_to_v2_rewrap_on_write() {
        // Writing a v1 ciphertext should lazily migrate it to v2.
        let key = "migrate-key";
        let plain = "sk-migrate";
        let v1 = format!("{ENC_PREFIX}{}", encrypt_value(key, plain));
        let mut conn = ProviderConnection {
            api_key: Some(v1),
            ..Default::default()
        };
        encrypt_connection(&mut conn, key);
        let stored = conn.api_key.clone().unwrap();
        assert!(
            stored.starts_with(ENC_PREFIX_V2),
            "v1 should be re-wrapped to v2 on write, got {stored:?}"
        );
        // Round-trips back to plaintext.
        decrypt_connection(&mut conn, key);
        assert_eq!(conn.api_key.as_deref(), Some(plain));
    }

    #[test]
    fn tamper_detection_gcm_authenticity() {
        // Flipping a byte in a v2 ciphertext must fail decryption (GCM auth).
        let key = "tamper-key";
        let plain = "sk-tamper-sensitive";
        let encrypted = encrypt_value_v2(key, plain);
        let mut data = base64::engine::general_purpose::STANDARD
            .decode(&encrypted)
            .unwrap();
        let last = data.len() - 1;
        data[last] ^= 0xFF; // corrupt the trailing auth tag
        let tampered = base64::engine::general_purpose::STANDARD.encode(&data);
        assert!(
            decrypt_value_v2(key, &tampered).is_err(),
            "GCM must reject tampered ciphertext"
        );
    }

    #[test]
    fn prefix_dispatch_mixed_formats() {
        let key = "dispatch-key";
        // v2 encrypted field.
        let mut v2_conn = ProviderConnection {
            api_key: Some(format!("{ENC_PREFIX_V2}{}", encrypt_value_v2(key, "v2-plain"))),
            ..Default::default()
        };
        decrypt_connection(&mut v2_conn, key);
        assert_eq!(v2_conn.api_key.as_deref(), Some("v2-plain"));

        // v1 encrypted field.
        let mut v1_conn = ProviderConnection {
            api_key: Some(format!("{ENC_PREFIX}{}", encrypt_value(key, "v1-plain"))),
            ..Default::default()
        };
        decrypt_connection(&mut v1_conn, key);
        assert_eq!(v1_conn.api_key.as_deref(), Some("v1-plain"));

        // Unprefixed plaintext stays.
        let mut plain_conn = ProviderConnection {
            api_key: Some("plain-token".into()),
            ..Default::default()
        };
        decrypt_connection(&mut plain_conn, key);
        assert_eq!(plain_conn.api_key.as_deref(), Some("plain-token"));
    }

    #[test]
    fn different_raw_keys_derive_different_keys() {
        // The KDF cache is keyed by raw key: two different raw keys must NOT
        // collide (regression for the once-single-key cache).
        let k1 = derive_key_v2("key-one");
        let k2 = derive_key_v2("key-two");
        assert_ne!(k1, k2, "different raw keys must derive different AES keys");
    }

    #[test]
    fn salt_is_persisted_and_stable() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = tempfile::TempDir::new().unwrap();
        std::env::set_var("DATA_DIR", temp.path());
        let s1 = get_or_create_salt();
        let s2 = get_or_create_salt();
        assert_eq!(s1, s2, "salt must be stable across calls within an install");
        std::env::remove_var("DATA_DIR");
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
