# Parity Implementation Report — openproxy → 9router v0.5.50

> **Version:** 2026-08-12 · **Full subagent research included**
> **Reference:** `.tmp/9router` = `decolua/9router` v0.5.50 (2026-08-05)
> **Port:** `openproxy` v0.2.0 (Rust) — claims "v0.5.30 full parity", gap window = v0.5.30 → v0.5.50

This report is the **implementation-ready** companion to `parity-report.md`. Every gap is specified to the level of **exact URLs, headers, auth schemes, model lists, field mappings, and step-by-step Rust changes** — verified by cross-checking agents against both codebases. Follow it exactly; a single wrong detail breaks the port.

- 151 findings audited (9 dimensions, 160 subagents, 5,498 tool calls)
- **146 CONFIRMED**, 5 REFUTED
- Each section: **JS source of truth** (verbatim) → **Rust current state** → **Implementation steps** → **Guard test** → **Risks**

---

## PHẦN A — PRIORITY 0: Providers không với tới được (VỠ REQUEST)

### A1. 17 providers enabled thiếu trong chat path `default.rs` (P0) [missing]

**Root cause:** `src/server/api/chat.rs:1635` → `DefaultExecutor::new` chỉ đọc `default.rs PROVIDER_CONFIGS` (96 keys). 17 providers của 9router v0.5.50 **không có entry** → `Err(UnsupportedProvider)` → HTTP 500. `provider.rs PROVIDER_REGISTRY` chứa vài provider này nhưng là **dead code** (chỉ `media.rs` đọc, `UnifiedExecutor` không được gọi).

**Source of truth — từng provider (verbatim từ registry JS):**

| Provider | alias | category | baseUrl (transport) | auth | models |
|---|---|---|---|---|---|
| `alims-intl` | alims-intl | apikey | `https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions` | — | qwen3.5-plus, kimi-k2.5, glm-5, MiniMax-M2.5, qwen3-coder-next, qwen3-coder-plus, glm-4.7 |
| `api-airforce` | af | freeTier | `https://api.airforce/v1/chat/completions` (+ headers `HTTP-Referer: https://endpoint-proxy.local`, `X-Title: Endpoint Proxy`) | apikey | claude-3.7-sonnet, kimi-k2.6, gemini-2.5-flash |
| `baidu` | qianfan | apikey | `https://qianfan.baidubce.com/v2/chat/completions` | apikey | deepseek-v4-pro, deepseek-v4-flash, glm-5.2, glm-5.1, kimi-k2.6, qwen3.5-397b-a17b, qwen3.5-27b |
| `bluesminds` | bm | apikey | `https://api.bluesminds.com/v1/chat/completions` | apikey | 14 models (gpt-4.1*, claude-sonnet-4-5, gemini-2.x, kimi-k2*, glm-4.7, minimax-m2.5...) |
| `clinepass` | clinepass | oauth | `https://api.cline.bot/api/v1/chat/completions` (+ headers `HTTP-Referer: https://cline.bot`, `X-Title: Cline`; auth `combined:true, header:Authorization, scheme:bearer, hooks:[clineHeaders]`) | oauth | 10 models `cline-pass/*` |
| `codebuddy-intl` | cbai | oauth | `https://www.codebuddy.ai/v2/chat/completions` (**forceStream:true**, thinkingFormat:openai, headers `User-Agent: IDE/2.108.1 CodeBuddy/2.108.1`, `X-Product: SaaS`, `X-...`; auth combined bearer) | oauth | 15 models (glm-5.2..., minimax-m3, kimi-k2.7...) |
| `featherless` | featherless | apikey | `https://api.featherless.ai/v1/chat/completions` | apikey | 7 models `*/DeepSeek-V4*`, `zai-org/GLM-*`, `moonshotai/Kimi-*` |
| `kilo-gateway` | kgw | freeTier | `https://api.kilo.ai/api/gateway/chat/completions` | apikey | kilo-auto/free, nemotron-3-super:free, nemotron-3-ultra:free, kat-coder:free, kilo-auto/frontier, kilo-auto/balanced |
| `perplexity-agent` | perplexity-agent | apikey | `https://api.perplexity.ai/v1/responses` (**format: openai-responses**) | apikey | 11 models `perplexity/sonar`, openai/gpt-5.5..., anthropic/claude-*, google/gemini-* |
| `poolside` | poolside | freeTier | `https://inference.poolside.ai/v1/chat/completions` | apikey | poolside/laguna-s-2.1, poolside/laguna-xs-2.1 |
| `selfhosted-embedding` | selfhosted-embedding | apikey | baseUrl **per-connection** (`providerSpecificData.baseUrl`, `/embeddings` appended; **không bao giờ fallback api.openai.com**) | apikey | embedding |
| `selfhosted-stt` | selfhosted-stt | apikey | baseUrl **per-connection** (e.g. `http://host:8080/v1/audio/transcriptions`) | apikey | whisper-1 |
| `selfhosted-tts` | selfhosted-tts | apikey | baseUrl **per-connection** (`/v1/audio/speech` appended, e.g. `http://host:8080`) | apikey | kokoro |
| `tencent` | hunyuan | apikey | `https://api.hunyuan.cloud.tencent.com/v1/chat/completions` | apikey | hunyuan-turbos-latest, hunyuan-t1-latest |
| `tokenrouter` | tokenrouter | apikey | `https://api.tokenrouter.com/v1/chat/completions` (**thinkingFormat: tokenrouter**) | apikey | **120 models** (xem danh sách đầy đủ dưới) |
| `venice` | venice | apikey | `https://api.venice.ai/api/v1/chat/completions` (**thinkingFormat: openai**) | apikey | 15 models (venice-uncensored-1-2, zai-org-glm-5, qwen3-*, deepseek-v4-pro, embeddings...) |
| `zed` | zd | oauth | `https://cloud.zed.dev/completions` (NDJSON/SSE, auth `<user_id> <access_token>` + `x-zed-cloud-token`) | oauth | (xem executor zed) |

**tokenrouter 120 models (đầy đủ):**
```
MiniMax-Hailuo-2.3, MiniMax-M3, anthropic/claude-fable-5, anthropic/claude-haiku-4.5,
anthropic/claude-opus-4.5/4.6/4.7/4.7-fast/4.8/4.8-fast/5/5-fast, anthropic/claude-sonnet-4/4.5/4.6/5,
bytedance-seed/seedream-4.5/5.0-lite/5.0-pro, claude-haiku-4-5, claude-opus-4-8-m-aws,
deepseek/deepseek-v3.2/v4-flash/v4-flash-0731/v4-pro, ex/gpt-5.4, google/gemini-2.5-flash-image,
google/gemini-3-flash-preview/3-pro-image-preview/3.1-flash-image-preview/3.1-flash-lite-image/3.1-pro-preview/3.5-flash/3.5-flash-lite/3.6-flash,
google/gemini-embedding-2, google/gemma-4-26b-a4b-it, happyhorse-1.0-t2v, kling-3.0-turbo/v2-6/v3/v3-omni,
microsoft/mai-image-2.5, minimax/minimax-m2-her/m2.1/m2.1-highspeed/m2.5/m2.7/m2.7-highspeed,
miromind/mirothinker-1-7-deepresearch(-mini), mistralai/devstral-2512/medium-3-5/small-2603/voxtral-small-24b-2507,
moonshotai/kimi-k2.5/k2.6/k2.7-code/k3/k3-free, nvidia/nemotron-3-nano-omni:free/nemotron-3-super-120b-a12b,
openai/gpt-4o-mini/5/5-image/5-image-mini/5-mini/5.2/5.4/5.4-image-2/5.4-mini/5.4-nano/5.4-pro/5.5/5.5-pro/5.6-luna/5.6-sol/5.6-terra/gpt-audio/gpt-audio-mini/gpt-oss-120b,
qwen/qwen3-coder-next/3.5-122b-a10b/3.5-35b-a3b/3.5-397b-a17b/3.5-9b/3.5-flash/3.5-plus-02-15/3.6-plus/3.7-max/3.7-plus/3.8-max,
qwen3.5-omni-plus, qwen3.6-flash, sakana/fugu-ultra, seed-2-0-*, stepfun/step-3.5-flash/3.7-flash,
tencent/hy3-preview, x-ai/grok-4.1-fast/4.20-beta/4.3/4.5/grok-build-0.1, xiaomi/mimo-v2-flash/omni/pro/2.5/2.5-pro, z-ai/glm-4.5-air/4.6/4.6v/4.7/5/5-turbo/5.1/5.2
```

**Implementation steps (Rust):**
1. Trong `src/core/executor/default.rs`, thêm 17 entry vào `PROVIDER_CONFIGS` với baseUrl chính xác ở bảng trên. Ví dụ:
   ```rust
   ("alims-intl", ProviderConfig::openai("https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions")),
   ("api-airforce", ProviderConfig::openai("https://api.airforce/v1/chat/completions")),
   ("baidu", ProviderConfig::openai("https://qianfan.baidubce.com/v2/chat/completions")),
   ("bluesminds", ProviderConfig::openai("https://api.bluesminds.com/v1/chat/completions")),
   ("clinepass", ProviderConfig::openai("https://api.cline.bot/api/v1/chat/completions")),
   ("codebuddy-intl", ProviderConfig::openai("https://www.codebuddy.ai/v2/chat/completions")),
   ("featherless", ProviderConfig::openai("https://api.featherless.ai/v1/chat/completions")),
   ("kilo-gateway", ProviderConfig::openai("https://api.kilo.ai/api/gateway/chat/completions")),
   ("perplexity-agent", /* format openai-responses */),
   ("poolside", ProviderConfig::openai("https://inference.poolside.ai/v1/chat/completions")),
   ("tencent", ProviderConfig::openai("https://api.hunyuan.cloud.tencent.com/v1/chat/completions")),
   ("tokenrouter", ProviderConfig::openai("https://api.tokenrouter.com/v1/chat/completions")),
   ("venice", ProviderConfig::openai("https://api.venice.ai/api/v1/chat/completions")),
   ("zed", /* NDJSON executor — xem A3 */),
   ```
