//! Source-contract cover for the page/component cluster of bead
//! `openproxy-vb3m` (wave `w4-disjoint`).
//!
//! ## Why a Rust test for TypeScript defects
//!
//! The dashboard has no JS test runner (no vitest/jest in `web/package.json`),
//! so this follows the convention established in
//! `tests/login_autofocus_parity.rs`: read the source as text and make narrow
//! source-contract assertions. Deliberately narrow — no incidental formatting
//! checks, and no assertion on a file this bead does not own.
//!
//! Each test names the 9router behaviour it pins and the finding number it
//! closes. 9router paths are relative to `.tmp/9router` (git 17c4cc76).

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

// ---------------------------------------------------------------------------
// Finding 1 — Endpoint page: Tailscale connect / auth-wait state
// ---------------------------------------------------------------------------

const ENDPOINT: &str = "web/src/components/EndpointPageClient.tsx";

/// The auth URL and its label travel together, so the wait screen can offer a
/// button that re-opens the popup the connect flow is blocked on.
#[test]
fn tailscale_auth_label_is_live_state() {
    let src = read_src("components/EndpointPageClient.tsx");

    assert_contains(
        &src,
        "const [tsAuthLabel, setTsAuthLabel] = useState",
        ENDPOINT,
        "9router carries the auth button's label in state (EndpointPageClient.js:485-492); \
         the port had a `useRef` that was written nowhere and read nowhere",
    );
    assert_not_contains(
        &src,
        "tsAuthLabelRef",
        ENDPOINT,
        "the ref was dead state — the label it held never reached the button",
    );
    assert_contains(
        &src,
        "requestUserAuth(data.authUrl, \"Open Login Page\")",
        ENDPOINT,
        "9router labels the Tailscale login handoff (EndpointPageClient.js:516-517)",
    );
    assert_contains(
        &src,
        "requestUserAuth(enableUrl, \"Open Funnel Settings\")",
        ENDPOINT,
        "the Funnel handoff is a different action and needs its own label \
         (EndpointPageClient.js:566-567)",
    );
}

/// The button must live in the branch that is actually rendered while the
/// connect flow is parked, and the flow must clear its own wait state.
#[test]
fn tailscale_auth_button_is_rendered_during_the_wait() {
    let src = read_src("components/EndpointPageClient.tsx");

    let wait_start = src
        .find(") : (tsLoading || tsConnecting) ? (")
        .unwrap_or_else(|| panic!("{ENDPOINT} must branch on the connect-in-flight state"));
    let wait_end = wait_start
        + src[wait_start..]
            .find(") : tsStatus?.type === \"error\" ? (")
            .expect("the error branch must follow the connect-in-flight branch");
    let wait_branch = &src[wait_start..wait_end];

    assert_contains(
        wait_branch,
        "{tsAuthUrl && (",
        ENDPOINT,
        "9router renders the auth button inside the (tsLoading || tsConnecting) branch \
         (EndpointPageClient.js:872-880); the port had it in the !tsReachable branch, which \
         is unreachable while the flow is in flight",
    );
    assert_contains(
        wait_branch,
        "{tsAuthLabel || \"Open\"}",
        ENDPOINT,
        "the button is labelled by the state the flow set",
    );

    // The old guard could never be false: both the login and the funnel path set
    // tsAuthUrl first, so progress was never cleared on completion.
    assert_not_contains(
        &src,
        "if (!tsAuthUrl) setTsProgress(\"\")",
        ENDPOINT,
        "tsAuthUrl is always set by the time the finally runs, so this guard is always false \
         and the progress line leaked into the settled state",
    );

    // Anchor on the tailscale cleanup specifically — the file has several
    // unrelated `finally` blocks.
    let finally_start = src
        .find("setTsConnecting(false);\n      setTsProgress(\"\");")
        .map(|i| i + "setTsConnecting(false);\n      ".len())
        .unwrap_or_else(|| panic!("{ENDPOINT} must clear its connect state in a finally block"));
    let cleanup = &src[finally_start..finally_start + 120];
    assert_contains(
        cleanup,
        "setTsProgress(\"\")",
        ENDPOINT,
        "9router clears the progress line unconditionally (EndpointPageClient.js:556-563)",
    );
    assert_contains(
        cleanup,
        "clearUserAuth()",
        ENDPOINT,
        "and clears the auth URL with it, so a finished flow leaves no stale popup link",
    );
}

