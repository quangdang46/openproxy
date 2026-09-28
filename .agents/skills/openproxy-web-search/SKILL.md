---
name: openproxy-web-search
description: Run web search and web fetch through OpenProxy's local router on 127.0.0.1:4623 — pick between the raw `/v1/search` endpoint and the chat-completion-shaped `/v1/chat/search` endpoint, configure the 14 dedicated search providers and the 10 search-via-chat providers, and read back the response envelopes. Use whenever the user asks to search the web through openproxy, wire a search tool into the router, add a search credential (tavily, serper, exa, brave, …), make a chat model do grounded web search (gemini, openai, xai, kimi, minimax, perplexity), or fetch a URL to markdown through the proxy.
---

# openproxy — web search & web fetch through the proxy

OpenProxy exposes web search on **two different POST endpoints** that share one provider
dispatch but differ in how you address a provider, what they return, whether they expand
combos, and whether they fail over across providers. Picking the wrong one is the single
most common mistake here, so read section 1 before writing a request.

Both endpoints are authenticated the same way (section 2) and both take the same optional
search parameters (section 3).

## 0 · Preconditions

```bash
curl -sS http://127.0.0.1:4623/health
```

If that fails: `openproxy server start --detach --no-open`.

Export a key. `/v1/*` needs **any** active OpenProxy API key (the admin key or one minted
with `openproxy key add`), passed as a bearer:

```bash
export OPENPROXY_API_KEY="$(cat ~/.openproxy/admin.key)"
# or mint a scoped key (name is positional; --auto generates the secret):
# export OPENPROXY_API_KEY="$(openproxy --robot key add agent-search --auto | jq -r '.data.key')"
```

Authentication on both search routes is **conditional** (`src/server/api/media.rs:835`,
`src/server/api/chat_search.rs:218`): a bearer is required only when
`settings.require_api_key` is on. It is never gated on the dashboard's login flag. Send
the bearer anyway — it is harmless when the setting is off and required when it is on.

## 1 · Pick the endpoint

| | `POST /v1/search` | `POST /v1/chat/search` |
|---|---|---|
| Alias | `/v1/v1/search` | `/v1/v1/chat/search` |
| Handler | `media::search` → `generic_media_handler` | `chat_search::handle_search_completions` |
| How you name a provider | **`model: "<provider>/<model>"`** — required, `400 Missing model` without it | **`model` or `provider`**, a bare search-provider id or alias; unknown values silently become `serper` |
| Query field | `query` only | `query`, or derived from the last `messages[].content` of role `user` |
| Response | raw result set | `chat.completion` + `search_results` + `search_metadata` |
| Combos | **no** — `model: "combo:<name>"` → `400 Combos not supported for search` | **yes** — pass the bare combo name in `model` |
| Cross-provider failover | none; rotates *accounts* of one provider on `401/403/429` only | yes — walks a 15-provider canonical order |
| Reaches search-via-chat providers | yes | only `antigravity` (see section 5) |

Both call the same `dispatch` in `src/core/media/search/mod.rs:34`, so a dedicated search
adapter always wins over the chat-search path for the same provider id.

Rule of thumb: use **`/v1/search`** for programmatic search (a search tool, an agent loop, a
`/v1/models/web` client). Use **`/v1/chat/search`** when you want a combo chain over search
providers, or when you want the OpenAI-shaped envelope so the response drops straight into a
chat client.

## 2 · Register a search provider

Search credentials live in the **media-provider** table, not the normal provider table.
Managed (admin) API — note the camelCase `mediaType` field:

```bash
curl -sS -X POST http://127.0.0.1:4623/api/media-providers \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"provider":"tavily","mediaType":"search","name":"tavily-main",
       "apiKey":"tvly-…","baseUrl":"https://api.tavily.com"}'
# → {"success":true,"message":"Media provider added successfully","id":"mp-…"}
```

