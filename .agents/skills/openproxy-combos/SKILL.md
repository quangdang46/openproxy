---
name: openproxy-combos
description: Create, edit, test, and dispatch OpenProxy combos — ordered fallback chains across providers and models. Use whenever the user asks to build a combo, set up failover or round-robin across models, list or delete combos, check why a combo 400s or 503s, or use a combo as the `model` field in /v1/chat/completions.
---

# openproxy-combos — build and operate fallback chains

A **combo** is a named, ordered list of `<provider>/<model>` entries that
OpenProxy dispatches as a single model. It is the router's failover unit: a
client sends one model name, the combo walks the list, and the first member
that answers wins.

Combos are one of two core data types (alongside providers). They are stored
in the data dir, edited from `/dashboard/combos`, and are first-class entries
in `GET /v1/models`.

Everything below was verified against the source in this repo. Where OpenProxy
deliberately diverges from the 9router reference, the divergence is called
out and `docs/parity-9router.md` is cited.

## 0 · Decide CLI or REST first

This is the one decision that trips people up, so settle it before writing
anything.

`openproxy combo …` is a **local-only** command. `src/cli/mod.rs` handles
`Command::Combo` by calling `Db::load()` and handing the DB straight to
`combo::run` — it never consults `--url` / `--api-key`. Two consequences,
both confirmed by running the binary:

- `openproxy combo create --url http://127.0.0.1:4623 --api-key $KEY` writes
  to the local data dir, **not** to the server. The server's `/api/combos`
  stays empty. `--url` is silently ignored.
- A combo created with the CLI while a server is running is **invisible to
  that server** — absent from `/api/combos` and from `GET /v1/models` — until
  the server is restarted and reloads the data dir.

| Situation | Use |
|---|---|
| No server running, or you are about to restart one | `openproxy combo …` CLI |
| A server is already running | REST API (`/api/combos`) — the only path that goes through the live process |
| Declarating many combos at once | `openproxy combo apply --from-file` against a stopped server, or the REST API via curl |

If you have already used the CLI against a live server, restart it:

```bash
openproxy server stop && openproxy server start --detach --no-open
```

## 1 · What a combo record looks like

The stored record (`Combo` in `src/types/mod.rs`) is `camelCase` on the wire,
with the extras flattened into the top level:

```json
{
  "id": "212f459e-d876-43d6-b5e3-541d2030dbbc",
  "name": "premium-coding",
  "models": ["openai/gpt-4o", "anthropic/claude-3-5-sonnet"],
  "disabledModels": [],
  "kind": "llm",
  "createdAt": "2026-09-28T14:53:16.422574+00:00",
  "updatedAt": "2026-09-28T14:53:16.422574+00:00",
  "strategy": "fallback"
}
```

| Field | Meaning |
|---|---|
| `id` | UUID. The `{id}` REST routes key on this, **not** the name. |
| `name` | The model name clients send. Letters, digits, `_`, `-`, `.`; max 100 chars. |
| `models` | Ordered members, each `<provider-prefix>/<model-id>`. Order is the fallback priority. |
| `disabledModels` | Members the operator muted. Filtered out **before** rotation — never dispatched. |
| `kind` | Media modality (`llm` / `tts` / `image` / `webSearch` / `webFetch` / …). **Never a strategy.** |
| `strategy` | Dispatch strategy, stored in the flattened extras as `strategy`. |
| `isActive` | Flattened extra. A combo with no marker is **active**. |

> **`kind` is not `strategy`.** Putting a strategy in `kind` hides the combo
> from the Combos page and from `GET /v1/models`, because both filter on
> `kind`. A SQLite patch (`COMBO_KIND_STRATEGY_LEAKS` in
> `src/db/sqlite/patch.rs`) clears historical leaks, but new writes must go to
> `strategy`.

## 2 · Member naming

