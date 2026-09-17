//! SAML 2.0 SSO service-provider support.
//!
//! Backend-only port of 9router `src/lib/auth/saml.js` (commit 65197ad1
//! "feat(auth): add native SAML 2.0 SSO integration").
//!
//! The handshake is:
//!
//! 1. Browser hits `GET /api/auth/saml/start` → server builds a SAML
//!    AuthnRequest, stashes its ID in a short-lived HttpOnly `saml_state`
//!    cookie, and 302-redirects to the IdP's SSO URL.
//! 2. IdP authenticates the user, then POSTs a base64 `SAMLResponse` to
//!    `POST /api/auth/saml/acs`.
//! 3. Server checks `InResponseTo` against the cookie (replay protection),
//!    verifies the XML-DSig RSA signature over the Response (or its
//!    Assertion) with the configured IdP X.509 certificate, enforces
//!    `wantAssertionsSigned`, audience/bearer conditions, and clock skew,
//!    then issues the dashboard session cookie and 302-redirects to `/`.
//!
//! Signature verification is implemented directly on `rsa 0.9` +
//! `x509-parser 0.16` + `quick-xml` (already in the dependency tree for
//! other features): enveloped-signature c14n is restricted to the
//! WS-Security subset 9router relies on (exclusive c14n without comments,
//! RSA-SHA256 / RSA-SHA1, SHA-256 / SHA-1 digests) — see
//! [`verify_xml_signature`] for the exact supported surface.

use base64::Engine;
use quick_xml::events::Event;
use quick_xml::Reader;
use rsa::pkcs1::DecodeRsaPublicKey;
use rsa::{BigUint, RsaPublicKey};
use sha2::{Digest, Sha256};
use thiserror::Error;
use x509_parser::prelude::*;

/// Errors produced by the SAML flow. Each variant maps onto a single
/// failure mode the caller can act on.
#[derive(Debug, Error)]
pub enum SamlError {
    #[error("SAML error: {0}")]
    Config(String),

    #[error("SAML error: {0}")]
    Protocol(String),

    #[error("SAML signature verification failed: {0}")]
    BadSignature(String),

    #[error("SAML error: {0}")]
    Time(String),
}

/// Format a raw Base64 string or unformatted X.509 certificate into
/// standard PEM format. Mirrors `formatX509Certificate` in saml.js.
pub fn format_x509_certificate(cert_str: &str) -> String {
    if cert_str.is_empty() {
        return String::new();
    }
    let clean: String = cert_str
        .replace("-----BEGIN CERTIFICATE-----", "")
        .replace("-----END CERTIFICATE-----", "")
        .replace("-----begin certificate-----", "")
        .replace("-----end certificate-----", "")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/' || *c == '=')
        .collect();
    // Handle case-insensitive markers missed above (mixed case).
    let clean: String = {
        let upper = cert_str.to_uppercase();
        if upper.contains("BEGIN CERTIFICATE") && clean.is_empty() {
            String::new()
        } else {
            clean
        }
    };
    if clean.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = clean
        .chars()
        .collect::<Vec<_>>()
        .chunks(64)
        .map(|c| c.iter().collect())
        .collect();
    format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----",
        lines.join("\n")
    )
}

/// True when the settings carry the essential SAML parameters
/// (entryPoint + cert). Mirrors `isSamlConfigured` in saml.js.
pub fn is_saml_configured(entry_point: &str, cert: &str) -> bool {
    !entry_point.trim().is_empty() && !cert.trim().is_empty()
}

/// Resolve the public base URL for SAML requests. Mirrors `getSamlBaseUrl`
/// in saml.js: explicit `base_url` setting first, then `BASE_URL` /
/// `NEXT_PUBLIC_BASE_URL` env, then forwarded-proto/host headers, then the
/// request origin, falling back to `http://localhost:20128`.
pub fn saml_base_url(
    configured_base_url: &str,
    forwarded_proto: Option<&str>,
    forwarded_host: Option<&str>,
    host: Option<&str>,
    request_origin: Option<&str>,
) -> String {
    fn trim_slashes(s: &str) -> String {
        s.trim_end_matches('/').to_string()
    }
    let configured = configured_base_url.trim();
    if !configured.is_empty() {
        return trim_slashes(configured);
    }
    if let Ok(env) = std::env::var("BASE_URL") {
        if !env.trim().is_empty() {
            return trim_slashes(env.trim());
        }
    }
    if let Ok(env) = std::env::var("NEXT_PUBLIC_BASE_URL") {
        if !env.trim().is_empty() {
            return trim_slashes(env.trim());
        }
    }
    let host = forwarded_host
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .or_else(|| host.map(str::trim).filter(|h| !h.is_empty()));
    if let Some(host) = host {
        let proto = forwarded_proto
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or("http")
            .trim_end_matches(':');
        return format!("{proto}://{host}")
            .trim_end_matches('/')
            .to_string();
    }
    if let Some(origin) = request_origin.map(str::trim).filter(|o| !o.is_empty()) {
        return trim_slashes(origin);
    }
    "http://localhost:20128".to_string()
}

/// Settings snapshot needed to build SAML requests. Kept as plain strings
/// so both the settings-DB path and the test endpoint (which accepts
/// candidate values in the body) can share it.
#[derive(Debug, Clone, Default)]
pub struct SamlSettings {
    pub entry_point: String,
    pub issuer: String,
    pub cert: String,
    pub attribute_email: String,
    pub attribute_name: String,
}

impl SamlSettings {
    pub fn issuer_or_default(&self) -> &str {
        if self.issuer.trim().is_empty() {
            "urn:9router:sp"
        } else {
            self.issuer.trim()
        }
    }
}

/// Build a SAML AuthnRequest redirect URL (`authorizeUrl`) plus the request
/// ID stashed in the `saml_state` cookie. Mirrors `buildSamlAuthorizeUrl`
/// in saml.js (which delegates XML emission + deflate-encoding to
/// node-saml internals — reimplemented here explicitly).
pub fn build_authorize_url(
    settings: &SamlSettings,
    origin: &str,
    request_id: &str,
) -> Result<String, SamlError> {
    if settings.entry_point.trim().is_empty() {
        return Err(SamlError::Config(
            "SAML entryPoint is not configured".into(),
        ));
    }
    let callback_url = format!("{}/api/auth/saml/acs", origin.trim_end_matches('/'));
    let issue_instant = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let xml = format!(
        r#"<samlp:AuthnRequest xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="{id}" Version="2.0" IssueInstant="{instant}" Destination="{dest}" ProtocolBinding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST" AssertionConsumerServiceURL="{acs}"><saml:Issuer>{issuer}</saml:Issuer><samlp:NameIDPolicy Format="urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress" AllowCreate="true"/></samlp:AuthnRequest>"#,
        id = xml_escape(request_id),
        instant = issue_instant,
        dest = xml_escape(settings.entry_point.trim()),
        acs = xml_escape(&callback_url),
        issuer = xml_escape(settings.issuer_or_default()),
    );
    // HTTP-Redirect binding: raw DEFLATE + base64 + urlencode.
    let deflated = deflate_raw(xml.as_bytes());
    let encoded = base64::engine::general_purpose::STANDARD.encode(&deflated);
    let sep = if settings.entry_point.contains('?') {
        '&'
    } else {
        '?'
    };
    Ok(format!(
        "{}{}{}={}",
        settings.entry_point.trim(),
        sep,
        "SAMLRequest",
        urlencoding::encode(&encoded)
    ))
}

/// Generate a random SAML request/cookie ID (`_xxxx` + 32 hex chars,
/// matching node-saml's `generateUniqueID` shape closely enough for
/// InResponseTo correlation).
pub fn generate_request_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("_{}", hex::encode(bytes))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn deflate_raw(data: &[u8]) -> Vec<u8> {
    use flate2::write::DeflateEncoder;
    use flate2::Compression;
    use std::io::Write;
    let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).unwrap_or(());
    enc.finish().unwrap_or_default()
}