`baseUrl` is optional — each adapter ships its own upstream default. It is SSRF-guarded:
private/loopback/link-local targets are rejected on both the literal and the DNS-resolved
check.

Once a provider is registered, `/v1/models/web` starts advertising two model ids for it —
`<provider>/search` (`kind: "webSearch"`) and `<provider>/fetch` (`kind: "webFetch"`). The
first is the id you pass to `/v1/search`.

List and delete:

```bash
curl -sS "http://127.0.0.1:4623/api/media-providers?kind=search" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY"

# NOTE: the id goes in the single path segment (the route param is named `kind`,
# but the handler reads it as the connection id).
curl -sS -X DELETE "http://127.0.0.1:4623/api/media-providers/mp-<uuid>" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY"
```

CLI equivalents (they call the same routes):

```bash
# The API key has no dedicated flag, so it comes from the JSON body on stdin.
echo '{"apiKey":"tvly-…"}' | \
  openproxy media providers add --provider tavily --kind search --name tavily-main --from-file -

openproxy media providers list --kind web     # `web` is the friendly alias for search+fetch
openproxy media providers edit <id> --from-file -
openproxy media providers delete <id> --kind search
```

## 3 · Request parameters

Snake_case on both routes.

| Field | Type | Default | Notes |
|---|---|---|---|
| `query` | string | — | Required by `/v1/search` (`400 Missing required field: query`). NFKC-normalized and whitespace-collapsed; control characters and an empty result are rejected. |
| `max_results` | int | `5` | Hard-clamped to 100, then to the provider's cap (section 4). |
| `search_type` | string | `web` | One of `web`, `news`, `x`. Anything else falls back to `web`. |
| `country` | string | — | |
| `language` | string | — | |
| `time_range` | string | — | e.g. `day`, `week`, `month` |
| `offset` | int | — | Pagination start. |
| `domain_filter` | string[] | — | Restrict to these domains. |
| `content_options` | object | — | Passed to the adapter. |
| `provider_options` | object | — | `{"model": "…"}` overrides the chat-search model; `{"baseUrl": "…"}` overrides the upstream (SSRF-checked, public hosts only). |

## 4 · Dedicated search adapters

These 14 providers have a real search API. Reachable on **both** routes (the ids in the
second column are the `/v1/chat/search` aliases; the first column works everywhere).

| Provider | `/v1/chat/search` aliases | `max_results` cap |
|---|---|---|
| `serper` | `serper` | 100 |
| `serpingapi` | `serpingapi`, `sping` | 100 |
| `brave-search` | `brave-search`, `brave`, `bs` | 20 |
| `perplexity` | `perplexity` | 100 |
| `exa` | `exa` | 100 |
| `tavily` | `tavily`, `tv` | 20 |
| `google-pse` | `google-pse`, `gps` | 10 |
| `linkup` | `linkup`, `lu` | 50 |
| `searchapi` | `searchapi`, `sa` | 100 |
| `youcom` | `youcom`, `you` | 100 |
| `searxng` | `searxng`, `searx` | 50 |
| `xquik` | `xquik` | 100 |
| `ollama-search` | `ollama-search`, `ollama_search` | 10 |
| `glm` | `glm` | 50 |

Note `ollama` on its own does **not** resolve as a search alias. `ollama-search` reuses the
`ollama` chat credential when it has none of its own, so configure `ollama` and search via
`ollama-search`.

`/v1/search` response (`SearchResultSet`):

```json
{
  "results": [
    { "title": "…", "url": "https://…", "snippet": "…", "position": 1,
      "display_url": "…", "score": 0.87, "published_at": "…",
      "favicon_url": "…", "metadata": {}, "citation": {} }
  ],
  "total_results": 5
}
```

Only `title`, `url`, `snippet`, `position`, `metadata` and `citation` are always present;
the rest are omitted when empty.

## 5 · Search-via-chat providers

