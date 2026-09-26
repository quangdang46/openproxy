#!/usr/bin/env bash
# Live feature smoke for the four core product surfaces (AGENTS.md).
#
# This is the check /loop runs. It is deliberately cheap and side-effect-free:
# no model inference, no quota, no writes. Anything that costs tokens or mutates
# state belongs in a bead, not on a recurring timer.
#
#   surfaces  — the 4 dashboards must serve 200 (providers, cli-tools, combos)
#   health    — the server is up and its DB is loadable
#   stream    — the TRANSLATED SSE path, which is what bead g37k was about:
#               a regression here is a 200 with an empty body, invisible to a
#               status check, so it runs the real tests rather than a probe
#   upstream  — configured providers report their health, so a dead credential
#               is visible before a user spends a turn finding out
#
# Exit 0 = all green. Exit 1 = at least one FAIL; the summary is on stdout.
#
# Offline mode: set SMOKE_SKIP_TESTS=1 to skip cargo (use when the loop fires
# while a build is already running and would contend for target/).

set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

BASE="${OPENPROXY_URL:-http://127.0.0.1:4623}"
KEY="${OPENPROXY_API_KEY:-}"
BIN=./target/debug/openproxy
fails=0
pass() { printf '  ok   %s\n' "$1"; }
fail() { printf '  FAIL %s\n' "$1"; fails=$((fails + 1)); }

echo "== surfaces =="
for p in /dashboard /dashboard/providers /dashboard/combos /dashboard/cli-tools/opencode; do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 10 "$BASE$p" 2>/dev/null)
  if [ "$code" = "200" ]; then pass "$p -> 200"; else fail "$p -> ${code:-no-response}"; fi
done

echo "== health =="
if curl -s -m 10 "$BASE/health" 2>/dev/null | grep -q '"status":"ok"'; then
  pass "server reports status ok"
else
  fail "server health is not ok (is it running? ./scripts/dev.sh detach)"
fi
if [ -x "$BIN" ]; then
  if "$BIN" doctor 2>/dev/null | grep -q 'all checks passed'; then
    pass "doctor: all checks passed"
  else
    fail "doctor reported a failing check"
  fi
else
  fail "$BIN missing — run ./scripts/dev.sh build"
fi

echo "== stream (translated SSE, bead g37k) =="
if [ "${SMOKE_SKIP_TESTS:-0}" = "1" ]; then
  printf '  skip cargo (SMOKE_SKIP_TESTS=1)\n'
else
  if cargo test -p openproxy --test streaming_translation_parity --quiet 2>&1 | tail -3 | grep -q 'test result: ok'; then
    pass "translated SSE tests green"
  else
    fail "translated SSE tests FAILED — a 200-with-empty-body regression"
  fi
fi

echo "== upstream health =="
if [ -x "$BIN" ] && [ -n "$KEY" ]; then
  # Report, do not assert: a dead third-party credential is the user's to fix
  # and is not a code regression. Surfaced so it is not rediscovered by hand.
  "$BIN" --robot provider list 2>/dev/null | python3 -c '
import sys, json
try:
    conns = json.load(sys.stdin)["data"]["provider_connections"]
except Exception:
    print("  (provider list unavailable)"); raise SystemExit(0)
for c in conns:
    st = c.get("healthStatus") or "unknown"
    name = c.get("provider") or c.get("type") or "?"
    mark = "ok  " if st == "healthy" else "warn"
    print(f"  {mark} {name:16} {st}")
' 2>/dev/null || printf '  (provider list unavailable)\n'
fi

echo
if [ "$fails" -eq 0 ]; then
  echo "SMOKE OK"
  exit 0
fi
echo "SMOKE FAILED ($fails)"
exit 1