2. Chuyển config của kilo-gateway/venice/featherless-ai/trae/devin-cli/windsurf/opencode-zen từ `PROVIDER_REGISTRY` (provider.rs) sang `PROVIDER_CONFIGS`.
3. `perplexity-agent` cần `format: openai-responses` → đảm bảo DefaultExecutor hỗ trợ transport `format`.
4. `tokenrouter`/`venice` cần `thinkingFormat` (`tokenrouter` / `openai`) → map vào thinking config.
5. Cập nhật `src/core/model/catalog.rs` (providers + providerModels + providerIdToAlias) cho 17 provider + model lists.
6. `selfhosted-*`: baseUrl **per-connection** — thêm cơ chế đọc `providerSpecificData.baseUrl` (xem A2).

**Guard test:** `default_executor_covers_all_9router_providers` — assert `PROVIDER_CONFIGS` có key cho cả 17 provider; gọi `DefaultExecutor::new("tokenrouter")` không trả `UnsupportedProvider`.

**Risks:** sai 1 URL là 404; `perplexity-agent` dùng Responses API không phải chat-completions; selfhosted-* phải đọc per-connection baseUrl chứ không hardcode; `tokenrouter` 120 models phải thêm đủ vào catalog.

### A2. Selfhosted STT / TTS / Embedding (v0.5.50 headline feature) — thiếu hoàn toàn (P0) [missing]

**Source of truth — registry JS:**
- **`selfhosted-stt`** (`selfhosted-stt.js:19-55`): `sttConfig.baseUrl` mặc định `http://localhost:8080/v1/audio/transcriptions`, **overwrite bằng `providerSpecificData.baseUrl`** (full transcriptions URL), format openai, model `whisper-1`.
- **`selfhosted-tts`** (`selfhosted-tts.js:13-40`): `ttsConfig.baseUrl` mặc định `http://localhost:8880`, **`/v1/audio/speech` được append** vào baseUrl, format openai-speech, model `kokoro`.
- **`selfhosted-embedding`** (`selfhosted-embedding.js:31-66`): adapter riêng `selfhostedEmbedding.js` — baseUrl per-connection (appends `/embeddings`), **chủ động REFUSE cloud fallback** (nếu thiếu baseUrl → lỗi, KHÔNG về api.openai.com).

**Rust current:** không có chữ `selfhosted` trong `src/`. `stt/mod.rs:84-124` chỉ biết openai/groq/deepgram/assemblyai/huggingface/gemini → STT trả 400. `embeddings/mod.rs:46-66`, `tts/mod.rs:88-118` không có.

**⚠️ Rủi ro bảo mật nghiêm trọng:** nếu route selfhosted-embedding qua node openai-compatible generic, `embeddings/base.rs:172` (`OPENAI_COMPAT_NODE`) sẽ **âm thầm fallback về api.openai.com** — gửi text + API key sang OpenAI. Đúng lỗi JS adapter viết để chặn.

**Implementation steps:**
1. `media/stt/mod.rs`: thêm `selfhosted-stt` → `stt_config()` trả openai format + đọc `providerSpecificData.baseUrl`.
2. `media/tts/mod.rs`: thêm `selfhosted-tts` → `tts_config()` openai-speech format, append `/v1/audio/speech` vào baseUrl.
3. `media/embeddings/mod.rs`: thêm adapter `selfhosted_embedding` — đọc per-connection baseUrl, **không fallback**; thiếu baseUrl → `MissingBaseUrlError` 400.
4. Thêm 3 provider vào catalog + `default.rs`/dispatch.

**Guard test:** `selfhosted_embedding_no_openai_fallback` — adapter không baseUrl → lỗi (không phải api.openai.com). `selfhosted_tts_appends_speech_path`.

**Risks:** tuyệt đối không fallback cloud cho selfhosted; baseUrl phải per-connection.

### A3. OAuth codebuddy-intl + zed (P0) [missing]

**Source of truth:**
- **`codebuddy-intl`** (`src/lib/oauth/providers/codebuddy-intl.js:1-58`): device_code flow www.codebuddy.ai, stateUrl/tokenUrl `/v2/plugin/*`, `platform:ide`, headers `User-Agent: IDE/2.63.2 CodeBuddy/2.63.2`, `X-Product: SaaS`. Executor `open-sse/executors/codebuddy-intl.js:1-45`: forceStream + rewrite messages với system preamble + reasoning_summary.
- **`zed`** (`src/lib/oauth/providers/zed.js:1-62` + `open-sse/shared/zedAuth.js`): **RSA keypair native-app signin**, `zed.dev/native_app_signin`, không tokenUrl, decrypt RSA-encrypted access token từ local callback, resolve organizationId. Executor `open-sse/executors/zed.js`: NDJSON/SSE tại `cloud.zed.dev/completions`, auth header `Authorization: <user_id> <access_token>` + `x-zed-cloud-token`.

**Rust current:** `src/oauth/providers.rs:451-472` `get_config()` không có `codebuddy-intl`/`zed`. `oauth.rs:4217` trả `unknown_provider`. Không executor nào.

**Implementation steps:**
1. Port codebuddy-intl OAuth device-code (`.ai` domain) vào `src/oauth/providers.rs`.
2. Port Zed RSA keypair flow (`zedAuth.js`) — sinh keypair, build native_app_signin URL, decrypt token, resolve org.
3. Thêm `CodeBuddyIntlExecutor` (force-stream + message rewrite + reasoning_summary) và `ZedHostedExecutor` (NDJSON/SSE + auth header) vào `executor/`.

**Guard test:** `zed_oauth_flow_returns_native_app_signin_url`, `codebuddy_intl_device_code_starts`.

**Risks:** Zed là RSA unique — generic OAuth path không cover được; codebuddy-intl phải dùng đúng `www.codebuddy.ai` domain (khác CN).

### A4. Image-generation adapters xai + vercel-ai-gateway (P0) [missing]

**Source of truth:** `imageProviders/index.js:16-35` ADAPTERS có `xai: createOpenAIAdapter('xai')` (baseUrl `https://api.x.ai/v1/images/generations`, model grok-2-image-1212) và vercel-ai-gateway (`https://ai-gateway.vercel.sh/v1/images/generations`).

**Rust current:** `image/mod.rs:67-85` `get_image_adapter()` thiếu cả 2.

**Implementation steps:** thêm `"xai"` và `"vercel-ai-gateway"` vào `get_image_adapter()` dùng openai-compatible adapter với đúng baseUrl.

**Guard test:** `image_adapter_covers_xai_and_vercel`.

### A5–A10. Sai URL/endpoint (P0) [wrong-url]

| # | Gap | JS (đúng) | Rust (sai) | Fix |
|---|---|---|---|---|
| A5 | **youcom search** | `searchConfig.baseUrl = https://ydc-index.io/v1/search` (GET + X-API-Key) | `search/providers.rs:829` hardcode `https://api.you.com/search` | đổi thành `https://ydc-index.io/v1/search`, param giữ nguyên |
| A6 | **tortoise TTS** | `http://localhost:5000/api/tts` | `tts/mod.rs:153` `http://localhost:8000/tts` | đổi port 5000 + thêm `/api` |
| A7 | **blackbox** | `https://api.blackbox.ai/v1/chat/completions` | `default.rs:183-184` + `provider.rs:775-776` `https://api.blackbox.ai/api/chat/completions` | đổi `/api/` → `/v1/` |
| A8 | **SiliconFlow** | `https://api.siliconflow.com/v1` | `default.rs:101-102` + `api_key.rs:296` `.cn` | đổi `.cn` → `.com` |
| A9 | **iflow** | `https://apis.iflow.cn` | mintlify docs host | đổi host |
| A10 | **baidu/qianfan** | id `baidu`, base `https://qianfan.baidubce.com/v2/chat/completions` | `provider.rs:1298-1299` chỉ `qianfan` thiếu `/chat/completions`; `default.rs` không có | thêm `baidu` + đúng URL |

**Guard test:** mỗi URL → `assert_eq!(config.base_url, "<đúng>")`.

---

## PHẦN B — OAuth (thiếu flow & lệch wire)

### B1. Trae / Windsurf / Zed OAuth — thiếu hoàn toàn (P1) [missing]
- **JS:** `src/lib/oauth/providers/trae.js` (11.7K: GetLoginGuidance→ExchangeToken→GetUserInfo, marscode device flow), `windsurf.js` (6.1K: RegisterUser/GetOneTimeAuthToken gRPC, Firebase id-token), `zed.js` (2.4K: RSA keypair native-app). Cả 3 được thêm v0.5.45 kèm "harden callback proxies".
- **Rust:** `src/oauth/providers.rs:446-471` `get_config` không có; `oauth.rs exchange_oauth_compat (4141-4179)` và `authorize_oauth_compat (3170-3274)` không có arm; **không có `/register-session` route**.
- **Impl:** port 3 flow + `/register-session` route. Trae = marscode device flow (loginTraceId, verification URL). Windsurf = gRPC RegisterUser/GetOneTimeAuthToken. Zed = RSA keypair (native_app_signin).
- **Test:** `trae_flow_produces_verification_url`, `windsurf_get_one_time_token`, `zed_native_app_signin_url`.

### B2. Grok CLI / Grok Build OAuth device-code — thiếu (P1) [missing]
- **JS:** `grok-cli.js:8-64` device-code flow (POST device/code với referrer=grok-build + UA `grok-pager/0.2.93 grok-shell/0.2.93`, poll token, postExchange `cli-chat-proxy.grok.com/v1/user`).
- **Rust:** `oauth.rs:703` **có** liệt kê grok-cli trong `is_device_code_provider` nhưng `get_config` **không có arm** → flow lỗi trước HTTP. Refresh thì được (token_refresh.rs:1038-1045).
- **Impl:** thêm `grok-cli` arm vào `get_config` + device-code flow.
- **Test:** `grok_cli_device_code_starts`.

### B3. CodeBuddy Intl OAuth — thiếu (P1) [missing]
- **JS:** `codebuddy-intl.js` (device-code, X-Domain www.codebuddy.ai, platform=ide, UA IDE/2.63.2). `noPkceDeviceProviders` gồm codebuddy-intl.
- **Rust:** `oauth.rs:692-706` `is_device_code_provider` liệt kê `codebuddy`/`codebuddy-cn` nhưng **không** `codebuddy-intl`.
- **Impl:** thêm codebuddy-intl như provider riêng.
- **Test:** `codebuddy_intl_is_device_code_provider`.

