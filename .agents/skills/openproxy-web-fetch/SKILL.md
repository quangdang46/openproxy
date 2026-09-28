---
name: openproxy-web-fetch
description: Fetch a web page through OpenProxy and get back extracted text. Use whenever the user wants to read a URL through the proxy, route page fetches to Firecrawl / Jina Reader / Tavily / Exa / Ollama, fall back across fetch accounts, or debug `POST /v1/web/fetch` — including the `/v1/fetch` path that the model kind map advertises but the router does not serve.
---

# openproxy — web fetch

`POST /v1/web/fetch` turns a URL into extracted text. It is the same router, the same
account-fallback loop, and the same combo machinery as `/v1/chat/completions`, aimed at
pages instead of prompts.

This skill covers:

1. The one real path (`POST /v1/web/fetch`) and the `/v1/fetch` that only exists in metadata
2. Registering a fetch provider
3. Calling it — curl and the `openproxy` CLI
4. The request/response contract
5. Combos and account fallback
6. Verifications and failure modes

## 1 · The path — `/v1/web/fetch`, not `/v1/fetch`

**The fetch route is `POST /v1/web/fetch`** (`src/server/api/web_fetch.rs:28-33`, mounted at
`src/server/api/mod.rs:114`). `GET /v1` advertises it correctly (`src/server/api/mod.rs:459`).

There is a real, confirmed discrepancy you should not fall into:

| Source | Value | Reality |
|---|---|---|
| `kind_endpoint("webFetch")` — `src/server/api/v1_models.rs:36` | `/v1/fetch` | **404.** No route is registered for it. |
| Route table in `src/server/api/web_fetch.rs:30` | `/v1/web/fetch` | The live path. |
| `GET /v1` index — `src/server/api/mod.rs:459` | `/v1/web/fetch` | The live path. |

`/v1/fetch` appears in exactly two places in the tree: the `kind_endpoint` arm and the unit
test that pins it (`v1_models.rs:1330`, `kind_endpoint_matches_9router`). Nothing serves it.

```bash
$ curl -s -o /dev/null -w '%{http_code}\n' -X POST http://127.0.0.1:4623/v1/fetch \
    -H 'content-type: application/json' -d '{"provider":"firecrawl","url":"https://example.com"}'
404
```

The wrong value surfaces as the `endpoint` field of `GET /v1/models/info` (`v1_models.rs:889`).
It is **latent today, not user-visible**: no catalog model id is literally `fetch`, so
`catalog.find_model(provider, "fetch")` returns `None` and the merge block that emits
`endpoint` is skipped. Do not rely on it either way — call `/v1/web/fetch`.

Two other path facts:

- Only `POST` and `OPTIONS` are routed. `GET /v1/web/fetch` returns **405**.
- No `/v1/v1/web/fetch` double-prefix alias exists for this route (unlike chat, embeddings,
  search, and the other media routes). `/v1/web/fetch` is the only spelling.

## 2 · Register a fetch provider

Dispatch matches on the connection's `provider` field, so the value must be one of the
canonical ids below — a short alias is resolved on the *request* side only, never on the
stored row.

| Canonical id | Short alias | Upstream | Upstream body | Upstream auth header |
|---|---|---|---|---|
| `firecrawl` | `fc` | `https://api.firecrawl.dev/v1/scrape` | `{url, formats:[format]}` | `Authorization: Bearer <key>` |
| `jina-reader` | `jina` | `https://r.jina.ai/` | `{url}` | `Authorization: Bearer <key>` |
| `tavily` | `tv` | `https://api.tavily.com/extract` | `{urls:[url], extract_depth:"basic"}` | `Authorization: Bearer <key>` |
| `exa` | `exa` | `https://api.exa.ai/contents` | `{ids:[url], text:true}` | `x-api-key: <key>` |
| `ollama` | `ol` | `https://ollama.com/api/web_fetch` (overridable) | `{url}` | `Authorization: Bearer <key>` |

Anything else resolves to itself and fails at credential selection with
`400 {"error":"No credentials for provider: <id>"}` — the credential check runs *before* the
`Unsupported web fetch provider` check in `build_fetch_request`, so an unknown id reports
missing credentials, not an unsupported provider.

Add one with the normal provider command. Only `provider` and `name` are required:

```bash
export OPENPROXY_API_KEY="$(cat ~/.openproxy/admin.key)"

openproxy provider add firecrawl-main \
  '{"provider":"firecrawl","apiKey":"fc-…","isActive":true,"priority":0}'
```

Two constraints worth knowing:

- **`openproxy media providers add --kind webFetch` does not work.** The handler whitelists
  `["tts","stt","embedding","image","search"]` (`src/server/api/media_providers.rs:194`) and
  answers `400 Invalid media_type`. If you want the row to also appear in the dashboard's web
  list, add it with `--kind search` — `mediaType` does not affect fetch dispatch at all.
- **Self-hosted Ollama:** set `providerSpecificData.baseUrl` to override the upstream
  (`web_fetch.rs:487-493`). The target `url` is still SSRF-checked; the upstream is not.

