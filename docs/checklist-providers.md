# Provider Composite Checklist

> **Parent**: [Master Plan: OpenProxy = 9Router (Source of Truth Parity)](MASTER_PLAN_9ROUTER_PARITY.md)
> **Issue**: #P-CHK (sub-checklist of #P1.1–#P1.5)
> **Scope**: All 17 OAuth-bearing providers
> **Reference**: 9router `v0.4.66-1-gcce8a50`

---

## Acceptance criteria (Definition of Done)

For each provider the following must be true:

- [ ] **Authorize URL** — byte-identical to 9router's exact URL string.
- [ ] **Token URL** — byte-identical to 9router's exact URL string.
- [ ] **clientId** — byte-identical to 9router's exact value; annotated `// DO NOT CHANGE — value pinned to 9router v0.4.66-1-gcce8a50`.
- [ ] **Scopes** — set-equal to 9router (no extra, no missing).
- [ ] **PKCE verifier length** — matches 9router (32 bytes default, 96 for xAI).
- [ ] **Refresh URL & body format** — matches 9router (JSON for Claude, form-urlencoded for others).
- [ ] **Refresh lead time** — matches 9router's `REFRESH_LEAD_MS`.
- [ ] **OAuth unit test** — constructs authorize URL, asserts host + clientId + scopes.
- [ ] **Refresh mock test** — asserts HTTP method, content-type, and body shape.
- [ ] **End-to-end** — `curl /v1/chat/completions` succeeds (mock upstream) for each provider.
- [ ] **Combo fallback** — if this provider returns 429, next combo member picks up.

---

## 17 Providers

### Phase 1.1 — URL/scopes/clientId fixes (4 providers)

#### 1. claude

- [ ] Authorize URL: `https://claude.ai/oauth/authorize` (not `auth.claude.ai`)
- [ ] Token URL: `https://api.anthropic.com/v1/oauth/token`
- [ ] clientId: `9d1c250a-e61b-44d9-88ed-5944d1962f5e`
- [ ] Scopes: `org:create_api_key user:profile user:inference`
- [ ] Extra: `code: true` query param on authorize
- [ ] Parse `#state` from returned code
- [ ] Refresh body format: **JSON** (`{"grant_type":"refresh_token","refresh_token":...,"client_id":"9d1c250a-..."}`)
- [ ] Refresh Content-Type: `application/json`
- [ ] Refresh lead time: 4 hours
- [ ] OAuth unit test passes
- [ ] Refresh mock test passes
- [ ] E2E test passes

#### 2. codex

- [ ] Authorize URL: `https://auth.openai.com/oauth/authorize` (not `codex.ai`)
- [ ] Token URL: `https://auth.openai.com/oauth/token`
- [ ] clientId: `app_EMoamEEZ73f0CkXaXp7hrann`
- [ ] Scopes: `openid profile email offline_access`
- [ ] Fixed port: 1455
- [ ] Callback: `/auth/callback`
- [ ] Extra params: `id_token_add_organizations=true, codex_cli_simplified_flow=true, originator=codex_cli_rs`
- [ ] Refresh lead time: 5 days
- [ ] OAuth unit test passes
- [ ] Refresh mock test passes
- [ ] E2E test passes

#### 3. github

- [ ] Authorize URL: `https://github.com/login/device/code`
- [ ] Token URL: `https://github.com/login/oauth/access_token`
- [ ] clientId: `Iv1.b507a08c87ecfe98`
- [ ] Scopes: `read:user` (NOT `read:user repo`)
- [ ] User-Agent: `GitHubCopilotChat/0.38.0`
- [ ] Header: `X-GitHub-Api-Version: 2025-04-01`
- [ ] Post-exchange call: `copilot_internal/v2/token`
- [ ] No refresh_token in device response — cache copilotToken + poll
- [ ] OAuth unit test passes
- [ ] Refresh mock test passes
- [ ] E2E test passes

#### 4. kiro

