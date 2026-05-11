//! Shared idempotent `apply --from-file` plumbing for resource commands.
//!
//! All `*_apply` subcommands accept a YAML or JSON document on stdin (`-`)
//! or a path, parse it into a list of payloads, then diff against the
//! current DB snapshot and produce a single mutation. The output envelope
//! is the same across resources:
//!
//! ```text
//! { "created": [...], "updated": [...], "unchanged": [...], "deleted": [...] }
//! ```
//!
//! `--prune` opts in to deleting resources that are present in DB but not
//! in the input document (kubectl-style). Default behaviour is upsert only.

use std::fs;
use std::io::{self, Read};
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFormat {
    Json,
    Yaml,
}

/// Load a YAML or JSON document from a path or from stdin (`-`).
///
/// Returns the parsed `serde_json::Value` plus the detected format.
pub fn load_document(source: &str) -> anyhow::Result<(Value, InputFormat)> {
    let raw = read_source(source)?;
    let trimmed = raw.trim_start();
    let (value, fmt) = if trimmed.starts_with('{') || trimmed.starts_with('[') {
        (
            serde_json::from_str::<Value>(&raw)
                .map_err(|e| anyhow::anyhow!("failed to parse JSON: {e}"))?,
            InputFormat::Json,
        )
    } else {
        let yaml = serde_yml::from_str::<serde_yml::Value>(&raw)
            .map_err(|e| anyhow::anyhow!("failed to parse YAML: {e}"))?;
        let bytes = serde_json::to_vec(&yaml)
            .map_err(|e| anyhow::anyhow!("failed to convert YAML to JSON: {e}"))?;
        (
            serde_json::from_slice::<Value>(&bytes)
                .map_err(|e| anyhow::anyhow!("failed to normalise YAML payload: {e}"))?,
            InputFormat::Yaml,
        )
    };
    Ok((value, fmt))
}

fn read_source(source: &str) -> anyhow::Result<String> {
    if source == "-" {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        Ok(buf)
    } else {
        let path = Path::new(source);
        Ok(
            fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("failed to read {source}: {e}"))?,
        )
    }
}

/// Parse a `Value` as either a single object or an array of objects.
///
/// Returns the items, plus an error if the shape is wrong. Used by every
/// `apply` command so a user can pipe in either form.
pub fn into_items<T: for<'de> Deserialize<'de>>(value: Value) -> anyhow::Result<Vec<T>> {
    match value {
        Value::Array(items) => items
            .into_iter()
            .enumerate()
            .map(|(idx, item)| {
                serde_json::from_value::<T>(item)
                    .map_err(|e| anyhow::anyhow!("invalid item at index {idx}: {e}"))
            })
            .collect(),
        Value::Object(_) => {
            Ok(vec![serde_json::from_value::<T>(value)
                .map_err(|e| anyhow::anyhow!("invalid item: {e}"))?])
        }
        other => Err(anyhow::anyhow!(
            "expected an object or array, got {}",
            type_name(&other)
        )),
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Aggregated diff returned by every `*_apply` command.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct ApplyDiff {
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub unchanged: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<String>,
}

impl ApplyDiff {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.created.is_empty() {
            parts.push(format!("{} created", self.created.len()));
        }
        if !self.updated.is_empty() {
            parts.push(format!("{} updated", self.updated.len()));
        }
        if !self.unchanged.is_empty() {
            parts.push(format!("{} unchanged", self.unchanged.len()));
        }
        if !self.deleted.is_empty() {
            parts.push(format!("{} deleted", self.deleted.len()));
        }
        if parts.is_empty() {
            "nothing to do".into()
        } else {
            parts.join(", ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize, Debug, PartialEq)]
    struct Item {
        name: String,
    }

    #[test]
    fn into_items_accepts_single_object() {
        let value: Value = serde_json::from_str(r#"{"name":"foo"}"#).unwrap();
        let items: Vec<Item> = into_items(value).unwrap();
        assert_eq!(items, vec![Item { name: "foo".into() }]);
    }

    #[test]
    fn into_items_accepts_array() {
        let value: Value = serde_json::from_str(r#"[{"name":"a"},{"name":"b"}]"#).unwrap();
        let items: Vec<Item> = into_items(value).unwrap();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn into_items_rejects_string() {
        let value: Value = Value::String("nope".into());
        assert!(into_items::<Item>(value).is_err());
    }

    #[test]
    fn apply_diff_summary() {
        let mut diff = ApplyDiff::default();
        diff.created = vec!["a".into()];
        diff.unchanged = vec!["b".into(), "c".into()];
        assert_eq!(diff.summary(), "1 created, 2 unchanged");
    }

    #[test]
    fn apply_diff_empty_summary() {
        assert_eq!(ApplyDiff::default().summary(), "nothing to do");
    }
}
