---
name: openproxy-cli-tools
description: Wire AI coding CLIs into the local OpenProxy router at 127.0.0.1:4623 — Claude Code, Codex CLI, OpenCode, Cline, Roo, Kilo Code, Continue, Copilot, Cursor, Factory Droid, OpenClaw, Hermes, jcode, Grok Build, Devin CLI, Claude Cowork. Covers /api/cli-tools and the per-tool *-settings routes, which model each tool should use, and how to verify a tool is really talking to the proxy. Use when a CLI should route through OpenProxy, when a tool's settings file needs the proxy base URL or key, or when a "wired" tool still talks to its vendor.
---

# openproxy-cli-tools — wire AI coding CLIs into the proxy

Extends §5 of [`.agents/skills/openproxy/SKILL.md`](../openproxy/SKILL.md), which shows the one-liner per tool. This skill is the reference version: every route, every settings-file path, the exact POST body each endpoint accepts, and the verification that proves traffic is flowing.

Read the entry skill first if the server isn't running yet. This skill assumes `openproxy server start --detach --no-open` has already succeeded and `$OPENPROXY_API_KEY` holds the management key.

**The one thing to get right:** OpenProxy normalizes the base URL for you. Send `http://127.0.0.1:4623` and the server appends `/v1` wherever the tool expects a versioned path. Sending `.../v1` yourself is fine too — `normalize_v1_base_url()` is idempotent.

## 0 · Preflight

```bash
# Server is up?
curl -sS http://127.0.0.1:4623/health | head -c 200

# Management key present?
test -n "$OPENPROXY_API_KEY" && echo "key in env" || echo "need openproxy server init"
```

## 1 · Auth model for `/api/cli-tools/*`

Every route below is gated by `require_dashboard_or_management_api_key()`. Two accepted shapes:

| Method | Header |
|---|---|
| Management key | `Authorization: Bearer $OPENPROXY_API_KEY` |
| Management key (alt) | `x-api-key: $OPENPROXY_API_KEY` |
| Browser session | `auth_token` cookie from `POST /api/auth/login` |

If a request carries *any* key header, the dashboard-session path is skipped and the key is validated strictly. A malformed key therefore fails even on a server with `requireLogin` off — don't debug that as a missing cookie.

```bash
export OP=http://127.0.0.1:4623
export AUTH="Authorization: Bearer $OPENPROXY_API_KEY"

curl -sS "$OP/api/cli-tools" -H "$AUTH"
```

## 2 · Route map

Every path below was read out of `src/server/api/cli_tools.rs` and `src/server/api/cli_tools/*.rs`. Routes that exist on disk but are not mounted into `cli_tools::routes()` are called out explicitly rather than documented as if they worked.

### Writable (GET / POST / DELETE unless noted)

| Tool | Route | Settings file the server edits |
|---|---|---|
| Claude Code | `/api/cli-tools/claude-settings` | `~/.claude/settings.json` |
| Codex CLI | `/api/cli-tools/codex-settings` | `~/.codex/config.toml` **+** `export OPENAI_BASE_URL` in your shell profiles |
| OpenCode | `/api/cli-tools/opencode-settings` (GET/POST/**PATCH**/DELETE) | `~/.config/opencode/opencode.jsonc` |
| Copilot | `/api/cli-tools/copilot-settings` | `chatLanguageModels.json` (VS Code / Cursor user data) |
| Factory Droid | `/api/cli-tools/droid-settings` | `~/.factory/settings.json` |
| OpenClaw | `/api/cli-tools/openclaw-settings` | `~/.openclaw/openclaw.json` |
| Cline | `/api/cli-tools/cline-settings` | `~/.cline/data/globalState.json` + `secrets.json` |
| Kilo Code | `/api/cli-tools/kilo-settings` | `~/.local/share/kilo/auth.json` (+ VS Code `settings.json`) |
| DeepSeek TUI | `/api/cli-tools/deepseek-tui-settings` | `~/.deepseek/config.toml` |
| jcode | `/api/cli-tools/jcode-settings` | `~/.jcode/config.toml` + `$XDG_CONFIG_HOME/jcode/provider-openproxy.env` |
| Grok Build | `/api/cli-tools/grok-build-settings` | `~/.grok/config.toml` |
| Hermes | `/api/cli-tools/hermes-settings` | `~/.hermes/config.yaml` + `~/.hermes/.env` |
| Claude Cowork | `/api/cli-tools/cowork-settings` — **loopback-only** | per-apply config JSON under the Cowork config dir |

