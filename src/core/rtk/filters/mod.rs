use once_cell::sync::Lazy;
use regex::Regex;

use crate::core::rtk::constants::*;
use crate::core::rtk::apply_filter::safe_apply;

pub struct GitDiffFilter;
impl GitDiffFilter {
    pub fn apply(&self, text: &str) -> String {
        safe_apply(git_diff_impl, text, FILTER_GIT_DIFF)
    }
}

pub fn git_diff_impl(diff: &str) -> String {
    let mut result = Vec::new();
    let mut current_file = String::new();
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut in_hunk = false;
    let mut hunk_shown = 0usize;
    let mut hunk_skipped = 0usize;
    let mut was_truncated = false;
    let max_hunk_lines = GIT_DIFF_HUNK_MAX_LINES;
    let max_lines = 500;

    for line in diff.lines() {
        if line.starts_with("diff --git") {
            if hunk_skipped > 0 {
                result.push(format!("  ... ({} lines truncated)", hunk_skipped));
                was_truncated = true;
                hunk_skipped = 0;
            }
            if !current_file.is_empty() && (added > 0 || removed > 0) {
                result.push(format!("  +{} -{}", added, removed));
            }
            let parts: Vec<&str> = line.split(" b/").collect();
            current_file = if parts.len() > 1 {
                parts[1..].join(" b/")
            } else {
                "unknown".to_string()
            };
            result.push(format!("\n{}", current_file));
            added = 0;
            removed = 0;
            in_hunk = false;
            hunk_shown = 0;
        } else if line.starts_with("@@") {
            if hunk_skipped > 0 {
                result.push(format!("  ... ({} lines truncated)", hunk_skipped));
                was_truncated = true;
                hunk_skipped = 0;
            }
            in_hunk = true;
            hunk_shown = 0;
            result.push(format!("  {}", line));
        } else if in_hunk {
            if line.starts_with('+') && !line.starts_with("+++") {
                added += 1;
                if hunk_shown < max_hunk_lines {
                    result.push(format!("  {}", line));
                    hunk_shown += 1;
                } else {
                    hunk_skipped += 1;
                }
            } else if line.starts_with('-') && !line.starts_with("---") {
                removed += 1;
                if hunk_shown < max_hunk_lines {
                    result.push(format!("  {}", line));
                    hunk_shown += 1;
                } else {
                    hunk_skipped += 1;
                }
            } else if hunk_shown < max_hunk_lines && !line.starts_with('\\') {
                if hunk_shown > 0 {
                    result.push(format!("  {}", line));
                    hunk_shown += 1;
                }
            }
        }

        if result.len() >= max_lines {
            result.push(String::from("\n... (more changes truncated)"));
            was_truncated = true;
            break;
        }
    }

    if hunk_skipped > 0 {
        result.push(format!("  ... ({} lines truncated)", hunk_skipped));
        was_truncated = true;
    }

    if !current_file.is_empty() && (added > 0 || removed > 0) {
        result.push(format!("  +{} -{}", added, removed));
    }

    if was_truncated {
        result.push(String::from("[full diff: rtk git diff --no-compact]"));
    }

    result.join("\n")
}

pub struct GitStatusFilter;
impl GitStatusFilter {
    pub fn apply(&self, text: &str) -> String {
        safe_apply(git_status_impl, text, FILTER_GIT_STATUS)
    }
}

