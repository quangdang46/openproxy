<div align="center">
  <img src="./images/openproxy.png?1" alt="OpenProxy Dashboard" width="800"/>

  # OpenProxy

  Single-binary AI router for AI coding tools. Embedded web dashboard, OpenAI-compatible API, auto-fallback across 40+ providers, token compression via RTK.

  [![npm](https://img.shields.io/npm/v/@openprx/openproxy.svg)](https://www.npmjs.com/package/@openprx/openproxy)
  [![License](https://img.shields.io/npm/l/@openprx/openproxy.svg)](https://github.com/quangdang46/openproxy/blob/main/LICENSE)
</div>

---

## What it does

OpenProxy runs as one binary on `127.0.0.1:4623`. Point any tool that speaks the OpenAI Chat Completions API at it (Claude Code, Codex, Cursor, Cline, OpenClaw, Copilot, ...) and OpenProxy:

- routes the request to a provider you've configured (OAuth, API key, or free)
- falls back to the next provider in your combo when one is rate-limited or errors
- compresses tool-call results via [RTK](https://github.com/rtk-ai/rtk) before they hit the LLM (typical −20–40% input tokens on tool-heavy turns)
- tracks per-account quota so you can use subscription tiers fully before paying for API calls
- serves a local dashboard at `/` for configuration, monitoring, and account management

There is no cloud component required. All state lives in `~/.openproxy/db.json`.

---

## Install

```bash
# Linux / macOS — x86_64 + aarch64
curl -fsSL "https://raw.githubusercontent.com/quangdang46/openproxy/main/install.sh" | bash

# Or via npm (any platform with Node 18+)
npm install -g @openprx/openproxy
```

Both pull the same prebuilt binary from the same GitHub release. The curl path drops the binary at `~/.local/bin/openproxy`. The npm path uses `optionalDependencies` to install only the platform binary that matches your machine.

```bash
openproxy
```

The server binds to `127.0.0.1:4623` and the dashboard auto-opens in your browser. Use `--no-open` for headless / SSH / container contexts.

<details>
<summary>Other install options</summary>

```bash
# Pin a version
curl -fsSL ".../install.sh" | bash -s -- --version v0.1.0

# Install system-wide (may need sudo)
curl -fsSL ".../install.sh" | bash -s -- --system

# Add to PATH automatically (~/.bashrc / ~/.zshrc)
curl -fsSL ".../install.sh" | bash -s -- --easy-mode

# Build from source (requires cargo + Node 20 + pnpm)
curl -fsSL ".../install.sh" | bash -s -- --from-source

# Uninstall
curl -fsSL ".../install.sh" | bash -s -- --uninstall
```

Manual download: https://github.com/quangdang46/openproxy/releases

</details>

---

## Connect a CLI tool

Most tools ask for an OpenAI base URL and an API key.

| Tool | Setting | Value |
|---|---|---|
| Cursor / Cline / Continue / Roo / Kilo | OpenAI base URL | `http://127.0.0.1:4623/v1` |
| Codex CLI | `OPENAI_BASE_URL` | `http://127.0.0.1:4623` |
| Claude Code | `~/.claude/config.json` `anthropic_api_base` | `http://127.0.0.1:4623/v1` |
| OpenClaw | dashboard → CLI Tools → OpenClaw | one-click apply |

The API key comes from the dashboard. Visit `http://127.0.0.1:4623`, create an API key, paste it into the tool's settings.

Tested CLIs: **Claude Code, Codex, Cursor, Cline, Continue, Roo, Kilo, Copilot, OpenClaw, OpenCode, Antigravity, Droid**.

---

## Supported providers

| Tier | Provider | Auth | Notes |
|---|---|---|---|
| OAuth subscription | Claude Code, Codex, GitHub Copilot, Cursor, Antigravity | OAuth (PKCE) | Use your existing subscription quota. Auto-refresh. |
| API key | OpenAI, Anthropic, Gemini, OpenRouter, GLM, Kimi, MiniMax, DeepSeek, Groq, xAI, Mistral, Perplexity, Together, Fireworks, Cerebras, Cohere, NVIDIA, SiliconFlow, Nebius, Chutes, Hyperbolic, custom OpenAI/Anthropic-compatible endpoints | API key | 40+ supported. |
| Free | Kiro AI (Claude 4.5 + GLM-5 + MiniMax), OpenCode Free, Vertex AI ($300 trial credits) | OAuth / no auth / GCP service account | Best for fallback tiers. |

Configure providers from the dashboard (`Providers` tab) or via `openproxy provider` CLI subcommands. Each provider supports multiple accounts; OpenProxy round-robins between them.

---

## Combos: build a fallback chain

A combo is an ordered list of models. OpenProxy tries them in order, falling back when one is rate-limited or errors. Models are addressed as `<provider-prefix>/<model-id>`:

```
combo: my-stack
  1. cc/claude-opus-4-7      # Claude Pro/Max subscription
  2. glm/glm-4.7             # paid backup ($0.6/1M)
  3. kr/claude-sonnet-4.5    # Kiro free fallback
```

Created from `Combos` in the dashboard or `openproxy combo create`. Use the combo name as the model field in your CLI tool — OpenProxy resolves it.

---

## Configuration

Most operators only set `JWT_SECRET` and `INITIAL_PASSWORD` and leave the rest at defaults.

| Variable | Default | Purpose |
|---|---|---|
| `JWT_SECRET` | `openproxy-default-secret-change-me` | Sign the dashboard session cookie. **Change in production.** |
| `INITIAL_PASSWORD` | `123456` | First-login password (one-time, replaced on first save). |
| `DATA_DIR` | `~/.openproxy` | Where `db.json`, `usage.json`, `log.txt` live. |
| `PORT` | `4623` | HTTP listen port. |
| `HOSTNAME` | `127.0.0.1` | Bind host. Set `0.0.0.0` to expose on LAN. |
| `BASE_URL` | `http://localhost:4623` | Internal base URL for cloud-sync jobs. |
| `CLOUD_URL` | _unset_ | Cloud-sync endpoint. Leave unset to disable cloud sync. |
| `API_KEY_SECRET` | `endpoint-proxy-api-key-secret` | HMAC secret for generated API keys. |
| `MACHINE_ID_SALT` | `endpoint-proxy-salt` | Salt for the stable machine-ID hash. |
| `AUTH_COOKIE_SECURE` | `false` | Force `Secure` flag on the auth cookie. Set `true` behind HTTPS. |
| `REQUIRE_API_KEY` | `false` | Reject `/v1/*` requests without `Authorization: Bearer …`. Recommended for any internet-exposed deploy. |
| `ENABLE_REQUEST_LOGS` | `false` | Write per-request logs under `logs/`. |
| `HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY` | _unset_ | Forward outbound provider calls through an HTTP proxy. Lowercase variants also honored. |

---

## CLI reference

```
openproxy [FLAGS]                  # default: start server + open browser
openproxy --port 4623 --no-open    # foreground, no browser
openproxy --web-dir ./web/dist     # serve dashboard from disk (UI dev)
openproxy --dashboard-sidecar-url http://127.0.0.1:4624
                                   # reverse-proxy dashboard requests to a dev server

openproxy provider list
openproxy provider add <name> <type>
openproxy combo create <name> --models cc/opus,glm/glm-5
openproxy key list
openproxy key add <name>
openproxy quota
openproxy usage
openproxy doctor                   # diagnose common config issues

openproxy server start [--detach] [--port P]
openproxy server status
openproxy server stop
```

`openproxy --help` prints the full reference. Subcommands have their own `--help`.

Output formats: human (default), `--robot` (line-delimited JSON for agent/automation use), `--quiet`.

---

## API

OpenAI-compatible chat completions:

```http
POST /v1/chat/completions
Authorization: Bearer <api-key>
Content-Type: application/json

{
  "model": "cc/claude-opus-4-6",
  "messages": [{"role": "user", "content": "..."}],
  "stream": true
}
```

List available models and combos:

```http
GET /v1/models
Authorization: Bearer <api-key>
```

Health probe (no auth):

```http
GET /health   →   200 OK
```

The dashboard at `/` is the same authenticated API surface in HTML form. Admin endpoints live under `/api/*` and use the dashboard session cookie.

---

## Architecture

```
┌─────────────────────────────────────────────┐
│ openproxy  (single binary, port 4623)      │
│                                             │
│  /            embedded web dashboard       │
│               (Astro static via rust-embed)│
│                                             │
│  /v1/*        OpenAI-compatible API        │
│  /api/*       admin / dashboard data       │
│  /codex/*     Codex OAuth helper           │
│                                             │
│  RTK token compression  ─┐                  │
│  format translation     ─┤                  │
│  quota tracking         ─┼─→ provider HTTP │
│  account fallback       ─┘                  │
└─────────────────────────────────────────────┘
                                                  │
                                                  ↓
                          [ provider APIs: Anthropic, OpenAI, GLM, ... ]
```

Stack: Rust 1.76+, axum 0.8, hyper 1, rusqlite (bundled), Astro 4 (static, embedded), React 19, Tailwind. Storage: `db.json` + SQLite.

---

## Build from source

Requires Node ≥ 20.3 and `pnpm` (`corepack enable && corepack prepare pnpm@10.33.2 --activate`, or `npm i -g pnpm`).

```bash
git clone https://github.com/quangdang46/openproxy.git
cd openproxy

pnpm --dir web install
pnpm --dir web run build

cargo build --release --locked
./target/release/openproxy
```

UI iteration without rebuilding the binary:

```bash
pnpm --dir web run build
cargo run -- --web-dir ./web/dist
```

UI live-reload via the Astro dev server:

```bash
# Terminal 1
pnpm --dir web run dev   # → http://127.0.0.1:4624

# Terminal 2
cargo run -- --dashboard-sidecar-url http://127.0.0.1:4624
```

Headless build (no embedded dashboard, smaller binary):

```bash
cargo build --release --locked --no-default-features
# Requires --web-dir or --dashboard-sidecar-url at runtime.
```

---

## Deployment

### Docker

```bash
docker build -t openproxy .
docker run -d \
  --name openproxy \
  -p 4623:4623 \
  --env-file ./.env \
  -v openproxy-data:/app/data \
  -v openproxy-usage:/root/.openproxy \
  openproxy
```

Container defaults: `PORT=4623`, `HOSTNAME=0.0.0.0`. Mount a writable volume at `/app/data` for persistence.

### Behind a reverse proxy

For internet-exposed deploys: set `REQUIRE_API_KEY=true`, `AUTH_COOKIE_SECURE=true`, terminate TLS at the proxy, and forward only `/v1/*` if you don't need the dashboard accessible publicly.

---

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `EADDRINUSE` on `4623` | Port in use | `openproxy --port 4624` or `openproxy server stop` |
| Dashboard shows blank page | Embedded asset not hashed correctly | Hard reload (`Ctrl+Shift+R`); check `/health` returns 200 |
| OAuth "callback failed" | Browser blocked the redirect | Retry from the dashboard's `Providers → Reconnect` |
| 401 on `/v1/chat/completions` | Wrong API key | Copy fresh from dashboard. Header: `Authorization: Bearer <key>` |
| Quota exhausted message | Subscription / API limit hit | Combo fallback handles this — add a cheaper or free tier as the next entry |
| `cargo build` fails with "web/dist not built" | Embedded build needs the dashboard | `(cd web && pnpm install --frozen-lockfile && pnpm run build)` first |
| First login password rejected | `INITIAL_PASSWORD` not what you set | Default is `123456` if unset; check `.env` is sourced |

Logs: enable with `ENABLE_REQUEST_LOGS=true`, then watch `logs/` (or stderr).

---

## Acknowledgments

Built on the work of others:

- **CLIProxyAPI** — Go implementation that inspired the architecture.
- **[RTK](https://github.com/rtk-ai/rtk)** — token compression pipeline. OpenProxy's `tool_result` compression is a port.
- **[Caveman](https://github.com/JuliusBrussee/caveman)** — caveman-speak prompt that trims output tokens by reframing the system instruction.

---

## License

MIT — see [LICENSE](LICENSE).
