//! Final source-contract cover for the remainder of bead `openproxy-vb3m`
//! (wave `w10`).
//!
//! The per-file suites landed by the earlier slots of this bead —
//! `dashboard_chrome_parity.rs`, `dashboard_pages_parity.rs`,
//! `dashboard_remainder_parity.rs` and `design_tokens_parity.rs` — already pin
//! each finding where it lands. What is left here is the finding that slot
//! could not close plus the two invariants none of those files can see,
//! because each one spans more than a single component.
//!
//! ## Why a Rust test for TypeScript defects
//!
//! The dashboard ships no JS test runner (`web/package.json` has no
//! vitest/jest), so this follows the convention established in
//! `tests/login_autofocus_parity.rs`: read `web/src` as text and assert
//! narrowly. 9router paths are relative to `.tmp/9router` (git 17c4cc76).

use std::path::PathBuf;

const ADD_API_KEY: &str = "web/src/components/providers/AddApiKeyModal.tsx";
const AVAILABLE_MODELS: &str = "web/src/shared/models/availableModels.ts";
const HEADER: &str = "web/src/shared/components/Header.tsx";
const SIDEBAR: &str = "web/src/shared/components/Sidebar.tsx";

fn web_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web")
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

/// Every `useEffect(() => { ... }, [deps])` call in `src`, as its own string.
/// Splitting the file up this way keeps a claim about *one* effect from being
/// satisfied by a line that merely lives somewhere else in the same component.
fn effects(src: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(i) = rest.find("useEffect(() => {") {
        rest = &rest[i..];
        let end = rest
            .find("\n  }, [")
            .and_then(|e| rest[e..].find("]);").map(|c| e + c + 3))
            .unwrap_or(rest.len());
        out.push(&rest[..end]);
        rest = &rest[end..];
    }
    out
}

/// Every `href: "/dashboard/..."` literal in `src`, in source order.
fn dashboard_hrefs(src: &str) -> Vec<String> {
    const PREFIX: &str = "href: \"/dashboard/";
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(i) = rest.find(PREFIX) {
        rest = &rest[i + PREFIX.len()..];
        let Some(end) = rest.find('"') else { break };
        out.push(format!("/dashboard/{}", &rest[..end]));
        rest = &rest[end..];
    }
    out
}

// ---------------------------------------------------------------------------
// Finding 10 — Add/Edit Connection modals: the Region field
// ---------------------------------------------------------------------------

/// One `<AddApiKeyModal>` instance serves every provider (the parent swaps
/// `provider={addProviderId}` behind a single mount), so `useState` only ever
/// seeds the region for whichever provider was on screen at mount time. Left
/// alone, the second provider opened inherits the first one's region, and
/// `buildProviderSpecificData` gates on a truthy `region` — a stale value from
/// a different provider's cluster list is exactly as wrong as none at all, so
/// the field must follow the provider's own default (9router
/// AddApiKeyModal.js:41).
#[test]
fn region_select_reseeds_when_the_provider_changes() {
    let src = read_web("src/components/providers/AddApiKeyModal.tsx");

    let seed = effects(&src)
        .into_iter()
        .find(|e| e.contains("setRegion(defaultRegion)"))
        .unwrap_or_else(|| {
            panic!(
                "{ADD_API_KEY} must re-seed `region` from `defaultRegion` in an effect — \
                 the useState initialiser alone only fires for the provider that happened to \
                 be mounted first, so every later provider opens on a foreign or empty region"
            )
        });
    assert_contains(
        seed,
        "[defaultRegion]",
        ADD_API_KEY,
        "the re-seed must be keyed on the provider's own default, or it fires on every render",
    );
    assert_contains(
        &src,
        "const [region, setRegion] = useState<string>(defaultRegion);",
        ADD_API_KEY,
        "the provider's first-region default stays the initial value (9router AddApiKeyModal.js:41)",
    );
}

// ---------------------------------------------------------------------------
// AGENTS.md core surface #4 — the provider page and ModelSelectModal
// ---------------------------------------------------------------------------

/// The provider page and the shared model picker must read Available Models
/// from the same builder. AGENTS.md names them as one product surface: any
/// change to the disabled map, the custom rows or the catalog merge has to
/// land in both, and the only thing that keeps them honest is that neither
/// re-implements the merge.
#[test]
fn the_model_picker_and_the_provider_page_share_one_available_models_builder() {
    let models = read_web("src/shared/models/availableModels.ts");
    assert_contains(
        &models,
        "export function buildAvailableModels(",
        AVAILABLE_MODELS,
        "the single merge of disabled map + custom rows + catalog",
    );
    assert_contains(
        &models,
        "export function useAvailableModels(",
        AVAILABLE_MODELS,
        "the provider page's entry point",
    );

    let hook = models
        .find("export function useAvailableModels(")
        .expect("useAvailableModels is exported");
    assert!(
        models[hook..].contains("buildAvailableModels({"),
        "{AVAILABLE_MODELS}: useAvailableModels must delegate to buildAvailableModels — \
         re-deriving the rows inside the hook is how the provider page and the model picker \
         drift apart"
    );

    for (file, surface) in [
        (
            "src/components/providers/ProviderDetailPageClient.tsx",
            "the provider page",
        ),
        (
            "src/shared/components/ModelSelectModal.tsx",
            "the shared model picker",
        ),
    ] {
        let src = read_web(file);
        assert_contains(
            &src,
            "from \"@/shared/models/availableModels\"",
            file,
            &format!("{surface} must read Available Models from the shared builder"),
        );
    }
}

// ---------------------------------------------------------------------------
// Dead-end states — a nav-reachable page never renders a blank header
// ---------------------------------------------------------------------------

/// `getPageInfo` falls through to an empty title, and the header renders that
/// empty string as its own `<h1>`. Every route the sidebar can actually send
/// you to therefore needs a branch, or the page opens nameless.
#[test]
fn every_nav_reachable_route_has_a_header_title() {
    let sidebar = read_web("src/shared/components/Sidebar.tsx");
    let header = read_web("src/shared/components/Header.tsx");

    let routes = dashboard_hrefs(&sidebar);
    assert!(
        routes.len() >= 10,
        "expected the nav to expose its routes; parsed {routes:?} from {SIDEBAR}"
    );

    for route in routes {
        if route.starts_with("/dashboard/media-providers") {
            // The media tree is keyed by the [kind] regex, not an includes().
            assert_contains(
                &header,
                "mediaKindMatch",
                HEADER,
                &format!("{route} is reachable from the sidebar but has no getPageInfo branch"),
            );
            continue;
        }
        let segment = route.rsplit('/').next().unwrap_or_default();
        assert_contains(
            &header,
            &format!("pathname.includes(\"/{segment}\")"),
            HEADER,
            &format!(
                "{route} is reachable from the sidebar but getPageInfo has no branch for it, \
                 so the page header renders blank"
            ),
        );
    }
}
