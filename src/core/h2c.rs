//! IntelliJ JBR h2c-upgrade handling (9router custom-server.js parity, P1-E5).
//!
//! JetBrains Runtime 25 clients (JetBrains AI Assistant / JBR25-based LLM
//! clients) send an HTTP/2-cleartext (`Upgrade: h2c`) preamble that an
//! HTTP/1.1 server would otherwise reject. 9router's custom server detects
//! the upgrade, drains the request body, replays it through the normal
//! HTTP/1.1 handler with `connection: close`, and removes the upgrade headers
//! so the client falls back to plain HTTP/1.1.
//!
//! This module contains the pure, testable pieces of that downgrade — the
//! h2c detection predicate, content-length validation, and the request-header
//! cleaning rules. The transport wiring (a hyper accept loop that intercepts
//! `upgrade` before the axum service) is a tracked follow-up.

/// True when an `Upgrade` header value requests h2c (case-insensitive).
/// 9router custom-server.js: `String(req.headers.upgrade || "").toLowerCase() === "h2c"`.
pub fn is_h2c_upgrade(upgrade_header: Option<&str>) -> bool {
    upgrade_header
        .map(|v| v.trim().eq_ignore_ascii_case("h2c"))
        .unwrap_or(false)
}

/// Validate a `content-length` value for the h2c downgrade replay.
/// 9router custom-server.js: `Number.isSafeInteger(contentLength) && contentLength >= 0`.
/// Returns the parsed length, or `None` when unsafe.
pub fn h2c_content_length(value: Option<&str>) -> Option<u64> {
    let value = value?;
    let trimmed = value.trim();
    // Reject empty / non-numeric / negative.
    let parsed: i128 = trimmed.parse().ok()?;
    if parsed < 0 {
        return None;
    }
    // Reject unsafe integers (beyond Number.MAX_SAFE_INTEGER in JS terms).
    if parsed > i64::MAX as i128 {
        return None;
    }
    Some(parsed as u64)
}

/// Clean request headers for the h2c downgrade replay.
/// 9router custom-server.js: delete `upgrade` and `http2-settings`,
/// set `connection: close`.
///
/// Takes an iterator of `(name, value)` pairs and returns the cleaned list.
/// Names are matched case-insensitively (HTTP/2 uses lowercase `:authority`
/// style; the JS uses lowercase literal keys).
pub fn h2c_replay_headers<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        if lower == "upgrade" || lower == "http2-settings" {
            continue;
        }
        if lower == "connection" {
            // Force close so the client falls back to plain HTTP/1.1.
            out.push((name.to_string(), "close".to_string()));
            continue;
        }
        out.push((name.to_string(), value.to_string()));
    }
    // Ensure a connection header exists (add if none survived).
    if !out
        .iter()
        .any(|(n, _)| n.eq_ignore_ascii_case("connection"))
    {
        out.push(("connection".to_string(), "close".to_string()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_h2c_upgrade() {
        assert!(is_h2c_upgrade(Some("h2c")));
        assert!(is_h2c_upgrade(Some("H2C")));
        assert!(is_h2c_upgrade(Some("  h2c  ")));
        assert!(!is_h2c_upgrade(Some("websocket")));
        assert!(!is_h2c_upgrade(None));
    }

    #[test]
    fn content_length_validated() {
        assert_eq!(h2c_content_length(Some("0")), Some(0));
        assert_eq!(h2c_content_length(Some("123")), Some(123));
        assert_eq!(h2c_content_length(Some("1024")), Some(1024));
        // Negative / non-numeric / huge / missing → None.
        assert_eq!(h2c_content_length(Some("-1")), None);
        assert_eq!(h2c_content_length(Some("abc")), None);
        assert_eq!(h2c_content_length(Some("99999999999999999999999999")), None);
        assert_eq!(h2c_content_length(None), None);
    }

    #[test]
    fn replay_headers_strip_upgrade_and_force_close() {
        let headers = vec![
            ("Host", "127.0.0.1:4623"),
            ("Upgrade", "h2c"),
            ("HTTP2-Settings", "AAMAAABkAAQA"),
            ("Connection", "Upgrade, HTTP2-Settings"),
            ("Content-Length", "42"),
        ];
        let cleaned = h2c_replay_headers(headers);
        // upgrade / http2-settings stripped; connection forced to close.
        assert!(!cleaned
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("upgrade")));
        assert!(!cleaned
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("http2-settings")));
        let conn = cleaned
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("connection"))
            .expect("connection header present");
        assert_eq!(conn.1, "close");
        // Other headers preserved.
        assert!(cleaned
            .iter()
            .any(|(n, v)| n == "Host" && v == "127.0.0.1:4623"));
        assert!(cleaned
            .iter()
            .any(|(n, v)| n == "Content-Length" && v == "42"));
    }

    #[test]
    fn replay_headers_adds_connection_when_absent() {
        let headers = vec![("Host", "x")];
        let cleaned = h2c_replay_headers(headers);
        let conn = cleaned
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("connection"))
            .expect("connection header added");
        assert_eq!(conn.1, "close");
    }
}
