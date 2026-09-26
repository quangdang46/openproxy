//! Source-contract cover for bead `openproxy-mv0w.4` — the four shared
//! design-token components (Card, Input, SegmentedControl, Badge) against
//! 9router's `.tmp/9router/src/shared/components/*`.
//!
//! ## Why a Rust test for TypeScript defects
//!
//! The dashboard has no JS test runner (no vitest/jest in `web/package.json`),
//! so this follows the convention established in
//! `tests/dashboard_chrome_parity.rs` and `tests/login_autofocus_parity.rs`:
//! read the source as text and make narrow source-contract assertions.
//! Deliberately narrow — no incidental formatting checks.
//!
//! ## The cluster
//!
//! These four components are the leaf primitives every dashboard page is built
//! from, and each drifted from 9router on the same axis: a size step (one or
//! two pixels of type or padding), a radius step, or a surface step. Because
//! they are primitives the drift is multiplied — ~95 `<Card>` call sites inherit
//! the default padding alone, and `md` is the default `<Badge>` size, so the
//! tracking/leading changes hit every un-sized badge in the tree.
//!
//! ## One deliberate divergence from the audit's literal fix
//!
//! The audit text for P214-001 asks for `hover:bg-surface-2/50` and
//! `hover:border-brand-500/30`. Neither spelling compiles in this repo:
//! `surface.2` and `brand.500` are declared in `web/tailwind.config.js` as bare
//! `var(--…)` references with no `<alpha-value>` channel placeholder, and
//! Tailwind v3 resolves a colour it cannot parse to `undefined` — the whole
//! declaration is dropped, so the hover would silently do nothing. 9router is on
//! Tailwind v4, which synthesises the alpha for any colour value. So the
//! assertions below encode the *rendered* intent (a 50% wash of `--color-surface-2`,
//! a 30% coral border) in the spelling that actually reaches the browser here:
//! `brand-coral` is the channel-format alias of the identical `--color-brand-500`
//! value, and the wash is spelled out as an explicit `color-mix`.
//!
//! Assertions carry the 9router citation they encode, so a future reader can
//! check the port against the canonical source without re-deriving it.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn web_root() -> PathBuf {
    repo_root().join("web")
}

fn read_web(rel: &str) -> String {
    let path = web_root().join(rel);
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

/// Slice the text between two markers, so an assertion about one declaration
/// cannot be satisfied by an unrelated line elsewhere in the file.
fn slice_between(src: &str, start: &str, end: &str) -> String {
    let from = src
        .find(start)
        .unwrap_or_else(|| panic!("slice start `{start}` not found"));
    let rest = &src[from..];
    let to = rest
        .find(end)
        .unwrap_or_else(|| panic!("slice end `{end}` not found after `{start}`"));
    rest[..to].to_string()
}

/// Every `.tsx` under `web/src`, so an assertion about "no call site does X"
/// covers call sites that did not exist when the finding was filed.
fn web_src_tsx_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries =
            std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read dir {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "tsx") {
                out.push(path);
            }
        }
    }

    let mut out = Vec::new();
    walk(&web_root().join("src"), &mut out);
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// P214-001 · P2 · Card: padding, radius, default shadow, hover, row wash
// ---------------------------------------------------------------------------

const CARD: &str = "web/src/shared/components/Card.tsx";

/// ~95 call sites pass no `padding`, so the `md` step is the card's real
/// padding across the whole dashboard.
#[test]
fn card_default_padding_matches_9router() {
    let card = read_web("src/shared/components/Card.tsx");
    let paddings = slice_between(&card, "const paddings", "const radii");

    assert_contains(&paddings, "md: \"p-6\"", CARD, "Card.js:21 — 24px default");
    assert_contains(&paddings, "lg: \"p-8\"", CARD, "Card.js:22 — 32px at lg");
    assert_contains(
        &paddings,
        "xl: \"p-10\"",
        CARD,
        "9router has no xl step; the \
        openproxy-only step keeps the same 8px increment as md and lg",
    );
}

