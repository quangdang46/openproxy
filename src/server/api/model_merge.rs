//! Model-listing merge semantics — port of OmniRoute's managed-import merge.
//!
//! Upstream reference (`/tmp/omniroute_latest`, SHA `1853a618`):
//! - `src/lib/providers/mergeProviderModelListing.ts:57-118`
//!   (`mergeProviderModelListing`)
//! - `src/lib/providers/modelMetadataPrecedence.ts:20-30`
//!   (`mergeModelsWithCustomPrecedence`, `mergeCustomModelMetadata`)
//! - `src/lib/providerModels/managedModelImport.ts:240,329`
//!   (`preserveRemovedCustomModelCompat` — saves compat knobs from removed
//!   `imported` rows into per-model compat overrides so a later re-import of
//!   the same model id keeps the operator's tuning)
//! - `src/lib/providerModels/managedModelImport.ts:60`
//!   (`normalizeManagedSource`: `api-sync`/`auto-sync`/`imported` → `imported`)
//!
//! Rules ported here:
//! 1. `imported` rows (source `imported`/`api-sync`/`auto-sync`) from a prior
//!    sync that are absent from the fresh listing are **dropped** (stale).
//! 2. Operator rows (any other source, including untagged legacy rows) are
//!    **always preserved**, even when absent from the fresh listing.
//! 3. When an operator row shares an id with a fresh row, the operator's
//!    defined fields win (custom precedence overlay) while richer discovered
//!    metadata for fields the custom row does not define is kept.
//! 4. Compat knobs on a dropped `imported` row are preserved via
//!    [`CompatOverrideStore`] so re-imports restore the operator's tuning.
//!
//! NOTE: openproxy's `CustomModel.extra` is an untyped bag, so "compat knobs"
//! are the known tuning keys listed in [`COMPAT_KEYS`]. OmniRoute's
//! `getCompatPatchFromCustomModel` covers
//! (`normalizeToolCallId`, `preserveOpenAIDeveloperRole`, `isHidden`,
//! `compatByProtocol`, `upstreamHeaders`) — none of which openproxy reads yet
//! (grep: no hits in `src/`). They are still preserved verbatim so the data
//! survives for the future reader; the keys openproxy *does* read today are
//! `targetFormat` and `contextWindow`-family metadata.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use crate::types::CustomModel;

/// Sources that mark a row as machine-imported (OmniRoute
/// `normalizeManagedSource`: `api-sync`/`auto-sync`/`imported` → `imported`).
fn is_imported_source(source: Option<&str>) -> bool {
    matches!(
        source.map(str::trim).map(str::to_lowercase).as_deref(),
        Some("imported" | "api-sync" | "auto-sync")
    )
}

/// The `extra.source` tag on a stored row. Untagged legacy rows are operator
/// rows (`manual`), never dropped.
fn row_source(model: &CustomModel) -> Option<&str> {
    model.extra.get("source").and_then(Value::as_str)
}

/// Compat/tuning keys preserved across a drop + re-import round-trip
/// (OmniRoute `getCompatPatchFromCustomModel` + `copyImportedModelMetadata`).
const COMPAT_KEYS: &[&str] = &[
    // OmniRoute compat patch (no openproxy reader yet — preserved verbatim).
    "normalizeToolCallId",
    "preserveOpenAIDeveloperRole",
    "isHidden",
    "compatByProtocol",
    "upstreamHeaders",
    // Metadata openproxy reads today.
    "targetFormat",
    "upstreamProtocol",
    "supportedEndpoints",
    "supportedThinkingEfforts",
    "defaultThinkingEffort",
    "inputTokenLimit",
    "contextWindow",
    "outputTokenLimit",
    "description",
    "supportsThinking",
    "alwaysThinking",
    "supportsTools",
    "supportsVideo",
];

/// Per-(alias, model id) compat overrides saved from dropped `imported` rows
/// (OmniRoute `mergeModelCompatOverride`, applied on re-import).
#[derive(Debug, Default)]
pub struct CompatOverrideStore {
    overrides: HashMap<(String, String), BTreeMap<String, Value>>,
}

impl CompatOverrideStore {
    pub fn save(&mut self, alias: &str, id: &str, extra: &BTreeMap<String, Value>) {
        let patch: BTreeMap<String, Value> = COMPAT_KEYS
            .iter()
            .filter_map(|k| extra.get(*k).map(|v| (k.to_string(), v.clone())))
            .collect();
        if !patch.is_empty() {
            self.overrides
                .insert((alias.to_string(), id.to_string()), patch);
        }
    }

