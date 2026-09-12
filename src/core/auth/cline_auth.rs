/// Cline Auth Module
///
/// Port of 9router `open-sse/shared/clineAuth.js` (v0.5.75, incl. commit
/// f6e7cabe): builds the `Authorization` header for Cline/ClinePass.
/// Cline OAuth access tokens are WorkOS JWTs and must carry the `workos:`
/// prefix; ClinePass API keys (e.g. `clp_…`) are opaque strings and must be
/// sent verbatim — prefixing them makes api.cline.bot reject the request
/// with HTTP 401.
///
/// Returns the token with the `workos:` prefix when required: an existing
/// `workos:` prefix (case-insensitive) is never doubled, bare WorkOS JWTs
/// (`eyJ…`) get prefixed, and API keys / other opaque tokens pass through
/// untouched.
///
/// # Examples
///
/// ```
/// use openproxy::core::auth::cline_auth::get_cline_access_token;
///
/// assert_eq!(
///     get_cline_access_token("workos:eyJhbGciOiJSUzI1NiJ9.eyJwYXAiJ9"),
///     "workos:eyJhbGciOiJSUzI1NiJ9.eyJwYXAiJ9"
/// );
/// assert_eq!(
///     get_cline_access_token("eyJhbGciOiJSUzI1NiJ9.eyJwYXAiJ9"),
///     "workos:eyJhbGciOiJSUzI1NiJ9.eyJwYXAiJ9"
/// );
/// assert_eq!(
///     get_cline_access_token("clp_1234567890abcdef"),
///     "clp_1234567890abcdef"
/// );
/// ```
pub fn get_cline_access_token(token: &str) -> String {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.to_lowercase().starts_with("workos:") {
        return trimmed.to_string();
    }
    // Cline OAuth access tokens are WorkOS JWTs (base64url `eyJ…` header).
    // ClinePass API keys (category "apikey", e.g. `clp_…`) are NOT JWTs and must
    // be sent verbatim — prefixing them with `workos:` makes the Cline API reject
    // the request with HTTP 401 ("Please make sure you're using the latest
    // version of Cline and re-authenticate your Cline account.").
    if is_workos_jwt(trimmed) {
        return format!("workos:{trimmed}");
    }
    trimmed.to_string()
}

/// Builds a Bearer authorization header value from a Cline token.
///
/// Returns an empty string when the token is empty (9router
/// `getClineAuthorizationHeader`).
///
/// # Examples
///
/// ```
/// use openproxy::core::auth::cline_auth::get_cline_authorization_header;
///
/// assert_eq!(get_cline_authorization_header("clp_abc"), "Bearer clp_abc");
/// assert_eq!(
///     get_cline_authorization_header("eyJpeg.eyJbG"),
///     "Bearer workos:eyJpeg.eyJbG"
/// );
/// assert_eq!(
///     get_cline_authorization_header("workos:eyJpeg.eyJbG"),
///     "Bearer workos:eyJpeg.eyJbG"
/// );
/// assert_eq!(get_cline_authorization_header(""), "");
/// ```
pub fn get_cline_authorization_header(token: &str) -> String {
    let access_token = get_cline_access_token(token);
    if access_token.is_empty() {
        return String::new();
    }
    format!("Bearer {access_token}")
}

/// Reports whether a token looks like a WorkOS JWT (`eyJ…`, i.e. a base64url
/// JWT header): mirrors the 9router `/^eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/`
/// test in `getClineAccessToken`.
fn is_workos_jwt(token: &str) -> bool {
    let bytes = token.as_bytes();
    if bytes.len() < 5 || !token.starts_with("eyJ") {
        return false;
    }
    let is_b64url = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
    let mut i = 3;
    while i < bytes.len() && is_b64url(bytes[i]) {
        i += 1;
    }
    if i == 3 || i >= bytes.len() || bytes[i] != b'.' {
        return false;
    }
    i += 1;
    i < bytes.len() && is_b64url(bytes[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_an_existing_workos_prefix() {
        // Mirrors tests/unit/cline-auth.test.js.
        let token = "workos:eyJhbGciOiJSUzI1NiJ9.eyJwYXAiJ9";
        assert_eq!(get_cline_access_token(token), token);
        assert_eq!(get_cline_access_token(&format!("  {token}  ")), token);
    }

    #[test]
    fn prefixes_a_bare_workos_jwt() {
        // Mirrors tests/unit/cline-auth.test.js.
        let jwt = "eyJhbGciOiJSUzI1NiJ9.eyJwYXAiJ9";
        assert_eq!(get_cline_access_token(jwt), format!("workos:{jwt}"));
    }

    #[test]
    fn does_not_prefix_clinepass_api_keys() {
        // Mirrors tests/unit/cline-auth.test.js: ClinePass API keys are
        // opaque strings; sending them as `workos:clp_…` makes api.cline.bot
        // respond 401.
        assert_eq!(
            get_cline_access_token("clp_1234567890abcdef"),
            "clp_1234567890abcdef"
        );
        assert_eq!(get_cline_access_token("sk-9r-abcdef"), "sk-9r-abcdef");
        assert_eq!(get_cline_access_token(""), "");
        assert_eq!(get_cline_access_token("   "), "");
    }

    #[test]
    fn authorization_header_builds_bearer_without_double_prefixing() {
        // Mirrors tests/unit/cline-auth.test.js.
        assert_eq!(get_cline_authorization_header("clp_abc"), "Bearer clp_abc");
        assert_eq!(
            get_cline_authorization_header("eyJpeg.eyJbG"),
            "Bearer workos:eyJpeg.eyJbG"
        );
        assert_eq!(
            get_cline_authorization_header("workos:eyJpeg.eyJbG"),
            "Bearer workos:eyJpeg.eyJbG"
        );
    }
}