/// 9router hard-codes `rounded-[14px]` on both the elevated and the plain
/// branch (Card.js:29). The 12px `rounded-mini-lg` made every default card read
/// a step tighter than its neighbours.
#[test]
fn card_default_radius_is_14px() {
    let card = read_web("src/shared/components/Card.tsx");
    let radii = slice_between(&card, "const radii", "const toneSurfaces");

    assert_contains(&radii, "lg: \"rounded-[14px]\"", CARD, "Card.js:29");
    assert_not_contains(
        &radii,
        "lg: \"rounded-mini-lg\"",
        CARD,
        "that token is 12px",
    );
}

/// The non-elevated branch keeps a soft shadow, so a bare `<Card>` reads as a
/// raised panel against the canvas rather than a flat box.
#[test]
fn card_keeps_a_shadow_when_not_elevated() {
    let card = read_web("src/shared/components/Card.tsx");

    assert_contains(
        &card,
        "elev ? \"shadow-elev\" : \"shadow-soft\"",
        CARD,
        "Card.js:29 — both branches carry a shadow; only the depth differs",
    );
    assert_not_contains(
        &card,
        "shadow-none",
        CARD,
        "the flat default is the defect — it is what made bare cards read as \
         unraised boxes",
    );
}

/// 9router's hover lifts the card with the warm shadow and tints the border at
/// 30% of the brand colour (Card.js:30). `brand-coral` is the channel-format
/// alias of the same `--color-brand-500` value; the plain `brand-500` token is a
/// bare `var()` and its `/30` modifier is dropped at build time.
#[test]
fn card_hover_lifts_and_tints_the_border() {
    let card = read_web("src/shared/components/Card.tsx");
    let tone_hover = slice_between(&card, "const toneHover", "const rowHover");

    assert_contains(
        &tone_hover,
        "hover:border-brand-coral/30",
        CARD,
        "Card.js:30",
    );
    assert_contains(&tone_hover, "hover:shadow-warm", CARD, "Card.js:30");
    assert_not_contains(
        &tone_hover,
        "hover:shadow-soft",
        CARD,
        "soft is the resting \
        shadow in 9router; hovering must lift to the warm one",
    );
    assert_contains(
        &card,
        "transition-all cursor-pointer",
        CARD,
        "Card.js:30 — a shadow cross-fade needs `transition-all`, not `-colors`",
    );
}

/// Title inherits the page's 16px and the subtitle is 14px, so a card header is
/// not a step below the section heading above it.
#[test]
fn card_title_and_subtitle_match_9router_type_scale() {
    let card = read_web("src/shared/components/Card.tsx");

    assert_not_contains(
        &card,
        "font-semibold text-[15px]",
        CARD,
        "Card.js:46 renders the \
        title at the inherited 16px with no size override",
    );
    assert_not_contains(
        &card,
        "text-[13px] mt-0.5",
        CARD,
        "Card.js:49 — the subtitle is \
        `text-sm` (14px) with no top margin",
    );
    assert_contains(&card, "text-sm mt-0.5", CARD, "Card.js:49");
}

/// List rows wash at 50% rather than jumping to an opaque fill. The audit's
/// literal `hover:bg-surface-2/50` is a Tailwind v3 no-op here (see the module
/// docs), so the mix is spelled out.
#[test]
fn card_row_hover_is_a_50_percent_wash_not_an_opaque_fill() {
    let card = read_web("src/shared/components/Card.tsx");

    assert_contains(
        &card,
        "color-mix(in_srgb,var(--color-surface-2)_50%,transparent)",
        CARD,
        "Card.js:82,103 — a 50% wash of the card surface",
    );
    assert_not_contains(
        &card,
        "hover:bg-surface-soft",
        CARD,
        "the opaque fill was the defect — it is a visibly bigger jump on hover \
         than the wash 9router uses",
    );
}

// ---------------------------------------------------------------------------
// P217-001 · P2 · Input: no fixed height, 10px radius, ring error state
// ---------------------------------------------------------------------------

const INPUT: &str = "web/src/shared/components/Input.tsx";