### Read-only (no write route — wire the tool by hand)

| Tool | Route | Why |
|---|---|---|
| Devin CLI | `/api/cli-tools/devin-settings` | GET only; reports whether the `devin` binary was found and its version. The Devin *provider* spawns `devin acp` — the CLI is a local dependency, not a routed client. |

### Not mounted — these 404, wire the tool by hand

`continue_settings.rs`, `cursor_settings.rs`, and `roo_settings.rs` exist under `src/server/api/cli_tools/` but are **not declared as `mod` and not merged** into `cli_tools::routes()`, so they are never compiled. `GET /api/cli-tools/continue-settings`, `/roo-settings`, and `/cursor-settings` all return `404`.

| Tool | Status | Do this instead |
|---|---|---|
| Continue | `404` | Add a `models[]` entry to `~/.continue/config.json` by hand — see §5c. |
| Roo | `404` | Roo Settings → API Provider → OpenAI Compatible, base URL `http://127.0.0.1:4623/v1`. |
| Cursor | `404` | Cursor proxies through its own server, so it needs a tunnel/cloud endpoint URL, not `127.0.0.1`. Cursor Settings → Models → enable "OpenAI API key" → View All Model → Add Custom Model. |

### Discovery / execution

| Route | Method | Purpose |
|---|---|---|
| `/api/cli-tools` | GET | Built-in tool names: `provider-list`, `key-list`, `pool-list`, `pool-status`, `route` |
| `/api/cli-tools/all-statuses` | GET | Every writable tool's status in one shot — the verification endpoint |
| `/api/cli-tools/help` | GET | Help index from the server |
| `/api/cli-tools/execute` | POST | Run an arbitrary command (`{command, args, timeoutSecs}`) |
| `/api/cli-tools/run/{tool_name}` | POST | Run one of the four supported built-ins |
| `/api/cli-tools/openclaw-config` | GET | Resolved base URL + active key + model list for OpenClaw |
| `/api/cli-tools/cowork-mcp-registry` | GET | Anthropic MCP marketplace registry |
| `/api/cli-tools/cowork-mcp-tools` | GET + POST | Probe an MCP server's tool list |
| `/api/cli-tools/antigravity-mitm` | GET/POST/DELETE/PATCH | Start/stop/toggle Antigravity MITM |
| `/api/cli-tools/antigravity-mitm/alias` | GET/PUT/DELETE | MITM hostname alias |

## 3 · Step 1 — pick the model first

The tool config stores a *model id*, and OpenProxy resolves it across every provider. Do not hardcode a vendor name; ask the router what it currently serves.

```bash
curl -sS "$OP/v1/models" -H "$AUTH" \
  | jq -r '.data[].id' | sort | head -40
```

Pick from that list, or from a combo you created. Combo entries are `<provider-key>/<model-id>`. After wiring, the same id is what appears in the tool's config file — for OpenCode it is additionally namespaced as `openproxy/<model>`.

## 4 · Step 2 — see what is already wired

Always read before you write. `all-statuses` returns one object per tool, each with `installed`, `hasOpenProxy` (or an equivalent), and the path it manages:

```bash
curl -sS "$OP/api/cli-tools/all-statuses" -H "$AUTH" \
  | jq 'to_entries[] | {tool: .key, installed: .value.installed, wired: (.value.hasOpenProxy // null)}'
```

Keys returned: `claude`, `cline`, `codex`, `copilot`, `cowork`, `deepseek-tui`, `droid`, `grok-build`, `hermes`, `jcode`, `kilo`, `openclaw`, `opencode`.

`hasOpenProxy: true` means the server found *its own* marker in the file. It does **not** mean the tool is talking to the proxy — see §7.

## 5 · Step 3 — apply

### 5a. CLI path (preferred for the nine supported tools)

```bash
openproxy tool list
openproxy tool show claude
openproxy tool apply opencode --endpoint http://127.0.0.1:4623 \
  --api-key "$OPENPROXY_API_KEY" --model 'my-combo' --dry-run
openproxy tool apply opencode --model 'my-combo'          # real write
```

