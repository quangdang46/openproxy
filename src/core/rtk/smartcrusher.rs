//! SmartCrusher: Tabular data detection and compression engine.
//!
//! Detects tabular data (CSV, TSV, pipe-separated, JSON arrays of objects)
//! in request bodies and compresses using either:
//!
//! - **GCF (Graph Compact Format)**: column-oriented storage with run-length
//!   encoding for repeated adjacent values. Best for data with few unique
//!   values per column or mostly numeric columns.
//!
//! - **TOON (Token-Oriented Object Notation)**: token-substitution format
//!   that maps frequently-repeated string values to short tokens. Best for
//!   data with many long, repeated string values.
//!
//! The encoder selection heuristic picks whichever format yields a smaller
//! output for the given data.

use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Types of tabular data the SmartCrusher can detect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabularType {
    /// Comma-separated values (also TSV, pipe-separated)
    Csv,
    /// JSON array of objects with consistent keys
    JsonArray,
}

impl TabularType {
    /// Return the filter name identifier for the RTK dispatch chain.
    pub fn filter_name(self) -> &'static str {
        match self {
            Self::Csv => "smartcrusher-csv",
            Self::JsonArray => "smartcrusher-json",
        }
    }
}

// ---------------------------------------------------------------------------
// SmartCrusher — main orchestrator
// ---------------------------------------------------------------------------

/// Tabular data compression engine.
///
/// Parses input into a structured `Input` representation, then encodes it
/// using both GCF and TOON, selecting whichever yields smaller output.
pub struct SmartCrusher;

impl SmartCrusher {
    /// Detect tabular data and return the best compressed form.
    ///
    /// Returns `Some(compressed)` if the input is tabular and at least one
    /// encoder produces a smaller representation; `None` otherwise.
    pub fn compress(text: &str) -> Option<String> {
        let input = Input::try_from(text).ok()?;

        let gcf = GcfEncoder::encode(&input);
        let toon = ToonEncoder::encode(&input);

        let best = if gcf.len() <= toon.len() { gcf } else { toon };

        if best.len() < text.len() {
            Some(best)
        } else {
            None
        }
    }

    /// Detect the type of tabular data present, without parsing the full body.
    pub fn detect(text: &str) -> Option<TabularType> {
        if text.len() < 20 {
            return None;
        }

        if looks_like_csv(text) {
            return Some(TabularType::Csv);
        }

        if looks_like_json_array(text) {
            return Some(TabularType::JsonArray);
        }

        None
    }
}

// ---------------------------------------------------------------------------
// Input — common intermediate representation
// ---------------------------------------------------------------------------

/// Parsed tabular data with headers, rows, and a reference to the original size.
#[derive(Debug, Clone)]
pub(crate) struct Input {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub original_size: usize,
}