// ---------------------------------------------------------------------------
// Finding 16 (web half) — the Require API key toggle's read default
// ---------------------------------------------------------------------------

/// A settings blob written before the field existed must read as 9router's
/// default (`true`), not as `false`.
#[test]
fn require_api_key_reads_the_nine_router_default() {
    let src = read_src("components/EndpointPageClient.tsx");

    assert_contains(
        &src,
        "setRequireApiKey(data.requireApiKey ?? true)",
        ENDPOINT,
        "9router defaults requireApiKey to true (settingsRepo.js:26-27); `|| false` read a \
         pre-field blob as \"no key required\" and silently opened /v1",
    );
    assert_contains(
        &src,
        "body: JSON.stringify({ requireApiKey: value })",
        ENDPOINT,
        "the toggle PATCHes the camelCase field the settings API accepts",
    );
}

// ---------------------------------------------------------------------------
// Finding 2 — CLI-tool preset stores
// ---------------------------------------------------------------------------

const PRESETS: &str = "web/src/components/cli-tools/cliEndpointPresets.ts";

/// Both preset stores exist, are separate, and announce their writes.
#[test]
fn api_key_and_endpoint_preset_stores_are_separate() {
    let src = read_src("components/cli-tools/cliEndpointPresets.ts");

    assert_contains(
        &src,
        "openproxy.cliToolEndpointPresets",
        PRESETS,
        "the endpoint store keeps its own key (9router cliEndpointPresets.js:52-60)",
    );
    assert_contains(
        &src,
        "openproxy.cliToolApiKeyPresets",
        PRESETS,
        "9router has a second store for saved API keys (cliEndpointPresets.js:62-66); the \
         port had none, so ApiKeySelect could not save a key at all",
    );
    assert_contains(
        &src,
        "window.dispatchEvent(new CustomEvent(changeEvent))",
        PRESETS,
        "without the change event two mounted cards cannot see each other's writes \
         (cliEndpointPresets.js:19)",
    );
    assert_contains(
        &src,
        "subscribe: (handler)",
        PRESETS,
        "the store exposes the subscribe half of the event (cliEndpointPresets.js:24-28)",
    );
}

/// The shape conflict that silently dropped entries is gone: the endpoint key
/// lives in exactly one file, and the two stores keep their own record shape.
#[test]
fn no_two_components_share_one_storage_key_with_different_shapes() {
    let endpoints = read_src("components/cli-tools/cliEndpointPresets.ts");

    for component in [
        "components/cli-tools/ApiKeySelect.tsx",
        "components/cli-tools/BaseUrlSelect.tsx",
        "components/cli-tools/EndpointPresetControl.tsx",
    ] {
        assert_not_contains(
            &read_src(component),
            "openproxy.cliToolEndpointPresets",
            component,
            "the endpoint store's key belongs to cliEndpointPresets.ts alone; a second writer \
             silently dropped whichever record shape it did not expect",
        );
    }

    // EndpointPresetControl pairs a URL with a key. It must join the two
    // canonical stores on the shared name rather than widening either one.
    let control = read_src("components/cli-tools/EndpointPresetControl.tsx");
    assert_contains(
        &control,
        "upsertKeyPreset(apiKey, name)",
        "web/src/components/cli-tools/EndpointPresetControl.tsx",
        "the paired save writes both halves through the two stores",
    );
    assert_contains(
        &control,
        "deleteKeyPreset(name)",
        "web/src/components/cli-tools/EndpointPresetControl.tsx",
        "and a paired delete removes both halves",
    );
    assert_not_contains(
        &endpoints,
        "apiKey: string;",
        PRESETS,
        "the endpoint record must not grow the api-key store's field",
    );
}