/// The fixed 40px box is the highest-value single change here: it is what
/// crammed the 16px iOS-zoom font into a box sized for 14px type. Every form in
/// the dashboard inherits it.
#[test]
fn input_box_is_padding_driven_not_fixed_height() {
    let input = read_web("src/shared/components/Input.tsx");
    let classes = slice_between(&input, "\"w-full py-2.5", "inputClassName");

    assert_not_contains(
        &classes,
        "h-10",
        INPUT,
        "Input.js:41 carries no height at all — \
        the box grows with its content, so the 16px mobile font raises the target",
    );
    assert_contains(
        &input,
        "text-[16px] sm:text-sm",
        INPUT,
        "Input.js:46 keeps the iOS zoom fix, but the desktop step is `text-sm`",
    );
}

#[test]
fn input_surface_matches_9router() {
    let input = read_web("src/shared/components/Input.tsx");

    assert_contains(
        &input,
        "bg-surface-2 rounded-[10px]",
        INPUT,
        "Input.js:41 — recessed 10px surface, not the flat 8px canvas field",
    );
    assert_contains(
        &input,
        "border border-transparent",
        INPUT,
        "Input.js:42 — the resting border is transparent; the field is defined by \
         its fill, not by a hairline",
    );
    assert_contains(
        &input,
        "material-symbols-outlined text-[20px]",
        INPUT,
        "Input.js:31",
    );
}

/// The error state is a ring, so a flagged field keeps the same geometry as a
/// clean one instead of shifting by a border width.
#[test]
fn input_error_state_is_a_ring() {
    let input = read_web("src/shared/components/Input.tsx");

    assert_contains(
        &input,
        "ring-1 ring-red-500 focus:ring-2 focus:ring-red-500/40 border-red-500/40",
        INPUT,
        "Input.js:48",
    );
    assert_not_contains(
        &input,
        "focus:border-[color:var(--color-danger)]",
        INPUT,
        "the red border colour was the defect — it reads flatter than a ring and \
         the error/hint rows were a step larger than 9router's",
    );
}

/// Hint and error copy sits at 12px in 9router (Input.js:45,49); 14px changed
/// two-line form density on every modal in the app.
#[test]
fn input_hint_and_error_rows_are_12px() {
    let input = read_web("src/shared/components/Input.tsx");
    let rows = slice_between(&input, "{error && (", "\n    </div>");

    assert_contains(
        &rows,
        "text-xs text-[color:var(--color-danger)]",
        INPUT,
        "Input.js:55",
    );
    assert_contains(&rows, "text-xs text-slate", INPUT, "Input.js:61");
    assert_not_contains(&rows, "type-body-sm", INPUT, "that token is 14px");
}

// ---------------------------------------------------------------------------
// P220-001 · P2 · SegmentedControl: the grouped bar is the default
// ---------------------------------------------------------------------------

const SEGMENTED: &str = "web/src/shared/components/SegmentedControl.tsx";

/// All three call sites omit `variant`, so the default is the whole render.
#[test]
fn segmented_control_defaults_to_the_grouped_bar() {
    let seg = read_web("src/shared/components/SegmentedControl.tsx");

    assert_contains(
        &seg,
        "variant = \"segmented\"",
        SEGMENTED,
        "SegmentedControl.js has no variant at all — the grouped bar is the only \
         shape it renders",
    );
    assert_not_contains(
        &seg,
        "variant = \"pill\"",
        SEGMENTED,
        "the free-standing ink-filled pill row was the default and is not a shape \
         9router has",
    );
}

