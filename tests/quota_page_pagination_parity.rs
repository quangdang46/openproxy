//! Regression cover for bead `openproxy-afvu` findings 2-4 — the quota page lost
//! its account-status filter, its page-size control, its pagination footer and
//! its filter-aware empty states.
//!
//! ## The defect
//!
//! `web/src/components/usage/ProviderLimits/index.tsx` fetched *every* connection
//! with a bare `await fetch("/api/providers")` and rendered them unbounded, even
//! though the helpers that support all of this already existed and were exported
//! from `./utils` with zero call sites. The server side was never the problem:
//! `list_providers_api` (`src/server/api/mod.rs:985-1090`) already reads
//! `provider`/`account_status`/`sort`/`page`/`page_size` and answers with
//! 9router's exact envelope
//!
//! ```json
//! { "connections": [...], "providerOptions": [...],
//!   "pagination": { "page": 1, "pageSize": 20, "total": 57, "totalPages": 3 },
//!   "totals": { "eligibleConnections": 57, "providerFilteredConnections": 4 } }
//! ```
//!
//! so this was a pure frontend gap.
//!
//! Three user-visible consequences, all fixed together because they share one
//! state block and one fetch call:
//!
//! 1. **No paging.** Every account loaded on every render, and narrowing to a
//!    provider that has no rows left the page claiming no providers were
//!    connected at all — the single hardcoded empty state ignored the filters.
//! 2. **No Active / Turned-off filter**, which is how you find the connection
//!    you deliberately disabled.
//! 3. **No page-size control**, so there was no way to trade rows for screen
//!    space.
//!
//! 9router is the spec for all three: `index.js:923-939` (status filter),
//! `index.js:1322-1440` (page-size select + custom input + First/Prev/Next/Last)
//! and `index.js:770-818` with `utils.js:124-146` (the three-branch empty state).
//!
//! ## Why a Rust test for a TypeScript defect
//!
//! The dashboard has no JS test runner (no vitest/jest in `web/package.json`),
//! so this follows the convention established in
//! `tests/available_models_disabled.rs` and `tests/login_autofocus_parity.rs`:
//! read the source as text and make narrow source-contract assertions.
//! Deliberately narrow — no incidental formatting checks.

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

fn assert_not_contains(haystack: &str, needle: &str, file: &str, why: &str) {
    assert!(
        !haystack.contains(needle),
        "{file} must NOT contain `{needle}` — {why}"
    );
}

const LIMITS: &str = "web/src/components/usage/ProviderLimits/index.tsx";

fn provider_limits_src() -> String {
    read_src("components/usage/ProviderLimits/index.tsx")
}

// --- finding 2, render half: the filter and pagination controls -------------

/// The Active / Turned-off / All select. Without it there is no way to surface a
/// connection the user turned off — the whole point of the `isActive` toggle
/// sitting in each row.
#[test]
fn account_status_filter_select_is_rendered() {
    let src = provider_limits_src();

    assert_contains(
        &src,
        "aria-label=\"Filter accounts by status\"",
        LIMITS,
        "9router renders a status select bound to `accountFilter` \
         (index.js:923-939); without it a disabled connection is unreachable",
    );
    assert_contains(
        &src,
        "ACCOUNT_FILTER_OPTIONS.map(",
        LIMITS,
        "the select has to enumerate `ACCOUNT_FILTER_OPTIONS` (utils.ts:13) — \
         a hand-rolled list would drift from the shared constant",
    );
    assert_contains(
        &src,
        "shouldResetPage(accountFilter, nextValue)",
        LIMITS,
        "changing the status filter resets to page 1, otherwise a narrowed \
         filter can leave the user on a page that no longer exists \
         (9router index.js:926-930)",
    );
}