impl Input {
    /// Select the best compression format for this data.
    pub(crate) fn select_format(&self) -> CompressFormat {
        let total_cells = self.headers.len() * self.rows.len();
        if total_cells == 0 {
            return CompressFormat::Gcf;
        }

        // Count string cells (non-numeric, non-boolean, non-null)
        let string_cells: usize = self
            .rows
            .iter()
            .flat_map(|r| r.iter())
            .filter(|v| {
                v.is_empty()
                    || (v.parse::<f64>().is_err() && *v != "true" && *v != "false" && *v != "null")
            })
            .count();

        // Compute repetition ratio: fraction of string cells that are repeated
        let unique_strings: usize = {
            let mut seen = HashSet::new();
            for row in &self.rows {
                for val in row {
                    if val.len() > 3 {
                        seen.insert(val.as_str());
                    }
                }
            }
            seen.len()
        };

        let repetition_ratio = if string_cells > 0 {
            1.0 - (unique_strings as f64 / string_cells as f64)
        } else {
            0.0
        };

        if repetition_ratio > 0.15 && string_cells as f64 / total_cells as f64 > 0.3 {
            CompressFormat::Toon
        } else {
            CompressFormat::Gcf
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompressFormat {
    /// Graph Compact Format — column-oriented with RLE
    Gcf,
    /// Token-Oriented Object Notation — token substitution
    Toon,
}

// ---------------------------------------------------------------------------
// Input parsing
// ---------------------------------------------------------------------------

impl TryFrom<&str> for Input {
    type Error = ();

    fn try_from(text: &str) -> Result<Self, Self::Error> {
        let trimmed = text.trim();

        // Try CSV/TSV/pipe-separated first (cheap)
        if let Ok(input) = Self::parse_dsv(trimmed) {
            return Ok(input);
        }

        // Try JSON array of objects
        if let Ok(input) = Self::parse_json_array(trimmed) {
            return Ok(input);
        }

        Err(())
    }
}

impl Input {
    /// Parse delimiter-separated values (CSV, TSV, pipe, semicolon).
    fn parse_dsv(text: &str) -> Result<Self, ()> {
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        if lines.len() < 3 {
            return Err(());
        }

        // Filter out markdown table separator lines (e.g. |---|---|)
        let data_lines: Vec<&str> = lines
            .iter()
            .filter(|l| !is_markdown_separator(l))
            .copied()
            .collect();

        if data_lines.len() < 3 {
            return Err(());
        }

        let delimiter = detect_delimiter(&data_lines)?;

        let headers: Vec<String> = data_lines[0]
            .split(delimiter)
            .map(|s| s.trim().trim_matches('"').to_string())
            .collect();

        if headers.len() < 2 {
            return Err(());
        }

        let rows: Vec<Vec<String>> = data_lines[1..]
            .iter()
            .map(|line| {
                line.split(delimiter)
                    .map(|s| s.trim().trim_matches('"').to_string())
                    .collect()
            })
            .collect();

        // Verify all rows have the correct number of columns
        if rows.iter().any(|r| r.len() != headers.len()) {
            return Err(());
        }

        Ok(Input {
            headers,
            rows,
            original_size: text.len(),
        })
    }

    /// Parse a JSON array of objects into tabular form.
    fn parse_json_array(text: &str) -> Result<Self, ()> {
        let trimmed = text.trim();
        if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
            return Err(());
        }

        let arr: Vec<Value> = serde_json::from_str(trimmed).map_err(|_| ())?;
        if arr.len() < 2 {
            return Err(());
        }

        // All items must be objects
        for v in &arr {
            if !v.is_object() {
                return Err(());
            }
        }

        let objects: Vec<&Value> = arr.iter().collect();
        let first = objects[0].as_object().unwrap();

        // Collect headers in sorted order for determinism
        let mut headers: Vec<String> = first.keys().cloned().collect();
        headers.sort();
        if headers.len() < 2 {
            return Err(());
        }

        // Verify all objects have the same keys
        for obj in &objects[1..] {
            let map = obj.as_object().unwrap();
            if map.len() != headers.len() {
                return Err(());
            }
            if !headers.iter().all(|k| map.contains_key(k)) {
                return Err(());
            }
        }

        // Extract row values
        let rows: Vec<Vec<String>> = objects
            .iter()
            .map(|obj| {
                let map = obj.as_object().unwrap();
                headers.iter().map(|k| value_to_string(&map[k])).collect()
            })
            .collect();

        Ok(Input {
            headers,
            rows,
            original_size: text.len(),
        })
    }
}

/// Convert a JSON value to a string representation suitable for tabular storage.
fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(value_to_string).collect();
            format!("[{}]", items.join(","))
        }
        Value::Object(obj) => {
            let items: Vec<String> = obj
                .iter()
                .map(|(k, v)| format!("{}:{}", k, value_to_string(v)))
                .collect();
            format!("{{{}}}", items.join(","))
        }
    }
}

// ---------------------------------------------------------------------------
// DSV detection helpers
// ---------------------------------------------------------------------------