/// The step that flipped the default is easy to leave behind: a call site that
/// was written against the old default would now ask for a shape nobody uses.
#[test]
fn no_call_site_opts_into_the_pill_variant() {
    let offenders: Vec<String> = web_src_tsx_files()
        .iter()
        .filter(|path| {
            std::fs::read_to_string(path)
                .map(|src| src.contains("variant=\"pill\""))
                .unwrap_or(false)
        })
        .map(|path| {
            path.strip_prefix(repo_root())
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "these call sites pin the pill variant, which defeats the default flip: {offenders:?}"
    );
}

#[test]
fn segmented_control_sizes_match_9router_scale() {
    let seg = read_web("src/shared/components/SegmentedControl.tsx");
    let sizes = slice_between(&seg, "const sizes", "if (variant");

    assert_contains(
        &sizes,
        "sm: \"h-7 text-xs\"",
        SEGMENTED,
        "SegmentedControl.js:13",
    );
    assert_contains(
        &sizes,
        "md: \"h-9 text-sm\"",
        SEGMENTED,
        "SegmentedControl.js:14",
    );
    assert_contains(
        &sizes,
        "lg: \"h-11 text-base\"",
        SEGMENTED,
        "SegmentedControl.js:15",
    );
    assert_not_contains(
        &sizes,
        "px-3",
        SEGMENTED,
        "the horizontal padding comes from \
        the button class, not the size step",
    );
}

/// 9router's grouped control is one container behind `bg-surface-2`; the pill row
/// has no shared surface at all, which is what read as "free-standing".
#[test]
fn segmented_bar_is_one_grouped_surface() {
    let seg = read_web("src/shared/components/SegmentedControl.tsx");
    let bar = slice_between(&seg, "// segmented (grouped", "</div>");

    assert_contains(
        &bar,
        "inline-flex items-center p-1 rounded-[10px] overflow-x-auto",
        SEGMENTED,
        "SegmentedControl.js:21",
    );
    assert_contains(&bar, "bg-surface-2", SEGMENTED, "SegmentedControl.js:22");
    assert_contains(
        &bar,
        "shrink-0 px-4 rounded-[8px] font-medium transition-all",
        SEGMENTED,
        "SegmentedControl.js:31",
    );
    assert_contains(
        &bar,
        "bg-surface text-ink shadow-sm",
        SEGMENTED,
        "SegmentedControl.js:34 — the active tab is a raised surface, `text-ink` \
         standing in for `text-text-main`",
    );
    assert_contains(
        &bar,
        "text-steel hover:text-ink",
        SEGMENTED,
        "SegmentedControl.js:35 — `text-steel` stands in for `text-text-muted`",
    );
}

// ---------------------------------------------------------------------------
// P313-001 · P3 · Badge: 12px md, no tracking, no forced line-height
// ---------------------------------------------------------------------------

const BADGE: &str = "web/src/shared/components/Badge.tsx";

#[test]
fn badge_sizes_match_9router() {
    let badge = read_web("src/shared/components/Badge.tsx");
    let sizes = slice_between(&badge, "const sizes", "export default function Badge");

    assert_contains(
        &sizes,
        "sm: \"px-2 py-0.5 text-[10px]\"",
        BADGE,
        "Badge.js:15",
    );
    assert_contains(
        &sizes,
        "md: \"px-2.5 py-1 text-xs\"",
        BADGE,
        "Badge.js:16 — 12px, and \
        `md` is the default size, so this hits every un-sized badge in the tree",
    );
    assert_contains(&sizes, "lg: \"px-3 py-1.5 text-sm\"", BADGE, "Badge.js:17");
    assert_not_contains(
        &sizes,
        "tracking-wide",
        BADGE,
        "Badge.js:31 sets no letter-spacing",
    );
    assert_not_contains(&sizes, "text-[11px]", BADGE, "1px under 9router's md");
    assert_not_contains(&sizes, "text-[13px]", BADGE, "1px under 9router's lg");
}

/// `leading-none` centres the glyph on the font box instead of the browser's
/// default line-height, which is what made badge-dense grids sit shorter than
/// 9router's. Removing it shifts vertical rhythm, so it is asserted explicitly.
#[test]
fn badge_base_class_does_not_force_a_line_height() {
    let badge = read_web("src/shared/components/Badge.tsx");
    let base = slice_between(&badge, "className={cn(", "radius,");

    assert_contains(
        &base,
        "inline-flex items-center gap-1.5 font-semibold",
        BADGE,
        "Badge.js:31",
    );
    assert_not_contains(
        &base,
        "leading-none",
        BADGE,
        "Badge.js:31 carries no line-height \
        override; the default applies",
    );
}

/// The `new` / `beta` / `code` variants are openproxy extensions with no 9router
/// counterpart. Trimming them would break consumers, so the port keeps them.
#[test]
fn badge_keeps_its_additive_variants() {
    let badge = read_web("src/shared/components/Badge.tsx");
    let variants = slice_between(&badge, "const variants", "const sizes");

    for (name, why) in [
        ("new: ", "full-coral NEW/BETA pill"),
        ("beta: ", "teal status-dot tone"),
        ("code: ", "inline code chip"),
    ] {
        assert_contains(&variants, name, BADGE, why);
    }
}
