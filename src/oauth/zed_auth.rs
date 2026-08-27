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
use rand::rngs::OsRng;
use rsa::pkcs1::{DecodeRsaPrivateKey, EncodeRsaPrivateKey, EncodeRsaPublicKey, LineEnding};
use rsa::{Oaep, Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use serde_json::Value;
use sha2_rsa_compat::Sha256;

pub const ZED_WEB_BASE_URL: &str = "https://zed.dev";
pub const ZED_CLOUD_BASE_URL: &str = "https://cloud.zed.dev";
/// JS ZED_HOSTED_CONFIG.defaultNativeAppPort.
pub const ZED_DEFAULT_NATIVE_APP_PORT: u16 = 58443;
const PRIVATE_KEY_PREFIX: &str = "zed-rsa-pkcs1:";
const LLM_TOKEN_TTL_SECS: i64 = 50 * 60;

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
        request = request.header("x-zed-system-id", sid);
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

#[cfg(test)]
mod tests {
    use super::*;

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