## 3 · Call it

### curl

```bash
BASE=http://127.0.0.1:4623
export OPENPROXY_API_KEY="$(cat ~/.openproxy/admin.key)"

curl -sS -X POST "$BASE/v1/web/fetch" \
  -H "authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"provider":"firecrawl","url":"https://example.com"}'
```

```json
{
  "provider": "firecrawl",
  "url": "https://example.com",
  "title": "Example Domain",
  "content": { "format": "markdown", "text": "# Example Domain\n\n…", "length": 41 },
  "metadata": { "author": null, "published_at": null, "language": null },
  "usage": { "fetch_cost_usd": null },
  "metrics": { "response_time_ms": 1, "upstream_latency_ms": 0 }
}
```

Truncate the output, and ask for plain text:

```bash
curl -sS -X POST "$BASE/v1/web/fetch" \
  -H "authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -d '{"provider":"firecrawl","url":"https://example.com","format":"text","maxCharacters":2000}' \
  | jq -r '.content.text'
```

`provider` may also be sent as `model` (serde alias) — the dashboard sends `model` because for
this endpoint the provider *is* the model.

### CLI

```bash
openproxy media web fetch https://example.com --provider firecrawl
openproxy media web fetch https://example.com --provider firecrawl --format text --max-chars 2000
```

Against a remote server, add the global `--url` / `--api-key` pair (or `OPENPROXY_URL` /
`OPENPROXY_API_KEY`):

```bash
openproxy --url "$BASE" --api-key "$OPENPROXY_API_KEY" --robot \
  media web fetch https://example.com --provider firecrawl \
  | jq -r '.data.content.text'
```

**Always use `--robot` and read `.data.content.text`.** Without `--robot` the command prints
an empty line: `run_web_fetch` reads `payload["content"]` as a *string*
(`src/cli/media.rs:760`) while the server returns `content` as an *object* with a nested
`text`. `Value::as_str` yields `None` and it falls back to `""`. The robot envelope
(`openproxy.v1.media.web.fetch`) carries the whole payload and is unaffected — the CLI test
passes only because it mocks a flat `content` string, which is not the shape the real handler
emits.

## 4 · Contract

### Request

| Field | Type | Required | Notes |
|---|---|---|---|
| `provider` (alias `model`) | string | yes | Provider id, short alias, or a combo name. |
| `url` | string | yes | Parsed with `url::Url`; must survive the SSRF guard. |
| `format` | string | no | `markdown` (default) \| `html` \| `text`. Passed to the upstream, not enforced locally. |
| `maxCharacters` | usize | no | Truncates the extracted text; `content.length` reflects the cut. |

### Response

One normalized envelope for every provider (see `normalize_fetch_response`,
`web_fetch.rs:592-704`). `content.text` is the payload; `title` is populated where the
upstream exposes one (Firecrawl metadata, Jina's leading `Title:` line or `# ` heading, Exa
result title) and is `null` otherwise. `metadata.author` / `published_at` / `language` and
`usage.fetch_cost_usd` are always `null` placeholders today.

### Errors

Errors are a bare JSON string, **not** an object: `{"error":"<message>"}`.

| Status | When | Example message |
|---|---|---|
| 400 | missing / invalid / blocked `url` | `Missing required field: url`, `Invalid URL format` |
| 400 | missing provider | `Missing required field: provider (or model)` |
| 400 | no usable credential | `No credentials for provider: firecrawl` |
| 400 | SSRF guard | `URL resolves to a private/internal IP address: 127.0.0.1` |
| 401 | auth gate (see §5) | `{"error":{"code":"invalid_api_key","message":"Missing API key",…}}` |
| 404 | you used `/v1/fetch` | — |
| 405 | wrong method | — |
| 502 | client-pool or JSON parse failure | `JSON parse error: …` |
| 504 | upstream exceeded the 30 s timeout | `Request timed out` |
| upstream | non-2xx status and body are passed through | whatever the provider said |

### SSRF guard

`check_private_ip` (`web_fetch.rs:556-590`) rejects loopback, RFC1918, link-local,
unspecified, and multicast addresses for IPv4; loopback, unspecified, multicast, and
`fc00::/7` for IPv6. Hostnames are resolved with `lookup_host` first, so a public name
pointing at a private address is caught too. The guard runs on the **target `url` only** —
the configured upstream base URL is not checked.

## 5 · Auth

Auth is conditional on `settings.require_login` (`web_fetch.rs:76-82`), which **defaults to
`true`** (`src/types/mod.rs:771`) and can be turned off per install. On a default install a
bearer is required:

```bash
curl -sS -X POST "$BASE/v1/web/fetch" -H 'content-type: application/json' \
  -d '{"provider":"firecrawl","url":"https://example.com"}'
# → 401 {"error":{"code":"invalid_api_key","message":"Missing API key","type":"authentication_error"}}
```

Use the admin key (what `openproxy server init` emits) or any key from `openproxy key add`.
`GET /v1/models` needs no key, so discovery stays open even when fetch is gated.

