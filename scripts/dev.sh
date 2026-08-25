#!/usr/bin/env bash
# dev build + run loop for OpenProxy
# Usage:
#   ./scripts/dev.sh              # incremental debug build + run foreground (Ctrl+C to stop)
#   ./scripts/dev.sh detach       # build + run detached on 127.0.0.1:4623
#   ./scripts/dev.sh build        # only build, don't run
#   PORT=4624 ./scripts/dev.sh    # custom port
#   MODE=release ./scripts/dev.sh # release build (slower, optimized)
set -euo pipefail
cd "$(dirname "$0")/.."

PORT="${PORT:-4623}"
MODE="${1:-run}"
BUILD_MODE="${BUILD_MODE:-debug}"  # debug (incremental, fast) or release

BIN_DEBUG="target/debug/openproxy"
BIN_RELEASE="target/release/openproxy"
BIN="$BIN_DEBUG"
CARGO_ARGS=(build --bin openproxy)
if [[ "$BUILD_MODE" == "release" ]]; then
  BIN="$BIN_RELEASE"
  CARGO_ARGS=(build --release --bin openproxy)
fi

kill_port() {
  # stop systemd auto-service if still up (now disabled, but guard)
  systemctl --user stop openproxy.service 2>/dev/null || true
  # kill whatever is on PORT (openproxy or stale)
  if command -v fuser >/dev/null 2>&1; then
    fuser -k "${PORT}/tcp" 2>/dev/null || true
  fi
  pkill -f "openproxy server start" 2>/dev/null || true
  # also kill direct binary invocations on this repo
  pkill -f "target/.*/openproxy.*${PORT}" 2>/dev/null || true
  sleep 0.5
}

build() {
  echo "== cargo ${CARGO_ARGS[*]} =="
  # incremental by default; only rebuilds crates that changed
  # --bin openproxy avoids building tests/examples
  cargo "${CARGO_ARGS[@]}"
  echo "== built $BIN =="
  ls -lh "$BIN" | awk '{print $9, $5, $6, $7, $8}'
}

case "$MODE" in
  build)
    build
    echo "Build done. Run ./scripts/dev.sh to start."
    ;;
  detach)
    kill_port
    build
    echo "== starting $BIN server start --port $PORT --detach --no-open =="
    "$BIN" server start --detach --no-open --port "$PORT"
    echo "== status =="
    "$BIN" --robot server status 2>&1 | head -n 20 || curl -sf "http://127.0.0.1:${PORT}/health" && echo "health ok"
    echo "Logs: tail -f ~/.openproxy/log.txt  (or journalctl --user -u openproxy -f if using service)"
    echo "Stop: $BIN server stop  or  pkill -f openproxy  or  fuser -k ${PORT}/tcp"
    ;;
  run|restart|"")
    kill_port
    build
    echo "== starting $BIN server start --port $PORT (foreground, Ctrl+C to stop) =="
    echo "   Dashboard: http://127.0.0.1:${PORT}"
    echo "   API:       http://127.0.0.1:${PORT}/v1  (Bearer \$OPENPROXY_API_KEY)"
    exec "$BIN" server start --port "$PORT" --no-open
    ;;
  *)
    echo "Unknown mode: $MODE"
    echo "Usage: $0 [run|build|detach]  (default: run)"
    exit 1
    ;;
esac