/// The rows-per-page select plus the free-form number box. 9router commits the
/// custom value on blur *and* on Enter, clamped to `[1, ACCOUNT_PAGE_SIZE_MAX]`;
/// a half-ported input silently ignores typed values.
#[test]
fn page_size_select_and_custom_input_are_rendered() {
    let src = provider_limits_src();

    assert_contains(
        &src,
        "aria-label=\"Accounts per page\"",
        LIMITS,
        "9router's page-size select (index.js:1327-1345) is the only control \
         for how many rows a page holds",
    );
    assert_contains(
        &src,
        "aria-label=\"Custom accounts per page\"",
        LIMITS,
        "the custom box sits beside the select and drives the same state \
         (9router index.js:1346-1360)",
    );
    assert_contains(
        &src,
        "ACCOUNT_PAGE_SIZE_MAX",
        LIMITS,
        "the custom value is clamped to `ACCOUNT_PAGE_SIZE_MAX` (utils.ts:12), \
         mirroring 9router's Math.min/Math.max on blur and on Enter",
    );
    assert_contains(
        &src,
        "if (event.key !== \"Enter\") return;",
        LIMITS,
        "Enter commits the custom page size on blur-equivalent footing; without \
         it a keyboard user cannot apply a typed value without leaving the field",
    );
}

/// First / Prev / Next / Last. `title` alone leaves an AT user with four
/// indistinguishable "button" entries at the bottom of every page.
#[test]
fn prev_and_next_page_buttons_are_rendered() {
    let src = provider_limits_src();

    assert_contains(
        &src,
        "aria-label=\"Previous accounts page\"",
        LIMITS,
        "9router names the direction buttons (index.js:1404, 1423) so they are \
         not four anonymous buttons",
    );
    assert_contains(
        &src,
        "aria-label=\"Next accounts page\"",
        LIMITS,
        "same, for the forward direction",
    );
}

// --- finding 3, fetch/state half -------------------------------------------

/// The page must ask the server for one page of connections. The bare
/// `fetch("/api/providers")` is the pre-fix shape: no params, no envelope.
#[test]
fn fetch_connections_requests_the_paginated_endpoint() {
    let src = provider_limits_src();

    assert_contains(
        &src,
        "page: String(",
        LIMITS,
        "the server contract is `GET /api/providers?page=&pageSize=&accountStatus=&sort=` \
         (src/server/api/mod.rs:985-1090, mirroring 9router's client route)",
    );
    assert_contains(
        &src,
        "pageSize: String(",
        LIMITS,
        "without a page size the server falls back to its own default and the \
         client's footer would disagree with what it actually received",
    );
    assert_contains(
        &src,
        "accountStatus: accountFilter",
        LIMITS,
        "the status filter is applied server-side, not by slicing the page in \
         the browser",
    );
    assert_contains(
        &src,
        "sort: \"priority\"",
        LIMITS,
        "9router sends `sort: \"priority\"` (index.js:182) so the server orders \
         rows before slicing",
    );
    assert_contains(
        &src,
        "params.set(\"provider\", providerFilter)",
        LIMITS,
        "the provider filter is omitted when it is `all` and set otherwise, so \
         the server can report `providerOptions` across every eligible \
         connection rather than only the current page",
    );
    assert_contains(
        &src,
        "/api/providers?${params",
        LIMITS,
        "the request must carry the query string built above",
    );
    assert_not_contains(
        &src,
        "await fetch(\"/api/providers\")",
        LIMITS,
        "the pre-fix fetch loaded every connection on every render — the exact \
         unbounded behaviour the server already paginates away",
    );
}

/// The response envelope is the source of truth for the footer and the
/// provider dropdown. Re-deriving either from the current page is the failure
/// mode: the dropdown would only ever list the providers visible right now.
#[test]
fn pagination_totals_and_provider_options_come_from_the_response() {
    let src = provider_limits_src();

    assert_contains(
        &src,
        "setPagination(getSafePagination(",
        LIMITS,
        "`getSafePagination` (utils.ts:164) clamps a malformed envelope; the \
         footer must read the server's paging, not guess it",
    );
    assert_contains(
        &src,
        "setTotals(getSafeTotals(",
        LIMITS,
        "`totals` drives the three-branch empty state — without it the page \
         cannot tell 'no providers' from 'no matches'",
    );
    assert_contains(
        &src,
        "getPaginationPageValue(data.pagination,",
        LIMITS,
        "the server clamps a page past the end back to the last real page, so a \
         delete moves the user without a click (utils.ts:190)",
    );
    assert_contains(
        &src,
        "setProviderOptions(getProviderOptions(data.providerOptions))",
        LIMITS,
        "9router takes the dropdown options from the response (index.js:190) so \
         they span every eligible connection, not just the current page",
    );
    assert_not_contains(
        &src,
        "Array.from(new Set(filteredConnections.map((conn) => conn.provider)))",
        LIMITS,
        "deriving the provider list from the loaded page reduces the dropdown to \
         the providers currently on screen, so filtering to a provider you just \
         left makes it impossible to filter back",
    );
}

