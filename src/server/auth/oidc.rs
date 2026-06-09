//! OIDC SSO client.
//!
//! Implements the Authorization Code with PKCE flow per RFC 7636 against
//! any spec-compliant OpenID Connect provider (Google, Okta, Auth0,
//! Keycloak, Azure AD, …). The handshake is:
//!
//! 1. Browser hits `GET /api/auth/oidc/login` → server generates
//!    `state`, `nonce`, and a PKCE verifier/challenge, stashes them in
//!    short-lived HttpOnly cookies, and 302-redirects to the IdP's
//!    `authorization_endpoint`.
//! 2. IdP authenticates the user, then 302-redirects back to
//!    `GET /api/auth/oidc/callback?code=…&state=…`.
//! 3. Server verifies `state`, exchanges the code at the IdP's
//!    `token_endpoint`, fetches the JWKS, and verifies the signed
//!    `id_token` (RS256 by default, RS384/RS512 also accepted). On
//!    success the server issues a dashboard session cookie and 302-
//!    redirects to `/`.
//!
//! `OidcClient::discover` fetches the provider's
//! `/.well-known/openid-configuration` document once at boot. The
//! discovered endpoints are cached for the lifetime of the process.

use std::time::Duration;

use base64::Engine;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use rand::RngCore;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Errors produced by [`OidcClient`]. Each variant maps onto a single
/// failure mode the caller can act on (retry, log, return 502, etc.).
#[derive(Debug, Error)]
pub enum OidcError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("discovery document missing required field: {0}")]
    DiscoveryMissingField(&'static str),

    #[error("JWKS missing required field on key {kid}: {field}")]
    JwksMissingField { kid: String, field: &'static str },

    #[error("no JWK in JWKS matches id_token kid={0:?}")]
    JwksNoMatchingKid(Option<String>),

    #[error("unsupported JWK kty: {0}")]
    UnsupportedKty(String),

    #[error("invalid id_token: {0}")]
    InvalidIdToken(String),

    #[error("id_token nonce mismatch")]
    NonceMismatch,
}

/// Authenticated OIDC client. Construct via [`OidcClient::discover`].
#[derive(Debug, Clone)]
pub struct OidcClient {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
}

impl OidcClient {
    /// Fetch the IdP's discovery document and construct an [`OidcClient`].
    pub async fn discover(
        issuer: &str,
        client_id: &str,
        client_secret: &str,
        redirect_uri: &str,
    ) -> Result<Self, OidcError> {
        let issuer = issuer.trim_end_matches('/');
        let client = reqwest::Client::builder()
            .timeout(DISCOVERY_TIMEOUT)
            .build()?;
        let url = format!("{issuer}/.well-known/openid-configuration");
        let resp = client.get(&url).send().await?.error_for_status()?;
        let doc: Value = resp.json().await?;

        let pick_string = |key: &'static str| -> Result<String, OidcError> {
            doc.get(key)
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or(OidcError::DiscoveryMissingField(key))
        };

        Ok(Self {
            issuer: issuer.to_string(),
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            redirect_uri: redirect_uri.to_string(),
            scopes: vec!["openid".into(), "profile".into(), "email".into()],
            authorization_endpoint: pick_string("authorization_endpoint")?,
            token_endpoint: pick_string("token_endpoint")?,
            jwks_uri: pick_string("jwks_uri")?,
        })
    }

    /// Construct an [`OidcClient`] directly from already-known endpoints
    /// (skips the discovery round-trip). Useful in tests and in callers
    /// that have already cached the discovery document.
    pub fn from_endpoints(
        issuer: &str,
        client_id: &str,
        client_secret: &str,
        redirect_uri: &str,
        authorization_endpoint: String,
        token_endpoint: String,
        jwks_uri: String,
    ) -> Self {
        Self {
            issuer: issuer.trim_end_matches('/').to_string(),
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            redirect_uri: redirect_uri.to_string(),
            scopes: vec!["openid".into(), "profile".into(), "email".into()],
            authorization_endpoint,
            token_endpoint,
            jwks_uri,
        }
    }

    /// Build the `authorization_endpoint` URL with all required PKCE
    /// parameters appended.
    pub fn build_authorize_url(&self, state: &str, nonce: &str, code_challenge: &str) -> String {
        let scope = self.scopes.join(" ");
        let mut url = url::Url::parse(&self.authorization_endpoint)
            .expect("authorization_endpoint is a valid URL by construction");
        {
            let mut qp = url.query_pairs_mut();
            qp.append_pair("response_type", "code")
                .append_pair("client_id", &self.client_id)
                .append_pair("redirect_uri", &self.redirect_uri)
                .append_pair("scope", &scope)
                .append_pair("state", state)
                .append_pair("nonce", nonce)
                .append_pair("code_challenge_method", "S256")
                .append_pair("code_challenge", code_challenge);
        }
        url.to_string()
    }

    /// POST the authorization code to the token endpoint and return the
    /// parsed JSON response. The form body uses
    /// `application/x-www-form-urlencoded`.
    pub async fn exchange_code(&self, code: &str, code_verifier: &str) -> Result<Value, OidcError> {
        let client = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build()?;
        let resp = client
            .post(&self.token_endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", &self.redirect_uri),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
                ("code_verifier", code_verifier),
            ])
            .send()
            .await?
            .error_for_status()?;
        let body: Value = resp.json().await?;
        Ok(body)
    }

    /// Fetch the JWKS document from the discovered `jwks_uri`.
    pub async fn fetch_jwks(&self) -> Result<Value, OidcError> {
        let client = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build()?;
        let resp = client
            .get(&self.jwks_uri)
            .send()
            .await?
            .error_for_status()?;
        let body: Value = resp.json().await?;
        Ok(body)
    }

    /// Verify the signed `id_token` against `jwks` and return the
    /// decoded claims. Checks: signature, `exp`, `iss` equals
    /// `self.issuer`, `aud` contains `self.client_id`, and (if
    /// `expected_nonce` is `Some`) `nonce` matches.
    pub fn verify_id_token(
        &self,
        id_token: &str,
        jwks: &Value,
        expected_nonce: Option<&str>,
    ) -> Result<Value, OidcError> {
        let header = decode_header(id_token)
            .map_err(|e| OidcError::InvalidIdToken(format!("bad header: {e}")))?;
        let kid = header.kid.clone();
        let alg = header.alg;

        // Only RS-family is supported — OIDC core spec mandates RS256
        // for ID tokens but allows ES/Ed; we don't accept HMAC because
        // the symmetric key would have to be shipped with the client.
        match alg {
            Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512 => {}
            other => {
                return Err(OidcError::InvalidIdToken(format!(
                    "unsupported alg: {other:?}"
                )));
            }
        }

        let key = jwks_lookup(jwks, &kid)?;
        let decoding_key = DecodingKey::from_rsa_components(&key.n, &key.e)
            .map_err(|e| OidcError::InvalidIdToken(format!("rsa components: {e}")))?;

        let mut validation = Validation::new(alg);
        validation.set_issuer(&[self.issuer.as_str()]);
        validation.set_audience(&[self.client_id.as_str()]);
        validation.validate_exp = true;

        let data = decode::<Value>(id_token, &decoding_key, &validation)
            .map_err(|e| OidcError::InvalidIdToken(format!("decode: {e}")))?;

        if let Some(expected) = expected_nonce {
            let actual = data
                .claims
                .get("nonce")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if actual != expected {
                return Err(OidcError::NonceMismatch);
            }
        }

        Ok(data.claims)
    }
}

