use std::panic::{catch_unwind, AssertUnwindSafe};

#[derive(Debug, Clone)]
pub struct RtkStats {
    pub bytes_before: usize,
    pub bytes_after: usize,
    pub hits: Vec<RtkHit>,
}

#[derive(Debug, Clone)]
pub struct RtkHit {
    pub shape: String,
    pub filter: String,
    pub saved: usize,
}

pub fn safe_apply(filter_fn: fn(&str) -> String, text: &str, filter_name: &str) -> String {
    let result = catch_unwind(AssertUnwindSafe(|| filter_fn(text)));

    match result {
        Ok(out) => {
            if out.is_empty() || out.len() >= text.len() {
                return text.to_string();
            }
            out
        }
        Err(_) => {
            eprintln!(
                "[rtk] warning: filter '{}' panicked — passing through raw output",
                filter_name
            );
            text.to_string()
        }
    }
}

pub fn safe_apply_from_option(filter_fn: Option<fn(&str) -> String>, text: &str, filter_name: &str) -> String {
    match filter_fn {
        Some(f) => safe_apply(f, text, filter_name),
        None => text.to_string(),
    }
}

pub fn git_diff(text: &str) -> String {
    use crate::core::rtk::filters::GitDiffFilter;
    GitDiffFilter.apply(text)
}

pub fn git_status(text: &str) -> String {
    use crate::core::rtk::filters::GitStatusFilter;
    GitStatusFilter.apply(text)
}

pub fn grep(text: &str) -> String {
    use crate::core::rtk::filters::GrepFilter;
    GrepFilter.apply(text)
}

pub fn find(text: &str) -> String {
    use crate::core::rtk::filters::FindFilter;
    FindFilter.apply(text)
}

pub fn tree(text: &str) -> String {
    use crate::core::rtk::filters::TreeFilter;
    TreeFilter.apply(text)
}

pub fn ls(text: &str) -> String {
    use crate::core::rtk::filters::LsFilter;
    LsFilter.apply(text)
}

pub fn search_list(text: &str) -> String {
    use crate::core::rtk::filters::SearchListFilter;
    SearchListFilter.apply(text)
}

pub fn read_numbered(text: &str) -> String {
    use crate::core::rtk::filters::ReadNumberedFilter;
    ReadNumberedFilter.apply(text)
}

pub fn dedup_log(text: &str) -> String {
    use crate::core::rtk::filters::DedupLogFilter;
    DedupLogFilter.apply(text)
}

pub fn smart_truncate(text: &str) -> String {
    use crate::core::rtk::filters::SmartTruncateFilter;
    SmartTruncateFilter.apply(text)
}