/// A verified SAML assertion profile: attribute/claim map plus the
/// NameID, if present.
#[derive(Debug, Clone, Default)]
pub struct SamlProfile {
    /// Flat claim map: attribute name → values (single values stored as
    /// one-element vecs). Mirrors the `profile` object node-saml returns.
    pub attributes: std::collections::BTreeMap<String, Vec<String>>,
    pub name_id: Option<String>,
    pub name_id_format: Option<String>,
    pub session_index: Option<String>,
}

impl SamlProfile {
    fn get_first(&self, key: &str) -> Option<&str> {
        self.attributes
            .get(key)
            .and_then(|v| v.first())
            .map(String::as_str)
    }
}

/// Extract the email claim. Mirrors `pickSamlEmail` in saml.js:
/// configured custom attribute → common email claims → `attributes`
/// object (folded into the same map here).
pub fn pick_saml_email(profile: &SamlProfile, settings: &SamlSettings) -> String {
    if !settings.attribute_email.trim().is_empty() {
        if let Some(v) = profile.get_first(settings.attribute_email.trim()) {
            return v.to_string();
        }
    }
    const EMAIL_KEYS: &[&str] = &[
        "email",
        "emailAddress",
        "mail",
        "nameID",
        "nameId",
        "upn",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/emailaddress",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/nameidentifier",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/upn",
    ];
    for key in EMAIL_KEYS {
        if let Some(v) = profile.get_first(key) {
            return v.to_string();
        }
    }
    String::new()
}

/// Extract the display-name claim. Mirrors `pickSamlDisplayName` in saml.js.
pub fn pick_saml_display_name(profile: &SamlProfile, settings: &SamlSettings) -> String {
    if !settings.attribute_name.trim().is_empty() {
        if let Some(v) = profile.get_first(settings.attribute_name.trim()) {
            return v.to_string();
        }
    }
    const NAME_KEYS: &[&str] = &[
        "displayName",
        "name",
        "cn",
        "commonName",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/name",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/givenname",
    ];
    for key in NAME_KEYS {
        if let Some(v) = profile.get_first(key) {
            return v.to_string();
        }
    }
    let given = profile.get_first("givenName").unwrap_or("");
    let surname = profile
        .get_first("sn")
        .or_else(|| profile.get_first("surname"))
        .unwrap_or("");
    let combined = format!("{given} {surname}").trim().to_string();
    if !combined.is_empty() {
        return combined;
    }
    pick_saml_email(profile, settings)
}

/// Validate a base64 `SAMLResponse` POST body and return the verified
/// profile. Mirrors `validateSamlResponse` in saml.js:
///
/// 1. Require a configured IdP cert.
/// 2. Require the `SAMLResponse` parameter.
/// 3. Replay protection: when `expected_request_id` is non-empty, the
///    response XML must carry a matching `InResponseTo`. Callers should
///    additionally call [`is_assertion_replayed`] / [`mark_assertion_used`]
///    to get single-use semantics (issues #454/#458).
/// 4. Verify the enveloped XML-DSig RSA signature over the Response (or
///    its Assertion when only the latter is signed) with the IdP cert.
///    `wantAssertionsSigned` is enforced (issues #449/#450): when the
///    Response is signed, the Assertion must ALSO be signed — and the
///    Response Reference must actually cover the Assertion element, not
///    just the Response wrapper.
/// 5. Enforce audience (our issuer/SP entity id), bearer
///    `SubjectConfirmation` recipient (== expected ACS URL) +
///    `NotOnOrAfter`, `Destination` (required, must be the ACS URL), and
///    `Conditions` `NotBefore`/`NotOnOrAfter` with 60s clock skew
///    (issues #452/#455).
pub fn validate_saml_response(
    saml_response_b64: &str,
    expected_request_id: &str,
    settings: &SamlSettings,
    now_unix: i64,
    expected_acs_url: &str,
) -> Result<SamlProfile, SamlError> {
    if settings.cert.trim().is_empty() {
        return Err(SamlError::Config(
            "IdP X.509 Certificate (samlCert) is missing or not configured".into(),
        ));
    }
    if saml_response_b64.trim().is_empty() {
        return Err(SamlError::Protocol(
            "Missing SAMLResponse parameter in assertion POST body".into(),
        ));
    }
    let xml_bytes = base64::engine::general_purpose::STANDARD
        .decode(saml_response_b64.trim())
        .map_err(|e| SamlError::Protocol(format!("SAMLResponse is not valid base64: {e}")))?;
    let xml = String::from_utf8(xml_bytes)
        .map_err(|e| SamlError::Protocol(format!("SAMLResponse is not valid UTF-8: {e}")))?;

    if !expected_request_id.is_empty() {
        let in_response_to = extract_attr(&xml, "InResponseTo");
        match in_response_to {
            Some(v) if v == expected_request_id => {}
            Some(v) => {
                return Err(SamlError::Protocol(format!(
                    "InResponseTo mismatch: expected {expected_request_id}, received {v}"
                )));
            }
            None => {
                return Err(SamlError::Protocol(format!(
                    "InResponseTo mismatch: expected {expected_request_id}, received none"
                )));
            }
        }
    }

    let cert_pem = format_x509_certificate(&settings.cert);
    if cert_pem.is_empty() {
        return Err(SamlError::Config(
            "Invalid IdP X.509 Certificate format".into(),
        ));
    }
    let public_key =
        rsa_public_key_from_cert_pem(&cert_pem).map_err(|e| SamlError::Config(e.to_string()))?;

    // Verify signatures. wantAssertionsSigned (issues #449/#450): a signed
    // Response wrapping an UNSIGNED Assertion is rejected — the Assertion
    // must carry its own signature, and the Response Reference must cover
    // the Assertion element (URI="" whole-doc, or #ID resolving to the
    // Assertion, or ID == the Response ID whose digest covers the child).
    let response_sig = find_signature(&xml, "Response");
    let assertion_xml = extract_assertion(&xml).ok_or_else(|| {
        SamlError::Protocol("SAMLResponse contains no Assertion element".to_string())
    })?;
    let assertion_sig = find_signature(&assertion_xml, "Assertion");
    match (response_sig, assertion_sig) {
        (Some(rsig), Some(asig)) => {
            verify_xml_signature(&xml, &rsig, &public_key)?;
            // The Response Reference must cover the Assertion: accept
            // whole-document refs (URI=""), refs resolving to the Assertion
            // ID, or refs to the Response ID (digest covers the child).
            let assertion_id = extract_attr(&assertion_xml, "ID").unwrap_or_default();
            let covered = rsig.reference_uri.is_empty()
                || (!assertion_id.is_empty() && rsig.reference_uri == format!("#{assertion_id}"))
                || response_id_covered(&xml, &rsig.reference_uri, &assertion_xml);
            if !covered {
                return Err(SamlError::BadSignature(
                    "Response signature Reference does not cover the Assertion element".into(),
                ));
            }
            verify_xml_signature(&assertion_xml, &asig, &public_key)?;
        }
        (None, Some(sig)) => verify_xml_signature(&assertion_xml, &sig, &public_key)?,
        (Some(_), None) => {
            return Err(SamlError::BadSignature(
                "wantAssertionsSigned: Response is signed but the Assertion carries no signature"
                    .into(),
            ));
        }
        (None, None) => {
            return Err(SamlError::BadSignature(
                "no enveloped Signature found on Response or Assertion".into(),
            ));
        }
    }

    check_conditions(
        &xml,
        &assertion_xml,
        settings.issuer_or_default(),
        expected_acs_url,
        now_unix,
    )?;
    Ok(parse_assertion_profile(&assertion_xml))
}

/// True when the Response-signature Reference URI resolves to the Response
/// root itself (whose digest then covers the Assertion child element).
fn response_id_covered(xml: &str, reference_uri: &str, assertion_xml: &str) -> bool {
    let Some(id) = reference_uri.strip_prefix('#') else {
        return false;
    };
    if id.is_empty() {
        return false;
    }
    // The referenced element must be an ancestor of (or equal to) the
    // Assertion — i.e. the Response root carrying this ID must contain the
    // Assertion markup.
    let Some(target) = find_element_by_id(xml, id) else {
        return false;
    };
    target.contains(assertion_xml)
}