/// Look up a JWK in a JWKS document by `kid`. Returns the RSA `n` and
/// `e` components as base64url strings ready for
/// `DecodingKey::from_rsa_components`.
fn jwks_lookup(jwks: &Value, kid: &Option<String>) -> Result<RsaJwk, OidcError> {
    let keys =
        jwks.get("keys")
            .and_then(|v| v.as_array())
            .ok_or_else(|| OidcError::JwksMissingField {
                kid: kid.clone().unwrap_or_default(),
                field: "keys",
            })?;

    let target_kid = kid.as_deref().unwrap_or("");
    for key in keys {
        let kty = key.get("kty").and_then(|v| v.as_str()).unwrap_or_default();
        if kty != "RSA" {
            continue;
        }
        let this_kid = key.get("kid").and_then(|v| v.as_str()).unwrap_or("");
        if this_kid != target_kid {
            continue;
        }
        let n = key
            .get("n")
            .and_then(|v| v.as_str())
            .ok_or(OidcError::JwksMissingField {
                kid: target_kid.to_string(),
                field: "n",
            })?
            .to_string();
        let e = key
            .get("e")
            .and_then(|v| v.as_str())
            .ok_or(OidcError::JwksMissingField {
                kid: target_kid.to_string(),
                field: "e",
            })?
            .to_string();
        return Ok(RsaJwk { n, e });
    }

    Err(OidcError::JwksNoMatchingKid(kid.clone()))
}

