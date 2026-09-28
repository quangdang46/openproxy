---
name: openproxy-rtk
description: Operate OpenProxy's RTK request-compression stack — the token saver that rewrites tool-call results before they reach a model, the `x-9router-token-saver` per-request opt-out header, the `rtkEnabled` / `cavemanEnabled` / `ponytailEnabled` / `headroomEnabled` settings toggles, and the `GET /api/compression/stats` endpoint that reports real savings. Use whenever the user asks to reduce input tokens, cut cost or latency, measure compression savings, debug why tool output is not being compressed, or asks about the `/dashboard/compression` and `/dashboard/token-saver` pages.
---

# openproxy-rtk — token saving / request compression

OpenProxy's RTK stack is a set of **savers** that run on the request body *after* translation and
*before* dispatch, so the provider receives a smaller payload than the client sent. It is not a
prompt-engineering feature and it does not touch responses — it compresses the **inbound** context,
primarily tool-call results.

This skill covers:

1. The saver chain and exactly where it sits in the dispatch pipeline
2. Turning RTK (and the adjacent Caveman / Ponytail / Headroom savers) on and off
3. The per-request `x-9router-token-saver` opt-out header
4. Which payloads RTK actually compresses, and which it deliberately leaves alone
5. Reading the real savings numbers from `GET /api/compression/stats`
6. PXPIPE — the token-saver that is **not** wired up, and what to use instead

Everything here is non-interactive and safe to run unattended.

## 0 · Preflight

```bash
curl -sS http://127.0.0.1:4623/health | jq '.status'
# → "ok"

export OPENPROXY_API_KEY="$(openproxy --robot server init | jq -r '.data.admin_key.key')"
```

If the server is not running: `openproxy server start --detach --no-open`.
If there is no admin key, mint one with `openproxy key add <name> --auto`.

`/api/compression/stats` and every `/api/pxpipe/*` route sit behind the admin/management gate — send
`Authorization: Bearer $OPENPROXY_API_KEY` (or a dashboard session cookie). A request with **no**
header may succeed on a loopback dev instance with login disabled, but a *wrong* key is always a
`401 invalid_api_key`. Always send the bearer.

## 1 · Where the savers run

The dispatch pipeline (`src/server/api/chat.rs`) applies, in this order, after the request has been
translated into the provider's wire format:

1. **RTK** — tool-result compression (`compress_messages`)
2. **Headroom** — optional external compressor at `headroomUrl`
3. **Caveman / Ponytail** — system-prompt injection
4. Claude tool dedupe
5. TTS tool strip
6. PXPIPE image-context pass (disabled — see §6)

Two gates apply to **all** of steps 1–3 and 5:

- the per-request `x-9router-token-saver: off` header (§3), and
- each saver's own settings toggle.

RTK's toggle is `rtkEnabled`. It is the only saver that defaults to **on** (`true`); Caveman,
Ponytail and Headroom all default to **off**.

## 2 · Read and write the toggles

There is **no `openproxy rtk` subcommand** — the savers are configured through the settings
document. Do not invent one.

```bash
# Read a single field
openproxy --robot settings get --key rtkEnabled
openproxy settings get --key cavemanLevel

# Flip a toggle (values are auto-coerced: true/false ⇒ bool, integers ⇒ number)
openproxy settings set --key rtkEnabled     --value false
openproxy settings set --key cavemanEnabled --value true
openproxy settings set --key cavemanLevel   --value ultra

# Or PATCH the document directly
curl -sS -X PATCH http://127.0.0.1:4623/api/settings \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"rtkEnabled":true,"cavemanEnabled":false}'
```

Both `PUT` and `PATCH` on `/api/settings` are accepted. Password changes are **not** — they return
`501 Password changes must use a dedicated endpoint`.

The saver settings, with their defaults:

| Field (camelCase in JSON / CLI) | Default | Meaning |
|---|---|---|
| `rtkEnabled` | `true` | RTK tool-result compression |
| `cavemanEnabled` | `false` | Inject the terse-response system prompt |
| `cavemanLevel` | `full` | `lite` \| `full` \| `ultra` \| `wenyan-lite` \| `wenyan` \| `wenyan-ultra` |
| `ponytailEnabled` | `false` | Inject the Ponytail system prompt |
| `ponytailLevel` | `full` | `lite` \| `full` \| `ultra` |
| `headroomEnabled` | `false` | Route the body through the external Headroom proxy |
| `headroomUrl` | `http://localhost:8787` | Where Headroom listens |
| `headroomTimeoutMs` | `3000` | Per-request Headroom timeout |
| `headroomCompressUserMessages` | `false` | Let Headroom compress user turns, not just tool results |
| `headroomCodeAware` | `false` | Pass `--code-aware` (AST compression) to Headroom |
| `headroomKompress` | `true` | Pass `--disable-kompress` when false |
| `systemInject` (in `extra`) | `false` | Inject a custom system prompt from `extra.systemPrompt` |

An unrecognized `cavemanLevel` / `ponytailLevel` does **not** error — it falls back to `full`.

RTK settings live in the server's settings document (SQLite) and survive binary rebuilds. Prefer
`openproxy settings set` over editing files by hand.

## 3 · Per-request opt-out

`x-9router-token-saver: off` (case-insensitive) disables **every** saver for a single request —
RTK, Headroom, Caveman, Ponytail, and the PXPIPE pass. The header must be on the request you send
to `/v1/chat/completions`; it is a per-request override, not a server setting.

```bash
# Normal request — savers active
curl -sS http://127.0.0.1:4623/v1/chat/completions \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"openai/gpt-4o","messages":[{"role":"user","content":"ping"}]}'

# Same request, savers bypassed — useful for A/B measuring the compression delta
curl -sS http://127.0.0.1:4623/v1/chat/completions \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'x-9router-token-saver: off' \
  -H 'content-type: application/json' \
  -d '{"model":"openai/gpt-4o","messages":[{"role":"user","content":"ping"}]}'
```

Absent header, empty value, or any value other than `off` (e.g. `yes`, `0`) leaves the savers **on**.
A malformed element in one tool message costs only that element — the pass does not abort.

## 4 · What RTK actually compresses

RTK only touches **tool output** in a request body. It walks the message array (`messages[]`,
falling back to `input[]`) and rewrites:

- `role: "tool"` messages (OpenAI chat, string or part-array `content`)
- `type: "function_call_output"` items (OpenAI Responses, `output` field)
- `type: "tool_result"` content blocks (Claude)
- Kiro's `conversationState.history[].userInputMessage…toolResults[]` and `currentMessage`

For each text payload it runs an **autodetect** over the first 1024 characters, picks one filter, and
only accepts the result if it is strictly smaller than the input. The autodetect chain
(`src/core/rtk/autodetect.rs`) returns exactly these twelve, in priority order:

1. `git-log`
2. `git-diff`
3. `git-status` — explicit match first, then a mostly-porcelain heuristic
4. `build-output` — npm/yarn/cargo/pip logs, checked **before** the porcelain heuristic so
   cargo `Compiling` lines are not misread as git status
5. `grep`
6. `find`
7. `tree`
8. `ls`
9. `search-list`
10. `read-numbered`
11. `dedup-log` — any payload with ≥5 non-empty lines
12. `smart-truncate` — any payload with ≥250 lines

If nothing matches, the payload passes through untouched. `test-runner`, `json-summary` and
`smartcrusher` are **implemented** (`src/core/rtk/filters/mod.rs`, `src/core/rtk/smartcrusher.rs`)
but deliberately **not** in the autodetect chain — the code comments record that 9router has no such
stages, so cargo-test output reaches `dedup-log`, a large JSON array reaches `dedup-log`, and a CSV
body reaches `dedup-log` too. Do not expect those filter names in the debug log.

Thresholds, both hard-coded in `src/core/rtk/constants.rs`:

- payloads **under 500 bytes** are skipped entirely (not worth the pass),
- payloads **over 10 MiB** are skipped,
- a filter that panics or returns something not-smaller is discarded and the raw text passes through.

Deliberately preserved: image blocks (counted in `imagePrompts`, never rewritten), and **error** tool
results — Claude blocks with `is_error: true` and Kiro tool results with `status == "error"` are left
byte-for-byte so the model still sees the failure trace.

If a filter panics, the raw text passes through and the server writes
`[rtk] warning: filter '<name>' panicked — passing through raw output` to its **stderr** — that line
is unconditional, not gated on `RUST_LOG`. Headroom separately logs `headroom …` / `headroom skip=…`
lines at `RUST_LOG=debug`.

## 5 · Read the savings