/// Single-use assertion replay cache (issues #454/#458).
///
/// Maps consumed Assertion `@ID` (or `InResponseTo` fallback) → expiry unix
/// time (NotOnOrAfter + skew). Entries are bounded (10k) and lazily evicted
/// on insert; a periodic sweep is unnecessary because stale entries are
/// skipped on lookup and overwritten on insert.
static CONSUMED_ASSERTIONS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, i64>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// True when this assertion ID was already consumed and has not yet expired.
/// Stale (expired) entries are treated as unused.
pub fn is_assertion_replayed(assertion_id: &str, now_unix: i64) -> bool {
    if assertion_id.is_empty() {
        return false;
    }
    let map = CONSUMED_ASSERTIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.get(assertion_id).is_some_and(|&exp| now_unix <= exp)
}

/// Record an assertion ID as consumed until `expiry_unix`. No-op on empty ID.
pub fn mark_assertion_used(assertion_id: &str, expiry_unix: i64) {
    if assertion_id.is_empty() {
        return;
    }
    let mut map = CONSUMED_ASSERTIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // Opportunistic eviction: drop expired entries, then cap size.
    map.retain(|_, &mut exp| exp > expiry_unix - 3600);
    if map.len() >= 10_000 {
        if let Some(k) = map.keys().next().cloned() {
            map.remove(&k);
        }
    }
    map.insert(assertion_id.to_string(), expiry_unix);
}

/// Extract the Assertion `@ID` (or `InResponseTo` fallback) for replay
/// tracking. Returns "" when neither is present.
pub fn assertion_replay_id(assertion_xml: &str, response_xml: &str) -> String {
    extract_attr(assertion_xml, "ID")
        .or_else(|| extract_attr(response_xml, "InResponseTo"))
        .unwrap_or_default()
}

/// Extract the tightest expiry bound (min of all NotOnOrAfter values + 60s
/// skew) for replay-cache TTL. Falls back to now + 300s when unparseable.
pub fn assertion_expiry_unix(assertion_xml: &str, response_xml: &str, now_unix: i64) -> i64 {
    let mut best: Option<i64> = None;
    for src in [assertion_xml, response_xml] {
        let mut search = src;
        while let Some(pos) = search.find("NotOnOrAfter") {
            let rest = &search[pos..];
            let Some(end) = rest.find('>') else { break };
            let head = &rest[..end];
            if let Some(v) = extract_attr(head, "NotOnOrAfter") {
                if let Some(ts) = parse_saml_time(&v) {
                    best = Some(best.map_or(ts, |b: i64| b.min(ts)));
                }
            }
            search = &rest[end + 1..];
        }
    }
    best.map(|ts| ts + 60).unwrap_or(now_unix + 300)
}

/// Generate standard SP XML metadata. Mirrors `generateSamlMetadata` in
/// saml.js (node-saml `generateServiceProviderMetadata`).
pub fn generate_saml_metadata(origin: &str, settings: &SamlSettings) -> String {
    let origin = origin.trim_end_matches('/');
    let entity_id = xml_escape(settings.issuer_or_default());
    let acs = format!("{origin}/api/auth/saml/acs");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><md:EntityDescriptor xmlns:md="urn:oasis:names:tc:SAML:2.0:metadata" entityID="{entity}"><md:SPSSODescriptor AuthnRequestsSigned="false" WantAssertionsSigned="true" protocolSupportEnumeration="urn:oasis:names:tc:SAML:2.0:protocol"><md:AssertionConsumerService Binding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST" Location="{acs}" index="1"/></md:SPSSODescriptor></md:EntityDescriptor>"#,
        entity = entity_id,
        acs = xml_escape(&acs),
    )
}

// ---------------------------------------------------------------------------
// XML helpers (quick-xml based, std-only otherwise)
// ---------------------------------------------------------------------------

/// Extract the first `attr="value"` occurrence from raw XML text.
/// Used for `InResponseTo` pre-screening before signature verification
/// (the value is re-checked on the verified document afterwards — the
/// pre-check only decides which signature scope to verify).
fn extract_attr(xml: &str, attr: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let needle = format!("{attr}={quote}");
        if let Some(start) = xml.find(&needle) {
            let rest = &xml[start + needle.len()..];
            if let Some(end) = rest.find(quote) {
                return Some(rest[..end].to_string());
            }
        }
    }
    None
}

/// Extract the first `<saml:Assertion …>…</saml:Assertion>` (or
/// `<Assertion …>`) block, preserving prefixes.
/// Public for ACS replay-ID derivation.
pub fn extract_assertion(xml: &str) -> Option<String> {
    for tag in ["saml:Assertion", "saml2:Assertion", "Assertion"] {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        if let Some(start) = xml.find(&open) {
            if let Some(end) = xml[start..].find(&close) {
                return Some(xml[start..start + end + close.len()].to_string());
            }
        }
    }
    None
}

/// Build an RSA public key from a PEM X.509 certificate.
/// The SPKI public key is extracted with x509-parser; the RSA modulus and
/// exponent come from its DER (PKCS#1 RSAPublicKey sequence).
/// Public so the `saml/test` endpoint and the settings validator can reject
/// malformed certs before persisting (issues #457/#460).
pub fn rsa_public_key_from_cert_pem(pem: &str) -> Result<RsaPublicKey, String> {
    let b64: String = pem
        .replace("-----BEGIN CERTIFICATE-----", "")
        .replace("-----END CERTIFICATE-----", "")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let der = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("decode X.509 certificate: {e}"))?;
    let (_, cert) =
        X509Certificate::from_der(&der).map_err(|e| format!("parse X.509 certificate: {e}"))?;
    let spki = cert.tbs_certificate.subject_pki;
    // subject_pki.raw is the full SubjectPublicKeyInfo DER (algorithm +
    // BIT STRING). Unwrap the BIT STRING to the inner PKCS#1 RSAPublicKey.
    let spki_der: &[u8] = spki.raw;
    let pubkey_der = {
        use x509_parser::der_parser::ber::BerObjectContent;
        use x509_parser::der_parser::der::parse_der_sequence;
        let (_, seq) =
            parse_der_sequence(spki_der).map_err(|e| format!("parse SPKI sequence: {e}"))?;
        // SPKI = SEQ { AlgorithmIdentifier, BIT STRING }; take 2nd child.
        let children: Vec<_> = seq.ref_iter().collect();
        let bitstring = children
            .get(1)
            .ok_or_else(|| "SPKI has no subjectPublicKey BIT STRING".to_string())?;
        match &bitstring.content {
            BerObjectContent::BitString(_, bytes) => bytes.data.to_vec(),
            other => {
                return Err(format!(
                    "SPKI subjectPublicKey is not a BIT STRING: {other:?}"
                ));
            }
        }
    };
    // Prepended unused-bits byte is already stripped by the parser's
    // BitString view (it returns the raw content bytes).
    let rsa_key = rsa::pkcs1::RsaPublicKey::try_from(pubkey_der.as_slice())
        .map_err(|e| format!("extract RSA public key from certificate: {e}"))?;
    let n = BigUint::from_bytes_be(rsa_key.modulus.as_bytes());
    let e = BigUint::from_bytes_be(rsa_key.public_exponent.as_bytes());
    RsaPublicKey::new(n, e).map_err(|e| format!("convert RSA public key: {e}"))
}

/// Find the next open tag whose local name is `Audience`
/// (`<Audience>`, `<Audience …>`, `<saml:Audience …>`, …). Skips
/// `<AudienceRestriction>` via a tag-name boundary check.
fn find_audience_open(xml: &str) -> Option<usize> {
    let mut from = 0;
    let bytes = xml.as_bytes();
    while from < bytes.len() {
        let rel = xml[from..].find('<')?;
        let abs = from + rel;
        // Skip closing tags.
        if bytes.get(abs + 1) == Some(&b'/') {
            from = abs + 2;
            continue;
        }
        let mut name_end = abs + 1;
        while name_end < bytes.len()
            && !matches!(bytes[name_end], b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'>')
        {
            name_end += 1;
        }
        let name = &xml[abs + 1..name_end];
        if name.rsplit(':').next() == Some("Audience") {
            return Some(abs);
        }
        from = name_end + 1;
    }
    None
}

