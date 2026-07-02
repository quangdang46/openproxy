use once_cell::sync::Lazy;
use regex::Regex;

use crate::core::rtk::constants::*;
use crate::core::rtk::filters::{
    build_output_impl, dedup_log_impl, find_impl, git_diff_impl, git_status_impl, grep_impl,
    json_summary_impl, ls_impl, read_numbered_impl, search_list_impl, smart_truncate_impl,
    test_runner_impl, tree_impl, READ_NUMBERED_LINE_RE, SEARCH_LIST_HEADER_RE,
};

static RE_GIT_DIFF: Lazy<Regex> = Lazy::new(|| Regex::new(r"diff --git").unwrap());
static RE_GIT_DIFF_HUNK: Lazy<Regex> = Lazy::new(|| Regex::new(r"@@ ").unwrap());
static RE_GIT_STATUS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^On branch |^nothing to commit|^Changes (not |to be )|^Untracked files:")
        .unwrap()
});
static RE_PORCELAIN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^[ MADRCU?!][ MADRCU?!] \S").unwrap());
static RE_TREE_GLYPH: Lazy<Regex> = Lazy::new(|| Regex::new(r"[├└]──|│  ").unwrap());
static RE_LS_ROW: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^[-dlbcps][rwx-]{9}").unwrap());
static RE_LS_TOTAL: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^total \d+$").unwrap());
/// Build-tool output detector. Catches npm/yarn/cargo/pip-style logs so
/// they can be compressed before being treated as git-status porcelain.
/// Priority: after explicit RE_GIT_STATUS (to match 9router), but before
/// the heuristic is_mostly_porcelain check, preventing cargo "Compiling"
/// lines from being misclassified as porcelain status.
static RE_BUILD_OUTPUT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?im)^(npm (warn|error|ERR!)|yarn (warn|error)|\s*Compiling\s+\S+|\s*Downloading\s+\S+|added \d+ package|\[ERROR\]|BUILD (SUCCESS|FAILED)|\s*Finished\s+|Successfully (installed|built)|ERROR:)")
        .unwrap()
});

/// Test-runner output detector. Catches cargo test, pytest, jest, go test output.
static RE_TEST_RUNNER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?im)^(running \d+ test(s)?|ok \d+|not ok \d+|test result:|test \S+ \.\.\.\s+(ok|FAILED)|PASS|FAIL(ED)?|testsuite:\s+)" )
        .unwrap()
});

/// JSON/NDJSON bulk detector: checks if text starts with `[`, `{`, or has
/// many newline-separated `{` lines.
static RE_NDJSON_LINE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\{.*\}\s*$").unwrap()
});

pub type FilterFn = fn(&str) -> String;

pub struct DetectedFilter {
    pub filter_fn: FilterFn,
    pub filter_name: &'static str,
}

