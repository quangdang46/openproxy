#!/usr/bin/env bash
# install.sh — one-shot installer for openproxy
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/quangdang46/openproxy/main/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/quangdang46/openproxy/main/install.sh | bash -s -- --easy-mode
#   curl -fsSL https://raw.githubusercontent.com/quangdang46/openproxy/main/install.sh | bash -s -- --version v0.1.0
#
# Flags:
#   --dest <path>          Install location. Default: ~/.local/bin
#   --system               Shortcut for --dest /usr/local/bin (may need sudo)
#   --version <vX.Y.Z>     Pin a specific release. Default: latest
#   --easy-mode            Append PATH export to ~/.bashrc / ~/.zshrc if needed
#   --verify               Run `openproxy --version` after install
#   --from-source          Skip release download, build from source via cargo
#   --quiet, -q            Suppress info logs
#   --uninstall            Remove the binary and any easy-mode PATH lines
#   -h, --help             Show this help and exit

set -euo pipefail
umask 022

# ════════════════════════════════════════════════════════════════════════════
# Configuration
# ════════════════════════════════════════════════════════════════════════════

BINARY_NAME="openproxy"
OWNER="quangdang46"
REPO="openproxy"

DEST="${DEST:-$HOME/.local/bin}"
VERSION="${VERSION:-}"
QUIET=0
EASY=0
VERIFY=0
FROM_SOURCE=0
UNINSTALL=0
MAX_RETRIES=3
DOWNLOAD_TIMEOUT=120
LOCK_DIR="/tmp/${BINARY_NAME}-install.lock.d"
TMP=""

# ════════════════════════════════════════════════════════════════════════════
# Logging
# ════════════════════════════════════════════════════════════════════════════

# ANSI helpers — only colour when stderr is a TTY.
if [ -t 2 ]; then
    C_RED=$'\033[31m'; C_GREEN=$'\033[32m'; C_YELLOW=$'\033[33m'
    C_CYAN=$'\033[36m'; C_RESET=$'\033[0m'
else
    C_RED=""; C_GREEN=""; C_YELLOW=""; C_CYAN=""; C_RESET=""
fi

log_info()    { [ "$QUIET" -eq 1 ] && return 0; printf '%s==>%s [%s] %s\n' "$C_CYAN" "$C_RESET" "$BINARY_NAME" "$*" >&2; }
log_warn()    { printf '%s!!%s [%s] %s\n' "$C_YELLOW" "$C_RESET" "$BINARY_NAME" "$*" >&2; }
log_success() { [ "$QUIET" -eq 1 ] && return 0; printf '%s✓%s %s\n' "$C_GREEN" "$C_RESET" "$*" >&2; }
die()         { printf '%sERROR:%s %s\n' "$C_RED" "$C_RESET" "$*" >&2; exit 1; }

# ════════════════════════════════════════════════════════════════════════════
# Cleanup & lock
# ════════════════════════════════════════════════════════════════════════════

cleanup() {
    [ -n "$TMP" ] && rm -rf "$TMP" 2>/dev/null || true
    rm -rf "$LOCK_DIR" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

acquire_lock() {
    if mkdir "$LOCK_DIR" 2>/dev/null; then
        echo $$ > "$LOCK_DIR/pid"
        return 0
    fi
    die "another install appears to be running. If stuck, remove: $LOCK_DIR"
}

# ════════════════════════════════════════════════════════════════════════════
# Help
# ════════════════════════════════════════════════════════════════════════════

usage() {
    # Print the leading comment block, stopping at the first line that isn't
    # a comment. Robust against future edits that move sections around.
    awk '
        NR == 1 { next }                          # skip shebang
        /^#/ { sub(/^# ?/, ""); print; next }
        { exit }
    ' "$0"
    exit 0
}

# ════════════════════════════════════════════════════════════════════════════
# Argument parsing — supports both `--flag value` and `--flag=value`
# ════════════════════════════════════════════════════════════════════════════

while [ $# -gt 0 ]; do
    case "$1" in
        --dest)        DEST="$2"; shift 2;;
        --dest=*)      DEST="${1#*=}"; shift;;
        --version)     VERSION="$2"; shift 2;;
        --version=*)   VERSION="${1#*=}"; shift;;
        --system)      DEST="/usr/local/bin"; shift;;
        --easy-mode)   EASY=1; shift;;
        --verify)      VERIFY=1; shift;;
        --from-source) FROM_SOURCE=1; shift;;
        --quiet|-q)    QUIET=1; shift;;
        --uninstall)   UNINSTALL=1; shift;;
        -h|--help)     usage;;
        *) log_warn "unknown argument: $1 (ignored)"; shift;;
    esac