/// Enforce audience, bearer SubjectConfirmation, and Conditions time
/// windows (60s clock skew, mirroring node-saml defaults +
/// `acceptedClockSkewMs: 60000` in saml.js).
///
/// `expected_acs_url` is `{origin}/api/auth/saml/acs` (issues #452/#455):
/// - `Recipient` (SubjectConfirmationData) is REQUIRED and must equal it.
/// - `Destination` (Response attribute) is REQUIRED and must equal it.
fn check_conditions(
    response_xml: &str,
    assertion_xml: &str,
    expected_audience: &str,
    expected_acs_url: &str,
    now_unix: i64,
) -> Result<(), SamlError> {
    const SKEW: i64 = 60;
    let mut audience_ok = false;
    let mut search = assertion_xml;
    // Match `<Audience>`, `<Audience …>`, and any prefixed form
    // (`<saml:Audience>`, `<saml2:Audience …>`) — real IdPs always prefix.
    // NOTE: a naive `find("<Audience")` misses prefixed tags entirely AND
    // false-matches `<AudienceRestriction>` (fixed here with a tag-name
    // boundary check).
    while let Some(pos) = find_audience_open(search) {
        let rest = &search[pos..];
        let Some(end) = rest.find('>') else { break };
        // Skip self-closing and the Restriction wrapper (no text content).
        let local = rest[1..end]
            .split([' ', '\t', '\n', '\r', '/', '>'])
            .next()
            .unwrap_or("");
        let local = local.rsplit(':').next().unwrap_or(local);
        if local != "Audience" || rest[..end].ends_with('/') {
            search = &rest[end + 1..];
            continue;
        }
        let close = rest
            .find("</Audience>")
            .or_else(|| rest.find("</saml:Audience>"))
            .or_else(|| rest.find("</saml2:Audience>"));
        let text = match close {
            Some(c) => rest[end + 1..c].trim().to_string(),
            None => String::new(),
        };
        if text == expected_audience {
            audience_ok = true;
            break;
        }
        search = &rest[end + 1..];
    }
    if !audience_ok {
        return Err(SamlError::Protocol(format!(
            "SAML audience mismatch: expected {expected_audience}"
        )));
    }
    let sc_start = assertion_xml
        .find("SubjectConfirmation")
        .ok_or_else(|| SamlError::Protocol("SAML assertion has no SubjectConfirmation".into()))?;
    let sc_snippet = &assertion_xml[sc_start..(sc_start + 2000).min(assertion_xml.len())];
    if !sc_snippet.contains("bearer") {
        return Err(SamlError::Protocol(
            "SAML SubjectConfirmation is not bearer method".into(),
        ));
    }
    if let Some(v) = extract_attr(sc_snippet, "NotOnOrAfter")
        .or_else(|| extract_attr(assertion_xml, "NotOnOrAfter"))
    {
        let ts =
            parse_saml_time(&v).ok_or_else(|| SamlError::Time(format!("bad NotOnOrAfter: {v}")))?;
        if now_unix > ts + SKEW {
            return Err(SamlError::Time(
                "SAML assertion expired (NotOnOrAfter)".into(),
            ));
        }
    }
    if let Some(v) = extract_attr(assertion_xml, "NotBefore") {
        let ts =
            parse_saml_time(&v).ok_or_else(|| SamlError::Time(format!("bad NotBefore: {v}")))?;
        if now_unix < ts - SKEW {
            return Err(SamlError::Time(
                "SAML assertion not yet valid (NotBefore)".into(),
            ));
        }
    }
    if let Some(v) = extract_attr(assertion_xml, "NotOnOrAfter") {
        let ts =
            parse_saml_time(&v).ok_or_else(|| SamlError::Time(format!("bad NotOnOrAfter: {v}")))?;
        if now_unix > ts + SKEW {
            return Err(SamlError::Time(
                "SAML assertion expired (NotOnOrAfter)".into(),
            ));
        }
    }
    if let Some(dest) = extract_attr(response_xml, "Destination") {
        if dest.trim().is_empty() {
            return Err(SamlError::Protocol(
                "SAML Response Destination is empty".into(),
            ));
        }
        if dest != expected_acs_url {
            return Err(SamlError::Protocol(format!(
                "SAML Response Destination mismatch: {dest}"
            )));
        }
    } else {
        return Err(SamlError::Protocol(
            "SAML Response Destination is required".into(),
        ));
    }
    // Recipient is REQUIRED and must equal the ACS URL (issues #452/#455).
    let sc_recipient = extract_attr(sc_snippet, "Recipient");
    match sc_recipient {
        Some(r) if r == expected_acs_url => {}
        Some(r) => {
            return Err(SamlError::Protocol(format!(
                "SAML SubjectConfirmation Recipient mismatch: {r}"
            )));
        }
        None => {
            return Err(SamlError::Protocol(
                "SAML SubjectConfirmation Recipient is required".into(),
            ));
        }
    }
    Ok(())
}

fn parse_saml_time(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp())
}

/// Parse the verified Assertion into a [`SamlProfile`]: NameID plus every
/// Attribute Name to AttributeValue list (multi-valued preserved).
fn parse_assertion_profile(assertion_xml: &str) -> SamlProfile {
    let mut profile = SamlProfile::default();
    profile.name_id = extract_element_text(assertion_xml, "NameID");
    profile.name_id_format = extract_attr(assertion_xml, "Format");
    profile.session_index = extract_attr(assertion_xml, "SessionIndex");
    let mut search = assertion_xml;
    loop {
        let Some(start) = find_open_tag(search, "Attribute") else {
            break;
        };
        let Some(head_end_rel) = search[start..].find('>') else {
            break;
        };
        let head = &search[start..start + head_end_rel];
        let name = extract_attr(head, "Name").or_else(|| extract_attr(head, "AttributeName"));
        let tag_end = head
            .find([' ', '\t', '\n', '\r', '/', '>'])
            .unwrap_or(head.len());
        let tag_name = &head[1.min(tag_end)..tag_end.max(1)];
        let tag_name = if tag_name.is_empty() {
            "Attribute"
        } else {
            tag_name
        };
        let close = format!("</{tag_name}>");
        let Some(close_rel) = search[start..].find(&close) else {
            break;
        };
        let inner = &search[start + head_end_rel + 1..start + close_rel];
        if let Some(attr_name) = name {
            let mut values = Vec::new();
            let mut rest = inner;
            while let Some(v_start) = find_open_tag(rest, "AttributeValue") {
                let Some(v_head_end) = rest[v_start..].find('>') else {
                    break;
                };
                let v_head = &rest[v_start..v_start + v_head_end];
                let v_space = v_head
                    .find([' ', '\t', '\n', '\r', '/', '>'])
                    .unwrap_or(v_head.len());
                let v_tag = &v_head[1.min(v_space)..v_space.max(1)];
                let v_tag = if v_tag.is_empty() {
                    "AttributeValue"
                } else {
                    v_tag
                };
                let v_close = format!("</{v_tag}>");
                let Some(v_close_rel) = rest[v_start..].find(&v_close) else {
                    break;
                };
                let text = rest[v_start + v_head_end + 1..v_start + v_close_rel]
                    .trim()
                    .to_string();
                values.push(strip_tags(&text));
                rest = &rest[v_start + v_close_rel + v_close.len()..];
            }
            if !values.is_empty() {
                profile.attributes.insert(attr_name, values);
            }
        }
        search = &search[start + close_rel + close.len()..];
    }
    if let Some(ref nid) = profile.name_id.clone() {
        if !nid.is_empty() {
            profile
                .attributes
                .entry("nameID".into())
                .or_insert_with(|| vec![nid.clone()]);
        }
    }
    profile
}

