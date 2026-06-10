# Master Plan: OpenProxy = 9Router (Source of Truth Parity)

> **Status**: ACTIVE — 7-day rollout
> **Owner**: @quangdang46
> **Source of truth**: https://github.com/decolua/9router (commit pinned to 9router master HEAD at planning time)
> **Reference snapshot**: `src/core/model/sources/9router.json` (ref: `v0.4.66-1-gcce8a50`)

---

## 0. Nguyên tắc bất di bất dịch (Non-negotiable principles)

1. **9router is the absolute source of truth.** Every OAuth URL, clientId, scope, PKCE method, refresh URL, response header, error handling rule in 9router MUST be mirrored byte-for-byte in OpenProxy. There is no "we know better" exception.
2. **Port `open-sse/` JS modules → Rust modules 1:1.** Every constant, every function, every comment with non-obvious context (why a header is set, what the upstream returns, what error triggers fallback) MUST be preserved or translated faithfully. Use `// Port of <9router path>` comments.
3. **Two files must not disagree.** The bug we are fixing is that `src/oauth/mod.rs` and `src/server/api/oauth.rs` hold different URLs for the same provider. After this plan, there is exactly ONE source of truth per concern.
4. **Test against the real upstream where possible.** OAuth URL must be hit at least once with a real browser/codex-cli to confirm `200 OK` or expected error path. Don't trust the docs.
5. **CLI setup must produce files that the actual CLI accepts.** Run the real Claude Code / Cline / Cursor after `openproxy api cli-tools <id>-settings` and verify it boots, sends a request that lands in OpenProxy logs, and gets a 200 response back.

---

## 1. Executive summary

OpenProxy is a Rust port of 9router's architecture. After audit, we have **2 critical classes of bugs** that will break the user experience if not fixed:

### 1.1 Auth per model (the core, 17 providers)

Out of 17 OAuth-bearing providers, **5 are completely missing OAuth flows** (xai, gemini-cli, antigravity, openai-native, qoder) and **4 have wrong URLs/scopes/clientId** in the code (claude, codex, github, kiro). The Kiro upstream endpoint is the wrong host entirely. If a user tries to log into any of these, the OAuth dance fails.

### 1.2 Setup auth per CLI tool (the other core, 11+ CLIs)

5 of 11 supported CLIs have **no automated setup endpoint at all** (codex, continue, roo, droid, openclaw). The 6 that exist have correct skeleton but need verification against 9router's exact env-var names, settings-file paths, and OS-specific paths. Users currently have to read docs and edit files by hand.

### 1.3 Three smaller gaps

- **Caveman injection**: settings field exists, function not wired into chat pipeline.
- **Format::CommandCode**: executor exists, Format enum doesn't include it.
- **Token dedup**: 9router has 10s in-flight dedup to prevent Auth0-family `refresh_token_reused` revocation; we don't.

---

## 2. Architecture overview

```
                    ┌──────────────────────────────────────────┐
                    │       OpenProxy (Rust, port 4623)        │
                    │                                          │
  CLI request ────▶ │  /v1/{chat,messages,responses,...}       │
                    │        │                                 │
                    │        ├─▶ auth.rs (ApiKey guard)        │
                    │        ├─▶ chat.rs                       │
                    │        │     │                           │
                    │        │     ├─▶ RTK compression (rtk/)  │
                    │        │     ├─▶ inject_caveman ← PHASE 4│
                    │        │     ├─▶ translator (registry.rs)│
                    │        │     ├─▶ combo executor (combo/) │
                    │        │     │                           │
                    │        │     └─▶ executor (provider.rs)  │
                    │        │           │                     │
                    │        │           ├─▶ claude.rs ← PHASE 1+2
                    │        │           ├─▶ codex.rs ← PHASE 1
                    │        │           ├─▶ kiro.rs ← PHASE 2
                    │        │           ├─▶ xai.rs ← PHASE 2
                    │        │           ├─▶ antigravity.rs ← PHASE 2
                    │        │           ├─▶ gemini_cli.rs ← PHASE 2
                    │        │           ├─▶ qoder.rs ← PHASE 2
                    │        │           └─▶ ... (40+ API-key)  │
                    │        │                                 │
                    │        └─▶ usage stream + log            │
                    │                                          │
                    │  /api/cli-tools/{id}-settings ← PHASE 3  │
                    │  /api/cli/providers/{id}                 │
                    │                                          │
                    │  oauth/  ← PHASE 1 (single source)       │
                    │   ├─ providers.rs (constants)            │
                    │   ├─ claude.rs / codex.rs / kiro.rs /    │
                    │   │   xai.rs / gemini_cli.rs /          │
                    │   │   antigravity.rs / openai.rs /       │
                    │   │   qoder.rs / github.rs / ...         │
                    │   ├─ token_refresh.rs                    │
                    │   └─ pending.rs                          │
                    │                                          │
                    │  mitm/ (kiro/copilot/antigravity IDE)    │
                    └──────────────────────────────────────────┘
                                          │
                                          ▼
                          provider upstreams (real OAuth + API)
```

---

## 3. Auth per model — 17 providers (PHASE 1 + 2)

### 3.1 Master table (byte-for-byte vs 9router)

