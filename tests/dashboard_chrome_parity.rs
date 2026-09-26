//! Source-contract cover for bead `openproxy-vb3m` — the shared dashboard
//! chrome (Sidebar, Header, HeaderMenu, Modal, Tooltip, Button, Select, the
//! global stylesheet and the Tailwind config).
//!
//! ## Why a Rust test for a TypeScript defect
//!
//! The dashboard has no JS test runner (no vitest/jest in `web/package.json`),
//! so this follows the convention established in
//! `tests/available_models_disabled.rs` and `tests/login_autofocus_parity.rs`:
//! read the source as text and make narrow source-contract assertions.
//! Deliberately narrow — no incidental formatting checks.
//!
//! ## The cluster
//!
//! The dashboard shell is the one surface every page renders, and each of the
//! findings below is a divergence from 9router's
//! `.tmp/9router/src/shared/components/*` that is invisible to any Rust test.
//! They are grouped here rather than split per finding because they share one
//! file: the Sidebar nav constants are rewritten by the tail restructure, the
//! nav-order fix, the surface restyle and the first-paint pathname fix at once.
//!
//! Assertions carry the 9router citation they encode, so a future reader can
//! check the port against the canonical source without re-deriving it.

use std::path::PathBuf;

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

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

// ---------------------------------------------------------------------------
// P332-003 · P1 · dark: utilities were media-scoped, not class-scoped
// ---------------------------------------------------------------------------

/// Tailwind v3 defaults to `darkMode: "media"`, so every `dark:` utility was
/// compiled inside `@media (prefers-color-scheme: dark)` and ignored the
/// `.dark` class that `themeStore` toggles. 9router drives dark mode from the
/// class alone (`globals.css:5` + `themeStore.js:47`).
#[test]
fn tailwind_dark_mode_is_class_scoped() {
    let config = read_web("tailwind.config.js");

    assert_contains(
        &config,
        "darkMode: \"class\"",
        "web/tailwind.config.js",
        "v3 defaults to `darkMode: 'media'`, which compiles all 75 `dark:` \
         utilities into a prefers-color-scheme block and ignores the `.dark` \
         class the in-app toggle actually sets",
    );
}

/// The v4-only directive is silently dropped by v3 and reads as though class
/// scoping is already configured.
#[test]
fn v4_only_custom_variant_directive_is_removed() {
    let css = read_web("src/shared/components/styles/global.css");

    assert_not_contains(
        &css,
        "@custom-variant dark",
        "web/src/shared/components/styles/global.css",
        "v3 discards this at-rule, so it registers nothing while looking like it \
         configures class-scoped dark mode; `darkMode: \"class\"` in the Tailwind \
         config is the directive that actually does the work",
    );
}

// ---------------------------------------------------------------------------
// P263-001 / P271-001 · P2 · Modal: closeOnOverlay, traffic lights, a11y name
// ---------------------------------------------------------------------------

/// 9router's `Modal` defaults `closeOnOverlay = true` and no call site in either
/// tree overrides it, so all ~42 mounts dismiss on an outside click.
#[test]
fn modal_dismisses_on_overlay_by_default() {
    let modal = read_web("src/shared/components/Modal.tsx");

    assert_contains(
        &modal,
        "closeOnOverlay = true",
        "web/src/shared/components/Modal.tsx",
        "Modal.js:15 defaults it to true; the overlay onClick at Modal.js:51 is \
         dead for every mount in the app when the default is false",
    );
    assert_not_contains(
        &modal,
        "closeOnOverlay = false",
        "web/src/shared/components/Modal.tsx",
        "the flipped default is the defect; a dialog that needs the old behaviour \
         should opt out explicitly at its own call site",
    );
}