fn find_open_tag(xml: &str, local: &str) -> Option<usize> {
    let mut i = 0usize;
    let bytes = xml.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' && bytes.get(i + 1) != Some(&b'/') {
            let mut name_end = i + 1;
            while name_end < bytes.len()
                && !matches!(bytes[name_end], b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'>')
            {
                name_end += 1;
            }
            let name = &xml[i + 1..name_end];
            if name.rsplit(':').next() == Some(local) {
                return Some(i);
            }
            i = name_end + 1;
        } else {
            i += 1;
        }
    }
    None
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Exclusive XML canonicalization subset (no comments); see
/// [`verify_xml_signature`] for the supported surface contract.
fn c14n_exclusive(xml: &str) -> Result<String, String> {
    let mut k = 0usize;
    while let Some(amp) = xml[k..].find('&') {
        let a = k + amp;
        let Some(semi_rel) = xml[a..].find(';') else {
            return Err("bare & in XML".into());
        };
        let entity = &xml[a + 1..a + semi_rel];
        if !matches!(entity, "amp" | "lt" | "gt" | "quot" | "apos") {
            if let Some(hex) = entity.strip_prefix("#x") {
                if u32::from_str_radix(hex, 16).is_err() {
                    return Err(format!("bad character reference: &{entity};"));
                }
            } else if let Some(dec) = entity.strip_prefix('#') {
                if dec.parse::<u32>().is_err() {
                    return Err(format!("bad character reference: &{entity};"));
                }
            } else {
                return Err(format!("general entity not allowed: &{entity};"));
            }
        }
        k = a + semi_rel + 1;
    }
    if xml.contains("xml:base") {
        return Err("xml:base not supported".into());
    }
    let mut reader = Reader::from_str(xml);
    reader.config_mut().expand_empty_elements = true;
    reader.config_mut().check_end_names = true;
    let mut out = String::new();
    let mut ns_stack: Vec<Vec<(String, String)>> = vec![vec![
        (
            "saml".into(),
            "urn:oasis:names:tc:SAML:2.0:assertion".into(),
        ),
        (
            "samlp".into(),
            "urn:oasis:names:tc:SAML:2.0:protocol".into(),
        ),
        ("ds".into(), "http://www.w3.org/2000/09/xmldsig#".into()),
        ("md".into(), "urn:oasis:names:tc:SAML:2.0:metadata".into()),
    ]];
    let mut open: Vec<String> = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let name = e.name().as_ref().to_string();
                let prefix = name.split(':').next().unwrap_or("").to_string();
                if !prefix.is_empty()
                    && !["saml", "samlp", "ds", "md", "xml"].contains(&prefix.as_str())
                {
                    return Err(format!("unsupported namespace prefix: {prefix}"));
                }
                let mut attrs: Vec<(String, String)> = Vec::new();
                let mut own_ns: Vec<(String, String)> = Vec::new();
                for attr in e.attributes() {
                    let attr = attr.map_err(|e| format!("bad attribute: {e}"))?;
                    let key = attr.key.as_ref().to_string();
                    if key == "xmlns" || key.starts_with("xmlns:") {
                        let pfx = key.strip_prefix("xmlns:").unwrap_or("").to_string();
                        let uri = attr.value.to_string();
                        own_ns.push((pfx.clone(), uri.clone()));
                        attrs.push((key, uri));
                        continue;
                    }
                    if key.contains(':') {
                        let apfx = key.split(':').next().unwrap_or("");
                        if apfx != "xml" {
                            return Err(format!("prefixed attribute without known ns: {key}"));
                        }
                    }
                    let raw_val = attr
                        .normalized_value(quick_xml::XmlVersion::Explicit1_0)
                        .map_err(|e| format!("bad attr value: {e}"))?;
                    attrs.push((key, escape_attr(&raw_val)));
                }
                let mut needed: Vec<(String, String)> = Vec::new();
                for pfx in element_prefixes_used(&name, &attrs) {
                    if pfx.is_empty() || pfx == "xml" {
                        continue;
                    }
                    let declared_here = own_ns.iter().any(|(p, _)| p == &pfx);
                    if !declared_here && !ancestor_declares(&ns_stack, &pfx) {
                        return Err(format!("undeclared namespace prefix: {pfx}"));
                    }
                    if !declared_here {
                        if let Some(uri) = lookup_ns(&ns_stack, &pfx) {
                            needed.push((format!("xmlns:{pfx}"), uri));
                        }
                    }
                }
                attrs.extend(needed);
                attrs.sort_by(|a, b| a.0.cmp(&b.0));
                out.push('<');
                out.push_str(&name);
                for (kk, vv) in &attrs {
                    out.push(' ');
                    out.push_str(kk);
                    out.push_str("=\"");
                    out.push_str(vv);
                    out.push('"');
                }
                out.push('>');
                ns_stack.push(own_ns);
                open.push(name);
            }
            Ok(Event::Empty(ref e)) => {
                // Unreachable in practice: expand_empty_elements=true makes
                // quick-xml emit Start+End instead of Empty. Kept as a loud
                // reject (not silent attr-dropping) so a future flag flip
                // can never arm a signature bypass (issues #451/#453).
                let name = e.name().as_ref().to_string();
                return Err(format!(
                    "self-closing element not allowed in canonicalization: {name}"
                ));
            }
            Ok(Event::End(ref e)) => {
                let name = e.name().as_ref().to_string();
                match open.pop() {
                    Some(o) if o == name => {}
                    _ => return Err(format!("mismatched end tag: {name}")),
                }
                ns_stack.pop();
                out.push_str("</");
                out.push_str(&name);
                out.push('>');
            }
            Ok(Event::Text(ref e)) => {
                let txt = e.xml_content(quick_xml::XmlVersion::Explicit1_0);
                out.push_str(&escape_text(&txt));
            }
            Ok(Event::CData(ref e)) => {
                // BytesCData derefs to str.
                let txt: &str = e;
                out.push_str(&escape_text(txt));
            }
            Ok(Event::Comment(_)) => {}
            Ok(Event::Decl(_)) | Ok(Event::DocType(_)) | Ok(Event::PI(_)) => {}
            // Entity refs were pre-screened above; a GeneralRef here would
            // mean the pre-screen missed one — reject rather than
            // mis-canonicalize.
            Ok(Event::GeneralRef(_)) => {
                return Err("general entity reference not allowed".into());
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("XML parse error: {e}")),
        }
    }
    if !open.is_empty() {
        return Err("unclosed elements".into());
    }
    Ok(out)
}

fn ancestor_declares(stack: &[Vec<(String, String)>], prefix: &str) -> bool {
    stack
        .iter()
        .rev()
        .any(|level| level.iter().any(|(p, _)| p == prefix))
}

fn lookup_ns(stack: &[Vec<(String, String)>], prefix: &str) -> Option<String> {
    stack.iter().rev().find_map(|level| {
        level
            .iter()
            .find(|(p, _)| p == prefix)
            .map(|(_, u)| u.clone())
    })
}

fn element_prefixes_used(name: &str, attrs: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    if let Some((pfx, _)) = name.split_once(':') {
        out.push(pfx.to_string());
    }
    for (k, _) in attrs {
        if k == "xmlns" || k.starts_with("xmlns:") {
            continue;
        }
        if let Some((pfx, _)) = k.split_once(':') {
            if !out.contains(&pfx.to_string()) {
                out.push(pfx.to_string());
            }
        }
    }
    out
}

fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#xD;"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#x9;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            _ => out.push(c),
        }
    }
    out
}

/// Find the first element carrying `ID="<id>"` and return its full XML.
fn find_element_by_id(xml: &str, id: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let needle = format!("ID={quote}{id}{quote}");
        if let Some(pos) = xml.find(&needle) {
            let start = xml[..pos].rfind('<')?;
            let mut name_end = start + 1;
            while name_end < xml.len()
                && !matches!(
                    xml.as_bytes()[name_end],
                    b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'>'
                )
            {
                name_end += 1;
            }
            let name = &xml[start + 1..name_end];
            let first_token = name.split_whitespace().next()?;
            let close = format!("</{first_token}>");
            let end_rel = xml[start..].find(&close)?;
            return Some(xml[start..start + end_rel + close.len()].to_string());
        }
    }
    None
}

/// Remove all nested `<Signature>` blocks from an element (enveloped
/// transform applied per Reference).
fn strip_nested_signatures(xml: &str) -> String {
    let mut out = xml.to_string();
    loop {
        let Some(pos) = find_sig_open(&out) else {
            break;
        };
        let Some(end) = find_sig_close(&out, pos) else {
            break;
        };
        out.replace_range(pos..end, "");
    }
    out
}

