//! Regression cover for bead `openproxy-2mlo` — a P0 break of the AGENTS.md
//! core-surface mirror rule between the Providers page (surface #1) and
//! `ModelSelectModal` (surface #4).
//!
//! ## Why a Rust test for a TypeScript defect
//!
//! The defect lives in the shared dashboard data path
//! (`web/src/shared/models/availableModels.ts`), and the repo has no JS test
//! runner (no vitest/jest in `web/package.json`). This file therefore executes
//! the **real** `web/src` TypeScript through a node loader: the `typescript`
//! package already vendored in `web/node_modules` transpiles each module on the
//! fly, and a small resolve hook maps the `@/` path alias plus extensionless
//! specifiers. That runs the same code the browser runs, so a failure here is a
//! real behavioural failure, not a source-text guess.
//!
//! This works on any Node >= 18 (CI pins Node 20) because it does not rely on
//! native type stripping.
//!
//! ## What is asserted
//!
//! Both surfaces read their user-visible list out of a single
//! `buildAvailableModels` result, so the builder's set contract is what keeps
//! them mirrored:
//!
//! * `enabledRows` must contain **no** disabled row, from *any* source
//!   (this is the actual P0: custom rows used to be spliced in unfiltered).
//! * `enabledRows` and `disabledRows` must partition `allRows`.
//! * `enabledCustomRows` + `enabledCoreRows` must reconstruct `enabledRows`.
//!
//! The React components cannot be rendered (no DOM test runner), so the wiring
//! that connects each surface to those fields is covered by explicit
//! source-contract assertions instead. Those are kept narrow — one per bead
//! claim, no incidental formatting checks.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

/// Resolves an ESM specifier the way the Astro/Vite build does:
///   * `@/x`            -> `web/src/x`
///   * `./x` (no ext)   -> `web/src/.../x.ts|.tsx|/index.ts|...`
///
/// Bare specifiers (`react`, `zustand`) fall through to normal node resolution
/// so they load from `web/node_modules`.
const HOOKS_MJS: &str = r#"
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createRequire } from "node:module";

// `dir` normalises to a trailing slash; the blank capture keeps a path that
// already ends in "/" from having its last real segment eaten.
const dir = (p) => p.replace(/([^/])?$/, "$1/");

const WEB_ROOT = process.env.OP_WEB_ROOT;
const SRC = pathToFileURL(dir(process.env.OP_WEB_SRC)).href;
const EXTS = [".ts", ".tsx", "/index.ts", "/index.tsx", ".js", ".jsx", "/index.js"];

const require = createRequire(pathToFileURL(dir(WEB_ROOT) + "package.json"));
const ts = require("typescript");

export async function resolve(specifier, context, next) {
  if (!specifier.startsWith("@/") && !specifier.startsWith("./") && !specifier.startsWith("../")) {
    return next(specifier, context);
  }
  let target = specifier.startsWith("@/")
    ? SRC + specifier.slice(2)
    : new URL(specifier, context.parentURL).href;
  if (!/\.[cm]?[jt]sx?$/.test(target)) {
    for (const ext of EXTS) {
      if (existsSync(fileURLToPath(target + ext))) return next(target + ext, context);
    }
  }
  return next(target, context);
}

export async function load(url, context, next) {
  if (url.startsWith(SRC) && /\.tsx?$/.test(url)) {
    const fileName = fileURLToPath(url);
    const { outputText } = ts.transpileModule(readFileSync(fileName, "utf8"), {
      fileName,
      compilerOptions: {
        module: ts.ModuleKind.ESNext,
        target: ts.ScriptTarget.ES2022,
        jsx: ts.JsxEmit.ReactJSX,
        useDefineForClassFields: false,
      },
    });
    return { format: "module", shortCircuit: true, source: outputText };
  }
  return next(url, context);
}
"#;

const LOADER_MJS: &str = r#"
import { register } from "node:module";
register(new URL("./hooks.mjs", import.meta.url));
"#;

/// Plain `.mjs` so node can run the entry directly; the TypeScript it pulls in
/// is transpiled by the loader hook. Emits a single JSON observation object.
const PROBE_MJS: &str = r#"
const out = {};
const mod = await import("@/shared/models/availableModels");

const ids = (rows) => (Array.isArray(rows) ? rows.map((r) => r.id) : null);
const strIds = (rows) => (Array.isArray(rows) ? rows.slice() : null);
const flags = (rows) =>
  Array.isArray(rows) ? rows.map((r) => ({ id: r.id, disabled: r.disabled, source: r.source })) : null;

