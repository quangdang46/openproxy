//! Build script.
//!
//! When the `embed-web` feature is enabled (default for release builds), the
//! Astro static output at `web/dist/` is baked into the binary via
//! `rust-embed`. This script fails the build early with a clear message if
//! `web/dist/index.html` is missing, instead of producing a binary with an
//! empty dashboard.
//!
//! To intentionally build without the embedded UI (smaller binary, requires
//! `--dashboard-sidecar-url` or `--web-dir` at runtime):
//!     cargo build --release --no-default-features

fn main() {
    let embed_enabled = std::env::var("CARGO_FEATURE_EMBED_WEB").is_ok();
    if !embed_enabled {
        return;
    }

    let dist = std::path::Path::new("web/dist/index.html");
    if !dist.exists() {
        // `cargo:warning=` lines are printed without colour but are visible in
        // release builds. We also panic so the build actually fails.
        println!(
            "cargo:warning=web/dist/index.html is missing. \
             Build the dashboard first: (cd web && pnpm install --frozen-lockfile && pnpm run build)"
        );
        panic!(
            "web/dist not built. Run:\n  \
             (cd web && pnpm install --frozen-lockfile && pnpm run build)\n\
             Or build without the embedded UI:\n  \
             cargo build --release --no-default-features"
        );
    }

    // Trigger a rebuild whenever the embedded assets change. Without this,
    // editing `web/dist/...` won't invalidate the existing rust-embed cache
    // and the binary will keep serving stale assets.
    println!("cargo:rerun-if-changed=web/dist");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_EMBED_WEB");
}