| # | Provider | Method | Authorize URL | Token URL | clientId | Scopes | Special | openproxy current | target file |
|---|---|---|---|---|---|---|---|---|---|
| 1 | `claude` | PKCE Auth Code, JSON body | `https://claude.ai/oauth/authorize` | `https://api.anthropic.com/v1/oauth/token` | `9d1c250a-e61b-44d9-88ed-5944d1962f5e` | `org:create_api_key user:profile user:inference` | extra `code: true`; parse `#state` from returned code | ❌ WRONG (`auth.claude.ai`) | `oauth/providers.rs` |
| 2 | `codex` | PKCE Auth Code, **fixedPort 1455**, callback `/auth/callback` | `https://auth.openai.com/oauth/authorize` | `https://auth.openai.com/oauth/token` | `app_EMoamEEZ73f0CkXaXp7hrann` | `openid profile email offline_access` | extraParams: `id_token_add_organizations=true, codex_cli_simplified_flow=true, originator=codex_cli_rs` | ❌ WRONG (`codex.ai`) | `oauth/providers.rs` |
| 3 | `github` | Device Code | `https://github.com/login/device/code` | `https://github.com/login/oauth/access_token` | `Iv1.b507a08c87ecfe98` | `read:user` (NOT `read:user repo`) | `User-Agent: GitHubCopilotChat/0.38.0`, `X-GitHub-Api-Version: 2025-04-01`; post-exchange call `copilot_internal/v2/token` | ❌ extra `repo` scope | `oauth/providers.rs` |
| 4 | `kiro` | **5 methods**: builder-id / idc / google / github / import | AWS SSO OIDC dynamic | AWS SSO OIDC dynamic + Cognito social | dynamic via `oidc.us-east-1.amazonaws.com/client/register` | `codewhisperer:completions codewhisperer:analysis codewhisperer:conversations` | `kiro://kiro.kiroAgent/authenticate-success` redirect (Cognito whitelist); refresh: AWS OIDC for AWS paths, `prod.us-east-1.auth.desktop.kiro.dev/refreshToken` for social | ❌ URL wrong, only 1 method | `oauth/providers.rs` + `oauth/kiro.rs` |
| 5 | `xai` | PKCE + **dynamic discovery** | discovered from `https://auth.x.ai/.well-known/openid-configuration` (validated only `x.ai`/`*.x.ai`) | same | `b1a00492-073a-47ea-816f-4c329264a828` | `openid profile email offline_access grok-cli:access api:access` | **fixed loopback port 56121**, callback `/callback`, **96-byte verifier** (not 32), extra: `nonce, plan=generic, referrer=cli-proxy-api`, form-urlencoded body | ❌ MISSING entirely | `oauth/xai.rs` + `executor/xai.rs` |
| 6 | `gemini-cli` | Google Auth Code + **postExchange loadCodeAssist** | `https://accounts.google.com/o/oauth2/v2/auth` | `https://oauth2.googleapis.com/token` | `681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com` | `cloud-platform userinfo.email userinfo.profile` | `access_type=offline, prompt=consent`; post-exchange fetch `cloudcode-pa.googleapis.com/v1internal:loadCodeAssist` (mode=1) → save `cloudaicompanionProject.id` | ❌ MISSING entirely | `oauth/gemini_cli.rs` |
| 7 | `antigravity` | Google Auth Code + **loadCodeAssist + onboardUser** | `https://accounts.google.com/o/oauth2/v2/auth` | `https://oauth2.googleapis.com/token` | `1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com` | `cloud-platform userinfo.email userinfo.profile cclog experimentsandconfigs` | Headers: `User-Agent: google-api-nodejs-client/9.15.1`, `X-Goog-Api-Client: google-cloud-sdk vscode_cloudshelleditor/0.1`, `Client-Metadata: {"ideType":9,"platform":<numeric>,"pluginType":2}`. `onboardUser` polls 5s × 10 retries | ❌ MISSING entirely | `oauth/antigravity.rs` + `executor/antigravity.rs` |
| 8 | `qwen` | Device Code + PKCE | `https://chat.qwen.ai/api/v1/oauth2/device/code` | `https://chat.qwen.ai/api/v1/oauth2/token` | `f0304373b74a44d2b584a3fb70ca9e56` | `openid profile email model.completion` | poll `authorization_pending`/`slow_down`; capture `resource_url` | ✅ verify exact | `oauth/providers.rs` |
| 9 | `iflow` | Auth Code + **Basic Auth** | `https://iflow.cn/oauth` | `https://iflow.cn/oauth/token` | `10009311001` | — | `Authorization: Basic base64(client:secret)`, query `loginMethod=phone type=phone`; `userInfoUrl: https://iflow.cn/api/oauth/getUserInfo?accessToken=...` returns `apiKey` to store | ✅ verify exact | `oauth/providers.rs` |
| 10 | `qoder` | Device Token + PKCE | `https://qoder.com/device/selectAccounts` | `https://openapi.qoder.sh/api/v1/deviceToken/poll` | (none — uses nonce+verifier+challenge) | — | 15s per-poll timeout, 202/404 = pending, 200 = token; tokens 30d; **refresh returns 403** → re-login required | ❌ MISSING entirely | `oauth/qoder.rs` + `executor/qoder.rs` |
| 11 | `openai` (native) | PKCE Auth Code | `https://auth.openai.com/oauth/authorize` | `https://auth.openai.com/oauth/token` | `app_EMoamEEZ73f0CkXaXp7hrann` | `openid profile email offline_access` | extraParams: `id_token_add_organizations=true, originator=openai_native` (NOT `codex_cli_rs`) | ❌ MISSING entirely | `oauth/openai.rs` |
| 12 | `kimi-coding` | Device Code | `https://api.moonshot.cn/kimi-device/oauth/device/code` | `https://api.moonshot.cn/kimi-device/oauth/token` | env `KIMI_CODING_OAUTH_CLIENT_ID` \|\| `17e5f671-d194-4dfb-9706-5516cb48c098` | `kimi:read` | Custom headers `X-Msh-Platform: 9router`, `X-Msh-Version: 2.1.2`, `X-Msh-Device-Model`, `X-Msh-Device-Id` | ✅ verify exact | `oauth/providers.rs` |
| 13 | `kilocode` | Device Code | `https://api.kilo.ai/api/device-auth/codes` (initiate AND poll same URL) | same | (none) | `read` | — | ✅ verify exact | `oauth/providers.rs` |
| 14 | `cline` | Auth Code via `app.cline.bot` | `https://api.cline.bot/api/v1/auth/authorize` | `https://api.cline.bot/api/v1/auth/token` | (custom) | — | refresh: `https://api.cline.bot/api/v1/auth/refresh` | ✅ verify exact | `oauth/providers.rs` |
| 15 | `gitlab` | PKCE Auth Code (self-hostable) | `https://gitlab.com/oauth/authorize` | `https://gitlab.com/oauth/token` | (user-provided) | `api read_user` | `gitlab_with_baseurl(base_url)` for self-host; PAT `glpat-...` ≥20 chars accepted | ✅ verify exact | `oauth/providers.rs` |
| 16 | `codebuddy` | Device Code | `https://copilot.tencent.com/v2/plugin/auth/state` | `https://copilot.tencent.com/v2/plugin/auth/token` | (custom) | `read` | `User-Agent: CLI/2.63.2 CodeBuddy/2.63.2`, `Platform: CLI`, poll 5s, refresh `https://copilot.tencent.com/v2/plugin/auth/token/refresh` | ✅ verify exact | `oauth/providers.rs` |
| 17 | `cursor` | **Import Token only** (no OAuth) | — | — | — | — | SQLite path varies by OS. Key: `cursorAuth/accessToken` + `storage.serviceMachineId`. **Checksum jyh cipher**: XOR timestamp bytes with rolling key (init=165), base64 → `{encoded_ts},{machineId}`. Headers: `x-cursor-checksum`, `x-cursor-client-version: 3.1.0`, `x-cursor-client-type: ide`, `x-cursor-client-os`, `x-cursor-client-arch`, `x-cursor-client-device-type: desktop`, `x-ghost-mode`. Token TTL 24h, no refresh — user re-imports | ✅ verify exact | `oauth/cursor.rs` + `executor/cursor.rs` |

