---
name: openproxy-chat
description: Send chat completions through OpenProxy's OpenAI-compatible API — POST /v1/chat/completions, the `provider/model` addressing convention, combos as bare model names, `stream: true` SSE framing, tools / tool_calls passthrough, and reading combo fallback out of a response. Use whenever the user asks to call a model through the proxy, pick a model from GET /v1/models, wire an agent loop to the router, or debug a chat call that 404s with `No active credentials for provider: …`, 400s with `Unknown combo model`, or returns an unexpected `model` field.
---

# openproxy-chat — chat completions through `/v1/chat/completions`

OpenProxy exposes an OpenAI-compatible chat surface on `127.0.0.1:4623`. One endpoint —
`POST /v1/chat/completions` — fronts 40+ providers, with model-level account fallback, combos,
and request/response format translation underneath.

This skill is about getting a chat call right: addressing a model, streaming it, using tools, and
reading back what the router actually did.

## 0 · Preconditions

The server must already be running. Check before anything else:

```bash
curl -sS http://127.0.0.1:4623/health
```

`/health` needs no auth and returns `{"status":"ok", …}`. If it is not `ok`, start the server first
— see the `openproxy` skill (`.agents/skills/openproxy/SKILL.md`) for install and start.

Two different credentials, and this trips people up:

| Surface | Credential | Notes |
|---|---|---|
| `/v1/*` (`/v1/chat/completions`, `/v1/models`) | Proxy API key | `Authorization: Bearer $OPENPROXY_API_KEY`. Mint with `openproxy key add <name> --auto`. |
| `/api/*` (dashboard, settings, combos, usage, logs) | Admin / management key | The key printed once by `openproxy server init`. A proxy key gets `401 invalid_api_key` here. |

For the `curl` examples in this skill you need the **proxy** key:

```bash
export BASE=http://127.0.0.1:4623
export OPENPROXY_API_KEY="$(openproxy key list --robot | jq -r '.data.keys[0].key')"
```

`openproxy key list` masks secrets in human output; `--robot` emits the full `key` value (handle it
as a secret — do not paste it into a commit, a log, or a shared transcript). To mint a fresh one
instead: `openproxy key add my-agent --auto`. The admin key from `server init` also authenticates
`/v1/*`, so one credential can cover both surfaces.

Sections that touch `/api/*` — the `openproxy chat send|stream`, `openproxy usage …`, and
`openproxy logs …` commands — need the **admin** key specifically.

If `/v1/*` answers an unauthenticated request with `404` instead of `401`, the install has
`requireApiKey` off — `/v1` is open on loopback. That is a settings choice, not a bug; see
"Auth" in §8.

## 1 · Pick a model from `GET /v1/models`

Never guess a model id. Read the list the router is actually advertising:

```bash
curl -sS "$BASE/v1/models" -H "Authorization: Bearer $OPENPROXY_API_KEY"
```

```json
{
  "object": "list",
  "data": [
    {"id": "ollama/glm-4.7-flash", "object": "model", "owned_by": "ollama", "root": "glm-4.7-flash"},
    {"id": "dbg-combo",           "object": "model", "owned_by": "combo",  "root": "dbg-combo"}
  ]
}
```

`owned_by` tells you which of the two addressing forms applies:

- `owned_by: "combo"` → a **combo**. Use the id **bare**: `"model": "dbg-combo"`.
- anything else → a **provider model**. Use it with its prefix: `"model": "ollama/glm-4.7-flash"`.

Only models that are *active* and *not disabled* on the Providers page appear here, so a model you
know exists but do not see is disabled — enable it in the dashboard under
`http://127.0.0.1:4623/dashboard/providers`.

Resolve any single id to see exactly what it routes to:

```bash
curl -sS "$BASE/v1/models/info?id=ollama/glm-4.7-flash" -H "Authorization: Bearer $OPENPROXY_API_KEY"
# {"id":"ollama/glm-4.7-flash","provider":"ollama","model":"glm-4.7-flash",
#  "routeKind":"Direct","owned_by":"ollama","kind":"llm","endpoint":"/v1/chat/completions", …}

curl -sS "$BASE/v1/models/info?id=dbg-combo" -H "Authorization: Bearer $OPENPROXY_API_KEY"
# {"id":"dbg-combo","provider":null,"model":"dbg-combo","routeKind":"Combo","owned_by":""}
```

`routeKind` is the ground truth for §3. `endpoint` is the kind→path table: an `llm` or
`imageToText` model posts to `/v1/chat/completions`; an `embedding` model posts to `/v1/embeddings`;
`tts` → `/v1/audio/speech`; `image` → `/v1/images/generations`.

Filter by kind with `GET /v1/models/<kind>` — `image`, `tts`, `stt`, `embedding`,
`image-to-text`, `web`. There is deliberately **no `llm` arm** in `list_models_by_kind`
(`src/server/api/v1_models.rs`), so `GET /v1/models/llm` 404s: an unrecognised kind is
resolved as a model *id* lookup. Use `GET /v1/models` (no kind), which returns `llm` only.

## 2 · Model addressing — the prefix convention

The `model` string is resolved by `get_model_info` (`src/core/model/mod.rs`). The rules, in order:

1. **`combo:<name>`** → always routed as a combo, even if `<name>` is also a provider model. This is
   the unambiguous form; use it whenever a name is ambiguous.
2. **`<provider>/<model>`** → routed directly. The prefix is the provider id (e.g. `ollama`,
   `anthropic`, `openai`). For a custom provider node the prefix is the node's **prefix**, and
   credentials are keyed by the node's **UUID** — see `get_model_info` and the note in the
   `openproxy` skill §4.
3. **bare name** → resolved as, in order:
   - a **combo** with that exact name, or
   - a **model alias** from settings, or
   - a built-in alias (`grok-build`), or
   - a last-resort provider guess from the family prefix: `claude-*` → `anthropic`,
     `gemini-*` → `gemini`, `gpt-*` / `o1` / `o3` / `o4` → `openai`, `deepseek-*` → `openrouter`,
     everything else → `openai`.

Step 3's fallback is why an unqualified `gpt-4o-mini` reaches `openai` even when you only have
credentials for something else. **Always pass the `provider/model` form** unless you mean a combo.

The model string also strips a few annotations before resolution: Claude Code's `<model>[1m]`
context marker, and a `(level)` / `-level` thinking suffix. The real 1M capability travels in the
`anthropic-beta` header, which is forwarded untouched.

## 3 · The request

Minimum body:

```bash
curl -sS "$BASE/v1/chat/completions" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{
        "model": "ollama/glm-4.7-flash",
        "messages": [{"role": "user", "content": "Reply with exactly: pong"}]
      }'
```

```json
{
  "id": "chatcmpl-…",
  "object": "chat.completion",
  "model": "glm-4.7-flash",
  "choices": [
    {"index": 0, "message": {"role": "assistant", "content": "pong"}, "finish_reason": "stop"}
  ],
  "usage": {"prompt_tokens": 12, "completion_tokens": 2, "total_tokens": 14}
}
```

A missing or empty `model` is a `400 {"error":{"code":"bad_request","message":"Missing model"}}`.

The same handler is also mounted at `/chat/completions` and `/v1/v1/chat/completions`, so a client
whose base URL is already the bare host (or already ends in `/v1`) still lands on one code path.
`/v1/chat/completions` is the canonical path — use it.

### The same call from the CLI

`openproxy chat` drives the running server. It requires a live server — there is no offline mode.

```bash
openproxy chat models                              # GET /v1/models, first 20 ids + count
openproxy chat send   --model ollama/glm-4.7-flash --prompt "ping"
echo "ping" | openproxy chat send --model my-combo --prompt -
openproxy chat send   --model my-combo --prompt "hi" --system "be terse"
openproxy chat stream --model my-combo --prompt "hi"   # streams until Ctrl+C
```

