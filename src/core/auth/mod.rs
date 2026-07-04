pub mod cline_auth;
pub mod credential_manager;
pub mod machine_id;

use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;
use std::sync::OnceLock;

pub const CLI_TOKEN_HEADER: &str = "x-9r-cli-token";

type HmacSha256 = Hmac<Sha256>;

const DEFAULT_API_KEY_SECRET: &str = "endpoint-proxy-api-key-secret";

/// Returns the HMAC secret used for API key CRC generation, resolving from
/// the `API_KEY_SECRET` environment variable at first call with lazy caching.
/// Falls back to the static default when the env var is not set.
pub fn api_key_secret() -> &'static str {
    static SECRET: OnceLock<String> = OnceLock::new();
    SECRET.get_or_init(|| {
        std::env::var("API_KEY_SECRET").unwrap_or_else(|_| DEFAULT_API_KEY_SECRET.to_string())
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
fn timing_safe_eq(a: &str, b: &str) -> bool {
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
    hex::encode(mac.finalize().into_bytes())[..8].to_string()
}

#[cfg(test)]
mod tests {
    use super::{generate_api_key_with_machine, generate_crc, parse_api_key};

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
}
