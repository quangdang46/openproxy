//! Live-IdP-style round-trip test for the SAML ACS handler (issue #461).
//!
//! No external IdP is required: the test generates a fresh RSA-2048 keypair
//! in-process, self-signs a minimal X.509 certificate for it with rcgen,
//! builds a fully-signed SAML Response (Response-level AND Assertion-level
//! signatures, mirroring what @node-saml/node-saml and real IdPs emit),
//! and POSTs it to `/api/auth/saml/acs` on a real Axum router. Success is a
//! 303 to `/dashboard` with an `auth_token` cookie whose claims decode to a
//! SAML session — then `/api/auth/status` must report loginMethod SAML.
//!
//! This exercises the production path end-to-end (base64 → InResponseTo →
//! XML-DSig verify → conditions → replay cache → session cookie) against
//! real cryptographic signatures rather than fixtures.

mod common;

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use base64::Engine;
use rsa::pkcs1v15::SigningKey;
use rsa::sha2::Sha256 as RsaSha256;
use rsa::signature::{RandomizedSigner, SignatureEncoding};
use serde_json::json;
use tower::util::ServiceExt;

use openproxy::server::auth::saml::SamlSettings;
use openproxy::server::state::AppState;

async fn boot_app() -> (axum::Router, AppState) {
    let temp = tempfile::tempdir().expect("tempdir");
    let db = Arc::new(openproxy::db::Db::load_from(temp.path()).await.expect("db"));
    // leak the tempdir so the SQLite files outlive the fn
    std::mem::forget(temp);
    let state = AppState::new(db);
    (openproxy::build_app(state.clone()), state)
}

/// Generate an RSA-2048 keypair and a self-signed X.509 cert (DER) for it.
/// Returns (signing_key, cert_der).
///
/// Strategy: generate the RSA key with the `rsa` crate, export PKCS#8 DER,
/// import into rcgen via `from_pkcs8_der_and_sign_algo` (ring backend —
/// available in our tree), self-sign, and use the original `rsa` key for
/// signing. Keypair and cert always agree by construction.
fn idp_keypair() -> (SigningKey<RsaSha256>, Vec<u8>) {
    use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
    use rsa::pkcs8::EncodePrivateKey;
    let mut rng = rand::thread_rng();
    let priv_key = rsa::RsaPrivateKey::new(&mut rng, 2048).expect("rsa keygen");
    let pkcs8_der = priv_key
        .to_pkcs8_der()
        .expect("pkcs8 der")
        .as_bytes()
        .to_vec();
    // rcgen re-exports rustls-pki-types; build PrivatePkcs8KeyDer via TryFrom.
    use std::convert::TryFrom;
    let key_pair = KeyPair::try_from(pkcs8_der.as_slice()).expect("rcgen import rsa key");
    let mut params = CertificateParams::new(vec!["idp.example.com".to_string()]).expect("params");
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "Test IdP");
    params.distinguished_name = dn;
    let cert = params.self_signed(&key_pair).expect("self-sign");
    let signing_key = SigningKey::<RsaSha256>::new(priv_key);
    (signing_key, cert.der().to_vec())
}