`GET /api/compression/stats?period=<period>` is the only endpoint with real numbers. It aggregates
`usageHistory` rows where `bytesSaved > 0`.

```bash
curl -sS "http://127.0.0.1:4623/api/compression/stats?period=7d" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" | jq
```

Valid `period` values: `today` (the default), `24h`, `7d`, `30d`, `60d`, `all`. Anything else returns
`400 {"error":"Invalid period. Use one of: today, 24h, 7d, 30d, 60d, all"}`.

Response shape:

```json
{
  "totalRequests": 128,
  "tokensSaved": 412908,
  "avgSavingsPct": 31.4,
  "avgDurationMs": 0,
  "receipts": 0,
  "fallbacks": 0,
  "imagePrompts": 7,
  "realUsage": {
    "promptTokens": 902110,
    "completionTokens": 48800,
    "totalTokens": 950910,
    "cacheTokens": 0,
    "sources": { "provider": 128, "stream": 0 }
  },
  "modeBreakdown": [{ "mode": "Standard", "requests": 128, "tokensSaved": 412908 }]
}
```

Reading it honestly:

- `totalRequests` / `tokensSaved` / `avgSavingsPct` / `imagePrompts` / `realUsage` are **measured**.
- `avgDurationMs`, `receipts`, `fallbacks`, and `realUsage.sources.stream` are always `0` — the
  fields are reserved, not unimplemented stubs to debug.
- `avgSavingsPct` is `tokensSaved / bytesBefore` across the window, so it is a **byte** ratio, not a
  token ratio.
- `modeBreakdown` currently carries a single `Standard` row that mirrors the totals.
- An all-zero payload means **no request has saved anything yet** — either the savers are off, or no
  request has carried a compressible tool result (the 500-byte floor and the tool-result-only scope
  mean ordinary chat traffic saves nothing).

Dashboard page: `/dashboard/compression` renders this endpoint with a period selector. The toggles
live at `/dashboard/token-saver`.

## 6 · PXPIPE — not wired, don't enable it

`/api/pxpipe/*` exists and answers, but it is a **reporting skeleton only**. PXPIPE is an external
npm token compressor that OpenProxy does not install or manage; the endpoints always report
`NOT_INSTALLED`. This is an intentional divergence from 9router, recorded in
`docs/parity-9router.md` ("Not ported — use RTK + Headroom + Caveman/Ponytail").

```bash
openproxy --robot pxpipe status
# → {"code":"NOT_INSTALLED",
#    "error":"PXPIPE is not managed by OpenProxy. Use RTK + Headroom + Caveman/Ponytail instead.",
#    "installed":false,"running":false,"mode":"library","enabled":<pxpipeEnabled>,…}

curl -sS http://127.0.0.1:4623/api/pxpipe/status -H "Authorization: Bearer $OPENPROXY_API_KEY" | jq
```

The other three routes exist and are readable, but their payloads are empty by construction:

| Route | Method | Returns |
|---|---|---|
| `/api/pxpipe/status` | `GET` | install/version/config skeleton, always `NOT_INSTALLED` |
| `/api/pxpipe/health` | `GET`/`POST` | `healthy:false` with three failing checks |
| `/api/pxpipe/stats` | `GET` | `windows.{all,today,yesterday,last7d,last30d}` all zero, empty `timeline`/`recent` |
| `/api/pxpipe/logs` | `GET` | `?limit=N` accepted, no entries |

The `pxpipe*` settings (`pxpipeEnabled`, `pxpipeAutoInstall`, `pxpipeMinChars`, `pxpipeTimeoutMs`)
persist and are surfaced in the dashboard, but flipping them does nothing. **If the user wants token
saving, point them at `rtkEnabled` (+ Headroom / Caveman / Ponytail), not at PXPIPE.** The
`/dashboard/pxpipe` page links over to `/dashboard/token-saver` for this reason.

## 7 · Headroom (the one adjacent saver worth wiring)

Headroom is a separate external service, not part of RTK, but it runs in the same slot of the chain
and shares the `x-9router-token-saver` gate.

```bash
openproxy settings set --key headroomEnabled --value true
openproxy settings set --key headroomUrl     --value http://localhost:8787

curl -sS http://127.0.0.1:4623/api/headroom/status \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" | jq
```