/// The API-key picker can save and delete a key, like 9router's.
#[test]
fn api_key_picker_supports_saving_and_deleting() {
    let src = read_src("components/cli-tools/ApiKeySelect.tsx");

    assert_contains(
        &src,
        "if (next === SAVE_VALUE) {",
        "web/src/components/cli-tools/ApiKeySelect.tsx",
        "9router's ApiKeySelect saves the typed key under a name (ApiKeySelect.js:41-43)",
    );
    assert_contains(
        &src,
        "upsertKeyPreset(",
        "web/src/components/cli-tools/ApiKeySelect.tsx",
        "…through the shared key store",
    );
    assert_contains(
        &src,
        "deleteKeyPreset(",
        "web/src/components/cli-tools/ApiKeySelect.tsx",
        "and can delete a saved key again",
    );
}

// ---------------------------------------------------------------------------
// Finding 6 — /dashboard/media-providers/webSearch and /webFetch
// ---------------------------------------------------------------------------

/// The alias kinds are folded into `web` without pushing a history entry, so the
/// back button skips them the way 9router's `router.replace` does.
#[test]
fn web_kind_alias_replaces_history() {
    let src = read_src("components/MediaProvidersKindPageClient.tsx");

    assert_contains(
        &src,
        "window.history.replaceState(null, \"\", \"/dashboard/media-providers/web\")",
        "web/src/components/MediaProvidersKindPageClient.tsx",
        "9router uses router.replace (media-providers/[kind]/page.js:149-153); the port used \
         window.location.href, a full document load that also pushed a history entry",
    );
    assert_not_contains(
        &src,
        "window.location.href = \"/dashboard/media-providers/web\"",
        "web/src/components/MediaProvidersKindPageClient.tsx",
        "the alias is not a navigation, it is a rewrite",
    );
    assert_contains(
        &src,
        "setKind(\"web\")",
        "web/src/components/MediaProvidersKindPageClient.tsx",
        "the rewritten URL must render the web kind without waiting for a reload",
    );
}

// ---------------------------------------------------------------------------
// Finding 10 — Add/Edit Connection: the Region control is registry-driven
// ---------------------------------------------------------------------------

const ADD_MODAL: &str = "web/src/components/providers/AddApiKeyModal.tsx";
const EDIT_MODAL: &str = "web/src/shared/components/EditConnectionModal.tsx";

/// Region is data on the provider entry, not a hard-coded Xiaomi special case.
#[test]
fn region_select_is_registry_driven() {
    let src = read_src("components/providers/AddApiKeyModal.tsx");

    assert_contains(
        &src,
        "AI_PROVIDERS[provider]",
        ADD_MODAL,
        "9router reads the region list off the provider entry \
         (AddApiKeyModal.js:23-24); the port hard-coded three Xiaomi options and left every \
         other region-aware provider with no way to set a region",
    );
    assert_contains(
        &src,
        "providerRegions.map((r) => ({ value: r.id, label: r.label }))",
        ADD_MODAL,
        "the options come from the registry (AddApiKeyModal.js:287-294)",
    );
    assert_not_contains(
        &src,
        "isXiaomiTokenplan",
        ADD_MODAL,
        "a per-provider special case is exactly what the registry replaces",
    );
    assert_contains(
        &src,
        "if (providerRegions && region) {",
        ADD_MODAL,
        "the region is persisted only when the provider declares one (AddApiKeyModal.js:70-72)",
    );
}

/// Edit can change the region Add set, and falls back to the registry default
/// when the stored value is no longer offered.
#[test]
fn edit_connection_modal_has_a_region_control() {
    let src = read_src("shared/components/EditConnectionModal.tsx");

    assert_contains(
        &src,
        "connection.providerSpecificData?.region",
        EDIT_MODAL,
        "the saved region is read back out of providerSpecificData \
         (9router EditConnectionModal.js:53-58)",
    );
    assert_contains(
        &src,
        "label=\"Region\"",
        EDIT_MODAL,
        "Edit offers the same control Add does; without it a wrong region was \
         permanently unfixable",
    );
    assert_contains(
        &src,
        "updates.providerSpecificData = { region }",
        EDIT_MODAL,
        "and writes it back (9router EditConnectionModal.js:171-174)",
    );
    assert_contains(
        &src,
        "list.some((r) => r.id === saved) ? (saved as string) : fallback",
        EDIT_MODAL,
        "a region the registry no longer lists falls back to the default rather than \
         rendering an unselected control",
    );
}

