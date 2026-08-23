#!/usr/bin/env bash
# dev.sh — single build+test+run for OpenProxy (replaces dev.sh + verify.sh)
# Usage:
#   ./scripts/dev.sh                # quick (default): cargo build + web build + quick tests + start detached on :4623
#   ./scripts/dev.sh --full         # quick + full lib suite (1690 tests, --test-threads=1)
#   ./scripts/dev.sh --no-web       # skip dashboard build
#   ./scripts/dev.sh --no-test      # skip tests
#   ./scripts/dev.sh --no-run       # build+test only, don't start server
#   ./scripts/dev.sh --foreground   # start foreground instead of detached (Ctrl+C to stop)
#   ./scripts/dev.sh --cleanup      # prune old build/debug artifacts before building
#   PORT=4624 ./scripts/dev.sh      # custom port
#   MODE=release ./scripts/dev.sh   # release build (also: --release)
set -euo pipefail
cd "$(dirname "$0")/.."

PORT="${PORT:-4623}"
BUILD_MODE="${BUILD_MODE:-debug}"

BIN_DEBUG="target/debug/openproxy"
BIN_RELEASE="target/release/openproxy"
BIN="$BIN_DEBUG"
CARGO_ARGS=(build --bin openproxy)

FULL=0
DO_WEB=1
DO_TEST=1
DO_RUN=1
FOREGROUND=0
DO_CLEANUP=0

for arg in "$@"; do
  case "$arg" in
    --full) FULL=1 ;;
    --quick) ;; # default is quick, kept for compat
    --no-web) DO_WEB=0 ;;
    --no-test) DO_TEST=0 ;;
    --no-run) DO_RUN=0 ;;
    --foreground|--fg|foreground|run) FOREGROUND=1 ;;
    --release) BUILD_MODE="release" ;;
    --cleanup) DO_CLEANUP=1 ;;
    --detach) ;; # default is detach, kept for compat
    -h|--help)
      sed -n '2,10p' "$0" | sed 's/^# //;s/^#//'
      exit 0
      ;;
    build) DO_TEST=0; DO_WEB=0; DO_RUN=0 ;; # compat: ./scripts/dev.sh build
    detach) ;; # compat
    run) FOREGROUND=1 ;;
    *) echo "Unknown arg: $arg (try --help)"; exit 1 ;;
  esac
done

if [[ "$BUILD_MODE" == "release" ]]; then
  BIN="$BIN_RELEASE"
  CARGO_ARGS=(build --release --bin openproxy)
fi

say() { printf "\n\033[1;36m== %s ==\033[0m\n" "$*"; }
ok()  { printf "\033[1;32m✓ %s\033[0m\n" "$*"; }
fail(){ printf "\033[1;31m✗ %s\033[0m\n" "$*"; }

kill_port() {
  systemctl --user stop openproxy.service 2>/dev/null || true
  if command -v fuser >/dev/null 2>&1; then
    fuser -k "${PORT}/tcp" 2>/dev/null || true
  fi
  pkill -f "openproxy server start" 2>/dev/null || true
  pkill -f "target/.*/openproxy.*${PORT}" 2>/dev/null || true
  sleep 0.5
}

# ── Disk-exhaustion guard ──────────────────────────────────────────────────
# Prune old build/debug artifacts before building when the disk is tight, or
# when --cleanup is requested explicitly. Prevents ENOSPC mid-build (the prior
# incident was caused by /tmp/op-* debug builds + a 3GB+ target/ tree).
run_cleanup() {
  local mode="${1:-}"
  if [[ -x scripts/cleanup.sh ]]; then
    say "pruning old build/debug artifacts (scripts/cleanup.sh${mode:+ $mode})"
    if [[ -n "$mode" ]]; then
      ./scripts/cleanup.sh "$mode" || echo "(cleanup reported errors; continuing)"
    else
      ./scripts/cleanup.sh || echo "(cleanup reported errors; continuing)"
    fi
  fi
}
DISK_NOW="$(df -P . | awk 'NR==2 {gsub("%","",$5); print $5+0}')"
if [[ "$DO_CLEANUP" == 1 ]]; then
  run_cleanup
elif [[ "${DISK_NOW:-0}" -ge 80 ]]; then
  say "disk ${DISK_NOW}% >= 80% — auto-pruning before build"
  run_cleanup --aggressive
fi

say "cargo ${CARGO_ARGS[*]} (incremental)"
cargo "${CARGO_ARGS[@]}"
ok "backend built: $(ls -lh "$BIN" | awk '{print $5}')  $BIN"

if [[ "$DO_WEB" == 1 ]]; then
  say "web build (Astro → web/dist)"
  if [[ ! -d web/node_modules ]]; then
    echo "node_modules missing — running pnpm install..."
    (cd web && pnpm install)
  fi
  (cd web && pnpm build)
  ok "dashboard built (web/dist, 183 pages)"
else
  echo "(skip web build)"
fi

if [[ "$DO_TEST" == 0 ]]; then
  echo "(skip tests)"
elif [[ "$FULL" == 1 ]]; then
  say "cargo test --lib (full suite, --test-threads=1)"
  cargo test -p openproxy --lib -- --test-threads=1
  say "cargo test --test providers_api"
  cargo test --test providers_api -- --test-threads=1 --skip provider_test_models_route_fetches_live_compatible_models_and_warms_first_request
  ok "full tests passed"
else
  say "cargo test (quick: provider_models + import_catalog)"
  cargo test -p openproxy --lib provider_models -- --nocapture
  cargo test --test providers_api import_catalog -- --nocapture
  cargo test -p openproxy --lib parity_tests -- --nocapture 2>&1 | tail -n 5 || true
  ok "quick tests passed"
fi

if [[ "$DO_RUN" == 0 ]]; then
  say "done (no-run)"
  echo "Run ./scripts/dev.sh to build+test+start, or ./scripts/dev.sh --foreground for Ctrl+C mode"
  exit 0
fi

# auto-start (quick default = detached)
if [[ "$FOREGROUND" == 1 ]]; then
  kill_port
  say "starting $BIN server start --port $PORT (foreground, Ctrl+C to stop)"
  echo "  Dashboard: http://127.0.0.1:${PORT}"
  echo "  API:       http://127.0.0.1:${PORT}/v1  (Bearer \$OPENPROXY_API_KEY)"
  exec "$BIN" server start --port "$PORT" --no-open
else
  kill_port
  say "starting $BIN server start --port $PORT --detach --no-open"
  "$BIN" server start --detach --no-open --port "$PORT"
  say "status"
  "$BIN" --robot server status 2>&1 | head -n 20 || curl -sf "http://127.0.0.1:${PORT}/health" && echo "health ok"
  echo "Dashboard: http://127.0.0.1:${PORT}/dashboard/providers"
  echo "Logs: tail -f ~/.openproxy/log.txt"
  echo "Stop: $BIN server stop  or  fuser -k ${PORT}/tcp"
  ok "dev ready — quick build+test+start done"
fi