pub fn git_status_impl(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    if lines.is_empty() || (lines.len() == 1 && lines[0].trim().is_empty()) {
        return String::from("Clean working tree");
    }

    let mut branch = String::new();
    let mut staged_files = Vec::new();
    let mut modified_files = Vec::new();
    let mut untracked_files = Vec::new();
    let mut staged_count = 0usize;
    let mut modified_count = 0usize;
    let mut untracked_count = 0usize;
    let mut conflicts_count = 0usize;

    static RE_PORCELAIN: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"^[ MADRCU?!][ MADRCU?!] ").unwrap());

    for raw in lines {
        if raw.trim().is_empty() {
            continue;
        }

        if let Some(caps) = raw.strip_prefix("On branch ") {
            branch = caps.trim().to_string();
            continue;
        }

        if raw.starts_with("##") {
            branch = raw.replace("##", "").trim().to_string();
            continue;
        }

        if RE_PORCELAIN.is_match(raw) {
            let x = raw.chars().next().unwrap();
            let y = raw.chars().nth(1).unwrap_or(' ');

            if raw.starts_with("??") {
                untracked_count += 1;
                if raw.len() > 3 {
                    untracked_files.push(raw[3..].trim().to_string());
                }
                continue;
            }

            if "MADRC".contains(x) {
                if raw.len() > 3 {
                    staged_files.push(raw[3..].trim().to_string());
                }
            } else if x == 'U' {
                conflicts_count += 1;
            }

            if y == 'M' || y == 'D' {
                if raw.len() > 3 {
                    modified_files.push(raw[3..].trim().to_string());
                }
            }
            continue;
        }

        if let Some(caps) = raw.trim().strip_prefix("modified:") {
            let path = caps.trim().to_string();
            if !path.is_empty() {
                modified_count += 1;
                modified_files.push(path);
            }
            continue;
        }
        if let Some(caps) = raw.trim().strip_prefix("new file:") {
            let path = caps.trim().to_string();
            if !path.is_empty() {
                staged_count += 1;
                staged_files.push(path);
            }
            continue;
        }
        if let Some(caps) = raw.trim().strip_prefix("deleted:") {
            let path = caps.trim().to_string();
            if !path.is_empty() {
                modified_count += 1;
                modified_files.push(path);
            }
            continue;
        }
        if let Some(caps) = raw.trim().strip_prefix("renamed:") {
            let path = caps.trim().to_string();
            if !path.is_empty() {
                staged_count += 1;
                staged_files.push(path);
            }
            continue;
        }
        if let Some(caps) = raw.trim().strip_prefix("both modified:") {
            let path = caps.trim().to_string();
            if !path.is_empty() {
                conflicts_count += 1;
            }
            continue;
        }
    }

    let mut out = String::new();
    if !branch.is_empty() {
        out.push_str(&format!("* {}\n", branch));
    }

    if staged_count > 0 {
        out.push_str(&format!("+ Staged: {} files\n", staged_count));
        for f in staged_files.iter().take(STATUS_MAX_FILES) {
            out.push_str(&format!("   {}\n", f));
        }
        if staged_files.len() > STATUS_MAX_FILES {
            out.push_str(&format!(
                "   ... +{} more\n",
                staged_files.len() - STATUS_MAX_FILES
            ));
        }
    }

    if modified_count > 0 {
        out.push_str(&format!("~ Modified: {} files\n", modified_count));
        for f in modified_files.iter().take(STATUS_MAX_FILES) {
            out.push_str(&format!("   {}\n", f));
        }
        if modified_files.len() > STATUS_MAX_FILES {
            out.push_str(&format!(
                "   ... +{} more\n",
                modified_files.len() - STATUS_MAX_FILES
            ));
        }
    }

    if untracked_count > 0 {
        out.push_str(&format!("? Untracked: {} files\n", untracked_count));
        for f in untracked_files.iter().take(STATUS_MAX_UNTRACKED) {
            out.push_str(&format!("   {}\n", f));
        }
        if untracked_files.len() > STATUS_MAX_UNTRACKED {
            out.push_str(&format!(
                "   ... +{} more\n",
                untracked_files.len() - STATUS_MAX_UNTRACKED
            ));
        }
    }

    if conflicts_count > 0 {
        out.push_str(&format!("conflicts: {} files\n", conflicts_count));
    }

    if staged_count == 0 && modified_count == 0 && untracked_count == 0 && conflicts_count == 0 {
        out.push_str("clean — nothing to commit\n");
    }

    out.trim_end().to_string()
}

pub struct GrepFilter;
impl GrepFilter {
    pub fn apply(&self, text: &str) -> String {
        safe_apply(grep_impl, text, FILTER_GREP)
    }
}

pub fn grep_impl(input: &str) -> String {
    let mut by_file: std::collections::BTreeMap<String, Vec<(String, String)>> =
        std::collections::BTreeMap::new();
    let mut total = 0usize;

    for line in input.lines() {
        let first = match line.find(':') {
            Some(i) => i,
            None => continue,
        };
        let second = match line.find(':') {
            Some(i) if i > first => i,
            _ => continue,
        };

        let file = &line[..first];
        let line_num_str = &line[first + 1..second];
        let content = &line[second + 1..];

        if line_num_str.chars().all(|c| c.is_ascii_digit()) {
            total += 1;
            by_file.entry(file.to_string())
                .or_default()
                .push((line_num_str.to_string(), content.trim().to_string()));
        }
    }

    if total == 0 {
        return input.to_string();
    }

    let files: Vec<&String> = by_file.keys().collect();
    let mut out = format!("{} matches in {}F:\n\n", total, files.len());

    for file in files {
        let matches = &by_file[file];
        out.push_str(&format!("[file] {} ({}):\n", file, matches.len()));
        let show = matches.iter().take(GREP_PER_FILE_MAX);
        for (line_num, content) in show {
            out.push_str(&format!("  {:>4}: {}\n", line_num, content));
        }
        if matches.len() > GREP_PER_FILE_MAX {
            out.push_str(&format!("  +{}\n", matches.len() - GREP_PER_FILE_MAX));
        }
        out.push('\n');
    }

    out
}

