//! Regression cover for bead `openproxy-afvu` findings 5-7 — the quota row's
//! icon-only action buttons.
//!
//! ## The defect
//!
//! Each connection row ends in four icon-only buttons: auto-ping, refresh,
//! edit, delete. Three of them were rendered as bare `<button title="...">` with
//! a `p-1.5` padding box, while their immediate neighbours in the *same row* —
//! the Codex reset-credit and Codex expiry buttons — carried the shared
//! `<Tooltip>` and a fixed `h-8 w-8` box. The inconsistency was an oversight, not
//! a style choice.
//!
//! It has two halves worth separating:
//!
//! * **Announcement (finding 7).** `title` is a weak accessible-name source: it
//!   is not announced by several screen-reader/browser pairs, it is suppressed
//!   while the user is typing, and most browsers do not show it on keyboard
//!   focus. An AT user heard four indistinguishable "button" entries per
//!   connection. The two Codex buttons already had `aria-label`, which is what
//!   made the omission visible as an inconsistency *within the row*.
//! * **Affordance (finding 5).** A `p-1.5` icon button is a ~30px target that
//!   shrinks the moment a glyph is missing, and `title` is the only hover
//!   affordance — so sighted mouse users got a tooltip from the browser default
//!   that looks nothing like the rest of the dashboard.
//!
//! 9router is the spec for both: `index.js:1183-1231` wraps all four in
//! `<Tooltip text=...>` and gives each an `aria-label` plus a fixed
//! `flex h-8 w-8` box.
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

const LIMITS: &str = "web/src/components/usage/ProviderLimits/index.tsx";

fn provider_limits_src() -> String {
    read_src("components/usage/ProviderLimits/index.tsx")
}

/// Every row action gets the shared tooltip rather than the browser's default
/// one, so hovering reads the same everywhere on the page.
#[test]
fn action_buttons_use_the_shared_tooltip_not_native_title() {
    let src = provider_limits_src();

    for (tooltip, why) in [
        (
            "<Tooltip text=\"Refresh quota\">",
            "9router index.js:1194-1205",
        ),
        (
            "<Tooltip text=\"Edit connection\">",
            "9router index.js:1206-1217",
        ),
        (
            "<Tooltip text=\"Delete connection\">",
            "9router index.js:1218-1231",
        ),
    ] {
        assert_contains(
            &src,
            tooltip,
            LIMITS,
            &format!("{why} wraps the button in the shared Tooltip"),
        );
    }

    assert_not_contains(
        &src,
        "title=\"Refresh quota\"",
        LIMITS,
        "9router carries `aria-label` + Tooltip, never `title`, on this button; \
         the pre-fix source had the title and no Tooltip",
    );
    assert_not_contains(
        &src,
        "title=\"Edit connection\"",
        LIMITS,
        "same, for the edit button",
    );
    // `title="Delete connection"` deliberately still exists — it is the
    // `ConfirmModal`'s dialog heading at the bottom of the file, not an
    // attribute on the row's delete button. The button is pinned positively
    // below instead.
    assert_contains(
        &src,
        concat!(
            "<Tooltip text=\"Delete connection\">\n",
            "                      <button\n",
            "                        type=\"button\"\n",
            "                        onClick={() => handleDeleteConnection(conn.id)}\n",
            "                        disabled={rowBusy}\n",
            "                        aria-label=\"Delete connection\"\n",
        ),
        LIMITS,
        "the delete button inside its Tooltip carries an aria-label and no \
         `title`; the contiguous block proves no title sits between the opening \
         tag and the label. (`title=\"Delete connection\"` elsewhere in this file \
         is the ConfirmModal's dialog title, which is a different thing.)",
    );
}

/// The fixed box is a design-system value in this very row — the Codex buttons
/// and the auto-ping button already use `flex h-8 w-8` — so the three outliers
/// were padding-derived and shrank whenever the glyph was missing.
#[test]
fn action_buttons_are_fixed_32px_boxes() {
    let src = provider_limits_src();

    assert_contains(
        &src,
        "className=\"flex h-8 w-8 items-center justify-center rounded-lg hover:bg-black/5 dark:hover:bg-white/5 transition-colors disabled:opacity-50\"",
        LIMITS,
        "the refresh button's box (9router index.js:1195-1202) — the other two \
         keep the same box with their own hover/colour tokens",
    );
    assert_not_contains(
        &src,
        "className=\"p-1.5 rounded-lg",
        LIMITS,
        "a padding-sized icon button has no stable hit area; every sibling in the \
         row is a fixed h-8 w-8 box",
    );
}

/// A toggle that changes the ordering but never announces its state is a
/// toggle only for people who can see the amber highlight.
#[test]
fn expiring_first_toggle_exposes_its_pressed_state() {
    let src = provider_limits_src();

    assert_contains(
        &src,
        "onClick={() => setExpiringFirst((prev) => !prev)}\n            aria-pressed={expiringFirst}",
        LIMITS,
        "9router index.js:958-962 puts aria-pressed immediately after onClick; \
         the adjacency is asserted so the attribute cannot land on a different \
         toggle",
    );
    assert_contains(
        &src,
        "title=\"Sort accounts by earliest quota reset time\"",
        LIMITS,
        "9router keeps the title alongside aria-pressed — the finding is about \
         the announcement, not about removing the hover text",
    );
}

/// All four row actions must be nameable, and the two that already were must
/// stay that way — a fix that strips `aria-label`s wholesale is a regression.
#[test]
fn row_action_buttons_have_accessible_names() {
    let src = provider_limits_src();

    for (label, why) in [
        (
            "aria-label=\"Toggle auto-ping\"",
            "9router index.js:1187 — the string is 'Toggle auto-ping', not the \
             longer pre-fix title 'Toggle auto-ping warmup'",
        ),
        ("aria-label=\"Refresh quota\"", "9router index.js:1199"),
        ("aria-label=\"Edit connection\"", "9router index.js:1211"),
        ("aria-label=\"Delete connection\"", "9router index.js:1223"),
        (
            "aria-label=\"View Codex reset credit expiry\"",
            "this one was already correct (pre-fix :1160) and is pinned so a \
             blanket aria-label removal is caught",
        ),
    ] {
        assert_contains(&src, label, LIMITS, why);
    }

    assert_not_contains(
        &src,
        "title=\"Toggle auto-ping warmup\"",
        LIMITS,
        "9router has no title on the auto-ping button — the per-provider \
         Tooltip at the top of the row is the affordance, and the button takes \
         its accessible name from the aria-label",
    );
}