CORS is wide open — `OPTIONS` answers 200 with `access-control-allow-origin: *`,
`allow-methods: *`, `allow-headers: *`, and every response carries `allow-origin: *`.

## 6 · Combos and fallback

`provider` may be a **combo name**, resolved before provider dispatch (`web_fetch.rs:115`).
Matching is by name only, so the name must contain no `/`, the combo must be active, and it
must have members. `kind` is irrelevant to dispatch, so a combo created with plain
`openproxy combo create` works even though the CLI never sets `kind`:

```bash
openproxy combo create --name webcombo --models "firecrawl,jina-reader" --strategy fallback

curl -sS -X POST "$BASE/v1/web/fetch" \
  -H "authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -d '{"provider":"webcombo","url":"https://example.com"}'
```

The combo strategy comes from `settings.comboStickyRoundRobinLimit` for sticky round-robin
(`web_fetch.rs:171-176`), matching chat's behaviour.

**Per-account fallback** is the same loop as chat: credentials are selected by lowest
`priority` among active connections with a non-empty `api_key` or `access_token`; a failed
attempt marks that connection unavailable (cooldown, backoff level, `test_status`) and
retries the next one; the first success clears the error state. `comboStickyRoundRobinLimit`
is honoured, unlike the older hardcoded `1`.

## 7 · Discovery

With an active connection to a catalogued fetch provider, the model list advertises the
synthetic `fetch` model:

```bash
curl -sS "$BASE/v1/models/web" | jq '.data[] | {id, kind}'
# → { "id": "firecrawl/fetch", "kind": "webFetch" }
```

`firecrawl`, `jina-reader`, `tavily`, and `exa` carry `webFetch` in the embedded catalog. On
an install with **no** active provider connections the list is empty — the catalog fallback
only enumerates `providerModels` entries, and the four fetch providers have none:

```bash
curl -sS "$BASE/v1/models/web"
# → {"object":"list","data":[]}
```

Register a provider first (§2) before expecting anything here.

## 8 · Verifications

```bash
# The real route is reached (400 = handler ran), the wrong one is not (404)
curl -s -o /dev/null -w 'wrong path:     %{http_code}\n' -X POST "$BASE/v1/fetch" \
  -H 'content-type: application/json' -d '{}'                  # 404
curl -s -o /dev/null -w 'web/fetch route: %{http_code}\n' -X POST "$BASE/v1/web/fetch" \
  -H "authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' -d '{}'  # 400

# Preflight
curl -s -o /dev/null -D - -X OPTIONS "$BASE/v1/web/fetch" | grep -i 'access-control'

# The advertised index
curl -sS "$BASE/v1" | jq '.endpoints' | grep web

# End to end through a real provider
curl -sS -X POST "$BASE/v1/web/fetch" \
  -H "authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -d '{"provider":"firecrawl","url":"https://example.com"}' | jq '.content.length'

# SSRF guard fires
curl -sS -X POST "$BASE/v1/web/fetch" -H "authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"provider":"firecrawl","url":"http://127.0.0.1:8080/"}'
# → 400 URL resolves to a private/internal IP address: 127.0.0.1
```

A CLI write is not visible to a running server until it restarts — `openproxy provider add`
and `openproxy combo create` write the store, but the running process serves its own
snapshot. If a change appears to be ignored, `openproxy server stop` then `server start
--detach --no-open` and retry.

## Common failure modes

| Symptom | Cause and fix |
|---|---|
| `404` on the fetch | You used `/v1/fetch`. Use `/v1/web/fetch` — see §1. |
| `405` on the fetch | Wrong method. `POST` and `OPTIONS` only. |
| `400 No credentials for provider: X` | No active connection whose `provider` field is exactly `X`. Short aliases are resolved on the request side only, so the stored row needs the canonical id. |
| `400 Invalid media_type` from `media providers add` | `--kind webFetch` is not whitelisted. Use `openproxy provider add`, or `--kind search` for dashboard visibility. |
| CLI prints an empty line | Non-robot output reads `content` as a string. Use `--robot` and `.data.content.text` — see §3. |
| `401` on every call | `require_login` defaults to true. Send a bearer. |
| `400 URL resolves to a private/internal IP address` | SSRF guard, including after DNS. Not configurable. |
| `504 Request timed out` | 30 s hard cap on the upstream call. |
| `content.text` empty but status 200 | The upstream returned a shape the normalizer did not recognise for that provider. |
| Combo change has no effect | Restart the server so it reloads the combo. |

## Not this skill

- **Web *search*** (query → links) is a different endpoint: `POST /v1/search`, and
  `openproxy media search --provider X --query "…"`. Its body needs `model` set to
  `<provider>/search` plus `query`; the provider alone is not enough. `/v1/web/fetch` needs
  `provider` plus `url` and takes no `query`.
- **Chat with a web-enabled provider** goes through `/v1/chat/completions`.
- General install, provider, combo, and CLI-tool wiring: `.agents/skills/openproxy/SKILL.md`.
