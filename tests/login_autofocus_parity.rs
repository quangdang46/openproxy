//! Regression cover for bead `openproxy-0ror` finding 3 (P304-001) — the login
//! page's password field used the wrong autofocus predicate.
//!
//! ## The defect
//!
//! `web/src/components/LoginPageClient.tsx` gated the password input on
//!
//! ```tsx
//! autoFocus={!ssoAvailable}
//! ```
//!
//! where `ssoAvailable = samlAvailable || oidcAvailable`. On a **SAML-only**
//! deployment `ssoAvailable` is true (SAML is the configured SSO provider) while
//! `oidcAvailable` is false — so the password field, which is the only
//! credential input left on screen, received no focus at all. 9router gates on
//! `!oidcAvailable` (`.tmp/9router/src/app/login/page.js:228`): the password box
//! takes initial focus unless OIDC specifically supersedes it with a redirect
//! to the provider.
//!
//! The distinction matters because the two SSO types are mutually exclusive
//! here — `activeSsoType` is a single value, so a SAML deployment never has
//! `oidcAvailable`. The only behaviour change is that SAML-only deployments
//! regain the password focus 9router gives them; on OIDC deployments and on
//! no-SSO deployments the two predicates already agree.
//!
//! ## Why a Rust test for a TypeScript defect
//!
//! The dashboard has no JS test runner (no vitest/jest in `web/package.json`),
//! so this follows the convention established in
//! `tests/available_models_disabled.rs`: read the source as text and make narrow
//! source-contract assertions. Deliberately narrow — no incidental formatting
//! checks.

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

const LOGIN: &str = "web/src/components/LoginPageClient.tsx";

/// The fix itself: match 9router's `autoFocus={!oidcAvailable}`.
#[test]
fn password_field_autofocuses_unless_oidc_is_available() {
    let src = read_src("components/LoginPageClient.tsx");

    assert_contains(
        &src,
        "autoFocus={!oidcAvailable}",
        LOGIN,
        "9router focuses the password field unless OIDC is the active provider \
         (login/page.js:228); SAML-only deployments otherwise get no focus on \
         the only credential input on screen",
    );
    assert_not_contains(
        &src,
        "autoFocus={!ssoAvailable}",
        LOGIN,
        "gating on the union suppresses the password field's focus for SAML-only \
         deployments, where ssoAvailable is true but oidcAvailable is false",
    );
}

/// The fix depends on this binding. An implementer who "cleans it up" as
/// unused — it has no other consumer — would break the build rather than the
/// behaviour, so it is asserted explicitly.
#[test]
fn oidc_available_binding_is_kept() {
    let src = read_src("components/LoginPageClient.tsx");

    assert_contains(
        &src,
        "const oidcAvailable = isSsoEnabled && activeSsoType === \"oidc\" && oidcConfigured;",
        LOGIN,
        "the autofocus predicate reads this binding, so removing it as unused \
         would fail the build rather than silently regress the SAML case",
    );
}