### 3.2 Refresh logic per provider (PHASE 1.4)

```
codex:         5 * 24 * 60 * 60 * 1000   (5 days, from OAUTH_ENDPOINTS.openai.token)
claude:        4 * 60 * 60 * 1000         (4 hours, from api.anthropic.com/v1/oauth/token)
iflow:         24 * 60 * 60 * 1000        (24 hours, from iflow.cn/oauth/token)
qwen:          20 * 60 * 1000              (20 min, from chat.qwen.ai/api/v1/oauth2/token)
kimi-coding:   5 * 60 * 1000               (5 min)
antigravity:   5 * 60 * 1000               (5 min)
github:        copilotToken separate TTL  → poll /copilot_internal/v2/token when expires within 5min
xai:           5 * 60 * 1000               (5 min, XAI_REFRESH_LEAD_SECONDS = 300)
kiro:          dynamic — AWS OIDC vs Cognito refreshToken URL
cursor:        no refresh — manual re-import after 24h
qoder:         no refresh — server returns 403
gemini-cli:    expiresIn * 1000
openai-native: same as codex (5 days)
cline:         expiresIn from response
gitlab:        expiresIn from response
codebuddy:     expiresIn from response
kilocode:      expiresIn from response
```

**Special refresh cases**:

1. **Claude refresh body** is **JSON** (not form): `{"grant_type":"refresh_token","refresh_token":...,"client_id":"9d1c250a-..."}`. `Content-Type: application/json`. We currently have form-urlencoded in `oauth/mod.rs:fetch` — WRONG.
2. **GitHub** has no `refresh_token` in device-flow response. So we cache `copilotToken` + its `expiresAt`, poll `/copilot_internal/v2/token` when about to expire. The GitHub access token itself never expires during the session.
3. **Kiro** has 2 paths:
   - AWS path: POST `https://oidc.<region>.amazonaws.com/token` with `{clientId, clientSecret, refreshToken, grantType:"refresh_token"}`
   - Social path: POST `https://prod.us-east-1.auth.desktop.kiro.dev/refreshToken` with `{refreshToken}`
   - Decision based on `providerSpecificData.authMethod` (or presence of `clientId/clientSecret`).
4. **xAI**: re-call `discoverXaiEndpoints()` only on cold cache, not every refresh.
5. **Refresh dedup** (anti-`refresh_token_reused`): cache in-flight promise by `${provider}:${oldRefreshToken}` for **10 seconds**. If a 2nd refresh comes in within that window, return the in-flight result. This prevents Auth0/Google from revoking the refresh token because two clients raced to use it.

### 3.3 File-by-file action list (PHASE 1 + 2)