`--prompt` defaults to `-`, so piping on stdin is the default path. Add `--robot` to any of these
for a stable `openproxy.v1.*` envelope instead of human output — `chat stream --robot` emits one
NDJSON line per chunk with schema `openproxy.v1.chat.event`, and `chat send --robot` emits
`openproxy.v1.chat.response`.

**Auth differs by subcommand, which is a real trap.** `chat models` hits `/v1/models` and works
with a proxy API key. `chat send` and `chat stream` post to `/api/dashboard/chat/completions`, which
is guarded by the **admin/management key** — with only a proxy key they fail
`401 invalid_api_key`. That dashboard surface also normalizes a bare model name by prefixing it when
exactly one active connection advertises that model. For scripted work against a different host, or
to control the exact bytes on the wire, use `curl` against `/v1/chat/completions` with a proxy key.

## 4 · Streaming

Set `"stream": true`. The response comes back as `text/event-stream` with OpenAI SSE framing:

```
data: {"choices":[{"delta":{"content":"hel"},"index":0}]}

data: {"choices":[{"delta":{"content":"lo"},"index":0}]}

data: [DONE]

```

```bash
curl -N -sS "$BASE/v1/chat/completions" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"ollama/glm-4.7-flash","messages":[{"role":"user","content":"count to 3"}],"stream":true}'
```

Things that bite:

- **Frames are `data: ` prefixed and blank-line separated; the stream ends with a literal
  `data: [DONE]`.** A client that splits on newlines instead of blank lines will break.
- **Streaming is the default when `stream` is absent.** `resolve_stream_flags` sets
  `stream = (body.stream != false)`. Send `"stream": false` explicitly for a buffered JSON reply.
- **Some providers force streaming upstream even when you did not ask.** When that happens the
  router aggregates the SSE and returns a single JSON body (`sse_to_json`), so the shape you get
  back still matches what you requested.
- **`Accept: application/json` flips a non-`stream:true` request to non-streaming** — but an
  explicit `"stream": true` always wins.
- **Gemini / Antigravity / GeminiCli sources always stream**, whatever `stream` says.
- Image-generation models on `antigravity` / `gemini-cli` are forced non-streaming.

## 5 · Tools and `tool_calls`

`tools` and `tool_calls` pass through the OpenAI dialect unchanged, and are **translated** when the
target provider speaks a different dialect (Claude `tools` / `tool_use`, Gemini `functionDeclarations`,
Kiro, Cursor, Codex, Ollama, CommandCode). You send the OpenAI shape; the router handles the rest.

Declare tools:

```bash
curl -sS "$BASE/v1/chat/completions" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{
        "model": "ollama/glm-4.7-flash",
        "messages": [{"role": "user", "content": "What is the weather in Osaka?"}],
        "tools": [{
          "type": "function",
          "function": {
            "name": "get_weather",
            "description": "Get current weather for a city",
            "parameters": {
              "type": "object",
              "properties": {"city": {"type": "string"}},
              "required": ["city"]
            }
          }
        }],
        "tool_choice": "auto"
      }'
```

The assistant replies with `choices[0].message.tool_calls` — an array of
`{id, type: "function", function: {name, arguments}}` where `arguments` is a **JSON string**, not a
parsed object. Parse it before using it.

Feed the result back as a `role: "tool"` message carrying the matching `tool_call_id`:

```json
{
  "model": "ollama/glm-4.7-flash",
  "messages": [
    {"role": "user", "content": "What is the weather in Osaka?"},
    {"role": "assistant", "tool_calls": [
      {"id": "call_abc", "type": "function",
       "function": {"name": "get_weather", "arguments": "{\"city\":\"Osaka\"}"}}
    ]},
    {"role": "tool", "tool_call_id": "call_abc", "content": "18C, light rain"}
  ],
  "tools": [ /* re-send the same tools array */ ]
}
```

Re-sending `tools` on the follow-up turn is required — the router has no memory of the previous
request.