/// Trae gained the three regions its executor can reach.
#[test]
fn trae_declares_its_regions() {
    let src = read_src("shared/constants/providers.ts");
    let trae = src
        .find("  trae: {")
        .map(|i| &src[i..i + 900])
        .expect("the trae registry entry must exist");

    assert_contains(
        trae,
        "regions: [{ id: \"cn\"",
        "web/src/shared/constants/providers.ts",
        "9router's trae registry declares cn/sg/us (open-sse/providers/registry/trae.js:41-47); \
         the port had no regions at all, so the Region control could never appear for it",
    );
    assert_contains(
        trae,
        "defaultRegion: \"cn\"",
        "web/src/shared/constants/providers.ts",
        "cn is trae's default region",
    );
}

// ---------------------------------------------------------------------------
// Finding 3 + 40 — /dashboard/providers/new
// ---------------------------------------------------------------------------

const PROVIDERS_NEW: &str = "web/src/components/ProvidersNewPageClient.tsx";

/// The create card is a real form, so Enter in either Input submits.
#[test]
fn create_provider_form_is_a_native_form() {
    let src = read_src("components/ProvidersNewPageClient.tsx");

    assert_contains(
        &src,
        "<form onSubmit={handleSubmit}",
        PROVIDERS_NEW,
        "9router wraps the card in a form (providers/new/page.js:94); the port had no form \
         element, so Enter in the name or key field submitted nothing",
    );
    assert_contains(
        &src,
        "e.preventDefault();",
        PROVIDERS_NEW,
        "…and suppresses the native navigation while handleConnect runs",
    );
    assert_contains(
        &src,
        "type=\"submit\"",
        PROVIDERS_NEW,
        "the primary action is the form's submit button (providers/new/page.js:211), not an \
         onClick that only fires on a pointer",
    );
    assert_not_contains(
        &src,
        "onClick={() => void handleConnect()}",
        PROVIDERS_NEW,
        "the submit button must not also carry its own click handler — one action, one path",
    );
}

/// The provider picker is the single native control 9router renders.
#[test]
fn provider_picker_is_a_single_focusable_control() {
    let src = read_src("components/ProvidersNewPageClient.tsx");

    assert_not_contains(
        &src,
        "searchable",
        PROVIDERS_NEW,
        "9router's Select renders one native <select> (shared/components/Select.js:19-55); the \
         searchable prop added a second, unlinked text input above it, so the whole \
         \"choose a provider\" interaction took two tab stops",
    );
    assert_not_contains(
        &src,
        "searchPlaceholder",
        PROVIDERS_NEW,
        "same extra input, by another name",
    );
}

// ---------------------------------------------------------------------------
// Finding 30 — Providers page batch test
// ---------------------------------------------------------------------------

const PROVIDERS_PAGE: &str = "web/src/components/providers/ProvidersPageClient.tsx";

/// No client-side cross-mode chain, and no button spins for a mode no button sets.
#[test]
fn batch_test_has_no_dead_global_chain() {
    let src = read_src("components/providers/ProvidersPageClient.tsx");

    assert_not_contains(
        &src,
        "mode === \"all\"",
        PROVIDERS_PAGE,
        "the only three call sites pass \"oauth\", \"free\" and \"apikey\"; the chain branch was \
         unreachable and its `setTestingMode(\"all\")` could never fire",
    );
    assert_not_contains(
        &src,
        "mode === \"global\"",
        PROVIDERS_PAGE,
        "same dead branch",
    );
    assert_not_contains(
        &src,
        "|| testingMode === \"all\"",
        PROVIDERS_PAGE,
        "9router spins a section's Test All button only while its own mode runs \
         (providers/page.js:241-264); the `|| \"all\"` half widened three spinners for a state \
         no button could set",
    );
    assert_contains(
        &src,
        "setTestingMode(mode === \"provider\" ? providerId : mode)",
        PROVIDERS_PAGE,
        "handleBatchTest drives exactly the mode it was handed",
    );
}