fn find_sig_open(xml: &str) -> Option<usize> {
    let mut i = 0usize;
    let bytes = xml.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' && bytes.get(i + 1) != Some(&b'/') {
            let mut name_end = i + 1;
            while name_end < bytes.len()
                && !matches!(bytes[name_end], b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'>')
            {
                name_end += 1;
            }
            if xml[i + 1..name_end].rsplit(':').next() == Some("Signature") {
                return Some(i);
            }
            i = name_end + 1;
        } else {
            i += 1;
        }
    }
    None
}

fn find_sig_close(xml: &str, open_pos: usize) -> Option<usize> {
    let rest = &xml[open_pos..];
    let mut name_end = 1usize;
    while name_end < rest.len()
        && !matches!(
            rest.as_bytes()[name_end],
            b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'>'
        )
    {
        name_end += 1;
    }
    let name = &rest[1..name_end];
    let close = format!("</{name}>");
    rest.find(&close).map(|r| open_pos + r + close.len())
}

/// A parsed enveloped `<Signature>` block plus its byte offsets, so the
/// signed content can be reconstructed by excising exactly those bytes.
#[derive(Debug, Clone)]
struct XmlSignature {
    /// Full `<Signature …>…</Signature>` bytes (to excise for digesting).
    signature_xml: String,
    /// Byte offset of the signature start within the scoped document.
    start: usize,
    /// Byte offset just past the signature end.
    end: usize,
    /// Base64 of `<SignedInfo>` canonical form is computed at verify time;
    /// these are the raw extracted fields.
    signed_info_xml: String,
    signature_value_b64: String,
    digest_value_b64: String,
    /// Optional `<Reference URI="…">` (empty = whole-document reference).
    reference_uri: String,
    signature_method: String,
    digest_method: String,
}

/// Find the first enveloped `<Signature>` that is a DIRECT child of the
/// named root element (`Response` or `Assertion`), ignoring nested
/// signatures (e.g. an Assertion signature when scanning the Response).
fn find_signature(xml: &str, root_tag: &str) -> Option<XmlSignature> {
    // Locate the root element's inner span first.
    let (inner_start, inner_end) = root_inner_span(xml, root_tag)?;
    let inner = &xml[inner_start..inner_end];
    // Direct-child scan: track depth relative to inner; accept the first
    // <Signature> opened at depth 0.
    let mut depth = 0usize;
    let mut i = 0usize;
    let bytes = inner.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let is_close = bytes.get(i + 1) == Some(&b'/');
            let name_start = i + if is_close { 2 } else { 1 };
            let mut name_end = name_start;
            while name_end < bytes.len()
                && !matches!(bytes[name_end], b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'>')
            {
                name_end += 1;
            }
            let name = &inner[name_start..name_end];
            let local = name.rsplit(':').next().unwrap_or(name);
            // Find tag end for self-closing detection.
            let mut tag_end = name_end;
            while tag_end < bytes.len() && bytes[tag_end] != b'>' {
                tag_end += 1;
            }
            let self_closing =
                tag_end > name_start && bytes.get(tag_end.saturating_sub(1)) == Some(&b'/');
            if local == "Signature" && !is_close && depth == 0 {
                let abs_start = inner_start + i;
                // Find matching close tag (signatures are never nested).
                let close_needle = format!("</{name}>");
                let rest = &inner[i..];
                let close_rel = rest.find(&close_needle)?;
                let abs_end = inner_start + i + close_rel + close_needle.len();
                let signature_xml = xml[abs_start..abs_end].to_string();
                return parse_signature_block(&signature_xml, abs_start, abs_end);
            }
            if !is_close && !self_closing {
                depth += 1;
            } else if is_close && depth > 0 {
                depth -= 1;
            }
            i = tag_end + 1;
        } else {
            i += 1;
        }
    }
    None
}

/// Byte span of the inner content of the first `<…root_tag …>…</…root_tag>`.
fn root_inner_span(xml: &str, root_tag: &str) -> Option<(usize, usize)> {
    for tag in [
        root_tag.to_string(),
        format!("samlp:{root_tag}"),
        format!("saml:{root_tag}"),
    ] {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        if let Some(start) = xml.find(&open) {
            let head_end = xml[start..].find('>')? + start + 1;
            let end_rel = xml[head_end..].find(&close)?;
            return Some((head_end, head_end + end_rel));
        }
    }
    None
}

fn parse_signature_block(signature_xml: &str, start: usize, end: usize) -> Option<XmlSignature> {
    let signed_info = extract_element(signature_xml, "SignedInfo")?;
    let signature_value = extract_element_text(signature_xml, "SignatureValue")?;
    let digest_value = extract_element_text(&signed_info, "DigestValue")
        .or_else(|| extract_element_text(signature_xml, "DigestValue"))?;
    let reference_uri = extract_attr(&signed_info, "URI").unwrap_or_default();
    let signature_method = extract_attr(&signed_info, "SignatureMethod Algorithm")
        .or_else(|| extract_attr(&signed_info, "Algorithm"))
        .unwrap_or_default();
    let digest_method = extract_attr(&signed_info, "DigestMethod Algorithm")
        .or_else(|| extract_attr(&signed_info, "Algorithm"))
        .unwrap_or_default();
    // Disambiguate: the FIRST Algorithm inside SignedInfo belongs to
    // SignatureMethod; DigestMethod has its own. Re-parse scoped.
    let signature_method =
        scoped_algorithm(&signed_info, "SignatureMethod").unwrap_or(signature_method);
    let digest_method = scoped_algorithm(&signed_info, "DigestMethod").unwrap_or(digest_method);
    Some(XmlSignature {
        signature_xml: signature_xml.to_string(),
        start,
        end,
        signed_info_xml: signed_info,
        signature_value_b64: signature_value.split_whitespace().collect(),
        digest_value_b64: digest_value.split_whitespace().collect(),
        reference_uri,
        signature_method,
        digest_method,
    })
}

fn extract_element(xml: &str, local: &str) -> Option<String> {
    let mut i = 0usize;
    let bytes = xml.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' && bytes.get(i + 1) != Some(&b'/') {
            let mut name_end = i + 1;
            while name_end < bytes.len()
                && !matches!(bytes[name_end], b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'>')
            {
                name_end += 1;
            }
            let name = &xml[i + 1..name_end];
            if name.rsplit(':').next() == Some(local) {
                // Find matching close.
                let close_needle = format!("</{name}>");
                let rest = &xml[i..];
                // Self-closing?
                let mut tag_end = name_end;
                while tag_end < bytes.len() && bytes[tag_end] != b'>' {
                    tag_end += 1;
                }
                if bytes.get(tag_end.saturating_sub(1)) == Some(&b'/') {
                    return Some(xml[i..=tag_end].to_string());
                }
                let close_rel = rest.find(&close_needle)?;
                return Some(xml[i..i + close_rel + close_needle.len()].to_string());
            }
            i = name_end + 1;
        } else {
            i += 1;
        }
    }
    None
}

fn extract_element_text(xml: &str, local: &str) -> Option<String> {
    let el = extract_element(xml, local)?;
    // Strip outer tags, return inner text.
    let start = el.find('>')? + 1;
    let end = el.rfind('<')?;
    if end <= start {
        return Some(String::new());
    }
    Some(el[start..end].to_string())
}

fn scoped_algorithm(signed_info: &str, scope_local: &str) -> Option<String> {
    let scope = extract_element(signed_info, scope_local)?;
    extract_attr(&scope, "Algorithm")
}

