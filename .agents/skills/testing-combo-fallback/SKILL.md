---
name: testing-combo-fallback
description: End-to-end test the OpenProxy combo fallback dispatch path (Rust BE + Astro FE) and seed the dashboard with mock data. Use whenever validating combo/provider/media changes or producing a UI tour.
---

# Testing OpenProxy combo fallback + seeding the dashboard

This skill captures the non-obvious bits learned while E2E-testing the combo
fallback dispatch fix in `src/server/api/chat.rs` and unblanking the four
dashboard pages in PR #8. Most setup is straightforward; the gotchas below are
what actually consumed time.

## Stack layout (local)

- Rust BE on `127.0.0.1:4623` (serves `/api/*` and `/v1/*`).
- Astro dashboard on `127.0.0.1:4624` (sidecar; the BE also serves the built
  dashboard at `/dashboard`, which is what most testing uses).
- Recommended: `npm run dev:stack` against an empty `DATA_DIR=/tmp/openproxy-e2e`.
  Empty directory means a fresh `db.json` and a freshly-generated management API
  key (printed to stdout).

For combo fallback specifically you also want a tiny mock OpenAI on a local port
that returns a canned `chat.completion`. A Python `http.server` BaseHTTPRequestHandler
emitting `{"id":"cmpl-mock-pass","choices":[{"message":{"content":"hello-from-pass"}}]}`
on `POST /v1/chat/completions` is enough.

## Toolchain prerequisites

The Rust BE uses crates that require **Rust 1.95.0** or newer (`hybrid-array` needs
`edition2024`). On a fresh box:

```
rustup update stable           # 1.95.0+ as of April 2026
sudo apt-get install -y libssl-dev pkg-config   # for openssl-sys
```

Astro dev needs an explicit host or it binds IPv6-only:

```
npx astro dev --port 4624 --host 127.0.0.1
```

## Auth: turn off `require_login` before scripted seeding

Default `db.json` has `settings.requireLogin == true`. With that on, every
`/api/*` request must carry a session cookie or Bearer token, and the dashboard
will silently render zero-state for things you _did_ seed. Disable it once the
stack is up:

```
curl -sS -X PATCH http://127.0.0.1:4623/api/settings \
  -H "authorization: Bearer $APIKEY" -H 'content-type: application/json' \
  -d '{"requireLogin":false}'
```

Management API key is the one printed at startup. Save it somewhere
(`/tmp/openproxy-apikey.txt` works) - many endpoints still want it as Bearer
even when require_login is off (e.g. `PATCH /api/settings`, `PUT /api/models`).

If the BE was started against an already-populated db where the key is unknown,
log in via cookie instead: `POST /api/auth/login` with `{"password":"123456"}`
(default), then use the `Cookie:` header for subsequent calls.

## Provider model in OpenProxy is two-tier

The one thing that will block a combo test until you understand it:

- **ProviderNode**: a registered provider _type_ (`type="openai-compatible"`,
  `"anthropic-compatible"`, etc.) with `name` + `baseUrl`. Created via
  `POST /api/provider-nodes`. Returns a UUID.
- **ProviderConnection**: an instance of a provider with `apiKey`, optional
  `baseUrl` override, priority, `name`. Created via `POST /api/providers`.
  The `provider` field on a connection is **either** a registered alias
  (`openai`, `anthropic`, `groq`, ...) **or** a node UUID for custom nodes.
- **Combo entries** must use the same identifier the connection uses:
  - For built-in providers: `"openai/gpt-4o"`, `"anthropic/claude-3-5-sonnet"`.
  - For a custom node: `"<node-uuid>/gpt-4o"`. **Not** the node's `name`.