- [ ] 5 methods supported: builder-id / idc / google / github / import
- [ ] AWS SSO OIDC dynamic URLs
- [ ] Cognito social redirect: `kiro://kiro.kiroAgent/authenticate-success`
- [ ] clientId: dynamic via `oidc.us-east-1.amazonaws.com/client/register`
- [ ] Scopes: `codewhisperer:completions codewhisperer:analysis codewhisperer:conversations`
- [ ] Refresh AWS path: `oidc.<region>.amazonaws.com/token`
- [ ] Refresh social path: `prod.us-east-1.auth.desktop.kiro.dev/refreshToken`
- [ ] Refresh decision based on `providerSpecificData.authMethod`
- [ ] OAuth unit test passes
- [ ] Refresh mock test passes
- [ ] E2E test passes

### Phase 1.2 — Missing OAuth services (5 providers)

#### 5. xai

- [ ] Dynamic discovery from `https://auth.x.ai/.well-known/openid-configuration`
- [ ] Host validation: only `x.ai` / `*.x.ai`
- [ ] clientId: `b1a00492-073a-47ea-816f-4c329264a828`
- [ ] Scopes: `openid profile email offline_access grok-cli:access api:access`
- [ ] Fixed loopback port: 56121
- [ ] Callback: `/callback`
- [ ] PKCE verifier: **96 bytes** (not 32)
- [ ] Extra params: `nonce, plan=generic, referrer=cli-proxy-api`
- [ ] Body format: form-urlencoded
- [ ] Refresh lead time: 5 minutes
- [ ] Re-discover endpoints only on cold cache
- [ ] OAuth unit test passes
- [ ] Refresh mock test passes
- [ ] E2E test passes

#### 6. gemini-cli

- [ ] Authorize URL: `https://accounts.google.com/o/oauth2/v2/auth`
- [ ] Token URL: `https://oauth2.googleapis.com/token`
- [ ] clientId: `681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com`
- [ ] Scopes: `cloud-platform userinfo.email userinfo.profile`
- [ ] Params: `access_type=offline, prompt=consent`
- [ ] Post-exchange: fetch `cloudcode-pa.googleapis.com/v1internal:loadCodeAssist` (mode=1)
- [ ] Save `cloudaicompanionProject.id`
- [ ] Project ID cache: per-connection, 5 min TTL
- [ ] Refresh: `expiresIn` from response
- [ ] OAuth unit test passes
- [ ] Refresh mock test passes
- [ ] E2E test passes

#### 7. antigravity

- [ ] Authorize URL: `https://accounts.google.com/o/oauth2/v2/auth`
- [ ] Token URL: `https://oauth2.googleapis.com/token`
- [ ] clientId: `1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com`
- [ ] Scopes: `cloud-platform userinfo.email userinfo.profile cclog experimentsandconfigs`
- [ ] Headers: `User-Agent: google-api-nodejs-client/9.15.1`, `X-Goog-Api-Client: google-cloud-sdk vscode_cloudshelleditor/0.1`
- [ ] Client-Metadata: `{"ideType":9,"platform":<numeric>,"pluginType":2}`
- [ ] Post-exchange: `loadCodeAssist` + `onboardUser` (polls 5s x 10 retries)
- [ ] Project ID cache: per-connection, 5 min TTL
- [ ] Refresh lead time: 5 minutes
- [ ] OAuth unit test passes
- [ ] Refresh mock test passes
- [ ] E2E test passes

#### 8. openai (native)

- [ ] Authorize URL: `https://auth.openai.com/oauth/authorize`
- [ ] Token URL: `https://auth.openai.com/oauth/token`
- [ ] clientId: `app_EMoamEEZ73f0CkXaXp7hrann` (same as codex)
- [ ] Scopes: `openid profile email offline_access`
- [ ] Extra params: `id_token_add_organizations=true, originator=openai_native` (NOT `codex_cli_rs`)
- [ ] Refresh lead time: 5 days
- [ ] OAuth unit test passes
- [ ] Refresh mock test passes
- [ ] E2E test passes

#### 9. qoder