struct RsaJwk {
    n: String,
    e: String,
}

/// Generate a 32-byte PKCE code verifier (base64url-no-pad encoded).
pub fn generate_code_verifier() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Compute the S256 PKCE code challenge for a given verifier.
pub fn code_challenge_from_verifier(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// Generate a 32-character random opaque state / nonce token (printable
/// ASCII, URL-safe).
pub fn generate_state_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use rsa::pkcs1::{EncodeRsaPrivateKey, LineEnding};
    use rsa::traits::PublicKeyParts;
    use rsa::RsaPrivateKey;
    use serde_json::json;

    /// Helper: generate an RSA keypair, build a JWKS that contains the
    /// public key (with the supplied `kid`), and sign a JWT whose header
    /// also has that `kid`. Returns the encoded token and the JWKS
    /// value the verifier can consume.
    fn sign_rsa_jwt(kid: &str, claims: Value, alg: Algorithm) -> (String, Value) {
        let mut rng = rand::thread_rng();
        let key = RsaPrivateKey::new(&mut rng, 2048).expect("rsa keygen");
        let pub_key = key.to_public_key();
        let n_b64 =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pub_key.n().to_bytes_be());
        let e_b64 =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pub_key.e().to_bytes_be());

        let jwks = json!({
            "keys": [{
                "kty": "RSA",
                "use": "sig",
                "alg": format!("{alg:?}"),
                "kid": kid,
                "n": n_b64,
                "e": e_b64,
            }]
        });

        let mut header = Header::new(alg);
        header.kid = Some(kid.to_string());
        let pem = key.to_pkcs1_pem(LineEnding::LF).expect("pem");
        let enc = EncodingKey::from_rsa_pem(pem.as_bytes()).expect("enc key");
        let token = encode(&header, &claims, &enc).expect("encode");
        (token, jwks)
    }

    fn client() -> OidcClient {
        OidcClient::from_endpoints(
            "https://issuer.example.com",
            "test-client",
            "secret",
            "https://app.example.com/cb",
            "https://issuer.example.com/authorize".into(),
            "https://issuer.example.com/token".into(),
            "https://issuer.example.com/jwks".into(),
        )
    }

    #[test]
    fn authorize_url_contains_all_required_params() {
        let c = client();
        let url = c.build_authorize_url("st", "no", "chal");
        assert!(url.contains("response_type=code"), "{url}");
        assert!(url.contains("client_id=test-client"), "{url}");
        assert!(url.contains("redirect_uri="), "{url}");
        assert!(url.contains("scope=openid"), "{url}");
        assert!(url.contains("state=st"), "{url}");
        assert!(url.contains("nonce=no"), "{url}");
        assert!(url.contains("code_challenge=chal"), "{url}");
        assert!(url.contains("code_challenge_method=S256"), "{url}");
    }

    #[test]
    fn verify_accepts_valid_rs256_token() {
        let c = client();
        let claims = json!({
            "sub": "user-1",
            "iss": c.issuer,
            "aud": c.client_id,
            "exp": 9_999_999_999u64,
            "nonce": "nonce-xyz",
            "email": "user@example.com",
        });
        let (token, jwks) = sign_rsa_jwt("kid-1", claims, Algorithm::RS256);
        let out = c
            .verify_id_token(&token, &jwks, Some("nonce-xyz"))
            .expect("verify");
        assert_eq!(out["sub"], "user-1");
        assert_eq!(out["email"], "user@example.com");
    }

    #[test]
    fn verify_rejects_expired_token() {
        let c = client();
        let claims = json!({
            "sub": "user-1",
            "iss": c.issuer,
            "aud": c.client_id,
            "exp": 1u64, // Jan 1 1970 — long expired
        });
        let (token, jwks) = sign_rsa_jwt("kid-1", claims, Algorithm::RS256);
        let err = c
            .verify_id_token(&token, &jwks, None)
            .expect_err("must reject");
        assert!(matches!(err, OidcError::InvalidIdToken(_)), "{err}");
    }

    #[test]
    fn verify_rejects_wrong_audience() {
        let c = client();
        let claims = json!({
            "sub": "user-1",
            "iss": c.issuer,
            "aud": "some-other-client",
            "exp": 9_999_999_999u64,
        });
        let (token, jwks) = sign_rsa_jwt("kid-1", claims, Algorithm::RS256);
        let err = c
            .verify_id_token(&token, &jwks, None)
            .expect_err("must reject");
        assert!(matches!(err, OidcError::InvalidIdToken(_)), "{err}");
    }

    #[test]
    fn verify_rejects_wrong_issuer() {
        let c = client();
        let claims = json!({
            "sub": "user-1",
            "iss": "https://attacker.example.com",
            "aud": c.client_id,
            "exp": 9_999_999_999u64,
        });
        let (token, jwks) = sign_rsa_jwt("kid-1", claims, Algorithm::RS256);
        let err = c
            .verify_id_token(&token, &jwks, None)
            .expect_err("must reject");
        assert!(matches!(err, OidcError::InvalidIdToken(_)), "{err}");
    }

    #[test]
    fn verify_rejects_nonce_mismatch() {
        let c = client();
        let claims = json!({
            "sub": "user-1",
            "iss": c.issuer,
            "aud": c.client_id,
            "exp": 9_999_999_999u64,
            "nonce": "actual",
        });
        let (token, jwks) = sign_rsa_jwt("kid-1", claims, Algorithm::RS256);
        let err = c
            .verify_id_token(&token, &jwks, Some("expected"))
            .expect_err("must reject");
        assert!(matches!(err, OidcError::NonceMismatch), "{err}");
    }

    #[test]
    fn verify_rejects_missing_kid_in_jwks() {
        let c = client();
        let claims = json!({
            "sub": "user-1",
            "iss": c.issuer,
            "aud": c.client_id,
            "exp": 9_999_999_999u64,
        });
        let (token, _jwks) = sign_rsa_jwt("kid-1", claims, Algorithm::RS256);
        let empty_jwks = json!({ "keys": [] });
        let err = c
            .verify_id_token(&token, &empty_jwks, None)
            .expect_err("must reject");
        assert!(matches!(err, OidcError::JwksNoMatchingKid(_)), "{err}");
    }

    #[test]
    fn code_challenge_matches_pkce_spec() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(code_challenge_from_verifier(verifier), expected);
    }

    #[test]
    fn state_token_is_url_safe_and_long_enough() {
        let token = generate_state_token();
        assert!(token.len() >= 32);
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "non-url-safe char: {token}"
        );
    }
}
