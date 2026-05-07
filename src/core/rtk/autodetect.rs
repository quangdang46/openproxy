use once_cell::sync::Lazy;
use regex::Regex;

use crate::core::rtk::constants::*;
use crate::core::rtk::filters::{
    dedup_log_impl, find_impl, git_diff_impl, git_status_impl, grep_impl, ls_impl,
    read_numbered_impl, search_list_impl, smart_truncate_impl, tree_impl, READ_NUMBERED_LINE_RE,
    SEARCH_LIST_HEADER_RE,
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

    if RE_GIT_STATUS.is_match(head) || is_mostly_porcelain(head) {
        return Some(DetectedFilter {
            filter_fn: git_status_impl,
            filter_name: FILTER_GIT_STATUS,
        });
    }

    let lines: Vec<&str> = head.lines().collect();
    let non_empty: Vec<&str> = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| *l)
        .collect();

    let first5: Vec<&&str> = non_empty.iter().take(5).collect();
    if first5.iter().any(|l| is_grep_line(l)) {
        return Some(DetectedFilter {
            filter_fn: grep_impl,
            filter_name: FILTER_GREP,
        });
    }

    if non_empty.len() >= 3 && non_empty.iter().all(|l| is_path_like(l)) {
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
}
