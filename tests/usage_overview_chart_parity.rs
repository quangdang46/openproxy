//! Regression cover for bead `openproxy-afvu` finding 8 — the Usage Overview
//! tab lost its token/cost trend chart, leaving the chart components orphaned.
//!
//! ## The defect
//!
//! `UsageChart.tsx` and `UsageChartInner.tsx` existed and were correct, but
//! nothing imported them: `grep -rn "UsageChart" web/src` found the two files
//! and no importer. In 9router the chart sits between the topology/recent-requests
//! grid and the table selector (`UsageStats.js:482-484`); in OpenProxy that slot
//! was empty, so the Overview tab had no time axis at all.
//!
//! Two things were missing, not one:
//!
//! 1. **The mount.** `UsageStats.tsx` had no `UsageChart` import and no chart
//!    slot. Its Provider-breakdown table occupied the same visual position —
//!    but that table is already rendered on the Providers tab
//!    (`UsagePageClient.tsx:98`), so it was a duplicate, not a substitute.
//! 2. **The empty state.** `UsageChartInner` had only a `loading` branch and
//!    then rendered the chart unconditionally. A period where every bucket is
//!    legitimately zero drew an empty axis, which reads as a broken chart
//!    rather than an absence of usage. 9router guards on
//!    `data.some((d) => d.tokens > 0 || d.cost > 0)` and renders "No data for
//!    this period" instead (`UsageChart.js:42-51, 72-77`).
//!
//! ## Backend coupling
//!
//! This finding deliberately does **not** touch the backend. A separate bead
//! (P248-001, openproxy-c6gg finding 21) reshapes `/api/usage/chart` to drop
//! the `{data: ...}` envelope and rename the bucket key `date` → `label`. That
//! reshape has since landed, so `UsageChartInner` now reads the bare array and
//! keys the axis on `label`; the two sides must move together, which is what
//! the last test here pins so a half-landed P248-001 is caught.
//!
//! ## Why a Rust test for a TypeScript defect
//!
//! The dashboard has no JS test runner (no vitest/jest in `web/package.json`),
//! so this follows the convention established in
//! `tests/available_models_disabled.rs` and `tests/login_autofocus_parity.rs`:
//! read the source as text and make narrow source-contract assertions.

use std::path::PathBuf;

fn web_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web")
}

fn read_src(rel: &str) -> String {
    let path = web_root().join("src").join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn assert_contains(haystack: &str, needle: &str, file: &str, why: &str) {
    assert!(
        haystack.contains(needle),
        "{file} must contain `{needle}` — {why}\n(the dashboard serves web/dist, so this must ship via `cd web && pnpm build`)"
    );
}

const STATS: &str = "web/src/shared/components/UsageStats.tsx";
const CHART_INNER: &str = "web/src/components/usage/UsageChartInner.tsx";

/// The chart has to be mounted in 9router's slot, and the Overview tab must not
/// carry a second copy of the provider breakdown that the Providers tab
/// already owns.
#[test]
fn usage_overview_renders_the_token_cost_chart() {
    let src = read_src("shared/components/UsageStats.tsx");

    assert_contains(
        &src,
        "import UsageChart from \"@/components/usage/UsageChart\";",
        STATS,
        "the chart is lazy-loaded, so the import is what pulls it in at all",
    );
    assert_contains(
        &src,
        "{loading ? spinner : <UsageChart period={period} />}",
        STATS,
        "9router UsageStats.js:482-484 renders the chart in the same loading \
         shape as every other panel, and passes the synced period so both read \
         the same window",
    );
}

/// The original failure mode was orphan code: the components shipped, nothing
/// imported them, and nothing noticed.
#[test]
fn chart_component_is_no_longer_dead_code() {
    let src = read_src("shared/components/UsageStats.tsx");

    assert_contains(
        &src,
        "UsageChart",
        STATS,
        "if UsageStats stops referencing UsageChart the two chart files become \
         orphans again — the exact pre-fix state",
    );
}

/// Zero across every bucket is a real answer, not a broken chart.
#[test]
fn chart_shows_the_no_data_state() {
    let src = read_src("components/usage/UsageChartInner.tsx");

    assert_contains(
        &src,
        "const hasData = data.some(",
        CHART_INNER,
        "9router UsageChart.js:42 guards on `data.some((d) => d.tokens > 0 || \
         d.cost > 0)`; without it an all-zero period draws an empty axis",
    );
    assert_contains(
        &src,
        "No data for this period",
        CHART_INNER,
        "the message the user sees instead of the empty axis (9router \
         UsageChart.js:72-77)",
    );
    assert_contains(
        &src,
        "if (!hasData) {",
        CHART_INNER,
        "the guard has to actually short-circuit the render, not just compute a \
         boolean",
    );
}

/// Pins the chart's coupling to the backend shape so a reshape on either side
/// has to change both in the same commit rather than shipping a silently blank
/// chart.
#[test]
fn chart_reads_the_bare_backend_envelope() {
    let src = read_src("components/usage/UsageChartInner.tsx");

    assert_contains(
        &src,
        "Array.isArray(result) ? result : []",
        CHART_INNER,
        "/api/usage/chart answers a bare `[{...}]` — an envelope lookup here \
         would silently render an empty chart",
    );
    assert_contains(
        &src,
        "dataKey=\"label\"",
        CHART_INNER,
        "the bucket key is `label`; `date` would leave the X axis blank",
    );
}