// ── The P0 fixture ────────────────────────────────────────────────────────
// kilocode ("kc"): two catalog models and three custom/legacy rows; one custom
// row ("my-tuned-model") and one catalog row ("gpt-4o") are disabled.
const built = mod.buildAvailableModels({
  catalogModels: [
    { id: "gpt-4o", name: "GPT-4o" },
    { id: "claude-sonnet", name: "Claude Sonnet" },
  ],
  liveModels: [],
  customModels: [
    { id: "my-tuned-model", providerAlias: "kc", name: "My Tuned Model" },
    { id: "keeper-model", providerAlias: "kc", name: "Keeper Model" },
  ],
  modelAliases: { "legacy-nick": "kc/legacy-model" },
  disabledIds: ["my-tuned-model", "gpt-4o"],
  providerAlias: "kc",
  type: "llm",
  freeOnly: false,
});

out.p0 = {
  enabledRows: ids(built.enabledRows),
  customRows: flags(built.customRows),
  enabledCustomRows: ids(built.enabledCustomRows),
  enabledCoreRows: ids(built.enabledCoreRows),
  disabledCoreRows: ids(built.disabledCoreRows),
  disabledRows: ids(built.disabledRows),
  allRows: ids(built.allRows),
  allCoreIds: strIds(built.allCoreIds),
};

// ── freeOnly persistence fallback ─────────────────────────────────────────
// The modal used to read /api/providers/filters with no localStorage fallback
// while the provider page (via loadFreeOnly) had one, so a single failed GET
// desynchronised the two surfaces. loadFreeOnly must be exported and must fall
// back to localStorage when the endpoint is unreachable.
const freeOnly = { exported: typeof mod.loadFreeOnly === "function" };
if (freeOnly.exported) {
  const store = new Map();
  globalThis.localStorage = {
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => { store.set(k, v); },
  };
  globalThis.fetch = async () => { throw new Error("offline"); };

  const KEY = "openproxy:freeOnly:kc";
  store.set(KEY, "1");
  freeOnly.offlineStoredTrue = await mod.loadFreeOnly("kc");
  store.set(KEY, "0");
  freeOnly.offlineStoredFalse = await mod.loadFreeOnly("kc");
  store.delete(KEY);
  freeOnly.offlineUnset = await mod.loadFreeOnly("kc");
}
out.freeOnly = freeOnly;

console.log(JSON.stringify(out));
"#;

fn web_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web")
}

fn web_src() -> PathBuf {
    web_root().join("src")
}

/// Runs the probe once per test process and caches the observation object.
fn observations() -> &'static Value {
    static CACHE: OnceLock<Value> = OnceLock::new();
    CACHE.get_or_init(run_probe)
}

fn run_probe() -> Value {
    let web = web_root();
    let src = web_src();
    assert!(
        src.join("shared/models/availableModels.ts").is_file(),
        "expected the shared model builder at {}",
        src.join("shared/models/availableModels.ts").display()
    );

    let dir = tempfile::Builder::new()
        .prefix("openproxy-available-models-")
        .tempdir()
        .expect("create temp harness dir");
    for (name, body) in [
        ("hooks.mjs", HOOKS_MJS),
        ("loader.mjs", LOADER_MJS),
        ("probe.mjs", PROBE_MJS),
    ] {
        std::fs::write(dir.path().join(name), body).expect("write harness file");
    }

    let out = Command::new("node")
        .arg("--no-warnings")
        .arg("--no-deprecation")
        .arg("--import")
        .arg("./loader.mjs")
        .arg("./probe.mjs")
        .current_dir(dir.path())
        .env("OP_WEB_ROOT", &web)
        .env("OP_WEB_SRC", &src)
        .output()
        .expect(
            "node is required to run tests/available_models_disabled.rs (the dashboard has no JS test runner)",
        );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let json = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with('{'))
        .unwrap_or_else(|| {
            panic!(
                "node harness produced no JSON.\n--- status: {}\n--- stdout:\n{stdout}\n--- stderr:\n{stderr}",
                out.status
            )
        });
    serde_json::from_str(json)
        .unwrap_or_else(|e| panic!("probe emitted invalid JSON ({e}): {json}"))
}

