//! Smart text filter for MCP tool results. Mirrors `smartFilterText` /
//! `collapseRepeated` from upstream 9router (`src/lib/mcp/stdioSseBridge.js`)
//! byte-for-byte so existing MCP clients receive the same filtered output.
//!
//! The filter is conservative:
//!   1. Skip short payloads (< 2_000 chars) — they're already cheap.
//!   2. Strip a couple of well-known noise lines (`- generic:`, `- text: ""`).
//!   3. Collapse blocks of consecutive same-indent + same-role siblings
//!      (e.g. 50 identical `- listitem` rows → 10 head + omitted-marker + 5 tail).
//!   4. Hard-truncate the result at `MAX_TEXT_CHARS` with an explanatory note.
//!
//! `[ref=eXX]` markers are preserved automatically — we only strip whole
//! lines that don't contain them.

const MAX_TEXT_CHARS: usize = 50_000;
const COLLAPSE_THRESHOLD: usize = 30;
const COLLAPSE_KEEP_HEAD: usize = 10;
const COLLAPSE_KEEP_TAIL: usize = 5;
const MIN_FILTER_LEN: usize = 2_000;

/// Returns a filtered copy of `text` matching the upstream `smartFilterText`
/// behaviour. Returns `None` when the text is short enough to skip — callers
/// can then keep the original reference and avoid an allocation.
pub fn smart_filter_text(text: &str) -> Option<String> {
    if text.len() < MIN_FILTER_LEN {
        return None;
    }

    let stripped = strip_noise_lines(text);
    let collapsed = collapse_repeated(&stripped);

    let final_text = if collapsed.len() > MAX_TEXT_CHARS {
        let head_end = MAX_TEXT_CHARS.saturating_sub(300);
        let head = &collapsed[..head_end];
        format!(
            "{head}\n\n... [truncated {} chars by openproxy bridge. Page is large; ask user to scroll/navigate to a specific section, or click an element with the refs shown above]",
            text.len() - head.len()
        )
    } else {
        collapsed
    };

    if final_text == text {
        None
    } else {
        Some(final_text)
    }
}

fn strip_noise_lines(text: &str) -> String {
    // Mirror upstream regex semantics: strip `- generic:?` and `- text: ""`
    // lines (with arbitrary leading whitespace) but preserve everything else.
    text.lines()
        .filter(|line| !is_noise_line(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_noise_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("- ") {
        // `- generic:` or `- generic` with trailing whitespace only
        if rest == "generic" || rest == "generic:" {
            return true;
        }
        if let Some(after) = rest.strip_prefix("generic") {
            let after = after.strip_prefix(':').unwrap_or(after);
            if after.trim().is_empty() {
                return true;
            }
        }
        // `- text: ""` with trailing whitespace only
        if let Some(after) = rest.strip_prefix("text:") {
            if after.trim() == "\"\"" {
                return true;
            }
        }
    }
    false
}

/// Returns a header (indent, role) for a line if it matches the
/// `^(\s*)-\s*([A-Za-z]+)\b` shape upstream uses to detect siblings.
fn parse_header(line: &str) -> Option<(&str, &str)> {
    let indent_end = line.bytes().take_while(|b| b.is_ascii_whitespace()).count();
    let (indent, rest) = line.split_at(indent_end);

    let rest = rest.strip_prefix("- ").or_else(|| rest.strip_prefix('-'))?;
    let rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());

    let role_end = rest.bytes().take_while(|b| b.is_ascii_alphabetic()).count();
    if role_end == 0 {
        return None;
    }
    let role = &rest[..role_end];
    // upstream uses `\b` — next char must not be alphanumeric/underscore.
    if let Some(c) = rest.as_bytes().get(role_end) {
        let c = *c as char;
        if c.is_ascii_alphanumeric() || c == '_' {
            return None;
        }
    }
    Some((indent, role))
}

fn collapse_repeated(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let Some((indent, role)) = parse_header(line) else {
            out.push(line.to_string());
            i += 1;
            continue;
        };

        // Group: starting at `i`, extend forward through consecutive sibling
        // headers with the same (indent, role), plus any deeper-indented
        // continuation lines belonging to those siblings.
        let mut j = i;
        let nested_prefix_space = format!("{indent} ");
        let nested_prefix_tab = format!("{indent}\t");
        while j < lines.len() {
            let ln = lines[j];
            match parse_header(ln) {
                Some((ind, r)) if ind == indent && r == role => {
                    j += 1;
                    continue;
                }
                _ => {}
            }
            if ln.starts_with(&nested_prefix_space) || ln.starts_with(&nested_prefix_tab) {
                j += 1;
                continue;
            }
            break;
        }

        let group_len = j - i;
        if group_len >= COLLAPSE_THRESHOLD {
            let head_end = find_nth_sibling_end(&lines, i, indent, role, COLLAPSE_KEEP_HEAD);
            let tail_start = find_last_n_sibling_start(&lines, j, indent, role, COLLAPSE_KEEP_TAIL);
            for line in &lines[i..head_end] {
                out.push((*line).to_string());
            }
            let omitted = group_len - COLLAPSE_KEEP_HEAD - COLLAPSE_KEEP_TAIL;
            out.push(format!(
                "{indent}... [{omitted} similar \"{role}\" items omitted by openproxy bridge]"
            ));
            for line in &lines[tail_start..j] {
                out.push((*line).to_string());
            }
        } else {
            for line in &lines[i..j] {
                out.push((*line).to_string());
            }
        }
        i = j;
    }

    out.join("\n")
}