/// Paging is only meaningful if the refresh loops stay on the page the user is
/// looking at.
#[test]
fn refresh_loop_targets_the_current_page() {
    let src = provider_limits_src();

    assert_contains(
        &src,
        "await fetchConnections(page)",
        LIMITS,
        "`refreshAll` and the mount effect must refetch the page the user is on; \
         an argument-less call silently yanks them back to page 1 on every tick",
    );
    assert_contains(
        &src,
        "}, [refreshingAll, fetchConnections, fetchQuota, page]);",
        LIMITS,
        "`page` is a dependency of `refreshAll` — without it the 60s interval \
         keeps calling a closure frozen on page 1",
    );
}

// --- finding 4: the three empty states ------------------------------------

/// "No providers connected", "no accounts match" and "nothing on this page" are
/// three different problems with three different fixes. Collapsing them into one
/// hardcoded block tells a user with 57 connections and a narrow filter that
/// they have none.
#[test]
fn three_empty_states_are_driven_by_get_connections_empty_message() {
    let src = provider_limits_src();

    assert_contains(
        &src,
        "getConnectionsEmptyMessage(totals, providerFilter, accountFilter)",
        LIMITS,
        "the three-branch helper already exists at utils.ts:124-153 and mirrors \
         9router utils.js:124-146; re-deriving it inline would drift",
    );
    assert_contains(
        &src,
        "const hasEligibleConnections = totals.eligibleConnections > 0;",
        LIMITS,
        "9router names this guard (index.js:770); it is what separates 'no \
         providers at all' from 'the filters match nothing'",
    );
    assert_contains(
        &src,
        "const hasVisibleConnections = sortedConnections.length > 0;",
        LIMITS,
        "the companion guard (index.js:771) — an empty *page* is a third case, \
         not the same as an empty result set",
    );
    assert_contains(
        &src,
        "{emptyState.icon}",
        LIMITS,
        "the icon is data-driven so the filter states are not hardcoded",
    );
    assert_contains(&src, "{emptyState.title}", LIMITS, "same, for the title");
    assert_contains(
        &src,
        "{emptyState.description}",
        LIMITS,
        "same, for the body — this is where 9router names the active filters",
    );
    assert!(
        src.matches("if (!connectionsLoading").count() >= 2,
        "{LIMITS} must keep TWO distinct `if (!connectionsLoading` guards — the \
         pre-fix source had exactly one (`... && sortedConnections.length === 0`), \
         which is why the filter-mismatch and out-of-page states were unreachable"
    );
}

// --- the helpers must never go dead again ---------------------------------

/// Every helper these findings depend on lives in `./utils` and is used nowhere
/// else. A source-grep test is the only thing standing between the port and a
/// silent re-regression.
#[test]
fn pagination_and_empty_state_helpers_are_imported_and_used() {
    let src = provider_limits_src();

    for (helper, why) in [
        ("ACCOUNT_FILTER_OPTIONS", "drives the status filter select"),
        ("ACCOUNT_PAGE_SIZE_OPTIONS", "drives the page-size select"),
        ("ACCOUNT_PAGE_SIZE_MAX", "clamps the custom page-size input"),
        (
            "CONNECTIONS_PAGE_SIZE",
            "seeds `pageSize` and the initial pagination",
        ),
        ("shouldResetPage", "resets to page 1 when a filter changes"),
        ("getPageSizeLabel", "labels the custom page-size chip"),
        (
            "getConnectionsPaginationSummary",
            "renders the footer's summary span",
        ),
        ("getSafePagination", "clamps the server's paging envelope"),
        ("getSafeTotals", "clamps the server's totals envelope"),
        (
            "getPaginationPageValue",
            "adopts the server's clamped page number",
        ),
        (
            "getProviderOptions",
            "sanitises the server's providerOptions list",
        ),
        (
            "getConnectionsEmptyMessage",
            "selects one of the three empty states",
        ),
    ] {
        assert_contains(
            &src,
            &format!("\n  {helper},"),
            LIMITS,
            &format!("`{helper}` is exported from ./utils for this page alone — {why}"),
        );
    }
}
