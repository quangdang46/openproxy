//! Port of `open-sse/utils/cursorChecksum.js`.
//!
//! Generates the `x-cursor-checksum` header used for Cursor API auth, plus
//! the surrounding header bundle. Implements the "Jyh cipher" from the
//! upstream Cursor IDE: a small XOR + base64 obfuscation over a downscaled
//! timestamp.
//!
//! The JS source uses 32-bit-coerced shifts (`>> 40`, `>> 32`) on a number
//! that is always < 2^32, so the upper 16 bits of the 48-bit byte array
//! are always zero in practice. We replicate that exact behaviour.

use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// SHA-256 over `input + salt`, hex-encoded (64 chars).
pub fn generate_hashed64_hex(input: &str, salt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hasher.update(salt.as_bytes());
    hex::encode(hasher.finalize())
}

/// UUID v5 of `auth_token` under the DNS namespace.
pub fn generate_session_id(auth_token: &str) -> String {
    Uuid::new_v5(&Uuid::NAMESPACE_DNS, auth_token.as_bytes())
        .as_hyphenated()
        .to_string()
}

const URL_SAFE_ALPHABET: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Generate the Cursor checksum.
///
/// Algorithm (matches upstream JS exactly):
/// 1. `t = floor(unix_ms / 1_000_000)` (this is < 2^32 in 2026+).
/// 2. Pack into 6 big-endian bytes (upper 2 are always 0 in practice).
/// 3. Jyh cipher: `b[i] = ((b[i] ^ t) + (i % 256)) & 0xFF; t = b[i]`.
/// 4. URL-safe base64 (no padding, no leading sentinel byte).
/// 5. Append the machine id verbatim.
pub fn generate_cursor_checksum(machine_id: &str) -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0) as u64;
    let downscaled = now_ms / 1_000_000;
    // The JS source coerces the value to 32-bit before each shift; for the
    // upper bytes that yields zero, but we replicate the lower bytes
    // faithfully. Keep the masking explicit for clarity.
    let mut bytes: [u8; 6] = [
        ((downscaled >> 40) & 0xFF) as u8,
        ((downscaled >> 32) & 0xFF) as u8,
        ((downscaled >> 24) & 0xFF) as u8,
        ((downscaled >> 16) & 0xFF) as u8,
        ((downscaled >> 8) & 0xFF) as u8,
        (downscaled & 0xFF) as u8,
    ];

    let mut t: u8 = 165;
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = ((*b ^ t).wrapping_add((i % 256) as u8)) & 0xFF;
        t = *b;
    }

    let mut encoded = String::with_capacity(8);
    let mut i = 0;
    while i < bytes.len() {
        let a = bytes[i];
        let b = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
        let c = if i + 2 < bytes.len() { bytes[i + 2] } else { 0 };

        encoded.push(URL_SAFE_ALPHABET[(a >> 2) as usize] as char);
        encoded.push(URL_SAFE_ALPHABET[(((a & 0b11) << 4) | (b >> 4)) as usize] as char);
        if i + 1 < bytes.len() {
            encoded
                .push(URL_SAFE_ALPHABET[(((b & 0b1111) << 2) | (c >> 6)) as usize] as char);
        }
        if i + 2 < bytes.len() {
            encoded.push(URL_SAFE_ALPHABET[(c & 0b111111) as usize] as char);
        }
        i += 3;
    }

    format!("{encoded}{machine_id}")
}

/// Bundle of headers Cursor API expects on a chat request.
#[derive(Debug, Clone)]
pub struct CursorHeaders {
    pub authorization: String,
    pub session_id: String,
    pub client_key: String,
    pub checksum: String,
    pub machine_id: String,
    pub os: &'static str,
    pub arch: &'static str,
    pub ghost_mode: bool,
}

/// Build the headers Cursor API expects on a chat request. Matches
/// `buildCursorHeaders` in 9router. If `machine_id` is `None` it is
/// derived deterministically from the cleaned token.
pub fn build_cursor_headers(
    access_token: &str,
    machine_id: Option<&str>,
    ghost_mode: bool,
) -> CursorHeaders {
    let clean_token = match access_token.split_once("::") {
        Some((_, after)) => after,
        None => access_token,
    };

    let effective_machine_id = match machine_id {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => generate_hashed64_hex(clean_token, "machineId"),
    };

    let session_id = generate_session_id(clean_token);
    let client_key = generate_hashed64_hex(clean_token, "");
    let checksum = generate_cursor_checksum(&effective_machine_id);

    let os = match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "macos",
        _ => "linux",
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        _ => "x64",
    };

    CursorHeaders {
        authorization: format!("Bearer {clean_token}"),
        session_id,
        client_key,
        checksum,
        machine_id: effective_machine_id,
        os,
        arch,
        ghost_mode,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashed64_is_deterministic() {
        let a = generate_hashed64_hex("token", "salt");
        let b = generate_hashed64_hex("token", "salt");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert_ne!(a, generate_hashed64_hex("token", "different-salt"));
    }

    #[test]
    fn session_id_is_uuid_v5_dns() {
        let id = generate_session_id("my-token");
        assert_eq!(id.len(), 36);
        // v5 → version nibble is 5
        assert_eq!(id.as_bytes()[14], b'5');
    }

    #[test]
    fn checksum_appends_machine_id() {
        let cs = generate_cursor_checksum("MID-XYZ");
        assert!(cs.ends_with("MID-XYZ"));
    }

    #[test]
    fn build_cursor_headers_strips_token_prefix() {
        let h = build_cursor_headers("ID::abc123", None, true);
        assert_eq!(h.authorization, "Bearer abc123");
        assert!(h.machine_id.len() == 64);
    }
}