// ---------------------------------------------------------------------------
// Finding 14 + 12 (web half) — useModelCaps
// ---------------------------------------------------------------------------

const MODEL_CAPS: &str = "web/src/shared/hooks/useModelCaps.ts";

/// One /api/models fetch is shared by every consumer, and a custom-model change
/// invalidates it.
#[test]
fn model_caps_cache_and_invalidation_are_restored() {
    let src = read_src("shared/hooks/useModelCaps.ts");

    assert_contains(
        &src,
        "let cache: CapsMaps | null = null;",
        MODEL_CAPS,
        "9router keeps one module-level cache (useModelCaps.js:7-8); the port refetched \
         /api/models per mount across five consumers",
    );
    assert_contains(
        &src,
        "let inflight: Promise<CapsMaps> | null = null;",
        MODEL_CAPS,
        "and dedupes concurrent mounts into one request (useModelCaps.js:22-38)",
    );
    assert_contains(
        &src,
        "if (inflight) return inflight;",
        MODEL_CAPS,
        "the second mount joins the in-flight request instead of starting another",
    );
    assert_contains(
        &src,
        "window.addEventListener(\"customModelChanged\", invalidate)",
        MODEL_CAPS,
        "8 sites dispatch customModelChanged; without a listener the cache went stale after \
         any model edit (useModelCaps.js:71-80)",
    );
    assert_contains(
        &src,
        "window.removeEventListener(\"customModelChanged\", invalidate)",
        MODEL_CAPS,
        "and the listener is removed on unmount",
    );
    assert_contains(
        &src,
        "useState<Record<string, ModelCaps>>(() => cache?.byFull || {})",
        MODEL_CAPS,
        "an already-warm cache paints synchronously instead of after a fetch \
         (useModelCaps.js:58)",
    );
}

/// A routed model is addressable by `providerAlias/model`, the way a caller that
/// already resolved the alias looks it up.
#[test]
fn model_caps_index_routed_models() {
    let src = read_src("shared/hooks/useModelCaps.ts");

    assert_contains(
        &src,
        "routedModel?: string;",
        MODEL_CAPS,
        "the /api/models entry carries a routedModel (9router api/models/route.js:22-37)",
    );
    assert_contains(
        &src,
        "if (m.routedModel) byFull[m.routedModel] = caps;",
        MODEL_CAPS,
        "9router indexes caps under it (useModelCaps.js:15-16); without the branch a client \
         holding an aliased model id got no badges at all",
    );
}

// ---------------------------------------------------------------------------
// Finding 32 — ModelSelectModal chips (core surface #4)
// ---------------------------------------------------------------------------

const MODEL_SELECT: &str = "web/src/shared/components/ModelSelectModal.tsx";

/// One native button per model: one tab stop, free Enter/Space, announced as a
/// button, and no interactive content nested inside it.
#[test]
fn model_chips_are_native_buttons() {
    let src = read_src("shared/components/ModelSelectModal.tsx");

    assert_not_contains(
        &src,
        "role=\"listbox\"",
        MODEL_SELECT,
        "the container claimed the listbox pattern but implemented no arrow-key navigation, \
         so the role promised an interaction the component did not have",
    );
    assert_not_contains(
        &src,
        "role=\"option\"",
        MODEL_SELECT,
        "9router renders a plain flex div of buttons (ModelSelectModal.js:578-618)",
    );
    assert_not_contains(
        &src,
        "aria-multiselectable",
        MODEL_SELECT,
        "a listbox that cannot be arrow-navigated cannot be multi-selectable either",
    );
    assert_contains(
        &src,
        "<button\n                      type=\"button\"\n                      onClick={rowClick}",
        MODEL_SELECT,
        "the chip is a native button: Enter and Space activate it without a key handler, \
         and it is announced as what it is",
    );
    assert_contains(
        &src,
        "aria-pressed={isMultiSelected || isSingleSelected}",
        MODEL_SELECT,
        "selection state stays exposed now that role=option is gone",
    );
}