Available: `GET /api/headroom/status`, `POST /api/headroom/start`, `POST /api/headroom/stop`,
`POST /api/headroom/restart`, and `GET|POST|DELETE /api/headroom/extras`. The dashboard's
`/dashboard/token-saver` page drives these. If Headroom is not running, the pass is skipped with a
`headroom skip=<reason>` debug line and the request proceeds uncompressed — it never fails the call.

## 8 · Verifications

```bash
# 1. RTK is on
openproxy --robot settings get --key rtkEnabled | jq -r '.data.value'

# 2. A real chat round-trip still works with savers on
curl -sS http://127.0.0.1:4623/v1/chat/completions \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"openai/gpt-4o","messages":[{"role":"user","content":"ping"}]}' | jq '.choices[0].message.content'

# 3. The opt-out header is honored (identical result, savers bypassed)
curl -sS http://127.0.0.1:4623/v1/chat/completions \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'x-9router-token-saver: off' \
  -H 'content-type: application/json' \
  -d '{"model":"openai/gpt-4o","messages":[{"role":"user","content":"ping"}]}' >/dev/null && echo "opt-out ok"

# 4. Savings are being recorded
curl -sS "http://127.0.0.1:4623/api/compression/stats?period=all" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  | jq '{totalRequests, tokensSaved, avgSavingsPct}'
```

To prove compression end-to-end, send a request whose tool output is a large build log or `git diff`
(over 500 bytes) and re-read step 4 — `totalRequests` should increase. Ordinary chat with no tool
results will never move the numbers; that is the design, not a bug.

Smoke test for the compressor itself:

```bash
cargo test -p openproxy --lib rtk
```

## Common failure modes

| Symptom | Cause / fix |
|---|---|
| `/api/compression/stats` all zeros | No request carried a compressible **tool result**. RTK only rewrites tool output, and only above 500 bytes. It does not compress prompts. |
| `totalRequests` frozen after toggling | The toggle applies to the **next** request; there is no re-processing of past requests. |
| `avgSavingsPct` = 0 with nonzero `totalRequests` | `bytesBefore` was 0 for the window — treat the percentage as undefined rather than a real 0% result. |
| `400 {"error":"Invalid period…"}` | `period` is not one of `today`, `24h`, `7d`, `30d`, `60d`, `all`. |
| `401 invalid_api_key` on `/api/compression/stats` or `/api/pxpipe/*` | Missing/incorrect admin bearer. `Authorization: Bearer $OPENPROXY_API_KEY`. |
| `501 Password changes must use a dedicated endpoint` | You PATCHed a password field at `/api/settings`. Use the auth endpoints instead. |
| `cavemanLevel` reads back as `full` after you set `wenyan` | Expected only if the value is unrecognized. Valid: `lite`, `full`, `ultra`, `wenyan-lite`, `wenyan`, `wenyan-ultra`. |
| RTK apparently ran but nothing shrank | A filter that returns empty, larger, or panicking output is discarded by design — the raw text passes through. Check the debug log for `[rtk] warning: filter '…' panicked`. |
| PXPIPE page shows nothing happening | Expected. PXPIPE is not managed by OpenProxy; `status` always reports `NOT_INSTALLED`. Use RTK + Headroom + Caveman/Ponytail. |
| Model answer degraded after enabling Caveman | Caveman changes **response** style, not compression. Turn it off (`cavemanEnabled=false`) and keep `rtkEnabled` on. |

## When _not_ to use this skill

- The user wants to compress **responses** or cut output tokens — this stack only compresses inbound context.
- The user wants PXPIPE's image-context rendering — it is not ported; do not promise it.
- The user is wiring a CLI tool into the proxy — use `.agents/skills/openproxy/SKILL.md`.
- The user is debugging combo dispatch or provider fallback — use `.agents/skills/openproxy-combos/SKILL.md`.

## See also

- Pipeline order and intentional divergences: [`docs/parity-9router.md`](../../../docs/parity-9router.md) (§"Key pipeline" step 8; PXPIPE row)
- RTK implementation: `src/core/rtk/` — `mod.rs` (dispatch), `autodetect.rs`, `constants.rs`, `apply_filter.rs`, `filters/`, `smartcrusher.rs`, `headroom.rs`
- Chat pipeline wiring: `src/server/api/chat.rs` (header gate + saver order)
- Savings endpoint: `src/server/api/usage.rs` (`compression_stats`)
- Install & operate: `.agents/skills/openproxy/SKILL.md`