- [ ] Device Token + PKCE flow
- [ ] Init URL: `https://qoder.com/device/selectAccounts`
- [ ] Poll URL: `https://openapi.qoder.sh/api/v1/deviceToken/poll`
- [ ] No clientId — uses nonce + verifier + challenge
- [ ] Poll timeout: 15s per poll
- [ ] 202/404 = pending, 200 = token
- [ ] Token TTL: 30 days
- [ ] Refresh returns 403 → re-login required (NO refresh)
- [ ] OAuth unit test passes
- [ ] Refresh mock test passes (403 expected)
- [ ] E2E test passes

### Phase 1.3 — No changes needed (verify only)

#### 10. qwen

- [ ] Device Code + PKCE
- [ ] Init URL: `https://chat.qwen.ai/api/v1/oauth2/device/code`
- [ ] Token URL: `https://chat.qwen.ai/api/v1/oauth2/token`
- [ ] clientId: `f0304373b74a44d2b584a3fb70ca9e56`
- [ ] Scopes: `openid profile email model.completion`
- [ ] Poll: handle `authorization_pending` / `slow_down`
- [ ] Capture `resource_url`
- [ ] Refresh lead time: 20 minutes
- [ ] OAuth unit test passes
- [ ] Refresh mock test passes
- [ ] E2E test passes

#### 11. iflow

- [ ] Auth Code + Basic Auth
- [ ] Authorize URL: `https://iflow.cn/oauth`
- [ ] Token URL: `https://iflow.cn/oauth/token`
- [ ] clientId: `10009311001`
- [ ] No scopes (empty set)
- [ ] Header: `Authorization: Basic base64(client:secret)`
- [ ] Query params: `loginMethod=phone type=phone`
- [ ] userInfoUrl: `https://iflow.cn/api/oauth/getUserInfo?accessToken=...` returns `apiKey`
- [ ] Refresh lead time: 24 hours
- [ ] OAuth unit test passes
- [ ] Refresh mock test passes
- [ ] E2E test passes

#### 12. kimi-coding

- [ ] Device Code
- [ ] Init URL: `https://api.moonshot.cn/kimi-device/oauth/device/code`
- [ ] Token URL: `https://api.moonshot.cn/kimi-device/oauth/token`
- [ ] clientId: env `KIMI_CODING_OAUTH_CLIENT_ID` \|\| `17e5f671-d194-4dfb-9706-5516cb48c098`
- [ ] Scopes: `kimi:read`
- [ ] Headers: `X-Msh-Platform: 9router`, `X-Msh-Version: 2.1.2`, `X-Msh-Device-Model`, `X-Msh-Device-Id`
- [ ] Refresh lead time: 5 minutes
- [ ] OAuth unit test passes
- [ ] Refresh mock test passes
- [ ] E2E test passes

#### 13. kilocode

- [ ] Device Code
- [ ] URL: `https://api.kilo.ai/api/device-auth/codes` (initiate AND poll same URL)
- [ ] No clientId
- [ ] Scopes: `read`
- [ ] Refresh: `expiresIn` from response
- [ ] OAuth unit test passes
- [ ] Refresh mock test passes
- [ ] E2E test passes

#### 14. cline

- [ ] Auth Code via `app.cline.bot`
- [ ] Authorize URL: `https://api.cline.bot/api/v1/auth/authorize`
- [ ] Token URL: `https://api.cline.bot/api/v1/auth/token`
- [ ] Refresh URL: `https://api.cline.bot/api/v1/auth/refresh`
- [ ] Custom clientId (dynamic)
- [ ] Refresh: `expiresIn` from response
- [ ] OAuth unit test passes
- [ ] Refresh mock test passes
- [ ] E2E test passes

#### 15. gitlab

- [ ] PKCE Auth Code (self-hostable)
- [ ] Authorize URL: `https://gitlab.com/oauth/authorize`
- [ ] Token URL: `https://gitlab.com/oauth/token`
- [ ] clientId: user-provided (dynamic)
- [ ] Scopes: `api read_user`
- [ ] Self-host: `gitlab_with_baseurl(base_url)` support
- [ ] PAT support: `glpat-...` >= 20 chars accepted
- [ ] Refresh: `expiresIn` from response
- [ ] OAuth unit test passes
- [ ] Refresh mock test passes
- [ ] E2E test passes

