---
name: openproxy-image
description: Generate images through the OpenProxy router on 127.0.0.1:4623 — POST /v1/images/generations (and /v1/images/edits), which providers serve them, how the model string is addressed, and how to add a new image endpoint with a custom openai-compatible provider node. Use whenever the user wants to generate, edit, or text-to-image an image through openproxy, wants to know which image models are available, or wants to wire a new image vendor into the router.
---

# openproxy — image generation

OpenProxy exposes an OpenAI-compatible image API on `127.0.0.1:4623`. One endpoint —
`POST /v1/images/generations` — fans out to every provider that has a native image
adapter, and falls through to a generic upstream forwarder for anything else. This
skill covers:

1. Calling the endpoint with a real request
2. Addressing a model (`<provider>/<model-id>`) and finding the valid ids
3. Which providers actually serve images
4. Account rotation, OAuth refresh, and combos on this route
5. Adding a brand-new image endpoint with a custom provider node
6. Verifying the whole chain

Every command is non-interactive and copy-pasteable.

## 0 · Preflight

```bash
command -v openproxy && openproxy --version || echo "not installed"
curl -sS http://127.0.0.1:4623/health          # liveness, no auth required

# Management + /v1 auth: the admin key from `openproxy server init`, or any
# key minted with `openproxy key add`.
export OPENPROXY_API_KEY="$(cat "$HOME/.openproxy/admin.key" 2>/dev/null || true)"

curl -sS http://127.0.0.1:4623/v1/models/image \
  -H "Authorization: Bearer $OPENPROXY_API_KEY"
```

If the server is not running: `openproxy server start --detach --no-open`.
If `/v1/*` answers 401, the router's `require_api_key` setting is on (there is no
`REQUIRE_API_KEY` env var — no code reads it) and the bearer is
wrong — see [`.agents/skills/openproxy/SKILL.md`](../openproxy/SKILL.md) §2.

`/v1/*` is gated on the `require_api_key` setting (default follows `require_login`;
`require_api_key` setting). The management API under `/api/*` takes the same bearer via
`require_dashboard_or_management_api_key`. One key works for both.

## 1 · The endpoint

Registered in `src/server/api/mod.rs`:

| Route | Handler | Body |
|---|---|---|
| `POST /v1/images/generations` | `media::images_generations` | JSON |
| `POST /v1/images/edits` | `media::images_edits` | JSON |
| `OPTIONS` on both | `media::cors_options` | — |

Both routes are also registered under a double prefix — `/v1/v1/images/generations`
and `/v1/v1/images/edits` — so a client that already appends `/v1` to a base URL
still lands. Both handlers return `Access-Control-Allow-Origin: *`.

> `/v1/images/edits` takes a **JSON** body here, not `multipart/form-data` as
> upstream OpenAI does. `images_edits` uses the same `Result<Json<Value>, _>`
> extractor as `images_generations` (`src/server/api/media.rs:740`), so there is
> no multipart path on this route. Send the same JSON shape.

### Request fields

| Field | Required | Default | Notes |
|---|---|---|---|
| `model` | **yes** | — | Empty/absent → `400 Missing model`. See §2. |
| `prompt` | **yes** | — | Empty/absent → `400 Missing required field: prompt`. |
| `n` | no | `1` | Number of images. |
| `size` | no | `1024x1024` | Provider-dependent; also mapped to an aspect ratio by the adapters that need one. |
| `quality` | no | — | Forwarded by the OpenAI-compatible adapters. |
| `style` | no | — | Forwarded by the OpenAI-compatible adapters. |
| `response_format` | no | — | `b64_json` returns `data[].b64_json`; the default returns `data[].url`. |

**xAI is the exception.** Its adapter whitelists only `model`, `prompt`, `n` and
`response_format` — `size`, `quality` and `style` are dropped
(`src/core/media/image/openai_compat.rs:50`). Sending them to `xai/…` is not an
error; they simply never reach the upstream.

### Response

The OpenAI shape comes back verbatim for the OpenAI-compatible adapters:

```json
{ "created": 1759000000, "data": [ { "url": "https://…/img.png" } ] }
```

Adapters that return something else normalize into it — `sdwebui` maps its
`images[]` array to `data[].b64_json` entries, for example. A request that
produces a non-JSON image is only available through the binary path, which the
public route does not expose.

## 2 · Addressing a model

The `model` string is read by `get_model_info` (`src/core/model/mod.rs:303`) and
resolved in this order:

1. **`combo:<name>`** — forces the combo path. A bare combo name works too, but
   the explicit prefix removes the ambiguity.