/// 9router's desktop close affordance is a Tooltip-wrapped 16px red dot that
/// reveals ✕ on hover; OpenProxy rendered three inert 12px dots plus a second ✕.
#[test]
fn modal_traffic_lights_match_9router() {
    let modal = read_web("src/shared/components/Modal.tsx");

    assert_contains(
        &modal,
        "hidden md:flex items-center gap-2 mr-4 ml-2",
        "web/src/shared/components/Modal.tsx",
        "Modal.js:71 hides the traffic lights below md, where the mobile ✕ takes over",
    );
    assert_contains(
        &modal,
        "w-4 h-4 rounded-full",
        "web/src/shared/components/Modal.tsx",
        "9router's dots are 16px; the 12px OpenProxy used are the Sidebar's \
         decorative dots, not the dialog's close control",
    );
    assert_contains(
        &modal,
        "md:hidden p-1.5 rounded-[10px]",
        "web/src/shared/components/Modal.tsx",
        "Modal.js:94 scopes the ✕ to mobile, so desktop shows exactly one close \
         affordance rather than a dot and a cross side by side",
    );
    assert_contains(
        &modal,
        "group-hover/dot:opacity-100",
        "web/src/shared/components/Modal.tsx",
        "Modal.js:79 reveals ✕ on hover — the only cue that the red dot is live",
    );
    assert_contains(
        &modal,
        "bg-[#3a3a3a]/20 dark:bg-white/15 cursor-not-allowed",
        "web/src/shared/components/Modal.tsx",
        "Modal.js:82-83 render the yellow and green dots as dimmed, non-functional \
         placeholders rather than full macOS colours",
    );
}

/// Both close controls need an explicit accessible name, and the red dot must be
/// a real button — before the fix it was a bare `<div>` with no name at all.
#[test]
fn every_modal_close_control_has_an_accessible_name() {
    let modal = read_web("src/shared/components/Modal.tsx");

    assert!(
        count(&modal, "aria-label=\"Close\"") >= 2,
        "web/src/shared/components/Modal.tsx must name both close controls \
         (the traffic-light button and the mobile ✕) — a screen reader otherwise \
         announces an unlabelled glyph on every modal in the app"
    );
    assert_contains(
        &modal,
        "<button\n                      type=\"button\"\n                      onClick={onClose}\n                      aria-label=\"Close\"",
        "web/src/shared/components/Modal.tsx",
        "the red dot must be the button Modal.js:73-80 makes it, not a decorative div",
    );
    assert_not_contains(
        &modal,
        "w-3 h-3 rounded-full bg-[#FF5F56]",
        "web/src/shared/components/Modal.tsx",
        "the inert 12px dot is what made the header's primary close control \
         non-functional",
    );
}

/// The header is gated on the traffic lights, not the ✕: a titleless dialog must
/// still offer a way out on desktop.
#[test]
fn modal_header_renders_without_a_title() {
    let modal = read_web("src/shared/components/Modal.tsx");

    assert_contains(
        &modal,
        "{(title || showTrafficLights) && (",
        "web/src/shared/components/Modal.tsx",
        "Modal.js:66 gates on `title || showTrafficLights`; gating on the mobile ✕ \
         hides the whole bar on desktop whenever a dialog has no title",
    );
}

// ---------------------------------------------------------------------------
// P266-001 · P2 · Tooltip lost its `color` prop
// ---------------------------------------------------------------------------

/// The bubble tint is the only reason the red dot can carry a matching tooltip.
#[test]
fn tooltip_supports_the_color_prop() {
    let tooltip = read_web("src/shared/components/Tooltip.tsx");

    assert_contains(
        &tooltip,
        "color?: string",
        "web/src/shared/components/Tooltip.tsx",
        "Tooltip.js:3 takes `color`; without it the traffic-light tooltip renders \
         dark grey on a red dot",
    );
    assert_contains(
        &tooltip,
        "backgroundColor: color",
        "web/src/shared/components/Tooltip.tsx",
        "Tooltip.js:11 applies the tint inline and drops the default background \
         class so the two do not fight",
    );
    assert_contains(
        &tooltip,
        "const bgClass = color ? \"\" : \"bg-gray-900\";",
        "web/src/shared/components/Tooltip.tsx",
        "Tooltip.js:12 — keeping both would leave the default grey under the tint",
    );
}

// ---------------------------------------------------------------------------
// P256-001 · P2 · Button metrics
// ---------------------------------------------------------------------------

