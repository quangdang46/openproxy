//! AES-256-CBC encryption for ProviderConnection sensitive fields,
//! plus SHA-256 checksum and schema version management.

use aes::cipher::{
    block_padding::Pkcs7, generic_array::GenericArray, BlockDecryptMut, BlockEncryptMut, KeyIvInit,
};
use anyhow::Context;
use base64::Engine;
use rand::Rng;
use sha2::{Digest, Sha256};

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
    // Never re-encrypt OpenProxy ciphertext
    if plain.starts_with(ENC_PREFIX) {
        *field = Some(plain);
        return;
    }
    // Legacy ciphertext without prefix: if it already decrypts, re-wrap with prefix
    if let Ok(decoded) = decrypt_value(key, &plain) {
        *field = Some(format!("{ENC_PREFIX}{}", encrypt_value(key, &decoded)));
        return;
    }
    *field = Some(format!("{ENC_PREFIX}{}", encrypt_value(key, &plain)));
}

fn decrypt_opt(field: &mut Option<String>, key: &str) {
    let Some(cipher) = field.take() else { return };

    let (is_marked, payload) = if let Some(rest) = cipher.strip_prefix(ENC_PREFIX) {
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

    match decrypt_value(key, &payload) {
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

/// Heuristic for legacy (unprefixed) AES blobs: long standard-base64, decodable, ≥ IV+block.
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
        Ok(raw) => raw.len() >= IV_LEN + 16,
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
        assert!(stored.starts_with(ENC_PREFIX));
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
        assert!(conn.api_key.as_ref().unwrap().starts_with(ENC_PREFIX));
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
}