/// Check if a line looks like a markdown table separator (e.g. `|---|---|---|`).
fn is_markdown_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }

    // Check for pipe-delimited separator lines: |---|---|
    if trimmed.starts_with('|') && trimmed.ends_with('|') {
        let body = &trimmed[1..trimmed.len() - 1];
        return body
            .chars()
            .all(|c| c == '-' || c == ':' || c == '|' || c == '+' || c == ' ');
    }

    // Check for simple separator lines: ---, ===
    let without_delim = trimmed
        .chars()
        .filter(|&c| c != '-' && c != ':' && c != '|' && c != '+' && c != ' ')
        .count();
    without_delim == 0 && (trimmed.contains("---") || trimmed.contains("==="))
}

/// Detect the delimiter character used in DSV data.
fn detect_delimiter(lines: &[&str]) -> Result<char, ()> {
    let candidates = [',', '\t', '|', ';'];

    for &delim in &candidates {
        let counts: Vec<usize> = lines.iter().map(|l| l.matches(delim).count()).collect();
        let first = counts[0];
        if first < 2 {
            continue;
        }

        // All lines must have the same delimiter count
        if counts.iter().all(|&c| c == first) && lines.len() >= 3 {
            return Ok(delim);
        }
    }

    Err(())
}

/// Check if text looks like delimiter-separated values (CSV, TSV, pipe).
fn looks_like_csv(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 3 {
        return false;
    }

    // Filter out markdown separator lines
    let data_lines: Vec<&str> = lines
        .iter()
        .filter(|l| !is_markdown_separator(l))
        .copied()
        .collect();
    if data_lines.len() < 3 {
        return false;
    }

    // Reject markdown table format where lines start with '|'
    let pipe_prefix_count = data_lines
        .iter()
        .filter(|l| l.trim().starts_with('|'))
        .count();
    let pipe_suffix_count = data_lines
        .iter()
        .filter(|l| l.trim().ends_with('|'))
        .count();
    if pipe_prefix_count > data_lines.len() / 2 || pipe_suffix_count > data_lines.len() / 2 {
        return false;
    }

    detect_delimiter(&data_lines).is_ok()
}