fn find_nth_sibling_end(lines: &[&str], start: usize, indent: &str, role: &str, n: usize) -> usize {
    let mut count = 0;
    for (k, line) in lines.iter().enumerate().skip(start) {
        if let Some((ind, r)) = parse_header(line) {
            if ind == indent && r == role {
                count += 1;
                if count > n {
                    return k;
                }
            }
        }
    }
    lines.len()
}

fn find_last_n_sibling_start(
    lines: &[&str],
    end: usize,
    indent: &str,
    role: &str,
    n: usize,
) -> usize {
    let mut positions: Vec<usize> = Vec::new();
    for (k, line) in lines.iter().enumerate().take(end) {
        if let Some((ind, r)) = parse_header(line) {
            if ind == indent && r == role {
                positions.push(k);
            }
        }
    }
    if positions.len() > n {
        positions[positions.len() - n]
    } else {
        end
    }
}

/// Apply [`smart_filter_text`] to every `result.content[*].text` entry in a
/// JSON-RPC line. Returns the (possibly re-serialised) line. Invalid JSON or
/// frames without a `result.content[]` array pass through untouched.
pub fn filter_jsonrpc_frame(line: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(line) else {
        return line.to_string();
    };
    let mut mutated = false;
    if let Some(content) = value
        .get_mut("result")
        .and_then(|v| v.get_mut("content"))
        .and_then(|v| v.as_array_mut())
    {
        for item in content.iter_mut() {
            let Some(obj) = item.as_object_mut() else {
                continue;
            };
            if obj.get("type").and_then(|v| v.as_str()) != Some("text") {
                continue;
            }
            let Some(text) = obj.get("text").and_then(|v| v.as_str()) else {
                continue;
            };
            if let Some(filtered) = smart_filter_text(text) {
                obj.insert("text".to_string(), serde_json::Value::String(filtered));
                mutated = true;
            }
        }
    }
    if mutated {
        serde_json::to_string(&value).unwrap_or_else(|_| line.to_string())
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_returns_none() {
        assert!(smart_filter_text("hello").is_none());
        assert!(smart_filter_text(&"x".repeat(1_999)).is_none());
    }

    #[test]
    fn noise_line_stripping() {
        assert!(is_noise_line("- generic"));
        assert!(is_noise_line("- generic:"));
        assert!(is_noise_line("  - generic:"));
        assert!(is_noise_line("- text: \"\""));
        assert!(is_noise_line("    - text: \"\""));
        assert!(!is_noise_line("- text: \"hello\""));
        assert!(!is_noise_line("- button: clicky"));
    }

    #[test]
    fn parse_header_detects_role() {
        assert_eq!(parse_header("- button: foo"), Some(("", "button")));
        assert_eq!(parse_header("  - listitem"), Some(("  ", "listitem")));
        assert_eq!(parse_header("    - link [ref=e42]"), Some(("    ", "link")));
        assert_eq!(parse_header("not a header"), None);
        assert_eq!(parse_header("- 1234"), None);
        // Underscore is a word char, so the `\b` between `e` and `_` does
        // NOT match — mirrors upstream JS regex behaviour.
        assert_eq!(parse_header("- snake_case"), None);
    }

    #[test]
    fn collapses_long_runs_of_siblings() {
        // 50 sibling listitems at the top level → upstream collapses to
        // 10 head + omitted marker + 5 tail.
        let lines: Vec<String> = (0..50).map(|i| format!("- listitem item-{i}")).collect();
        let input = lines.join("\n");
        // Pad to MIN_FILTER_LEN so smart_filter_text actually runs.
        let padded = format!("{input}\n{}", "padding-".repeat(300));

        let out = smart_filter_text(&padded).expect("should filter");
        let count = out.lines().filter(|l| l.starts_with("- listitem")).count();
        // 10 head + 5 tail = 15 listitem lines; the marker doesn't match.
        assert_eq!(count, 15);
        assert!(out.contains("35 similar \"listitem\" items omitted"));
    }

    #[test]
    fn short_runs_arent_collapsed() {
        // 20 sibling listitems is below the 30-threshold so no collapse runs;
        // the filter returns None because nothing changed.
        let lines: Vec<String> = (0..20).map(|i| format!("- listitem item-{i}")).collect();
        let body = lines.join("\n");
        let padded = format!("{body}\n{}", "padding-".repeat(300));
        assert!(smart_filter_text(&padded).is_none());
    }

    #[test]
    fn truncates_at_max_chars() {
        let huge = "X".repeat(80_000);
        let out = smart_filter_text(&huge).expect("should filter");
        assert!(out.len() <= MAX_TEXT_CHARS + 300);
        assert!(out.contains("truncated"));
        assert!(out.contains("openproxy bridge"));
    }

    #[test]
    fn frame_filter_rewrites_only_long_text() {
        let text = (0..60)
            .map(|i| format!("- listitem item-{i}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
            + &"padding-".repeat(300);
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "content": [
                    { "type": "text", "text": text },
                    { "type": "text", "text": "short — keep me as-is" },
                    { "type": "image", "data": "..." }
                ]
            }
        });
        let line = serde_json::to_string(&frame).unwrap();
        let filtered = filter_jsonrpc_frame(&line);
        let parsed: serde_json::Value = serde_json::from_str(&filtered).unwrap();
        let arr = parsed["result"]["content"].as_array().unwrap();
        assert!(arr[0]["text"]
            .as_str()
            .unwrap()
            .contains("similar \"listitem\" items omitted"));
        assert_eq!(arr[1]["text"].as_str().unwrap(), "short — keep me as-is");
        assert_eq!(arr[2]["data"].as_str().unwrap(), "...");
    }

    #[test]
    fn frame_filter_passes_through_bad_json() {
        assert_eq!(filter_jsonrpc_frame("{not json}"), "{not json}");
        assert_eq!(filter_jsonrpc_frame(""), "");
    }

    #[test]
    fn frame_filter_passes_through_when_no_content() {
        let frame = r#"{"jsonrpc":"2.0","method":"ping"}"#;
        assert_eq!(filter_jsonrpc_frame(frame), frame);
    }
}