Each entry is `<provider-prefix>/<model-id>`. The prefix resolves against, in
order: a configured connection's provider id, a provider alias
(`resolve_provider_alias`), or a **provider node id** — for a custom
`openai-compatible` / `anthropic-compatible` / `custom-embedding` node the
prefix is the node's **UUID**, not its display name.

A prefix with no matching connection or node will not dispatch. Verify
membership before you rely on it — see step 6.

Combos may name other combos as members; they are expanded at dispatch time
with a depth cap of 8 and a cycle guard, so `A` containing `A` terminates
instead of recursing.

## 3 · Create a combo

### CLI (server stopped)

`--name` is a **flag** on `create` (every other subcommand takes the name
positionally):

```bash
openproxy combo create \
  --name premium-coding \
  --models "openai/gpt-4o,anthropic/claude-3-5-sonnet" \
  --strategy fallback
```

`--models` is required and must list at least one member. Creating a name that
already exists exits with a `conflict` error — use `combo edit` or
`combo apply` instead.

Robot mode gives you a stable envelope to parse:

```bash
openproxy --robot combo create --name premium-coding \
  --models "openai/gpt-4o,anthropic/claude-3-5-sonnet" \
  | jq -r '.data.id'
```

### REST (server running)

```bash
K=op-20150fedf5f12c85abb8ef28498ae59fa58652eaa4c57ebb   # admin / management key
B=http://127.0.0.1:4623

curl -sS -X POST "$B/api/combos" \
  -H "Authorization: Bearer $K" \
  -H 'content-type: application/json' \
  -d '{
        "name": "premium-coding",
        "models": ["openai/gpt-4o", "anthropic/claude-3-5-sonnet"],
        "strategy": "fallback",
        "kind": "llm",
        "isActive": true
      }'
```

`201` returns the created combo. `400` for a missing/empty name, a name with
illegal characters, or a duplicate name. Both routes require the admin /
management key — an unauthenticated call is `401`.

Discover the payload shape instead of guessing it:

```bash
openproxy schema show combo      # JSON Schema, incl. the strategy enum
openproxy schema example combo   # ready-to-edit example
```

## 4 · Read, edit, enable/disable, delete

### CLI

```bash
openproxy combo list                      # openproxy.v1.combo.list
openproxy combo get premium-coding        # openproxy.v1.combo.get
openproxy combo edit premium-coding --models "openai/gpt-4o,groq/llama-3.1-70b"
openproxy combo edit premium-coding --strategy round-robin
openproxy combo disable premium-coding    # isActive=false
openproxy combo enable  premium-coding    # isActive=true
openproxy combo delete premium-coding
openproxy combo delete premium-coding --strict   # exit 3 if absent
```

`edit` is a partial update — any flag you omit is left unchanged. Plain
`delete` on a missing combo is a **no-op that exits 0**, which makes it safe in
scripts; add `--strict` when a missing combo should fail the step.

### REST

All four routes key on the combo **id**, from `GET /api/combos`:

```bash
ID=$(curl -sS -H "Authorization: Bearer $K" "$B/api/combos" \
      | jq -r '.combos[] | select(.name=="premium-coding") | .id')

curl -sS -H "Authorization: Bearer $K" "$B/api/combos/$ID"
```

`PUT` is a partial update; omitted fields are left untouched. `disabledModels`
replaces the whole list when present. `strategy` merges into the extras;
`extra` merges key-by-key:

```bash
curl -sS -X PUT "$B/api/combos/$ID" \
  -H "Authorization: Bearer $K" \
  -H 'content-type: application/json' \
  -d '{
        "models": ["openai/gpt-4o", "anthropic/claude-3-5-sonnet"],
        "disabledModels": ["anthropic/claude-3-5-sonnet"],
        "strategy": "round-robin",
        "kind": "llm",
        "isActive": true
      }'

curl -sS -X DELETE "$B/api/combos/$ID" -H "Authorization: Bearer $K"
# → {"success":true}
```

