//! Regression cover for bead `openproxy-0ror` finding 1 (P155-001) — enabling
//! OIDC from Settings did not require a client secret.
//!
//! ## The defect
//!
//! `web/src/components/ProfilePageClient.tsx` gated the save on
//!
//! ```tsx
//! if (authMode !== "password" && (!issuerUrl || !clientId) && !settings.oidcConfigured) {
//! ```
//!
//! 9router gates on the secret too
//! (`.tmp/9router/src/app/(dashboard)/dashboard/profile/page.js:354`):
//!
//! ```tsx
//! if (authMode !== "password" && (!issuerUrl || !clientId || !secret) && !settings.oidcConfigured) {
//! ```
//!
//! The surrounding code was otherwise intact — the secret is still trimmed
//! (`:521`) and still attached to the payload only when non-blank (`:544`) —
//! so only the guard had lost its `|| !secret` term.
//!
//! Why the gap mattered: the backend's configured flag is
//! `Settings::is_oidc_configured()` (`src/types/mod.rs:845-849`), i.e.
//! `issuer && clientId && secret`. A first-time enable with a blank secret
//! therefore leaves `oidcConfigured` false, so the guard is the *only* thing
//! between the operator and a half-configured OIDC provider that is advertised
//! as enabled but can never complete a token exchange. The write is silently
//! dropped server-side rather than rejected (`src/server/api/mod.rs:2524-2530`,
//! "empty/blank secret means keep existing"), so the UI reported success while
//! the provider stayed unusable.
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

const PROFILE: &str = "web/src/components/ProfilePageClient.tsx";

/// The fix itself: match 9router's `|| !secret` guard term and its message.
/// Fails before the fix, passes after.
#[test]
fn oidc_enable_requires_client_secret() {
    let src = read_src("components/ProfilePageClient.tsx");

    assert_contains(
        &src,
        "(!issuerUrl || !clientId || !secret) && !settings.oidcConfigured",
        PROFILE,
        "9router requires the secret to enable OIDC (profile/page.js:354); without \
         this term a first-time enable with a blank secret writes a provider that \
         can never complete a token exchange",
    );
    assert_contains(
        &src,
        "Issuer URL, client ID, and client secret are required to enable OIDC.",
        PROFILE,
        "the error must name the field that is actually missing, or the operator \
         fills in issuer and client ID again and gets the same rejection",
    );
    assert_not_contains(
        &src,
        "Issuer URL and client ID are required to enable OIDC.",
        PROFILE,
        "the secret-less message is the pre-fix wording and no longer describes the guard",
    );
}

/// The regression guard on the fix itself. `!settings.oidcConfigured` is the
/// escape hatch that lets an already-configured provider be re-saved without
/// retyping its secret — 9router keeps it too, and the backend's blank-secret
/// rule (`src/server/api/mod.rs:2524-2530`) is built for exactly that re-save.
/// Making the secret unconditionally mandatory is the easy way to over-correct
/// this finding and would lock operators out of editing scopes or the login
/// label. Passes before and after; it exists to catch that regression.
#[test]
fn oidc_resave_without_secret_still_allowed_when_configured() {
    let src = read_src("components/ProfilePageClient.tsx");

    assert_contains(
        &src,
        "&& !settings.oidcConfigured)",
        PROFILE,
        "an already-configured provider must stay re-savable without retyping its \
         secret, otherwise editing scopes or the login label becomes impossible",
    );
    assert_contains(
        &src,
        "if (secret) {",
        PROFILE,
        "the secret stays write-only: a blank field must omit the key from the \
         payload so the backend keeps the stored value",
    );
}