pub struct FindFilter;
impl FindFilter {
    pub fn apply(&self, text: &str) -> String {
        safe_apply(find_impl, text, FILTER_FIND)
    }
}

pub fn find_impl(input: &str) -> String {
    let lines: Vec<&str> = input.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return input.to_string();
    }
    let line_count = lines.len();

    let mut by_dir: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();

    for path in &lines {
        let last_slash = path.rfind('/');
        let (dir, basename) = match last_slash {
            Some(idx) => (&path[..idx], &path[idx + 1..]),
            None => (".", *path),
        };
        let dir_s = if dir.is_empty() { "/" } else { dir };
        by_dir.entry(dir_s.to_string())
            .or_default()
            .push(basename.to_string());
    }

    let dirs: Vec<&String> = by_dir.keys().collect();
    let mut out = format!("{} files in {} dirs:\n\n", line_count, dirs.len());

    for dir in dirs.iter().take(FIND_TOTAL_DIR_MAX) {
        let files = &by_dir[*dir];
        out.push_str(&format!("{}/ ({}):\n", dir, files.len()));
        for f in files.iter().take(FIND_PER_DIR_MAX) {
            out.push_str(&format!("  {}\n", f));
        }
        if files.len() > FIND_PER_DIR_MAX {
            out.push_str(&format!("  +{}\n", files.len() - FIND_PER_DIR_MAX));
        }
        out.push('\n');
    }
    if dirs.len() > FIND_TOTAL_DIR_MAX {
        out.push_str(&format!(
            "+{} more dirs\n",
            dirs.len() - FIND_TOTAL_DIR_MAX
        ));
    }

    out
}

pub struct TreeFilter;
impl TreeFilter {
    pub fn apply(&self, text: &str) -> String {
        safe_apply(tree_impl, text, FILTER_TREE)
    }
}

pub fn tree_impl(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    if lines.is_empty() {
        return input.to_string();
    }

    let mut filtered: Vec<&str> = Vec::with_capacity(lines.len());
    for line in lines {
        if line.contains("director") && line.contains("file") {
            continue;
        }
        if line.trim().is_empty() && filtered.is_empty() {
            continue;
        }
        filtered.push(line);
    }

    while let Some(last) = filtered.last() {
        if last.trim().is_empty() {
            filtered.pop();
        } else {
            break;
        }
    }

    if filtered.len() > TREE_MAX_LINES {
        let cut = filtered.len() - TREE_MAX_LINES;
        let head = &filtered[..TREE_MAX_LINES];
        let mut result = head.join("\n");
        result.push('\n');
        result.push_str(&format!("... +{} more lines", cut));
        return result;
    }

    filtered.join("\n")
}

pub struct LsFilter;
impl LsFilter {
    pub fn apply(&self, text: &str) -> String {
        safe_apply(ls_impl, text, FILTER_LS)
    }
}

static LS_DATE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\s+(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\s+\d{1,2}\s+(\d{4}|\d{2}:\d{2})\s+").unwrap()
});

fn human_size(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1}M", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1}K", bytes as f64 / 1024.0)
    } else {
        format!("{}B", bytes)
    }
}

fn parse_ls_line(line: &str) -> Option<(char, u64, &str)> {
    let m = LS_DATE_RE.find(line)?;
    let name = &line[m.end()..];
    let before_date = &line[..m.start()];
    let before_parts: Vec<&str> = before_date.split_whitespace().collect();
    if before_parts.len() < 4 {
        return None;
    }

    let perms = before_parts[0];
    let file_type = perms.chars().next()?;

    let mut size: u64 = 0;
    for part in before_parts.iter().rev() {
        if let Ok(n) = part.parse::<u64>() {
            size = n;
            break;
        }
    }

    Some((file_type, size, name))
}