2. **`<prefix>/<model-id>`** — everything before the first `/` is the provider
   (or its alias). This is the form to use.
3. **A bare model name** — resolved as a model alias, a combo name, or — failing
   both — by family-prefix inference.

Family-prefix inference is the fallback and it is coarse:

| Prefix | Provider |
|---|---|
| `claude-` | `anthropic` |
| `gemini-` | `gemini` |
| `gpt-`, `o1`, `o3`, `o4` | `openai` |
| `deepseek-` | `openrouter` |
| anything else | `openai` |

So a bare `gpt-image-1` lands on `openai` and a bare `dall-e-3` also lands on
`openai` — correct by luck. A bare `grok-2-image-1212` or `flux-schnell` lands on
`openai` and fails. **Always pass the provider prefix.**

### Which prefix to use

The prefix is resolved through the alias table, so both the short alias and the
full provider id work: `cx/gpt-5.5-image` and `codex/gpt-5.5-image` both reach
`codex`; `ag/…` and `antigravity/…` both reach `antigravity`. Providers with no
alias (`fal-ai`, `black-forest-labs`, `stability-ai`, `runwayml`, `recraft`,
`sdwebui`, `comfyui`, `huggingface`, `cloudflare-ai`, `nanobanana`) take their
own id.

`GET /v1/models/image` returns the live list, and every id there is already in
`<alias>/<model-id>` form — copy it verbatim:

```bash
curl -sS http://127.0.0.1:4623/v1/models/image \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" | jq -r '.data[].id'
```

Note the ids come back with the **catalog alias** prefix, not always the provider
id (`cx/…` for codex, `ag/…` for antigravity). Both forms route identically.

Note also that a model disabled on the provider page is filtered out of this list
— the endpoint reads the same disabled map the dashboard and
`ModelSelectModal.tsx` use, so the two never disagree.

## 3 · Which providers serve images

A provider serves images if `get_image_adapter` returns an adapter for its id
(`src/core/media/image/mod.rs`). There are exactly 18:

| Provider | Adapter | Upstream |
|---|---|---|
| `openai` | openai-compat | `https://api.openai.com/v1/images/generations` |
| `minimax` | openai-compat | `https://api.minimaxi.com/v1/images/generations` |
| `openrouter` | openai-compat | `https://openrouter.ai/api/v1/images/generations` |
| `recraft` | openai-compat | `https://external.api.recraft.ai/v1/images/generations` |
| `xai` | openai-compat | `https://api.x.ai/v1/images/generations` |
| `vercel-ai-gateway` | openai-compat | `https://ai-gateway.vercel.sh/v1/images/generations` |
| `gemini` | native | `generativelanguage.googleapis.com/v1beta/models/…` |
| `codex` | native | `chatgpt.com/backend-api/codex/responses` (OAuth) |
| `antigravity` | native | `…/v1internal:generateContent` (OAuth) |
| `fal-ai` | native | `https://queue.fal.run` (submit + poll) |
| `stability-ai` | native | `https://api.stability.ai/v2beta/stable-image/generate/…` |
| `black-forest-labs` | native | `https://api.bfl.ai/v1` (submit + poll) |
| `runwayml` | native | `https://api.dev.runwayml.com/v1` (submit + poll) |
| `cloudflare-ai` | native | `https://api.cloudflare.com/client/v4/accounts/{accountId}/…` |
| `nanobanana` | native | `https://api.nanobananaapi.ai/api/v1/…` (submit + poll) |
| `huggingface` | native | `https://api-inference.huggingface.co/models/{model}` |
| `sdwebui` | native | `http://localhost:7860/sdapi/v1/txt2img` — **no auth** |
| `comfyui` | native | `http://localhost:8188` — **no auth** |

`openrouter` additionally sends `HTTP-Referer: https://endpoint-proxy.local` and
`X-Title: Endpoint Proxy` on the image path, matching its chat path. The other
OpenAI-compatible variants send a plain `Authorization: Bearer <key>`.

`cloudflare-ai` needs an `accountId` in `provider_specific_data`; without it
`build_url` fails and the request is rejected before it leaves the machine.

**A catalog image model is not the same as an adapter.** `venice` and
`tokenrouter` have image models in the catalog but no adapter, so they fall
through to the generic forwarder in §5 — which works only if the connection
carries its own `baseUrl`.

## 4 · Making a request

```bash
curl -sS -X POST http://127.0.0.1:4623/v1/images/generations \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{
        "model": "openai/gpt-image-1",
        "prompt": "a red panda astronaut, studio lighting",
        "size": "1024x1024",
        "n": 1
      }'
```