/// 9router's buttons are 28/36/44px, weight 600, press to 0.97 and dim to 50%
/// when disabled. OpenProxy ran 4px taller at weight 500 with no disabled tint.
#[test]
fn button_metrics_match_9router() {
    let button = read_web("src/shared/components/Button.tsx");

    for size in ["h-7 px-3 text-xs", "h-9 px-4 text-sm", "h-11 px-6 text-sm"] {
        assert_contains(
            &button,
            size,
            "web/src/shared/components/Button.tsx",
            "Button.js:15-17 sizes buttons at 28/36/44px",
        );
    }
    assert_contains(
        &button,
        "font-semibold",
        "web/src/shared/components/Button.tsx",
        "Button.js:35 puts the weight on the base string, not per size",
    );
    assert_contains(
        &button,
        "active:scale-[0.97]",
        "web/src/shared/components/Button.tsx",
        "Button.js:36 press-scale; 0.99 is imperceptible",
    );
    assert_contains(
        &button,
        "disabled:opacity-50",
        "web/src/shared/components/Button.tsx",
        "Button.js:36 — a disabled button that looks enabled invites a second click",
    );
    for stale in ["h-8 px-3", "h-10 px-4", "h-12 px-6", "active:scale-[0.99]"] {
        assert_not_contains(
            &button,
            stale,
            "web/src/shared/components/Button.tsx",
            "the pre-fix metric this assertion replaces",
        );
    }
}

// ---------------------------------------------------------------------------
// P270-001 · P2 · Sidebar: surface, nav rows, tail, nav contents
// ---------------------------------------------------------------------------

/// The frosted macOS panel and the 12px/4px nav rows with a coral-tinted active
/// state — OpenProxy had an opaque panel, 16px/6px rows and a left rail.
#[test]
fn sidebar_surface_and_nav_rows_match_9router() {
    let sidebar = read_web("src/shared/components/Sidebar.tsx");

    assert_contains(
        &sidebar,
        "bg-vibrancy backdrop-blur-xl",
        "web/src/shared/components/Sidebar.tsx",
        "Sidebar.js:112 — `.bg-vibrancy` is already defined in global.css; the \
         opaque `bg-canvas` block killed the translucency",
    );
    assert_contains(
        &sidebar,
        "px-3 py-1 rounded-lg transition-all",
        "web/src/shared/components/Sidebar.tsx",
        "Sidebar.js:145 — OpenProxy used pl-4/pr-3/py-1.5 with transition-colors, \
         making every row 4px taller with a different inset",
    );
    assert_contains(
        &sidebar,
        "group-hover:text-primary transition-colors",
        "web/src/shared/components/Sidebar.tsx",
        "Sidebar.js:152-153 recolours the glyph on hover, which is the whole cue \
         that a row is clickable",
    );
    assert_contains(
        &sidebar,
        "text-[13px] font-medium",
        "web/src/shared/components/Sidebar.tsx",
        "Sidebar.js:158 sets the label weight; OpenProxy dropped it",
    );
    assert_not_contains(
        &sidebar,
        "before:w-[3px]",
        "web/src/shared/components/Sidebar.tsx",
        "the coral left rail replaced 9router's `bg-primary/10 text-primary` tint",
    );
    assert_not_contains(
        &sidebar,
        "bg-surface-card text-ink font-medium before:",
        "web/src/shared/components/Sidebar.tsx",
        "the pre-fix active row painted a cream pill plus a rail",
    );
}

/// 9router closes `</nav>` right after the System block — there is no footer, and
/// the tail inside the nav is 9Remote, 9English, Settings.
#[test]
fn sidebar_tail_matches_9router() {
    let sidebar = read_web("src/shared/components/Sidebar.tsx");

    assert_contains(
        &sidebar,
        "https://9english.net/",
        "web/src/shared/components/Sidebar.tsx",
        "Sidebar.js:309-323 renders 9English between 9Remote and Settings",
    );
    assert_contains(
        &sidebar,
        ">9Remote<",
        "web/src/shared/components/Sidebar.tsx",
        "Sidebar.js:305 labels the row 9Remote; OpenProxy's plain \"Remote\" \
         collides with the header menu's Remote entry",
    );
    assert_contains(
        &sidebar,
        ">Settings<",
        "web/src/shared/components/Sidebar.tsx",
        "Settings is a nav row inside the System block (Sidebar.js:326-345)",
    );
    assert_not_contains(
        &sidebar,
        "footerItems",
        "web/src/shared/components/Sidebar.tsx",
        "the array only ever fed the footer div that 9router does not have",
    );
    assert_not_contains(
        &sidebar,
        "border-t border-hairline-soft space-y-1",
        "web/src/shared/components/Sidebar.tsx",
        "the border-t footer wrapper relocates Settings out of the nav",
    );
    assert_not_contains(
        &sidebar,
        "power_settings_new",
        "web/src/shared/components/Sidebar.tsx",
        "Shutdown belongs in the header overflow menu (HeaderMenu.js:106); a second \
         full-width copy in the sidebar makes the affordance ambiguous",
    );
}

