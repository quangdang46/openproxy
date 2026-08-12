#!/usr/bin/env bash
# update-openproxy.sh — pull latest upstream, rebuild from source, swap the binary in.
#
#   - Tracks upstream quangdang46/openproxy (main) even though origin is your fork.
#   - Fast-forwards when possible; falls back to a merge commit if you have local commits.
#   - Skips the whole rebuild when HEAD is unchanged and the installed binary already
#     matches the last source build (daily no-op is instant).
#   - Restarts the detached server only if it was already running. Data in ~/.openproxy
#     is untouched.
#
# Overridable env: UPSTREAM_URL, UPSTREAM_BRANCH, DEST_BIN
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UPSTREAM_URL="${UPSTREAM_URL:-https://github.com/quangdang46/openproxy.git}"
UPSTREAM_BRANCH="${UPSTREAM_BRANCH:-main}"
DEST_BIN="${DEST_BIN:-$HOME/.local/bin/openproxy}"

cd "$REPO_DIR"

log() { printf '\n==> %s\n' "$*"; }

trap 'log "FAILED at line $LINENO. Nothing was broken on purpose; fix and re-run."' ERR

# --- 1. ensure the upstream remote exists -----------------------------------
if ! git remote get-url upstream >/dev/null 2>&1; then
  log "adding upstream remote"
  git remote add upstream "$UPSTREAM_URL"
fi

# --- 2. fetch ---------------------------------------------------------------
log "fetching upstream/$UPSTREAM_BRANCH"
git fetch upstream --prune

if git merge-base --is-ancestor "upstream/$UPSTREAM_BRANCH" HEAD; then
  log "already up to date with upstream/$UPSTREAM_BRANCH"
  HEAD_CHANGED=false
else
  log "updating from upstream/$UPSTREAM_BRANCH"
  if ! git merge --ff-only "upstream/$UPSTREAM_BRANCH" 2>/dev/null; then
    log "local commits detected - merging upstream (creates a merge commit)"
    git merge --no-edit -m "merge: upstream/$UPSTREAM_BRANCH" "upstream/$UPSTREAM_BRANCH"
  fi
  HEAD_CHANGED=true
fi

# --- 3. decide whether anything needs doing ----------------------------------
NEED_BUILD=false
NEED_SWAP=false
if [[ "$HEAD_CHANGED" == true || ! -f target/release/openproxy ]]; then
  NEED_BUILD=true
fi
if [[ ! -f "$DEST_BIN" ]] || ! cmp -s target/release/openproxy "$DEST_BIN"; then
  NEED_SWAP=true
fi

if [[ "$NEED_BUILD" == false && "$NEED_SWAP" == false ]]; then
  log "nothing to do - installed binary matches the current source build"
  "$DEST_BIN" --version
  exit 0
fi

# --- 4. build the embedded dashboard (required before cargo build) ----------
if [[ "$NEED_BUILD" == true ]]; then
  log "building dashboard (web/dist)"
  pnpm --dir web install --frozen-lockfile
  pnpm --dir web run build
fi

# --- 5. build the release binary --------------------------------------------
if [[ "$NEED_BUILD" == true ]]; then
  log "building release binary (first build can take several minutes)"
  cargo build --release --locked
fi

# --- 6. stop the server if it is running --------------------------------------
WAS_RUNNING=false
if "$DEST_BIN" --robot server status 2>/dev/null | grep -q '"process_alive":true'; then
  WAS_RUNNING=true
  log "stopping running server"
  "$DEST_BIN" server stop >/dev/null 2>&1 || true
  for _ in $(seq 1 20); do
    curl -sf http://127.0.0.1:4623/health >/dev/null 2>&1 || break
    sleep 0.5
  done
fi

# --- 7. swap the binary in -----------------------------------------------------
log "installing to $DEST_BIN"
cp target/release/openproxy "$DEST_BIN"
chmod +x "$DEST_BIN"
NEW_VERSION="$("$DEST_BIN" --version)"
log "installed $NEW_VERSION"

# --- 8. restart + verify --------------------------------------------------------
if [[ "$WAS_RUNNING" == true ]]; then
  log "restarting server"
  "$DEST_BIN" server start --detach --no-open
fi
log "verifying"
"$DEST_BIN" doctor

if [[ "$HEAD_CHANGED" == true ]]; then
  log "local branch advanced - run 'git push' if you want your fork in sync"
fi