/// A native button must not contain interactive descendants, so the star and
/// the checkbox are siblings of the chip rather than children.
#[test]
fn model_chip_controls_are_siblings_not_descendants() {
    let src = read_src("shared/components/ModelSelectModal.tsx");

    let row_start = src
        .find("<div className=\"inline-flex items-center gap-0.5\" key={model.value}>")
        .expect("the chip row wrapper must exist");
    let chip_start = src[row_start..]
        .find(
            "<button\n                    type=\"button\"\n                    onClick={rowClick}",
        )
        .map(|i| row_start + i)
        .expect("the chip button must follow the star and the checkbox");

    let before_chip = &src[row_start..chip_start];
    assert_contains(
        before_chip,
        "toggleFavorite(favAlias, model.id)",
        MODEL_SELECT,
        "the favourite toggle sits outside the chip button",
    );
    assert_contains(
        before_chip,
        "type=\"checkbox\"",
        MODEL_SELECT,
        "so does the multi-select checkbox — a button may not contain either",
    );
}

// ---------------------------------------------------------------------------
// Finding 17 — /callback
// ---------------------------------------------------------------------------

const CALLBACK: &str = "web/src/pages/callback.astro";

/// The relay page is a normal page, so the theme and i18n runtime apply.
#[test]
fn callback_page_uses_the_shared_layout() {
    let src = read_src("pages/callback.astro");

    assert_contains(
        &src,
        "import Layout from '@/layouts/Layout.astro'",
        CALLBACK,
        "9router's callback is a normal App Router page, inheriting layout.js (ThemeProvider, \
         RuntimeI18nProvider, the pre-paint theme script)",
    );
    assert_not_contains(
        &src,
        "<!doctype html>",
        CALLBACK,
        "the standalone document shell is what kept Layout from applying",
    );
    assert_not_contains(
        &src,
        "background: #0d0d0d",
        CALLBACK,
        "the hard-coded dark-only palette is replaced by the theme tokens",
    );
    assert_contains(
        &src,
        "</Layout>",
        CALLBACK,
        "the page closes through Layout",
    );
}

/// A callback that carried nothing to relay is exactly the case where the user
/// needs the URL back, so the copy block no longer depends on the relay having
/// failed.
#[test]
fn no_params_branch_always_shows_the_url() {
    let src = read_src("pages/callback.astro");

    assert_contains(
        &src,
        "if (!code && !error) {",
        CALLBACK,
        "9router renders the Copy This URL card whenever the callback carried no code, token \
         or error (callback/page.js:112-125)",
    );
    assert_not_contains(
        &src,
        "if (!sentByChannel && !sentByStorage && !sentByMessage) {",
        CALLBACK,
        "the relay flags were set on every path that reached the UI, so this guard was \
         effectively never true and the URL was never shown",
    );
    assert_contains(
        &src,
        "copyBtn.textContent = 'Copy URL'",
        CALLBACK,
        "with a copy affordance for the manual path",
    );
}

// ---------------------------------------------------------------------------
// Finding 21 — /dashboard/settings/pricing renders outside the dashboard chrome
// ---------------------------------------------------------------------------

const PRICING: &str = "web/src/pages/dashboard/settings/pricing.astro";

#[test]
fn pricing_page_renders_without_dashboard_chrome() {
    let src = read_src("pages/dashboard/settings/pricing.astro");

    assert_not_contains(
        &src,
        "DashboardLayout",
        PRICING,
        "9router places this route outside the (dashboard) group, so it draws no Sidebar and \
         no Header",
    );
    assert_contains(
        &src,
        "<PricingPageClient client:only=\"react\" />",
        PRICING,
        "…and the page supplies its own shell",
    );

    let client = read_src("components/PricingPageClient.tsx");
    assert_contains(
        &client,
        "max-w-6xl",
        "web/src/components/PricingPageClient.tsx",
        "the 9router route draws its own max-w-6xl shell (pricing/page.js:54), which is what \
         replaces the dashboard chrome",
    );
}

