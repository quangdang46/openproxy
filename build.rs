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

/// Resolve the version the binary reports.
///
/// `CARGO_PKG_VERSION` alone is wrong for a tagged release: Cargo.toml is
/// bumped by hand, so v0.3.1 and v0.3.2 both shipped a binary that printed
/// and announced "0.3.0" — and that string is not cosmetic, it is served in
/// the A2A agent card and the MCP `serverInfo` handshake, so a connecting
/// agent was told the wrong version.
///
/// Precedence: an explicit CI tag, then the nearest git tag, then
/// CARGO_PKG_VERSION for a plain local build where none of those exist.
/// Emitted as OPENPROXY_VERSION so call sites read one name.
fn emit_version() {
    let from_tag = std::env::var("GITHUB_REF_NAME")
        .ok()
        .filter(|v| v.starts_with('v') && v.len() > 1)
        .or_else(|| {
            let out = std::process::Command::new("git")
                .args(["describe", "--tags", "--abbrev=0"])
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let tag = String::from_utf8(out.stdout).ok()?.trim().to_string();
            (!tag.is_empty()).then_some(tag)
        })
        .unwrap_or_else(|| format!("v{}", env!("CARGO_PKG_VERSION")));

    println!("cargo:rustc-env=OPENPROXY_VERSION={from_tag}");
    // Say so once, so a stale Cargo.toml is visible in the build log rather
    // than only in a user's bug report.
    if from_tag != format!("v{}", env!("CARGO_PKG_VERSION")) {
        println!(
            "cargo:warning=openproxy reporting version {from_tag} (Cargo.toml is {})",
            env!("CARGO_PKG_VERSION")
        );
    }
}

fn main() {
    emit_version();
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