`openproxy tool apply` accepts exactly: **claude, codex, continue, copilot, openclaw, hermes, cowork, opencode, droid**. (`--help` lists only six of them — the help text lags the code; the const `SUPPORTED_TOOLS` in `src/cli/tool.rs` is the truth. `continue` is in that list but its route is not mounted — it will 404. See §2.)

It builds a per-tool POST body, so the shapes differ:

| Tool | Body `openproxy tool apply` sends |
|---|---|
| `claude` | `{"env": {"ANTHROPIC_BASE_URL", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_MODEL"}}` |
| `codex`, `opencode`, `droid` | `{"baseUrl", "apiKey", "model"}` |
| `copilot` | `{"baseUrl", "apiKey", "models": [model]}` |
| `hermes`, `cowork`, `openclaw` | `{"baseUrl", "apiKey", "model", "models": [model]}` |

Preview without writing: `--dry-run` prints `{path, body}` and sends nothing. Revert with `openproxy tool revert <name>`.

### 5b. REST path (every writable tool, exact bodies)

**Claude Code** — the server appends `/v1` to `ANTHROPIC_BASE_URL` and writes `~/.claude/settings.json`:

```bash
curl -sS -X POST "$OP/api/cli-tools/claude-settings" -H "$AUTH" \
  -H 'content-type: application/json' -d '{
    "env": {
      "ANTHROPIC_BASE_URL": "http://127.0.0.1:4623",
      "ANTHROPIC_AUTH_TOKEN": "'"$OPENPROXY_API_KEY"'",
      "ANTHROPIC_MODEL": "cc/claude-sonnet-5",
      "ANTHROPIC_DEFAULT_SONNET_MODEL": "cc/claude-sonnet-5",
      "ANTHROPIC_DEFAULT_OPUS_MODEL": "cc/claude-opus-5",
      "ANTHROPIC_DEFAULT_HAIKU_MODEL": "cc/claude-haiku-4-5-20251001"
    },
    "auto_compact_window": ""
  }'
```

Add `"auto_compact_window": "<token count>"` to set `CLAUDE_CODE_AUTO_COMPACT_WINDOW`; omit it (or send `""`) and the key is removed so Claude Code derives the window from the model.

**Codex CLI** — note the server **strips** `/v1` before writing `baseUrl`, defaults `defaultModel` to `cx/gpt-5.2-codex` when you send an empty one, chmods the file `0600`, and upserts `export OPENAI_BASE_URL="…"` between `# OpenProxy Codex settings` markers in your shell profiles:

```bash
curl -sS -X POST "$OP/api/cli-tools/codex-settings" -H "$AUTH" \
  -H 'content-type: application/json' -d '{
    "baseUrl": "http://127.0.0.1:4623",
    "apiKey": "'"$OPENPROXY_API_KEY"'",
    "defaultModel": "cx/gpt-5.2-codex"
  }'
```

**OpenCode** — writes a `provider.openproxy` block (`@ai-sdk/openai-compatible`) into `opencode.jsonc`, sets top-level `model` to `openproxy/<activeModel>`, and points the built-in `explorer` subagent at `openproxy/<subagentModel>`. Comments in the JSONC are preserved; the key defaults to `sk_openproxy` when you send an empty one:

```bash
curl -sS -X POST "$OP/api/cli-tools/opencode-settings" -H "$AUTH" \
  -H 'content-type: application/json' -d '{
    "baseUrl": "http://127.0.0.1:4623",
    "apiKey": "'"$OPENPROXY_API_KEY"'",
    "models": ["cc/claude-sonnet-5", "cx/gpt-5.2-codex"],
    "activeModel": "cc/claude-sonnet-5",
    "subagentModel": "cx/gpt-5.2-codex"
  }'

# Clear just the active model selection:  PATCH {"clearActiveModel": true}
# Remove a single model instead of all:  DELETE /api/cli-tools/opencode-settings?model=cc/claude-sonnet-5
```

**Copilot** — `models` is a required array:

```bash
curl -sS -X POST "$OP/api/cli-tools/copilot-settings" -H "$AUTH" \
  -H 'content-type: application/json' -d '{
    "baseUrl": "http://127.0.0.1:4623",
    "apiKey": "'"$OPENPROXY_API_KEY"'",
    "models": ["gpt-5-mini", "claude-haiku-4.5"]
  }'
```

**Factory Droid** — `~/.factory/settings.json`; `activeModel` reorders the generated `customModels` array and is inserted at index 0:

```bash
curl -sS -X POST "$OP/api/cli-tools/droid-settings" -H "$AUTH" \
  -H 'content-type: application/json' -d '{
    "baseUrl": "http://127.0.0.1:4623",
    "apiKey": "'"$OPENPROXY_API_KEY"'",
    "models": ["cc/claude-sonnet-5"],
    "activeModel": "cc/claude-sonnet-5"
  }'
```

**OpenClaw** — `model` is required; `agentModels` maps subagent name → model id:

```bash
curl -sS -X POST "$OP/api/cli-tools/openclaw-settings" -H "$AUTH" \
  -H 'content-type: application/json' -d '{
    "baseUrl": "http://127.0.0.1:4623",
    "apiKey": "'"$OPENPROXY_API_KEY"'",
    "model": "cc/claude-sonnet-5",
    "agentModels": {"explorer": "cx/gpt-5.2-codex"}
  }'
```

**Cline / Kilo Code** — identical three-field body; each writes its own store:

```bash
for tool in cline kilo; do
  curl -sS -X POST "$OP/api/cli-tools/$tool-settings" -H "$AUTH" \
    -H 'content-type: application/json' -d '{
      "baseUrl": "http://127.0.0.1:4623",
      "apiKey": "'"$OPENPROXY_API_KEY"'",
      "model": "cc/claude-sonnet-5"
    }'
done
```

**DeepSeek TUI / jcode** — `{baseUrl, apiKey, model}`; jcode also takes `models: []` and writes an env file next to its config:

```bash
curl -sS -X POST "$OP/api/cli-tools/deepseek-tui-settings" -H "$AUTH" \
  -H 'content-type: application/json' -d '{
    "baseUrl":"http://127.0.0.1:4623","apiKey":"'"$OPENPROXY_API_KEY"'","model":"cc/claude-sonnet-5"}'

curl -sS -X POST "$OP/api/cli-tools/jcode-settings" -H "$AUTH" \
  -H 'content-type: application/json' -d '{
    "baseUrl":"http://127.0.0.1:4623","apiKey":"'"$OPENPROXY_API_KEY"'",
    "models":["cc/claude-opus-5","cx/gpt-5.5"]}'
```

**Grok Build** — `~/.grok/config.toml` gets a `[model.openproxy]` entry set as default. Optional `contextWindow` (number) and `subagentModels` (object):

```bash
curl -sS -X POST "$OP/api/cli-tools/grok-build-settings" -H "$AUTH" \
  -H 'content-type: application/json' -d '{
    "baseUrl":"http://127.0.0.1:4623","apiKey":"'"$OPENPROXY_API_KEY"'",
    "model":"gcli/grok-build","contextWindow":131072}'
```

After applying, start `grok` and pick the model with `/model openproxy`; `/model grok-build` returns to xAI's own endpoint. `grok --model provider/model-id` also works.

**Hermes** — all fields optional (`~/.hermes/config.yaml` + `~/.hermes/.env`):

```bash
curl -sS -X POST "$OP/api/cli-tools/hermes-settings" -H "$AUTH" \
  -H 'content-type: application/json' \
  -d '{"baseUrl":"http://127.0.0.1:4623","apiKey":"'"$OPENPROXY_API_KEY"'","model":"cc/claude-sonnet-5"}'
```

**Claude Cowork** — loopback-only; every field optional (`baseUrl`, `apiKey`, `models`, `plugins`, `localPlugins`, `customPlugins`):

```bash
curl -sS -X POST "$OP/api/cli-tools/cowork-settings" -H "$AUTH" \
  -H 'content-type: application/json' \
  -d '{"baseUrl":"http://127.0.0.1:4623","apiKey":"'"$OPENPROXY_API_KEY"'"}'
```

### 5c. Tools with no server-side writer

**Continue** — `/api/cli-tools/continue-settings` is not mounted, so edit `~/.continue/config.json` directly and append an entry to `models[]`:

```json
{
  "models": [
    {
      "title": "cc/claude-sonnet-5",
      "provider": "openai",
      "model": "cc/claude-sonnet-5",
      "apiKey": "<management or proxy key>",
      "apiBase": "http://127.0.0.1:4623/v1"
    }
  ]
}
```

**Roo** — Roo Settings panel → API Provider → OpenAI Compatible, base URL `http://127.0.0.1:4623/v1`, then the model id from §3. Store lives at `~/.RooVeterinaryInc.roo-cline/data/`.

