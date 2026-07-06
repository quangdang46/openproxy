/// Cline Auth Module
///
/// Provides utilities for handling Cline authentication tokens,
/// including stripping the "workos_" prefix that Cline uses for
/// WorkOS-proxied tokens.
///
/// Strips the "workos_" prefix from a token if present.
/// Returns the remainder of the token after the prefix, or the original
/// token if no prefix is found.
///
/// # Examples
///
/// ```
/// use openproxy::core::auth::cline_auth::strip_workos_prefix;
///
/// assert_eq!(strip_workos_prefix("workos_sk-abc123"), "sk-abc123");
/// assert_eq!(strip_workos_prefix("sk-abc123"), "sk-abc123");
/// assert_eq!(strip_workos_prefix(""), "");
/// ```
pub fn strip_workos_prefix(token: &str) -> &str {
    token.strip_prefix("workos_").unwrap_or(token)
}

/// A basic validator for a Cline authentication token.
///
/// A valid Cline token must not be empty and must contain at least
/// one non-whitespace character.
///
/// # Examples
///
/// ```
/// use openproxy::core::auth::cline_auth::validate_cline_token;
///
/// assert!(validate_cline_token("workos_sk-abc123"));
/// assert!(validate_cline_token("sk-abc123"));
/// assert!(!validate_cline_token(""));
/// assert!(!validate_cline_token("   "));
/// ```
pub fn validate_cline_token(token: &str) -> bool {
    let trimmed = token.trim();
    !trimmed.is_empty()
}

/// Builds a Bearer authorization header value from a Cline token.
///
/// The token is first passed through [`strip_workos_prefix`] to remove
/// any "workos_" prefix before constructing the header.
///
/// # Examples
///
/// ```
/// use openproxy::core::auth::cline_auth::build_cline_auth_header;
///
/// assert_eq!(build_cline_auth_header("workos_sk-abc123"), "Bearer sk-abc123");
/// assert_eq!(build_cline_auth_header("sk-abc123"), "Bearer sk-abc123");
/// ```
pub fn build_cline_auth_header(token: &str) -> String {
    let clean = strip_workos_prefix(token);
    format!("Bearer {clean}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_workos_prefix_removes_prefix() {
        assert_eq!(strip_workos_prefix("workos_sk-abc123"), "sk-abc123");
        assert_eq!(
            strip_workos_prefix("workos_sk-machine-key-crc"),
            "sk-machine-key-crc"
        );
    }

    #[test]
    fn strip_workos_prefix_passthrough_without_prefix() {
        assert_eq!(strip_workos_prefix("sk-abc123"), "sk-abc123");
        assert_eq!(strip_workos_prefix(""), "");
    }

    #[test]
    fn strip_workos_prefix_only_strips_at_start() {
        assert_eq!(
            strip_workos_prefix("not-a-workos_prefix"),
            "not-a-workos_prefix"
        );
    }

    #[test]
    fn validate_cline_token_accepts_valid_tokens() {
        assert!(validate_cline_token("workos_sk-abc123"));
        assert!(validate_cline_token("sk-abc123"));
        assert!(validate_cline_token("a"));
    }

    #[test]
    fn validate_cline_token_rejects_empty_or_whitespace() {
        assert!(!validate_cline_token(""));
        assert!(!validate_cline_token("   "));
        assert!(!validate_cline_token("\t\n"));
    }

    #[test]
    fn build_cline_auth_header_strips_prefix_and_formats_bearer() {
        assert_eq!(
            build_cline_auth_header("workos_sk-abc123"),
            "Bearer sk-abc123"
        );
        assert_eq!(build_cline_auth_header("sk-abc123"), "Bearer sk-abc123");
        assert_eq!(build_cline_auth_header(""), "Bearer ");
    }
}