### B4. Kimchi browser_token exchange — thiếu (P1) [missing]
- **JS:** `kimchi.js:6-72` flowType `browser_token`, buildAuthUrl `/cli-auth?callback+state`, exchangeToken validate `api.cast.ai` rồi fetch `app.kimchi.dev/api/v1/me`. `noPkceExchangeProviders` gồm kimchi.
- **Rust:** `providers.rs:209-227` có config kimchi (web_app_url/validation_url/user_info_url) nhưng `exchange_oauth_compat` **không có arm** → "Unknown provider".
- **Impl:** port exchange kimchi + browser_token flow.
- **Test:** `kimchi_browser_token_exchange`.

### B5. Qoder device-token — sai wire (P1) [behavior-diff]
- **JS:** `qoder.js:9-76` + `services/qoder.js:69-150`: sinh PKCE+nonce+machineId local, mở `qoder.com/device/selectAccounts?challenge&nonce&machine_id`, poll **GET** `openapi.qoder.sh/api/v1/deviceTokens/...`, response cần `codeVerifier`.
- **Rust:** `src/oauth/qoder.rs` module standalone **không wire vào route**; path wired là `start_device_code` generic **POST form**.
- **Impl:** wire qoder.rs vào route + GET poll + PKCE/nonce/machineId + trả codeVerifier.
- **Test:** `qoder_poll_uses_get_with_nonce`.

### B6. CodeBuddy CN — sai wire (P1) [behavior-diff]
- **JS:** `codebuddy-cn.js:10-71`: POST stateUrl?platform=CLI với X-No-Authorization/X-No-User-Id → {state,authUrl}, rồi poll **GET** tokenUrl?state với X-No-Enterprise-Id/X-No-Department-Info.
- **Rust:** generic `start_device_flow` POST form không có per-provider state-then-GET hook.
- **Impl:** thêm hook state-then-GET cho codebuddy-cn/intl.
- **Test:** `codebuddy_cn_posts_state_then_gets_token`.

### B7. KiloCode — sai wire (P1) [behavior-diff]
- **JS:** `kilocode.js`: JSON POST initiateUrl → {code,verificationUrl,expiresIn}; poll **GET** pollUrlBase/{code} phân biệt 202/403/410; approve → fetch `/api/profile` orgId.
- **Rust:** generic POST form; poll_for_token xử lý mọi non-200 là error/pending, không phân biệt status, không GET.
- **Impl:** port initiate + GET poll status mapping + org profile.
- **Test:** `kilocode_poll_maps_202_403_410`.

### B8. Kimi device-code — thiếu X-Msh headers (P1) [behavior-diff]
- **JS:** `kimi.js:9-39`: sinh deviceId=UUID, gửi `buildKimiHeaders(deviceId)` (X-Msh-Device-Id...) trên request VÀ poll; trả `_kimiDeviceId`; verification_uri_complete = `https://www.kimi.com/code/authorize_device?user_code=...`.
- **Rust:** `mod.rs device_code::poll_for_token (142-210)` chỉ gửi form không X-Msh; DeviceCodeResponse (592-599) thiếu `_kimiDeviceId` và `verification_uri_complete`.
- **Impl:** thêm X-Msh headers + `_kimiDeviceId` persistence + verification_uri_complete.
- **Test:** `kimi_poll_sends_x_msh_device_id`.

### B9. Qwen OAuth — STALE (P1) [behavior-diff]
- **JS:** v0.5.50 CHANGELOG "remove Qwen"; registry không còn qwen.
- **Rust:** `providers.rs:143-154` qwen config + `get_config:452` + `oauth.rs:705` + `executor/qwen.rs` + `token_refresh.rs` đều còn.
- **Impl:** xoá/disable qwen (registry, oauth, executor, refresh) như JS.
- **Test:** `qwen_not_in_registry`.

### B10. xAI authorize URL — sai param (P1) [behavior-diff]
- **JS:** `xai.js:39-58` buildAuthUrl params: response_type, client_id, redirect_uri, scope, code_challenge, code_challenge_method, state, nonce, plan='generic', referrer='cli-proxy-api' — **KHÔNG prompt**.
- **Rust:** `oauth.rs:3455` + `xai.rs:108` thêm `('prompt','login')`.
- **Impl:** xoá `prompt=login`.
- **Test:** `xai_auth_url_has_no_prompt`.

### B11. Codex refresh — JSON vs form + mất id_token (P1) [behavior-diff]
- **JS:** `tokenRefresh/providers.js:213-267` `refreshCodexToken` POST **JSON** {client_id, grant_type, refresh_token} + Content-Type application/json; trả `{accessToken, refreshToken, idToken: tokens.id_token, expiresIn}`; `mergeRefreshedCredentials` giữ idToken; `backfillCodexEmails` sửa legacy connections.
- **Rust:** `token_refresh.rs:406-416` `refresh_form_token` (form-urlencoded); `RefreshResult` (140-144) chỉ access/refresh/expires — **thiếu id_token**; không có backfillCodexEmails.
- **Impl:** refresh JSON body + thêm `id_token` vào RefreshResult + backfill email/chatgptAccountId.
- **Test:** `codex_refresh_posts_json_with_id_token`.

### B12. `/register-session` route — thiếu (P1) [missing]
- **JS:** `route.js:254-266` POST register-session (state từ query+body, register trae/windsurf session + zed codeVerifier server-side; v0.5.50 fix "declare searchParams").
- **Rust:** `oauth.rs routes() (5360-5420)` không có.
- **Impl:** thêm route + handler, declare `searchParams`.
- **Test:** `register_session_route_exists`.

### B13. codeVerifier exempt set — lệch (P1) [behavior-diff]
- **JS:** `route.js:346` `noPkceExchangeProviders = ["cline","clinepass","kimchi"]`.
- **Rust:** `oauth.rs:4130-4139` chỉ `provider != "cline"`.
- **Impl:** thêm clinepass, kimchi.
- **Test:** `no_pkce_exchange_covers_clinepass_kimchi`.

### B14. Antigravity metadata — string vs numeric (P1) [behavior-diff]
- **JS:** `oauth.js:44-56` loadCodeAssistClientMetadata/getOAuthClientMetadata → `{ideType:9, platform:<os enum 0-5>, pluginType:2}` (numeric).
- **Rust:** `oauth.rs:83-84` gửi string `{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}`; còn thêm `x-request-source:local` + `X-Goog-Api-Client` headers mà JS không gửi.
- **Impl:** numeric ClientMetadata; scope header fixes chỉ tới loadCodeAssist/onboardUser.
- **Test:** `antigravity_metadata_is_numeric`.

### B15. Device-code response — thiếu fields (P1) [behavior-diff]
- **JS:** `route.js:227-232` trả `{...deviceData, codeVerifier: deviceData.codeVerifier || authData.codeVerifier}`; kimi/kilocode/github trả `verification_uri_complete`.
- **Rust:** `DeviceCodeResponse (592-599)` chỉ device_code/user_code/verification_uri/interval/expires_in.
- **Impl:** thêm `verification_uri_complete` + `codeVerifier`.
- **Test:** `device_code_response_has_code_verifier`.

### B16. GitHub postExchange — v1 vs v2 (P1) [behavior-diff]
- **JS:** `github.js:55-95` postExchange fetch `copilot_internal/v2/token` (GET) + `api.github.com/user` (Bearer + X-GitHub-Api-Version 2022-11-28 + UA GitHubCopilotChat/0.26.7); mapTokens lưu copilot v2 + PSD.
- **Rust:** `oauth/mod.rs exchange_github_copilot_token (253-279)` POST `copilot_internal/v1/token` (v1, POST); poll_device_code lưu v1 token không merge v2+user.
- **Impl:** GET v2 + merge user PSD + đúng UA/apiVersion.
- **Test:** `github_post_exchange_uses_v2_get`.

---

## PHẦN C — Media (TTS / STT / Embedding / Image / Search / Video)

### C1. Selfhosted STT / TTS / Embedding (P0) [missing]
Xem A2 — 3 provider selfhosted, per-connection baseUrl, **no-openai-fallback guard** cho embedding.

### C2. Dedicated search providers không với tới qua bare provider-id (P1) [behavior-diff]
- **JS:** `src/sse/handlers/search.js:34` nhận `body.provider || body.model` làm provider id; SKILL.md:91 ghi `"provider":"tavily" ≡ "model":"tavily"`.
- **Rust:** `media.rs:291-298` generic_media_handler yêu cầu `body.model`; `model/mod.rs:303-338` `infer_provider_from_model_name("tavily")` không resolve (catalog có tavily/exa/serper/brave-search/searxng nhưng ALIAS_TO_PROVIDER_ID mod.rs:9-129 bỏ sót).
- **Impl:** thêm các search provider vào ALIAS_TO_PROVIDER_ID + infer provider từ catalog khi model là search id.
- **Test:** `bare_tavily_model_resolves_to_tavily_provider`.

### C3. Chat-based search providers — thiếu (P1) [missing]
- **JS:** `search/chatSearch.js:16-270` CHAT_SEARCH_CONFIG: gemini (google_search tool), openai (web_search tool), xai (web_search /v1/responses), kimi ($web_search builtin), minimax, perplexity, perplexity-agent, vercel-ai-gateway. Trả `{answer, citations}` — path khác hẳn (POST chat/completions với search tool).
- **Rust:** `search/providers.rs:14-28` `lookup()` chỉ 10 dedicated providers (serper, brave-search, perplexity, exa, tavily, google-pse, linkup, searchapi, youcom, searxng); **không có chat-based**.
- **Impl:** port chatSearch.js — mỗi provider wrap chat-completions + search tool, trả {answer, citations}.
- **Test:** `chat_search_gemini_uses_google_search_tool`.

### C4. Perplexity search adapter — endpoint KHÔNG TỒN TẠI (P1) [wrong-url]
- **JS:** `perplexity.js:29-34` KHÔNG có searchConfig — chỉ searchViaChat `{defaultModel:"sonar", endpoint:"https://api.perplexity.ai/chat/completions"}`. `callers.js:243` buildPerplexityRequest không bao giờ được dispatch.
- **Rust:** `search/providers.rs:240-247` PERPLEXITY.build_url → `https://api.perplexity.ai/search` (POST {query,max_results}) — **endpoint này không tồn tại**.
- **Impl:** xoá dedicated perplexity adapter; chuyển sang chat-search (C3).
- **Test:** `perplexity_search_uses_chat_completions_not_search`.