// ---------------------------------------------------------------------------
// Finding 18 — devin and opendesign are registered CLI tools
// ---------------------------------------------------------------------------

const CLI_TOOLS: &str = "web/src/shared/constants/cliTools.ts";

#[test]
fn devin_and_opendesign_are_registered_cli_tools() {
    let src = read_src("shared/constants/cliTools.ts");

    for (id, name) in [("devin", "Devin CLI"), ("opendesign", "OpenDesign")] {
        let start = src
            .find(&format!("  {id}: {{"))
            .unwrap_or_else(|| panic!("{CLI_TOOLS} must register the `{id}` CLI tool"));
        let entry = &src[start..(start + 1600).min(src.len())];

        assert_contains(
            entry,
            &format!("id: \"{id}\""),
            CLI_TOOLS,
            "the entry declares its own id",
        );
        assert_contains(
            entry,
            &format!("name: \"{name}\""),
            CLI_TOOLS,
            "9router's CLI_TOOLS carries both tools (cliTools.js:406, :432); without the entry \
             `getStaticPaths` emits no page and the route falls through to the endpoint shell",
        );
        assert_contains(
            entry,
            "configType: \"guide\"",
            CLI_TOOLS,
            "both are guide-configured: there is no env or custom config to write",
        );
        assert_contains(
            entry,
            "guideSteps:",
            CLI_TOOLS,
            "and the guide card renders the steps",
        );
        assert_contains(entry, "codeBlock:", CLI_TOOLS, "plus the install matrix");
    }
}

/// The guide card is the shared `default:` branch, so both tools render without
/// a bespoke card of their own.
#[test]
fn guide_tools_render_through_the_default_card() {
    let src = read_src("components/cli-tools/ToolDetailClient.tsx");

    assert_contains(
        &src,
        "return <DefaultToolCard",
        "web/src/components/cli-tools/ToolDetailClient.tsx",
        "an unlisted toolId falls through to DefaultToolCard, which renders configType \
         \"guide\" from guideSteps — so devin and opendesign need no new card component",
    );
}

// ---------------------------------------------------------------------------
// Findings 35 + 37 — /login
// ---------------------------------------------------------------------------

const LOGIN: &str = "web/src/components/LoginPageClient.tsx";

/// Only the submit button is disabled during a rate-limit lockout, so the
/// password field keeps focus and Enter still reaches the server.
#[test]
fn password_field_stays_focusable_during_lockout() {
    let src = read_src("components/LoginPageClient.tsx");

    assert_not_contains(
        &src,
        "if (retryAfter > 0) return;",
        LOGIN,
        "9router's handleLogin has no retryAfter guard (login/page.js:66-91): Enter still \
         submits, the server rejects it, and retryAfter is refreshed from the 429",
    );
    assert_not_contains(
        &src,
        "autoComplete=\"current-password\"\n                    disabled={retryAfter > 0}",
        LOGIN,
        "disabling the only credential input during a lockout strands a keyboard-only user \
         (9router login/page.js:225-232 never disables it)",
    );
    // The submit button keeps its own guard — that one is 9router parity.
    assert_contains(
        &src,
        "{retryAfter > 0 ? `Wait ${retryAfter}s` : \"Sign In\"}",
        LOGIN,
        "the submit button stays disabled during the lockout (login/page.js:241-244)",
    );
}

/// Uniform 16px padding, so the card sits on the vertical centre line.
#[test]
fn login_shell_uses_uniform_padding() {
    let src = read_src("components/LoginPageClient.tsx");

    assert_contains(
        &src,
        "min-h-screen flex items-center justify-center bg-canvas p-4 relative overflow-hidden",
        LOGIN,
        "9router uses uniform p-4 (login/page.js:153); px-4 py-12 pushed the card 16px below \
         centre at every viewport height",
    );
    assert_not_contains(&src, "px-4 py-12", LOGIN, "the asymmetric padding is gone");
}