Return base64 instead of a URL, and write it out:

```bash
curl -sS -X POST http://127.0.0.1:4623/v1/images/generations \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"openai/gpt-image-1","prompt":"a lighthouse at dusk","response_format":"b64_json"}' \
  | jq -r '.data[0].b64_json' | base64 --decode > /tmp/lighthouse.png
```

Through a combo (see §6):

```bash
curl -sS -X POST http://127.0.0.1:4623/v1/images/generations \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"combo:my-image-stack","prompt":"a lighthouse at dusk"}'
```

### From the CLI

```bash
openproxy media image generate \
  --provider openai \
  --model openai/gpt-image-1 \
  --prompt "a red panda astronaut, studio lighting" \
  --size 1024x1024
```

`--prompt -` reads from stdin. `--size` defaults to `1024x1024`. Omitting
`--model` substitutes `gpt-image-1` (`src/cli/media.rs:686`). Add `--robot` for a
stable JSON envelope keyed `openproxy.v1.media.image.generate`.

> **The `--provider` flag does not choose the provider.** `run_image_generate`
> puts it in the request body, and the media handler reads **only** `model` off
> that body — `provider` is discarded. Routing is decided entirely by the model
> string. So `--provider xai --model grok-2-image-1212` silently goes to
> `openai`; write `--model xai/grok-2-image-1212` instead. The flag is harmless
> but inert.

## 5 · Adding a new image endpoint

This is what a custom **provider node** of type `openai-compatible` is for. The
media handler has no adapter for an arbitrary vendor, so a node-backed provider
falls through to the generic forwarder, which composes the URL as
`{node.baseUrl}/images/generations` and sends `Authorization: Bearer <apiKey>`.
That is the supported path for any OpenAI-shaped image API.

Three steps.

**1 — Create the node.** `prefix` is the addressable name; `baseUrl` stops at the
version segment, because OpenProxy appends `/images/generations` itself.

```bash
curl -sS -X POST http://127.0.0.1:4623/api/provider-nodes \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{
        "name": "My Vendor",
        "type": "openai-compatible",
        "prefix": "myimg",
        "apiType": "chat",
        "baseUrl": "https://api.myvendor.com/v1"
      }'
```

```json
{ "success": true, "node": { "id": "openai-compatible-chat-1f0c…", "prefix": "myimg", … } }
```

The **`id` is the provider key** — it is derived from the type, not a bare uuid
(`compatible_node_id`, `src/server/api/provider_nodes.rs:107`). Capture it:

```bash
NODE_ID=$(curl -sS http://127.0.0.1:4623/api/provider-nodes \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  | jq -r '.nodes[] | select(.prefix=="myimg") | .id')
```

**2 — Create a credential for it.** The connection's `provider` must be the node
id — that is the only form `is_valid_provider` accepts for a `*-compatible-` id.

```bash
curl -sS -X POST http://127.0.0.1:4623/api/providers \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d "{
        \"provider\": \"$NODE_ID\",
        \"name\": \"myimg\",
        \"apiKey\": \"sk-myvendor-…\"
      }"
```

The node's `baseUrl` is copied into the connection's `provider_specific_data` on
create and wins over anything in the request body — the node, not the payload,
decides which host is dialled.

**3 — Call it with the prefix.**

```bash
curl -sS -X POST http://127.0.0.1:4623/v1/images/generations \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"myimg/flux-schnell","prompt":"a lighthouse at dusk","size":"1024x1024"}'
```

Why this resolves: `get_model_info` sees that the prefix `myimg` survives alias
resolution unchanged (`provider == provider_alias`), finds the `openai-compatible`
node whose `prefix` matches, and returns the **node id** as the provider. From
there `get_image_adapter(node_id)` is `None`, so the request takes the generic
forwarder and lands on `https://api.myvendor.com/v1/images/generations`.

Two traps:

- **Do not put `/images/generations` in `baseUrl`.** It is stripped on create only
  for `anthropic-compatible` (`/messages`) and `custom-embedding`
  (`/embeddings`); an `openai-compatible` URL is stored as given, so the path gets
  doubled and the upstream 404s.
- **`apiType` is chat-only.** It selects `/chat/completions` vs `/responses` in the
  chat executor and is ignored entirely on the media path. Keep it for the
  dashboard's benefit; it changes nothing here.

Adding a provider through the media API instead (no node, no prefix) is a
different shape and does **not** give you a custom endpoint:

```bash
curl -sS -X POST http://127.0.0.1:4623/api/media-providers \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"name":"my-vendor","provider":"myvendor","api_key":"sk-…","base_url":"https://api.myvendor.com/v1","media_type":"image"}'
```