/// The primary nav is 9router's seven rows, in 9router's order, with 9router's
/// labels. Five OpenProxy-only routes (Compression, PXPIPE, Payload Rules, DB
/// Backups, MITM) stay routable but leave the nav.
#[test]
fn primary_nav_matches_9router_order_and_labels() {
    let sidebar = read_web("src/shared/components/Sidebar.tsx");
    let nav = slice_between(&sidebar, "const navItems: NavItem[] = [", "];");

    let order = [
        "Endpoint & Key",
        "Providers",
        "Combo & Vision Adapter",
        "Usage",
        "Quota Tracker",
        "Token Saver",
        "CLI Tools",
    ];
    let mut prev = 0usize;
    for label in order {
        let at = nav
            .find(&format!("label: \"{label}\""))
            .unwrap_or_else(|| panic!("navItems is missing the `{label}` row — Sidebar.js:20-30"));
        assert!(
            at > prev,
            "navItems order diverges from 9router: `{label}` must come after the \
             preceding entry (Sidebar.js:20-30)"
        );
        prev = at;
    }

    for hidden in [
        "Compression",
        "Payload Rules",
        "DB Backups",
        "PXPIPE",
        "MITM",
    ] {
        assert_not_contains(
            &nav,
            &format!("label: \"{hidden}\""),
            "web/src/shared/components/Sidebar.tsx",
            "the page stays routable at its URL, but 9router keeps it out of the \
             primary nav (Sidebar.js:28 comments PXPIPE out; the other four have \
             no 9router nav counterpart)",
        );
    }
}

/// MITM and PXPIPE keep exactly one entry point each — the pages stay routable.
#[test]
fn openproxy_only_routes_are_nav_hidden_but_still_routable() {
    let sidebar = read_web("src/shared/components/Sidebar.tsx");

    for href in ["/dashboard/mitm", "/dashboard/pxpipe"] {
        assert_not_contains(
            &sidebar,
            &format!("href: \"{href}\""),
            "web/src/shared/components/Sidebar.tsx",
            "9router deliberately keeps this row out of the nav (Sidebar.js:28); \
             the page itself stays routable",
        );
    }
    assert_contains(
        &read_web("src/components/cli-tools/MitmLinkCard.tsx"),
        "/dashboard/mitm",
        "web/src/components/cli-tools/MitmLinkCard.tsx",
        "MitmLinkCard is the sole entry point to /dashboard/mitm once the nav row \
         is gone — losing it would strand the page",
    );
}

// ---------------------------------------------------------------------------
// P271-002 · P2 · Header: chrome, typography, action rail
// ---------------------------------------------------------------------------

/// The bar is 12px/8px and fully transparent at lg, and the H1 is Inter 600 at
/// 16→24px rather than Instrument Serif 400 at 20→30px.
#[test]
fn page_header_typography_and_chrome_match_9router() {
    let header = read_web("src/shared/components/Header.tsx");

    assert_contains(
        &header,
        "lg:bg-transparent",
        "web/src/shared/components/Header.tsx",
        "Header.js:230 lets the landing-grid texture run under the bar at lg",
    );
    assert_contains(
        &header,
        "pt-3 pb-2",
        "web/src/shared/components/Header.tsx",
        "Header.js:230 — 12px top / 8px bottom, not 16/16",
    );
    assert_contains(
        &header,
        "text-base lg:text-2xl font-semibold",
        "web/src/shared/components/Header.tsx",
        "Header.js:275 and :291 — Inter 600 at 16→24px; the serif 20→30px title \
         competed with the section headings beneath it",
    );
    assert_not_contains(
        &header,
        "font-serif font-normal text-[20px] lg:text-[30px]",
        "web/src/shared/components/Header.tsx",
        "the pre-fix page title",
    );
    assert_not_contains(
        &header,
        "lg:bg-canvas",
        "web/src/shared/components/Header.tsx",
        "an opaque bar hides the grid the sidebar panel now shows through",
    );
    assert_contains(
        &header,
        "hidden lg:block text-sm text-text-muted truncate",
        "web/src/shared/components/Header.tsx",
        "Header.js:296 sets the description with no extra margin",
    );
}

