pub mod cline_auth;
pub mod credential_manager;
pub mod machine_id;

use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;
use std::path::PathBuf;
use std::sync::OnceLock;

pub const CLI_TOKEN_HEADER: &str = "x-9r-cli-token";

type HmacSha256 = Hmac<Sha256>;

/// Directory where the persisted API-key HMAC secret file is stored.
fn openproxy_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .unwrap_or_else(|| PathBuf::from(".").into());
    PathBuf::from(home).join(".openproxy")
}

/// Path to the persisted API-key HMAC secret file.
fn api_key_secret_path() -> PathBuf {
    openproxy_dir().join("api_key_secret")
}

/// Generate a fresh random secret: 32 random bytes hex-encoded (64 chars).
fn generate_random_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Read a persisted secret from disk, if present and non-empty.
fn read_persisted_secret() -> Option<String> {
    std::fs::read_to_string(api_key_secret_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Persist a freshly generated secret so it stays stable across restarts.
fn persist_secret(secret: &str) {
    if let Some(dir) = api_key_secret_path().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(api_key_secret_path(), secret);
}

/// Returns the HMAC secret used for API key CRC generation.
///
/// Resolution order at first call (then cached for the process lifetime):
/// 1. `API_KEY_SECRET` environment variable, when set.
/// 2. A per-install random secret persisted at `$DATA_DIR/api_key_secret`
///    (or `~/.openproxy/api_key_secret`). Generated on first use so there is
///    never a well-known fallback; the persisted value keeps existing API
///    keys valid across restarts.
pub fn api_key_secret() -> &'static str {
    static SECRET: OnceLock<String> = OnceLock::new();
    SECRET.get_or_init(|| {
        if let Ok(v) = std::env::var("API_KEY_SECRET") {
            if !v.trim().is_empty() {
                return v;
            }
        }
        if let Some(persisted) = read_persisted_secret() {
            return persisted;
        }
        let fresh = generate_random_secret();
        persist_secret(&fresh);
        fresh
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthContext {
    pub provider: String,
    pub machine_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedApiKey {
    pub machine_id: Option<String>,
    pub key_id: String,
    pub is_new_format: bool,
}

/// Compares two strings in constant time (no early-exit on mismatch).
/// Both strings must be the same length; if lengths differ, returns false
/// without leaking which byte differed.
pub fn timing_safe_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut diff: u8 = 0;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

pub fn parse_api_key(api_key: &str) -> Option<ParsedApiKey> {
    if !api_key.starts_with("sk-") {
        return None;
    }

    let parts: Vec<_> = api_key.split('-').collect();
    if parts.len() == 4 {
        let machine_id = parts[1];
        let key_id = parts[2];
        let crc = parts[3];
        let expected_crc = generate_crc(machine_id, key_id);
        if !timing_safe_eq(crc, &expected_crc) {
            return None;
        }

        return Some(ParsedApiKey {
            machine_id: Some(machine_id.to_string()),
            key_id: key_id.to_string(),
            is_new_format: true,
        });
    }

    if parts.len() == 2 {
        return Some(ParsedApiKey {
            machine_id: None,
            key_id: parts[1].to_string(),
            is_new_format: false,
        });
    }

    None
}

pub fn generate_api_key_with_machine(machine_id: &str) -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

    let mut rng = rand::thread_rng();
    let key_id: String = (0..6)
        .map(|_| {
            let index = rng.gen_range(0..CHARS.len());
            CHARS[index] as char
        })
        .collect();
    let crc = generate_crc(machine_id, &key_id);

    format!("sk-{machine_id}-{key_id}-{crc}")
}

fn generate_crc(machine_id: &str, key_id: &str) -> String {
    let key = api_key_secret();
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC key");
    mac.update(machine_id.as_bytes());
    mac.update(key_id.as_bytes());
    hex::encode(mac.finalize().into_bytes())[..12].to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        api_key_secret, generate_api_key_with_machine, generate_crc, generate_random_secret,
        parse_api_key, persist_secret, read_persisted_secret,
    };

    #[test]
    fn parse_api_key_accepts_new_and_old_formats() {
        let crc = generate_crc("machine1", "key01");
        let new_key = format!("sk-machine1-key01-{crc}");

        assert_eq!(
            parse_api_key(&new_key),
            Some(super::ParsedApiKey {
                machine_id: Some("machine1".into()),
                key_id: "key01".into(),
                is_new_format: true,
            })
        );

        assert_eq!(
            parse_api_key("sk-legacy01"),
            Some(super::ParsedApiKey {
                machine_id: None,
                key_id: "legacy01".into(),
                is_new_format: false,
            })
        );
    }

    #[test]
    fn parse_api_key_rejects_bad_crc_and_invalid_shapes() {
        assert!(parse_api_key("sk-machine-key01-deadbeef").is_none());
        assert!(parse_api_key("not-a-key").is_none());
        assert!(parse_api_key("sk-too-many-parts-extra-here").is_none());
    }

    #[test]
    fn generate_api_key_with_machine_matches_parser() {
        let key = generate_api_key_with_machine("machine1");
        let parsed = parse_api_key(&key).expect("generated key should parse");

        assert_eq!(parsed.machine_id.as_deref(), Some("machine1"));
        assert_eq!(parsed.key_id.len(), 6);
        assert!(parsed
            .key_id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit()));
    }

    #[test]
    fn random_secret_is_hex_and_round_trips_through_persistence() {
        let secret = generate_random_secret();
        // 32 random bytes → 64 hex chars.
        assert_eq!(secret.len(), 64);
        assert!(secret.chars().all(|c| c.is_ascii_hexdigit()));
        // Must not be the old well-known default.
        assert_ne!(secret, "endpoint-proxy-api-key-secret");

        // Persist/read round trip (in a temp dir via DATA_DIR).
        let temp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("DATA_DIR", temp.path());
        persist_secret(&secret);
        let read = read_persisted_secret().expect("persisted secret readable");
        assert_eq!(read, secret);
        std::env::remove_var("DATA_DIR");
    }

    #[test]
    fn api_key_secret_prefers_env_over_persisted() {
        // Env var wins; no file access needed.
        std::env::set_var("API_KEY_SECRET", "env-var-secret");
        assert_eq!(api_key_secret(), "env-var-secret");
        std::env::remove_var("API_KEY_SECRET");
    }
}