#### 16. codebuddy

- [ ] Device Code
- [ ] Init URL: `https://copilot.tencent.com/v2/plugin/auth/state`
- [ ] Token URL: `https://copilot.tencent.com/v2/plugin/auth/token`
- [ ] Custom clientId (dynamic)
- [ ] Scopes: `read`
- [ ] Headers: `User-Agent: CLI/2.63.2 CodeBuddy/2.63.2`, `Platform: CLI`
- [ ] Poll interval: 5s
- [ ] Refresh URL: `https://copilot.tencent.com/v2/plugin/auth/token/refresh`
- [ ] Refresh: `expiresIn` from response
- [ ] OAuth unit test passes
- [ ] Refresh mock test passes
- [ ] E2E test passes

### Phase 1.5 — Cursor import

#### 17. cursor

- [ ] Import Token only (no OAuth flow)
- [ ] SQLite path: OS-specific (`cursor.db`)
- [ ] Key: `cursorAuth/accessToken`
- [ ] Key: `storage.serviceMachineId`
- [ ] Checksum algorithm: jyh cipher (XOR timestamp bytes with rolling key init=165)
- [ ] Checksum output: base64 → `{encoded_ts},{machineId}`
- [ ] Headers: `x-cursor-checksum`, `x-cursor-client-version: 3.1.0`, `x-cursor-client-type: ide`, `x-cursor-client-os`, `x-cursor-client-arch`, `x-cursor-client-device-type: desktop`, `x-ghost-mode`
- [ ] Token TTL: 24h (no refresh — user re-imports)
- [ ] OAuth unit test passes (import service)
- [ ] E2E test passes

---

## Token dedup (Phase 1.4)

- [ ] `dedupRefresh` implemented with `Mutex<HashMap<String, DedupEntry>>`
- [ ] Key format: `${provider}:${oldRefreshToken}`
- [ ] In-flight dedup: 2nd refresh within 10s returns in-flight promise
- [ ] Cache TTL: 10 seconds after completion
- [ ] Prevents `refresh_token_reused` Auth0 revocation

---

## OAuth refactor verification (Phase 1.1)

- [ ] `src/oauth/providers.rs` created with all 14 PKCE/device-code provider constants
- [ ] Single struct `OAuthProviderConfig { client_id, authorize_url, token_url, refresh_url?, scopes, uses_pkce, extra_params, fixed_port?, callback_path?, content_type }`
- [ ] Conflicting constants removed from `src/oauth/mod.rs` and `src/server/api/oauth.rs`
- [ ] Claude URL changed from `auth.claude.ai` to `claude.ai/oauth/authorize`
- [ ] Codex URL changed from `codex.ai` to `auth.openai.com/oauth/authorize`
- [ ] GitHub scope fixed: `read:user` (not `read:user repo`)
- [ ] Kiro URL corrected (wrong host entirely)

---

## Cross-cutting

- [ ] All OAuth URLs have `// Port of <9router path>` comments
- [ ] `// DO NOT CHANGE — value pinned to 9router v0.4.66-1-gcce8a50` on each clientId
- [ ] Per-provider OAuth unit test: constructs authorize URL, asserts host + clientId + scopes
- [ ] Per-provider refresh mock test: asserts HTTP method, content-type, body shape
- [ ] All 17 providers pass E2E: `curl /v1/chat/completions` via mock upstream
- [ ] Combo fallback: each provider returns 429 -> fallback picks next combo member
- [ ] `claude_settings.rs` uses `ANTHROPIC_AUTH_TOKEN` (NOT `ANTHROPIC_API_KEY`)
- [ ] `codex` settings: `OPENAI_BASE_URL` without `/v1` suffix, `~/.codex/config.json` lowercase keys
- [ ] `cursor` settings: guide only, notes "Requires Cursor Pro + tunnel/cloud endpoint"