`media_type` must be one of `tts`, `stt`, `embedding`, `image`, `search`. This
is for dashboard bookkeeping of a *known* provider id; reach for the node recipe
above when the endpoint is new.

## 6 · Combos, rotation, and refresh

**Combos expand on this route.** `combo_expands("images/generations")` is true —
alongside `audio/speech` and nothing else. Combs are rejected on stt, video, and
embeddings; images are not one of them. Members are tried in configured order and
the first 2xx wins; if all fail, the last member's own error is returned rather
than a synthesized combo error.

```bash
openproxy combo create --name my-image-stack \
  --models "openai/gpt-image-1,openrouter/openai/dall-e-3,fal-ai/fal-ai/flux/schnell"
```

Members use the same `<provider>/<model>` addressing as the direct path.

**Account rotation.** If several active credentials exist for the resolved
provider, they are tried in `priority` order and the request moves to the next one
on `401`, `403` or `429`. Any other status (a `400` for a bad prompt, a `5xx` from
the vendor) is returned as-is — retrying a request's own problem on every account
just multiplies latency for the same answer.

**OAuth refresh.** On a `401` or `403`, the image handler refreshes the OAuth
token once and re-fires the request exactly once, if the connection carries a
refresh token. It never retries a second time, and it never retries for the
no-auth local adapters (`sdwebui`, `comfyui`).

**Upstream errors keep their status.** A provider's `429` comes back as `429`, not
re-derived from the message text.

## 7 · Verifications

```bash
# 1. Server up
curl -sS http://127.0.0.1:4623/health

# 2. The route is registered (advertised by the root index — no trailing slash)
curl -sS http://127.0.0.1:4623/v1 | jq -r '.endpoints[]' | grep images

# 3. Image models exist for your provider
curl -sS http://127.0.0.1:4623/v1/models/image \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" | jq -r '.data[].id' | grep '^openai/'

# 3b. Which image providers are configured, from the CLI
openproxy --robot media providers list --kind image

# 4. A credential is registered for that provider
curl -sS http://127.0.0.1:4623/api/providers \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  | jq -r '.connections[] | select(.provider=="openai") | "\(.name)  \(.id)"'

# 5. End to end
curl -sS -X POST http://127.0.0.1:4623/v1/images/generations \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"openai/gpt-image-1","prompt":"a smoke test"}' | jq '{created, n: (.data|length)}'

# 6. Same thing through the CLI
openproxy --robot media image generate --provider openai \
  --model openai/gpt-image-1 --prompt "a smoke test"
```

Stop the server when done: `openproxy server stop`.

## Common failure modes

| Symptom | Cause / fix |
|---|---|
| `400 Missing model` | `model` absent or empty. Required on every image call. |
| `400 Missing required field: prompt` | `prompt` absent or empty — validated before any network call. |
| `400 No credentials for provider: <p>` | The resolved provider has no active connection with a key. Check the connection's `provider` matches the prefix you addressed. |
| `400 Invalid provider` (on `POST /api/providers`) | The id is not in the known-provider set and does not start with `openai-compatible-` / `anthropic-compatible-` / `custom-embedding-`. Use the node id from §5. |
| `404` from the vendor | `baseUrl` on an `openai-compatible` node already includes `/images/generations`; OpenProxy appended it again. |
| Image arrives from the wrong vendor | The model string had no provider prefix and fell through to the `openai` fallback. Use `<provider>/<model>`. |
| `xai` ignores `size` / `quality` | By design — its adapter whitelists `model`, `prompt`, `n`, `response_format`. |
| `cloudflare-ai` errors before sending | `accountId` missing from `provider_specific_data`. |
| 401 on every attempt | The bearer is wrong, or the vendor key is wrong. These are separate: `/v1/*` auth is the router's; the upstream key rides on the connection. |
| 429 on every account | All credentials for that provider are rate-limited. Rotation already tried them all. |
| Catalog lists an image model for a provider with no adapter (`venice`, `tokenrouter`) | The generic forwarder needs a `baseUrl` on the connection; without one it synthesizes `https://api.<provider>.com/v1`. |

## See also

- Install, init, and CLI wiring: [`.agents/skills/openproxy/SKILL.md`](../openproxy/SKILL.md)
- Combo dispatch and fallback: `.agents/skills/openproxy-combos/SKILL.md`
- Schema introspection: `openproxy schema list`, `openproxy schema show provider-node`
- Intended divergences from the 9router reference: `docs/parity-9router.md`