These have no search API. The router sends a chat completion with the provider's grounding
tool attached and normalizes the citations back into `results` plus an `answer` block. The
authoritative list is `has_chat_search` in `src/core/media/search/chat_search.rs:47`:

| Provider | Default chat-search model |
|---|---|
| `gemini` | `gemini-2.5-flash` |
| `antigravity` | `gemini-2.5-flash` |
| `openai` | `openai/gpt-4o-mini` (the `openai/` prefix is stripped upstream) |
| `xai` | `grok-3` |
| `kimi`, `kimi-coding` | `kimi-k2.7` |
| `minimax`, `minimax-cn` | `abab6.5s-chat` |
| `perplexity`, `perplexity-agent` | `sonar` |

Override per request with `provider_options: {"model": "grok-4.20-reasoning"}`.

**Address them through `/v1/search`, not `/v1/chat/search`.** `gemini`, `openai`, `xai`,
`kimi`, `kimi-coding`, `minimax`, `minimax-cn` and `perplexity-agent` are absent from the
`/v1/chat/search` alias table, so sending them there silently reroutes the request to
`serper` and its failover chain — the search you asked for never runs.

```bash
curl -sS -X POST http://127.0.0.1:4623/v1/search \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"xai/grok-3","query":"latest rust release","max_results":5}'
```

`perplexity` is the one overlap: it has a dedicated adapter, so `/v1/search` uses the
Perplexity search API rather than the `sonar` chat model. `antigravity` is the reverse —
chat-search only, and it *is* in the `/v1/chat/search` alias table.

The `searchViaChat` block in `web/src/shared/constants/providers.ts` also lists
`vercel-ai-gateway`, but there is no Rust handler for it. Do not route search through it.

The chat-search path adds `provider`, `query`, `answer` (`{source, text, model}`), `usage`
(`{queries_used, search_cost_usd, llm_tokens}`), `metrics` and `errors` alongside `results`.
The dedicated adapters declare no chat fallback, so a 5xx from a search API surfaces as an
error rather than silently degrading to a chat model.

## 6 · Raw search — `POST /v1/search`

```bash
curl -sS -X POST http://127.0.0.1:4623/v1/search \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"tavily/search","query":"rust 1.90 release notes",
       "max_results":5,"search_type":"web","language":"en"}'
```

The part after the slash in `model` only has to complete the `provider/model` parse; `search`
is the suffix `/v1/models/web` advertises and the one the CLI sends, so use it.

This route rotates only between **accounts of the same provider**, and only on `401`, `403`
or `429` (`src/server/api/media.rs:960`). A `400`, `404` or `5xx` returns immediately — it
will not be retried on another account.

## 7 · Chat search — `POST /v1/chat/search`

```bash
curl -sS -X POST http://127.0.0.1:4623/v1/chat/search \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"tavily","messages":[{"role":"user","content":"rust 1.90 release notes"}],
       "max_results":3}'
```

Response (`chat_search.rs:582`):

```json
{
  "id": "searchcmpl-<uuid>",
  "object": "chat.completion",
  "created": 1759000000,
  "model": "tavily",
  "choices": [{ "index": 0,
                "message": { "role": "assistant", "content": "1. Title\n   URL: …\n   snippet\n\n" },
                "finish_reason": "stop" }],
  "usage": { "prompt_tokens": 4, "completion_tokens": 5, "total_tokens": 9 },
  "search_results": [ { "title": "…", "url": "…", "snippet": "…", "position": 1 } ],
  "search_metadata": { "provider": "tavily", "query": "…",
                       "total_results": 3, "search_type": "web" }
}
```

`search_metadata.provider` reports the provider that actually answered, which is how you
confirm a failover happened.

**Failover.** If the primary provider has no usable credential or fails, the request walks
this canonical order, skipping any provider without an active connection, and returns the
first success:

```
serper → serpingapi → brave-search → perplexity → exa → tavily → google-pse
      → linkup → searchapi → youcom → searxng → xquik → ollama-search → glm
      → antigravity
```

