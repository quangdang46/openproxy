---
name: openproxy-embeddings
description: Generate embedding vectors through OpenProxy's `POST /v1/embeddings` — the `input` / `dimensions` / `encoding_format` contract, the built-in embedding-provider adapters, the `custom-embedding` providerNode type for self-hosted and OpenAI-compatible embedding servers, and the checks that prove an embedding provider is actually wired. Use whenever the user asks to embed text, set up RAG / semantic search / vector search, add an embedding model, or debug an embeddings call that 400s, 401s, or returns `Provider '…' does not support embeddings.`
---

# openproxy-embeddings — vectors through `/v1/embeddings`

[OpenProxy](https://github.com/quangdang46/openproxy) proxies embedding calls through one
OpenAI-shaped route, `POST http://127.0.0.1:4623/v1/embeddings`. The request is resolved to a
**provider** from the `model` field, checked against the embedding-adapter registry, and fanned
out with credential rotation on 401/403/429.

This skill covers:

1. The request contract (`model`, `input`, `dimensions`, `encoding_format`)
2. Which providers can serve embeddings, and their upstream URLs
3. Registering a `custom-embedding` provider node for a self-hosted / OpenAI-compatible server
4. Verifying an embedding provider is wired, end to end

Everything here is non-interactive and safe to run unattended.

## 0 · Preflight

Confirm the server is up and a key is available before touching anything:

```bash
curl -sS http://127.0.0.1:4623/health | jq '.status'
# → "ok"

export OPENPROXY_API_KEY="$(cat "$HOME/.openproxy/admin.key")"
# or mint a scoped one:
openproxy key add rag-embedder --auto
```

If the server is not running: `openproxy server start --detach --no-open`.

## 1 · The request contract

```bash
curl -sS http://127.0.0.1:4623/v1/embeddings \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{
    "model": "openai/text-embedding-3-small",
    "input": "the quick brown fox"
  }' | jq '{object, n: (.data|length), dims: (.data[0].embedding|length), usage}'
```

Response shape is always OpenAI's, regardless of upstream:

```json
{
  "object": "list",
  "data": [{ "object": "embedding", "index": 0, "embedding": [0.0023, -0.0119, …] }],
  "model": "text-embedding-3-small",
  "usage": { "prompt_tokens": 4, "total_tokens": 4 }
}
```

| Field | Required | Notes |
|---|---|---|
| `model` | yes | `<provider>/<model-id>`, or `<node-prefix>/<model-id>` for a custom node, or a registered alias. This field alone picks the provider — a `provider` field in the body is **ignored** by the handler. |
| `input` | yes | A string, an array of strings, an array of token-id arrays, or an array of token arrays. An **empty array is valid** and reaches the provider. |
| `dimensions` | no | Output vector width. See §2. |
| `encoding_format` | no | Defaults to `float`. An empty string is coerced to `float`. |
| `user` | no | Forwarded only to upstreams whose adapter keeps the caller body. |

Array input — batch in one call:

```bash
curl -sS http://127.0.0.1:4623/v1/embeddings \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"openai/text-embedding-3-small",
       "input":["first document","second document","third document"]}' \
  | jq '.data | map({index, dims: (.embedding|length)})'
```

### Validation order — cheap failures come first

`/v1/embeddings` rejects a bad request **before** any model resolution, credential lookup, or
network call. The exact error bodies:

```bash
# no input
curl -sS http://127.0.0.1:4623/v1/embeddings -H 'content-type: application/json' \
  -d '{"model":"openai/text-embedding-3-small"}'
# → 400 {"error":{"code":"bad_request","message":"Missing required field: input","type":"invalid_request_error"}}

# no model
curl -sS http://127.0.0.1:4623/v1/embeddings -H 'content-type: application/json' \
  -d '{"input":"hi"}'
# → 400 {"error":{"code":"bad_request","message":"Missing model","type":"invalid_request_error"}}

# provider has no embedding adapter
curl -sS http://127.0.0.1:4623/v1/embeddings -H 'content-type: application/json' \
  -d '{"model":"anthropic/claude-3-5-sonnet","input":"hi"}'
# → 400 {"error":{"code":"bad_request","message":"Provider 'anthropic' does not support embeddings.","type":"invalid_request_error"}}

# model resolved, but no credential is stored for that provider
# → 400 {"error":{"code":"bad_request","message":"No credentials for provider: openai", …}}
```

`input` is tested with JS truthiness, so `""`, `null`, `0`, and `false` all read as **missing**;
`[]` does not. `input` must otherwise be a string or an array.

### Combos are rejected here

9router expands a combo on TTS and image generation but not on embeddings, and OpenProxy matches
it. A combo name in `model` returns:

```
400  {"error":{…,"message":"Combos not supported for embeddings", …}}
```

Use a single provider, and put redundancy in **credentials** for that provider (the handler already
rotates across them on 401/403/429), not in a combo.

## 2 · The `dimensions` field

`dimensions` is coerced exactly like JavaScript's `Number()` before it is used, and a non-finite or
non-positive value is silently dropped:

| You send | Forwarded as |
|---|---|
| `256` | `256` |
| `"256"` | `256` (numeric strings are coerced) |
| `256.0` | `256` (an integral double serializes as an integer, never `256.0`) |
| `0`, `-1`, `""`, `null`, `"abc"` | *(omitted — the provider's default width wins)* |

```bash
curl -sS http://127.0.0.1:4623/v1/embeddings \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"openai/text-embedding-3-small","input":"hi","dimensions":256}' \
  | jq '.data[0].embedding | length'
# → 256
```

**Provider-specific naming.** For Gemini (`gemini` / `google_ai_studio`) the same number is sent as
`outputDimensionality`, and the URL switches on input shape: a string hits
`…/models/<model>:embedContent?key=…`, an array hits `…/models/<model>:batchEmbedContents?key=…`.
Gemini's response carries no token counts, so OpenProxy normalises it into the OpenAI shape with a
zero `usage` placeholder.

A `dimensions` value the upstream does not support surfaces as the upstream's own error. The HTTP
status is passed through verbatim and the upstream text is prefixed with its status:

```json
{ "error": { "code": "bad_request", "message": "[400]: <upstream message>", "type": "invalid_request_error" } }
```

## 3 · Which providers serve embeddings

A provider may serve `/v1/embeddings` if it is in the adapter registry **or** is a provider node
registered as an embedding endpoint (§4). Everything else is refused before any call leaves the
machine.

| Provider id | Upstream URL | Dialect |
|---|---|---|
| `openai` | `https://api.openai.com/v1/embeddings` | OpenAI |
| `openrouter` | `https://openrouter.ai/api/v1/embeddings` | OpenAI + `HTTP-Referer` / `X-Title` |
| `mistral` | `https://api.mistral.ai/v1/embeddings` | OpenAI |
| `voyage-ai` | `https://api.voyageai.com/v1/embeddings` | OpenAI |
| `fireworks` | `https://api.fireworks.ai/inference/v1/embeddings` | OpenAI |
| `together` | `https://api.together.xyz/v1/embeddings` | OpenAI |
| `nebius` | `https://api.tokenfactory.nebius.com/v1/embeddings` | OpenAI |
| `github` | `https://models.github.ai/inference/embeddings` | OpenAI |
| `nvidia` | `https://integrate.api.nvidia.com/v1/embeddings` | OpenAI |
| `jina-ai` | `https://api.jina.ai/v1/embeddings` | OpenAI |
| `vercel-ai-gateway` | `https://ai-gateway.vercel.sh/v1/embeddings` | OpenAI |
| `gemini`, `google_ai_studio` | `https://generativelanguage.googleapis.com/v1beta/…` | Gemini |
| `selfhosted-embedding` | per-connection `baseUrl` | OpenAI, **no cloud fallback** |
| `openai-compatible-*`, `custom-embedding-*` | per-connection `baseUrl` | OpenAI |

List what the model catalog actually offers:

```bash
curl -sS http://127.0.0.1:4623/v1/models/embedding \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" | jq '.data[].id'

openproxy models list --provider voyage-ai
openproxy provider models list voyage-ai
```

## 4 · Wire a built-in provider

Two steps: a **connection** (the credential) and, if the model is not in the catalog, a **custom
model** entry.

```bash
# 1. connection — the provider id is the key; the secret goes in apiKey
openproxy --robot provider apply --from-file - <<'JSON'
{
  "providers": [
    { "name": "voyage-main", "provider": "voyage-ai", "apiKey": "pa-...", "isActive": true }
  ]
}
JSON

# 2. optional — register a model the catalog does not carry
openproxy provider models custom add voyage-ai voyage-3-large --type embedding
```

Verify the credential resolves:

```bash
curl -sS http://127.0.0.1:4623/v1/embeddings \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"voyage-ai/voyage-3-large","input":"ping"}' | jq '.data[0].embedding | length'
```

`select_media_connections` returns the connections for the provider in priority order and the
handler walks them on **401 / 403 / 429** only. A 400 or a 5xx is the request's own problem and is
returned immediately rather than retried on every account.

## 5 · Register a `custom-embedding` provider node

A provider node says *"this server's embeddings endpoint is here"*. Use it for a self-hosted
server (Ollama, TEI, vLLM, LocalAI, an internal gateway) or any OpenAI-compatible `/embeddings`
endpoint that is not a built-in id.

### 5a. Create the node over the HTTP API

```bash
NODE=$(curl -sS -X POST http://127.0.0.1:4623/api/provider-nodes \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{
    "name": "local-tei",
    "type": "custom-embedding",
    "baseUrl": "http://127.0.0.1:8080",
    "prefix": "local"
  }')

echo "$NODE" | jq '.node'
```

The server **derives the node's id from its type** — the id *is* the provider key, and everything
downstream keys off that prefix. For `type: "custom-embedding"` you get `custom-embedding-<uuid>`:

| `type` | derived id |
|---|---|
| `custom-embedding` | `custom-embedding-<uuid>` |
| `anthropic-compatible` | `anthropic-compatible-<uuid>` |
| `openai-compatible` | `openai-compatible-<apiType>-<uuid>` (or `openai-compatible-<uuid>`) |

The stored `baseUrl` is normalised on write: one trailing `/` and a trailing `/embeddings` are both
stripped, because the executor appends `/embeddings` on the way out. Passing
`http://host:8080/embeddings` and `http://127.0.0.1:8080` produce the same stored value.

> **Caveat — use the HTTP API, not the CLI, for embedding nodes.** `openproxy provider node add`
> mints a bare `uuid` id, which does not carry the `custom-embedding-` prefix that
> `is_valid_provider` requires to create the node's credential. If you must use the CLI, run
> `openproxy provider node list` and confirm the id starts with `custom-embedding-` before
> continuing; if it does not, create the node with `POST /api/provider-nodes` instead.

### 5b. Attach a credential

The connection's `provider` is the node's **id**, exactly as returned in 5a:

```bash
NODE_ID=$(echo "$NODE" | jq -r '.node.id')

openproxy --robot provider apply --from-file - <<JSON
{
  "providers": [
    {
      "name": "local-tei-key",
      "provider": "$NODE_ID",
      "apiKey": "local-key-if-any",
      "isActive": true
    }
  ]
}
JSON
```

The node supplies `baseUrl`, and the connection may override it via `baseUrl` /
`providerSpecificData.baseUrl` for a self-hosted server with no auth.

### 5c. Address models through the node

Set `prefix` on the node (5a) and models are addressable as `<prefix>/<model>`:

```bash
curl -sS http://127.0.0.1:4623/v1/embeddings \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"local/bge-m3","input":"hello"}' | jq '.data[0].embedding | length'
```

Without a `prefix`, use the node id as the first segment: `<node-id>/<model>`.

Register the model so it appears in `/v1/models/embedding`:

```bash
openproxy provider models custom add "$NODE_ID" bge-m3 --type embedding
```

Editing a node propagates `prefix` / `baseUrl` / `apiType` into every connection that references
it, so an edit is visible without re-entering keys by hand:

```bash
curl -sS -X PUT "http://127.0.0.1:4623/api/provider-nodes/$NODE_ID" \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"baseUrl":"http://127.0.0.1:8081"}' | jq '.node.baseUrl'
```

Deleting a node cascades to its connections (and leaves custom models alone, so a node recreated
with the same id gets its model list back).

## 6 · Verify an embedding provider is wired

Run these four checks in order. Each one localises the failure to a different layer.

```bash
export KEY="$OPENPROXY_API_KEY"
export MODEL="openai/text-embedding-3-small"     # or "local/bge-m3", "voyage-ai/voyage-3-large"

# 1 · node registered (custom nodes only) — id must carry the type prefix
curl -sS http://127.0.0.1:4623/api/provider-nodes -H "Authorization: Bearer $KEY" \
  | jq '.nodes[] | select(.type=="custom-embedding") | {id, type, baseUrl, prefix}'

# 2 · the provider is accepted as an embedding endpoint
curl -sS -X POST http://127.0.0.1:4623/v1/embeddings -H "Authorization: Bearer $KEY" \
  -H 'content-type: application/json' -d "{\"model\":\"$MODEL\",\"input\":\"ping\"}" | jq -c .
#    "Provider '…' does not support embeddings." → the node type/apiType or provider id is wrong
#    "No credentials for provider: …"           → step 3

# 3 · the upstream answers, and the vector has the width you expect
curl -sS -X POST http://127.0.0.1:4623/v1/embeddings -H "Authorization: Bearer $KEY" \
  -H 'content-type: application/json' -d "{\"model\":\"$MODEL\",\"input\":\"ping\"}" \
  | jq '{dims: (.data[0].embedding|length), usage}'
```

4 · Probe the node **without** a DB write. This is the authoritative check for a custom embedding
node — it POSTs `{model, input:"ping"}` to `<baseUrl>/embeddings` and reports the dimension count
it read back:

```bash
curl -sS -X POST http://127.0.0.1:4623/api/provider-nodes/validate \
  -H "Authorization: Bearer $KEY" \
  -H 'content-type: application/json' \
  -d '{"baseUrl":"http://127.0.0.1:8080","apiKey":"local-key-if-any",
       "type":"custom-embedding","modelId":"bge-m3"}' | jq
# → {"valid":true,"error":null,"method":"embeddings","dimensions":1024}
```

`modelId` is required for `type: "custom-embedding"` — the probe is a real embeddings call, so
without a model the server answers `valid:false` with
`"Model ID required for embedding validation"`.

> `openproxy provider node validate <ID_OR_NAME>` is a weaker check: it issues a **GET** to
> `<baseUrl>/embeddings` (with `?model=` for the model), and embeddings endpoints generally do not
> answer GET. Use `POST /api/provider-nodes/validate` to confirm an embedding node.

### From the CLI

```bash
openproxy --robot media embed --provider openai --model text-embedding-3-small --text "ping"
# envelope kind: openproxy.v1.media.embed
echo "hello" | openproxy --robot media embed --provider openai --model text-embedding-3-small --text -

openproxy media providers list --kind embedding
openproxy media providers add --provider local-tei --kind embedding --name "Local TEI"
openproxy provider node list
openproxy provider node validate local-tei --model-id bge-m3
```

`media embed` defaults `--model` to `text-embedding-3-small` and posts to `/v1/embeddings`. It
sends a `provider` field too, but routing is decided by `model` alone.

## 7 · Self-hosted embedding servers

Use the `selfhosted-embedding` provider id when the server has no auth and you do not want to
register a node. It has **no cloud fallback**: if the connection's `baseUrl` is missing the call
fails with

> `Self-hosted Embedding needs an endpoint: set this connection's baseUrl to the OpenAI base URL of
> your server, e.g. http://host:8080/v1 (note the /v1 — "/embeddings" is appended to it). Refusing
> to fall back to api.openai.com, which would send your input and API key to OpenAI.`

Supply the base URL with the trailing `/v1` on the connection:

```bash
openproxy --robot provider apply --from-file - <<'JSON'
{
  "providers": [
    {
      "name": "local-ollama-embed",
      "provider": "selfhosted-embedding",
      "apiKey": "unused",
      "isActive": true,
      "baseUrl": "http://127.0.0.1:11434/v1"
    }
  ]
}
JSON
```

## Timeouts and refresh

| Var | Default | Effect |
|---|---|---|
| `FETCH_CONNECT_TIMEOUT_MS` | `60000` | Per-call upstream deadline on `/v1/embeddings`. Read per request, so it can be retuned without a restart. |

On a 401/403 the handler makes **one** refresh attempt (up to 3 tries, backing off 1 s then 2 s) and
re-fires the request **only if the refresh produced new credentials** — never a blind duplicate
call. The request body is deliberately the one built before the refresh, so `model` and
`dimensions` cannot shift on the retry. API-key providers carry no refresh token, so they see
exactly one upstream call.

## Common failure modes & fixes

| Symptom | Fix |
|---|---|
| `400 Provider 'x' does not support embeddings.` | `x` is not in the adapter registry and is not a node with `type: "custom-embedding"` (or `apiType: "embeddings"`). Register a node, or use a provider id from §3. |
| `400 No credentials for provider: x` | The provider resolved but no active connection holds a key. Create one (§4) — and check the node id matches exactly for custom nodes. |
| `400 Combos not supported for embeddings` | Combos are not expanded on this route by design. Point `model` at a single provider. |
| `400 Missing required field: input` | `input` was `""`, `null`, `0`, `false`, or absent. An empty **array** is fine. |
| `400 Missing model` | `model` absent or blank. It must be `<provider>/<model-id>`. |
| `401 / 403` on a custom node | The node id in the connection's `provider` does not match the id returned by node creation, or the upstream key is wrong. Re-check `GET /api/provider-nodes`. |
| Custom node created but unusable | The id is a bare uuid. Re-create it with `POST /api/provider-nodes`, which derives the `custom-embedding-<uuid>` id. |
| `dimensions` ignored | The value was non-numeric, `null`, or `<= 0` — it is dropped, and the provider's default width wins. |
| Gemini call 404s on `embedContent` | Gemini embeddings want `outputDimensionality`; that mapping is automatic. A 404 here usually means the model id is a chat model, not an embedding model. |
| Upstream timeout after 60 s | Raise `FETCH_CONNECT_TIMEOUT_MS` and restart, or pick a faster provider. |
| Vector width changed after adding `dimensions` | Expected — the stored vectors no longer match the query width. Re-embed the corpus. |

## When _not_ to use this skill

- The user is installing or first-running openproxy — use
  [`.agents/skills/openproxy/SKILL.md`](../openproxy/SKILL.md) first.
- The user is debugging chat routing or combo fallback — use
  `.agents/skills/openproxy-combos/SKILL.md`.
- The user wants a **model** to appear in a picker — that is the Available Models surface, not this
  route; see the `openproxy` skill's provider-page section.

## See also

- Entry skill: [`.agents/skills/openproxy/SKILL.md`](../openproxy/SKILL.md)
- Adapter registry: `src/core/media/embeddings/mod.rs` (`get_embedding_adapter`)
- Dialect adapters + `dimensions` coercion: `src/core/media/embeddings/base.rs`
- Route + validation order + credential rotation: `src/server/api/media.rs`
- Node CRUD: `src/server/api/provider_nodes.rs` (`compatible_node_id`, `sanitize_node_base_url`)
- Intentional divergences: `docs/parity-9router.md` (embeddings combo rejection; no refresh-once)
- Endpoint discovery: `GET /v1` lists every route the server answers
