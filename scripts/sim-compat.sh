#!/usr/bin/env bash
# sim-18 oracle shape-check (plan §5.2). Manual + documented, NOT CI-gated.
#
# Compares an external mock oracle (LLMock or llm-mock) against the PINNED
# fixtures in tests/simulation_compat_fixtures/requests.json on SCHEMA SHAPE
# (required keys, event names, tool/usage blocks) — never bytes, since engine
# ids/timestamps are deterministic-by-design and differ from any oracle.
#
# The engine side is locked by `cargo test --test simulation_compat` against
# the same fixtures; this script locks the ORACLE side. Transitive compat:
#   oracle ≈ fixtures  AND  engine == fixtures  ⇒  oracle ≈ engine.
#
# Usage:
#   ./scripts/sim-compat.sh [ORACLE_BASE]
# Examples:
#   ./scripts/sim-compat.sh http://127.0.0.1:8000   # LLMock (10 providers)
#   ./scripts/sim-compat.sh http://127.0.0.1:3000   # llm-mock (prefix routes)
#
# LLMock routes: /v1 (openai), /anthropic, /gemini/v1beta (see LLMock README).
# llm-mock routes: /openai/v1, /anthropic, /gemini (see llm-mock README).
# Pass the matching base; override per-provider with env:
#   SIM_OPENAI_BASE, SIM_ANTHROPIC_BASE, SIM_GEMINI_BASE
set -euo pipefail

BASE="${1:-http://127.0.0.1:8000}"
OPENAI_BASE="${SIM_OPENAI_BASE:-$BASE/v1}"
ANTHROPIC_BASE="${SIM_ANTHROPIC_BASE:-$BASE/anthropic}"
GEMINI_BASE="${SIM_GEMINI_BASE:-$BASE/gemini/v1beta}"
FIXTURES="tests/simulation_compat_fixtures/requests.json"

need() { command -v "$1" >/dev/null 2>&1 || { echo "need $1" >&2; exit 1; }; }
need curl
need python3

echo "== sim-compat oracle shape-check =="
echo "fixtures: $FIXTURES"
echo "openai:    $OPENAI_BASE"
echo "anthropic: $ANTHROPIC_BASE"
echo "gemini:    $GEMINI_BASE"
echo

pass=0
fail=0

check_shape() { # name, json-path-desc, python-expr (reads stdin JSON, exit 0 = ok)
  local name="$1" desc="$2"
  if python3 -c "import json,sys; v=json.load(sys.stdin); assert $3, '$desc'" ; then
    echo "  ok: $name ($desc)"
    pass=$((pass+1))
  else
    echo "  FAIL: $name ($desc)"
    fail=$((fail+1))
  fi
}

check_text() { # name, desc, python-expr (reads stdin TEXT as v, exit 0 = ok)
  local name="$1" desc="$2"
  if python3 -c "import sys; v=sys.stdin.read(); assert $3, '$desc'" ; then
    echo "  ok: $name ($desc)"
    pass=$((pass+1))
  else
    echo "  FAIL: $name ($desc)"
    fail=$((fail+1))
  fi
}

echo "-- OpenAI non-stream (text) --"
curl -sf -m 10 "$OPENAI_BASE/chat/completions" \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hello sim"}]}' \
| check_shape "openai-text" "choices+usage keys" \
  "'choices' in v and 'usage' in v and 'message' in v['choices'][0]"

echo "-- OpenAI tools --"
curl -sf -m 10 "$OPENAI_BASE/chat/completions" \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"search"}],"tools":[{"type":"function","function":{"name":"web_search"}}]}' \
| check_shape "openai-tools" "tool_calls shape" \
  "'tool_calls' in v['choices'][0]['message'] or v['choices'][0].get('finish_reason')=='tool_calls'"

echo "-- OpenAI SSE --"
curl -sfN -m 15 "$OPENAI_BASE/chat/completions" \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}],"stream":true}' \
| check_text "openai-stream" "delta frames + DONE" \
  "'data:' in v and '[DONE]' in v"
# NOTE: SSE shape is checked loosely here (framing bytes); the strict
# frame-order contract lives in simulation_compat.rs against our engine.

echo "-- Anthropic non-stream --"
curl -sf -m 10 "$ANTHROPIC_BASE/v1/messages" \
  -H 'Content-Type: application/json' \
  -H 'x-api-key: test' -H 'anthropic-version: 2023-06-01' \
  -d '{"model":"claude-sonnet-4-6","max_tokens":64,"messages":[{"role":"user","content":"hello sim"}]}' \
| check_shape "anthropic-text" "content+usage keys" \
  "'content' in v and 'usage' in v and 'stop_reason' in v"

echo "-- Gemini non-stream --"
curl -sf -m 10 "$GEMINI_BASE/models/gemini-2.5-flash:generateContent" \
  -H 'Content-Type: application/json' \
  -d '{"contents":[{"parts":[{"text":"hello sim"}]}]}' \
| check_shape "gemini-text" "candidates+usage keys" \
  "'candidates' in v and 'usageMetadata' in v"

echo
echo "fixture count: $(python3 -c "import json; print(len(json.load(open('$FIXTURES'))['requests']))") requests pinned"
echo "pass=$pass fail=$fail"
if [ "$fail" -gt 0 ]; then
  echo "ORACLE DRIFT: shapes above differ from the pinned contract."
  echo "Either update the oracle, or deliberately update fixtures + engine together."
  exit 1
fi
echo "oracle matches pinned shapes."