The code path that enforces this is `DefaultExecutor::new` in
`src/core/executor/default.rs` (~lines 351-380). For a custom node it uses
`ProviderConfig::openai("")` (which respects the connection's custom baseUrl)
as long as the node's `type` is one of the `*-compatible` types. Anything else
falls back to the static `PROVIDER_CONFIGS` catalog and yields
`UnsupportedProvider(...)`. If you see that error in the BE log during a combo
test, the combo is referencing a non-registered name - switch the combo entry
to the node UUID.

### Minimal seed for combo fallback test

```
# Two custom nodes (one fails, one passes via mock)
NODE_FAIL=$(curl ... POST /api/provider-nodes -d '{"name":"e2e-fail","type":"openai-compatible","baseUrl":"http://127.0.0.1:9/v1"}' | jq -r .node.id)
NODE_PASS=$(curl ... POST /api/provider-nodes -d '{"name":"e2e-pass","type":"openai-compatible","baseUrl":"http://127.0.0.1:18080/v1"}' | jq -r .node.id)

# Connection per node so DefaultExecutor has credentials
curl ... POST /api/providers -d '{"provider":"'$NODE_FAIL'","name":"fail-conn","apiKey":"sk-test","defaultModel":"gpt-4o"}'
curl ... POST /api/providers -d '{"provider":"'$NODE_PASS'","name":"pass-conn","apiKey":"sk-test","defaultModel":"gpt-4o"}'

# Combo: fail first, pass second - fallback should hit pass
curl ... POST /api/combos -d '{"name":"mix1","models":["'$NODE_FAIL'/gpt-4o","'$NODE_PASS'/gpt-4o"]}'
```

Port 9 is reserved -> connection refused -> forces fallback to pass.

## Verifying combo dispatch end-to-end

The definitive proof the per-iteration re-resolve works:

1. `POST /v1/chat/completions` with `Authorization: Bearer $APIKEY` and
   `{"model":"mix1", "messages":[...], "stream":false}` -> expect HTTP 200,
   `id == "cmpl-mock-pass"`, `content == "hello-from-pass"`.
2. Mock server log -> exactly one `POST /v1/chat/completions` per request.
3. `/dashboard/console-log` (or BE stdout) shows two distinct PLAN lines per
   request: first the fail-node UUID, then the pass-node UUID. Both with
   `model=gpt-4o`. That alternation is the visible fingerprint of the
   re-resolve.

If the fix were absent, `combo_provider_str` would be `"unknown"` for both
iterations, `select_connection` would return `None`, and the response would be
a 4xx with `"No credentials for provider: unknown"` and zero mock hits.

## Other endpoints worth seeding for a full UI tour

- `POST /api/keys` - extra API keys.
- `POST /api/proxy-pools` - shape after PR #8: BE returns `{proxyPools: [...]}`
  with each pool having `isActive` + `proxyUrl`. The FE reads exactly this now,
  so seeded pools render under `/dashboard/proxy-pools`. (Pre-PR #8 the FE was
  reading `data.pools`/`pool.active`/`pool.description` and showing zero-state.)
- `PUT /api/models` - set model aliases (`{"model":"openai/gpt-4o","alias":"smart"}`).
- `POST /api/media-providers` - requires `mediaType` field, value must be one of
  `[tts, stt, embedding, image, search]`. `webSearch`/`webFetch`/`video`/`music`
  appear in `KNOWN_KINDS` but the create handler rejects them - only the 5
  above are actually accepted by `add_media_provider`.
- `POST /api/provider-nodes` for non-built-in providers. Use type
  `openai-compatible` or `anthropic-compatible` so `DefaultExecutor` can dispatch.

## Dashboard FE quick-debug recipe

If a dashboard page renders only the header (or empty state) despite seeded
data, do **not** reseed - debug FE first. PR #8 found four such bugs in minutes
with this flow:

1. Open the page, F12 -> Console. Astro hydration errors are loud:
   - `ReferenceError: process is not defined at <Component>.tsx:<line>` -> the
     component reads `process.env.NEXT_PUBLIC_*` at module scope (Next.js
     residue). **Fix**: replace with `import.meta.env.PUBLIC_*`. The browser
     bundle has no `process`. Beware barrel files: a single bad component
     pulled via `@/components/cli-tools` index will crash *every* page that
     imports the barrel (this is why MITM died too).
   - `SyntaxError: ... does not provide an export named 'X'` -> a runtime
     `import { X }` for something that's actually a type-only export. PR #8
     hit this with `@xyflow/react@12`'s `Node` / `Edge`. **Fix**: split into
     `import type { Node, Edge } from '@xyflow/react'`.
2. Network tab. Look for 4xx on `/api/*` calls the page makes. Two known
   shape pitfalls:
   - Listing connections is `GET /api/providers` (returns `{connections: [...]}`).
     `GET /api/providers/client` is **different** - it returns client metadata
     (`{clientId, version, ...}`). Pages calling the wrong one render empty.
   - The Quota page (`ProviderLimits`) gates rows by both OAuth providers and
     an `USAGE_APIKEY_PROVIDERS` whitelist (defined in
     `web/src/components/usage/ProviderLimits/index.tsx`). Apikey-auth providers
     like `glm` and `minimax` only render if they're in that whitelist *and*
     the filter actually checks it (helper: `isUsageEligible`). A naive
     `authType === "oauth"` filter hides apikey providers entirely.
3. If the page is conceptually fine but data is missing, only then reseed.

## Recording / annotation conventions

For combo dispatch tests, useful test_start phrasings:
- `It should fallback from unreachable sub-model to mock and return its payload`
- `It should populate every dashboard page with seeded mock data`

For dashboard FE regression sweeps, use one `It should render <Page>` per page
and make the assertion describe a *seeded-specific* observable (e.g.
`Stats 11/88/33 + topology with pass-conn/fail-conn`). Generic assertions like
"page loaded" are not useful since the bug class is a rendered-but-blank body.

Assertion phrasings should match the 5 numbered assertions above (HTTP 200,
content, id, mock hit count, BE log alternation). For the regression sweep,
label each page check explicitly with the page name.

## Devin Secrets Needed

None for this test - everything is local. The only credential involved is the
management API key which is generated freshly on each empty-DATA_DIR boot and
printed to BE stdout.