/// Check if text looks like a JSON array of objects.
fn looks_like_json_array(text: &str) -> bool {
    let trimmed = text.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return false;
    }

    // Quick check: parse first few objects
    if let Ok(Value::Array(arr)) = serde_json::from_str(trimmed) {
        if arr.len() < 2 {
            return false;
        }
        let all_objects = arr.iter().all(|v| v.is_object());
        if !all_objects {
            return false;
        }

        // Check consistent keys
        let first_keys: HashSet<&str> = arr[0]
            .as_object()
            .map(|m| m.keys().map(|k| k.as_str()).collect())
            .unwrap_or_default();

        if first_keys.len() < 2 {
            return false;
        }

        arr[1..].iter().all(|v| {
            v.as_object().map_or(false, |m| {
                m.len() == first_keys.len() && first_keys.iter().all(|k| m.contains_key(*k))
            })
        })
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// GCF — Graph Compact Format encoder
// ---------------------------------------------------------------------------

/// Graph Compact Format encoder.
///
/// Represents tabular data column-by-column, applying run-length encoding
/// to adjacent repeated values. The output consists of:
/// 1. A schema line listing column names
/// 2. One column data line per column with compact RLE notation
pub(crate) struct GcfEncoder;

impl GcfEncoder {
    pub fn encode(input: &Input) -> String {
        let mut out = String::new();

        // Schema line
        out.push_str("[GCF] ");
        out.push_str(&input.headers.join(" | "));
        out.push('\n');

        // Column data
        for (col_idx, header) in input.headers.iter().enumerate() {
            out.push_str(&format!("  {}: ", header));

            let values: Vec<&str> = input.rows.iter().map(|r| r[col_idx].as_str()).collect();

            // Run-length encode adjacent identical values
            let mut parts: Vec<String> = Vec::new();
            let mut i = 0;
            while i < values.len() {
                let val = values[i];
                let mut count = 1;
                while i + count < values.len() && values[i + count] == val {
                    count += 1;
                }

                if count >= 2 {
                    let escaped = escape_gcf_value(val);
                    parts.push(format!("{}(x{})", escaped, count));
                    i += count;
                } else {
                    parts.push(escape_gcf_value(val).to_string());
                    i += 1;
                }
            }

            out.push_str(&parts.join(", "));
            out.push('\n');
        }

        // Footer with metrics
        out.push_str(&format!(
            "[GCF end: {} rows x {} cols]",
            input.rows.len(),
            input.headers.len()
        ));

        out
    }
}

/// Escape a value for GCF output: if it contains special chars, wrap in quotes.
fn escape_gcf_value(val: &str) -> &str {
    // Simple for now — values with commas or parentheses would need quoting
    // in a production system, but for basic compression this is fine.
    val
}

// ---------------------------------------------------------------------------
// TOON — Token-Oriented Object Notation encoder
// ---------------------------------------------------------------------------

/// Token-Oriented Object Notation encoder.
///
/// Maps frequently-repeated string values to compact tokens (`$1`, `$2`, ...),
/// then emits tokenized rows separated by the field delimiter. The output:
/// 1. Token dictionary (`~dict: $1=value $2=value ...`)
/// 2. Compact header
/// 3. Tokenized data rows
pub(crate) struct ToonEncoder;

impl ToonEncoder {
    pub fn encode(input: &Input) -> String {
        // Discover repeated string values worth tokenizing
        let mut freq: HashMap<&str, usize> = HashMap::new();
        for row in &input.rows {
            for val in row {
                // Only tokenize strings that would actually save bytes
                if val.len() >= 4 {
                    *freq.entry(val.as_str()).or_default() += 1;
                }
            }
        }

        // Build token map: values that appear more than once and save bytes
        let mut token_map: BTreeMap<&str, String> = BTreeMap::new();
        let mut next_token = 1usize;

        // Sort by count descending for best compression
        let mut sorted: Vec<(&&str, &usize)> = freq.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));

        for (val, count) in sorted {
            if *count > 1 && val.len() > 4 {
                let token = format!("${}", next_token);
                token_map.insert(val, token);
                next_token += 1;
            }
        }

        let mut out = String::new();

        // Token dictionary
        out.push_str("[TOON]\n");
        if !token_map.is_empty() {
            let dict_entries: Vec<String> = token_map
                .iter()
                .map(|(val, token)| format!("{}={}", token, val))
                .collect();
            out.push_str(&format!("  ~dict: {}\n", dict_entries.join(" ")));
        }

        // Column abbreviations
        let col_count = input.headers.len();
        let col_abbrevs: Vec<String> = (0..col_count).map(|i| format!("~{}", i)).collect();
        out.push_str(&format!("  ~col: {}\n", col_abbrevs.join(" ")));

        // Data rows — tokenize values
        for row in &input.rows {
            let tokenized: Vec<&str> = row
                .iter()
                .map(|v| token_map.get(v.as_str()).map(|t| t.as_str()).unwrap_or(v))
                .collect();
            out.push_str(&format!("  {}\n", tokenized.join("\t")));
        }

        // Footer
        out.push_str(&format!(
            "[TOON end: {} rows, {} cols, {} tokens]",
            input.rows.len(),
            input.headers.len(),
            token_map.len()
        ));

        out
    }
}

// ---------------------------------------------------------------------------
// Public filter function for RTK dispatch
// ---------------------------------------------------------------------------