fn ids(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .unwrap_or_else(|| panic!("`{key}` must be an array of ids, got {v}"))
        .iter()
        .map(|x| x.as_str().expect("id is a string").to_string())
        .collect()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

fn read_src(rel: &str) -> String {
    let path = web_src().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn assert_contains(haystack: &str, needle: &str, file: &str, why: &str) {
    assert!(
        haystack.contains(needle),
        "{file} must contain `{needle}` — {why}\n(the dashboard serves web/dist, so this must ship via `cd web && pnpm build`)"
    );
}

fn assert_not_contains(haystack: &str, needle: &str, file: &str, why: &str) {
    assert!(
        !haystack.contains(needle),
        "{file} must NOT contain `{needle}` — {why}"
    );
}

// ── Behavioural: the shared builder (both surfaces' single source of truth) ──

/// The P0 itself. A disabled custom model must leave the available list.
#[test]
fn disabled_custom_model_leaves_the_available_list() {
    let p0 = &observations()["p0"];

    // A disabled row of ANY source must be absent from the enabled set.
    assert_not_contains(
        &ids(p0, "enabledRows").join(","),
        "my-tuned-model",
        "enabledRows",
        "a disabled custom model is still selectable in the opencode picker",
    );
    assert_not_contains(
        &ids(p0, "enabledRows").join(","),
        "gpt-4o",
        "enabledRows",
        "a disabled catalog model leaked into the enabled set",
    );

    // Enabled custom rows survive, and so does a non-custom custom row.
    let enabled = ids(p0, "enabledRows");
    assert!(
        enabled.contains(&"keeper-model".to_string()),
        "an enabled custom model must stay available, got {enabled:?}"
    );
    assert!(
        enabled.contains(&"legacy-model".to_string()),
        "an enabled legacy-alias row must stay available, got {enabled:?}"
    );

    // The exact expected list, so a future regression is unambiguous.
    assert_eq!(
        sorted(ids(p0, "enabledRows")),
        sorted(vec![
            "keeper-model".into(),
            "legacy-model".into(),
            "claude-sonnet".into()
        ]),
        "enabledRows for the kc fixture"
    );
}

/// The mirror invariant: enabled + disabled must partition every row, so each
/// surface derives its list from the same complete picture.
#[test]
fn enabled_and_disabled_sets_partition_all_rows() {
    let p0 = &observations()["p0"];

    let enabled = ids(p0, "enabledRows");
    let disabled = ids(p0, "disabledRows");
    let all = ids(p0, "allRows");

    let overlap: Vec<_> = enabled
        .iter()
        .filter(|i| disabled.contains(i))
        .cloned()
        .collect();
    assert!(
        overlap.is_empty(),
        "enabledRows and disabledRows overlap on {overlap:?}"
    );

    let mut union: Vec<String> = enabled.iter().chain(disabled.iter()).cloned().collect();
    union.sort();
    union.dedup();
    assert_eq!(
        union,
        sorted(all.clone()),
        "enabledRows + disabledRows must equal allRows"
    );

    // The restore set is the combined disabled set — a lone disabled custom
    // model needs an escape hatch even when no catalog model is disabled.
    assert_eq!(
        sorted(disabled),
        sorted(vec!["my-tuned-model".into(), "gpt-4o".into()]),
        "disabledRows (drives both the 'Disabled (N)' section and the 'Active All' gate)"
    );

    // Core-only view is unchanged, so nothing else about core rows regressed.
    assert_eq!(
        sorted(ids(p0, "disabledCoreRows")),
        sorted(vec!["gpt-4o".into()])
    );
    assert_eq!(
        sorted(ids(p0, "allCoreIds")),
        sorted(vec!["gpt-4o".into(), "claude-sonnet".into()])
    );
}

/// `enabledCustomRows` + `enabledCoreRows` must reconstruct `enabledRows`.
/// This is what makes the Providers page and the modal provably identical:
/// both render those two lists, so the page cannot drift from the picker.
#[test]
fn page_and_picker_reconstruct_the_same_enabled_set() {
    let p0 = &observations()["p0"];

    let mut rebuilt = ids(p0, "enabledCustomRows");
    rebuilt.extend(ids(p0, "enabledCoreRows"));

    assert_eq!(
        sorted(rebuilt),
        sorted(ids(p0, "enabledRows")),
        "enabledCustomRows + enabledCoreRows must equal enabledRows (the mirror rule)"
    );

    // customRows keeps every custom row with its disabled flag intact, so the
    // disable action still knows what to act on — it is the display split that
    // hides them, not data loss.
    let custom: Vec<_> = p0["customRows"]
        .as_array()
        .expect("customRows array")
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_string(),
                r["disabled"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        custom.len(),
        3,
        "customRows must keep all three custom/legacy rows, got {custom:?}"
    );
    assert!(
        custom.iter().any(|(id, d)| id == "my-tuned-model" && *d),
        "the disabled custom row must keep disabled: true in customRows, got {custom:?}"
    );
}

/// `loadFreeOnly` must be exported so the modal can share the provider page's
/// persisted `freeOnly` value, localStorage fallback included.
#[test]
fn free_only_filter_is_shared_with_a_local_storage_fallback() {
    let f = &observations()["freeOnly"];
    assert_eq!(
        f["exported"],
        Value::Bool(true),
        "loadFreeOnly must be exported from availableModels.ts so the modal can reuse it"
    );
    assert_eq!(
        f["offlineStoredTrue"],
        Value::Bool(true),
        "with the endpoint down, a stored freeOnly=true must still be honoured"
    );
    assert_eq!(
        f["offlineStoredFalse"],
        Value::Bool(false),
        "with the endpoint down, a stored freeOnly=false must still be honoured"
    );
    assert_eq!(
        f["offlineUnset"],
        Value::Bool(false),
        "with the endpoint down and nothing stored, freeOnly must default to false"
    );
}

// ── Source contract: how each surface is wired to the shared fields ─────────
// No DOM runner exists, so the React wiring is asserted structurally. Each
// check corresponds to one claim in bead openproxy-2mlo.

#[test]
fn provider_page_renders_the_combined_disabled_set() {
    let page = read_src("components/providers/ProviderDetailPageClient.tsx");

    assert_contains(
        &page,
        "am.enabledCustomRows",
        "ProviderDetailPageClient.tsx",
        "the custom-model section must render only ENABLED custom rows",
    );
    assert_not_contains(
        &page,
        "am.customRows",
        "ProviderDetailPageClient.tsx",
        "rendering raw customRows puts disabled custom models back in the Available Models list",
    );
    assert_contains(
        &page,
        "am.disabledRows",
        "ProviderDetailPageClient.tsx",
        "the 'Disabled models' restore section must read the combined disabled set",
    );
    assert_not_contains(&page, "am.disabledCoreRows", "ProviderDetailPageClient.tsx",
        "the restore section and the 'Active All' gate must both read the combined disabled set, not core-only");
}

#[test]
fn model_row_offers_disable_for_custom_rows() {
    let row = read_src("components/providers/ModelRow.tsx");
    assert_not_contains(&row, "{!isCustom && onDisable && (", "ModelRow.tsx",
        "a custom row is structurally incapable of being disabled, which is how the flag became write-only");
    assert_contains(
        &row,
        "onDisable && (",
        "ModelRow.tsx",
        "the block button should render whenever an onDisable handler is supplied",
    );
}

#[test]
fn modal_mirrors_the_provider_page() {
    let modal = read_src("shared/components/ModelSelectModal.tsx");

    // The picker must offer the shared enabled set, not raw custom rows.
    assert_contains(
        &modal,
        "built.enabledRows",
        "ModelSelectModal.tsx",
        "the modal must render the shared enabled set",
    );
    assert_not_contains(
        &modal,
        "built.customRows",
        "ModelSelectModal.tsx",
        "the modal must not re-derive its list from unfiltered custom rows",
    );

    // Free-only must come from the same loader the provider page uses.
    assert_contains(&modal, "loadFreeOnly", "ModelSelectModal.tsx",
        "the modal's freeOnly must use loadFreeOnly, or a failed GET desynchronises the two surfaces");

    // Catalog readiness must be a dependency of the grouping memo.
    assert_contains(
        &modal,
        "catalogReady",
        "ModelSelectModal.tsx",
        "groupedModels must recompute when the catalog finishes loading, or the modal opens empty",
    );

    // A hidden provider must not render a phantom group.
    assert_contains(
        &modal,
        "hidden",
        "ModelSelectModal.tsx",
        "hidden:true providers (devin-cli, mimo-free) must be filtered out of the picker",
    );

    // A group may not render with zero models, and search must reach the value
    // and the provider alias.
    assert_not_contains(&modal, "|| providerNameMatches", "ModelSelectModal.tsx",
        "a provider-name-only match emits a bare header showing (0) and suppresses 'No models found'");
    assert_contains(
        &modal,
        "m.value.toLowerCase()",
        "ModelSelectModal.tsx",
        "search must match the model value",
    );
    assert_contains(
        &modal,
        "group.alias.toLowerCase()",
        "ModelSelectModal.tsx",
        "search must match the provider alias",
    );

    // The synthetic bare-alias row must only appear for a genuinely empty
    // provider, not for one whose models are all disabled.
    assert_contains(&modal, "built.allRows.length === 0", "ModelSelectModal.tsx",
        "the synthetic alias fallback must be gated on an empty provider, or a fully-disabled provider offers a row that resolves to the wrong provider");

    // Typed-kind custom rows must carry their type or filterByKind drops them.
    assert_contains(&modal, "type: r.type", "ModelSelectModal.tsx",
        "custom rows in the kindFilter branch must set `type` or every custom typed model is discarded");
}