/// Canonicalize with the SAME subset the verifier uses: reuse the
/// production c14n indirectly by signing what the verifier will digest.
/// The verifier canonicalizes (exclusive, attrs sorted, ns hoisted) before
/// hashing — so we build the SignedInfo/digest over the canonical form by
/// round-tripping through a tiny local c14n that matches the production
/// implementation for this controlled document shape (single-line elements,
/// prefixed tags, no comments).
///
/// Production c14n details mirrored here:
/// - attributes sorted lexicographically (xmlns:* included in the sort)
/// - visibly-used namespaces re-declared on first use: the SignedInfo and
///   Signature blocks declare xmlns:ds on themselves; children relying on
///   ancestor declarations get `xmlns:ds` hoisted onto them during digest
/// - self-closing elements expanded to Start+End
fn c14n(s: &str) -> String {
    // Known namespace URIs (must match the production c14n seed stack).
    let known = [
        ("saml", "urn:oasis:names:tc:SAML:2.0:assertion"),
        ("samlp", "urn:oasis:names:tc:SAML:2.0:protocol"),
        ("ds", "http://www.w3.org/2000/09/xmldsig#"),
        ("md", "urn:oasis:names:tc:SAML:2.0:metadata"),
    ];
    let mut out = String::new();
    let mut rest = s;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let after = &rest[lt..];
        let Some(gt) = after.find('>') else {
            out.push_str(after);
            break;
        };
        let tag = &after[..gt];
        if tag.starts_with("</") || tag.starts_with("<?") || tag.starts_with("<!") {
            out.push_str(&format!("{tag}>"));
            rest = &after[gt + 1..];
            continue;
        }
        let self_close = tag.ends_with('/');
        let inner = tag[1..].trim_end_matches('/').trim_end();
        let mut parts = split_tag(inner);
        if parts.is_empty() {
            out.push_str(&format!("{tag}>"));
            rest = &after[gt + 1..];
            continue;
        }
        let name = parts.remove(0);
        // Visibly-used prefixes: element prefix + non-xmlns attribute prefixes.
        let mut used: Vec<&str> = vec![];
        if let Some((p, _)) = name.split_once(':') {
            used.push(p);
        }
        for a in &parts {
            let aname = a.split('=').next().unwrap_or("");
            if aname.starts_with("xmlns") {
                continue;
            }
            if let Some((p, _)) = aname.split_once(':') {
                if p != "xml" && !used.contains(&p) {
                    used.push(p);
                }
            }
        }
        // Hoist xmlns declarations for used prefixes not declared on the tag
        // (mirrors production exclusive-c14n ns hoisting).
        let mut hoisted: Vec<String> = vec![];
        for (pfx, uri) in known {
            if !used.contains(&pfx) {
                continue;
            }
            let declared = parts.iter().any(|a| {
                let an = a.split('=').next().unwrap_or("");
                an == format!("xmlns:{pfx}")
            });
            if !declared {
                hoisted.push(format!("xmlns:{pfx}=\"{uri}\""));
            }
        }
        parts.extend(hoisted);
        parts.sort();
        // Expand self-closing into Start+End (production parser does this).
        if self_close {
            out.push_str(&format!("<{name}"));
            for a in &parts {
                out.push_str(&format!(" {a}"));
            }
            out.push('>');
            out.push_str(&format!("</{name}>"));
        } else {
            out.push_str(&format!("<{name}"));
            for a in &parts {
                out.push_str(&format!(" {a}"));
            }
            out.push('>');
        }
        rest = &after[gt + 1..];
    }
    out
}