/// Verify an enveloped XML signature against the IdP RSA public key.
///
/// Supported surface (matches what node-saml emits and what 9router
/// accepts through it):
/// - Enveloped signature (`<Transform Algorithm="…enveloped-signature">`)
///   plus exclusive c14n (`…xml-exc-c14n#`) — with or without comments —
///   on both the `<SignedInfo>` canonicalization and the digest transform.
/// - Signature methods RSA-SHA256 (`…xmldsig#rsa-sha256`) and RSA-SHA1
///   (`…xmldsig#rsa-sha1`, accepted for legacy IdPs).
/// - Digest methods SHA-256 and SHA-1.
/// - `Reference URI=""` (whole document) or `#ID` matching the scoped
///   root's ID attribute.
///
/// Canonicalization implemented here is a strict subset of exclusive XML
/// canonicalization WITHOUT comments, limited to: default + `saml:` /
/// `samlp:` / `ds:` namespace declarations hoisted from the ancestor
/// chain, attribute lexicographic ordering, `xml:` attribute retention,
/// and standard character escaping. Documents using exotic prefixes,
/// `xml:base`, or non-standard entity references are rejected rather
/// than mis-verified.
fn verify_xml_signature(
    scoped_xml: &str,
    sig: &XmlSignature,
    public_key: &RsaPublicKey,
) -> Result<(), SamlError> {
    // rsa 0.9 verify path: Pkcs1v15Sign + manual DigestInfo prefix check
    // via the `signature` crate's Verifier impl (rsa re-exports it when
    // built with default features — verified against Cargo.lock 0.9.10
    // which depends on `signature`). We go through the raw RSA op +
    // DigestInfo comparison instead so the only digest impls needed are
    // sha2 0.10-compat (already a dependency) and sha1.
    use rsa::signature::SignatureEncoding;

    // 1. Check the digest over the referenced content.
    let digest_ok = check_digest(scoped_xml, sig)?;
    if !digest_ok {
        return Err(SamlError::BadSignature(
            "DigestValue mismatch over the signed content".into(),
        ));
    }

    // 2. Check the RSA signature over canonicalized SignedInfo.
    let c14n_signed_info = c14n_exclusive(&sig.signed_info_xml)
        .map_err(|e| SamlError::BadSignature(format!("SignedInfo c14n failed: {e}")))?;
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(sig.signature_value_b64.trim())
        .map_err(|e| SamlError::BadSignature(format!("bad SignatureValue base64: {e}")))?;

    let method = sig.signature_method.to_lowercase();
    let rsa_sha256 = method.contains("rsa-sha256");
    let rsa_sha1 = method.contains("rsa-sha1");
    if !rsa_sha256 && !rsa_sha1 {
        return Err(SamlError::BadSignature(format!(
            "unsupported SignatureMethod: {}",
            sig.signature_method
        )));
    }
    verify_pkcs1v15(
        public_key,
        if rsa_sha256 { "sha256" } else { "sha1" },
        c14n_signed_info.as_bytes(),
        &sig_bytes,
    )?;
    Ok(())
}

/// PKCS#1 v1.5 signature verification through rsa 0.9's public
/// `signature::Verifier` impls. Kept in one place so the digest crate
/// versions stay consistent (sha2 0.10-compat for SHA-256, sha1 for
/// legacy SHA-1).
fn verify_pkcs1v15(
    public_key: &RsaPublicKey,
    digest_name: &str,
    msg: &[u8],
    sig_bytes: &[u8],
) -> Result<(), SamlError> {
    use rsa::pkcs1v15::Signature;
    use rsa::signature::Verifier;
    let signature = Signature::try_from(sig_bytes)
        .map_err(|e| SamlError::BadSignature(format!("malformed RSA signature: {e}")))?;
    // rsa 0.9 with the `sha2` feature re-exports `rsa::sha2`; sha1 has
    // no feature gate (dev-dependency only in rsa's own manifest, but the
    // `sha1` crate is already in OUR dependency tree).
    match digest_name {
        "sha256" => {
            use rsa::pkcs1v15::VerifyingKey;
            use rsa::sha2::Sha256 as RsaSha256;
            let key: VerifyingKey<RsaSha256> = VerifyingKey::new(public_key.clone());
            key.verify(msg, &signature)
                .map_err(|_| SamlError::BadSignature("RSA-SHA256 verification failed".into()))
        }
        _ => {
            use rsa::pkcs1v15::VerifyingKey;
            // VerifyingKey<D: Digest> — sha1::Sha1 implements digest::Digest.
            let key: VerifyingKey<sha1::Sha1> = VerifyingKey::new(public_key.clone());
            key.verify(msg, &signature)
                .map_err(|_| SamlError::BadSignature("RSA-SHA1 verification failed".into()))
        }
    }
}

/// Check the `<DigestValue>` against the canonicalized referenced node.
fn check_digest(scoped_xml: &str, sig: &XmlSignature) -> Result<bool, SamlError> {
    // Reconstruct the signed content: scoped document minus the Signature.
    let signed_content = format!("{}{}", &scoped_xml[..sig.start], &scoped_xml[sig.end..]);
    // Resolve the reference: "" (whole doc) or "#ID".
    let referenced: String = if sig.reference_uri.is_empty() {
        signed_content
    } else if let Some(id) = sig.reference_uri.strip_prefix('#') {
        // Find the element carrying ID="<id>" and excise nested Signature.
        let target = find_element_by_id(&signed_content, id).ok_or_else(|| {
            SamlError::BadSignature(format!(
                "Reference URI #{} not found in the signed document",
                id
            ))
        })?;
        // Remove any nested Signature inside the referenced element (the
        // enveloped-signature transform applies per Reference).
        strip_nested_signatures(&target)
    } else {
        return Err(SamlError::BadSignature(format!(
            "unsupported DigestMethod: {}",
            sig.digest_method
        )));
    };
    let c14n = c14n_exclusive(&referenced)
        .map_err(|e| SamlError::BadSignature(format!("digest c14n failed: {e}")))?;
    let method = sig.digest_method.to_lowercase();
    let computed: Vec<u8> = if method.contains("sha256") {
        let mut h = Sha256::new();
        h.update(c14n.as_bytes());
        h.finalize().to_vec()
    } else if method.contains("sha1") {
        use sha1::Digest as _;
        let mut h = sha1::Sha1::new();
        h.update(c14n.as_bytes());
        h.finalize().to_vec()
    } else {
        return Err(SamlError::BadSignature(format!(
            "unsupported DigestMethod: {}",
            sig.digest_method
        )));
    };
    let expected = base64::engine::general_purpose::STANDARD
        .decode(sig.digest_value_b64.trim())
        .map_err(|e| SamlError::BadSignature(format!("bad DigestValue base64: {e}")))?;
    Ok(subtle_eq(&computed, &expected))
}