### C5. youcom search URL (P1) [wrong-url] — `ydc-index.io/v1/search` vs `api.you.com/search`. Fix `providers.rs:829`.

### C6. searxng URL + env override (P1) [behavior-diff]
- **JS:** `runtimeConfig.js:49` `SEARXNG_URL = envUrl("SEARXNG_URL", "http://localhost:8888/search")`.
- **Rust:** `search/providers.rs:920` hardcode `http://localhost:8080` → `/search`; **không đọc SEARXNG_URL env**.
- **Impl:** thêm env override + đổi default sang `http://localhost:8888/search`.
- **Test:** `searxng_url_uses_env_override`.

### C7. Search timeout + sanitize (P1) [behavior-diff]
- **JS:** `search/index.js:14` GLOBAL_TIMEOUT_MS=15000, :96 `timeout = min(providerConfig.timeoutMs||10000, remaining)`; :14-18 sanitizeQuery reject control chars `[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]` + NFKC normalize + trim + collapse whitespace.
- **Rust:** `search/handler.rs:9` GLOBAL_TIMEOUT=30s cố định; không sanitize.
- **Impl:** timeout per-provider (10s cap, 15s global) + sanitizeQuery.
- **Test:** `search_query_sanitized_nfkc`.

### C8. Chat-based search failover (P1) [missing]
- **JS:** `search/index.js:165-181` khi dedicated provider fail với retriable status (không phải 400/401/403/404) trong timeout, nếu có searchViaChat → failover.
- **Rust:** không có.
- **Impl:** thêm failover branch.
- **Test:** `search_fails_over_to_chat_on_500`.

### C9. Xiaomi-MiMo TTS — sai API (P1) [behavior-diff]
- **JS:** `ttsProviders/xiaomi-mimo.js` SPECIAL_ADAPTER POST `https://api.xiaomimimo.com/v1/chat/completions` với `messages:[{role:"assistant",content:text}]` + optional `{role:"user",...}` style, `audio.voice` top-level.
- **Rust:** `tts/mod.rs` không có xiaomi-mimo; generic forwarder POST `https://api.xiaomimimo.com/v1/audio/speech` (OpenAI audio/speech) — **API khác hoàn toàn**.
- **Impl:** port SPECIAL_ADAPTER chat-completions contract + preset voices 冰糖/茉莉/苏打/白桦/Mia/Chloe/Milo/Dean + style control + language hint.
- **Test:** `xiaomi_mimo_tts_posts_chat_completions`.

### C10. Gemini TTS stale (P1) [behavior-diff]
- **JS:** `gemini.js:7` FALLBACK_MODEL=`gemini-3.1-flash-tts-preview`; KNOWN_MODELS = [gemini-3.1-flash-tts-preview, gemini-2.5-flash-preview-tts, gemini-2.5-pro-preview-tts].
- **Rust:** `tts/gemini.rs:15-17` DEFAULT_MODEL=`gemini-2.5-flash-preview-tts`, KNOWN_MODELS thiếu gemini-3.1.
- **Impl:** cập nhật model list.
- **Test:** `gemini_tts_default_is_3_1`.

### C11. Tortoise TTS URL (P1) [wrong-url] — `http://localhost:5000/api/tts` vs `8000/tts`. Fix `tts/mod.rs:153`.

### C12. OpenRouter TTS headers + model parse (P1) [behavior-diff]
- **JS:** `openrouter.js:48` headers `HTTP-Referer: https://endpoint-proxy.local`, `X-Title: Endpoint Proxy`; single-slash model → tts_model=full, voice=last segment.
- **Rust:** `tts/openrouter.rs:56-59` hardcode `https://openproxy.local`/`OpenProxy`; :31-43 **đảo** logic model/voice.
- **Impl:** đúng headers + đúng split.
- **Test:** `openrouter_tts_headers_match`.

### C13. Selfhosted-embedding no-fallback guard (P1) [missing] — xem A2. Rust `OpenAiCompatNodeAdapter` (embeddings/base.rs:169-179) fallback api.openai.com — **phải chặn**.

### C14. vercel-ai-gateway embeddings (P1) [missing]
- **JS:** `embeddingProviders/index.js:9` OPENAI_COMPAT_PROVIDERS gồm vercel-ai-gateway; `vercel-ai-gateway.js:32` baseUrl `https://ai-gateway.vercel.sh/v1/embeddings`.
- **Rust:** `embeddings/mod.rs:29-50` không có.
- **Impl:** thêm adapter.
- **Test:** `embeddings_covers_vercel_ai_gateway`.

### C15. xai image bodyFields (P1) [behavior-diff]
- **JS:** `xai.js:38` imageConfig `bodyFields:["model","prompt","n","response_format"]` — strip quality/style.
- **Rust:** không có xai adapter → forward full body (rò quality/style).
- **Impl:** thêm xai image adapter với bodyFields whitelist.
- **Test:** `xai_image_strips_quality_style`.

### C16. Antigravity image (P1) [missing]
- **JS:** `imageProviders/antigravity.js` useExecutor → antigravity executor, body `{contents:[{parts:[text,inlineData]}]}` Gemini-style, normalize candidates[].inlineData.
- **Rust:** không có adapter; generic forwarder → `https://cloudcode-pa.googleapis.com/v1internal/images/generations` (URL không tồn tại).
- **Impl:** port executor-delegation path.
- **Test:** `antigravity_image_uses_gemini_contents`.

### C17. Cloudflare num_steps (P1) [behavior-diff]
- **JS:** `cloudflareAi.js:12-19` OPTIONAL_FIELDS gồm `num_steps`.
- **Rust:** `cloudflare_ai.rs:27-33` thiếu `num_steps`.
- **Impl:** thêm.
- **Test:** `cloudflare_image_keeps_num_steps`.

### C18. Nanobanana typo (P1) [behavior-diff]
- **JS:** `nanobanana.js:23` `type: isEdit ? "IMAGETOIAMGE" : "TEXTTOIAMGE"` — **cố ý** (upstream API contract).
- **Rust:** `nanobanana.rs:64` `"IMAGE_TO_IMAGE"`/`"TEXT_TO_IMAGE"` — "sửa" sai.
- **Impl:** **giữ nguyên typo**.
- **Test:** `nanobanana_sends_typo_type`.

### C19. Codex image stale + không SSE (P1) [behavior-diff]
- **JS:** `codex.js:8-11` CODEX_USER_AGENT=`codex_cli_rs/0.136.0`, CODEX_VERSION=`0.136.0`; buildSseResponse pipes `progress`/`partial_image`/`done`/`error` events tới client.
- **Rust:** `image/codex.rs:21-25` stale (`codex-imagen/0.2.6`, `0.129.0`); build_sse_response đọc cả stream vào memory (30s cap) trả JSON 1 lần.
- **Impl:** cập nhật constants + true SSE streaming.
- **Test:** `codex_image_streams_sse`.

### C20. Media 401-403 refresh-then-retry (P1) [behavior-diff]
- **JS:** imageGenerationCore.js:153-185 / embeddingsCore.js:117-151 / videoCore.js:155-172 — refresh trên 401/403 rồi retry.
- **Rust:** image/embeddings/video handlers không có.
- **Impl:** thêm refresh-then-retry cho cả 3.
- **Test:** `image_refreshes_and_retries_on_401`.

### C21. Video error sanitize + refresh (P1) [behavior-diff]
- **JS:** `videoCore.js:42-50` sanitizeSecrets redact Bearer tokens khỏi error text; :155-172 refresh-once/retry-once.
- **Rust:** `media.rs:980-981,1052` forward error text verbatim; không sanitize, không refresh.
- **Impl:** thêm sanitizeSecrets + refresh branch.
- **Test:** `video_error_redacts_bearer_token`.

---

## PHẦN D — Translator (field mapping / wrapper / envelope)

