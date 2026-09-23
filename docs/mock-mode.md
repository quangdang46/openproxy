# Mock Mode — Operator Guide (Simulation Layer MVP)

> Epic `openproxy-simulation-layer`, beads sim-01…sim-21. Status: MVP complete.
> Goal: any provider configured via dashboard/CLI behaves as if real, but
> executes locally — **no API key / subscription needed**. Clients change nothing.

## 1. Quick start

```bash
# Per-provider mock (persists in SQLite, survives rebuilds):
openproxy provider mode openai mock
openproxy provider mode openai        # show configured + effective + reason
openproxy provider status             # all providers

# Global dev force (safety boundary — cannot be bypassed by request headers):
OPENPROXY_DEV_MOCK=1 openproxy serve
# ...or persistently:
# PATCH /api/settings {"devMockAll": true}   # or the dashboard checkbox
```

Test like production (no key configured anywhere):

```bash
curl localhost:4623/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}'
# → {"id":"chatcmpl-sim-…","object":"chat.completion",…,"choices":[{…"content":"Echo: hi"…}]}

curl localhost:4623/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}],"stream":true}'
# → SSE deltas + data: [DONE]

# Back to real (privileged action — CLI/config/admin only, never a header):
openproxy provider mode openai real
```

Dashboard: provider detail page → **Real / 🧪 Mock** toggle (works even with
zero connections); provider list + model picker mirror a 🧪 badge from the same
`/api/mock/status` source. A banner shows when effective ≠ configured
(`Effective: MOCK via OPENPROXY_DEV_MOCK / settings-force`). Providers list top
has a `Dev: mock all` checkbox (same as `devMockAll`).

## 2. Control headers (`x-openproxy-sim-*`)

| Header | Effect |
|---|---|
| `x-openproxy-sim: mock` | This request executes mock (real→mock only; **no header forces mock→real**) |
| `x-openproxy-sim-status: 400\|429\|500\|503` | Provider-correct error envelope; 429 adds `Retry-After: 2` |
| `x-openproxy-sim-latency-ms: N` | First-byte delay (saturates at 60 000); delays fault errors too |
| `x-openproxy-sim-response: <json\|string>` | Content override (§4); never raw protocol |
| `x-openproxy-sim-disconnect-after-chunks: N` | Cut SSE after N data frames (N counts the role-establishing chunk; N=0 → empty) |

Headers are stripped before any REAL forward and never logged. Invalid values
are ignored + warn-logged (status/disconnect) or saturated (latency) — never crash.

Example — force a fallback (mock 429 on openai → next provider serves):

```bash
curl localhost:4623/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -H 'x-openproxy-sim: mock' \
  -H 'x-openproxy-sim-status: 429' \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}'
# → 429 rate_limit_error envelope; combo/fallback moves to the next member
```

## 3. Tool echo bounds

Mock echoes the FIRST offered tool with mock input (`{}`) — it proves
tool-call plumbing (`tool_calls`/`tool_use`, `finish_reason`), it does NOT
script tool outputs. Per-call arguments cannot be pinned; exact per-call
outputs belong to Phase-2 scenarios, not MVP.

## 4. Determinism contract

```text
same request + same simulation config → same response bytes
```

IDs (`chatcmpl-sim-<8hex>`, `msg_sim_…`, `call_sim_…`) and `created` derive from
SHA-256 of the canonical request (NOT wall-clock, NOT SipHash — stable across
toolchain rebuilds). Snapshot-friendly for CI.

## 5. Troubleshooting

- **"I set real but it still mocks"** — check effective vs configured:
  `openproxy provider mode <name>` (or `/api/mock/status`). Global force
  (`OPENPROXY_DEV_MOCK` / `devMockAll`) overrides everything by design.
- **"Mock unavailable for this format"** — the provider's wire format has no
  registered simulator (`simulationSupported: false`). Only OpenAI /
  OpenAI-compatible / Anthropic / Anthropic-compatible (+Claude reuse) /
  Gemini execute mock in MVP.
- **"429 fault didn't fire on REAL"** — REAL-branch fault decorates SUCCESS
  only; upstream non-2xx flows into the normal error path (mock branch
  pre-empts all, including validation failures). Deliberate asymmetry, locked
  by test.
- **"Stream cut looks hanging"** — disconnect drops terminal markers
  (`[DONE]`/`message_stop`) so clients must surface truncation as error, not
  hang. If a client hangs, its SSE parser ignores truncation — client bug.
- **Stale dashboard after `web/src` change** — rebuild: `cd web && pnpm build`
  (`web/dist` is what the server serves; Astro has no live reload here).
- **Live-E2E verified 2026-09-23** (fresh binary, temp DATA_DIR, no credentials):
  non-stream echo, SSE + `[DONE]`, 429 (HTTP 429 + envelope), override,
  disconnect-cut, cache-bypass (no `x-cache` on sim requests) all PASS.
  Two live-only fixes resulted: credentialless stub dispatch (no-connection +
  mock ⇒ stub, single-shot anti-loop) and sim-header cache bypass (both
  directions). Known gap: native-format HTTP edges (`/v1/messages`,
  Gemini generateContent) return mistranslated/empty bodies — the simulators
  are correct at executor level (sim-09/10 E2E + contract), but the
  OpenAI-normalized chat pipeline mistranslates native shapes at the compat
  edge. Follow-up epic (translator interop), not MVP.

## 6. Manual verification checklist (plan §11)

- [ ] `provider add openai --api-key test-key` → `provider mode openai mock` →
      chat non-stream echo + `stream:true` SSE valid
- [ ] `-H "x-openproxy-sim-status: 429"` → fallback observed in logs
- [ ] Mixed fallback mock→real and real→mock both serve
- [ ] No key at all → mock still 200 (all 3 formats)
- [ ] `OPENPROXY_DEV_MOCK=1` → all mock; no header can force real; unset → back
- [ ] Dashboard toggle flips mode; banner + modal badge mirror; `web/dist` rebuilt
- [ ] `git status` / `git diff --cached` — no secrets

## 7. Endpoint coverage (mock MVP)

Mock execution is wired into the **chat dispatch path** only:

| Surface | Mock | Notes |
|---|---|---|
| `POST /v1/chat/completions` (plus `/chat/completions`, `/v1/v1/...`) | yes | stream + non-stream, 3 formats |
| `POST /v1/messages` (Anthropic) | executor-level | see troubleshooting note on the compat edge |
| `POST /v1beta/models/{model}:generateContent` (Gemini) | executor-level | same compat-edge note |
| `GET /v1/models`, `/api/*` | n/a | real data (mode is a config surface, not a request) |
| `POST /v1/embeddings`, `/v1/audio/*`, `/v1/images/*` | **no** | these dispatch outside the chat path and the simulator implements no embeddings/audio/image response shape, so they return the normal "No credentials" error. Simulators for them belong in a follow-up epic. |

## 8. Boundaries (NOT in MVP)

Record/replay, multi-step scenarios, stateful simulation, chaos
probabilities, Mock Lab page, semantic intelligence emulation, non-LLM
providers (Exa/Tavily/…), full `BehaviorProfile` — separate future epics.
`openproxy schema show provider` intentionally unchanged: simulation mode is
orthogonal to the connection schema (documented in sim-19 commit).
