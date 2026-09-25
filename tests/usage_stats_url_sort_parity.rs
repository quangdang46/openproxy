//! Regression cover for bead `openproxy-afvu` finding 9 — usage-table sorting
//! was component-local and therefore not shareable, not reloadable and not
//! navigable.
//!
//! ## The defect
//!
//! `sortBy`/`sortOrder` were plain `useState` in `UsageStats.tsx`:
//!
//! ```tsx
//! const [sortBy, setSortBy] = useState("rawModel");
//! const [sortOrder, setSortOrder] = useState("asc");
//! ```
//!
//! with a `toggleSort` that was a bare `setState` pair — no `history`, no query
//! string. 9router treats the URL as the single source of truth for table
//! sorting (`UsageStats.js:204-208` reads it, `:309-317` writes it with
//! `router.replace`), so a reload, a shared link or a back-navigation all
//! preserve the ordering. OpenProxy's did not.
//!
//! Astro has no `useSearchParams`/`useRouter`, so the port is the browser
//! primitives: read `window.location.search`, write with
//! `history.replaceState`, and re-sync on `popstate`. The usage page already
//! owns the query string for `?tab=`, so it hands down a `router` shim with
//! both `push` and `replace`.
//!
//! The subtle part, and the reason a fresh `URLSearchParams()` would be a bug:
//! the toggle must **copy** the live query string. Building an empty one would
//! drop `?tab=providers` every time the user re-sorted a column.
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

fn assert_not_contains(haystack: &str, needle: &str, file: &str, why: &str) {
    assert!(
        !haystack.contains(needle),
        "{file} must NOT contain `{needle}` — {why}"
    );
}

const STATS: &str = "web/src/shared/components/UsageStats.tsx";
const PAGE: &str = "web/src/components/usage/UsagePageClient.tsx";

/// Sorting is a URL concern on both ends: seeded from the query string so a
/// shared link or a reload restores the ordering, and written back so the link
/// is worth sharing.
#[test]
fn sort_state_is_read_from_and_written_to_the_url() {
    let src = read_src("shared/components/UsageStats.tsx");

    assert_contains(
        &src,
        ".get(\"sortBy\") || \"rawModel\"",
        STATS,
        "9router UsageStats.js:206 seeds the sort from `searchParams.get(\"sortBy\")` \
         with `rawModel` as the default",
    );
    assert_contains(
        &src,
        ".get(\"sortOrder\") || \"asc\"",
        STATS,
        "9router UsageStats.js:207, same for the direction",
    );
    assert_contains(
        &src,
        "params.set(\"sortOrder\", params.get(\"sortOrder\") === \"asc\" ? \"desc\" : \"asc\")",
        STATS,
        "re-clicking the sorted column flips the direction (9router \
         UsageStats.js:310-311); a new column resets it to asc (:314-315)",
    );
    assert_contains(
        &src,
        "window.history.replaceState(",
        STATS,
        "the write itself — 9router's `router.replace(..., { scroll: false })` \
         (UsageStats.js:317) is exactly this browser primitive, and the default \
         shim in this file supplies it when the page does not",
    );
    assert_contains(
        &src,
        "\"popstate\"",
        STATS,
        "back/forward navigation and a second tab's change have to re-sync the \
         two values, mirroring UsagePageClient's shim (9router re-reads \
         `useSearchParams`, which is reactive to the same events)",
    );
    assert_not_contains(
        &src,
        "const [sortBy, setSortBy] = useState(\"rawModel\");",
        STATS,
        "component-local sort state is the pre-fix defect — the ordering dies on \
         reload and cannot be linked to",
    );
}

/// The page owns the query string (`?tab=`), so it has to expose a `replace` for
/// the child to write through. Without it, `toggleSort` would have to reach for
/// `window.history` behind the page's back.
#[test]
fn page_router_exposes_replace() {
    let src = read_src("components/usage/UsagePageClient.tsx");

    assert_contains(
        &src,
        "replace: (url: string) => {",
        PAGE,
        "9router's `router.replace` is what writes the sort params; the Astro \
         shim only had `push` for tab switches",
    );
    assert_contains(
        &src,
        "router={router}",
        PAGE,
        "the shim is only useful if it is handed to the child that writes \
         `?sortBy=`/`?sortOrder=`",
    );
}

/// The copy is the whole point. A fresh `new URLSearchParams()` would be
/// shorter and would silently drop `?tab=providers` on every re-sort.
#[test]
fn sort_toggle_preserves_the_tab_param() {
    let src = read_src("shared/components/UsageStats.tsx");

    assert_contains(
        &src,
        "const params = new URLSearchParams(window.location.search);",
        STATS,
        "9router UsageStats.js:310 copies `searchParams.toString()` precisely so \
         `?tab=` survives a re-sort",
    );
    assert_not_contains(
        &src,
        "new URLSearchParams();",
        STATS,
        "an empty URLSearchParams in the sort path would wipe every other query \
         param on the page, including the active tab",
    );
}