| File | Issue # | Change |
|---|---|---|
| `src/oauth/mod.rs:158-296` (all `pub fn` in `providers` mod) | #P1.1 | Rewrite claude/codex/github/kiro URLs + scopes + clientIds to match 9router constants. Delete module — move to `src/oauth/providers.rs` |
| `src/oauth/providers.rs` (NEW) | #P1.1 | All 14 PKCE/device-code provider constants. Single struct `OAuthProviderConfig { client_id, authorize_url, token_url, refresh_url?, scopes, uses_pkce, extra_params, fixed_port?, callback_path?, content_type }` |
| `src/oauth/xai.rs` (NEW) | #P1.2 | `XaiService` with discovery + loopback 56121 + 96-byte verifier + nonce/plan/referrer |
| `src/oauth/gemini_cli.rs` (NEW) | #P1.2 | `GeminiCliService` with postExchange `loadCodeAssist` → save `projectId` to DB |
| `src/oauth/antigravity.rs` (NEW) | #P1.2 | `AntigravityService` with `loadCodeAssist` + `onboardUser` polling + `Client-Metadata` numeric enums |
| `src/oauth/openai.rs` (NEW) | #P1.2 | `OpenAIService` (PKCE + `originator=openai_native`) |
| `src/oauth/qoder.rs` (NEW) | #P1.2 | `QoderService` with device token + 15s timeout per poll |
| `src/oauth/kiro.rs` (NEW, refactor from pending) | #P1.3 | 5-method enum + per-method URL + per-method refresh URL + kiro:// redirect |
| `src/oauth/cursor.rs` (NEW) | #P1.5 | `CursorService` with SQLite read + jyh checksum + 24h expiry |
| `src/oauth/token_refresh.rs` (NEW or rewrite) | #P1.4 | Per-provider refresh + `dedupRefresh` + 10s TTL cache |
| `src/oauth/pending.rs` | (refactor) | Keep store, wire to new service dispatch |
| `src/core/executor/kiro.rs:23` | #P2.1 | Change `KIRO_API_ENDPOINT` to `https://codewhisperer.us-east-1.amazonaws.com/generateAssistantResponse` + add SigV4 signer |
| `src/core/executor/provider.rs:463,471` | #P2.1 | Update `kiro`/`kiro-free` config — switch to anthropic-compatible codewhisperer endpoint |
| `src/core/executor/antigravity.rs` | #P2.2 | Add `Client-Metadata` builder + `loadCodeAssist` flow + projectId cache (DashMap<connectionId, (projectId, fetchedAt)>) |
| `src/core/executor/gemini_cli.rs` | #P2.3 | Switch from `?key=` query to `Authorization: Bearer` + Client-Metadata |
| `src/core/executor/xai.rs` (NEW) | #P2.4 | Proxy to `https://api.x.ai/v1` with Bearer; port upstream `default` executor pattern |
| `src/core/executor/qoder.rs` (extend) | #P1.2+2 | Wire OAuth service → executor (already exists, need OAuth input) |
| `src/core/chat/mod.rs:11` | #P4.1 | Insert `inject_caveman(body, source_format, level)` after RTK + before forward |
| `src/core/translator/caveman.rs` (NEW) | #P4.1 | Port `open-sse/rtk/caveman.js` + `cavemanPrompts.js`. Handle: Claude system / Gemini system / OpenAI messages+instructions / OpenAI Responses string field / Antigravity nested `request.contents` |
| `src/core/translator/registry.rs:15-28` | #P4.2 | Add `Format::CommandCode` |
| `src/core/translator/request/commandcode.rs` (NEW) | #P4.2 | Port `openai-to-commandcode.js` (build Claude-shaped body for commandcode provider) |
| `src/core/translator/response/commandcode.rs` (NEW) | #P4.2 | Port `commandcode-to-openai.js` (SSE → OpenAI chunks) |

---

## 4. CLI setup auth per tool — 11+ CLIs (PHASE 3)

### 4.1 Master table

| # | CLI | configType | File or env | Path / keys | openproxy current |
|---|---|---|---|---|---|
| 1 | `claude` (Claude Code) | `env` | `~/.claude/settings.json` | keys: `ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_MODEL`, `ANTHROPIC_DEFAULT_OPUS_MODEL`, `ANTHROPIC_DEFAULT_SONNET_MODEL`, `ANTHROPIC_DEFAULT_HAIKU_MODEL`, `API_TIMEOUT_MS`, `DISABLE_TELEMETRY`, `DISABLE_ERROR_REPORTING` | ✅ `cli_tools/claude_settings.rs` — verify env var names match exactly |
| 2 | `codex` (OpenAI Codex CLI) | `custom` (guide UI + apply button) | env `OPENAI_BASE_URL=http://localhost:4623` (NO `/v1`), `OPENAI_API_KEY`; file `~/.codex/config.json` keys: `baseUrl, apiKey, defaultModel` | ❌ MISSING — need `codex_settings.rs` |
| 3 | `cursor` (Cursor IDE) | `guide` (UI-only, **no file**) | Cursor Settings → Models → OpenAI API Base URL = `{{baseUrl}}`, API Key = `{{apiKey}}`, then "Add Custom Model". **Requires Cursor Pro + cloud endpoint (NOT localhost)** | ❌ MISSING — need `cursor_settings.rs` (returns guide steps only) |
| 4 | `cline` (VSCode Cline) | `custom` | VSCode globalState file at `~/.config/Code/User/globalStorage/saoudrizwan.claude/settings/globalState.json` (Linux), `~/Library/Application Support/Code/...` (macOS), `%APPDATA%\Code\...` (Windows). Keys: `actModeApiProvider`, `actModeApiModelId`, `actModeOpenAiBaseUrl`, `actModeOpenAiModelId`, `actModeOpenAiApiKey`, `planModeApiProvider`, `planModeApiModelId`, `planModeOpenAiBaseUrl`, `planModeOpenAiModelId`, `planModeOpenAiApiKey` | ✅ `cline_settings.rs` — verify path + key names match |
| 5 | `continue` (VSCode Continue) | `guide` (JSON merge) | `~/.continue/config.json` (Linux/macOS), `%USERPROFILE%\.continue\config.json` (Win). Merge into `models[]`: `{"title":"{{model}}","provider":"openai","model":"{{model}}","apiKey":"{{apiKey}}","apiBase":"{{baseUrl}}"}` | ❌ MISSING — need `continue_settings.rs` |
| 6 | `roo` (Roo AI Assistant) | `guide` (Ollama-compatible) | VSCode globalState, ext `RooVeterinaryInc.roo-cline`. Same path as Cline but different prefix. Keys: similar to Cline. | ❌ MISSING — need `roo_settings.rs` |
| 7 | `kilo` (Kilo Code) | `custom` | VSCode globalState, ext `kilocode.kilo-code`. Same path as Cline. | ✅ `kilo_settings.rs` — verify ext id + auth key names |
| 8 | `openclaw` (Open Claw) | `custom` (one-click apply) | dashboard button writes to OpenClaw config (path TBD, port 4623-aware). | ❌ MISSING — need `openclaw_settings.rs` |
| 9 | `hermes` (Nous Hermes Agent) | `custom` | `~/.hermes/settings.json` (verify path) | ✅ `hermes_settings.rs` — verify |
| 10 | `droid` (Factory Droid) | `custom` | `~/.factory/settings.json` (verify path) | ❌ MISSING — need `droid_settings.rs` |
| 11 | `cowork` (Claude Cowork) | `custom` (Desktop Claude) | macOS `~/Library/Application Support/Claude/Cowork.json`; Linux `~/.config/Claude/Cowork.json`; Windows `%APPDATA%\Claude\Cowork.json`. | ✅ `cowork_settings.rs` — verify path |
| 12+ | MITM tools: `antigravity` IDE | `mitm` | CA cert install + route `daily-cloudcode-pa.googleapis.com`, `cloudcode-pa.googleapis.com`, `cloudcode-pa.sandbox.googleapis.com` to MITM proxy. Model aliases: `gemini-3.5-flash-low` (mandatory), `gemini-3-flash-agent`, `gemini-3.5-flash-extra-low`, `gemini-3.1-pro-low`, `gemini-pro-agent`, `claude-sonnet-4-6`, `claude-opus-4-6-thinking`, `gpt-oss-120b-medium`, `gemini-3-flash` | ✅ `core/mitm/` — verify domains + aliases |
| 13+ | MITM tool: `copilot` (GitHub Copilot IDE) | `mitm` | Route `api.individual.githubcopilot.com` (NOT `api.githubcopilot.com` — that one is for VSCode extension). Header `x-github-api-version: 2025-04-01`. Model aliases: `gpt-4o`, `gpt-4.1`, `gpt-5-mini`, `claude-haiku-4.5` | ✅ `core/mitm/` — verify |
| 14+ | MITM tool: `kiro` (Kiro IDE) | `mitm` | Route `q.us-east-1.amazonaws.com`. Model aliases: `auto` (mandatory), `claude-sonnet-4.5`, `claude-sonnet-4`, `claude-haiku-4.5`, `deepseek-3.2`, `minimax-m2.5`, `minimax-m2.1`, `glm-5`, `simple-task` | ✅ `core/mitm/` — verify |

