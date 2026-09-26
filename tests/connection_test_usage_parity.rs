//! Bead openproxy-c6gg (wave w10) — the dashboard half of the connection-probe
//! and usage-accounting findings.
//!
//! The Rust halves (N1/N3/N7/N9 in `provider_connection_test.rs`, N22 in
//! `providers.rs`) carry their own `#[cfg(test)]` blocks, which is where CI
//! actually runs them — `ci.yml` invokes `cargo test --lib`. What lives here is
//! the web half: N16 (overview cards), N17 (`?tab=` contract) and N26
//! (ProviderTopology controls), none of which the dashboard can cover without a
//! JS runner.
//!
//! ## Why a Rust test for TypeScript defects
//!
//! `web/package.json` has no test script and no vitest/jest dependency, so this
//! follows the convention in `tests/dashboard_pages_parity.rs` and
//! `tests/caveman_levels_parity.rs`: read the source and assert narrowly on
//! what was *parsed out of it* — the label literals, the tab allowlist, the
//! prop values — rather than grepping for incidental formatting.
//!
//! 9router paths are relative to `.tmp/9router` (git 17c4cc76).

use std::path::PathBuf;

fn web_src(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("web/src")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every `label: "…"` literal in a source file, in order.
fn string_props(source: &str, prop: &str) -> Vec<String> {
    let needle = format!("{prop}: \"");
    let mut found = Vec::new();
    let mut cursor = 0;
    while let Some(at) = source[cursor..].find(&needle) {
        let start = cursor + at + needle.len();
        let end = source[start..]
            .find('"')
            .unwrap_or_else(|| panic!("unterminated {prop} literal in:\n{source}"));
        found.push(source[start..start + end].to_string());
        cursor = start + end;
    }
    found
}

// ---------------------------------------------------------------------------
// Finding 16 — the overview row must carry 9router's five metrics.
// ---------------------------------------------------------------------------

/// 9router's OverviewCards renders Total Requests, Total Input Tokens, Cached
/// Tokens, Output Tokens and Est. Cost (UsageStats/page.js). OpenProxy shipped
/// three cards, demoted requests to an 11px sub-caption, and never read the
/// `totalCost` the backend already sends — so cached-token share and spend were
/// both unreadable on the page.
#[test]
fn overview_cards_carry_all_five_of_9routers_metrics() {
    let src = web_src("components/usage/OverviewCards.tsx");
    let labels = string_props(&src, "label");

    for expected in [
        "Total Requests",
        "Total Input Tokens",
        "Cached Tokens",
        "Output Tokens",
        "Est. Cost",
    ] {
        assert!(
            labels.iter().any(|label| label == expected),
            "the overview is missing the `{expected}` card; found {labels:?}"
        );
    }

    // The cost card has to read the number the backend ships, not re-derive it.
    assert!(
        src.contains("totalCost"),
        "Est. Cost must render `stats.totalCost`"
    );
    assert!(
        src.contains("totalCachedTokens"),
        "the cached card must render `stats.totalCachedTokens`"
    );
    assert!(
        src.contains("Estimated, not actual billing"),
        "9router captions the cost card so nobody reads it as an invoice"
    );
    // Five cards need the fifth column tier, or they wrap to a second row.
    assert!(
        src.contains("lg:grid-cols-5"),
        "the grid must open a fifth column at lg, as 9router does"
    );
}

// ---------------------------------------------------------------------------
// Finding 17 — `?tab=` is a three-value contract.
// ---------------------------------------------------------------------------

/// 9router allowlists `["overview", "logs", "details"]` and offers Overview and
/// Details in the control (dashboard/usage/page.js:31-33, :46-53). OpenProxy
/// accepted six values, so a `?tab=providers` deep link resolved to a
/// different page here than in 9router.
#[test]
fn usage_tab_allowlist_matches_9router() {
    let src = web_src("components/usage/UsagePageClient.tsx");

    let at = src
        .find("USAGE_TABS = [")
        .unwrap_or_else(|| panic!("UsagePageClient must export a USAGE_TABS allowlist:\n{src}"));
    let rest = &src[at + "USAGE_TABS = [".len()..];
    let end = rest.find(']').expect("unterminated USAGE_TABS array");
    let entries: Vec<&str> = rest[..end]
        .split(',')
        .map(|entry| entry.trim().trim_matches('"'))
        .filter(|entry| !entry.is_empty())
        .collect();

    assert_eq!(
        entries,
        ["overview", "logs", "details"],
        "9router's page.js:31-33 allowlists exactly these three"
    );

    // The control offers two of them; `logs` stays deep-link-only.
    let options_at = src
        .find("options={[\n            { value: \"overview\"")
        .unwrap_or_else(|| panic!("the SegmentedControl options must start with overview:\n{src}"));
    let options = &src[options_at..];
    let options_end = options.find("]}").expect("unterminated options array");
    let offered = string_props(&options[..options_end], "value");

    assert_eq!(
        offered,
        ["overview", "details"],
        "9router's page.js:46-53 offers exactly these two"
    );

    // The two extra views that fetch their own data stay on the overview
    // instead of becoming unreachable. ProviderBreakdownTable is fed by
    // `byProvider`, which only UsageStats holds, so from this page it could
    // only ever have rendered its empty state.
    for component in ["UsageAnalyticsGrid", "CompressionStats"] {
        assert!(
            src.contains(component),
            "{component} must stay rendered — dropping it loses a working view"
        );
    }
    assert!(
        !src.contains("import ProviderBreakdownTable") && !src.contains("<ProviderBreakdownTable"),
        "ProviderBreakdownTable needs a `byProvider` prop this page cannot supply"
    );
    for retired in [
        "activeTab === \"providers\"",
        "activeTab === \"analytics\"",
        "activeTab === \"compression\"",
    ] {
        assert!(
            !src.contains(retired),
            "`{retired}` can never fire now that the allowlist is three values"
        );
    }
}

// ---------------------------------------------------------------------------
// Finding 26 — ProviderTopology: stuck-provider timeout, re-fit, pan/zoom.
// ---------------------------------------------------------------------------

/// 9router ages a provider out of the active set after 60 s and re-fits the
/// view on resize and on node-count change (ProviderTopology.js:371-438). This
/// port had none of that and had every pan/zoom prop pinned to false with no
/// `<Controls>`, so a stuck request record pulsed forever and a large provider
/// list was unviewable.
#[test]
fn provider_topology_keeps_its_controls_and_staleness_cutoff() {
    let src = web_src("components/usage/ProviderTopology.tsx");

    assert!(
        src.contains("FE_ACTIVE_TIMEOUT_MS = 60000"),
        "the 60 s stuck-provider cut-off is missing"
    );
    assert!(
        src.contains("FE_ACTIVE_TICK_MS = 1000"),
        "the 1 s tick that drives the cut-off is missing"
    );
    assert!(
        src.contains("activeSetWithTimeout"),
        "the staleness filter must be a named, testable predicate"
    );
    assert!(
        src.contains("new ResizeObserver("),
        "the canvas must re-fit when its container resizes"
    );
    assert!(
        src.contains("<Controls"),
        "9router renders a zoom/fit control; this port lost it"
    );
    assert!(
        src.contains("minZoom={0.1}") && src.contains("maxZoom={2}"),
        "9router clamps the viewport to 0.1..2"
    );

    for prop in [
        "panOnDrag={false}",
        "zoomOnScroll={false}",
        "zoomOnPinch={false}",
        "zoomOnDoubleClick={false}",
    ] {
        assert!(
            !src.contains(prop),
            "`{prop}` pins the graph still — 9router leaves pan and zoom on"
        );
    }
}

/// The staleness rule itself: 9router drops a provider first seen more than
/// `FE_ACTIVE_TIMEOUT_MS` ago, keeps a recent one, and never shows a provider
/// that is not in the raw set even if its first sighting is still recorded.
#[test]
fn active_set_timeout_drops_only_stale_entries() {
    let rule = web_src("components/usage/ProviderTopology.tsx");
    assert!(
        rule.contains("now - ts < FE_ACTIVE_TIMEOUT_MS"),
        "the predicate must compare age against the timeout"
    );
    assert!(
        rule.contains("if (ts === undefined || now - ts < FE_ACTIVE_TIMEOUT_MS)"),
        "an unrecorded provider has no age yet and must be kept"
    );
}

// ---------------------------------------------------------------------------
// Finding 1 — the ollama arm must stay off the proxied request path.
// ---------------------------------------------------------------------------

/// 9router's ollama arm (testUtils.js:691-693) is the one API-key probe that
/// calls bare `fetch` rather than `fetchWithConnectionProxy`, so it inherits
/// neither the connection proxy / Vercel relay nor the 15 s abort signal the
/// wrapper adds.
///
/// This is a *routing* claim, not a payload one, so it is pinned structurally:
/// `provider_connection_test.rs` is a different file from this test, and the
/// only way to restore the old behaviour is to put the arm back on the shared
/// helper. The payload itself is covered by the `#[cfg(test)]` block in that
/// module, where `ollama_probe_request` can be called directly.
#[test]
fn the_ollama_arm_never_reaches_the_proxied_request_path() {
    let src = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/server/api/provider_connection_test.rs"),
    )
    .expect("read provider_connection_test.rs");

    assert!(
        src.contains("\"ollama\" => test_ollama_connection(connection).await"),
        "the ollama arm must call its own probe, not `simple_get_bearer_test`"
    );
    assert!(
        !src.contains("\"ollama\" => {\n            simple_get_bearer_test("),
        "the ollama arm regressed onto the proxied helper"
    );

    let start = src
        .find("async fn test_ollama_connection(")
        .expect("test_ollama_connection must exist");
    let body = &src[start..];
    let end = body
        .find("\nfn ollama_probe_client(")
        .expect("test_ollama_connection must be a top-level fn");
    let body = &body[..end];

    for forbidden in [
        "execute_request",
        "execute_prepared_request",
        "DEFAULT_TIMEOUT",
        "effective_proxy",
        "resolve_effective_proxy",
    ] {
        assert!(
            !body.contains(forbidden),
            "test_ollama_connection must not touch `{forbidden}` — bare `fetch` takes neither the proxy nor the timeout"
        );
    }
}