/// Order is identity chip → search → donate → theme → language → menu.
#[test]
fn header_action_rail_order_matches_9router() {
    let header = read_web("src/shared/components/Header.tsx");

    let chip = header
        .find("loginMethod === \"OIDC\"")
        .expect("the identity chip block must still be present");
    let search = header
        .find("<HeaderSearchInput />")
        .expect("HeaderSearchInput must still be rendered");
    let theme = header.find("<ThemeToggle />").expect("ThemeToggle");
    let language = header.find("<HeaderLanguage />").expect(
        "HeaderLanguage must render between ThemeToggle and HeaderMenu (Header.js:326-329)",
    );
    let menu = header.find("<HeaderMenu").expect("HeaderMenu");

    assert!(
        chip < search,
        "Header.js:306-318 renders the identity chip before the search box; OpenProxy \
         had them swapped"
    );
    assert!(
        theme < language && language < menu,
        "Header.js:327-329 orders theme → language → menu",
    );
}

/// The language control is a port of 9router's HeaderLanguage.js.
#[test]
fn header_renders_the_language_button() {
    let lang = read_web("src/shared/components/HeaderLanguage.tsx");

    assert_contains(
        &lang,
        "title=\"Language\"",
        "web/src/shared/components/HeaderLanguage.tsx",
        "HeaderLanguage.js:30 labels the flag button",
    );
    assert_contains(
        &lang,
        "data-i18n-skip=\"true\"",
        "web/src/shared/components/HeaderLanguage.tsx",
        "HeaderLanguage.js:31 keeps the i18n runtime from translating the flag",
    );
    assert_contains(
        &lang,
        "LOCALE_FLAGS[locale] || \"🌐\"",
        "web/src/shared/components/HeaderLanguage.tsx",
        "HeaderLanguage.js:33 renders the active locale's flag from the cookie",
    );
    assert_contains(
        &lang,
        "<LanguageSwitcher hideTrigger isOpen={open}",
        "web/src/shared/components/HeaderLanguage.tsx",
        "the button is the trigger; the switcher is a controlled overlay",
    );
    assert_contains(
        &read_web("src/shared/components/Header.tsx"),
        "import HeaderLanguage from \"@/shared/components/HeaderLanguage\";",
        "web/src/shared/components/Header.tsx",
        "Header.js:328 imports it",
    );
}

// ---------------------------------------------------------------------------
// P271-003 · P2 · HeaderMenu: Shutdown row + Close Proxy confirm
// ---------------------------------------------------------------------------

/// Shutdown lives in the header overflow menu (HeaderMenu.js:100-127) and is
/// confirmed through a "Close Proxy" dialog.
#[test]
fn header_menu_has_the_shutdown_item_and_close_proxy_confirm() {
    let menu = read_web("src/shared/components/HeaderMenu.tsx");

    assert_contains(
        &menu,
        "icon=\"power_settings_new\"",
        "web/src/shared/components/HeaderMenu.tsx",
        "HeaderMenu.js:94 — OpenProxy had no Shutdown row at all",
    );
    assert_contains(
        &menu,
        "label=\"Shutdown\"",
        "web/src/shared/components/HeaderMenu.tsx",
        "HeaderMenu.js:95",
    );
    assert_contains(
        &menu,
        "title=\"Close Proxy\"",
        "web/src/shared/components/HeaderMenu.tsx",
        "HeaderMenu.js:113 confirms the server shutdown under this title",
    );
    assert_contains(
        &menu,
        "Are you sure you want to close the proxy server?",
        "web/src/shared/components/HeaderMenu.tsx",
        "HeaderMenu.js:114",
    );
    assert_contains(
        &menu,
        "setShutdownOpen(true)",
        "web/src/shared/components/HeaderMenu.tsx",
        "HeaderMenu.js:96 opens the confirm rather than shutting down on one click",
    );
    assert_contains(
        &menu,
        "await fetch(\"/api/dashboard/shutdown\", { method: \"POST\" })",
        "web/src/shared/components/HeaderMenu.tsx",
        "OpenProxy's shutdown route (src/server/api/shutdown.rs:24), not 9router's \
         /api/version/shutdown",
    );
}

// ---------------------------------------------------------------------------
// P271-005 · P2 · DashboardLayout toasts
// ---------------------------------------------------------------------------