done

# ════════════════════════════════════════════════════════════════════════════
# Uninstall
# ════════════════════════════════════════════════════════════════════════════

do_uninstall() {
    if [ -f "$DEST/$BINARY_NAME" ]; then
        rm -f "$DEST/$BINARY_NAME"
        log_success "removed $DEST/$BINARY_NAME"
    else
        log_warn "no binary at $DEST/$BINARY_NAME"
    fi
    # Remove the PATH lines we added under --easy-mode (tagged with the
    # installer marker comment).
    for rc in "$HOME/.bashrc" "$HOME/.zshrc"; do
        [ -f "$rc" ] || continue
        if grep -q "${BINARY_NAME} installer" "$rc"; then
            # Use a portable sed-i wrapper.
            tmp="${rc}.tmp.$$"
            grep -v "${BINARY_NAME} installer" "$rc" > "$tmp" && mv -f "$tmp" "$rc"
            log_success "cleaned PATH lines from $rc"
        fi
    done
    log_success "uninstalled"
    exit 0
}

[ "$UNINSTALL" -eq 1 ] && do_uninstall

# ════════════════════════════════════════════════════════════════════════════
# Platform detection
# ════════════════════════════════════════════════════════════════════════════

# Output asset suffix exactly as it appears in release filenames:
#   openproxy-vX.Y.Z-<suffix>.tar.gz
detect_platform() {
    local os arch
    case "$(uname -s)" in
        Linux*)  os="linux";;
        Darwin*) os="macos";;
        MINGW*|MSYS*|CYGWIN*)
            die "Windows is not yet supported as a prebuilt release. Try --from-source, or use the npm package: npm install -g @openprx/openproxy";;
        *) die "unsupported OS: $(uname -s)";;
    esac
    case "$(uname -m)" in
        x86_64|amd64)   arch="x86_64";;
        aarch64|arm64)  arch="aarch64";;
        *) die "unsupported arch: $(uname -m)";;
    esac
    printf '%s-%s' "$os" "$arch"
}

# ════════════════════════════════════════════════════════════════════════════
# Version resolution — GitHub API → redirect-trick fallback
# ════════════════════════════════════════════════════════════════════════════

resolve_version() {
    if [ -n "$VERSION" ]; then
        # Allow the caller to omit "v"
        case "$VERSION" in
            v*) ;;
            *)  VERSION="v$VERSION";;
        esac
        return 0
    fi

    # Primary: GitHub releases API.
    VERSION=$(curl -fsSL --connect-timeout 10 --max-time 30 \
        -H 'Accept: application/vnd.github.v3+json' \
        "https://api.github.com/repos/${OWNER}/${REPO}/releases/latest" 2>/dev/null \
      | grep -m1 '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/' || true)

    # Fallback: HEAD redirect of /releases/latest → /tag/<version>
    if ! [[ "$VERSION" =~ ^v[0-9] ]]; then
        VERSION=$(curl -fsSL -o /dev/null -w '%{url_effective}' \
            "https://github.com/${OWNER}/${REPO}/releases/latest" 2>/dev/null \
          | sed -E 's|.*/tag/||' || true)
    fi

    [[ "$VERSION" =~ ^v[0-9] ]] || die "could not resolve latest version. Pass --version vX.Y.Z to pin."
    log_info "latest version: $VERSION"
}