So an under-configured router does not 400 — it quietly answers from a different vendor.
`search_metadata.provider` is how you detect that.

**Combos.** Unlike `/v1/search`, this route expands a combo. Create one over search
providers and pass the bare combo name:

```bash
openproxy combo create --name my-search --models "tavily,brave-search,exa"
curl -sS -X POST http://127.0.0.1:4623/v1/chat/search \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"my-search","query":"rust 1.90 release notes"}'
```

Each member goes through the same `resolve_search_provider` gate, so a combo member that is
not a search provider id is treated as a failed member and the chain rotates past it. Check
a combo's reachability before relying on it:

```bash
openproxy combo test my-search
```

## 8 · CLI

```bash
# Search. Sends {"provider":P,"model":"P/search","query":Q} to /v1/search.
openproxy media search --provider tavily --query "rust 1.90 release notes"
echo "rust release notes" | openproxy media search --provider exa        # - is the default for --query

# Agent-friendly NDJSON envelope (schema openproxy.v1.media.search).
openproxy --robot media search --provider tavily --query "rust"

# Fetch a page (section 9).
openproxy media web fetch --provider jina https://example.com --format markdown --max-chars 20000
```

The CLI talks to the local DB or to a remote router with `--url http://host:port` paired
with `--api-key` / `$OPENPROXY_API_KEY`. Run `openproxy media --help` for the full matrix.

## 9 · Web fetch — `POST /v1/web/fetch`

Search returns links; fetch turns a link into text. Same router, same auth, one path — there
is no `/v1/v1/web/fetch` alias.

```bash
curl -sS -X POST http://127.0.0.1:4623/v1/web/fetch \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"provider":"jina-reader","url":"https://example.com",
       "format":"markdown","maxCharacters":20000}'
```

| Field | Notes |
|---|---|
| `provider` (or `model`) | `firecrawl` (`fc`), `jina-reader` (`jina`), `tavily` (`tv`), `exa`, `ollama` (`ol`) |
| `url` | Required. Private/loopback/link-local targets are blocked. |
| `format` | `markdown` (default), `html`, `text` |
| `maxCharacters` | Truncate the output. |

Response:

```json
{ "provider": "jina-reader", "url": "https://example.com", "title": "Example",
  "content": { "format": "markdown", "text": "…", "length": 1234 },
  "metadata": { "author": null, "published_at": null, "language": null },
  "usage": { "fetch_cost_usd": null },
  "metrics": { "response_time_ms": 150, "upstream_latency_ms": 50 } }
```

The provider may be sent as `model` — the dashboard does that because for these endpoints
the provider *is* the model.

## 10 · Verifications

```bash
export OPENPROXY_API_KEY=…

# The router advertises the search + fetch endpoints here.
curl -sS http://127.0.0.1:4623/v1

# Which search providers are configured (empty `search: []` means none are).
curl -sS "http://127.0.0.1:4623/api/media-providers?kind=search" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY"

# Search models visible through the OpenAI-compatible models API.
# NOTE: the slug is `web`, not `webSearch` — `/v1/models/webSearch` 404s.
curl -sS http://127.0.0.1:4623/v1/models/web -H "Authorization: Bearer $OPENPROXY_API_KEY"
# {"object":"list","data":[
#   {"id":"tavily/search","object":"model","created":…,"owned_by":"tavily",
#    "permission":[],"root":"search","parent":null,"kind":"webSearch"},
#   {"id":"tavily/fetch", …,"kind":"webFetch"}]}

# End-to-end search. The `model` value is the id advertised above, verbatim.
curl -sS -X POST http://127.0.0.1:4623/v1/search \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -d '{"model":"tavily/search","query":"rust","max_results":3}'

# End-to-end fetch. Takes a BARE provider id, not the advertised "<provider>/fetch" id.
curl -sS -X POST http://127.0.0.1:4623/v1/web/fetch \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -d '{"provider":"jina-reader","url":"https://example.com"}'
```