### 4.2 Per-CLI **quirks** (the subtle gotchas)

1. **Claude Code**: env `ANTHROPIC_AUTH_TOKEN` (NOT `ANTHROPIC_API_KEY` — Claude Code rejects that). Opus/sonnet/haiku aliases must each point to a real `cc/<model>` model. `ANTHROPIC_MODEL` is the default.
2. **Codex CLI**: env `OPENAI_BASE_URL` does NOT include `/v1` suffix; `~/.codex/config.json` JSON keys are **lowercase**: `baseUrl, apiKey, defaultModel`. The CLI itself appends `/v1`.
3. **Cursor**: **localhost does not work** — Cursor proxies all requests through its own backend. Must use tunnel or cloud endpoint. Requires Cursor Pro.
4. **Cline**: chooses "Ollama" as the API provider type (because Ollama is OpenAI-compatible). Base URL must include `/v1`. Two modes: `actMode` (action) and `planMode` (planning) — both need separate settings.
5. **Continue**: provider literal must be `"openai"` (not `"openrouter"` — that would trigger Continue's special handling). `apiBase` includes `/v1`.
6. **Roo**: identical to Cline but ext id differs.
7. **Kilo**: ext id `kilocode.kilo-code` (NOT `kilocode` — VSCode uses fully qualified id). Auth keys live under this ext's storage.
8. **OpenClaw**: one-click apply from dashboard — no user-facing file path; OpenClaw's own config format.
9. **Hermes**: Nous Research self-improving agent. Verify exact config schema.
10. **Droid**: Factory Droid; verify path.
11. **Cowork**: Claude Desktop third-party inference; verify OS path.

### 4.3 File-by-file action list (PHASE 3)

| File | Issue # | Change |
|---|---|---|
| `src/server/api/cli_tools.rs` | #P3.1 | Add `CLI_TOOLS` registry struct (mirroring 9router `cliTools.js`): id, name, image, color, description, configType, envVars, settingsFile, guideSteps, modelAliases, defaultModels. Replace the hardcoded `tools` Vec. |
| `src/server/api/cli_tools/codex_settings.rs` (NEW) | #P3.2 | Port 9router `codex`: env `OPENAI_BASE_URL=http://localhost:4623`, `OPENAI_API_KEY`. File `~/.codex/config.json` keys `baseUrl, apiKey, defaultModel`. |
| `src/server/api/cli_tools/cursor_settings.rs` (NEW) | #P3.3 | Returns guide steps only (no file write). Notes: "Requires Cursor Pro + tunnel/cloud endpoint". |
| `src/server/api/cli_tools/continue_settings.rs` (NEW) | #P3.4 | Port JSON merge. `~/.continue/config.json` → `models[]` with `provider: "openai"`. |
| `src/server/api/cli_tools/roo_settings.rs` (NEW) | #P3.5 | Port `roo` globalState (Cline pattern, different ext id). |
| `src/server/api/cli_tools/droid_settings.rs` (NEW) | #P3.6 | Port `droid`. |
| `src/server/api/cli_tools/openclaw_settings.rs` (NEW) | #P3.7 | Port `openclaw` one-click apply. |
| `src/server/api/cli_tools/claude_settings.rs` | #P3.8 | Add `ANTHROPIC_AUTH_TOKEN` (NOT `ANTHROPIC_API_KEY`); verify all 6 env var names; verify opus/sonnet/haiku default values use current 9router model IDs. |
| `src/server/api/cli_tools/cline_settings.rs` | #P3.8 | Verify Linux/macOS/Windows globalState paths; verify all 10 keys (act/plan × 5). |
| `src/server/api/cli_tools/kilo_settings.rs` | #P3.8 | Verify ext id `kilocode.kilo-code`. |
| `src/server/api/cli_tools/hermes_settings.rs` | #P3.8 | Verify Hermes config path. |
| `src/server/api/cli_tools/cowork_settings.rs` | #P3.8 | Verify OS-specific Cowork.json paths. |
| `src/core/mitm/config.rs` | #P3.9 | Verify domains + model aliases. Add `MODEL_NO_MAP` list (`tab_*` models from Antigravity — never re-routed). |
| `src/core/mitm/handlers/copilot.rs` | #P3.9 | Verify domain `api.individual.githubcopilot.com` + header `x-github-api-version: 2025-04-01`. |

---

## 5. Cross-cutting concerns

### 5.1 Token refresh dedup (#P1.4)

In 9router `open-sse/services/tokenRefresh.js:104-122`:

```js
const REFRESH_RESULT_TTL_MS = 10_000;
const refreshDedupCache = new Map();  // key: `${provider}:${oldToken}` → { promise, expiresAt, value }

async function dedupRefresh(provider, oldToken, fn, log) {
  if (!oldToken) return fn();
  const key = `${provider}:${oldToken}`;
  const hit = refreshDedupCache.get(key);
  if (hit) {
    if (hit.promise) {
      log?.info?.("TOKEN_REFRESH", `Reusing in-flight refresh for ${provider}`);
      return hit.promise;
    }
    if (hit.expiresAt > Date.now()) {
      log?.info?.("TOKEN_REFRESH", `Reusing recent refresh result for ${provider}`);
      return hit.value;
    }
    refreshDedupCache.delete(key);
  }
  const promise = fn().finally(() => {
    const entry = refreshDedupCache.get(key);
    if (entry?.promise === promise) {
      entry.promise = null;
      entry.value = result;
      entry.expiresAt = Date.now() + REFRESH_RESULT_TTL_MS;
    }
  });
  refreshDedupCache.set(key, { promise, expiresAt: 0 });
  return promise;
}
```

Port this to Rust with `parking_lot::Mutex<HashMap<String, DedupEntry>>`. This is **required** to prevent Auth0-family `refresh_token_reused` revocation when concurrent requests all see "token about to expire".

### 5.2 Project ID caching (#P2.2 + #P2.3)

For Antigravity and Gemini-CLI, the upstream `loadCodeAssist` returns a `cloudaicompanionProject.id` that must accompany every request. 9router caches it per-connection in a `Map<connectionId, { projectId, fetchedAt }>` with TTL (typically 5 min) and invalidation on token refresh.

```rust
// src/core/executor/antigravity.rs (or gemini_cli.rs)
struct ProjectIdCache {
    inner: parking_lot::Mutex<HashMap<String, CachedProject>>,
    ttl_ms: i64,  // default 5 * 60 * 1000
}
struct CachedProject {
    project_id: String,
    fetched_at: Instant,
}
// On token refresh → invalidate that connectionId
// On request → if cache hit && not expired → use cached; else fetch
```

### 5.3 Combo + fallback (#P5 — verify, mostly exists)

`9router` combo mechanics (already ported in `src/core/combo/mod.rs` + `src/core/account_fallback/mod.rs`):
- `comboStrategy` global: `fallback` (default) or `round-robin`.
- `comboStrategies` per-combo override map (not yet in openproxy).
- `comboStickyRoundRobinLimit` per-combo (not yet in openproxy — we have global `sticky_round_robin_limit`).
- `modelLock_${model}` flat fields on connection for per-model cooldown (we have `AccountRegistry.model_locks` keyed `sticky_${combo_id}` — different).

→ Add `comboStrategies: BTreeMap<String, ComboStrategyOverride>` to `Settings` + plumb through combo executor.

### 5.4 Caveman prompt injection (#P4.1)

9router `caveman.js` injects a system message **format-aware**:
- Claude: append to `body.system` (array of content blocks) or create if missing.
- Gemini/Vertex/Antigravity/Codex: append to `body.system_instruction` (Gemini shape) or `body.request.systemInstruction` (Antigravity nested).
- OpenAI: append to first `system` or `developer` message in `messages[]`; if `body.instructions` is a string (Responses API), append there.
- OpenAI Responses: same.
- OpenCode Free / Cursor / Kiro / Ollama (OpenAI-shaped): same as OpenAI.

Levels: `lite`, `full`, `ultra`, `wenyan-lite`, `wenyan`, `wenyan-ultra`. Each has its own prompt (see `cavemanPrompts.js`).

**Where to inject**: in `src/core/chat/mod.rs` between step 4 (RTK) and step 5 (forward to provider), only if `settings.caveman_enabled`. The injection happens AFTER format detection (so we know if it's Claude or Gemini or OpenAI) and AFTER RTK (so RTK doesn't try to compress the caveman prompt).

### 5.5 Format::CommandCode (#P4.2)

9router has 13 formats. We have 12. Commandcode is a Claude-shaped provider with very specific field names (e.g., `command` for the user prompt content). Port `open-sse/translator/request/openai-to-commandcode.js` (build Claude body) + `open-sse/translator/response/commandcode-to-openai.js` (parse SSE → OpenAI chunks).

### 5.6 Format detection by endpoint

Already done in `src/core/translator/registry.rs:detect_source_format_by_endpoint`:
- `/v1/responses` → `OpenAiResponses`
- `/v1/messages` → `Claude`
- `/v1/chat/completions` + `body.input[]` → `OpenAi` (Cursor CLI sends Responses body via chat endpoint)

✅ OK.

---

## 6. Test plan (PHASE 0 + 5)

### 6.1 Per-provider OAuth unit test

For each of the 17 providers, a test that:
1. Constructs the authorize URL with known inputs.
2. Asserts URL contains the exact `clientId` from 9router.
3. Asserts URL contains all expected scopes (no more, no less).
4. Asserts the URL host matches 9router's constant.

```rust
#[test]
fn claude_authorize_url_matches_9router() {
    let url = build_auth_url("claude", "http://localhost:1234/cb", "state", "challenge");
    assert!(url.starts_with("https://claude.ai/oauth/authorize"));
    assert!(url.contains("client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e"));
    assert!(url.contains("scope=org%3Acreate_api_key+user%3Aprofile+user%3Ainference"));
    assert!(url.contains("code=true"));
}
```

### 6.2 Per-provider refresh test (mock HTTP)

```rust
#[tokio::test]
async fn claude_refresh_uses_json_body() {
    let mock = mock_server.expect(POST, "https://api.anthropic.com/v1/oauth/token")
        .match_header("content-type", "application/json")
        .match_body(...)
        .respond_with(...)
        .create();
    let new = refresh_claude("old_refresh_token").await.unwrap();
    mock.assert();
}
```

### 6.3 Per-CLI setup test (filesystem)

```rust
#[tokio::test]
async fn claude_settings_writes_correct_env_block() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join(".claude/settings.json");
    save_claude_settings(Settings { base_url: "http://localhost:4623".into(), ... }, &path).await.unwrap();
    let json: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).await.unwrap()).unwrap();
    assert_eq!(json["env"]["ANTHROPIC_BASE_URL"], "http://localhost:4623/v1");
    assert_eq!(json["env"]["ANTHROPIC_AUTH_TOKEN"], "op-...");
    assert!(json["env"]["ANTHROPIC_DEFAULT_OPUS_MODEL"].as_str().unwrap().starts_with("cc/"));
}
```

### 6.4 E2E smoke (PHASE 5)

Mock upstream OAuth + mock LLM API. Spin up OpenProxy. Send 1 request per provider via `curl` to `/v1/chat/completions`. Verify 200 OK. Verify combo fallback: if provider A returns 429, provider B picks up. Verify CLI: after `openproxy api cli-tools claude-settings --apply`, run `claude --version` and a tiny prompt.

---

## 7. Phased rollout (7 working days)

| Day | Phase | Deliverable | Issue IDs |
|---|---|---|---|
| 0 | Test infrastructure | OAuth URL unit tests + refresh mock tests pass for all 17 providers; CLI setup filesystem tests for 11 CLIs | #P0 |
| 1 | Phase 1.1 | `src/oauth/providers.rs` NEW with 14 providers (claude/codex/github/kiro/qwen/iflow/kimi/kilocode/cline/gitlab/codebuddy + cursor import). Delete conflicting constants from `src/oauth/mod.rs` and `src/server/api/oauth.rs`. | #P1.1 |
| 1 | Phase 1.2 | `xai.rs` + `gemini_cli.rs` + `antigravity.rs` + `openai.rs` + `qoder.rs` OAuth services | #P1.2 |
| 2 | Phase 1.3 | `kiro.rs` 5-method refactor + verify all 4 token-URL paths | #P1.3 |
| 2 | Phase 1.4 | `token_refresh.rs` with `dedupRefresh` + `REFRESH_LEAD_MS` per provider + Claude JSON body + GitHub copilotToken poll | #P1.4 |
| 2 | Phase 1.5 | `cursor.rs` import service + jyh checksum | #P1.5 |
| 3 | Phase 2.1 | Kiro executor → AWS CodeWhisperer + SigV4 signer | #P2.1 |
| 3 | Phase 2.2 | Antigravity executor → Client-Metadata + projectId cache | #P2.2 |
| 3 | Phase 2.3 | Gemini CLI executor → Bearer + Client-Metadata | #P2.3 |
| 3 | Phase 2.4 | xAI executor NEW | #P2.4 |
| 4 | Phase 3.1 | `cli_tools.rs` registry refactor | #P3.1 |
| 4 | Phase 3.2-3.7 | 5 new CLI setup modules (codex/cursor/continue/roo/droid/openclaw) | #P3.2-3.7 |
| 5 | Phase 3.8 | Verify existing 6 CLI modules against 9router (env var names, paths, key names) | #P3.8 |
| 5 | Phase 3.9 | Verify MITM domains + model aliases | #P3.9 |
| 5 | Phase 4.1 | `caveman.rs` translator + inject in `chat/mod.rs` | #P4.1 |
| 5 | Phase 4.2 | `Format::CommandCode` + request/response translators | #P4.2 |
| 6 | Phase 5 | E2E smoke test pass | #P5 |
| 7 | Polish | Update CHANGELOG.md + README "Connect a CLI" table + `--sync 9router` snapshot refresh | #P5+ |

---

## 8. Risk matrix

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| OAuth URL off by one path segment → user sees 404 in browser | HIGH (current state) | CRITICAL | Phase 1 + literal byte-compare test in #P0 |
| `clientId` wrong → upstream returns `unauthorized_client` | HIGH | CRITICAL | Hardcode 9router's exact string + comment `// DO NOT CHANGE — value pinned to 9router v0.4.66-1-gcce8a50` |
| `scopes` extra/missing → upstream returns `invalid_scope` or 403 | MED | HIGH | Set comparison test in #P0 |
| `kiro` executor hits `api.kiro.ai/v1` (404) instead of AWS CodeWhisperer | HIGH (current state) | CRITICAL | Phase 2.1 + SigV4 signer verified by integration test |
| `loadCodeAssist` missing → antigravity/gemini-cli 403 from Google | HIGH | CRITICAL | Phase 1.2 + 2.2/2.3 with mock test |
| Refresh fires twice in parallel → Auth0 revokes refresh_token | MED (existing) | HIGH | Phase 1.4 `dedupRefresh` + 10s TTL |
| CLI settings file path wrong OS → file not found error | MED | HIGH | Phase 3.8 + per-OS test in #P0 |
| Cursor localhost doesn't work → user thinks OpenProxy is broken | HIGH | MED | Phase 3.3 explicit error: "Cursor requires Pro + tunnel/cloud endpoint, see docs" |
| MITM mandatory model alias (`gemini-3.5-flash-low`, `auto`) unmapped → request pass-through | MED | HIGH | Phase 3.9 verify aliases + `MODEL_NO_MAP` list |
| `Format::CommandCode` missing → translate registry miss → 500 | MED | LOW | Phase 4.2 + fallback to passthrough if format unknown |

---

## 9. Definition of done

### 9.1 Per provider (all 17)

- [ ] Authorize URL string equals 9router's exact URL (byte-compare test).
- [ ] Token URL string equals 9router's exact URL.
- [ ] clientId equals 9router's exact value.
- [ ] Scopes set equals 9router's scopes (no extra, no missing).
- [ ] PKCE verifier byte length matches (32 default, 96 for xAI).
- [ ] Refresh URL & body format matches 9router (JSON for Claude, form for others).
- [ ] Refresh lead time matches 9router's `REFRESH_LEAD_MS`.
- [ ] End-to-end: 1 successful `curl /v1/chat/completions` per provider (mock upstream).
- [ ] Combo fallback: if this provider returns 429, next combo member picks up.

### 9.2 Per CLI (all 11)

- [ ] `GET /api/cli-tools/<id>-settings` returns correct path + has-flag.
- [ ] `POST /api/cli-tools/<id>-settings` writes the exact file/env the real CLI accepts.
- [ ] All env var names match 9router (`ANTHROPIC_AUTH_TOKEN` not `ANTHROPIC_API_KEY`, `OPENAI_BASE_URL` no `/v1`, etc.).
- [ ] All settings-file paths are OS-correct (Linux/macOS/Windows).
- [ ] Real CLI (claude/codex/cursor/cline/...) boots with OpenProxy config and successfully routes 1 prompt to OpenProxy logs.
- [ ] Reset endpoint (`DELETE`) cleans up exactly the keys OpenProxy wrote, no more.

### 9.3 Cross-cutting

- [ ] `dedupRefresh` prevents 2nd refresh within 10s for same `${provider}:${oldToken}`.
- [ ] `caveman_enabled=true` produces a system message that the LLM actually responds tersely to.
- [ ] `Format::CommandCode` round-trip: OpenAI request → commandcode → OpenAI response.

---

## 10. References

- 9router source: https://github.com/decolua/9router
- 9router pinned snapshot: `src/core/model/sources/9router.json` (`v0.4.66-1-gcce8a50`, 2026-06-06)
- open-sse package (reusable across projects): `open-sse/` in 9router repo
- 9router OAuth services: `src/lib/oauth/services/*.js` (13 files, one per provider or family)
- 9router OAuth constants: `src/lib/oauth/constants/oauth.js` + `constants/xai.js`
- 9router token refresh: `open-sse/services/tokenRefresh.js`
- 9router CLI tool catalog: `src/shared/constants/cliTools.js`
- 9router CLI setup docs: `gitbook/content/en/integration/*.md` (7 docs)
- 9router OAuth clientIds (all public, hardcoded):
  - claude: `9d1c250a-e61b-44d9-88ed-5944d1962f5e`
  - codex/openai: `app_EMoamEEZ73f0CkXaXp7hrann`
  - gemini-cli: `681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com`
  - qwen: `f0304373b74a44d2b584a3fb70ca9e56`
  - iflow: `10009311001`
  - antigravity: `1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com`
  - github: `Iv1.b507a08c87ecfe98`
  - xai: `b1a00492-073a-47ea-816f-4c329264a828`
  - kimi-coding: env override or `17e5f671-d194-4dfb-9706-5516cb48c098`

---

## 11. Issue index (mirror to GitHub)

| Issue # | Title | Phase | Est. effort |
|---|---|---|---|
| #P0 | Test infrastructure: per-provider OAuth URL tests + per-CLI filesystem tests | 0 | 0.5d |
| #P1.1 | Rewrite OAuth provider constants — single source of truth in `oauth/providers.rs` | 1.1 | 0.5d |
| #P1.2 | Add 5 missing OAuth services: xai, gemini-cli, antigravity, openai-native, qoder | 1.2 | 1.5d |
| #P1.3 | Kiro 5-method refactor (builder-id / idc / google / github / import) | 1.3 | 0.5d |
| #P1.4 | Token refresh — `dedupRefresh` + `REFRESH_LEAD_MS` per provider | 1.4 | 0.5d |
| #P1.5 | Cursor import service + jyh checksum | 1.5 | 0.3d |
| #P2.1 | Kiro executor → AWS CodeWhisperer + SigV4 signer | 2.1 | 0.5d |
| #P2.2 | Antigravity executor → Client-Metadata + projectId cache | 2.2 | 0.5d |
| #P2.3 | Gemini CLI executor → Bearer + Client-Metadata | 2.3 | 0.3d |
| #P2.4 | xAI executor NEW (port 9router default executor with Bearer) | 2.4 | 0.3d |
| #P3.1 | CLI tools registry refactor (mirror 9router `cliTools.js`) | 3.1 | 0.3d |
| #P3.2 | Codex CLI settings (env + `~/.codex/config.json`) | 3.2 | 0.3d |
| #P3.3 | Cursor IDE settings (guide steps, no file) | 3.3 | 0.2d |
| #P3.4 | Continue VSCode settings (JSON merge) | 3.4 | 0.3d |
| #P3.5 | Roo AI Assistant settings (globalState) | 3.5 | 0.3d |
| #P3.6 | Factory Droid settings | 3.6 | 0.2d |
| #P3.7 | OpenClaw one-click apply | 3.7 | 0.2d |
| #P3.8 | Verify 6 existing CLI modules (claude/cline/kilo/hermes/cowork/...) against 9router | 3.8 | 0.5d |
| #P3.9 | Verify MITM domains + model aliases for antigravity/copilot/kiro IDE | 3.9 | 0.3d |
| #P4.1 | Caveman prompt injection (port `caveman.js` + `cavemanPrompts.js`) | 4.1 | 0.5d |
| #P4.2 | `Format::CommandCode` + request/response translators | 4.2 | 0.5d |
| #P5 | E2E smoke test (per-provider request + per-CLI setup + combo fallback) | 5 | 1d |
| #P-CHK | Per-provider checklist issue (composite, sub-checklist of #P1.1-#P1.5) | — | — |
| #P-CLI | Per-CLI checklist issue (composite, sub-checklist of #P3.1-#P3.9) | — | — |

---

**Last updated**: 2026-06-10
**Pinned to**: 9router `v0.4.66-1-gcce8a50`
**Total est. effort**: 7 working days