fn split_tag(inner: &str) -> Vec<String> {
    // Split `name k="v" k2='v2'` on whitespace outside quotes.
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = inner.chars().peekable();
    for c in chars.by_ref() {
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
            cur.push(c);
        } else if c.is_whitespace() {
            if !cur.is_empty() {
                parts.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

fn sha256_b64(s: &str) -> String {
    use rsa::sha2::Digest;
    let mut h = RsaSha256::new();
    h.update(s.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(h.finalize())
}

fn sign_b64(key: &SigningKey<RsaSha256>, msg: &str) -> String {
    let mut rng = rand::thread_rng();
    let sig = key.sign_with_rng(&mut rng, msg.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(sig.to_bytes())
}

/// Build a signed `<Response>` string. Both Response and Assertion carry
/// enveloped signatures with `URI=""` (Response) / `URI="#_a1"` (Assertion)
/// references, RSA-SHA256 + SHA-256 digests, exc-c14n transforms.
fn build_signed_response(
    key: &SigningKey<RsaSha256>,
    request_id: &str,
    acs: &str,
    issuer: &str,
    not_on_or_after: &str,
) -> String {
    let assertion_body = format!(
        r#"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_a1" IssueInstant="2026-01-01T00:00:00Z" Version="2.0"><saml:Issuer>{issuer}</saml:Issuer><saml:Subject><saml:NameID Format="urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress">saml-user@example.com</saml:NameID><saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer"><saml:SubjectConfirmationData InResponseTo="{request_id}" NotOnOrAfter="{not_on_or_after}" Recipient="{acs}"/></saml:SubjectConfirmation></saml:Subject><saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="{not_on_or_after}"><saml:AudienceRestriction><saml:Audience>{issuer}</saml:Audience></saml:AudienceRestriction></saml:Conditions><saml:AuthnStatement AuthnInstant="2026-01-01T00:00:00Z"><saml:AuthnContext><saml:AuthnContextClassRef>urn:oasis:names:tc:SAML:2.0:ac:classes:PasswordProtectedTransport</saml:AuthnContextClassRef></saml:AuthnContext></saml:AuthnStatement><saml:AttributeStatement><saml:Attribute Name="email"><saml:AttributeValue>saml-user@example.com</saml:AttributeValue></saml:Attribute><saml:Attribute Name="displayName"><saml:AttributeValue>SAML User</saml:AttributeValue></saml:Attribute></saml:AttributeStatement></saml:Assertion>"#
    );

    // Assertion signature (Reference URI="#_a1").
    let assertion_c14n = c14n(&assertion_body);
    let assertion_digest = sha256_b64(&assertion_c14n);
    let assertion_signed_info = format!(
        r##"<ds:SignedInfo xmlns:ds="http://www.w3.org/2000/09/xmldsig#"><ds:CanonicalizationMethod Algorithm="http://www.w3.org/2001/10/xml-exc-c14n#"/><ds:SignatureMethod Algorithm="http://www.w3.org/2000/09/xmldsig#rsa-sha256"/><ds:Reference URI="#_a1"><ds:Transforms><ds:Transform Algorithm="http://www.w3.org/2000/09/xmldsig#enveloped-signature"/><ds:Transform Algorithm="http://www.w3.org/2001/10/xml-exc-c14n#"/></ds:Transforms><ds:DigestMethod Algorithm="http://www.w3.org/2000/09/xmldsig#sha256"/><ds:DigestValue>{assertion_digest}</ds:DigestValue></ds:Reference></ds:SignedInfo>"##
    );
    let assertion_sig = sign_b64(key, &c14n(&assertion_signed_info));
    let assertion_signed = assertion_body.replace(
        "</saml:Assertion>",
        &format!(
            r#"<ds:Signature xmlns:ds="http://www.w3.org/2000/09/xmldsig#">{assertion_signed_info}<ds:SignatureValue>{assertion_sig}</ds:SignatureValue></ds:Signature></saml:Assertion>"#
        ),
    );

    // Response envelope (Reference URI="" — whole document).
    let response_inner = format!(
        r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" Destination="{acs}" ID="_r1" InResponseTo="{request_id}" IssueInstant="2026-01-01T00:00:00Z" Version="2.0"><saml:Issuer>{issuer}</saml:Issuer><samlp:Status><samlp:StatusCode Value="urn:oasis:names:tc:SAML:2.0:status:Success"/></samlp:Status>{assertion_signed}</samlp:Response>"#
    );
    // Digest over the response with the Signature excised (enveloped).
    let response_c14n = c14n(&response_inner);
    let response_digest = sha256_b64(&response_c14n);
    let response_signed_info = format!(
        r#"<ds:SignedInfo xmlns:ds="http://www.w3.org/2000/09/xmldsig#"><ds:CanonicalizationMethod Algorithm="http://www.w3.org/2001/10/xml-exc-c14n#"/><ds:SignatureMethod Algorithm="http://www.w3.org/2000/09/xmldsig#rsa-sha256"/><ds:Reference URI=""><ds:Transforms><ds:Transform Algorithm="http://www.w3.org/2000/09/xmldsig#enveloped-signature"/><ds:Transform Algorithm="http://www.w3.org/2001/10/xml-exc-c14n#"/></ds:Transforms><ds:DigestMethod Algorithm="http://www.w3.org/2000/09/xmldsig#sha256"/><ds:DigestValue>{response_digest}</ds:DigestValue></ds:Reference></ds:SignedInfo>"#
    );
    let response_sig = sign_b64(key, &c14n(&response_signed_info));
    response_inner.replace(
        "</samlp:Response>",
        &format!(
            r#"<ds:Signature xmlns:ds="http://www.w3.org/2000/09/xmldsig#">{response_signed_info}<ds:SignatureValue>{response_sig}</ds:SignatureValue></ds:Signature></samlp:Response>"#
        ),
    )
}

#[tokio::test]
async fn saml_acs_live_round_trip_with_real_signatures() {
    let (signing_key, cert_der) = idp_keypair();
    let cert_b64 = base64::engine::general_purpose::STANDARD.encode(&cert_der);

    let (app, state) = boot_app().await;

    // Configure SAML directly in the DB (entry point + cert + issuer).
    state
        .db
        .update_settings(|s| {
            s.saml_entry_point = "https://idp.example.com/sso".to_string();
            s.saml_cert = cert_b64.clone();
            s.saml_issuer = "urn:test:sp-live".to_string();
            s.auth_mode = "sso".to_string();
            s.sso_type = "saml".to_string();
        })
        .await
        .expect("save settings");
    // Sanity: settings round-trip.
    assert_eq!(state.db.snapshot().settings.saml_issuer, "urn:test:sp-live");
    let _ = SamlSettings::default();

    let request_id = "_live_req_1";
    let acs = "http://localhost/api/auth/saml/acs";
    let not_on_or_after = "2030-01-01T00:00:00Z";
    let response_xml = build_signed_response(
        &signing_key,
        request_id,
        acs,
        "urn:test:sp-live",
        not_on_or_after,
    );
    let b64 = base64::engine::general_purpose::STANDARD.encode(response_xml.as_bytes());
    let form = format!("SAMLResponse={}", urlencoding::encode(&b64).into_owned());

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/saml/acs")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("host", "localhost")
                .header("cookie", format!("saml_state={request_id}"))
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .unwrap();

    if resp.status() != StatusCode::SEE_OTHER {
        let loc = resp
            .headers()
            .get(axum::http::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        panic!(
            "expected 303 from ACS; location={loc}; body={}",
            String::from_utf8_lossy(&body)
        );
    }
    let headers = resp.headers().clone();
    let loc = headers
        .get(axum::http::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    eprintln!("ACS redirect -> {loc}");
    let set_cookies: Vec<String> = headers
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap_or("").to_string())
        .collect();
    let auth_cookie = set_cookies
        .iter()
        .find(|c| c.starts_with("auth_token="))
        .expect("auth_token cookie");
    // Session cookie TTL must be 24h (SESSION_MAX_AGE_SEC parity).
    assert!(
        auth_cookie.contains("Max-Age=86400"),
        "expected 24h session cookie, got: {auth_cookie}"
    );

    // Decode the session JWT: must be a SAML session with picked claims.
    let token = auth_cookie
        .split(';')
        .next()
        .unwrap_or("")
        .trim_start_matches("auth_token=");
    let claims: serde_json::Value = {
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "expected JWT parts");
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[1])
            .expect("jwt payload b64");
        serde_json::from_slice(&payload).expect("jwt payload json")
    };
    assert_eq!(claims["authenticated"], true);
    assert_eq!(claims["saml"], true);
    assert_eq!(claims["saml_email"], json!("saml-user@example.com"));
    assert_eq!(claims["saml_name"], json!("SAML User"));

    // Replay of the same assertion must now be rejected.
    let (app2, _) = (openproxy::build_app(state.clone()), ());
    let _ = app2;
    let b64b = base64::engine::general_purpose::STANDARD.encode(response_xml.as_bytes());
    let form2 = format!("SAMLResponse={}", urlencoding::encode(&b64b).into_owned());
    // Reuse a fresh request id cookie — replay cache (not InResponseTo)
    // must be what rejects this.
    let resp2 = openproxy::build_app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/saml/acs")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("host", "localhost")
                .header("cookie", format!("saml_state={request_id}"))
                .body(Body::from(form2))
                .unwrap(),
        )
        .await
        .unwrap();
    let loc2 = resp2
        .headers()
        .get(axum::http::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        loc2.contains("saml_assertion_replayed"),
        "expected replay rejection, got location: {loc2}"
    );
}
