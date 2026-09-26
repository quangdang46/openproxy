//! Bead openproxy-4inl.11 — the `ls` noise-directory list.
//!
//! 9router's `LS_NOISE_DIRS` (open-sse/rtk/constants.js:21-29) carries 25
//! entries; OpenProxy carried 13, so `ls` output kept showing `.turbo/`,
//! `.tox/`, `coverage/` and a dozen other directories an operator never wants
//! in a file listing.

use openproxy::core::rtk::constants::LS_NOISE_DIRS;
use openproxy::core::rtk::filters::ls_impl;

/// The twelve entries 9router has and OpenProxy did not.
const ADDED: [&str; 12] = [
    ".turbo",
    ".vercel",
    ".pytest_cache",
    ".mypy_cache",
    ".tox",
    "env",
    "coverage",
    ".nyc_output",
    "Thumbs.db",
    ".vs",
    "*.egg-info",
    ".eggs",
];

#[test]
fn ls_noise_dirs_cover_the_9router_set() {
    for name in ADDED {
        assert!(
            LS_NOISE_DIRS.contains(&name),
            "{name} missing from LS_NOISE_DIRS"
        );
    }
    assert_eq!(LS_NOISE_DIRS.len(), 25);
    // "env" is the Python legacy virtualenv; ".env" is the dotenv file and
    // 9router keeps it out on purpose.
    assert!(LS_NOISE_DIRS.contains(&"env"));
    assert!(!LS_NOISE_DIRS.contains(&".env"));
}

#[test]
fn ls_noise_match_is_exact_not_glob() {
    // The consumer compares with `LS_NOISE_DIRS.contains(&parsed.2)`, the same
    // exact string compare as 9router's `LS_NOISE_DIRS.includes(parsed.name)`.
    // That is why `*.egg-info` is inert on BOTH sides — pinning it here so a
    // later "helpful" glob expansion is caught as the divergence it would be.
    assert!(LS_NOISE_DIRS.contains(&"build"));
    assert!(!LS_NOISE_DIRS.contains(&"project.egg-info"));
}

#[test]
fn ls_filter_skips_the_added_noise_dirs() {
    let input = concat!(
        "total 40\n",
        "drwxr-xr-x  6 user  staff  192 Sep 26 10:00 .turbo\n",
        "drwxr-xr-x  6 user  staff  192 Sep 26 10:00 .tox\n",
        "drwxr-xr-x  6 user  staff  192 Sep 26 10:00 coverage\n",
        "drwxr-xr-x  6 user  staff  192 Sep 26 10:00 .vs\n",
        "drwxr-xr-x  6 user  staff  192 Sep 26 10:00 src\n",
        "-rw-r--r--  1 user  staff  512 Sep 26 10:00 README.md\n",
    );

    let out = ls_impl(input);

    for skipped in [".turbo", ".tox", "coverage", ".vs"] {
        assert!(
            !out.contains(skipped),
            "{skipped} should have been filtered out, got:\n{out}"
        );
    }
    assert!(out.contains("src/"), "real directory was dropped:\n{out}");
    assert!(out.contains("README.md"), "real file was dropped:\n{out}");
}
