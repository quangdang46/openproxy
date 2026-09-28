---
name: openproxy-providers
description: Manage OpenProxy providers from the CLI or the REST API — add/edit/delete provider connections, register custom provider nodes (vLLM, Ollama, any OpenAI/Anthropic-compatible endpoint), set priority, run connectivity probes, and import a provider's model catalog. Use whenever the user asks to add a provider key, connect an OAuth subscription (Claude Code, Codex, Copilot, Cursor), wire up a self-hosted or third-party endpoint, troubleshoot a failing provider connection, or reorder providers for fallback.
---

# openproxy-providers — connections, nodes, and verification

[OpenProxy](https://github.com/quangdang46/openproxy) stores two distinct objects, and almost every provider bug comes from confusing them:

| Object | What it is | Where it lives |
|---|---|---|
| **Provider connection** | One credential for one provider alias (`openai`, `anthropic`, a node id, …). Holds the API key / OAuth tokens, `priority`, `isActive`, `testStatus`. | `/api/providers`, `openproxy provider …` |
| **Provider node** | A registered *endpoint* for a self-hosted or third-party service (vLLM, LiteLLM, Ollama, an internal gateway). Holds `type`, `baseUrl`, optional `apiType` / `prefix`. **Holds no credential.** | `/api/provider-nodes`, `openproxy provider node …` |

A node is useless alone. You register it, then create one or more connections whose `provider` is the **node's generated id**.

Prerequisite: the binary is installed and the server is running — see [`.agents/skills/openproxy/SKILL.md`](../openproxy/SKILL.md). This skill assumes `openproxy` is on PATH and an admin key is in `$OPENPROXY_API_KEY`.

## 0 · Pick your surface

Both surfaces drive the same SQLite store. Use whichever is available; the CLI works offline against the data dir, the REST surface works against a running server.

```bash
# CLI against the local data dir (no server needed)
openproxy --robot provider list

# CLI against a running server (remote management mode)
openproxy --url http://127.0.0.1:4623 --api-key "$OPENPROXY_API_KEY" --robot provider list

# REST — /api/* requires a management/admin key OR a dashboard session cookie.
# A plain proxy key minted with `openproxy key add` is NOT enough for /api/*.
curl -sS http://127.0.0.1:4623/api/providers -H "Authorization: Bearer $OPENPROXY_API_KEY"
```

Every `/api/providers*` and `/api/provider-nodes*` route is gated by `require_dashboard_or_management_api_key`: a bearer that is a management/admin key passes, otherwise a valid dashboard session cookie passes. No header at all → `401`.

## 1 · Inspect what exists

```bash
openproxy provider list                    # human: provider (authType) - name
openproxy --robot provider list            # .data.provider_connections + .data.provider_nodes
openproxy provider get <id-or-name>
openproxy provider node list
```

`--robot` output is a single stable JSON envelope, e.g.:

```json
{"schema":"openproxy.v1.provider.list","ok":true,
 "data":{"provider_connections":[ … ],"provider_nodes":[ … ]},"meta":{}}
```

`provider list --json` pretty-prints the same data without the envelope. Both redacted-vs-not behavior differs by surface: the REST list returns `"apiKey": null` plus `"hasApiKey": true`; the CLI returns the key masked (`sk-t…-123`) on `get` and unmasked on `list`.

REST list accepts filters and pagination (all optional):

```
GET /api/providers?provider=openai&accountStatus=all&sort=priority&page=1&pageSize=50
```

`sort` accepts `priority` (default) or `provider`. Pagination only engages when `page` **or** `pageSize` is present; without either you get every connection sorted by priority.

## 2 · Add a provider connection

### CLI

`provider add` takes a **human name** and a **JSON config blob** as two positionals. The blob is the connection; the name only fills in `name` (and `provider`, if the blob omits it).

```bash
openproxy provider add my-openai \
  '{"provider":"openai","apiKey":"sk-…","priority":1}'

openproxy --robot provider add my-openai \
  '{"provider":"openai","apiKey":"sk-…","priority":1}'
```

```json
{"schema":"openproxy.v1.provider.add","ok":true,
 "data":{"id":"7702b6d0-…","name":"my-openai","provider":"openai",
         "apiKey":"sk-…","authType":"apiKey","priority":1, …},
 "meta":{}}
```

`--robot` reports `authType: "apiKey"` because the CLI infers it when an `apiKey` is present and no explicit `authType` was set. The REST path writes `"apikey"` (lowercase) for the same connection. Both spellings are recognized on read; don't string-match on it.

### REST

```bash
curl -sS -X POST http://127.0.0.1:4623/api/providers \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"provider":"openai","name":"my-openai","apiKey":"sk-…","priority":1,"isActive":true}'
```

Accepted fields: `provider` (required), `name` (required), `apiKey`, `priority`, `globalPriority`, `defaultModel`, `testStatus`, `baseUrl`, `providerSpecificData`, `connectionProxyEnabled`, `connectionProxyUrl`, `connectionNoProxy`, `proxyPoolId`.

Validation on this route, in order:

1. `provider` empty → `400 Provider is required`.
2. `is_valid_provider(provider)` fails → `400 Invalid provider`. The accepted set is 130 aliases (`openai`, `anthropic`, `gemini`, `azure`, `openrouter`, `groq`, `ollama`, `ollama-local`, `deepseek`, `xai`, `cerebras`, `vertex`, …) plus the three web-cookie aliases `grok-web`, `perplexity-web`, `deepseek-web`, plus any `*-compatible-*` node id.
3. `name` empty → `400 Name is required`.
4. `apiKey` empty → `400 API key is required`, **except** `ollama-local`, which is allowed keyless.
5. If `provider` is a `*-compatible-*` id and no node with that id exists → `404 OpenAI Compatible node not found` (or the Anthropic / Custom Embedding wording). The node supplies the `baseUrl`, so the row is refused rather than created pointing nowhere.

The CLI's `provider add` writes straight to the store and skips checks 1–5. An invalid provider string added that way is accepted and only fails later at request time. Prefer REST when you want the validation.

## 3 · Auth types

`authType` is **derived, never written by an edit**. `PUT /api/providers/{id}` with `authType` returns:

```
{"error":"authType is derived from the provider and cannot be set here;
          re-run the provider's connect or import flow to change it"}
```

| Value | Set by | Credential actually used |
|---|---|---|
| `apikey` / `apiKey` | `POST /api/providers`, or CLI `provider add` with an `apiKey` | `apiKey` |
| `cookie` | `POST /api/providers` on `grok-web` / `perplexity-web` / `deepseek-web` | browser session cookie, stored in the `apiKey` field |
| `oauth` | the provider's own connect/import flow (`openproxy provider oauth …`, the dashboard Reconnect button) | `accessToken` / `refreshToken` |

A direct `apiKey` write via `PUT` is only honored for `apikey` / `api_key` auth types; on an OAuth connection it is not applied. To rotate an OAuth credential, refresh or re-import — do not PUT the key.

### OAuth providers

```bash
openproxy provider oauth start <provider>          # returns the URL the user must open
openproxy provider oauth poll  <provider>          # long-polls until the user finishes
openproxy provider oauth status <provider>
openproxy provider oauth refresh <provider> [--refresh-token <rt>]
openproxy provider oauth import-kiro [--auto]      # Kiro SSO cache
openproxy provider oauth iflow-cookie              # raw cookie on stdin
openproxy provider oauth gitlab-pat                # PAT on stdin
```

`start` prints the URL and state metadata — it cannot complete a browser dance on a headless box. In an agent context prefer API-key providers, or hand the user the dashboard at `http://127.0.0.1:4623/dashboard/providers` and let them finish the Reconnect flow.

## 4 · Provider nodes (self-hosted / third-party endpoints)

### The id convention — read this before wiring a node

**The server generates the node id; you never choose it.** `POST /api/provider-nodes` derives it from the node type (`compatible_node_id` in `src/server/api/provider_nodes.rs`):

| Node `type` | Generated id |
|---|---|
| `openai-compatible` **with** `apiType` | `openai-compatible-<apiType>-<uuid4>` — e.g. `openai-compatible-chat-23a5a4fe-99f6-4047-93bf-aecf39974aa9` |
| `openai-compatible` without `apiType` | `openai-compatible-<uuid4>` |
| `anthropic-compatible` | `anthropic-compatible-<uuid4>` |
| `custom-embedding` | `custom-embedding-<uuid4>` |

The prefix is load-bearing. `is_valid_provider` accepts only `*-compatible-*` strings, and `resolve_compatible_node` matches `provider == node.id` exactly. A bare uuid is not a valid `provider` value.

```bash
NODE=$(curl -sS -X POST http://127.0.0.1:4623/api/provider-nodes \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"name":"vllm","type":"openai-compatible","apiType":"chat",
       "baseUrl":"https://vllm.example.com/v1"}' \
  | jq -r '.node.id')
# → openai-compatible-chat-23a5a4fe-99f6-4047-93bf-aecf39974aa9
```

Creating a connection against the node uses the **full generated id**, not a truncated prefix:

```bash
curl -sS -X POST http://127.0.0.1:4623/api/providers \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -d "{\"provider\":\"$NODE\",\"name\":\"vllm-main\",\"apiKey\":\"k\",\"priority\":2}"
```

> **Divergence to know about.** `openproxy provider node add` (CLI) writes a **bare `uuid::Uuid::new_v4()`** id, while the REST route writes the type-prefixed form. A CLI-created node is therefore *not* usable as a `provider` value — `POST /api/providers` will reject it with `Invalid provider`, because the prefix is missing. If you need a routable node, create it over REST. Confirmed against the source: `src/cli/provider_node.rs:196` vs `src/server/api/provider_nodes.rs:107`.

`sanitize_node_base_url` strips a trailing `/messages` (anthropic-compatible) or `/embeddings` (custom-embedding) on create **and** edit, because the executor appends that segment on the way out. A stored URL that still ends in it gets suffixed twice and the upstream 404s.

### Node commands

```bash
openproxy provider node list [--type openai-compatible]
openproxy provider node get <id-or-name>
openproxy provider node add --name vllm --base-url https://vllm.example.com/v1 \
  --type openai-compatible [--api-type chat] [--prefix vllm]
openproxy provider node edit <id-or-name> [--name N] [--base-url U] [--prefix P] [--api-type T]
openproxy provider node delete <id-or-name> [--strict]
openproxy provider node validate <id-or-name> [--api-key K] [--model-id M]
```

`edit` leaves any omitted flag unchanged. `delete --strict` exits `3` on a missing node instead of `0`.

### Node REST surface

| Method | Path | Notes |
|---|---|---|
| `GET` | `/api/provider-nodes` | `{"nodes":[…]}` |
| `POST` | `/api/provider-nodes` | `{name, type, apiType?, baseUrl, prefix?}` → `201 {"success":true,"node":{…}}` |
| `GET` | `/api/provider-nodes/{id}` | `404 {"error":"Node not found"}` |
| `PUT` | `/api/provider-nodes/{id}` | |
| `DELETE` | `/api/provider-nodes/{id}` | |
| `POST` | `/api/provider-nodes/validate` | body `{baseUrl, apiKey, type?, modelId?}` — `apiKey` is **required**, even for an unauthenticated probe |

`POST /api/provider-nodes/validate` probes `<baseUrl>/models` (or `/embeddings` for embedding nodes) without touching the DB:

```bash
curl -sS -X POST http://127.0.0.1:4623/api/provider-nodes/validate \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -d '{"baseUrl":"https://vllm.example.com/v1","apiKey":"k","type":"openai-compatible"}'
# → {"valid":false,"error":"Models endpoint not available","method":null,"dimensions":null}
```

## 5 · Priority

`priority` is a `u32` on the connection. **Lower number wins** — the account selector takes `min_by_key(conn.priority.unwrap_or(999))`, so `1` is tried before `2`, and a connection with no priority sorts last. The REST list sorts ascending by `priority` (ties broken by `provider` name).

```bash
curl -sS -X PUT http://127.0.0.1:4623/api/providers/<connection-id> \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -d '{"priority":1,"isActive":true}'

openproxy provider edit <id-or-name> --priority 1
```

`POST /api/providers` defaults `priority` to `1` when omitted; CLI `provider add` leaves it unset (sorts as 999). `globalPriority` is a separate, cross-provider field — set it only if you know you want it; plain `priority` is the per-provider ordering knob.

`isActive: false` (or `openproxy provider disable <name>`) takes a connection out of routing without deleting it. `enable` puts it back.

## 6 · Edit and delete

```bash
openproxy provider get <id-or-name>
openproxy provider edit <id-or-name> \
  [--api-key K] [--base-url U] [--priority N] [--default-model M]
openproxy provider disable <id-or-name>
openproxy provider enable  <id-or-name>
openproxy provider delete <id-or-name> [--strict]
```

`--strict` on `delete` exits `3` when the target does not exist — use it in scripts so a typo'd name doesn't silently pass.

REST equivalents:

| Method | Path |
|---|---|
| `GET` | `/api/providers/{id}` |
| `PUT` | `/api/providers/{id}` |
| `DELETE` | `/api/providers/{id}` |
| `DELETE` | `/api/batch/providers` — body `{"ids":["…","…"]}`, returns `{"deleted":2}` |

`{id}` is always the **connection id** (uuid), never the provider alias. Aliases on `/api/providers/{id}` return `404 Connection not found`.

`PUT` accepts `name`, `email`, `displayName`, `priority`, `globalPriority`, `defaultModel`, `isActive`, `apiKey`, `testStatus`, `lastError`, `lastErrorAt`, `providerSpecificData`, `connectionProxyEnabled`, `connectionProxyUrl`, `connectionNoProxy`, `proxyPoolId`, and `mode` (`"real"` | `"mock"`). Omitted fields are left unchanged. A payload carrying only `mode` is accepted for providers that have no connection row yet.

## 7 · Verify a connection actually works

There are four probes. Use the narrowest one that answers your question.

```bash
# a) Raw credentials, no DB write — use before creating a connection.
openproxy --robot provider validate --provider openai --api-key "sk-…"
# REST: POST /api/providers/validate  {"provider":"openai","apiKey":"sk-…"}
# → {"valid":false,"error":"Invalid API key"}

# b) The stored connection, live probe. Persists the verdict to testStatus/lastError.
openproxy --robot provider test my-openai
# REST: POST /api/providers/<connection-id>/test
```
```json
{"schema":"openproxy.v1.provider.test","ok":true,
 "data":{"providerId":"7702b6d0-…","provider":"openai","valid":false,
         "latencyMs":640,
         "error":"HTTP 401 — Incorrect API key provided: sk-…"},
 "meta":{}}
```

```bash
# c) Several connections at once.
curl -sS -X POST http://127.0.0.1:4623/api/providers/test-batch \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" -H 'content-type: application/json' \
  -d '{"providerIds":["7702b6d0-…","325f125e-…"]}'
# → {"results":[{"providerId":"…","valid":false,"error":"…","latencyMs":380}]}

# d) The node's endpoint, without a connection.
openproxy --robot provider node validate vllm
# → {"schema":"openproxy.v1.provider-node.validate","ok":true,
#     "data":{"probeUrl":"https://vllm.example.com/v1/models","valid":false,
#             "error":"error sending request for url (…)"}, "meta":{}}
```

A probe returning `valid:false` still exits `0` in `--robot` mode — the envelope is `ok:true` with `valid:false` inside `data`. Branch on `.data.valid`, not on the exit code.

To see the persisted verdict afterwards:

```bash
curl -sS http://127.0.0.1:4623/api/providers \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  | jq '.connections[] | {id, name, provider, authType, isActive, testStatus,
                           healthStatus, lastError, priority}'
```

## 8 · Model catalog per provider

```bash
openproxy provider models list openai                     # built-in + custom + aliases
openproxy provider models disable openai --model gpt-3.5-turbo
openproxy provider models enable  openai --model gpt-3.5-turbo
openproxy provider models test openai [--model gpt-4o]     # real /v1/models probe
openproxy provider models custom add openai my-model [--type chat] [--name "My Model"]
openproxy provider models custom remove openai my-model
openproxy provider models custom list
openproxy provider models alias set openai gpt-4o fast
openproxy provider models alias unset fast
```

REST, all keyed by **connection id**:

| Method | Path |
|---|---|
| `GET` | `/api/providers/{id}/models` — merged built-in + custom list; `401` surfaces as `{"error":"Failed to fetch models: 401"}` |
| `POST` | `/api/providers/{id}/import-models` — import the provider's remote catalog into `customModels` |
| `POST` | `/api/providers/{id}/test-models` |

`import-models` merges the remote catalog into local config and preserves removed custom models for compatibility, so a re-import does not drop a model you added by hand.

## 9 · Declarative `apply`

`provider apply` reads a YAML or JSON document from `--from-file <path>` (or stdin with `-`). **The document is a bare object or a top-level array of connections — there is no `{"providers": […]}` wrapper.** A wrapped document fails:

```
{"schema":"openproxy.v1.error","ok":false,
 "error":{"code":"validation","message":"invalid item: missing field `name`"}}
```

> The entry skill (`.agents/skills/openproxy/SKILL.md` §4) shows the `{"providers": […]}` wrapper. That form is rejected by the current binary; use the array form below.

```bash
cat > /tmp/providers.json <<'JSON'
[
  {"name":"openai-main","provider":"openai","apiKey":"sk-…","priority":1,"isActive":true},
  {"name":"anthropic-main","provider":"anthropic","apiKey":"sk-ant-…","priority":2}
]
JSON

openproxy --robot provider apply --from-file /tmp/providers.json --dry-run
openproxy --robot provider apply --from-file /tmp/providers.json
openproxy --robot provider apply --from-file /tmp/providers.json --prune   # delete unmanaged rows
```

```json
{"schema":"openproxy.v1.provider.apply","ok":true,
 "data":{"diff":{"created":["openai-main"],"updated":[],"unchanged":[],"deleted":[]},
         "dry_run":true,"prune":false,"summary":"1 created"},
 "meta":{}}
```

Always `--dry-run` first. `--prune` makes the document authoritative and deletes every connection not in it — ask the user before running it against a live config.

Confirm the shapes with the built-in schema introspection:

```bash
openproxy schema show provider        # JSON Schema for a connection
openproxy schema example provider     # → {"apiKey":"sk-…","isActive":true,"name":"openai-main","priority":10,"provider":"openai"}
openproxy schema show provider-node   # enum: openai-compatible | anthropic-compatible | gemini-compatible
```

## 10 · Simulation mode (no live credentials)

```bash
openproxy provider mode <provider> [real|mock]   # omit MODE to show
openproxy provider status                         # all providers
```

```bash
curl -sS http://127.0.0.1:4623/api/mock/status -H "Authorization: Bearer $OPENPROXY_API_KEY"
# → {"envForce":false,"forcedAll":false,
#     "providers":{"openai":{"configured":"real","effective":"real",
#                             "reason":"default","simulationSupported":true}, …}}
```

`effective` is the mode actually in force (`configured` overridden by `envForce` / `forcedAll`). Useful for exercising fallback paths without spending quota. `PUT /api/providers/{provider-name} {"mode":"mock"}` writes the same setting over REST.

## Common failure modes

| Symptom | Cause | Fix |
|---|---|---|
| `400 Invalid provider` on `POST /api/providers` | Alias not in the 130-entry set, or the node id is missing its `*-compatible-` prefix | Use a real alias; for a node, pass the full generated id from `POST /api/provider-nodes` |
| `404 OpenAI Compatible node not found` | No node whose `id` equals the `provider` value | Create the node, then use the id it returns |
| `400 API key is required` | `apiKey` empty on a create | Supply one; `ollama-local` is the only keyless exception |
| `authType is derived from the provider…` on `PUT` | Sending `authType` to a generic edit | Drop the field; use `provider oauth …` or the dashboard Reconnect |
| CLI node id is a bare uuid and connections refuse to use it | CLI and REST derive ids differently | Create the node over REST, or delete and recreate it |
| `401` on every `/api/*` | Using a proxy key instead of the admin/management key, or no session cookie | `openproxy --robot server init \| jq -r .data.admin_key.key`; the key is shown once |
| `test` reports `valid:false` but exit code is `0` | `--robot` envelopes are always `ok:true` when the command ran | Branch on `.data.valid` |
| Node 404s upstream with a doubled path segment | Stored `baseUrl` still ends in `/messages` or `/embeddings` | Re-`PUT` the node; the trailing segment is stripped on edit |
| `delete` of a typo'd name exits `0` | Non-strict delete | Pass `--strict` in scripts (exit `3`) |

## Environment variables

| Var | Purpose |
|---|---|
| `OPENPROXY_API_KEY` | Bearer for `--robot` CLI calls and `/api/*` curl calls. Must be an **admin/management** key for `/api/*`. |
| `OPENPROXY_URL` | Remote management mode — target a running server instead of the local data dir (alias for `--url`). |
| `DATA_DIR` / `--data-dir` | Data directory (default `~/.openproxy`). |
| `PORT` | Server port (default `4623`). |

## When _not_ to use this skill

- **Combos / fallback ordering across providers** — a combo is a separate resource; see `.agents/skills/openproxy-combos/SKILL.md`.
- **Keys, pools, or CLI-tool wiring** — see `.agents/skills/openproxy/SKILL.md`.
- **Serving traffic** — this skill configures providers; the OpenAI-compatible serving surface is `.agents/skills/openproxy-chat/SKILL.md`.

## See also

- Install / server lifecycle / CLI wiring: [`.agents/skills/openproxy/SKILL.md`](../openproxy/SKILL.md)
- Intentional divergences from 9router: [`docs/parity-9router.md`](../../../docs/parity-9router.md)
- Schemas: `openproxy schema show provider`, `openproxy schema show provider-node`
- REST route source: `src/server/api/mod.rs` (connections), `src/server/api/admin_items.rs` (per-id CRUD + batch), `src/server/api/provider_nodes.rs` (nodes), `src/server/api/providers.rs` (test/validate/import routes)
- CLI source: `src/cli/mod.rs` (`ProviderCmd`), `src/cli/provider_node.rs`, `src/cli/provider_models.rs`, `src/cli/provider_ext.rs`
- Reference: https://github.com/quangdang46/openproxy