pub fn auto_detect_filter(text: &str) -> Option<DetectedFilter> {
    let head = if text.len() > DETECT_WINDOW {
        &text[..DETECT_WINDOW]
    } else {
        text
    };

    if RE_GIT_DIFF.is_match(head) || RE_GIT_DIFF_HUNK.is_match(head) {
        return Some(DetectedFilter {
            filter_fn: git_diff_impl,
            filter_name: FILTER_GIT_DIFF,
        });
    }

    // Explicit git-status match BEFORE build-output check: "On branch ..." etc.
    // should be treated as git status, not parsed as build tool output.
    if RE_GIT_STATUS.is_match(head) {
        return Some(DetectedFilter {
            filter_fn: git_status_impl,
            filter_name: FILTER_GIT_STATUS,
        });
    }

    // Build-output BEFORE porcelain check: prevents cargo "Compiling" lines
    // from being misclassified as git-status porcelain.
    if RE_BUILD_OUTPUT.is_match(head) {
        return Some(DetectedFilter {
            filter_fn: build_output_impl,
            filter_name: FILTER_BUILD_OUTPUT,
        });
    }

    // Test-runner detection: check for test output patterns BEFORE generic
    // text heuristics so cargo test/pytest/jest output gets proper compression.
    if RE_TEST_RUNNER.is_match(head) {
        let test_lines: Vec<&str> = head.lines().collect();
        if test_lines.len() >= TEST_RUNNER_MIN_LINES {
            return Some(DetectedFilter {
                filter_fn: test_runner_impl,
                filter_name: FILTER_TEST_RUNNER,
            });
        }
    }

    if is_mostly_porcelain(head) {
        return Some(DetectedFilter {
            filter_fn: git_status_impl,
            filter_name: FILTER_GIT_STATUS,
        });
    }

    let lines: Vec<&str> = head.lines().collect();
    let non_empty: Vec<&str> = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .copied()
        .collect();

    let first5: Vec<&&str> = non_empty.iter().take(5).collect();
    if first5.iter().any(|l| is_grep_line(l)) {
        return Some(DetectedFilter {
            filter_fn: grep_impl,
            filter_name: FILTER_GREP,
        });
    }

    if non_empty.len() >= 3 && non_empty.iter().all(is_path_like) {
        return Some(DetectedFilter {
            filter_fn: find_impl,
            filter_name: FILTER_FIND,
        });
    }

    if RE_TREE_GLYPH.is_match(head) {
        return Some(DetectedFilter {
            filter_fn: tree_impl,
            filter_name: FILTER_TREE,
        });
    }

    if RE_LS_TOTAL.is_match(head) || count_matches(head, &RE_LS_ROW) >= 3 {
        return Some(DetectedFilter {
            filter_fn: ls_impl,
            filter_name: FILTER_LS,
        });
    }

    if SEARCH_LIST_HEADER_RE.is_match(head) {
        return Some(DetectedFilter {
            filter_fn: search_list_impl,
            filter_name: FILTER_SEARCH_LIST,
        });
    }

    let text_lines: Vec<&str> = text.lines().collect();
    if text_lines.len() >= SMART_TRUNCATE_MIN_LINES && is_line_numbered(&text_lines) {
        return Some(DetectedFilter {
            filter_fn: read_numbered_impl,
            filter_name: FILTER_READ_NUMBERED,
        });
    }

    // JSON/NDJSON bulk detector: catches large JSON blobs (API response dumps,
    // config dumps) before they hit dedup-log. Check is cheap — just peek at
    // first non-whitespace char and count NDJSON lines.
    // Use text.len() (not head.len()) because JSON_SUMMARY_MIN_BYTES (2000)
    // exceeds DETECT_WINDOW (1024), so the head window would never trigger.
    if text.len() >= JSON_SUMMARY_MIN_BYTES {
        let peek_start = text[..DETECT_WINDOW.min(text.len())].trim_start();
        let is_json_like = peek_start.starts_with('{') || peek_start.starts_with('[');
        if is_json_like || is_mostly_ndjson(&text[..DETECT_WINDOW.min(text.len())]) {
            return Some(DetectedFilter {
                filter_fn: json_summary_impl,
                filter_name: FILTER_JSON_SUMMARY,
            });
        }
    }

    if non_empty.len() >= 5 {
        return Some(DetectedFilter {
            filter_fn: dedup_log_impl,
            filter_name: FILTER_DEDUP_LOG,
        });
    }

    if text.lines().count() >= SMART_TRUNCATE_MIN_LINES {
        return Some(DetectedFilter {
            filter_fn: smart_truncate_impl,
            filter_name: FILTER_SMART_TRUNCATE,
        });
    }

    None
}

fn is_grep_line(line: &&str) -> bool {
    let first = match line.find(':') {
        Some(i) => i,
        None => return false,
    };
    let second = match line[first + 1..].find(':') {
        Some(i) => first + 1 + i,
        None => return false,
    };
    let lineno = &line[first + 1..second];
    lineno.chars().all(|c| c.is_ascii_digit())
}

fn is_path_like(line: &&str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return false;
    }
    if t.contains(':') {
        return false;
    }
    t.starts_with('.') || t.starts_with('/') || t.contains('/')
}

fn is_mostly_porcelain(head: &str) -> bool {
    let lines: Vec<&str> = head.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 3 {
        return false;
    }
    let hits = lines.iter().filter(|l| RE_PORCELAIN.is_match(l)).count();
    hits * 10 >= lines.len() * 6
}

fn is_line_numbered(lines: &[&str]) -> bool {
    let mut hits = 0usize;
    let mut non_empty = 0usize;
    let sample = lines.iter().take(100).filter(|l| !l.is_empty());
    for l in sample {
        non_empty += 1;
        if READ_NUMBERED_LINE_RE.is_match(l) {
            hits += 1;
        }
    }
    if non_empty < 5 {
        return false;
    }
    hits as f64 / non_empty as f64 >= READ_NUMBERED_MIN_HIT_RATIO
}

fn count_matches(text: &str, re: &Regex) -> usize {
    re.find_iter(text).count()
}

