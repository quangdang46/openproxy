//! Source-contract cover for bead `openproxy-vb3m` — the dashboard shell's
//! first-paint state, the part of the bead's "loading UX" tail that the
//! first-paint pathname fix left behind.
//!
//! ## Why a Rust test for a TypeScript defect
//!
//! The dashboard has no JS test runner (no vitest/jest in `web/package.json`),
//! so this follows the convention established in
//! `tests/dashboard_chrome_parity.rs` and `tests/login_autofocus_parity.rs`:
//! read the source as text and make narrow source-contract assertions.
//!
//! ## The finding
//!
//! `Sidebar.tsx` and `Header.tsx` both seed `pathname` with a lazy
//! `useState(() => window.location.pathname)` so `isActive` / `getPageInfo`
//! resolve on the very first client render. `DashboardLayout.tsx` was left on
//! the older `useState("")` + mount-effect shape, and it is the one file where
//! that costs more than a blank frame: `pathname` is the `<Header key={...}>`
//! prop and the `/dashboard/basic-chat` layout switch (see the file at :125-127).
//!
//! With the empty seed the key is `""` on render 1 and the real path on render
//! 2, so React unmounts the Header island it just mounted and mounts a second
//! one — re-running its `/api/auth/status` fetch and repainting the header a
//! frame after the SSR skeleton has already faded out, which is the exact
//! "menu jump" `#dashboard-skeleton` exists to hide. The same one-frame `""`
//! also renders basic-chat inside the padded, `max-w-7xl mx-auto` shell and
//! then snaps it to the flush full-height one.
//!
//! 9router is canonical here: `DashboardLayout.js:34` reads
//! `const pathname = usePathname();`, which Next.js resolves before the first
//! client render, so the Header's key never changes and the layout switch is
//! right on paint one.

use std::path::PathBuf;

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

fn assert_not_contains(haystack: &str, needle: &str, file: &str, why: &str) {
    assert!(
        !haystack.contains(needle),
        "{file} must NOT contain `{needle}` — {why}"
    );
}

/// Pull the whole `const [pathname, setPathname] = useState(...)` statement,
/// so an assertion about how the path is seeded cannot be satisfied by an
/// unrelated `useState` in the same file.
fn pathname_state(src: &str, file: &str) -> String {
    let from = src
        .find("const [pathname, setPathname] = useState(")
        .unwrap_or_else(|| panic!("{file} declares no `pathname` state"));
    let rest = &src[from..];
    let to = rest
        .find(");")
        .unwrap_or_else(|| panic!("{file}: unterminated `pathname` state declaration"));
    rest[..=to].to_string()
}

/// Every shell file that resolves the current route must do it lazily, so the
/// first client render already has the real path. Fails if any of the three
/// reverts to the `useState("")` + mount-effect shape.
#[test]
fn shell_pathname_is_seeded_before_the_first_paint() {
    for (file, label) in [
        ("src/shared/components/Sidebar.tsx", "Sidebar"),
        ("src/shared/components/Header.tsx", "Header"),
        (
            "src/shared/components/layouts/DashboardLayout.tsx",
            "DashboardLayout",
        ),
    ] {
        let src = read_web(file);
        let decl = pathname_state(&src, file);
        assert!(
            decl.contains("useState(() =>"),
            "{file} must seed `pathname` with a lazy initialiser — {label} reads \
             it on its first render, and a `useState(\"\")` seed leaves that render \
             without a path (9router reads `usePathname()`, resolved pre-paint).\nfound: {decl}"
        );
        assert_contains(
            &decl,
            "window.location.pathname",
            file,
            "the lazy initialiser is what supplies the real path",
        );
        assert_not_contains(
            &decl,
            "useState(\"\")",
            file,
            &format!(
                "the empty-string seed is exactly the shape that makes {label} \
                 paint once against no path"
            ),
        );
    }
}

/// The layout hands `pathname` to the Header as a React `key`, so a first render
/// of `""` does not merely blank a frame — it tears down the Header island and
/// mounts a second one, re-running its auth-status fetch and repainting the bar
/// after `#dashboard-skeleton` has already faded. Pin both the key and the
/// basic-chat layout switch so the seed cannot be made lazy-but-unused.
#[test]
fn the_header_key_and_the_basic_chat_switch_read_the_seeded_path() {
    let file = "src/shared/components/layouts/DashboardLayout.tsx";
    let src = read_web(file);

    assert_contains(
        &src,
        "<Header key={pathname} onMenuClick={() => setSidebarOpen(true)} />",
        file,
        "DashboardLayout.js:112 keys the Header on the route; the key is only \
         stable if the seed is lazy",
    );

    let switches = src
        .matches("pathname === \"/dashboard/basic-chat\"")
        .count();
    assert_eq!(
        switches, 3,
        "{file} must keep the three basic-chat layout branches (the padding \
         switch, the column switch and the centring switch). Found {switches}."
    );

    // The Header itself must be free to key on a path that never changes after
    // mount, so it must not own a competing post-mount rewrite of the route.
    let header = read_web("src/shared/components/Header.tsx");
    let header_decl = pathname_state(&header, "src/shared/components/Header.tsx");
    assert!(
        header_decl.contains("useState(() =>"),
        "Header.tsx must keep its lazy pathname seed — the layout keys on it"
    );
}

/// `#dashboard-skeleton` is the server-rendered placeholder that hides the shell
/// until the layout mounts. It is only ever dismissed by `.dashboard-ready`,
/// which the layout adds in the same effect as the path seed, so the two must
/// stay in one mount effect — splitting them reopens the flash the skeleton
/// exists to prevent.
#[test]
fn the_skeleton_is_dismissed_by_the_same_mount_that_seeds_the_path() {
    let file = "src/shared/components/layouts/DashboardLayout.tsx";
    let src = read_web(file);
    let effect = src
        .split("useEffect(() => {")
        .nth(1)
        .and_then(|rest| rest.split("\n  }, []);").next())
        .unwrap_or_else(|| panic!("{file} has no `useEffect(..., [])` mount effect"));

    assert_contains(
        effect,
        "document.body.classList.add(\"dashboard-ready\")",
        file,
        "the mount effect is what fades #dashboard-skeleton out",
    );
    assert_contains(
        effect,
        "window.location.pathname",
        file,
        "and it is the same effect that re-reads the path after a client-side \
         navigation, so the two cannot drift apart",
    );
}