# ════════════════════════════════════════════════════════════════════════════
# Download with retry + resume + proxy support
# ════════════════════════════════════════════════════════════════════════════

download_file() {
    local url="$1" dest="$2"
    local partial="${dest}.part"
    local attempt=0

    # Choose progress mode: progress bar on TTY (and not --quiet), silent otherwise.
    local progress=(-sS)
    if [ "$QUIET" -eq 0 ] && [ -t 2 ]; then
        progress=(--progress-bar)
    fi

    while [ $attempt -lt $MAX_RETRIES ]; do
        attempt=$((attempt + 1))
        local resume_flags=()
        if [ -s "$partial" ]; then
            resume_flags=(--continue-at -)
        fi
        if curl -fL \
            --connect-timeout 30 \
            --max-time "$DOWNLOAD_TIMEOUT" \
            --retry 2 \
            "${progress[@]}" \
            "${resume_flags[@]}" \
            -o "$partial" "$url"
        then
            mv -f "$partial" "$dest"
            return 0
        fi
        if [ $attempt -lt $MAX_RETRIES ]; then
            log_warn "download attempt $attempt failed; retrying in 3s..."
            sleep 3
        fi
    done
    return 1
}

# ════════════════════════════════════════════════════════════════════════════
# Atomic install
# ════════════════════════════════════════════════════════════════════════════

install_binary_atomic() {
    local src="$1" dest="$2"
    local tmp="${dest}.tmp.$$"
    install -m 0755 "$src" "$tmp" || die "failed to write $tmp"
    mv -f "$tmp" "$dest" || { rm -f "$tmp"; die "failed to move into place"; }
}

# ════════════════════════════════════════════════════════════════════════════
# PATH update (opt-in via --easy-mode)
# ════════════════════════════════════════════════════════════════════════════

maybe_add_path() {
    case ":$PATH:" in
        *":$DEST:"*) return 0;;
    esac
    if [ "$EASY" -eq 1 ]; then
        local added=0
        for rc in "$HOME/.zshrc" "$HOME/.bashrc"; do
            [ -f "$rc" ] && [ -w "$rc" ] || continue
            if grep -qF "$DEST" "$rc"; then continue; fi
            printf '\nexport PATH="%s:$PATH"  # %s installer\n' "$DEST" "$BINARY_NAME" >> "$rc"
            log_success "added $DEST to PATH in $rc"
            added=1
        done
        if [ $added -eq 1 ]; then
            log_warn "open a new shell or run:  export PATH=\"$DEST:\$PATH\""
        fi
    else
        log_warn "$DEST is not on your PATH. Add this to your shell rc:"
        log_warn "  export PATH=\"$DEST:\$PATH\""
        log_warn "Or rerun with --easy-mode to update ~/.bashrc / ~/.zshrc automatically."
    fi
}

# ════════════════════════════════════════════════════════════════════════════
# Build from source
# ════════════════════════════════════════════════════════════════════════════

build_from_source() {
    command -v cargo >/dev/null || die "cargo not found. Install Rust: https://rustup.rs"
    command -v node  >/dev/null || die "node not found. Node 20+ required to build the dashboard."
    command -v pnpm  >/dev/null || die "pnpm not found. Install: npm i -g pnpm@10.33.2"
    command -v git   >/dev/null || die "git not found."

    log_info "cloning ${OWNER}/${REPO}"
    git clone --depth 1 "https://github.com/${OWNER}/${REPO}.git" "$TMP/src" >/dev/null 2>&1 \
        || die "git clone failed"

    log_info "building dashboard (pnpm)"
    (cd "$TMP/src/web" && pnpm install --frozen-lockfile && pnpm run build) \
        || die "dashboard build failed"

    log_info "building binary (cargo --release, this takes 2-5 minutes)"
    (cd "$TMP/src" && CARGO_TARGET_DIR="$TMP/target" cargo build --release --locked) \
        || die "cargo build failed"

    install_binary_atomic "$TMP/target/release/$BINARY_NAME" "$DEST/$BINARY_NAME"
}