**Cursor** — Cursor routes through its own servers, so `127.0.0.1` will not work. Enable the tunnel or cloud endpoint in OpenProxy Settings, use that public URL as the OpenAI base URL in Cursor Settings → Models → "OpenAI API key" → View All Model → Add Custom Model.

**Devin CLI** — install from `cli.devin.ai`, run `devin auth login`, then pick any Devin CLI model under the Providers tab. No API key field; the provider spawns `devin acp` locally. `--agent-type <T>` is appended only when the `CLI_DEVIN_AGENT_TYPE` env var is set and non-empty — it is not passed by default.

**Amp CLI / Qwen Code** — environment variables only, no OpenProxy route:

```bash
export OPENAI_API_KEY="$OPENPROXY_API_KEY"
export OPENAI_BASE_URL="http://127.0.0.1:4623/v1"
amp --model 'gemini/gemini-2.5-pro'
```

**OpenDesign** — a plugin/skills pack that inherits the host agent's model config. Once Claude Code or OpenCode points at OpenProxy, `/opendesign` traffic follows with no extra env.

## 6 · Which model should each tool use?

Short answer: any id from `/v1/models`, and a combo if you want fallback. Longer answer — pick a cheap-but-competent model for the interactive lane and a stronger one for the subagent/large-context lane, and let a combo absorb the failover:

| Use | What to set | Why |
|---|---|---|
| Interactive lane (`model` / `activeModel` / `ANTHROPIC_MODEL`) | a combo, or your strongest mid-tier model | Latency is felt on every keystroke-to-token round trip. |
| Subagent lane (`subagentModel`, `agentModels`, `ANTHROPIC_DEFAULT_HAIKU_MODEL`) | a fast, cheap model | Explorer/fable/summarizer traffic is high-volume and latency-tolerant. |
| Long-context work | the model with the real context window | `maxOutputTokens` is written as `131072` for droid; check the model's own limit before assuming. |
| Anything you'd rather not route | leave the tool unwired | a tool with no config change keeps its vendor endpoint. |

Claude Code reads five model env keys — `ANTHROPIC_MODEL` plus `ANTHROPIC_DEFAULT_{OPUS,SONNET,FABLE,HAIKU}_MODEL` — and the dashboard presets map them to `cc/claude-opus-5`, `cc/claude-sonnet-5`, `cc/claude-fable-5`, `cc/claude-haiku-4-5-20251001`. Set all five explicitly; a partial env makes Claude Code fall back to vendor defaults for the unset tier.

## 7 · Verify the tool is really talking to the proxy

Three checks, cheapest first. **A green `hasOpenProxy` is not sufficient** — it only proves the server can find its marker in the file.

```bash
# 1. The config the server manages
curl -sS "$OP/api/cli-tools/opencode-settings" -H "$AUTH" \
  | jq '{installed, hasOpenProxy, configPath, opencode: {activeModel: .opencode.activeModel, baseURL: .opencode.baseURL, models: .opencode.models}}'

# 2. A completion through the exact model id you configured
curl -sS "$OP/v1/chat/completions" -H "$AUTH" -H 'content-type: application/json' \
  -d '{"model":"cc/claude-sonnet-5","messages":[{"role":"user","content":"ping"}]}' | jq '{model, text: .choices[0].message.content}'
```

3. The one that proves it: run the tool, then read the request log.

```bash
curl -sS "$OP/api/usage/logs" -H "$AUTH" | jq -r '.[:5][]'
# → "27-09-2026 13:20:57 | mimo-v2.5-free | OPENCODE | opencode-free | 38 | 3 | -"
#    date-time | model | provider | connection | prompt tokens | completion tokens | cost
```

Newest first, capped at 200 entries, one formatted line each (no query params, no pagination). A line whose `model` is the id you configured and whose `provider` is the connection that served it is the proof the tool's traffic reached the proxy. `/api/usage/request-logs` is an alias for the same handler.

If nothing new appeared, the tool is still going to its vendor — check that you restarted it after applying (Claude Code and Codex read config once at startup) and that the file the tool reads is the one the server wrote.

Claude Code is the one tool worth testing on a second route: `POST /v1/messages` is the Anthropic-native path, and `POST /v1/messages/count_tokens` must work too or context accounting breaks:

```bash
curl -sS "$OP/v1/messages" -H "$AUTH" -H 'content-type: application/json' \
  -H 'anthropic-version: 2023-06-01' \
  -d '{"model":"cc/claude-sonnet-5","max_tokens":32,"messages":[{"role":"user","content":"ping"}]}'
```

## 8 · Revert

```bash
openproxy tool revert opencode                  # DELETE /api/cli-tools/opencode-settings
curl -sS -X DELETE "$OP/api/cli-tools/claude-settings" -H "$AUTH"
```

Reverts are scoped: they remove OpenProxy's own entries and leave everything else in the file alone. Codex's revert also strips the `# OpenProxy Codex settings` block from your shell profiles. OpenCode's `DELETE` accepts `?model=<id>` to drop one model instead of all of them.

## 9 · Running built-in tools

```bash
curl -sS -X POST "$OP/api/cli-tools/run/key-list" -H "$AUTH" \
  -H 'content-type: application/json' -d '{}'
```

Supported: `provider-list`, `key-list`, `pool-list`, `pool-status <name>`. An unknown name returns `400` with the supported list — it is **not** exec'd. `/api/cli-tools/execute` is the deliberate path for an arbitrary command, and it clamps `timeoutSecs` to 120.

## 10 · Failure modes

| Symptom | Cause | Fix |
|---|---|---|
| `404` on `/api/cli-tools/continue-settings`, `/roo-settings`, or `/cursor-settings` | Those modules are never merged into `cli_tools::routes()` — see §2 | Wire the tool by hand; see §5c. |
| `404` from `openproxy tool apply continue` | `continue` is in `SUPPORTED_TOOLS` but its route is not mounted | Edit `~/.continue/config.json` by hand (§5c). |
| `401` on any `/api/cli-tools/*` | Sent a *proxy* key instead of the management key | `Authorization` on `/api/*` needs the admin/management key. |
| `401` only from `curl`, dashboard works | You sent a malformed key header, so the session fallback was skipped | Drop the header and use the cookie jar, or fix the key. |
| Codex still hits OpenAI after apply | The shell profile export needs a new shell, and Codex caches config at startup | `source ~/.zshrc`, restart the terminal and Codex. |
| Claude Code 401s but the model list works | Only `ANTHROPIC_BASE_URL` was set | Also set `ANTHROPIC_AUTH_TOKEN` (or `ANTHROPIC_API_KEY`). |
| Tool unchanged, `hasOpenProxy` still true | Stale marker from an earlier apply on a stale base URL | Re-apply with the current `--endpoint`, then check `/api/usage/logs` per §7. |
| `run/route` returns `400 unknown tool 'route'` | `route` is advertised by `GET /api/cli-tools` but is not in `build_tool_command` | Use `provider-list` / `key-list` / `pool-list` / `pool-status` instead. |
| `openproxy tool apply cline` → `usage` error | cline/kilo are not in `SUPPORTED_TOOLS` (only the nine in §5a are) | Use the REST body in §5b. |
| `403 {"error":"Localhost only"}` on `/api/cli-tools/cowork-settings` | Cowork is wrapped in `require_local_only` | Run the curl on the host serving OpenProxy. |
| `hasOpenProxy: true` but nothing new in `/api/usage/logs` | The tool never restarted | Restart the tool; config is read once at startup. |
| Cursor cannot reach the proxy | Cursor proxies through its own server | Use a tunnel/cloud endpoint URL, not `127.0.0.1`. |

## 11 · When _not_ to use this skill

- The server isn't running or the management key isn't captured — go back to the [entry skill](../openproxy/SKILL.md) §2–§3.
- You need to debug *fallback behaviour* between providers inside a combo — use `.agents/skills/openproxy-combos/SKILL.md` after the tool is wired.
- The question is about calling `/v1/chat/completions` directly from your own code, not about a third-party CLI's config file — use the chat skill.

## See also

- Entry skill: [`.agents/skills/openproxy/SKILL.md`](../openproxy/SKILL.md)
- Route source: `src/server/api/cli_tools.rs` (`routes()`, line ~3013) and `src/server/api/cli_tools/*.rs`
- CLI source: `src/cli/tool.rs` (`SUPPORTED_TOOLS`, `build_apply_body`)
- Dashboard: `http://127.0.0.1:4623/dashboard/cli-tools/<tool-id>` — e.g. `/dashboard/cli-tools/opencode`
- Tool registry the dashboard reads: `web/src/shared/constants/cliTools.ts`