In a stream, tool arguments arrive fragmented across `delta.tool_calls[].function.arguments`; the
`index` field pairs fragments across chunks. Concatenate before parsing. Empty
`delta.tool_calls: []` arrays — which some providers emit in every chunk — are stripped before
reaching you, so a `!= null` check on `delta.tool_calls` behaves as OpenAI SDKs expect.

Two provider-specific behaviors, both intentional:

- **TTS models drop `tools` and every `role: "tool"` message** before dispatch.
- **Claude-dialect clients get their tools de-duplicated** before dispatch.

## 6 · Combos and fallback

A **combo** is a named list of `provider/model` entries dispatched as one. It is addressed as a bare
model name, and is what makes a chat call survive a provider going down mid-session.

```bash
openproxy combo list                    # existing combos
openproxy combo get my-combo            # membership
openproxy combo test my-combo           # dry-run the expansion, verify reachability
openproxy combo create --name my-stack \
  --models "openai/gpt-4o,anthropic/claude-3-5-sonnet" \
  --strategy fallback                   # fallback (default) | round-robin | sticky-round-robin
```

Call it with the combo name, bare or explicit:

```bash
-d '{"model":"my-stack","messages":[{"role":"user","content":"ping"}]}'
-d '{"model":"combo:my-stack","messages":[{"role":"user","content":"ping"}]}'
```

A name that is neither a combo, an alias, nor a prefixed provider model does **not** produce
"Unknown combo model" by accident — that 400 only comes from the `combo:` form naming a combo that
does not exist or is disabled:

```bash
# 400 {"error":{"code":"bad_request","message":"Unknown combo model"}}
-d '{"model":"combo:no-such-combo", …}'
```

### How fallback shows up in the response

When a combo member fails, the router tries the next member. **The response body's `model` field
names the member that actually answered**, not the combo you asked for. That is the primary signal:

```
request  "model": "my-stack"   →  response  "model": "gpt-4o"     (second member served it)
```

There is no separate "which member served" field or header — read `model`.

To see the full picture per request, check usage. The history rows carry `provider` and `model`
per request, so a combo's members appear as separate rows:

```bash
openproxy usage history --limit 20           # model / provider / cost per row
openproxy usage history --limit 20 --robot   # same, as openproxy.v1.usage.history
openproxy usage logs                         # paginated combined activity log
openproxy logs tail --follow                 # live NDJSON log stream
```

These talk to `/api/usage/*` and `/api/observability/*`, so they need the **admin/management key**,
not a proxy key — with only a proxy key they return `401 invalid_api_key`. Export the right one
first: `export OPENPROXY_API_KEY="$(openproxy --robot server init | jq -r '.data.admin_key.key')"`. (No code writes `~/.openproxy/admin.key`; only the bootstrap skill tells you to create it by hand, so reading it silently yields an empty key and a cascade of 401s.)

`GET /v1/usage` and `GET /api/usage/history` serve the same data over HTTP. A response that carries
no token usage at all (most SSE streams) records **no** usage row — absence of a row is not proof
the call did not happen.

When every member of a combo fails, the members that errored are auto-quarantined with a cooldown
drawn from the failure class, so the *next* request skips them rather than re-attempting a known-broken
member. That is why a failing combo gets *faster* to error, not slower, on repeat calls.

## 7 · Error reference

Every error is the OpenAI envelope: `{"error": {"message", "type", "code"}}`. Chat-specific arms:

| Status | `error.code` | `error.message` | Meaning |
|---|---|---|---|
| 400 | `bad_request` | `Missing model` | `model` absent or empty. |
| 400 | `bad_request` | `Unknown combo model` | `combo:<name>` names a combo that does not exist or is disabled. |
| 401 | `invalid_api_key` | — | Bad/expired bearer on a `/api/*` surface. |
| 403 | _(upstream's)_ | provider's own wording | Passed through from the provider — quota, entitlement, or account ban. |
| 404 | `model_not_found` | `No active credentials for provider: <provider>` | See below. |
| 429 | _(upstream's)_ | provider's own wording | Passed through from the provider. |
| 502 | _(upstream's)_ | provider's own wording | The provider **has** a credential and failed while serving. |
| 503 | `service_unavailable` | `[<provider>/<model>] <last error> (reset after 4m 12s)` | Every account for that provider is rate-limited / cooling down. `Retry-After` header is set. |
| 503 | `service_unavailable` | `All accounts for <provider>/<model> are unavailable` | Accounts were tried and all exhausted. |

Note the split: the `400` / `404` / `503` rows are **built by OpenProxy**, so their `code` and
`message` are stable. The `403` / `429` / `502` rows carry the **upstream provider's** body verbatim
(served back as JSON), so their `code` and `message` are whatever that provider emits and will not
match this table.

### `404 No active credentials for provider: X` — what it actually means

Verified verbatim against a running server:

```bash
$ curl -sS -w '\nHTTP %{http_code}\n' "$BASE/v1/chat/completions" \
    -H "Authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
    -d '{"model":"totally-unknown-provider/gpt-4o-mini","messages":[{"role":"user","content":"hi"}]}'
{"error":{"code":"model_not_found","message":"No active credentials for provider: totally-unknown-provider","type":"invalid_request_error"}}
HTTP 404
```

Despite the 404 and the `model_not_found` code, **this is not a routing or URL problem** — the model
string resolved fine. The precise meaning is: *for that provider, no connection was ever attempted.*

The router picks the status from what the credential lookup reported, not from the model name:

- **nothing excluded** (no account was ever tried) → `404 No active credentials for provider: X`.
  There is no usable credential on file for `X` at all.
- **every account rate-limited / cooling down** → `503` + `Retry-After`, message
  `[<provider>/<model>] <last error> (reset after 4m 12s)`.
- **accounts were tried and all failed** → `503 All accounts for <provider>/<model> are unavailable`.

So when you see the 404, check the *provider*, not the model:

1. Is the prefix spelled right? A typo'd prefix is a provider that does not exist — same 404. Use
   `GET /v1/models` and copy an id verbatim.
2. Does that provider have a connection? `openproxy provider list`, or
   `GET /api/providers` with the **admin** key.
3. Is the connection active and does it hold a credential? A connection with `api_key: null` —
   an OAuth provider whose token has not been refreshed, or an API-key provider never filled in —
   is exactly this case. The request never reaches the upstream; the provider receives zero traffic.
4. Was the model disabled on the Providers page? That removes it from `/v1/models`, but a directly
   addressed `provider/model` still resolves — so a disabled model gives a *provider* error, not a
   404 about the model.

Note the asymmetry: **the same provider with a credential that then fails gives a `502`, not this
404.** If you are getting 404, adding credentials is the fix; debugging the upstream key is not.

## 8 · Auth

`/v1/chat/completions` requires a valid, **active** proxy API key only when the `requireApiKey`
setting is on — which is the default on a fresh install, and which falls back to `requireLogin` on
installs predating the field. When it is off, `/v1` accepts unauthenticated loopback requests (and
returns `404` for an unconfigured provider rather than `401`, since there is no auth check to fail).

```bash
openproxy key list                 # existing keys (secrets masked)
openproxy key add my-agent --auto  # mint a fresh op-… secret, prints it once
openproxy key disable my-agent     # revoke without deleting
```

Auth is read from `Authorization: Bearer <key>`, or `x-api-key: <key>`, or `x-goog-api-key: <key>`
(Gemini-style clients). `Authorization: Bearer …` is the one to use.

Forwarded headers (`X-Forwarded-For`, `X-Real-IP`, `X-Forwarded-Proto`, …) are **stripped** unless
`TRUST_PROXY=true`, because on a default single-process deploy they are spoofable and are used for
rate limiting and IP logging.

## 9 · Verifications

Run these in order. Each one isolates a different layer.

```bash
# 1. Server is up (no auth)
curl -sS "$BASE/health" | head -c 80

# 2. Endpoint advertises itself (no auth)
curl -sS "$BASE/v1"        # → {"version":"v1","endpoints":["/v1/chat/completions", …]}

# 3. Your bearer works on /v1
curl -sS -o /dev/null -w '%{http_code}\n' \
  "$BASE/v1/models" -H "Authorization: Bearer $OPENPROXY_API_KEY"

# 4. The model you picked is real, and you know what it resolves to
curl -sS "$BASE/v1/models/info?id=my-model-or-combo" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY"

# 5. Non-streaming round trip
curl -sS "$BASE/v1/chat/completions" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -d '{"model":"my-model","messages":[{"role":"user","content":"ping"}],"stream":false}'

# 6. Streaming round trip — must end with data: [DONE]
curl -N -sS "$BASE/v1/chat/completions" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -d '{"model":"my-model","messages":[{"role":"user","content":"ping"}],"stream":true}'

# 7. The request was actually recorded
openproxy usage history --limit 5
```

If step 5 returns a body but no `choices`, the failure is upstream — read `error.message`, it carries
the provider's own wording with the router's prefixes (`Error from provider:`, `Upstream error:`)
already stripped. If step 5 returns `404 No active credentials for provider: X`, go to §7.

To exercise the whole path **without a live provider**, the `x-openproxy-sim: mock` request header
short-circuits dispatch and renders a canned response in your request's own dialect. Any
`x-openproxy-sim-*` header bypasses the response cache, so a simulated reply can never be served to
a real request:

```bash
curl -sS "$BASE/v1/chat/completions" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -H 'x-openproxy-sim: mock' \
  -d '{"model":"ollama/glm-4.7-flash","messages":[{"role":"user","content":"hi"}]}'
```

Note the mock still resolves credentials and the model, so it will not mask a `404` or a `400`.

## 10 · Debugging an unexpected `model` field

The `model` you sent and the `model` you got back can differ for four distinct reasons. Check them
in this order:

1. **You sent a bare name and it resolved by inference.** `gpt-4o-mini` → `openai`; `claude-*` →
   `anthropic`. Send `provider/model` explicitly to remove the guesswork.
2. **You sent a combo.** The response names the member that served. This is correct behavior — see §6.
3. **The catalog remaps the model upstream.** A model with an `upstreamModelId` in the provider
   catalog is dispatched under that id, and the provider echoes it. `GET /v1/models/info?id=…`
   returns `upstreamModelId` when one is set.
4. **The provider echoed a different name than the one you sent** — common with OpenRouter-style
   gateways that report a canonical or dated model id. Nothing to fix.

## When _not_ to use this skill

- **Wiring a CLI tool** (Claude Code, Codex, Cursor, Cline) to the proxy — that is base-URL + bearer
  config, not chat semantics. See `.agents/skills/openproxy-cli-tools/SKILL.md`.
- **Combo construction, membership, strategies, round-robin vs fallback** — this skill covers calling
  a combo, not designing one. See `.agents/skills/openproxy-combos/SKILL.md`, and use
  `openproxy combo test <name>` to verify an expansion end to end.
- **Non-chat media** — images, TTS, STT, embeddings, web search, web fetch each have their own
  endpoint and skill under `.agents/skills/`.
- **Installing or starting the server** — see `.agents/skills/openproxy/SKILL.md`.
- **Anthropic-native (`/v1/messages`) or Responses-API (`/v1/responses`) clients** — those are
  separate handlers; the OpenAI dialect documented here is translated to whatever the target
  provider needs, so an OpenAI-shaped body is the portable choice.

## See also

- `.agents/skills/openproxy/SKILL.md` — install, start, auth bootstrap
- `.agents/skills/openproxy-combos/SKILL.md` — building and editing combos
- `docs/parity-9router.md` — intentional divergences from 9router, and the pipeline order
- `GET $BASE/v1` — the live endpoint list, on the running server
- `openproxy chat --help` / `openproxy chat <cmd> --help`