A wrong id is `404 {"error":"Combo not found"}`. Bulk delete takes ids:

```bash
curl -sS -X DELETE "$B/api/batch/combos" \
  -H "Authorization: Bearer $K" -H 'content-type: application/json' \
  -d '{"ids":["212f459e-…","7c1a0b44-…"]}'
# → {"deleted":2}
```

Deleting a combo (by either route) clears its rotation index and its
auto-quarantine entries, so a later combo of the same name starts clean.

> There are by-name `update_combo_api` / `delete_combo_api` handlers in
> `src/server/api/mod.rs`, but they are `#[allow(dead_code)]` and **have no
> route**. Do not build against `PUT /api/combos/<name>` — it 404s. Use the id.

### Declarative apply

`combo apply` takes a **bare object or a bare array** — not a
`{"combos": […]}` wrapper. (The `provider apply` form is wrapped; this one is
not.)

```bash
cat > /tmp/combos.json <<'JSON'
[
  {
    "name": "premium-coding",
    "models": ["openai/gpt-4o", "anthropic/claude-3-5-sonnet"],
    "strategy": "fallback"
  },
  {
    "name": "cheap-pool",
    "models": ["groq/llama-3.1-70b", "ollama/qwen3-coder"],
    "strategy": "cheapest",
    "isActive": true
  }
]
JSON

openproxy --robot combo apply --from-file /tmp/combos.json
# {"schema":"openproxy.v1.combo.apply","ok":true,
#  "data":{"diff":{"created":["cheap-pool"],"unchanged":[],"updated":[]},
#          "prune":false,"summary":"1 created"},"meta":{}}

openproxy combo apply --from-file - --prune   # stdin; delete anything not in the doc
```

Apply is idempotent — a second run reports `unchanged`. `--prune` makes the
file authoritative and **deletes** combos missing from it. Read the diff
before running with `--prune` against a real data dir.

## 5 · Use a combo as a model name

A combo is called by its `name` in the `model` field. `get_model_info`
(`src/core/model/mod.rs`) resolves the model string in this order:

1. **`combo:<name>` prefix** → forced combo routing, even if a provider of that
   name exists.
2. **Contains `/`** → direct route to that provider.
3. **Bare name matching a combo** → combo. This check runs **before** the
   model-alias map, so a combo name shadows a same-named model alias.
4. Otherwise → model-alias map, built-in alias, then
   `infer_provider_from_model_name` → direct.

Because rule 2 short-circuits, a combo name can never contain `/`. (The name
charset already forbids it.)

```bash
curl -sS "$B/v1/chat/completions" \
  -H "Authorization: Bearer $K" \
  -H 'content-type: application/json' \
  -d '{"model":"premium-coding","messages":[{"role":"user","content":"ping"}]}'

# Or force it explicitly, bypassing name resolution entirely:
curl -sS "$B/v1/chat/completions" \
  -H "Authorization: Bearer $K" \
  -H 'content-type: application/json' \
  -d '{"model":"combo:premium-coding","messages":[{"role":"user","content":"ping"}]}'
```

A combo name that resolves to nothing reachable by rule 4 returns
`400 {"error":{"code":"bad_request","message":"Unknown combo model",…}}`.

**Where combos work**

| Endpoint | Combo |
|---|---|
| `POST /v1/chat/completions` | Yes — the primary path |
| `POST /v1/search` (`kind: webSearch`) | Yes |
| `POST /v1/web/fetch` (`kind: webFetch`) | Yes |
| `POST /v1/embeddings`, images, tts, stt | **No** — `400 Combos not supported for embeddings` |

The embeddings wording is an intentional divergence from 9router (which says
`Invalid model format`); same 400, same body shape, more actionable text. The
shared media resolver applies it to image/tts/stt too. See
`docs/parity-9router.md`.

Active combos also appear in the catalog:

```bash
curl -sS "$B/v1/models" -H "Authorization: Bearer $K" \
  | jq '.data[] | select(.owned_by=="combo") | {id, kind}'
```

Disabled combos and combos whose `kind` does not match the requested filter
are omitted — the catalog never advertises a model that would 400 on use.

## 6 · Test a combo

### Static membership check (no network)

```bash
openproxy combo test premium-coding
# combo 'premium-coding' resolution:
#   OK  openai/gpt-4o (openai)
#   MISS anthropic/claude-3-5-sonnet (anthropic)
```

```bash
openproxy --robot combo test premium-coding | jq '.data.reachable'
```

`combo test` is **local only** — it makes no upstream call. It splits each
member on the first `/` and marks it `resolved` when the prefix matches a
configured connection, a known provider alias, or a provider node id.
`reachable: true` means every member resolved; it does **not** mean the
upstreams are healthy.

Two traps in this output:

- The `kind` field in the robot payload is `Combo.kind` (the media modality)
  defaulting to the literal string `"fallback"`. **It is not the strategy.** Read
  `strategy` via `combo get` instead.
- `--prompt` is accepted and currently ignored — there is no `--live` flag.

### Live per-member health check

`POST /api/combos/test-model` runs a real request against one member
(`max_tokens: 1`, non-streaming, `"hi"`, 15s timeout; only 2xx counts as ok).
This is the per-row test the dashboard's combo editor uses:

```bash
curl -sS -X POST "$B/api/combos/test-model" \
  -H "Authorization: Bearer $K" -H 'content-type: application/json' \
  -d '{"model":"openai/gpt-4o"}'
# {"model":"openai/gpt-4o","ok":true,"latency_ms":812,"error":null}
```

### End-to-end

```bash
curl -sS "$B/v1/chat/completions" \
  -H "Authorization: Bearer $K" -H 'content-type: application/json' \
  -d '{"model":"premium-coding","messages":[{"role":"user","content":"ping"}]}'
```

## 7 · Strategies

Accepted values, from `openproxy schema show combo`:

| Strategy | Behaviour |
|---|---|
| `fallback` | Declared order, first success wins. **The default.** |
| `round-robin` | Rotate the starting member per request, then reorder by required capability. |
| `cheapest` | Sort by pricing-table cost ascending (free first). |
| `fastest` | Sort by latency hint ascending; members with no hint keep their order. |
| `quality` | Capability-tier ordering (vision / pdf / audio / video aware). |
| `fusion` | Fan out to a panel, judge the answers, return the winner. |
| `sticky-round-robin` | Accepted as a name; see the caveat below. |
| `hedging`, `shadow`, `auto-combo` | Scaffolded modules — see the caveat below. |

Resolution order (`strategy_for_combo` in `src/core/combo/mod.rs`) is:

1. `settings.comboStrategies["<combo-name>"]`
2. the combo's own `strategy` (its `extra.strategy`)
3. `settings.comboStrategy` — the global default, `"fallback"` on a fresh install

A `comboStrategies` entry that names no strategy (a judge-model-only fusion
config, say) falls through rather than pinning `fallback` over the global
setting. Each entry accepts a bare string or the nested 9router object:

```json
{
  "comboStrategy": "fallback",
  "comboStrategies": {
    "cheap-pool": "cheapest",
    "fuse-me": {
      "fallbackStrategy": "fusion",
      "judgeModel": "gpt-4o-mini",
      "fusionTuning": { "minPanel": 2, "stragglerGraceMs": 8000 }
    }
  }
}
```

```bash
openproxy settings get --key comboStrategy
openproxy settings set --key comboStrategy --value round-robin

# comboStrategies is an object — use --value-json for it, not --value
openproxy settings set --key comboStrategies --value-json '{"cheap-pool":"cheapest"}'
```

### Two caveats worth knowing before you configure