    pub fn take(&mut self, alias: &str, id: &str) -> Option<BTreeMap<String, Value>> {
        self.overrides.remove(&(alias.to_string(), id.to_string()))
    }
}

/// A freshly discovered model row (sync snapshot or live `/models` fetch).
#[derive(Debug, Clone)]
pub struct DiscoveredModel {
    pub id: String,
    pub name: Option<String>,
    pub extra: BTreeMap<String, Value>,
}

/// Result of [`merge_model_listing`].
#[derive(Debug, Default)]
pub struct ModelMergeOutcome {
    /// Rows to upsert (fresh `imported` rows + preserved operator rows).
    pub keep: Vec<CustomModel>,
    /// Previously-synced `imported` ids absent from the fresh listing.
    pub dropped_imported: Vec<String>,
    /// Operator ids absent from the fresh listing (kept, reported for UI).
    pub preserved_custom: Vec<String>,
}

/// Merge a fresh model listing over stored rows with OmniRoute semantics.
///
/// - `previous`: all stored `CustomModel` rows for the provider alias.
/// - `fresh`: the newly discovered listing.
/// - `model_type`: the `CustomModel.type` for fresh rows (`"llm"`).
/// - `compat`: compat-override store (mutated: saves patches from dropped
///   rows, re-applies them onto re-imported ids).
pub fn merge_model_listing(
    previous: &[CustomModel],
    fresh: &[DiscoveredModel],
    model_type: &str,
    compat: &mut CompatOverrideStore,
) -> ModelMergeOutcome {
    let mut outcome = ModelMergeOutcome::default();
    let fresh_ids: std::collections::BTreeSet<&str> = fresh.iter().map(|m| m.id.as_str()).collect();

    // Index previous rows by id; operator rows win ties (first wins below).
    let mut prev_by_id: HashMap<&str, &CustomModel> = HashMap::new();
    for m in previous {
        prev_by_id.entry(m.id.as_str()).or_insert(m);
    }

    for m in previous {
        let imported = is_imported_source(row_source(m));
        if fresh_ids.contains(m.id.as_str()) {
            continue; // handled in the fresh loop (overlay or refresh)
        }
        if imported {
            // Stale imported row → drop, but save compat knobs.
            compat.save(&m.provider_alias, &m.id, &m.extra);
            outcome.dropped_imported.push(m.id.clone());
        } else {
            // Operator row absent upstream → preserve verbatim.
            outcome.preserved_custom.push(m.id.clone());
            outcome.keep.push(m.clone());
        }
    }

    for disc in fresh {
        let alias = previous
            .first()
            .map(|m| m.provider_alias.clone())
            .unwrap_or_default();
        // Re-apply saved compat overrides (drop → re-import round-trip).
        let mut extra = disc.extra.clone();
        if let Some(patch) = compat.take(&alias, &disc.id) {
            for (k, v) in patch {
                extra.entry(k).or_insert(v);
            }
        }
        match prev_by_id.get(disc.id.as_str()) {
            Some(prev) if !is_imported_source(row_source(prev)) => {
                // Custom precedence overlay: operator's defined fields win.
                let mut merged_extra = extra;
                for (k, v) in &prev.extra {
                    if *k != "source" {
                        merged_extra.insert(k.clone(), v.clone());
                    }
                }
                // Keep the operator's source tag so the row is never
                // mistaken for a machine-imported row.
                if let Some(s) = row_source(prev) {
                    merged_extra.insert("source".to_string(), Value::String(s.to_string()));
                }
                outcome.keep.push(CustomModel {
                    provider_alias: prev.provider_alias.clone(),
                    id: disc.id.clone(),
                    r#type: prev.r#type.clone(),
                    name: prev.name.clone().or(disc.name.clone()),
                    extra: merged_extra,
                });
            }
            _ => {
                // Fresh imported row (new, refreshed, or replacing a stale
                // imported row — the old one was already dropped above).
                let mut final_extra = extra;
                final_extra
                    .entry("source".to_string())
                    .or_insert_with(|| Value::String("imported".to_string()));
                outcome.keep.push(CustomModel {
                    provider_alias: alias,
                    id: disc.id.clone(),
                    r#type: model_type.to_string(),
                    name: disc.name.clone(),
                    extra: final_extra,
                });
            }
        }
    }

    outcome.dropped_imported.sort();
    outcome.preserved_custom.sort();
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(id: &str, source: Option<&str>, note: Option<&str>) -> CustomModel {
        let mut extra = BTreeMap::new();
        if let Some(s) = source {
            extra.insert("source".into(), Value::String(s.into()));
        }
        if let Some(n) = note {
            extra.insert("note".into(), Value::String(n.into()));
        }
        CustomModel {
            provider_alias: "ds-web".into(),
            id: id.into(),
            r#type: "llm".into(),
            name: Some(id.into()),
            extra,
        }
    }

    fn disc(id: &str) -> DiscoveredModel {
        DiscoveredModel {
            id: id.into(),
            name: Some(id.into()),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn operator_rows_survive_sync_while_stale_imported_drop() {
        // OmniRoute managedModelImport.ts:292-303 — imported rows are replaced,
        // operator rows persist.
        let prev = vec![
            custom("keep-custom", Some("custom"), None),
            custom("legacy-untagged", None, None),
            custom("stale-imported", Some("imported"), None),
            custom("stale-api-sync", Some("api-sync"), None),
        ];
        let fresh = vec![disc("keep-custom"), disc("brand-new")];
        let mut compat = CompatOverrideStore::default();
        let out = merge_model_listing(&prev, &fresh, "llm", &mut compat);
        let ids: Vec<&str> = out.keep.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"keep-custom"), "operator row preserved");
        assert!(ids.contains(&"legacy-untagged"), "untagged row preserved");
        assert!(ids.contains(&"brand-new"), "fresh row added");
        assert_eq!(
            out.dropped_imported,
            vec!["stale-api-sync".to_string(), "stale-imported".to_string()],
            "stale imported rows dropped"
        );
        assert!(
            out.preserved_custom
                .contains(&"legacy-untagged".to_string()),
            "untagged reported as preserved custom"
        );
    }

    #[test]
    fn custom_fields_overlay_fresh_metadata() {
        // OmniRoute mergeModelsWithCustomPrecedence — operator's defined
        // fields win, e.g. a hand-tuned display name survives refresh.
        let mut prev = custom("m", Some("custom"), None);
        prev.name = Some("My Tuned Name".into());
        prev.extra
            .insert("targetFormat".into(), Value::String("openai".into()));
        let fresh = vec![DiscoveredModel {
            id: "m".into(),
            name: Some("Upstream Name".into()),
            extra: BTreeMap::from([("supportsTools".to_string(), Value::Bool(true))]),
        }];
        let mut compat = CompatOverrideStore::default();
        let out = merge_model_listing(&[prev], &fresh, "llm", &mut compat);
        assert_eq!(out.keep.len(), 1);
        let kept = &out.keep[0];
        assert_eq!(kept.name.as_deref(), Some("My Tuned Name"));
        assert_eq!(
            kept.extra.get("targetFormat"),
            Some(&Value::String("openai".into()))
        );
        assert_eq!(
            kept.extra.get("supportsTools"),
            Some(&Value::Bool(true)),
            "discovered metadata for undefined fields is kept"
        );
        assert_eq!(
            kept.extra.get("source"),
            Some(&Value::String("custom".into())),
            "operator source tag retained"
        );
    }

    #[test]
    fn compat_knobs_survive_drop_and_reimport() {
        // OmniRoute preserveRemovedCustomModelCompat (:240, called at :329).
        let mut prev = custom("m", Some("imported"), None);
        prev.extra
            .insert("targetFormat".into(), Value::String("openai".into()));
        let mut compat = CompatOverrideStore::default();
        let out = merge_model_listing(&[prev], &[], "llm", &mut compat);
        assert_eq!(out.dropped_imported, vec!["m".to_string()]);
        // Re-import later restores the tuning.
        let prev2 = vec![custom("other", Some("custom"), None)];
        let out2 = merge_model_listing(&prev2, &[disc("m")], "llm", &mut compat);
        let reimported = out2.keep.iter().find(|m| m.id == "m").unwrap();
        assert_eq!(
            reimported.extra.get("targetFormat"),
            Some(&Value::String("openai".into())),
            "compat knob restored on re-import"
        );
    }
}