pub fn ls_impl(input: &str) -> String {
    let mut dirs = Vec::new();
    let mut files: Vec<(String, String)> = Vec::new();
    let mut by_ext: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for line in input.lines() {
        if line.starts_with("total ") || line.is_empty() {
            continue;
        }
        let parsed = match parse_ls_line(line) {
            Some(p) => p,
            None => continue,
        };
        if parsed.2 == "." || parsed.2 == ".." {
            continue;
        }
        if LS_NOISE_DIRS.contains(&parsed.2) {
            continue;
        }

        if parsed.0 == 'd' {
            dirs.push(parsed.2.to_string());
        } else if parsed.0 == '-' || parsed.0 == 'l' {
            let dot = parsed.2.rfind('.');
            let ext = match dot {
                Some(idx) if idx > 0 => &parsed.2[idx..],
                _ => "no ext",
            };
            *by_ext.entry(ext.to_string()).or_default() += 1;
            files.push((parsed.2.to_string(), human_size(parsed.1)));
        }
    }

    if dirs.is_empty() && files.is_empty() {
        return input.to_string();
    }

    let mut out = String::new();
    for d in &dirs {
        out.push_str(&format!("{}/\n", d));
    }
    for (name, size) in &files {
        out.push_str(&format!("{}  {}\n", name, size));
    }

    let mut summary = format!("\nSummary: {} files, {} dirs", files.len(), dirs.len());
    if !by_ext.is_empty() {
        let mut ext: Vec<(String, usize)> = by_ext.iter().map(|(k, v)| (k.clone(), *v)).collect();
        ext.sort_by(|a, b| b.1.cmp(&a.1));
        let parts: Vec<String> = ext.iter()
            .take(LS_EXT_SUMMARY_TOP)
            .map(|(e, c)| format!("{} {}", c, e))
            .collect();
        summary.push_str(&format!(" ({}", parts.join(", ")));
        if ext.len() > LS_EXT_SUMMARY_TOP {
            summary.push_str(&format!(", +{} more", ext.len() - LS_EXT_SUMMARY_TOP));
        }
        summary.push(')');
    }

    out.push_str(&summary);
    out
}

pub struct SearchListFilter;
impl SearchListFilter {
    pub fn apply(&self, text: &str) -> String {
        safe_apply(search_list_impl, text, FILTER_SEARCH_LIST)
    }
}

pub static SEARCH_LIST_HEADER_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^Result of search in '[^']*' \(total \d+ files?\):").unwrap());

pub fn search_list_impl(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    if lines.is_empty() {
        return input.to_string();
    }

    let header = lines.first().unwrap_or(&"");
    let rest = &lines[1..];

    let mut paths: Vec<String> = Vec::new();
    for raw in rest.iter() {
        let t = raw.trim();
        if let Some(stripped) = t.strip_prefix("- ") {
            paths.push(stripped.to_string());
        }
    }
    if paths.is_empty() {
        return input.to_string();
    }

    let mut by_dir: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for p in &paths {
        let slash = p.rfind('/');
        let (dir, name) = match slash {
            Some(idx) => (&p[..idx], &p[idx + 1..]),
            None => (".", p.as_str()),
        };
        let dir_s = if dir.is_empty() { "/" } else { dir };
        by_dir.entry(dir_s.to_string())
            .or_default()
            .push(name.to_string());
    }

    let dirs: Vec<&String> = by_dir.keys().collect();
    let mut out = format!(
        "{}\n{} files in {} dirs:\n\n",
        header,
        paths.len(),
        dirs.len()
    );

    for dir in dirs.iter().take(SEARCH_LIST_TOTAL_DIR_MAX) {
        let names = &by_dir[*dir];
        out.push_str(&format!("{}/ ({}):\n", dir, names.len()));
        for n in names.iter().take(SEARCH_LIST_PER_DIR_MAX) {
            out.push_str(&format!("  {}\n", n));
        }
        if names.len() > SEARCH_LIST_PER_DIR_MAX {
            out.push_str(&format!("  +{}\n", names.len() - SEARCH_LIST_PER_DIR_MAX));
        }
        out.push('\n');
    }
    if dirs.len() > SEARCH_LIST_TOTAL_DIR_MAX {
        out.push_str(&format!(
            "+{} more dirs\n",
            dirs.len() - SEARCH_LIST_TOTAL_DIR_MAX
        ));
    }

    out.trim_end().to_string()
}

pub struct ReadNumberedFilter;
impl ReadNumberedFilter {
    pub fn apply(&self, text: &str) -> String {
        safe_apply(read_numbered_impl, text, FILTER_READ_NUMBERED)
    }
}

pub static READ_NUMBERED_LINE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\s*\d+\|").unwrap());

pub fn read_numbered_impl(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    if lines.len() < SMART_TRUNCATE_MIN_LINES {
        return input.to_string();
    }

    let head = &lines[..SMART_TRUNCATE_HEAD.min(lines.len())];
    let tail_start = lines.len().saturating_sub(SMART_TRUNCATE_TAIL);
    let tail = &lines[tail_start..];
    let cut = lines.len() - head.len() - tail.len();

    let mut result = head.join("\n");
    result.push('\n');
    result.push_str(&format!("... +{} lines truncated (file continues)", cut));
    result.push('\n');
    result.push_str(&tail.join("\n"));

    result
}