The ids from `/v1/models/web` are exactly what `/v1/search` expects as `model`, so discovery
and invocation line up. They are **not** interchangeable with `/v1/web/fetch`, which takes a
bare provider id — sending `tavily/fetch` there fails with
`No credentials for provider: tavily/fetch`, because the whole string is treated as the
provider name.

Cards carry a `kind` (`webSearch` / `webFetch`) but no `endpoint` field. An `endpoint` is
emitted only by `GET /v1/models/info?id=<model>`, and only when the id resolves to a catalog
model. That field comes from the static `kind_endpoint` map
(`src/server/api/v1_models.rs:28`), whose `webFetch` entry is `"/v1/fetch"` — a path that
does not exist. The real fetch route is `/v1/web/fetch`. Trust the route.

## Common failure modes & fixes

| Symptom | Cause / fix |
|---|---|
| `400 Missing model` on `/v1/search` | `model` is mandatory and parsed as `provider/model`. Send `{"model":"tavily/search"}`. |
| `400 Combos not supported for search` | `/v1/search` does not expand combos. Use `/v1/chat/search` with the bare combo name, or drop the `combo:` prefix and call a provider directly. |
| `400 No credentials for provider: X` | No active media-provider row of kind `search` for `X`. Add one (section 2). |
| `400 No active credentials found for search provider: X` | `/v1/chat/search` found no usable connection for `X` **and** the rest of the failover chain. |
| Search returns results from an unexpected vendor | `/v1/chat/search` failed over silently. Check `search_metadata.provider`. |
| Search returns nothing resembling your provider's | On `/v1/chat/search`, an unrecognized `model` silently becomes `serper`. `gemini`, `openai`, `xai`, `kimi` and `minimax` are **not** valid there — use `/v1/search`. |
| `404 model_not_found` on `/v1/models/webSearch` | The kind slug is `web`. |
| `No credentials for provider: tavily/fetch` | `/v1/web/fetch` wants a bare provider id. The `<provider>/fetch` id from `/v1/models/web` is for the models API, not the fetch call. |
| `405` / unexpected shape from an upstream | The provider's `baseUrl` points at the wrong host. Unset it to use the adapter default, or set the full API root. |
| `400 Query contains invalid control characters` or `Query is empty after normalization` | The query was control-character-bearing or collapsed to nothing. |
| Fewer results than `max_results` | The provider cap in section 4 is applied after the global clamp of 100. |
| Fetch returns 400 for an internal URL | SSRF guard — loopback, private, and link-local destinations are refused by design. |

## When _not_ to use this skill

- The user wants search through a CLI coding tool (Claude Code, Codex, …) rather than the
  router API — those tools have their own web-search tools; see
  [`.agents/skills/openproxy/SKILL.md`](../openproxy/SKILL.md) for wiring them to the proxy.
- The user is debugging combo dispatch for LLM chat, not search — see
  [`.agents/skills/openproxy-chat/SKILL.md`](../openproxy-chat/SKILL.md).
- Turning a URL into text rather than finding URLs — `.agents/skills/openproxy-web-fetch/`
  covers `/v1/web/fetch` in depth; section 9 here is the summary.
- Image/audio/video generation — different `/v1/*` routes and a different CLI subtree
  (`openproxy media image`, `openproxy media tts`).

## See also

- Endpoint and auth overview: [`.agents/skills/openproxy/SKILL.md`](../openproxy/SKILL.md)
- URL-to-markdown: [`.agents/skills/openproxy-web-fetch/SKILL.md`](../openproxy-web-fetch/SKILL.md)
- Intentional divergences: `docs/parity-9router.md`
- Source of truth for these routes: `src/server/api/chat_search.rs`,
  `src/server/api/media.rs` (route kind `search`), `src/core/media/search/`
- Dashboard catalog for search providers: `web/src/shared/constants/providers.ts`