# ════════════════════════════════════════════════════════════════════════════
# Main
# ════════════════════════════════════════════════════════════════════════════

main() {
    acquire_lock
    TMP=$(mktemp -d)
    mkdir -p "$DEST" || die "cannot create $DEST"

    if [ ! -w "$DEST" ]; then
        die "$DEST is not writable. Try --dest \$HOME/.local/bin or run with sudo."
    fi

    local platform
    platform=$(detect_platform)
    log_info "platform: $platform"
    log_info "destination: $DEST"

    if [ "$FROM_SOURCE" -eq 0 ]; then
        resolve_version
        local archive="${BINARY_NAME}-${VERSION}-${platform}.tar.gz"
        local base="https://github.com/${OWNER}/${REPO}/releases/download/${VERSION}"

        log_info "downloading ${archive}"
        if ! download_file "${base}/${archive}" "$TMP/$archive"; then
            log_warn "release download failed — falling back to building from source"
            build_from_source
        else
            # Verify checksum if the sidecar exists. If not, install anyway —
            # GitHub serves over HTTPS with strong TLS.
            if download_file "${base}/${archive}.sha256" "$TMP/checksum.sha256" 2>/dev/null; then
                local expected actual
                expected=$(awk '{print $1}' "$TMP/checksum.sha256")
                if command -v sha256sum >/dev/null; then
                    actual=$(sha256sum "$TMP/$archive" | awk '{print $1}')
                else
                    actual=$(shasum -a 256 "$TMP/$archive" | awk '{print $1}')
                fi
                if [ "$expected" != "$actual" ]; then
                    die "checksum mismatch for ${archive} — expected ${expected}, got ${actual}"
                fi
                log_info "checksum verified"
            else
                log_warn "no checksum file found at ${archive}.sha256 — skipping verification"
            fi

            tar -xzf "$TMP/$archive" -C "$TMP" || die "failed to extract ${archive}"

            # Locate the binary. The release tarball places it at the top level
            # next to LICENSE/README.md; tolerate one level of nesting just in case.
            local bin
            bin=$(find "$TMP" -maxdepth 3 -type f -name "$BINARY_NAME" -perm -u+x 2>/dev/null | head -1)
            [ -n "$bin" ] || die "$BINARY_NAME not found inside ${archive}"
            install_binary_atomic "$bin" "$DEST/$BINARY_NAME"
        fi
    else
        build_from_source
    fi

    maybe_add_path

    if [ "$VERIFY" -eq 1 ]; then
        log_info "running self-test: $DEST/$BINARY_NAME --version"
        "$DEST/$BINARY_NAME" --version || die "self-test failed"
    fi

    # Final summary.
    printf '\n'
    printf '%s✓%s %s installed → %s\n' "$C_GREEN" "$C_RESET" "$BINARY_NAME" "$DEST/$BINARY_NAME"
    if v=$("$DEST/$BINARY_NAME" --version 2>/dev/null); then
        printf '   version: %s\n' "$v"
    fi
    printf '\n'
    printf '   start the server + dashboard:\n'
    printf '     %s\n' "$BINARY_NAME"
    printf '   then visit:    http://127.0.0.1:4623/\n'
    printf '   full help:     %s --help\n' "$BINARY_NAME"
    printf '   uninstall:     curl -fsSL https://raw.githubusercontent.com/%s/%s/main/install.sh | bash -s -- --uninstall\n' "$OWNER" "$REPO"
    printf '\n'
}

# curl|bash safety: by wrapping main in braces, bash reads the entire script
# into memory before executing. A truncated download can't half-execute.
if [[ "${BASH_SOURCE[0]:-}" == "${0:-}" ]] || [[ -z "${BASH_SOURCE[0]:-}" ]]; then
    { main "$@"; }
fi