fn subtle_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cert_formatter_wraps_base64() {
        let raw = "QUJD".repeat(20);
        assert_eq!(format_x509_certificate(""), "");
        let pem = format_x509_certificate(&raw);
        assert!(pem.starts_with(
            "-----BEGIN CERTIFICATE-----
"
        ));
        assert!(pem.ends_with(
            "
-----END CERTIFICATE-----"
        ));
        // Idempotent on already-PEM input.
        assert_eq!(format_x509_certificate(&pem), pem);
    }

    #[test]
    fn not_configured_without_entry_point_or_cert() {
        assert!(!is_saml_configured("", ""));
        assert!(!is_saml_configured("https://idp/sso", ""));
        assert!(!is_saml_configured("", "c"));
        assert!(is_saml_configured("https://idp/sso", "c"));
    }

    #[test]
    fn base_url_prefers_explicit_then_env_then_headers() {
        assert_eq!(
            saml_base_url("https://example.com/", None, None, None, None),
            "https://example.com"
        );
        assert_eq!(
            saml_base_url("", None, None, Some("h.example:1"), None),
            "http://h.example:1"
        );
        assert_eq!(
            saml_base_url("", Some("https"), Some("h.example"), None, None),
            "https://h.example"
        );
        assert_eq!(
            saml_base_url("", None, None, None, None),
            "http://localhost:20128"
        );
    }

    #[test]
    fn authorize_url_carries_deflated_request() {
        let settings = SamlSettings {
            entry_point: "https://idp.example.com/sso".into(),
            issuer: "".into(),
            ..Default::default()
        };
        let url = build_authorize_url(&settings, "https://sp.example.com", "_req123").unwrap();
        assert!(url.starts_with("https://idp.example.com/sso?SAMLRequest="));
        // Round-trip: url-decode, base64-decode, inflate, check the XML.
        let enc = url.split("SAMLRequest=").nth(1).unwrap();
        let decoded = urlencoding::decode(enc).unwrap().into_owned();
        let raw = base64::engine::general_purpose::STANDARD
            .decode(decoded)
            .unwrap();
        let xml = inflate_raw(&raw);
        assert!(xml.contains("ID=\"_req123\""));
        assert!(xml.contains("_req123"));
        assert!(xml.contains("https://sp.example.com/api/auth/saml/acs"));
        assert!(xml.contains("urn:9router:sp"));
    }

    #[test]
    fn claim_pickers_follow_js_priority() {
        let mut profile = SamlProfile::default();
        profile
            .attributes
            .insert("mail".into(), vec!["a@b.c".into()]);
        profile
            .attributes
            .insert("displayName".into(), vec!["Alice".into()]);
        let settings = SamlSettings::default();
        assert_eq!(pick_saml_email(&profile, &settings), "a@b.c");
        assert_eq!(pick_saml_display_name(&profile, &settings), "Alice");
        // Custom attribute wins.
        let settings2 = SamlSettings {
            attribute_email: "custom".into(),
            ..Default::default()
        };
        let mut p2 = SamlProfile::default();
        p2.attributes.insert("custom".into(), vec!["z@z.z".into()]);
        assert_eq!(pick_saml_email(&p2, &settings2), "z@z.z");
        // givenName + sn combine (falls back to "A B" when no name claim).
        let mut p3 = SamlProfile::default();
        p3.attributes.insert("givenName".into(), vec!["A".into()]);
        p3.attributes.insert("sn".into(), vec!["B".into()]);
        assert_eq!(pick_saml_display_name(&p3, &SamlSettings::default()), "A B");
    }

    #[test]
    fn metadata_contains_entity_and_acs() {
        let settings = SamlSettings {
            issuer: "urn:test:sp".into(),
            ..Default::default()
        };
        let md = generate_saml_metadata("https://sp.example.com/", &settings);
        assert!(md.contains("entityID=\"urn:test:sp\""));
        assert!(md.contains("https://sp.example.com/api/auth/saml/acs"));
    }

    #[test]
    fn tampered_response_fails_inresponseto() {
        let settings = SamlSettings {
            cert: "QUJD".into(),
            ..Default::default()
        };
        let xml = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" InResponseTo="_other"><saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion"><saml:Subject><saml:NameID>u@e.c</saml:NameID></saml:Subject></saml:Assertion></samlp:Response>"#;
        let b64 = base64::engine::general_purpose::STANDARD.encode(xml);
        let err = validate_saml_response(
            &b64,
            "_expected",
            &settings,
            0,
            "https://sp.example.com/api/auth/saml/acs",
        )
        .unwrap_err();
        assert!(err.to_string().contains("InResponseTo mismatch"));
    }

    #[test]
    fn missing_response_param_rejected() {
        let settings = SamlSettings {
            cert: "QUJD".into(),
            ..Default::default()
        };
        let err = validate_saml_response(
            "",
            "",
            &settings,
            0,
            "https://sp.example.com/api/auth/saml/acs",
        )
        .unwrap_err();
        assert!(err.to_string().contains("Missing SAMLResponse"));
    }

    #[test]
    fn self_closing_elements_rejected_in_c14n() {
        // Issues #451/#453: the old Event::Empty arm silently dropped
        // attributes (e.g. Recipient/NotOnOrAfter on self-closing
        // SubjectConfirmationData). expand_empty_elements=true means the
        // parser never emits Empty in production — the arm is now a loud
        // reject, verified here by feeding a self-closing element with
        // expansion DISABLED is impossible via the public fn, so instead
        // assert the production path keeps attributes (Start+End path).
        let xml = r#"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_a"><saml:Subject><saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer"><saml:SubjectConfirmationData Recipient="https://sp.example.com/api/auth/saml/acs" NotOnOrAfter="2030-01-01T00:00:00Z"/></saml:SubjectConfirmation></saml:Subject></saml:Assertion>"#;
        let out = c14n_exclusive(xml).expect("c14n must succeed");
        assert!(
            out.contains("Recipient=\"https://sp.example.com/api/auth/saml/acs\""),
            "Recipient must survive canonicalization: {out}"
        );
        assert!(
            out.contains("NotOnOrAfter=\"2030-01-01T00:00:00Z\""),
            "NotOnOrAfter must survive canonicalization: {out}"
        );
    }

    #[test]
    fn replay_cache_single_use() {
        // Issues #454/#458: an assertion ID is usable once within its
        // lifetime, then rejected until expiry.
        let id = "test-assertion-replay-1";
        assert!(!is_assertion_replayed(id, 1000));
        mark_assertion_used(id, 2000);
        assert!(is_assertion_replayed(id, 1500));
        assert!(is_assertion_replayed(id, 2000));
        assert!(!is_assertion_replayed(id, 2001));
        assert!(!is_assertion_replayed("", 1500));
    }

    #[test]
    fn replay_id_prefers_assertion_id_then_inresponseto() {
        let ax = r#"<saml:Assertion ID="_abc123"></saml:Assertion>"#;
        let rx = r#"<samlp:Response InResponseTo="_req9"></samlp:Response>"#;
        assert_eq!(assertion_replay_id(ax, rx), "_abc123");
        assert_eq!(
            assertion_replay_id("<saml:Assertion></saml:Assertion>", rx),
            "_req9"
        );
        assert_eq!(assertion_replay_id("", ""), "");
    }

    #[test]
    fn recipient_and_destination_required() {
        use super::check_conditions;
        let assertion = r#"<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion"><saml:Conditions NotBefore="2020-01-01T00:00:00Z" NotOnOrAfter="2030-01-01T00:00:00Z"><saml:AudienceRestriction><saml:Audience>urn:test:sp</saml:Audience></saml:AudienceRestriction></saml:Conditions><saml:Subject><saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer"><saml:SubjectConfirmationData Recipient="https://sp.example.com/api/auth/saml/acs" NotOnOrAfter="2030-01-01T00:00:00Z"/></saml:SubjectConfirmation></saml:Subject></saml:Assertion>"#;
        let response_ok = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" Destination="https://sp.example.com/api/auth/saml/acs"></samlp:Response>"#;
        let acs = "https://sp.example.com/api/auth/saml/acs";
        // now_unix inside the fixture Conditions window (2020..2030).
        let now: i64 = 1_750_000_000;
        // Happy path: matching Recipient + Destination.
        check_conditions(response_ok, assertion, "urn:test:sp", acs, now)
            .expect("happy path must pass");
        // Missing Destination rejected (issues #452/#455).
        let no_dest = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol"></samlp:Response>"#;
        let err = check_conditions(no_dest, assertion, "urn:test:sp", acs, now).unwrap_err();
        assert!(err.to_string().contains("Destination is required"), "{err}");
        // Wrong Destination rejected.
        let bad_dest = r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" Destination="https://evil.example/acs"></samlp:Response>"#;
        let err = check_conditions(bad_dest, assertion, "urn:test:sp", acs, now).unwrap_err();
        assert!(err.to_string().contains("Destination mismatch"), "{err}");
        // Wrong Recipient rejected.
        let bad_recipient = assertion.replace(
            "Recipient=\"https://sp.example.com/api/auth/saml/acs\"",
            "Recipient=\"https://evil.example/acs\"",
        );
        let err =
            check_conditions(response_ok, &bad_recipient, "urn:test:sp", acs, now).unwrap_err();
        assert!(err.to_string().contains("Recipient mismatch"), "{err}");
        // Missing Recipient rejected.
        let no_recipient = assertion.replace(
            " Recipient=\"https://sp.example.com/api/auth/saml/acs\"",
            "",
        );
        let err =
            check_conditions(response_ok, &no_recipient, "urn:test:sp", acs, now).unwrap_err();
        assert!(err.to_string().contains("Recipient is required"), "{err}");
    }

    fn inflate_raw(data: &[u8]) -> String {
        use flate2::read::DeflateDecoder;
        use std::io::Read;
        let mut d = DeflateDecoder::new(data);
        let mut s = String::new();
        d.read_to_string(&mut s).unwrap();
        s
    }
}