/// Per-severity tint, 8px vertical padding, an 18px icon and a blurred surface —
/// OpenProxy had a neutral card with a 3px colour rail.
#[test]
fn toast_severity_styling_matches_9router() {
    let layout = read_web("src/shared/components/layouts/DashboardLayout.tsx");

    for (severity, color) in [
        ("success", "green"),
        ("error", "red"),
        ("warning", "amber"),
        ("info", "blue"),
    ] {
        assert_contains(
            &layout,
            &format!(
                "border-{color}-500/30 bg-{color}-500/10 text-{color}-600 \
                 dark:text-{color}-400"
            ),
            "web/src/shared/components/layouts/DashboardLayout.tsx",
            &format!(
                "DashboardLayout.js:9-27 tints the {severity} toast; a neutral card with \
                 a 3px rail reads as the same severity for all four"
            ),
        );
    }
    assert_contains(
        &layout,
        "rounded-lg border px-3 py-2 shadow-lg backdrop-blur-sm",
        "web/src/shared/components/layouts/DashboardLayout.tsx",
        "DashboardLayout.js:48 — OpenProxy ran py-3 with no blur and an injected \
         before: rail",
    );
    assert_contains(
        &layout,
        "text-current/70 hover:text-current",
        "web/src/shared/components/layouts/DashboardLayout.tsx",
        "DashboardLayout.js:60 — the dismiss control inherits the severity tint",
    );
    assert_not_contains(
        &layout,
        "before:w-[3px]",
        "web/src/shared/components/layouts/DashboardLayout.tsx",
        "the 3px left rail is the pre-fix severity signal",
    );
    assert_not_contains(
        &layout,
        "before:bg-success-text",
        "web/src/shared/components/layouts/DashboardLayout.tsx",
        "the pre-fix severity signal, one per type",
    );
}

/// 9router's toast block is bare divs; the only aria attribute is the dismiss
/// button's label. The two trees must not ship different announcement behaviour.
#[test]
fn toast_host_is_not_a_live_region() {
    let layout = read_web("src/shared/components/layouts/DashboardLayout.tsx");

    for attr in [
        "role=\"status\"",
        "aria-live=\"polite\"",
        "aria-atomic=",
        "aria-label=\"Notifications\"",
    ] {
        assert_not_contains(
            &layout,
            attr,
            "web/src/shared/components/layouts/DashboardLayout.tsx",
            "9router's DashboardLayout.js has no role/aria-live/aria-atomic anywhere \
             in the toast block; if a live region is the right target the change \
             belongs in 9router, not here",
        );
    }
    assert_contains(
        &layout,
        "aria-label=\"Dismiss notification\"",
        "web/src/shared/components/layouts/DashboardLayout.tsx",
        "DashboardLayout.js:61 — the one aria attribute 9router does have",
    );
}

// ---------------------------------------------------------------------------
// P272-002 · P2 · global.css: animation names and the landing grid
// ---------------------------------------------------------------------------

/// The call sites reference `fade-in` / `slide-in-right`; the stylesheet only
/// defined the `animate-` prefixed spellings, so every modal and drawer animated
/// by nothing at all.
#[test]
fn animation_class_names_match_their_call_sites() {
    let css = read_web("src/shared/components/styles/global.css");

    for (name, animation) in [
        (
            "slide-in-right",
            "slideInFromRight 0.25s cubic-bezier(0.22, 1, 0.36, 1) forwards",
        ),
        ("fade-in", "fadeIn 0.2s ease-out forwards"),
        (
            "slide-in-top",
            "slideInFromTop 0.18s cubic-bezier(0.22, 1, 0.36, 1) forwards",
        ),
    ] {
        assert_contains(
            &css,
            &format!("{animation}"),
            "web/src/shared/components/styles/global.css",
            &format!("the `.{name}` rule must carry 9router's timing (globals.css:412-414)"),
        );
        assert_contains(
            &css,
            &format!(".{name} {{"),
            "web/src/shared/components/styles/global.css",
            &format!(
                "the unprefixed name — that is what Drawer.tsx and Modal.tsx \
                      reference; only `animate-` was defined"
            ),
        );
    }
    assert_not_contains(
        &css,
        ".animate-fade-in {",
        "web/src/shared/components/styles/global.css",
        "the prefixed alias had no consumer; keeping it invites the same split again",
    );
}

