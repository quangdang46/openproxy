---
name: openproxy
description: Install, initialize, and operate OpenProxy from the CLI — either guiding a human through setup or driving it fully autonomously as an agent. Use whenever the user asks to install the openproxy binary, start the local AI router on 127.0.0.1:4623, configure providers / combos / keys, or wire an AI coding CLI (Claude Code, Codex, Cursor, Cline, OpenClaw, Copilot, …) into OpenProxy.
---

# openproxy — install & operate from the CLI

[OpenProxy](https://github.com/quangdang46/openproxy) is a single-binary AI router that exposes an OpenAI-compatible API on `127.0.0.1:4623` and fans out to 40+ providers with auto-fallback. This skill walks an agent through:

1. Installing or upgrading the `openproxy` binary
2. Initializing a data dir and capturing the admin API key
3. Starting the server **detached + headless** and verifying it
4. Configuring providers / keys / pools declaratively via `apply`
5. Wiring an AI coding CLI into the proxy

Every step is non-interactive and safe to run unattended.

## 0 · Detect what's already installed

Always inspect state before mutating anything:

```bash
command -v openproxy && openproxy --version || echo "not installed"
test -f "$HOME/.openproxy/db.json" && echo "data dir already provisioned" || true
```

If a binary is already on PATH, jump to step 2. Run `openproxy doctor` early — it catches half-installed states.

## 1 · Install the binary

Two equivalent install paths. Pick whichever is least invasive in the current environment.

### 1a. One-shot curl installer (recommended for Linux / macOS, x86_64 + aarch64)

```bash
curl -fsSL "https://raw.githubusercontent.com/quangdang46/openproxy/main/install.sh" | bash
export PATH="$HOME/.local/bin:$PATH"
```

Drops the binary at `~/.local/bin/openproxy`. Idempotent (locked via `/tmp/openproxy-install.lock.d`). Useful flags:

| Flag | Effect |
|---|---|
| `--version vX.Y.Z` | Pin a specific release (default: latest from GitHub Releases). |
| `--dest <path>` | Install to a custom directory. |
| `--system` | Install to `/usr/local/bin` (may need sudo). |
| `--easy-mode` | Append `PATH` export to `~/.bashrc` and `~/.zshrc`. |
| `--from-source` | Build from source via cargo (needs Rust ≥ 1.95 + Node 20 + pnpm). |
| `--verify` | Run `openproxy --version` after install. |
| `--uninstall` | Remove binary and any easy-mode PATH lines. |
| `--quiet` / `-q` | Suppress info logs. |

Pass flags after `--`, for example:

```bash
curl -fsSL "https://raw.githubusercontent.com/quangdang46/openproxy/main/install.sh" \
  | bash -s -- --version v0.1.0 --easy-mode --verify
```

### 1b. Windows (PowerShell 5.1+, x86_64)

```powershell
irm "https://raw.githubusercontent.com/quangdang46/openproxy/main/install.ps1" | iex
```

Drops `openproxy.exe` at `$env:USERPROFILE\.local\bin`. Pass flags by downloading first:

```powershell
irm "https://raw.githubusercontent.com/quangdang46/openproxy/main/install.ps1" -OutFile install.ps1
.\install.ps1 -Version v0.1.7 -EasyMode -Verify
```

`-EasyMode` appends the install dir to the *user* PATH; open a new shell for the change to take effect.

### 1c. Other / unsupported platforms

- Alpine / musl outside of `linux-x86_64` and `linux-aarch64`: build from source (`--from-source` flag on `install.sh`).
- Windows on ARM64: not yet published — build from source or open an issue.
- Anything else: clone the repo and run `cargo build --release --locked`.

### Verify

```bash
openproxy --version
openproxy doctor
```

Both should succeed before continuing.

## 2 · Initialize a data dir + capture the admin API key

`openproxy server init` creates an empty `db.json` at `$DATA_DIR` (default `~/.openproxy/`) and emits **one** fresh admin API key — shown exactly once.

In `--robot` mode the key is on `.data.admin_key.key` of the JSONL envelope:

```bash
openproxy --robot server init | tee /tmp/op-init.json
APIKEY=$(jq -r '.data.admin_key.key' /tmp/op-init.json)

mkdir -p "$HOME/.openproxy"
printf '%s\n' "$APIKEY" > "$HOME/.openproxy/admin.key"
chmod 600 "$HOME/.openproxy/admin.key"

# Export for subsequent commands in this session
export OPENPROXY_API_KEY="$APIKEY"
```

> Re-running `server init` against an existing data dir errors out (envelope kind `error`, reason `conflict`) unless `--force` is passed. **Do not force without asking the user** — it wipes the existing config.

If the data dir is already provisioned and the admin key is unknown, authenticate via password against the running server instead:

```bash
# Default INITIAL_PASSWORD is "123456" unless set in env at first boot.
curl -sS -X POST http://127.0.0.1:4623/api/auth/login \
  -H 'content-type: application/json' \
  -d '{"password":"123456"}' \
  -c /tmp/op.cookies
```

Then either mint a new admin key from the dashboard, or use the cookie jar (`-b /tmp/op.cookies`) for subsequent `/api/*` calls.

## 3 · Start the server (detached + headless)

```bash
openproxy server start --detach --no-open
openproxy --robot server status
openproxy --robot doctor
```

- `--detach` — daemonize. PID is recorded under `$DATA_DIR/server.pid`.
- `--no-open` — never spawn a browser. Required in SSH / container / CI / agent contexts.
- Server binds `127.0.0.1:4623` by default. Change with `--port N` or `PORT=N`.

To expose on LAN, set `HOSTNAME=0.0.0.0` **and** `REQUIRE_API_KEY=true` (otherwise `/v1/*` is unauthenticated). Never expose on a public interface without TLS termination at a reverse proxy.

Stop / restart:

```bash
openproxy server stop
openproxy server start --detach --no-open --port 4624   # alt port
```

## 4 · Configure providers + combos non-interactively

The CLI is self-documenting — discover the resource schemas before generating payloads:

```bash
openproxy schema list                  # available resource kinds
openproxy schema show provider         # JSON shape
openproxy schema example provider      # ready-to-edit example
```

`schema list` returns at least: `provider`, `provider-node`, `combo`, `key`, `pool`, `settings`, `custom-model`, `model-alias`, `usage-event`, `log-event`, `chat-event`, `quota`, `oauth-status`.

### Declarative `apply`

`apply --from-file` is idempotent. Pass `--prune` to delete resources not present in the file.

```bash
cat > /tmp/providers.json <<'JSON'
{
  "providers": [
    {
      "name": "openai-paid",
      "provider": "openai",
      "apiKey": "sk-...",
      "isActive": true
    },
    {
      "name": "anthropic-paid",
      "provider": "anthropic",
      "apiKey": "sk-ant-...",
      "isActive": true
    }
  ]
}
JSON

openproxy --robot provider apply --from-file /tmp/providers.json
# Or, with --prune to make the file authoritative:
# openproxy --robot provider apply --from-file /tmp/providers.json --prune
```

The same `apply --from-file` pattern works for `key apply` and `pool apply`. Read from stdin with `--from-file -`.

### Quick combo (imperative)

```bash
openproxy combo create my-stack \
  --models "openai/gpt-4o,anthropic/claude-3-5-sonnet"
```

Combo entries are `<provider-key>/<model-id>`. For a custom provider node, the prefix is the node's **UUID** (not its name) — see the model-resolution details in `.agents/skills/testing-combo-fallback/SKILL.md`.

### OAuth subscription providers (Claude Code, Codex, Copilot, Cursor, Antigravity)

OAuth providers (`openproxy provider oauth …`) require a one-time browser dance. In a headless / agent context, prefer:

- API-key providers wherever possible.
- If the user has a graphical session, direct them to the dashboard's **Providers → Reconnect** flow at `http://127.0.0.1:4623`.

## 5 · Wire a CLI tool into the proxy

Most AI CLIs accept an OpenAI-compatible base URL and a bearer token:

| Tool | Setting | Value |
|---|---|---|
| Cursor / Cline / Continue / Roo / Kilo | OpenAI base URL | `http://127.0.0.1:4623/v1` |
| Codex CLI | env `OPENAI_BASE_URL` | `http://127.0.0.1:4623` |
| Claude Code | `~/.claude/config.json` → `anthropic_api_base` | `http://127.0.0.1:4623/v1` |
| OpenClaw | dashboard → CLI Tools → OpenClaw | one-click apply |

The bearer is the admin key captured in step 2, or any key minted via `openproxy key add`. OpenProxy also has a `tool` subcommand (`openproxy tool …`) that can apply these settings programmatically — run `openproxy tool --help` to see the matrix of supported tools in the installed binary.

## 6 · Verifications

```bash
# Liveness (no auth required)
curl -sS http://127.0.0.1:4623/health

# Models list (auth required)
curl -sS http://127.0.0.1:4623/v1/models \
  -H "Authorization: Bearer $OPENPROXY_API_KEY"

# End-to-end chat completion through a combo
curl -sS http://127.0.0.1:4623/v1/chat/completions \
  -H "Authorization: Bearer $OPENPROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"my-stack","messages":[{"role":"user","content":"ping"}]}'
```

Stop after verification if you only needed a smoke test:

```bash
openproxy server stop
```

## 7 · Sync provider catalog from upstream routers (optional)

OpenProxy ships with an embedded snapshot of provider/model catalogs from
two sister open-source routers — [9router](https://github.com/decolua/9router)
and [OmniRoute](https://github.com/diegosouzapw/OmniRoute). The `sync`
subcommand applies those snapshots to the user's `db.json` so new models
land in `customModels` without manual edits.

```bash
# Preview what would change
openproxy --robot sync 9router --dry-run
openproxy --robot sync omniroute --dry-run

# Apply
openproxy sync 9router
openproxy sync omniroute

# Remove entries we previously synced that are no longer in the upstream
openproxy sync 9router --prune
```

Synced models are tagged in `customModels[].source` so a subsequent
`--prune` only touches entries this command put there — built-in models
and user-added customModels are never affected.

Maintainers refresh the embedded snapshots by running
`node scripts/sync/normalize-sources.mjs` (see `scripts/sync/README.md`).

## Common failure modes & fixes

| Symptom | Fix |
|---|---|
| `EADDRINUSE :4623` | `openproxy server stop` then restart, or `openproxy --port 4624 server start --detach`. |
| `401 on /v1/*` | Wrong bearer; re-issue with `openproxy key add` or fall back to the admin key from `server init`. |
| `db.json already exists at … (use --force to overwrite)` | Data dir is pre-populated. **Ask the user before passing `--force`** — it wipes existing config. |
| `installer: could not resolve latest version` | No GitHub Release tags published yet. Pass `--version vX.Y.Z` or `--from-source`. |
| `npm install -g @openprx/openproxy` → `E404` | npm publish path is currently disabled (registry stuck at 0.1.1). Use the curl installer (1a) on Linux/macOS or the PowerShell installer (1b) on Windows. |
| `unsupported platform "linux-musl"` | Alpine etc. — use `--from-source`. |
| `unsupported OS: …` from installer | Use the PowerShell installer (1b) on Windows or `--from-source`. |
| OAuth callback fails in headless env | Use API-key providers, or open the dashboard from a graphical session. |
| `openproxy doctor` reports `server not running` | `openproxy server start --detach --no-open`. |
| Dashboard blank / stale assets | Hard reload (`Ctrl+Shift+R`). Check `/health` returns 200. |

## Environment variables (most-used)

| Var | Default | Purpose |
|---|---|---|
| `OPENPROXY_API_KEY` | _(empty)_ | Bearer used for `--robot` CLI calls. Honor `--api-key` flag too. |
| `OPENPROXY_DATA_DIR` / `DATA_DIR` | `~/.openproxy` | Where `db.json`, `usage.json`, `log.txt` live. |
| `PORT` | `4623` | HTTP listen port. |
| `HOSTNAME` | `127.0.0.1` | Bind host. `0.0.0.0` exposes on LAN. |
| `INITIAL_PASSWORD` | `123456` | First-login password (replaced on first save). |
| `JWT_SECRET` | `openproxy-default-secret-change-me` | **Change in any non-throwaway deploy.** |
| `REQUIRE_API_KEY` | `false` | Reject `/v1/*` without a bearer. Required for any non-loopback bind. |
| `OPENPROXY_NO_OPEN` | _(unset)_ | Equivalent to `--no-open`. |
| `OPENPROXY_WEB_DIR` | _(unset)_ | Serve dashboard from a directory (UI dev). |

## When _not_ to use this skill

- The user has openproxy running and wants help debugging combo dispatch — use `.agents/skills/testing-combo-fallback/SKILL.md` instead.
- The user is asking about cloud-hosted multi-tenant OpenProxy — out of scope; this skill covers the local single-binary mode only.
- The user explicitly wants to `--from-source` build openproxy from scratch — follow the README's "Build from source" section; this skill optimizes for the prebuilt-binary path.

## See also

- Full README: https://github.com/quangdang46/openproxy
- CLI reference: `openproxy --help` and `openproxy <command> --help`
- Schema introspection: `openproxy schema list`
- Combo-fallback / E2E skill: [`.agents/skills/testing-combo-fallback/SKILL.md`](../testing-combo-fallback/SKILL.md)
