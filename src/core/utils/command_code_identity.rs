//! Port of `open-sse/shared/commandCodeHeaders.js` (9router v0.5.112) —
//! Command Code request identity shared by the request translator (body
//! `threadId`) and the executor (`x-session-id` / `x-project-slug` headers).
//!
//! Mirrors `buildCommandAuthHeaders()` of the official CLI
//! (command-code@1.88.0):
//!  - `x-project-slug`: `slugify(process.cwd())` — 9router reports its own
//!    working dir, which is what we do too (the body's
//!    `config.workingDir` carries the same value upstream).
//!  - `x-session-id`: conversation-stable id shared with the body's
//!    `threadId`, hashed into a deterministic UUID shape via
//!    [`to_command_code_session_uuid`].

use std::sync::LazyLock;

use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

/// Static identity headers of command-code@1.88.0 `buildCommandAuthHeaders()`
/// (9router providers/registry/commandcode.js `transport.headers`). The
/// dynamic `x-project-slug` / `x-session-id` are added per request.
pub const COMMAND_CODE_STATIC_HEADERS: &[(&str, &str)] = &[
    ("User-Agent", "cli"),
    ("x-command-code-version", "1.88.0"),
    ("x-cli-environment", "production"),
    ("x-cli-surface", "cli"),
    ("x-taste-learning", "false"),
];

/// The official CLI version these headers impersonate.
pub const COMMAND_CODE_CLI_VERSION: &str = "1.88.0";

/// Subset of @sindresorhus/slugify's built-in replacements + the transliterate
/// table entries that Unicode NFD decomposition does not cover
/// (commandCodeHeaders.js:15-22).
fn transliterate_char(c: char) -> &'static str {
    match c {
        '&' => " and ",
        '🦄' => " unicorn ",
        '♥' => " love ",
        'ß' => "ss",
        'ẞ' => "Ss",
        'ä' => "ae",
        'Ä' => "Ae",
        'ö' => "oe",
        'Ö' => "Oe",
        'ü' => "ue",
        'Ü' => "Ue",
        'Æ' => "AE",
        'æ' => "ae",
        'Ø' => "O",
        'ø' => "o",
        'Œ' => "OE",
        'œ' => "oe",
        'Ð' => "D",
        'ð' => "d",
        'Đ' => "D",
        'đ' => "d",
        'Þ' => "TH",
        'þ' => "th",
        'Ł' => "L",
        'ł' => "l",
        'ı' => "i",
        _ => "",
    }
}

/// transliterate(value): apply the replacement table, then NFD-decompose and
/// strip combining marks U+0300..=U+036F (commandCodeHeaders.js:24-28).
fn transliterate(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        let replacement = transliterate_char(c);
        if replacement.is_empty() {
            out.push(c);
        } else {
            out.push_str(replacement);
        }
    }
    out.as_str()
        .nfd()
        .filter(|c| !('\u{0300}'..='\u{036f}').contains(c))
        .collect()
}

/// decamelize(): the @sindresorhus/slugify default splitting rules
/// (commandCodeHeaders.js:31-37), applied in the same order as the JS
/// `.replace()` chain.
fn decamelize(value: &str) -> String {
    static RULES: LazyLock<[regex::Regex; 4]> = LazyLock::new(|| {
        [
            regex::Regex::new(r"([A-Z]{2,})(\d+)").unwrap(),
            regex::Regex::new(r"([a-z\d]+)([A-Z]{2,})").unwrap(),
            regex::Regex::new(r"([a-z\d])([A-Z])").unwrap(),
            regex::Regex::new(r"([A-Z]+)([A-Z][a-rt-z\d]+)").unwrap(),
        ]
    });
    let mut out = value.to_string();
    for re in RULES.iter() {
        out = re.replace_all(&out, "$1 $2").into_owned();
    }
    out
}