/// 40px pitch, drawn from the accent variable, opacity 0.08 / 0.04 and no mask.
#[test]
fn landing_grid_matches_9router() {
    let css = read_web("src/shared/components/styles/global.css");
    let grid = slice_between(&css, ".landing-grid {", ".dark .landing-grid {");

    assert_contains(
        &grid,
        "background-size: 40px 40px",
        "web/src/shared/components/styles/global.css",
        "globals.css:471 — 9router pitches the grid at 40px, OpenProxy at 32px",
    );
    assert_contains(
        &grid,
        "var(--color-accent)",
        "web/src/shared/components/styles/global.css",
        "globals.css:468 draws the lines from the brand accent, not a fixed rgba",
    );
    assert_contains(
        &grid,
        "opacity: 0.08",
        "web/src/shared/components/styles/global.css",
        "globals.css:472 dims the whole layer instead of baking it into the line colour",
    );
    assert_contains(
        &css,
        ".dark .landing-grid {\n  opacity: 0.04;\n}",
        "web/src/shared/components/styles/global.css",
        "globals.css:475 — the dark override is opacity only",
    );
    assert_not_contains(
        &css,
        "background-size: 32px 32px",
        "web/src/shared/components/styles/global.css",
        "the pre-fix pitch",
    );
    assert_not_contains(
        &css,
        "mask-image: radial-gradient(ellipse at center",
        "web/src/shared/components/styles/global.css",
        "9router's grid is a uniform wash; the radial mask turned the dashboard \
         background into a centred vignette",
    );
}

// ---------------------------------------------------------------------------
// P271-006 · P3 · first-paint pathname, and `/` as the endpoint section
// ---------------------------------------------------------------------------

/// `useState("")` plus a mount effect leaves one frame with no pathname, so the
/// nav highlights nothing and the header renders empty. 9router's `usePathname()`
/// resolves before the first client render.
#[test]
fn shell_pathname_is_available_on_first_render() {
    for (file, label) in [
        ("src/shared/components/Sidebar.tsx", "Sidebar"),
        ("src/shared/components/Header.tsx", "Header"),
    ] {
        let src = read_web(file);
        assert_contains(
            &src,
            "const [pathname, setPathname] = useState(() =>",
            file,
            &format!(
                "a lazy initialiser gives {label} the real path on its first \
                      render instead of a blank one"
            ),
        );
        assert_contains(
            &src,
            "typeof window !== \"undefined\" ? window.location.pathname : \"\"",
            file,
            "the guard keeps SSR/hydration safe",
        );
        assert_not_contains(
            &src,
            "const [pathname, setPathname] = useState(\"\")",
            file,
            &format!("the empty-string seed is what makes {label} flash unstyled"),
        );
    }
}

/// `/` renders the endpoint shell, so the shell must treat it as that section
/// (Sidebar.js:72-73, Header.js:172-178) whether or not the server 307s it.
#[test]
fn root_path_matches_the_endpoint_section() {
    assert_contains(
        &read_web("src/shared/components/Sidebar.tsx"),
        "pathname === \"/dashboard\" ||\n        pathname === \"/\" ||",
        "web/src/shared/components/Sidebar.tsx",
        "Sidebar.js:72-73 highlights the endpoint row; at `/` OpenProxy's isActive \
         matched nothing",
    );
    assert_contains(
        &read_web("src/shared/components/Header.tsx"),
        "if (pathname === \"/dashboard\" || pathname === \"/\")",
        "web/src/shared/components/Header.tsx",
        "Header.js:172-178 titles `/` Endpoint; otherwise the header bar is blank",
    );
}

// ---------------------------------------------------------------------------
// P332-001 · P3 · Select grew a second, unlinked focusable control
// ---------------------------------------------------------------------------

/// 9router's Select renders exactly one focusable control — the native
/// `<select>` (Select.js:19-55). OpenProxy's `searchable` mode inserted a filter
/// input above it that narrowed the option list but never handed focus to the
/// select, so Enter and ArrowDown in it were dead ends and "choose a provider"
/// became two tab stops instead of one.
#[test]
fn select_renders_a_single_focusable_control() {
    let select = read_web("src/shared/components/Select.tsx");

    for gone in ["searchable", "searchPlaceholder"] {
        assert_not_contains(
            &select,
            gone,
            "web/src/shared/components/Select.tsx",
            "the filter mode is the second control; 9router's Select has no such \
             prop and its one interaction is the platform select keyboard",
        );
    }
    assert_contains(
        &select,
        "{options.map((option) => (",
        "web/src/shared/components/Select.tsx",
        "the option list is rendered unfiltered, straight off the prop",
    );
    assert_not_contains(
        &select,
        "aria-label={searchPlaceholder}",
        "web/src/shared/components/Select.tsx",
        "the filter input's accessible name only exists to label the extra control",
    );
}