/// Check if text is mostly NDJSON: at least 3 non-empty lines and >=80% of them
/// look like JSON objects (start with `{`).
fn is_mostly_ndjson(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 3 {
        return false;
    }
    let sample: Vec<&&str> = lines.iter().take(50).collect();
    let sampled = sample.len();
    if sampled < 3 {
        return false;
    }
    let hits = sample.iter().filter(|l| RE_NDJSON_LINE.is_match(l)).count();
    hits * 10 >= sampled * 8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detects_git_diff() {
        let input = "diff --git a/file.rs b/file.rs\n@@ -1 +1 @@\n-old\n+new";
        let result = auto_detect_filter(input);
        assert!(result.is_some());
        assert_eq!(result.unwrap().filter_name, FILTER_GIT_DIFF);
    }

    #[test]
    fn test_detects_git_status() {
        let input = "On branch main\nChanges not staged:\n  modified: src/lib.rs";
        let result = auto_detect_filter(input);
        assert!(result.is_some());
        assert_eq!(result.unwrap().filter_name, FILTER_GIT_STATUS);
    }

    #[test]
    fn test_detects_grep() {
        let input = "src/main.rs:10:fn main() {\nsrc/lib.rs:5:use std::";
        let result = auto_detect_filter(input);
        assert!(result.is_some());
        assert_eq!(result.unwrap().filter_name, FILTER_GREP);
    }

    #[test]
    fn test_detects_find() {
        let input = "/home/user/project/src/main.rs\n/home/user/project/src/lib.rs\n/home/user/project/src/other.rs";
        let result = auto_detect_filter(input);
        assert!(result.is_some());
        assert_eq!(result.unwrap().filter_name, FILTER_FIND);
    }

    #[test]
    fn test_detects_tree() {
        let input = "src\n├── main.rs\n└── lib.rs";
        let result = auto_detect_filter(input);
        assert!(result.is_some());
        assert_eq!(result.unwrap().filter_name, FILTER_TREE);
    }

    #[test]
    fn test_detects_ls() {
        let input = "total 48\n-rw-r--r--  1 user staff  1234 Jan 15 10:30 file.txt\ndrwxr-xr-x   2 user staff   512 Jan 15 10:30 dir/";
        let result = auto_detect_filter(input);
        assert!(result.is_some());
        assert_eq!(result.unwrap().filter_name, FILTER_LS);
    }

    #[test]
    fn test_detects_search_list() {
        let input = "Result of search in '/src' (total 3 files):\n- src/a.rs\n- src/b.rs";
        let result = auto_detect_filter(input);
        assert!(result.is_some());
        assert_eq!(result.unwrap().filter_name, FILTER_SEARCH_LIST);
    }

    #[test]
    fn test_returns_none_for_short_text() {
        let input = "hello world";
        let result = auto_detect_filter(input);
        assert!(result.is_none());
    }

    #[test]
    fn test_detects_test_runner_cargo_output() {
        let mut lines: Vec<String> = Vec::new();
        lines.push("running 12 tests".to_string());
        for i in 0..12 {
            lines.push(format!("test case_{} ... ok", i));
        }
        lines.push("".to_string());
        lines.push("test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out".to_string());
        let input = lines.join("\n");
        let result = auto_detect_filter(&input);
        assert!(result.is_some(), "test-runner should be detected");
        assert_eq!(result.unwrap().filter_name, FILTER_TEST_RUNNER);
    }

    #[test]
    fn test_detects_json_summary_bracket_start() {
        // Pad the JSON payload to exceed JSON_SUMMARY_MIN_BYTES (2000)
        let large_json = format!("{{{}}}", "\"key\": ".repeat(2000));
        let result = auto_detect_filter(&large_json);
        assert!(result.is_some(), "json-summary should be detected: len={}", large_json.len());
        assert_eq!(result.unwrap().filter_name, FILTER_JSON_SUMMARY);
    }

    #[test]
    fn test_detects_ndjson_blob() {
        // Build enough lines to exceed JSON_SUMMARY_MIN_BYTES (2000)
        let lines: Vec<String> = (0..100)
            .map(|i| format!("{{\"id\": {:>4}, \"name\": \"test_value_{}\"}}", i, i))
            .collect();
        let input = lines.join("\n");
        assert!(input.len() > JSON_SUMMARY_MIN_BYTES, "fixture too small: {} < {}", input.len(), JSON_SUMMARY_MIN_BYTES);
        let result = auto_detect_filter(&input);
        assert!(result.is_some(), "NDJSON should be detected for large blobs: len={}", input.len());
        assert_eq!(result.unwrap().filter_name, FILTER_JSON_SUMMARY);
    }
}
