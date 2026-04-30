use hmac::{Hmac, Mac};
use sha2::Sha256;

pub const API_KEY_SECRET: &str = "endpoint-proxy-api-key-secret";
pub const CLI_TOKEN_HEADER: &str = "x-9r-cli-token";

type HmacSha256 = Hmac<Sha256>;

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
        if crc != expected_crc {
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

fn generate_crc(machine_id: &str, key_id: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(API_KEY_SECRET.as_bytes()).expect("static HMAC key");
    mac.update(machine_id.as_bytes());
    mac.update(key_id.as_bytes());
    hex::encode(mac.finalize().into_bytes())[..8].to_string()
}

#[cfg(test)]
mod tests {
    use super::{generate_crc, parse_api_key};

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
}
