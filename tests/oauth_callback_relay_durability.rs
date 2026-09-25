//! Regression cover for bead `openproxy-0ror` finding 2 (P302-001) — the OAuth
//! callback page deleted its own localStorage relay after 1s while the
//! dashboard consumer tolerates staleness for 30s.
//!
//! ## The defect
//!
//! `web/src/pages/callback.astro` relayed the auth code to the dashboard over
//! three channels (BroadcastChannel, localStorage, postMessage) and then armed a
//! `setTimeout(..., 1000)` that removed the `oauth_callback` key. The consumer,
//! `web/src/shared/components/OAuthModal.tsx`, reads that key **at mount time**
//! and accepts it for 30 seconds:
//!
//! ```ts
//! if (data.timestamp && Date.now() - data.timestamp < 30000) {
//!   handleCallback(data);
//! }
//! ```
//!
//! The writer's 1s self-delete therefore races the consumer's 30s window: any
//! dashboard that mounts its OAuth listener later than one second after the
//! popup redirected — a slow tab, a reloaded dashboard, a device that had been
//! backgrounded — reads a key that is already gone and the user is stranded at
//! a spinner with no code to exchange.
//!
//! 9router has no writer-side removal at all; the **consumer** owns cleanup on
//! both of its paths (`.tmp/9router/src/shared/components/OAuthModal.js:554-566`
//! and the `storage` handler). OpenProxy's consumer already matches, so the fix
//! belongs entirely on the writer.
//!
//! ## Why a Rust test for a TypeScript defect
//!
//! The dashboard has no JS test runner (no vitest/jest in `web/package.json`),
//! so this follows the convention established in
//! `tests/available_models_disabled.rs`: read the sources as text and make
//! narrow source-contract assertions. These are deliberately narrow — one per
//! bead claim, no incidental formatting checks.

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

const CALLBACK: &str = "web/src/pages/callback.astro";
const MODAL: &str = "web/src/shared/components/OAuthModal.tsx";

/// The fix itself: the writer must not delete the key. The consumer's
/// 30s staleness window is meaningless if the key is gone after 1s.
#[test]
fn callback_leaves_the_key_for_the_consumer() {
    let src = read_src("pages/callback.astro");

    assert_contains(
        &src,
        "localStorage.setItem('oauth_callback'",
        CALLBACK,
        "the localStorage relay is the fallback path when BroadcastChannel and \
         postMessage both miss, and must still be written",
    );
    assert_not_contains(
        &src,
        "localStorage.removeItem",
        CALLBACK,
        "the writer must never delete the relay it just wrote — a 1s self-delete \
         races the consumer's 30s mount-time window and strands the user with no \
         auth code. Cleanup belongs to the consumer (9router \
         callback/page.js has no removeItem on the writer side either)",
    );
}

/// The payload must keep its timestamp, or the consumer's staleness check
/// becomes a no-op and a stale key is replayed on a later mount.
#[test]
fn stored_payload_carries_a_timestamp() {
    let src = read_src("pages/callback.astro");

    assert_contains(
        &src,
        "timestamp: Date.now(),",
        CALLBACK,
        "the consumer gates the mount-time pickup on `data.timestamp` being \
         present and recent; without it a durable key would be replayed forever",
    );
}

/// The "no unbounded localStorage growth" half of the fix. A writer that keeps
/// the key forever is only safe because the consumer removes it — on the
/// `storage` event and again on the mount-time read.
#[test]
fn consumer_still_claims_and_clears_the_key() {
    let src = read_src("shared/components/OAuthModal.tsx");

    assert_contains(
        &src,
        "localStorage.getItem(\"oauth_callback\")",
        MODAL,
        "the mount-time pickup is the whole reason the writer must leave the key \
         in place",
    );
    assert_contains(
        &src,
        "Date.now() - data.timestamp < 30000",
        MODAL,
        "9router tolerates a relay up to 30s old before the mount-time pickup; \
         that window is only meaningful while the writer leaves the key",
    );
    assert_contains(
        &src,
        "localStorage.removeItem(\"oauth_callback\")",
        MODAL,
        "the consumer owns cleanup — without it a durable writer key would \
         accumulate in localStorage forever",
    );
}