pub struct DedupLogFilter;
impl DedupLogFilter {
    pub fn apply(&self, text: &str) -> String {
        safe_apply(dedup_log_impl, text, FILTER_DEDUP_LOG)
    }
}

pub fn dedup_log_impl(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut prev: Option<&str> = None;
    let mut run_count = 0usize;
    let mut blank_streak = 0usize;

    let flush_run = |out: &mut Vec<String>, prev: Option<&str>, run_count: usize| {
        if let Some(p) = prev {
            if run_count > 1 {
                out.push(format!("  ... ({} duplicate lines)", run_count - 1));
            }
        }
    };

    for line in lines {
        if line.trim().is_empty() {
            if blank_streak < 1 {
                out.push(line.to_string());
            }
            blank_streak += 1;
            flush_run(&mut out, prev, run_count);
            prev = None;
            run_count = 0;
            continue;
        }
        blank_streak = 0;
        if let Some(p) = prev {
            if line == p {
                run_count += 1;
                continue;
            }
        }
        flush_run(&mut out, prev, run_count);
        out.push(line.to_string());
        prev = Some(line);
        run_count = 1;
        if out.len() >= DEDUP_LINE_MAX {
            out.push(format!("... (truncated at {} lines)", DEDUP_LINE_MAX));
            return out.join("\n");
        }
    }
    flush_run(&mut out, prev, run_count);
    out.join("\n")
}

pub struct SmartTruncateFilter;
impl SmartTruncateFilter {
    pub fn apply(&self, text: &str) -> String {
        safe_apply(smart_truncate_impl, text, FILTER_SMART_TRUNCATE)
    }
}

pub fn smart_truncate_impl(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    if lines.len() < SMART_TRUNCATE_MIN_LINES {
        return input.to_string();
    }

    let head = &lines[..SMART_TRUNCATE_HEAD.min(lines.len())];
    let tail_start = lines.len().saturating_sub(SMART_TRUNCATE_TAIL);
    let tail = &lines[tail_start..];
    let cut = lines.len() - head.len() - tail.len();

    let mut result = head.join("\n");
    result.push('\n');
    result.push_str(&format!("... +{} lines truncated", cut));
    result.push('\n');
    result.push_str(&tail.join("\n"));

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_diff_basic() {
        let input = "diff --git a/src/main.rs b/src/main.rs\nindex 1234567..abcdefg 100644\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,4 @@\n fn main() {\n+    println!(\"hello\");\n }";
        let result = git_diff_impl(input);
        assert!(result.contains("src/main.rs"));
        assert!(result.contains("+1 -0"));
    }

    #[test]
    fn test_git_status_clean() {
        let result = git_status_impl("");
        assert_eq!(result, "Clean working tree");
    }

    #[test]
    fn test_grep_no_matches() {
        let input = "this is not grep output";
        let result = grep_impl(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_find_empty() {
        let result = find_impl("");
        assert_eq!(result, "");
    }

    #[test]
    fn test_tree_summary_stripped() {
        let input = ".\n├── src\n5 directories, 23 files";
        let result = tree_impl(input);
        assert!(!result.contains("5 directories"));
    }

    #[test]
    fn test_ls_basic() {
        let input = "total 48\n-rw-r--r--  1 user staff  1234 Jan 15 10:30 file.txt";
        let result = ls_impl(input);
        assert!(result.contains("file.txt"));
    }

    #[test]
    fn test_search_list_header() {
        let input = "Result of search in '/src' (total 3 files):\n- src/a.rs\n- src/b.rs\n- src/c.rs";
        let result = search_list_impl(input);
        assert!(result.contains("3 files"));
        assert!(result.contains("src/"));
    }

    #[test]
    fn test_read_numbered_truncates() {
        let lines: Vec<String> = (1..=300).map(|i| format!("  {}|content{}", i, i)).collect();
        let input = lines.join("\n");
        let result = read_numbered_impl(&input);
        assert!(result.contains("truncated"));
    }

    #[test]
    fn test_dedup_log_deduplicates() {
        let input = "line1\nline1\nline1\nline2";
        let result = dedup_log_impl(input);
        assert!(result.contains("duplicate lines"));
    }

    #[test]
    fn test_smart_truncate_short() {
        let input = "line1\nline2\nline3";
        let result = smart_truncate_impl(input);
        assert_eq!(result, input);
    }
}