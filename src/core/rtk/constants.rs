//! RTK compression constants mirroring the JS implementation.
//!
//! These constants control compression behavior, size thresholds,
//! and per-filter limits matching the Rust RTK implementation.

/// Maximum raw byte size before skipping compression (10 MiB)
pub const RAW_CAP: usize = 10 * 1024 * 1024;

/// Minimum text byte size before compression kicks in (500 bytes)
pub const MIN_COMPRESS_SIZE: usize = 500;

/// Autodetect peek window size — first N chars examined for shape detection
pub const DETECT_WINDOW: usize = 1024;

/// Per-hunk line cap for git diff compaction
pub const GIT_DIFF_HUNK_MAX_LINES: usize = 100;

/// Context lines kept around changed regions in git diff
pub const GIT_DIFF_CONTEXT_KEEP: usize = 3;

/// dedupLog truncation cap — max output lines before truncation message
pub const DEDUP_LINE_MAX: usize = 2000;

/// grep filter: max matches shown per file
pub const GREP_PER_FILE_MAX: usize = 10;

/// find filter: max files shown per directory
pub const FIND_PER_DIR_MAX: usize = 10;

/// find filter: max directories shown total
pub const FIND_TOTAL_DIR_MAX: usize = 20;

/// git status: max files shown per category
pub const STATUS_MAX_FILES: usize = 10;

/// git status: max untracked files shown
pub const STATUS_MAX_UNTRACKED: usize = 10;

/// ls filter: top-N extensions shown in summary
pub const LS_EXT_SUMMARY_TOP: usize = 5;

/// ls filter: directory names to skip in noise filter
pub const LS_NOISE_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "__pycache__",
    ".next",
    "dist",
    "build",
    ".venv",
    "venv",
    ".cache",
    ".idea",
    ".vscode",
    ".DS_Store",
];

/// tree filter: max output lines before truncation
pub const TREE_MAX_LINES: usize = 200;

/// searchList filter: max files shown per directory (Cursor Glob)
pub const SEARCH_LIST_PER_DIR_MAX: usize = 10;

/// searchList filter: max directories shown total (Cursor Glob)
pub const SEARCH_LIST_TOTAL_DIR_MAX: usize = 20;

/// smartTruncate: lines kept from start
pub const SMART_TRUNCATE_HEAD: usize = 120;

/// smartTruncate: lines kept from end
pub const SMART_TRUNCATE_TAIL: usize = 60;

/// smartTruncate minimum lines before filter activates
pub const SMART_TRUNCATE_MIN_LINES: usize = 250;

/// readNumbered filter: minimum hit ratio (matching lines / non-empty lines)
pub const READ_NUMBERED_MIN_HIT_RATIO: f64 = 0.7;

/// Filter name identifiers (Rust parity + JS extras)
pub const FILTER_GIT_DIFF: &str = "git-diff";
pub const FILTER_GIT_STATUS: &str = "git-status";
pub const FILTER_GIT_LOG: &str = "git-log";
pub const FILTER_GREP: &str = "grep";
pub const FILTER_FIND: &str = "find";
pub const FILTER_LS: &str = "ls";
pub const FILTER_TREE: &str = "tree";
pub const FILTER_DEDUP_LOG: &str = "dedup-log";
pub const FILTER_SMART_TRUNCATE: &str = "smart-truncate";
pub const FILTER_READ_NUMBERED: &str = "read-numbered";
pub const FILTER_SEARCH_LIST: &str = "search-list";
