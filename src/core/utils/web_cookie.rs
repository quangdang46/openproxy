//! Cookie-input normalization for web-cookie providers — port of
//! `open-sse/utils/webCookieAuth.js` (9router v0.5.112) plus the identity
//! helpers from `open-sse/services/webIdentity.js` used by the connection
//! probes.
//!
//! Accepts whatever the user pasted (bare value, `name=value`, full DevTools
//! blob, or a cookie-editor JSON array) and produces a `Cookie` header
//! string.

use std::collections::HashMap;

/// `stripCookieInputPrefix`: trim, drop a leading `Bearer ` and a leading
/// `Cookie:` scheme (both case-insensitive, JS `/^bearer\s+/i` /
/// `/^cookie:/i`).
pub fn strip_cookie_input_prefix(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let without_bearer = if trimmed
        .get(..7)
        .map(|p| p.eq_ignore_ascii_case("bearer "))
        .unwrap_or(false)
    {
        trimmed[7..].trim_start().to_string()
    } else {
        trimmed.to_string()
    };
    if without_bearer
        .get(..7)
        .map(|p| p.eq_ignore_ascii_case("cookie:"))
        .unwrap_or(false)
    {
        without_bearer[7..].trim().to_string()
    } else {
        without_bearer
    }
}

/// `parseJsonCookiesToHeader`: parse a JSON array of cookie objects
/// (`[{"name":"sso","value":"..."}]`, cookie-editor / DevTools export) into a
/// `Cookie` header string. Returns `None` when the input is not JSON
/// (pass-through); `Some("")` for an empty array; `Err` on a malformed entry.
pub fn parse_json_cookies_to_header(raw: &str) -> Result<Option<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || !trimmed.starts_with('[') {
        return Ok(None);
    }
    let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let arr = match parsed.as_array() {
        Some(a) => a,
        None => return Ok(None),
    };
    if arr.is_empty() {
        return Ok(Some(String::new()));
    }
    let mut parts = Vec::with_capacity(arr.len());
    for (i, entry) in arr.iter().enumerate() {
        let name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                format!("Invalid cookie JSON at index {i}: missing required field 'name'")
            })?;
        let value = entry.get("value").and_then(|v| v.as_str()).ok_or_else(|| {
            format!("Invalid cookie JSON at index {i}: missing required field 'value'")
        })?;
        parts.push(format!("{name}={value}"));
    }
    Ok(Some(parts.join("; ")))
}

/// `normalizeSessionCookieHeader`: whatever the user pasted → a `Cookie`
/// header value. JSON cookie arrays are expanded; a blob containing `=` is
/// passed through; a bare value becomes `<defaultCookieName>=<value>`.
pub fn normalize_session_cookie_header(raw: &str, default_cookie_name: &str) -> String {
    let stripped = strip_cookie_input_prefix(raw);
    if stripped.is_empty() {
        return String::new();
    }
    if let Ok(Some(header)) = parse_json_cookies_to_header(&stripped) {
        return header;
    }
    if stripped.contains('=') {
        return stripped;
    }
    format!("{default_cookie_name}={stripped}")
}

/// `cookieJar` (webIdentity.js): parse a `;`-separated cookie header into a
/// name → value map. Malformed segments are skipped.
pub fn cookie_jar(raw: &str) -> HashMap<String, String> {
    let mut jar = HashMap::new();
    for part in raw.split(';') {
        if let Some(eq) = part.find('=') {
            if eq == 0 {
                continue;
            }
            let name = part[..eq].trim();
            let value = part[eq + 1..].trim();
            if !name.is_empty() {
                jar.insert(name.to_string(), value.to_string());
            }
        }
    }
    jar
}

/// `extractCookieValue` (webCookieAuth.js): pull one cookie's value out of a
/// bare value, `name=value` pair, or a full `;`-separated cookie blob.
/// Returns "" when a blob lacks the named cookie.
pub fn extract_cookie_value(raw: &str, cookie_name: &str) -> String {
    let trimmed = strip_cookie_input_prefix(raw);
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.contains(';') {
        for part in trimmed.split(';') {
            let part = part.trim();
            if let Some((name, value)) = part.split_once('=') {
                if name.trim() == cookie_name {
                    return value.trim().to_string();
                }
            }
        }
        return String::new();
    }
    let prefix = format!("{cookie_name}=");
    if let Some(rest) = trimmed.strip_prefix(&prefix) {
        return rest.to_string();
    }
    trimmed
}

/// `buildGrokCookieHeader` (webCookieAuth.js): always emits `sso=<v>`; forwards
/// the paired `sso-rw`, `cf_clearance` and `__cf_bm` cookies when the pasted
/// blob carries them — Grok's anti-bot rejects `sso` without `sso-rw`, and
/// Cloudflare needs `cf_clearance`/`__cf_bm` to match this machine.
pub fn build_grok_cookie_header(raw: &str) -> String {
    let sso = extract_cookie_value(raw, "sso");
    if sso.is_empty() {
        return String::new();
    }
    let mut parts = vec![format!("sso={sso}")];
    for name in ["sso-rw", "cf_clearance", "__cf_bm"] {
        let value = extract_cookie_value(raw, name);
        if !value.is_empty() {
            parts.push(format!("{name}={value}"));
        }
    }
    parts.join("; ")
}

/// `decodeJwtPayload` (webIdentity.js): decode the payload segment of a JWT
/// (or `base64-` prefixed token). Returns `None` on any parse failure.
pub fn decode_jwt_payload(token: &str) -> Option<serde_json::Value> {
    use base64::{engine::general_purpose::URL_SAFE, Engine as _};
    let b64 = token
        .trim()
        .trim_start_matches("base64-")
        .split('.')
        .nth(1)?;
    if b64.is_empty() {
        return None;
    }
    // JS pads with '=' to a multiple of 4 before decoding.
    let mut padded = b64.to_string();
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    let bytes = URL_SAFE.decode(padded).ok()?;
    serde_json::from_slice(&bytes).ok()
}