- **`sticky-round-robin` is not a distinct runtime strategy.** It is a valid
  name for the CLI normalizer and the schema enum, but `parse_combo_strategy`
  has no arm for it, so it resolves to `fallback` ordering. Stickiness is
  actually a property of `round-robin`, controlled by
  `settings.comboStickyRoundRobinLimit` — a member keeps serving until the
  limit is reached. Set the strategy to `round-robin` and tune that setting;
  do not rely on `sticky-round-robin` alone.
- **`hedging`, `shadow`, and `auto-combo` are scaffold-only.** The modules
  exist under `src/core/combo/`, but the chat dispatcher does not wire them,
  so a combo set to one of them dispatches as `fallback`. `docs/parity-9router.md`
  records this as an intentional scaffold-until-demand gap, along with the
  P3 backlog item to wire them.

`cheapest` / `fastest` depend on the pricing table; a member with no pricing
entry sorts last on `cheapest`.

## 8 · Failure, fallback, and quarantine

The dispatcher has **no permanent-failure tier** — every error falls through
to the next member, and the cooldown duration comes from the error rules. A
`400` from one member does not lock it out longer than a `500` does. This is
9router parity (`accountFallback.js`).

- On a `502`/`503`/`504` whose cooldown is ≤ 5000 ms, the dispatcher waits that
  long before the next member, giving the upstream a brief recovery window.
- **When every member fails**, the response carries the **first** member's
  failure *status* and the **last** member's *message* and body. A message
  containing the substring `no credentials` is rewritten to `503`.
- **Every attempted member is auto-quarantined** after a full combo failure,
  so the next request does not immediately re-attempt a known-broken member
  and make a CLI agent appear to hang.

Inspect and clear quarantine:

```bash
curl -sS -H "Authorization: Bearer $K" "$B/api/combos/$ID/health"
# {"comboId":"…","comboName":"premium-coding","disabledModels":[],
#  "quarantined":[{"model":"openai/gpt-4o","remainingSeconds":100}]}

# Clear every quarantined member
curl -sS -X DELETE -H "Authorization: Bearer $K" "$B/api/combos/$ID/health"

# Or just one member
curl -sS -X DELETE -H "Authorization: Bearer $K" \
  "$B/api/combos/$ID/health?model=openai%2Fgpt-4o"
```

Quarantine is in-memory and advisory: the dispatcher repopulates it on the
next genuine failure, and a server restart clears it.

### The two 400/503 cases you will actually hit

| Response | Cause |
|---|---|
| `503 All combo members are currently quarantined after recent failures` | Every member is in the auto-quarantine map. Wait out the TTL, or `DELETE …/health`. |
| `400 All combo members are disabled` | Nothing is dispatchable and the set was not purely quarantined — usually every member in `disabledModels`. |

Because these share the same pre-gate, a combo that mixes one manually
disabled member with one quarantined member reports the `400` wording even
though the disabled list is not the whole story. Check `/health` before
concluding the operator muted everything.

## 9 · Pre-gates applied before dispatch

In this order, before any member is tried:

1. `disabledModels` — operator-muted members are dropped. They stay in the
   configured list so you can re-enable them without retyping.
2. Auto-quarantine — members parked by a recent failure are dropped.
3. Round-robin rotation.
4. Capability reorder — members that satisfy the request's required
   capabilities (vision, pdf, audio, video) float up.

Round-robin **capacity is an ordering preference, never a filter.** A
saturated member is still attempted, just last — 9router awaits every member
unconditionally, and dropping one here would lose the provider's own 503 and
`Retry-After`. If you see a `500`/`503` from a member you thought was being
skipped, it was tried.

A note on a divergence to expect: on a full combo failure OpenProxy sources
`Retry-After` from the member's JSON body, so the header appears **only** when
a member body genuinely carries `retryAfter`. 9router effectively never emits
it. `docs/parity-9router.md` records this as intentional.

## Common failure modes & fixes