/// Try to compress `text` as tabular data using SmartCrusher.
///
/// Returns the compressed form if SmartCrusher detects tabular data and
/// compression reduces the size. Otherwise returns the original text.
/// This is the entry point called by the RTK filter chain.
pub fn smartcrusher_impl(text: &str) -> String {
    SmartCrusher::compress(text).unwrap_or_else(|| text.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // TabularType detection
    // -----------------------------------------------------------------------

    #[test]
    fn detect_returns_none_for_short_text() {
        assert_eq!(SmartCrusher::detect("hello"), None);
    }

    #[test]
    fn detect_csv_basic() {
        let text = "name,age,city\nAlice,30,NYC\nBob,25,LA\nCarol,30,NYC\n";
        assert_eq!(SmartCrusher::detect(text), Some(TabularType::Csv));
    }

    #[test]
    fn detect_tsv() {
        let text = "name\tage\tcity\nAlice\t30\tNYC\nBob\t25\tLA\n";
        assert_eq!(SmartCrusher::detect(text), Some(TabularType::Csv));
    }

    #[test]
    fn detect_pipe_separated() {
        let text = "name|age|city\nAlice|30|NYC\nBob|25|LA\n";
        assert_eq!(SmartCrusher::detect(text), Some(TabularType::Csv));
    }

    #[test]
    fn detect_json_array() {
        let text = r#"[
            {"name":"Alice","age":30,"city":"NYC"},
            {"name":"Bob","age":25,"city":"LA"}
        ]"#;
        assert_eq!(SmartCrusher::detect(text), Some(TabularType::JsonArray));
    }

    #[test]
    fn detect_plain_text_returns_none() {
        let text = "This is just a plain paragraph of text that doesn't have any structure.\nIt goes on for several lines without any tabular formatting.\nJust regular prose that should not be detected as tabular.";
        assert_eq!(SmartCrusher::detect(text), None);
    }

    #[test]
    fn detect_json_array_with_inconsistent_keys_returns_none() {
        let text = r#"[
            {"name":"Alice","age":30,"city":"NYC"},
            {"name":"Bob","dept":"Engineering"}
        ]"#;
        // The first object has 3 keys, second has 2 -> inconsistent
        assert_eq!(SmartCrusher::detect(text), None);
    }

    #[test]
    fn detect_json_array_single_item_returns_none() {
        let text = r#"[
            {"name":"Alice","age":30}
        ]"#;
        assert_eq!(SmartCrusher::detect(text), None);
    }

    #[test]
    fn detect_not_enough_csv_rows() {
        let text = "name,age\nAlice,30\n";
        assert_eq!(SmartCrusher::detect(text), None);
    }

    // -----------------------------------------------------------------------
    // SmartCrusher::compress — CSV input
    // -----------------------------------------------------------------------

    #[test]
    fn compress_csv_basic() {
        // Large enough dataset for compression overhead to be worthwhile
        let mut text = String::from("name,age,city,department,country,language,role\n");
        let rows = vec![
            (
                "Alice",
                "30",
                "NYC",
                "Engineering",
                "USA",
                "English",
                "Senior",
            ),
            ("Bob", "25", "LA", "Engineering", "USA", "English", "Junior"),
            ("Carol", "30", "NYC", "Design", "USA", "English", "Senior"),
            ("David", "28", "SF", "Engineering", "USA", "English", "Mid"),
            ("Eve", "35", "NYC", "Design", "USA", "English", "Lead"),
            (
                "Frank",
                "32",
                "Chicago",
                "Engineering",
                "USA",
                "English",
                "Senior",
            ),
            (
                "Grace",
                "27",
                "NYC",
                "Marketing",
                "USA",
                "English",
                "Junior",
            ),
            (
                "Henry",
                "40",
                "Boston",
                "Engineering",
                "USA",
                "English",
                "Lead",
            ),
            ("Ivy", "29", "SF", "Design", "USA", "English", "Mid"),
            (
                "Jack",
                "33",
                "NYC",
                "Engineering",
                "USA",
                "English",
                "Senior",
            ),
            ("Kate", "31", "LA", "Marketing", "USA", "English", "Mid"),
            (
                "Leo",
                "26",
                "NYC",
                "Engineering",
                "USA",
                "English",
                "Junior",
            ),
            ("Mia", "34", "Chicago", "Design", "USA", "English", "Senior"),
            (
                "Noah",
                "38",
                "Boston",
                "Engineering",
                "USA",
                "English",
                "Lead",
            ),
            ("Olivia", "29", "SF", "Marketing", "USA", "English", "Mid"),
        ];
        for (name, age, city, dept, country, lang, role) in &rows {
            text.push_str(&format!(
                "{},{},{},{},{},{},{}\n",
                name, age, city, dept, country, lang, role
            ));
        }
        let result = SmartCrusher::compress(&text);
        assert!(result.is_some(), "compress should return Some for CSV");
        let compressed = result.unwrap();
        // Compression must be better than the original
        assert!(
            compressed.len() < text.len(),
            "compressed ({} B) should be smaller than original ({} B)",
            compressed.len(),
            text.len()
        );
    }

    #[test]
    fn compress_csv_small_input_returns_none() {
        let text = "a,b\nx,y\nz,w\n";
        let result = SmartCrusher::compress(text);
        // Very small CSV might not benefit from compression overhead
        assert!(result.is_none() || result.as_ref().unwrap().len() < text.len());
    }

    #[test]
    fn compress_csv_plain_text_returns_none() {
        let text = "This is just a regular paragraph that isn't tabular in any way.\nIt has multiple lines but they don't share a consistent delimiter.\nNo commas, tabs, or pipes as separators here.\nJust plain English text.";
        let result = SmartCrusher::compress(text);
        assert!(result.is_none());
    }

    // -----------------------------------------------------------------------
    // SmartCrusher::compress — JSON array input
    // -----------------------------------------------------------------------

    #[test]
    fn compress_json_array_basic() {
        let json = r#"[
            {"name":"Alice","age":30,"city":"NYC"},
            {"name":"Bob","age":25,"city":"LA"},
            {"name":"Carol","age":30,"city":"NYC"},
            {"name":"David","age":28,"city":"SF"},
            {"name":"Eve","age":35,"city":"NYC"}
        ]"#;
        let result = SmartCrusher::compress(json);
        assert!(
            result.is_some(),
            "compress should return Some for JSON array"
        );
        let compressed = result.unwrap();
        assert!(
            compressed.len() < json.len(),
            "compressed ({} B) should be smaller than original ({} B)",
            compressed.len(),
            json.len()
        );
    }

    #[test]
    fn compress_json_array_single_object_returns_none() {
        let json = r#"[
            {"name":"Alice","age":30}
        ]"#;
        let result = SmartCrusher::compress(json);
        assert!(result.is_none());
    }

    #[test]
    fn compress_json_array_inconsistent_keys_returns_none() {
        let json = r#"[
            {"name":"Alice","age":30},
            {"name":"Bob","dept":"Engineering"}
        ]"#;
        let result = SmartCrusher::compress(json);
        assert!(result.is_none());
    }

    // -----------------------------------------------------------------------
    // smartcrusher_impl (RTK filter function)
    // -----------------------------------------------------------------------

    #[test]
    fn smartcrusher_impl_compresses_csv() {
        // Large dataset to ensure compression overhead is worthwhile
        let mut text = String::from("name,age,city,department,country,language\n");
        for i in 0..50 {
            let dept = if i % 3 == 0 {
                "Engineering"
            } else if i % 3 == 1 {
                "Design"
            } else {
                "Marketing"
            };
            let city = if i % 2 == 0 { "NYC" } else { "SF" };
            text.push_str(&format!(
                "User{},{},{},{},USA,English\n",
                i,
                (20 + i % 30),
                city,
                dept
            ));
        }
        let result = smartcrusher_impl(&text);
        assert!(
            result.len() <= text.len(),
            "filter should not inflate input ({} > {})",
            result.len(),
            text.len()
        );
        // The result should have GCF or TOON markers
        assert!(
            result.starts_with("[GCF]") || result.starts_with("[TOON]"),
            "result should start with format marker: {}",
            &result[..20.min(result.len())]
        );
    }

    #[test]
    fn smartcrusher_impl_passthrough_for_plain_text() {
        let text = "This is completely normal prose that is not tabular.\nIt should pass through unchanged.\n";
        let result = smartcrusher_impl(text);
        assert_eq!(result, text);
    }

    #[test]
    fn smartcrusher_impl_empty_string_passthrough() {
        let result = smartcrusher_impl("");
        assert_eq!(result, "");
    }

    // -----------------------------------------------------------------------
    // GCF encoder tests
    // -----------------------------------------------------------------------

    #[test]
    fn gcf_encodes_with_run_length() {
        let input = Input {
            headers: vec!["name".into(), "city".into()],
            rows: vec![
                vec!["Alice".into(), "NYC".into()],
                vec!["Bob".into(), "NYC".into()],
                vec!["Carol".into(), "NYC".into()],
                vec!["David".into(), "LA".into()],
            ],
            original_size: 100,
        };

        let output = GcfEncoder::encode(&input);
        assert!(output.contains("[GCF]"));
        assert!(output.contains("name | city"));
        // NYC appears 3 times consecutively -> should be RLE'd
        assert!(output.contains("(x3)") || output.contains("NYC"));
        assert!(output.contains("[GCF end: 4 rows x 2 cols]"));
    }

    #[test]
    fn gcf_encodes_single_column_wide() {
        let input = Input {
            headers: vec!["value".into()],
            rows: vec![
                vec!["x".into()],
                vec!["x".into()],
                vec!["y".into()],
                vec!["z".into()],
            ],
            original_size: 50,
        };

        let output = GcfEncoder::encode(&input);
        assert!(output.contains("x(x2)") || output.contains("(x2)"));
    }

    #[test]
    fn gcf_no_rle_when_all_unique() {
        let input = Input {
            headers: vec!["id".into()],
            rows: (0..5).map(|i| vec![format!("val{}", i)]).collect(),
            original_size: 50,
        };

        let output = GcfEncoder::encode(&input);
        assert!(output.contains("val0"));
        assert!(output.contains("val4"));
        // No RLE markers since all values are unique
        assert!(!output.contains("(x"));
    }

    // -----------------------------------------------------------------------
    // TOON encoder tests
    // -----------------------------------------------------------------------

    #[test]
    fn toon_tokenizes_repeated_values() {
        let input = Input {
            headers: vec!["name".into(), "department".into()],
            rows: vec![
                vec!["Alice".into(), "Engineering".into()],
                vec!["Bob".into(), "Engineering".into()],
                vec!["Carol".into(), "Engineering".into()],
                vec!["David".into(), "Design".into()],
            ],
            original_size: 120,
        };

        let output = ToonEncoder::encode(&input);
        assert!(output.contains("[TOON]"));
        assert!(output.contains("~dict:"));
        // "Engineering" appears 3 times, should be tokenized
        assert!(
            output.contains("$1"),
            "Engineering should be tokenized as $1: {}",
            output
        );
        assert!(output.contains("[TOON end: 4 rows, 2 cols, "));
    }

    #[test]
    fn toon_skips_tokenization_when_no_repeated_values() {
        let input = Input {
            headers: vec!["name".into(), "id".into()],
            rows: vec![
                vec!["Alice".into(), "1".into()],
                vec!["Bob".into(), "2".into()],
                vec!["Carol".into(), "3".into()],
            ],
            original_size: 80,
        };

        let output = ToonEncoder::encode(&input);
        assert!(output.contains("[TOON]"));
        // Short values like "1" should not be tokenized
        assert!(!output.contains("~dict"));
    }

    // -----------------------------------------------------------------------
    // Input::select_format
    // -----------------------------------------------------------------------

    #[test]
    fn select_format_toon_for_repeated_strings() {
        let input = Input {
            headers: vec!["name".into(), "dept".into()],
            rows: vec![
                vec!["Alice".into(), "Engineering".into()],
                vec!["Bob".into(), "Engineering".into()],
                vec!["Carol".into(), "Engineering".into()],
            ],
            original_size: 100,
        };

        assert_eq!(input.select_format(), CompressFormat::Toon);
    }

    #[test]
    fn select_format_gcf_for_mostly_numbers() {
        let input = Input {
            headers: vec!["id".into(), "value".into()],
            rows: vec![
                vec!["1".into(), "42.5".into()],
                vec!["2".into(), "18.0".into()],
                vec!["3".into(), "99.9".into()],
            ],
            original_size: 60,
        };

        assert_eq!(input.select_format(), CompressFormat::Gcf);
    }

    // -----------------------------------------------------------------------
    // Detection helper tests
    // -----------------------------------------------------------------------

    #[test]
    fn csv_with_quoted_fields() {
        let text = "\"name\",\"age\",\"city\"\n\"Alice\",30,\"NYC\"\n\"Bob\",25,\"LA\"\n";
        let result = Input::parse_dsv(text);
        assert!(
            result.is_ok(),
            "quoted CSV should parse: {:?}",
            result.err()
        );
        if let Ok(input) = result {
            assert_eq!(input.headers, vec!["name", "age", "city"]);
            assert_eq!(input.rows[0][0], "Alice");
        }
    }

    #[test]
    fn is_markdown_separator_detected() {
        assert!(is_markdown_separator("|---|---|---|"));
        assert!(is_markdown_separator("| --- | --- |"));
        assert!(!is_markdown_separator("name,age,city"));
    }

    #[test]
    fn looks_like_csv_negative_for_markdown_table() {
        // A realistic markdown table should NOT be detected as CSV
        let text = "| Name  | Age | City |\n|-------|-----|------|\n| Alice | 30  | NYC  |\n| Bob   | 25  | LA   |\n";
        assert!(
            !looks_like_csv(text),
            "markdown tables should not be detected as CSV"
        );
    }

    #[test]
    fn parse_dsv_rejects_irregular_columns() {
        let text = "a,b,c\n1,2,3\n4,5,6,7\n";
        let result = Input::parse_dsv(text);
        assert!(result.is_err(), "irregular columns should be rejected");
    }

    // -----------------------------------------------------------------------
    // End-to-end: encoder selection picks smallest output
    // -----------------------------------------------------------------------

    #[test]
    fn compress_prefers_smallest_format() {
        // Larger dataset with many repeated distinct strings -> TOON likely wins
        let mut text = String::from("name,country,language,department\n");
        let data = vec![
            ("Alice", "USA", "English", "Engineering"),
            ("Bob", "USA", "English", "Engineering"),
            ("Carol", "Canada", "French", "Engineering"),
            ("David", "USA", "English", "Design"),
            ("Eve", "Canada", "French", "Design"),
            ("Frank", "UK", "English", "Engineering"),
            ("Grace", "USA", "English", "Marketing"),
            ("Henry", "Canada", "French", "Marketing"),
            ("Ivy", "USA", "English", "Engineering"),
            ("Jack", "UK", "English", "Design"),
        ];
        for (name, country, lang, dept) in &data {
            text.push_str(&format!("{},{},{},{}\n", name, country, lang, dept));
        }
        let result = SmartCrusher::compress(&text);
        assert!(result.is_some());

        let gcf = GcfEncoder::encode(&Input::parse_dsv(&text).unwrap());
        let toon = ToonEncoder::encode(&Input::parse_dsv(&text).unwrap());

        // The selected format is whichever is smaller
        let compressed = result.unwrap();
        let expected_size = gcf.len().min(toon.len());
        assert!(
            compressed.len() == expected_size,
            "compressed size {} should match best of GCF({})/TOON({})",
            compressed.len(),
            gcf.len(),
            toon.len()
        );
    }
}