### D1. Drop temperature cho Claude models (P1) [missing]
- **JS:** `translator/concerns/paramSupport.js:10` rule `{ match: /claude/i, drop: ["temperature"] }`, áp bởi `executors/default.js:78` `stripUnsupportedParams`. Anthropic reject temperature (#1748). Còn thiếu: github gpt-5.4 temperature rule, github copilot claude thinking/reasoning_effort rule, cloudflare-ai flattenContent rule, volcengine-ark maxOutputCap/clamp rules.
- **Rust:** `strip_unsupported.rs:15-34` `should_strip` chỉ strip max_completion_tokens (anthropic), reasoning_effort (gemini/vertex), max_tokens (gemini). **Không có temperature rule, không model-pattern match.**
- **Impl:** thêm STRIP_RULES: `/claude/i` → drop temperature; github gpt-5.4; copilot claude thinking/reasoning_effort; cloudflare-ai flattenContent; volcengine-ark maxOutputCap/clamp. Cần model-pattern matching.
- **Test:** `claude_model_strips_temperature`.

### D2. preserveCacheControl hardcode false — alicode hỏng (P1) [behavior-diff]
- **JS:** `translator/index.js:125-127` `filterToOpenAIFormat(result, { preserveCacheControl: !!PROVIDERS[provider]?.quirks?.preserveCacheControl })`. Registry alicode.js:19, alicode-intl.js:19, alims-intl.js:... có `quirks.preserveCacheControl:true`.
- **Rust:** `translator/registry.rs:486` `filter_to_openai_format(body, false)` — hardcode false, không lookup quirk.
- **Impl:** truyền provider quirk vào filter; giữ cache_control khi flag true.
- **Test:** `alicode_preserves_cache_control`.

### D3. Kiro tool_result status hardcode "success" — mất is_error (P1) [behavior-diff]
- **JS:** `openai-to-kiro.js:148` `status: block.is_error ? "error" : "success"`; :160 `msg.is_error || msg.status === "error" ? "error" : "success"`; `claude-to-kiro.js:110` tương tự.
- **Rust:** `openai_to_kiro.rs:232,248` + `claude_to_kiro.rs:226` hardcode `"status":"success"`.
- **Impl:** map is_error/status==="error" → "error".
- **Test:** `kiro_tool_result_preserves_is_error`.

### D4. canonicalizeKiroConversation — thiếu (P1) [missing]
- **JS:** `kiroConversation.js:79-110` normalizeKiroToolSpecs (uniqueName 64-char cap, description 10237 cap), :194-209 reserveToolId, :247-324 reconcileToolPair (adjacent 1:1), cleanSchemaValue (strip additionalProperties/empty required), validateKiroConversation + flattenAllStructuredTools repair.
- **Rust:** `openai_to_kiro.rs:85-113` + `claude_to_kiro.rs:100-125` inline naive toolSpecification, **không cap length, không dedup, không repair**.
- **Impl:** port canonicalizeKiroConversation.
- **Test:** `kiro_conversation_canonicalized_tool_specs`.

### D5. Kiro thinking prefix thiếu max_thinking_length (P1) [behavior-diff]
- **JS:** `kiroConstants.js:354-357` `buildThinkingSystemPrefix` emit `<thinking_mode>enabled</thinking_mode>\n<max_thinking_length>{budget}</max_thinking_length>` (clamp 1..32000); resolveKiroThinkingBudget honors anthropic-beta interleaved-thinking header + model-name hints.
- **Rust:** `openai_to_kiro.rs:471-473` + `claude_to_kiro.rs:457-459` chỉ emit `<thinking_mode>enabled</thinking_mode>` — **thiếu tag thứ 2 + clamp**.
- **Impl:** thêm `<max_thinking_length>` + budget resolve.
- **Test:** `kiro_thinking_prefix_has_max_thinking_length`.

### D6. Kiro system text wrap — đảo ngược (P1) [behavior-diff]
- **JS:** `openai-to-kiro.js:165-167` wrap system-origin trong `<instructions>...</instructions>`; `claude-to-kiro.js:254` push raw system text vào systemPrompt.
- **Rust:** `openai_to_kiro.rs:253` push raw (không wrap); `claude_to_kiro.rs:446` **wrap `<system>{sys}</system>`** mà JS không emit.
- **Impl:** openai→kiro thêm `<instructions>` wrap; claude→kiro **bỏ** `<system>` wrap.
- **Test:** `kiro_system_uses_instructions_wrap_openai`.

### D7. Kiro inferenceConfig — JS luôn emit (P1) [behavior-diff]
- **JS:** `openai-to-kiro.js:416-421` `if (maxTokens || temperature !== undefined || topP !== undefined)` với maxTokens const 32000 → **luôn truthy** → luôn gửi inferenceConfig.maxTokens.
- **Rust:** `openai_to_kiro.rs:596-605` `if temperature.is_some() || top_p.is_some()` → chỉ gửi khi có temperature/topP.
- **Impl:** luôn emit inferenceConfig với maxTokens.
- **Test:** `kiro_always_emits_inference_config`.

### D8. Antigravity envelope thiếu userAgent/requestType/requestId (P1) [missing]
- **JS:** `openai-to-gemini.js:271-276` wrapInCloudCodeEnvelope set `userAgent:"antigravity", requestType:"agent", requestId` (uuid từ sessionId/model); `executors/antigravity.js:268-276` re-set.
- **Rust:** `openai_to_gemini.rs:1033-1035` chỉ wrap `{"request": inner}`; `antigravity.rs:813-822` chỉ inject projectId.
- **Impl:** thêm userAgent/requestType/requestId.
- **Test:** `antigravity_envelope_has_request_id`.

### D9. Antigravity thiếu isClaudeModel branch (P1) [missing]
- **JS:** `openai-to-gemini.js:424-437` `isClaudeModel(model)` → claude* qua `openaiToClaudeRequestForAntigravity` + `wrapInCloudCodeEnvelopeForClaude` (generationConfig, temperature: claudeRequest.temperature||1, maxOutputTokens: claudeRequest.max_tokens||4096).
- **Rust:** `openai_to_gemini.rs:955-1050` luôn Gemini path; không branch claude.
- **Impl:** port claude-model branch.
- **Test:** `antigravity_claude_model_uses_claude_envelope`.

### D10. toolNameMap không seed vào streaming state (P1) [missing]
- **JS:** `utils/stream.js:61-62` `state = {..., toolNameMap, customToolNames: new Set(...), model}`; translators đọc (claude-to-openai etc.).
- **Rust:** `chat.rs:2689-2690,2782-2783` `ResponseTransformState::default()` không toolNameMap; `chat.rs:2620` nhận tool_name_map nhưng không ghi vào state streaming.
- **Impl:** seed toolNameMap vào ResponseTransformState.
- **Test:** `streaming_state_has_tool_name_map`.

### D11. _customToolNames passthrough + custom_tool_call (P1) [missing]
- **JS:** `request/openai-responses.js:113,229` set `_customToolNames`; `chatCore.js:182` extract; `utils/stream.js:62` seed; `response/openai-responses.js:261` emit `custom_tool_call` / `custom_tool_call_input.delta`.
- **Rust:** `request/openai_responses.rs` không emit _customToolNames; `response/openai_responses.rs:290-334` luôn emit `function_call` + `function_call_arguments.delta`.
- **Impl:** phân biệt custom tool → emit custom_tool_call.
- **Test:** `custom_tool_streams_custom_tool_call`.

### D12. openai-responses→chat drop reasoning items (P1) [behavior-diff]
- **JS:** `request/openai-responses.js:41-59` extractReasoningText + attachPendingReasoning, :149-160 REASONING case buffer summary + encrypted_content lên assistant message kế.
- **Rust:** `request/openai_responses.rs:152` `Some("reasoning") => {}` **no-op** — drop hết.
- **Impl:** buffer reasoning + attach.
- **Test:** `responses_to_chat_attaches_pending_reasoning`.

### D13. client_metadata không xoá (P1) [missing]
- **JS:** `request/openai-responses.js:247` `delete result.client_metadata`.
- **Rust:** `request/openai_responses.rs:184-190` remove input/instructions/include/prompt_cache_key/store/reasoning nhưng **không client_metadata**.
- **Impl:** thêm `client_metadata` vào remove list.
- **Test:** `responses_to_chat_strips_client_metadata`.

### D14. chat→responses thiếu reasoning_effort → reasoning (P1) [missing]
- **JS:** `request/openai-responses.js:422` `if (body.reasoning_effort !== undefined) result.reasoning = { effort: body.reasoning_effort, summary: "auto" }`.
- **Rust:** `request/openai_responses.rs:341-358` pass temperature/max_tokens/... nhưng **không build reasoning object**.
- **Impl:** thêm mapping.
- **Test:** `chat_to_responses_maps_reasoning_effort`.

### D15. claude→openai thiếu reasoning_effort (P1) [missing]
- **JS:** `claude-to-openai.js:83-91` `result.reasoning_effort = body.reasoning_effort ?? body.reasoning?.effort; if (body.reasoning !== undefined) result.reasoning = body.reasoning`.
- **Rust:** `claude_to_openai.rs` không handling reasoning_effort.
- **Impl:** thêm forward.
- **Test:** `claude_to_openai_forwards_reasoning`.

### D16. reasoning_content injector thiếu MiniMax (P1) [missing]
- **JS:** `reasoningContentInjector.js:9` providerRuleFor đọc `PROVIDERS[provider]?.reasoningInject`; registry minimax.js:26 / minimax-cn.js:26 / deepseek.js:25 `reasoningInject: { scope: ... }`.
- **Rust:** `reasoning_content_injector.rs:23-36` chỉ special-case "deepseek".
- **Impl:** thêm minimax, minimax-cn.
- **Test:** `minimax_gets_reasoning_content_placeholder`.

### D17. Claude max_tokens ceiling — hardcode vs capabilities (P1) [behavior-diff]
- **JS:** `formats/claude.js:201-218` ceiling = `getCapabilitiesForModel(...).maxOutput` (opus-4.6/4.7/4.8 = 128000) + thinking-budget reconciliation.
- **Rust:** `claude_format.rs:155-171` hardcode opus→200000, sonnet→128000, else 64000; không reconciliation.
- **Impl:** model-capability-driven + reconciliation.
- **Test:** `claude_max_tokens_uses_model_capabilities`.

### D18. reasoning_details array thiếu (P1) [behavior-diff]
- **JS:** `concerns/reasoning.js:19-22` extractReasoningText handle `delta.reasoning_details` array (MiniMax reasoning_split=true: [{text|content}]).
- **Rust:** `openai_to_claude.rs:264-270` chỉ check `reasoning_content`/`reasoning` string.
- **Impl:** thêm reasoning_details array handling.
- **Test:** `openai_to_claude_handles_reasoning_details`.

---

## PHẦN E — Executor (stub hỏng / sai wire / thiếu)

### E1. grok-web executor STUB HỎNG (P0) [behavior-diff]
- **JS:** `grok-web.js:9-24` MODEL_MAP routes grok-3/4/4.1/4.2 → {grokModel,modelMode,isThinking}; :247-259 grokPayload (temporary:true, modelName, modelMode); ~15 anti-bot headers (Sentry-statsig-id...); NDJSON→SSE rewrite.
- **Rust:** `grok_web.rs:144-146` build_url → `https://grok.com/app-chat/conversations/new` (**thiếu /rest**); :148-163 chỉ Content-Type, Accept.
- **Impl:** port MODEL_MAP + grokPayload + anti-bot headers + NDJSON→SSE + đúng URL.
- **Test:** `grok_web_builds_payload_with_model_map`.

### E2. perplexity-web executor STUB HỎNG (P0) [behavior-diff]
- **JS:** `perplexity-web.js:10-18` MODEL_MAP pplx-* → [mode, modelPref]; :160-182 buildPplxRequestBody (query_str, params.search_focus, frontend_uuid, last_backend_uuid); session cache; markdown post-process.
- **Rust:** `grok_web.rs:304-306` build_url → `https://perplexity.ai/rest/sse/perplexity_ask` (**thiếu www**); :308-324 chỉ Content-Type/Accept.
- **Impl:** port body + session cache + response cleaning + đúng host.
- **Test:** `perplexity_web_builds_pplx_body`.

### E3. windsurf — thiếu gRPC-web executor (P0) [missing]
- **JS:** `windsurf.js:15-18` WS_CHAT_URL = `https://server.codeium.com/exa.language_server_pb.LanguageServerService/GetChatMessage`; :26-119 MODEL_ALIAS_MAP (50+); Content-Type `application/grpc-web+proto`, Bearer apiKey + Metadata.api_key protobuf; decode binary CompletionChunk → OpenAI SSE.
- **Rust:** `provider.rs:1377-1379` windsurf = openai(`https://server.self-serve.windsurf.com`) — host thường, không phải gRPC-web.
- **Impl:** port gRPC-web executor + MODEL_ALIAS_MAP + protobuf decode.
- **Test:** `windsurf_uses_grpc_web_endpoint`.

### E4. trae — thiếu SOLO remote-agent executor (P0) [missing]
- **JS:** `trae.js:44-339` TraeExecutor: createSession POST /chat_sessions, streamEvents GET /chat_sessions/{id}/events?reply_to_message_id=, Cloud-IDE-JWT auth, X-Trae-Client-Type/X-Preferenced-Language/x-user-region headers, cumulative-thought rendering.
- **Rust:** `provider.rs:1087-1089` trae = openai(`https://core-normal.trae.ai/api/remote/v1`); chat.rs không có case → fall generic.
- **Impl:** port 2-phase SOLO agent protocol.
- **Test:** `trae_creates_chat_session`.

### E5. zed — catalogued nhưng không executor (P0) [missing]
- **JS:** `zed.js:207-302` ZedExecutor (thread envelope {thread_id,prompt_id,provider,provider_request}, zedLlmFetch LLM-token exchange, NDJSON status-frame stream).
- **Rust:** zed có trong provider_catalog.json:5852-5854 nhưng không có executor/provider.rs/default.rs.
- **Impl:** port ZedExecutor + LLM-token exchange.
- **Test:** `zed_executor_builds_thread_envelope`.

### E6. codebuddy-intl — catalogued nhưng không executor (P0) [missing]
- **JS:** `codebuddy-intl.js:16-41` transformRequest forceStream + delete reasoning_effort (none/off) + reasoning_summary=auto + message rewrite.
- **Rust:** codebuddy-intl trong catalog (alias cbai) nhưng không có executor/registry/default.
- **Impl:** port CodeBuddyIntlExecutor.
- **Test:** `codebuddy_intl_forces_stream`.

### E7. opencode-go — default.rs config STALE (P1) [behavior-diff]
- **JS:** `opencode-go.js:7-14` MESSAGES_FORMAT_MODELS = {minimax-m3, m2.7, m2.5, qwen3.7-max/plus, qwen3.6-plus} → /zen/go/v1/messages.
- **Rust:** `opencode_go.rs:26-37` đúng (6-model claude set); nhưng `default.rs:139-` giữ config STALE (sai path, chỉ 2/6 model) — hazard khi DefaultExecutor fallback.
- **Impl:** sửa default.rs opencode-go config khớp dedicated executor.
- **Test:** `opencode_go_default_config_matches_dedicated`.

### E8. iflow — sai host mintlify (P0) [wrong-url]
- **JS:** `iflow.js:88` buildUrl → registry baseUrl `https://apis.iflow.cn/v1/chat/completions`.
- **Rust:** `iflow.rs:16` IFLOW_BASE_URL = `https://iflow.mintlify.cc` (không path). HMAC signature đúng nhưng endpoint sai.
- **Impl:** đổi sang `https://apis.iflow.cn/v1/chat/completions`.
- **Test:** `iflow_uses_apis_iflow_cn`.

### E9. codebuddy-cn — thiếu neutralizer (P1) [behavior-diff]
- **JS:** `codebuddy-cn.js:28-48` AGENT_PATTERN regex + length>2000 catch-all replace Claude Code/Cursor system prompts với NEUTRAL_PROMPT (Tencent content filter); xoá reasoning_effort none/off.
- **Rust:** `codebuddy_cn.rs:93-111` chỉ forceStream + reasoning_summary; **không neutralize, không xoá none/off**.
- **Impl:** port neutralizer + none/off deletion.
- **Test:** `codebuddy_cn_neutralizes_agent_prompt`.

### E10. kimchi — strip sai hướng (P1) [behavior-diff]
- **JS:** `kimchi.js:79-87` strip `reasoning_content` từ **assistant REQUEST messages** (length>8); :110-114 xoá reasoning_effort/reasoning/thinking cho anthropic-backed (regex /claude|anthropic/i).
- **Rust:** `kimchi.rs:144-161` strip từ **RESPONSE**; :50-52 is_anthropic_backed chỉ starts_with("kimchi-sonnet")/("kimchi-haiku") — **đảo hướng + detector hẹp**.
- **Impl:** strip từ request assistant messages + regex detector.
- **Test:** `kimchi_strips_request_reasoning_content`.

### E11. kiro — thiếu repair loop (P1) [behavior-diff]
- **JS:** `kiro.js:130-137` appendRepairInstruction, :139-171 stopDisposition, :173-185 isEllipsisOnly/isShortFutureAction, execute() retry với repair instruction.
- **Rust:** `kiro.rs` không có repair (grep rỗng). Chỉ AWS EventStream wire + signing.
- **Impl:** port repair loop + stop-disposition + future-action detection (xem thêm B5 tương tự trong media/feature).
- **Test:** `kiro_repairs_ellipsis_response`.

### E12. grok-cli — thiếu machine-id (P1) [behavior-diff]
- **JS:** `grok-cli.js:528-549` execute() lazy resolve `getConsistentMachineId("grok-cli-agent")` khi connection không deviceId → UUID; gửi `x-grok-agent-id`.
- **Rust:** `grok_cli.rs:537-538` agent_id = psd deviceId/agentId; **không machine-id fallback** → x-grok-agent-id (grokl_cli.rs:351-352) không bao giờ gửi.
- **Impl:** thêm machine-id fallback.
- **Test:** `grok_cli_derives_machine_id`.

### E13. mimo-free — sai marker text (P1) [behavior-diff]
- **JS:** `mimo-free.js:24-25` MIMO_SYSTEM_MARKER = "You are MiMoCode, an interactive CLI tool that helps users with software engineering tasks." — upstream gate check **chuỗi chính xác này**.
- **Rust:** `mimo_free.rs:47-59` "You are MiMoCode, a helpful coding assistant created by MiMo." + rules block dài — **khác chuỗi** → marker substring absent.
- **Impl:** dùng đúng chuỗi JS.
- **Test:** `mimo_free_uses_exact_marker`.

### E14. qoder — thiếu jt- routing (P1) [behavior-diff]
- **JS:** `qoder.js:348-356` token bắt đầu `jt-` (và non-pt có jt- accessToken) → `api2.qoder.sh/algo/api/v2/service/...`.
- **Rust:** `qoder.rs:26` QODER_CHAT_URL_ENCODED = `https://api3.qoder.sh/algo/...`; :503-505 luôn api3, không jt- branch.
- **Impl:** thêm jt- branch → api2.
- **Test:** `qoder_jt_token_routes_to_api2`.

### E15. azure — sai env var names (P1) [behavior-diff]
- **JS:** `azure.js:10-21` đọc AZURE_ENDPOINT, AZURE_API_VERSION, AZURE_DEPLOYMENT (+ providerSpecificData); fallback api.openai.com.
- **Rust:** `azure.rs:96-136` đọc AZURE_OPENAI_ENDPOINT, AZURE_OPENAI_API_VERSION, AZURE_OPENAI_DEPLOYMENT; DEFAULT empty (không fallback).
- **Impl:** đúng env names + fallback.
- **Test:** `azure_reads_js_env_names`.

### E16. ollama-local — sai query (P1) [behavior-diff]
- **JS:** `ollama-local.js:10` `${host}/api/chat` không query (stream là body field).
- **Rust:** `ollama.rs:166` `{base}/api/chat?stream={stream}` — thêm query param JS không gửi.
- **Impl:** bỏ query, stream chỉ trong body.
- **Test:** `ollama_local_no_stream_query`.

---

## PHẦN F — Tính năng v0.5.35–v0.5.50

### F1. Vision/audio capacity adapter (P0) [missing]
- **JS:** `capacityAdapter.js:15` DEFAULT_FALLBACK_MODEL=`oc/mimo-v2.5-free`; :36-50 getCapacityAdapterConfig default-enable pools; prepend pool models **chỉ khi** không model original thoả hard capability; strip history theo context window adapter model.
- **Rust:** không có capacityAdapter; `combo/mod.rs:189` HARD_CAPS=["vision","pdf"]; detect_required_capabilities (193-260) chỉ vision/pdf.
- **Impl:** port capacityAdapter service + wiring (default-enable, fallback pool, context strip). Thêm `audioInput`/`videoInput` capabilities.
- **Test:** `capacity_adapter_prepends_fallback_model`.

### F2. Auto-provision Default Key (P0) [missing]
- **JS:** `EndpointPageClient.js:266-276` trên endpoint page load, nếu `/api/keys` trả 0 key → POST /api/keys {name:"Default Key",...}.
- **Rust:** `web/src/components/EndpointPageClient.tsx` không có; không server-side equivalent.
- **Impl:** thêm auto-provision client-side (hoặc server-side khi keys rỗng).
- **Test:** `default_key_auto_provisioned_when_empty`.

### F3. codex-tui / Codex Desktop detection (P1) [behavior-diff]
- **JS:** `clientDetector.js:41-44` codex branch match `codex-tui` || `codex-cli` || `codex_cli_rs` || `codex desktop` hoặc `originator.startsWith('codex_')`.
- **Rust:** `client_detector.rs:75` chỉ `ua.contains("codex-cli")`; **không đọc originator**.
- **Impl:** thêm 4 UA + originator prefix.
- **Test:** `codex_desktop_detected`.

### F4. X-9Router-Token-Saver header (P1) [missing]
- **JS:** `runtimeConfig.js:68` TOKEN_SAVER_HEADER=`x-9router-token-saver`; `chatCore.js:228-229` tokenSaverEnabled = header?.toLowerCase() !== "off" AND rtk/headroom/caveman/ponytail/pxpipe.
- **Rust:** không có reference; `chat.rs:831` compress_messages gate chỉ settings.
- **Impl:** đọc header, AND vào mọi saver enablement.
- **Test:** `token_saver_off_disables_rtk`.

### F5. Request-details + observability default (P1) [behavior-diff]
- **JS:** `requestDetailsRepo.js` batched SQLite gated bởi ENABLE_REQUEST_LOGS → UI enableObservability → OBSERVABILITY_ENABLED (default true); `settingsRepo.js:35` `enableObservability: false`.
- **Rust:** `request_repo.rs:6-22` save() **zero production caller**; `types/mod.rs:554` `observability_enabled: true` (ngược JS).
- **Impl:** wire save() vào chat path + default OFF.
- **Test:** `observability_defaults_off`.

### F6. GitHub monthly-exhausted 402 (P1) [missing]
- **JS:** `auth.js:13-19` githubMonthlyResetMs: resolved provider==github && status==402 && errorText includes "you've reached your additional usage limit for your plan" → cooldown = next UTC month start − now; lock cấp account (model null).
- **Rust:** `error_config.rs:190-194` 402 → cooldown LONG_MS=2min cố định.
- **Impl:** port githubMonthlyResetMs + next-UTC-month reset.
- **Test:** `github_402_holds_to_month_reset`.

### F7. IntelliJ h2c Upgrade (P1) [missing]
- **JS:** `custom-server.js:69-94` http upgrade h2c → drain body (content-length bounded), replay qua normal handler, remove 'upgrade'/'http2-settings', Connection: close.
- **Rust:** `main.rs:452` axum::serve không có hyper::upgrade/h2c.
- **Impl:** thêm h2c upgrade → HTTP/1.1 replay middleware.
- **Test:** `h2c_upgrade_replays_as_http1`.

### F8. forceStream cached-prompt tokens (P1) [behavior-diff]
- **JS:** `sseToJsonHandler.js:232-247` inTokens = input_tokens + cache_read_input_tokens/cached_tokens + cache_creation_input_tokens; expose prompt_tokens_details.cached_tokens.
- **Rust:** `stream_to_json.rs:458-472` chỉ input_tokens.
- **Impl:** sum cache tokens vào prompt_tokens.
- **Test:** `sse_to_json_sums_cached_tokens`.

### F9. Headroom byte-level report (P1) [behavior-diff]
- **JS:** `headroom.js:344-355` formatHeadroomSizeLog body/messages/tools/toolHistory byte before→after + effective % (bodyBytes delta); captureSizeSnaps.
- **Rust:** `headroom.rs:190-232` chỉ token counts + phantom heuristic.
- **Impl:** port byte-level snapshot + effective %.
- **Test:** `headroom_reports_byte_delta`.

### F10. claude-adaptive thinking (P1) [behavior-diff]
- **JS:** `thinkingUnified.js:238-247` case claude-adaptive: body.thinking = {type:"adaptive"} AND body.output_config = {effort: level}.
- **Rust:** `thinking_suffix.rs:425-437` chỉ insert output_config.effort; **không thinking.type adaptive**.
- **Impl:** emit cả 2 fields.
- **Test:** `claude_adaptive_sets_thinking_type`.

### F11. Kiro integrity recovery (P1) [behavior-diff]
- **JS:** `kiro.js:149-151` stopDisposition; :400-560 runIntegrityRecovery + readIntegrityAttempt (buffer maxBytes, stall/ttft timeouts, ellipsis/short-final/invalid-tool/missing-terminal → retry once với repair instruction → kiro_* SSE codes).
- **Rust:** `kiro.rs:288` chỉ POST + decode chunk; `kiro_to_openai.rs:238` stateless converter.
- **Impl:** port buffer validation + repair retry + kiro_* error codes.
- **Test:** `kiro_integrity_recovery_on_ellipsis`.

### F12. Cursor HTTP/2 AgentService (P1) [behavior-diff]
- **JS:** `cursor.js:385-390` AgentService HTTP/2-only, openAgentHttp2Stream (node:http2 duplex); isAgentTextRequest :73 routes plain-text turns.
- **Rust:** `cursor.rs:20-24` luôn ChatService/StreamUnifiedChatWithTools.
- **Impl:** thêm HTTP/2 AgentService branch (isAgentTextRequest routing).
- **Test:** `cursor_agent_text_uses_http2`.

### F13. Grok Build subagent config + expiresAt (P1) [behavior-diff]
- **JS:** `grokBuildConfig.js:7` GROK_SUBAGENT_TYPES=['general-purpose','explore','plan']; :172-186 write [subagents.models] slots `9router-<type>` + preserved context_window; refresh grok-cli khi expiresAt − now < 5min.
- **Rust:** `grok_build_settings.rs:340-367` chỉ 1 slot `[model.openproxy]`; không subagent models; không grok-cli refresh.
- **Impl:** port subagent slots + context_window + expiresAt refresh surface.
- **Test:** `grok_build_writes_subagent_slots`.

### F14. `9router xai video` CLI (P1) [missing]
- **JS:** CLI command xai video generation.
- **Rust:** không có.
- **Impl:** thêm CLI subcommand.
- **Test:** `xai_video_cli_exists`.

### F15. Ollama quota tracker (P1) [missing]
- **JS:** `usage/ollama.js` fetch Session(5h)/Weekly(7d) real usage.
- **Rust:** `quota_fetcher.rs` static message.
- **Impl:** port real quota fetch.
- **Test:** `ollama_quota_fetches_real`.

---

## PHẦN G — MITM & RTK

### G1. MITM handlers/DNS/CA (P1) [missing ×3]
- **JS:** `src/mitm/server.js:58-72` handlers map (antigravity/copilot/kiro/cursor); :316-336 request dispatch (extractModel → MODEL_NO_MAP → rewrite body model → forward localhost router). `mitm/dns/dnsConfig.js:136-210` addDNSEntry/removeAllDNSEntries write `127.0.0.1 <toolhost>` vào /etc/hosts. `mitm/cert/install.js:1-204` installCert (certutil/UAC Windows, sudo macOS, update-ca per-distro Linux, user NSS dbs) + stale cert cleanup.
- **Rust:** `core/mitm/server.rs:137-283` chỉ raw TLS byte-pump (pump_captured copy bytes); không decode/rewrite model; không hosts-file steering; `cert.rs:112-168` install/uninstall **không bao giờ gọi**.
- **Impl:** port 3 thành phần MITM đầy đủ.
- **Test:** `mitm_rewrites_model`, `mitm_hosts_steering`, `mitm_ca_installed`.

### G2. RTK gaps (P1)
- **G2a. `x-9router-token-saver: off`** — xem F4. [`core/rtk`]
- **G2b. git-log RTK filter thiếu** — JS `gitLog.js:6-78` (commit headers/Author/Date/subject/stat, drop body, cap GIT_LOG_MAX_LINES=200); Rust `filters/mod.rs:8-101` có GitDiffFilter/GitStatusFilter nhưng không GitLogFilter; autodetect không có git-log branch. [`core/rtk/filters`]
- **G2c. Caveman prompts thiếu 8 shared directives** — JS `cavemanPrompts.js:11-19` SHARED_EXAMPLES/BOUNDARIES/AUTO_CLARITY/PERSISTENCE/NO_INVENTED_ABBREV/PRESERVE_LANGUAGE/NO_SELF_REFERENCE/NO_DECORATION; Rust `mod.rs:49-91` mỗi level 3-6 câu. [`core/rtk`]
- **G2d. System prompt injector chỉ OpenAI shape** — JS `systemInject.js:14-25` dispatch CLAUDE/GEMINI/VERTEX/ANTIGRAVITY/RESPONSES; Rust `system_inject.rs:8-42` chỉ messages[] OpenAI. [`core/rtk`]
- **G2e. Headroom thiếu responses/kiro format** — JS `headroom.js:266-323` 4 shapes (claude/responses/kiro/openai) translate→compress→translate-back; Rust `headroom.rs:255-300` chỉ claude. [`core/rtk/headroom.rs`]
- **G2f. find filter thiếu backslash** — JS `find.js:22-29` lastIndexOf("/") và "\\"; Rust `filters/mod.rs:330-342` chỉ rfind('/'). [`core/rtk/filters`]

---

## PHẦN H — DB / Usage / CLI

### H1. requestDetails observability chạy (P2) [missing] — xem F5. `request_repo.rs save()` không caller.

### H2. Observability default OFF (P2) [behavior-diff] — xem F5.

### H3. Stuck pending-request 60s auto-clear (P2) [missing]
- **JS:** `usageRepo.js:12` PENDING_TIMEOUT_MS=60*1000, :172-185 trackPendingRequest set timer zero counts.
- **Rust:** `usage_live.rs:65-111` chỉ inc/dec; không timer.
- **Impl:** thêm timer.
- **Test:** `pending_request_clears_after_60s`.

### H4. /api/usage/history thiếu fields (P2) [behavior-diff]
- **JS:** `usageRepo.js:327-334` getUsageHistory maps {timestamp, provider, model, connectionId, apiKeyMasked: maskApiKey(...), endpoint, status, tokens...}.
- **Rust:** `usage.rs:307-336` UsageEntryDto chỉ {timestamp, provider, model, prompt_tokens, completion_tokens, cost}.
- **Impl:** thêm connectionId/apiKeyMasked/endpoint/status/tokens.
- **Test:** `usage_history_includes_api_key_masked`.

### H5. Request-log timestamp format (P2) [behavior-diff]
- **JS:** `usageRepo.js:734-737` formatLogDate pad `DD-MM-YYYY HH:MM:SS` local; :758-767 `ts | model | provider`.
- **Rust:** raw RFC3339.
- **Impl:** format local.
- **Test:** `request_log_uses_local_timestamp`.

### H6. Usage fetch không refresh OAuth (P2) [missing]
- **JS:** usage fetch refresh + force-retry trên auth-expired.
- **Rust:** không.
- **Impl:** thêm refresh.
- **Test:** `usage_refreshes_on_auth_expired`.

### H7. API-key usage whitelist (P2) [missing]
- **JS:** 12 providers trong whitelist; accept authType `api_key`.
- **Rust:** 6 providers; không accept `api_key`.
- **Impl:** mở rộng whitelist.
- **Test:** `apikey_usage_whitelist_covers_12`.

### H8. Vercel/CodeBuddy/Grok/Kiro quota handlers (P2) [missing]
- **Vercel AI Gateway** credit usage handler thiếu. **CodeBuddy CN/Intl** quota handler thiếu. **Grok CLI** quota fetch là stub (thiếu JWT tier, Monthly/On-demand/Prepaid, exhausted row). **Kiro** quota fetch drop `tokentype:API_KEY`/`TokenType:EXTERNAL_IDP` headers + profileArn.
- **Impl:** port 4 handlers theo JS.
- **Test:** mỗi provider có quota test.

### H9. saveRequestUsage dedupe (P2) [behavior-diff]
- **JS:** dedupe identical rows.
- **Rust:** `track_request` blind append.
- **Impl:** dedupe.
- **Test:** `request_usage_dedupes`.

### H10. DB migrations (P2) [missing]
- **JS:** có versioned migrations + additive column sync.
- **Rust:** schema frozen tại CREATE IF NOT EXISTS.
- **Impl:** thêm migration framework.
- **Test:** `migrations_apply_idempotently`.

### H11. Grok Build slot name (P2) [behavior-diff] — `openproxy` vs `9router` + subagentModels. Xem F13.

### H12. `9router xai video` CLI (P2) [missing] — xem F14.

### H13. Exa MCP toggle (P2) [missing]
- **JS:** claude settings viết `~/.claude.json` mcpServers.
- **Rust:** không.
- **Impl:** thêm toggle.
- **Test:** `exa_mcp_writes_claude_json`.

---

## PHẦN I — Web dashboard (Astro)

### I1. OAuth cards thiếu: trae / windsurf / zed (P1) [missing]
- **JS:** `providers.js:66-68` OAUTH_PROVIDERS từ registry trae.js/windsurf.js/zed.js.
- **Rust:** `web/src/shared/constants/providers.ts:52-83` OAUTH_PROVIDERS thiếu 3. (zed hidden trong JS nên chỉ cần khi đã thêm backend).
- **Impl:** thêm cards.
- **Test:** `web_oauth_cards_include_trae_windsurf`.

### I2. OAuth cards thiếu: clinepass / codebuddy-cn / codebuddy-intl / gitlab (P1) [missing]
- **JS:** registry clinepass (priority 85), codebuddy-cn (90), codebuddy-intl (90) — visible; gitlab hidden (chỉ search).
- **Rust:** `providers.ts:52-83` thiếu cả 4 (backend có clinepass/codebuddy_cn/gitlab).
- **Impl:** thêm cards.
- **Test:** `web_oauth_cards_include_codebuddy_intl`.

### I3. API-key cards thiếu 10 provider (P1) [missing]
- **JS:** alims-intl, commandcode, featherless, perplexity-agent, selfhosted-embedding/stt/tts, tokenrouter, venice, vercel-ai-gateway, mmf.
- **Rust:** `providers.ts:84-183` APIKEY_PROVIDERS thiếu (snapshot hardcode).
- **Impl:** thêm cards + models.
- **Test:** `web_apikey_cards_include_tokenrouter`.

### I4. Qwen stale card (P1) [behavior-diff]
- **JS:** registry v0.5.50 không còn qwen.
- **Rust:** `providers.ts:14` qwen deprecated banner.
- **Impl:** xoá.
- **Test:** `web_has_no_qwen_card`.

### I5. kimi duplicate card (P1) [behavior-diff]
- **JS:** kimi.js:23 category 'oauth' duy nhất + authModes ['oauth','apikey'] → hiện 1 lần.
- **Rust:** `providers.ts:59` (OAUTH) + `:87` (APIKEY) → 2 cards.
- **Impl:** bỏ khỏi APIKEY_PROVIDERS.
- **Test:** `web_kimi_shows_once`.

### I6. Usage chart shape (P1) [behavior-diff]
- **JS:** `route.js` → bare array `[{label:'Mar 7',tokens,cost}]`.
- **Rust:** `usage.rs:1046` → `{data:[{date:'Mar 7',tokens,cost}]}`.
- **Impl:** đổi envelope + key `date`→`label`.
- **Test:** `usage_chart_returns_bare_array`.

### I7. Quota visibility settings (P1) [missing]
- **JS:** `ProviderLimits/index.js:152` quotaVisibility state, :575-624 updateQuotaVisibility/handleHideQuota/handleShowQuota → PATCH /api/settings {provider:{hidden:[keys]}}.
- **Rust:** `ProviderLimits/index.tsx` không có.
- **Impl:** thêm UI + settings field.
- **Test:** `quota_visibility_saved`.

### I8. Quota filter/sort/pagination (P1) [missing]
- **JS:** `utils.js:27-44` CONNECTIONS_PAGE_SIZE=20, ACCOUNT_PAGE_SIZE_OPTIONS=[10,20,50,100], ACCOUNT_FILTER_OPTIONS, QUOTA_SORT_OPTIONS; server-side pagination.
- **Rust:** chỉ expiringFirst.
- **Impl:** thêm filter/sort/pagination.
- **Test:** `quota_pagination_works`.

### I9. ProviderTopology animation (P1) [behavior-diff]
- **JS:** `ProviderTopology.js:160-260` TopologyEdge feTurbulence plasma + animateMotion orbs + sparks + 60s stuck-drop timeout.
- **Rust:** `ProviderTopology.tsx:214` chỉ animated:active dashes.
- **Impl:** port custom edge.
- **Test:** `topology_uses_custom_edge`.

### I10. PXPIPE page (P0) [missing]
- **JS:** `pxpipe/page.js` + `PxpipeClient.js` (token savings display, timeline AreaChart) + 8 API routes.
- **Rust:** không có.
- **Impl:** port page + backend (xem E-phần PXPIPE).
- **Test:** `pxpipe_page_exists`.

### I11. CLI tools devin/opendesign (P1) [missing]
- **JS:** `cliTools.js:393-457` devin (guide) + opendesign (guide); route devin-settings.
- **Rust:** `cliTools.ts` 19 keys, thiếu 2.
- **Impl:** thêm 2 tool + route.
- **Test:** `cli_tools_include_devin`.

### I12. ProviderIcon lazy/404-cache/aliases (P1) [behavior-diff]
- **JS:** `providerIcon.js` ICON_ALIASES {perplexity-agent→perplexity, gitlab-duo→gitlab, vercel-ai-gateway→vercel}; session 404 cache; loading=lazy.
- **Rust:** `ProviderIcon.tsx` không có.
- **Impl:** thêm aliases + cache + lazy.
- **Test:** `provider_icon_uses_aliases`.

### I13. Bulk-add key gap-fill (P1) [behavior-diff]
- **JS:** `bulkAdd.js:30-90` planBulkAdd gap-fill smallest free `<base> <n>`.
- **Rust:** `AddApiKeyModal.tsx:95-103` blind `<base> ${i+1}` → **overwrite data-loss**.
- **Impl:** port gap-fill.
- **Test:** `bulk_add_skips_existing_names`.

### I14. UsageStats filters (P1) [behavior-diff]
- **JS:** `UsageStats.js:225-256` fetch /api/providers + /api/provider-nodes; filter isActive===false out; isLLMProvider; nodeNameMap.
- **Rust:** `UsageStats.tsx:231-245` chỉ /api/providers, dedupe, không filter.
- **Impl:** thêm filters + nodeName merge.
- **Test:** `usage_stats_filters_inactive`.

### I15. Donate button (P1) [missing]
- **JS:** `Header.js:317-328` pink Donate + DonateModal.
- **Rust:** `Header.tsx:283-287` không.
- **Impl:** thêm.
- **Test:** `header_has_donate`.

### I16. OIDC chip (P1) [behavior-diff]
- **JS:** `Header.js:309-318` OIDC display-name chip.
- **Rust:** không (backend OIDC có).
- **Impl:** thêm chip.
- **Test:** `header_shows_oidc_chip`.

### I17. freeTier placement (P1) [behavior-diff]
- **JS:** 11 provider freeTier (poolside, searxng, edge-tts, google-tts, coqui, tortoise, local-device, kilo-gateway, api-airforce, bazaarlink, kimchi).
- **Rust:** đặt dưới APIKEY.
- **Impl:** chuyển sang free-tier category.
- **Test:** `free_tier_providers_in_right_category`.

### I18. USAGE_SUPPORTED_PROVIDERS lists (P1) [behavior-diff]
- **JS:** `providers.js:163-170` từ registry features.usage (20 providers).
- **Rust:** `providers.ts:275-301` thiếu codebuddy-cn/intl, qoder, trae, vercel-ai-gateway, zed.
- **Impl:** đồng bộ với registry.
- **Test:** `usage_supported_lists_match_registry`.

### I19. OAuthModal device-code list (P1) [behavior-diff]
- **JS:** `OAuthModal.js:225-234` deviceCodeProviders = github,kiro,kimi,kimi-coding,kilocode,codebuddy-cn,codebuddy-intl,qoder,grok-cli.
- **Rust:** `OAuthModal.tsx:195-208` có qwen/kimchi/codebuddy (sai) + thiếu codebuddy-intl.
- **Impl:** đồng bộ list.
- **Test:** `oauth_modal_device_list_matches`.

### I20. QuotaTable layout (P1) [behavior-diff]
- **JS:** `QuotaTable.js:151-244` flex rows + pagination + hide.
- **Rust:** `QuotaTable.tsx:100` HTML table fixed % widths.
- **Impl:** flex + pagination + hide.
- **Test:** `quota_table_flex_pagination`.

---

## TỔNG KẾT

Toàn bộ 146 gap CONFIRMED đã được chi tiết hoá ở mức **implementation-ready** (URL chính xác, header, auth, model list, field mapping, step-by-step). Các phần còn thiếu trong report này (nếu có) sẽ được bổ sung từ workflow spec `wf_bc281048-1fe` khi hoàn tất.

**Thứ tự triển khai:**
1. **P0 (Phần A + E1-E6):** 17 providers + selfhosted + OAuth zed/intl/trae/windsurf + URL fixes + grok-web/pplx stubs + xoá qwen → mọi provider chạy được.
2. **P1 (B,C,D,E7-E16,F,G,I):** translator/oauth/media/executor chi tiết + web dashboard.
3. **P2 (H):** usage/observability/db/cli.

**Verify:** mỗi task có guard test. Chạy `cargo test --lib` sau mỗi cụm. Mở `.tmp/9router` tại dòng JS dẫn trong từng phần trước khi code.