| Symptom | Fix |
|---|---|
| Combo created via CLI, absent from `/v1/models` | The CLI is local-only and the running server does not reload. Use the REST API, or restart the server. |
| `401` on `/api/combos` | Needs the admin / management key, not a `/v1/*` proxy key. |
| `400 Unknown combo model` on chat | The name does not resolve to an active, non-empty combo. Check `isActive` and that `models` is non-empty. |
| `404 Combo not found` on `PUT`/`DELETE /api/combos/…` | You passed a name. Those routes take the **id**. |
| `combo create` exits with `conflict` | Name already exists — use `combo edit` or `combo apply`. |
| `combo test` says `MISS` for a member | No connection/alias/node matches the prefix. For a custom node the prefix is the node **UUID**. |
| Combo missing from the dashboard entirely | A strategy was written into `kind`. Move it to `strategy`. |
| `503 All combo members are currently quarantined` | `DELETE /api/combos/{id}/health` (advisory — a real failure re-quarantines). |
| Every member returns `No active credentials for provider: X` | The provider has no usable connection. Fix the provider, not the combo. |
| `Combos not supported for embeddings` | Combos do not expand on embeddings/media routes. Point at a single `provider/model`. |
| `combo apply` errors with `missing field name` | The input must be a bare object or array — not `{"combos": [...]}`. |
| `combo delete` on a missing name does not fail | Expected: plain delete is a 0-exit no-op. Use `--strict`. |
| `sticky-round-robin` behaves like `fallback` | Expected. Use `--strategy round-robin` plus `settings.comboStickyRoundRobinLimit`. |
| `combo test` `kind` says `fallback` on a round-robin combo | `kind` is the media modality, not the strategy. Read `combo get` for `strategy`. |

## Verification checklist

```bash
# 1. Exists, and the members resolve locally
openproxy combo get premium-coding
openproxy combo test premium-coding

# 2. Advertised in the catalog
curl -sS "$B/v1/models" -H "Authorization: Bearer $K" \
  | jq '.data[] | select(.id=="premium-coding") | {id, owned_by}'

# 3. Each member has a live credential
curl -sS -X POST "$B/api/combos/test-model" \
  -H "Authorization: Bearer $K" -H 'content-type: application/json' \
  -d '{"model":"openai/gpt-4o"}' | jq '{ok, latency_ms, error}'

# 4. End-to-end dispatch
curl -sS "$B/v1/chat/completions" \
  -H "Authorization: Bearer $K" -H 'content-type: application/json' \
  -d '{"model":"premium-coding","messages":[{"role":"user","content":"ping"}]}' \
  | jq '{model, choices: (.choices|length)}'

# 5. Nothing is quarantined or muted
curl -sS -H "Authorization: Bearer $K" "$B/api/combos/$ID/health" | jq
```

Upstream behaviour only changes with the binary — refresh the parity
expectations with:

```bash
cargo test -p openproxy --lib combo
```

## When _not_ to use this skill

- Installing the binary, initializing a data dir, or capturing the admin key —
  use `.agents/skills/openproxy/SKILL.md`.
- Configuring providers, connections, or credentials — a combo can only route
  to a provider you already configured. Use `.agents/skills/openproxy-providers/SKILL.md`.
- Wiring a coding CLI (Claude Code, Codex, opencode, …) to the proxy — a combo
  is just the model name you would put in that config. Use
  `.agents/skills/openproxy-cli-tools/SKILL.md`.
- Debugging an end-to-end failover run across many providers — the parity doc
  and the log tracing in `target: openproxy::combo` cover more than this skill.

## See also

- Dashboard: `http://127.0.0.1:4623/dashboard/combos`
- Install / operate / auth: `.agents/skills/openproxy/SKILL.md`
- Intentional divergences, pipeline order, executor dispatch: `docs/parity-9router.md`
- Schema introspection: `openproxy schema show combo`, `openproxy schema example combo`
- CLI reference: `openproxy combo --help`