/// `slugify(path)` with @sindresorhus/slugify defaults:
/// "/home/u/My Project" → "home-u-my-project" (commandCodeHeaders.js:40-43).
pub fn slugify_path(value: &str) -> String {
    let decamelized = decamelize(&transliterate(value));
    let lower = decamelized.to_lowercase();
    let mut slug = String::with_capacity(lower.len());
    let mut last_dash = true; // leading dashes are trimmed anyway
    for c in lower.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_end_matches('-').to_string()
}

/// 9router `COMMAND_CODE_WORKING_DIR` = `process.cwd()`.
pub fn command_code_working_dir() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

/// 9router `COMMAND_CODE_PROJECT_SLUG` = `slugifyPath(process.cwd())`,
/// computed once per process.
pub fn command_code_project_slug() -> &'static str {
    static SLUG: LazyLock<String> = LazyLock::new(|| slugify_path(&command_code_working_dir()));
    SLUG.as_str()
}

/// Deterministic UUID-shaped id derived from a session id
/// (`toCommandCodeSessionUuid`, commandCodeHeaders.js:48-52). With no session
/// id, returns a fresh random UUID — matching the JS `randomUUID()` fallback.
///
/// The hash layout mirrors the JS exactly: `sha256(sessionId)` hex, version
/// nibble forced to `4`, variant nibble forced to `8|9|a|b`.
pub fn to_command_code_session_uuid(session_id: Option<&str>) -> String {
    let Some(session_id) = session_id.filter(|s| !s.is_empty()) else {
        return Uuid::new_v4().to_string();
    };
    let digest = Sha256::digest(session_id.as_bytes());
    let h = digest.iter().fold(String::with_capacity(64), |mut acc, b| {
        use std::fmt::Write;
        let _ = write!(acc, "{b:02x}");
        acc
    });
    // h[16] is a hex char; variant = (parseInt(h[16],16) & 0x3) | 0x8
    let variant_nibble = h.as_bytes()[16];
    let variant_val = (variant_nibble as char).to_digit(16).unwrap_or(0) & 0x3 | 0x8;
    let variant_char = char::from_digit(variant_val, 16).unwrap_or('8');
    format!(
        "{}-{}-4{}-{}{}-{}",
        &h[0..8],
        &h[8..12],
        &h[13..16],
        variant_char,
        &h[17..20],
        &h[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic_path() {
        assert_eq!(slugify_path("/home/u/My Project"), "home-u-my-project");
        assert_eq!(slugify_path("."), "");
        assert_eq!(
            slugify_path("/data/projects/openProxy"),
            "data-projects-open-proxy"
        );
    }

    #[test]
    fn slugify_decamelizes() {
        assert_eq!(slugify_path("myHTTPServer"), "my-http-server");
        assert_eq!(slugify_path("ABCDef"), "abc-def");
    }

    #[test]
    fn slugify_transliterates() {
        assert_eq!(slugify_path("/tmp/café"), "tmp-cafe");
        assert_eq!(slugify_path("/tmp/straße"), "tmp-strasse");
        assert_eq!(slugify_path("a&b"), "a-and-b");
    }

    #[test]
    fn session_uuid_is_deterministic_and_uuid_shaped() {
        let a = to_command_code_session_uuid(Some("sess-123"));
        let b = to_command_code_session_uuid(Some("sess-123"));
        assert_eq!(a, b);
        assert_eq!(a.len(), 36);
        // version nibble = 4, variant nibble in {8,9,a,b}
        assert_eq!(&a[14..15], "4");
        assert!(matches!(a.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
        // different input → different id
        assert_ne!(a, to_command_code_session_uuid(Some("sess-456")));
    }

    #[test]
    fn session_uuid_without_session_is_random_uuid() {
        let a = to_command_code_session_uuid(None);
        let b = to_command_code_session_uuid(Some(""));
        assert_ne!(a, b);
        assert!(Uuid::parse_str(&a).is_ok());
    }

    #[test]
    fn project_slug_is_nonempty() {
        assert!(!command_code_project_slug().is_empty());
    }
}
