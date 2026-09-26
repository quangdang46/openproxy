//! Bead openproxy-4inl.9 — one Caveman level list for both pickers.
//!
//! 9router keeps `CAVEMAN_LEVELS` in a single constants module
//! (`src/app/(dashboard)/dashboard/endpoint/endpointConstants.js:19-26`) and
//! both the endpoint and token-saver pages read it. OpenProxy declared its own
//! copy in each page, and the endpoint page's copy had lost the three 文言文
//! entries — so those levels were unreachable from that page, and the two
//! lists had already drifted apart.
//!
//! ## Why a Rust test for a TypeScript defect
//!
//! The dashboard has no JS test runner (no vitest/jest in `web/package.json`),
//! so this follows the convention in `tests/dashboard_chrome_parity.rs` and
//! `tests/available_models_disabled.rs`: read the source and assert narrowly on
//! what was parsed out of it, not on incidental formatting.

use std::path::PathBuf;

fn read_web(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("web")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The `id: "…"` values of a level list, in source order.
fn level_ids(source: &str) -> Vec<String> {
    source
        .match_indices("id: \"")
        .map(|(at, _)| {
            let rest = &source[at + "id: \"".len()..];
            let end = rest.find('"').unwrap_or_else(|| {
                panic!("unterminated id literal at byte {at} in:\n{source}");
            });
            rest[..end].to_string()
        })
        .collect()
}

#[test]
fn caveman_levels_match_the_canonical_six() {
    let caveman = read_web("src/shared/constants/caveman.ts");

    assert_eq!(
        level_ids(&caveman),
        [
            "lite",
            "full",
            "ultra",
            "wenyan-lite",
            "wenyan",
            "wenyan-ultra"
        ],
        "9router endpointConstants.js:19-26 lists these six, in this order"
    );
    assert_eq!(
        caveman.matches("wenyan: true").count(),
        3,
        "exactly the three 文言文 entries carry the flag"
    );
    assert!(
        caveman.contains("WENYAN_LOCALES = [\"zh-CN\", \"zh-TW\"]"),
        "the two Chinese locales are what gate them:\n{caveman}"
    );
}

#[test]
fn the_shared_module_is_the_single_source() {
    // The drift guard: a future copy-paste that re-declares the list in a page
    // is exactly how the endpoint page lost the 文言文 entries in the first
    // place, so the literal must not reappear outside the shared module.
    for page in [
        "src/components/EndpointPageClient.tsx",
        "src/components/TokenSaverPageClient.tsx",
    ] {
        let source = read_web(page);
        assert!(
            !source.contains("const CAVEMAN_LEVELS"),
            "{page} must import CAVEMAN_LEVELS from @/shared/constants/caveman, \
             not re-declare it"
        );
        assert!(
            source.contains("from \"@/shared/constants/caveman\""),
            "{page} must read the levels from the shared module"
        );
    }
}

#[test]
fn both_pickers_gate_the_wenyan_levels_on_locale() {
    for page in [
        "src/components/EndpointPageClient.tsx",
        "src/components/TokenSaverPageClient.tsx",
    ] {
        let source = read_web(page);
        assert!(
            source.contains("WENYAN_LOCALES.includes(locale)"),
            "{page} must derive isWenyanLocale from WENYAN_LOCALES"
        );
        assert!(
            source.contains("CAVEMAN_LEVELS.filter((lvl) => !lvl.wenyan)"),
            "{page} must hide the 文言文 entries outside zh-CN/zh-TW"
        );
        assert!(
            source.contains("visibleCavemanLevels.map("),
            "{page} must render the filtered list, not the raw one"
        );
        assert!(
            source.contains("current?.wenyan && !isWenyanLocale"),
            "{page} must reset a persisted 文言文 level when the locale is not \
             Chinese, or the picker renders an unselected button"
        );
    }
}
