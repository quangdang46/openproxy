# Parity Audit: openproxy (Rust) vs 9router v0.5.75

**Date:** 2026-09-11  
**Reference:** `.tmp/9router` = 9router v0.5.75 (2026-09-10)  
**Delta window:** v0.5.65 → v0.5.75 (48 commits)  
**Audit method:** 23-cluster deep audit + 4 completeness-critic extra areas + 2-lens adversarial verification (36 agents)

---

## Summary

| Category | Count | Description |
|----------|-------|-------------|
| **Total gaps found** | 343 | Verified by reading both codebases at exact lines |
| **P0 — Critical (crash/404/500)** | 50 | Broken requests, missing providers, auth bypass, hard crashes |
| **P1 — Behavior divergence** | 176 | Feature present but wrong detail breaks some providers/clients |
| **P2 — Polish/observability** | 117 | Missing usage surfacing, logging, defaults, UI gaps |

**Already fixed this session (2 P0):**
1. ✅ `[1m]` context marker strip wired into chat entry (`chat.rs:276`)
2. ✅ `auto` effort folded to `high` in `ClaudeAdaptive` thinking (`thinking_suffix.rs:432`)
3. ✅ Codex tool Unicode pattern strip (`codex.rs` + `strip_codex_tool_patterns`)
4. ✅ Gemini `includeThoughts` camelCase fix (`openai_to_gemini.rs` 4×)
5. ✅ Combo failover on 4xx (removed `Permanent` block in `combo/mod.rs`)

---

## P0 — Critical Gaps (50)

### A. Missing Core Routes / Auth Bypass

| # | Title | Rust Ref | Crash Scenario | Fix Hint |
|---|-------|----------|----------------|----------|
| 1 | Missing `/codex/:path*` rewrite | `mod.rs:144-162` | Codex CLI with `/codex` prefix gets 404 on every request | Add `.route("/codex/{*path}", compat::responses)` mirroring `/responses` |
| 2 | `requireApiKey` enforcement missing on `/responses` | `chat.rs:264-269` | Operator enables API-key protection but `/responses` stays anonymous | Port `requireApiKey` gate before model check |
| 3 | SAML 2.0 SSO completely absent | `auth.rs:1034-1047` | Enterprise IdP users (Okta, ADFS) cannot authenticate dashboard | 4 routes + `saml.rs` library + settings fields |
| 4 | `xiaomi-mimo/api-key` import route missing (404) | `oauth.rs:54` | Adding Xiaomi MiMo account via documented API returns 404 | Add route + handler mirroring JS |
| 5 | `xiaomi-mimo/auto-import` route missing | `oauth.rs:54` | Auto-detect UX 404s; no desktop passToken reader | Implement route + desktop cookie/JSON reader |
| 6 | `grok-cli/bulk-import` route missing (404) | `oauth.rs:54` | Bulk-import Grok CLI credentials fails; must use device-code one-by-one | Port bulk import with JWT backfill |
| 7 | `codex/bulk-import` rejects bare-array/single-object | `oauth.rs:49` | Common one-account imports or raw arrays return 400 | Normalize per JS: accept array, object with `accounts`, or bare object |
| 8 | `codex/import-token` stores wrong authType, no JWT decode | `oauth.rs:50` | Pasted access token → no `chatgptAccountId`, no email → executor sends conn UUID | Decode access token + nested OpenAI claims |

### B. Broken Provider Executors / Wrong Upstream

| # | Title | Rust Ref | Crash Scenario | Fix Hint |
|---|-------|----------|----------------|----------|
| 9 | Qoder COSY RSA uses PKCS#1 PEM + OAEP (should be PKCS1v1.5 + SPKI) | `qoder.rs:12,150-155,384-395` | Every Qoder request → `CryptoError` → `execute_request` errors | PEM label `BEGIN PUBLIC KEY`, parse with `rsa::pkcs8::DecodePublicKey` |
| 10 | Qoder catalog parses `data/models` but API returns `body.chat` | `qoder.rs:887-906` | Valid token → model list call returns `{chat:[...]}`; Rust finds nothing | Parse `chat` array first, fallback to `data/models` |
| 11 | Qoder billing-block detection misses string error codes | `qoder.rs:272-305` | Code `10605` (queue throttle) not detected; leaks into chat content | Match code as string OR number (`as_u64` or `as_str` for 112/10605) |
| 12 | Qoder billing block never triggers combo/account fallback | `chat.rs:3210-3212,3673-3684` | Qoder quits (code 112/10605) → no failover; user sees error instead of transparent retry | Return real 403 before streaming (peek first frame) or consume flag in combo dispatcher |
| 13 | OpenCode Go Responses-only models never routed to `/zen/go/v1/responses` | `opencode_go.rs:26-38,123-130` | Request for `ocg/grok-4.6` or `ocg/muse-spark-1.3` dispatches wrong wire format | Add `/zen/go/v1/responses` path + per-model `targetFormat` |
| 14 | OpenCode Go Claude-capable models POST `/messages` with OpenAI body | `opencode_go.rs:31-38,119-130` | OpenAI-format client calling `ocg/minimax-m3` sends OpenAI body to `/messages` (expects Claude) | Replace hardcoded list with sourceFormat-matched transports table |
| 15 | OpenRouter video generation unsupported | `media.rs:1534-1538` | `POST /v1/videos/generations {"model":"openrouter/google/veo-3.1"}` → 400 | Port `videoProviders/index.js` + `openrouter.js` adapter |
| 16 | Vertex AI (Veo) adapter entirely absent | `media.rs:1534-1538` | `POST /v1/videos/generations {"model":"vertex/veo-3.1..."}` → 400 | Port `vertex.js` (job encode/decode, SA token, SSRF guard) |
| 17 | Antigravity image generation sends bare Gemini body | `media/image/antigravity.rs:49-51,80-107` | `/v1/images` with `provider=antigravity` → Google receives no project/model envelope | Route through Antigravity executor or replicate `image_gen` envelope |
| 18 | Antigravity missing from `/v1/search` dispatch | `media/search/mod.rs:28` | `POST /v1/search provider=antigravity` → 400 "Unsupported search provider" | Check provider's `searchViaChat` capability in dispatch |
| 19 | Antigravity missing from `handle_chat_search` match | `media/search/chat_search.rs:33-47` | Even if dispatch fixed, fallback returns `None` → caller fails | Add `antigravity` arm; unwrap thinking, build Google Search grounding request |
| 20 | Missing `ollama-search` search provider | `chat_search.rs:26-43` | User with only Ollama Cloud connection + `provider: "ollama-search"` → Serper or 400 | Add mapping + `OllamaSearchProvider` adapter |
| 21 | Missing `glm` search provider (GLM MCP `web_search_prime`) | `chat_search.rs:26-43` | GLM/ZAI connection + `provider: "glm"` → Serper or 400 | Add mapping + `GlmSearchProvider` with JSON-RPC envelope |
| 22 | Missing `xquik` search provider (X/Twitter) | `providers.rs:14-28` | `POST /v1/search model="xquik"` → 400 "Unsupported search provider" | Implement `XquikProvider`: GET, baseUrl `https://xquik.com/api/v1/x/tweets/search` |
| 23 | Missing `ollama-search` provider in `providers.rs` | `providers.rs:14-28` | `provider="ollama-search"` → generic forwarder wrong URL | `OllamaSearchProvider`: POST, baseUrl `https://ollama.com/api/web_search` |
| 24 | Missing `glm` provider in `providers.rs` | `providers.rs:14-28` | `provider="glm"` → generic forwarder wrong URL | `GlmSearchProvider`: POST, baseUrl `https://api.z.ai/api/mcp/web_search_prime/mcp`, JSON-RPC 2.0 |

### C. Translator / Format Parity

| # | Title | Rust Ref | Crash Scenario | Fix Hint |
|---|-------|----------|----------------|----------|
| 25 | `compat input_to_messages` drops `function_call`, `output`, `reasoning`, custom tools | `compat.rs:2098-2143` | Multi-turn Codex via `/v1/responses` loses all prior tool calls/results | Port full input-item switch (function_call, outputs, reasoning, additional_tools, input_image) |
| 26 | Streaming chat→responses closes on `finish_reason:null` | `response/openai_responses.rs:353` | Every streaming `/v1/responses` terminates after first token; truncated answer | Check non-null string `finish_reason` (not `as_str().is_some()`) |
| 27 | Parallel tool calls merged — missing `item_id`→index map | `response/openai_responses.rs:672-760` | Two parallel calls (exec + edit) concat N JSON payloads into one tool input | Add `item_id`→index map + `args-emitted` set per JS `respToolChatIndex` |
| 28 | String/empty-array Responses input never normalized | `request/openai_responses.rs:153-157` | Client sends `input` as string or `[]` → untranslated body or empty `messages[]` → 400 | Port string/empty-array branches including `...` placeholder |
| 29 | Passthrough Claude cache re-anchoring + 4-marker cap absent | `chat.rs:1056-1068` | Client body with 5 client markers → forwarded verbatim → upstream 400 budget exceeded | Port `anchorClaudeCache` + `countCacheControlBlocks`/`capCacheControlBlocks` (claude.js:64-102,343-410) |
| 30 | `[1m]` context marker defined but **never called** (dead code) | `claude_format.rs:74-83` | `claude-opus-5[1m]` → resolves against combos/aliases → "Invalid model format" | Call `strip_model_context_marker` at top of chat handler ✅ FIXED |
| 31 | Normalize Claude passthrough missing thinking-block validation (step 5) | `claude_format.rs:111-238` | Combo fallback carries thinking blocks to non-Claude → 400 | Add step 5: drop thinking/redacted blocks from assistant content |
| 32 | Kiro translators still emit top-level `systemPrompt` | `openai_to_kiro.rs:536-538` | Every Kiro request (`kr/...` models) gets 400 `REQUEST_BODY_INVALID` | Delete `payload["systemPrompt"]` writes |
| 33 | Kiro integrity repair appends to top-level `systemPrompt` | `kiro.rs:183-201` | Repair retry sent with `systemPrompt` → 400 again | Append to `conversationState.currentMessage.userInputMessage.content` |
| 34 | Kiro executor omits runtime-surface headers | `kiro.rs:559-625` | Modern payloads rejected `REQUEST_BODY_INVALID` (missing SSO/agent-mode/machine-id) | Port `buildHeaders` verbatim: `x-amz-sso-bearer`, `x-amzn-kiro-agent-mode=spec`, `x-amz-target`, etc. |
| 34 | Kiro base-URL ordering wrong for OAuth/social accounts | `kiro.rs:483-557` | OAuth Kiro account hits `runtime.us-east-1.kiro.dev` first → rejects | Enforce q → codewhisperer → others order (regionalized) |
| 35 | Cline/ClinePass non-stream responses not unwrapped | `chat.rs:2988-3067` | `stream:false` to cline/clinepass → upstream `{"success":true,"data":{...}}` → "no completion choices" | Add provider quirk (`cline`, `clinepass`); unwrap `success===true && object data` |
| 36 | Cline/ClinePass OAuth JWT missing required `workos:` prefix | `default.rs:1060-1094`, `cline_auth.rs` | Every cline/clinepass OAuth request → 401 (Bearer without `workos:`) | In `build_headers` for cline/clinepass, prefix `workos:` iff JWT starts with `eyJ` |
| 37 | Responses→chat `call_id` coercion missing | `request/openai_responses.rs:244-251` | Overlong/missing `call_ids` rejected by strict upstreams; arguments double-encoded | Port `clampResponsesCallId`/`coerceResponsesArguments`/`coerceResponsesOutput` |
| 38 | Custom tool calls always emitted as `function_call` | `response/openai_responses.rs:296-311,510-537` | Codex custom tools (shell freeform input) → `{input:...}` wrapper → client JSON validation fails | Thread `_customToolNames` into streaming state; branch emit/close on `isCustomTool` |
| 39 | Reasoning extraction limited to `reasoning_content` | `response/openai_responses.rs:116-178` | Qwen/MiniMax/think-tagged models lose thinking stream; leaks into answer deltas | Port `extractReasoningText` vendor-shape fallback + `omitted` state machine |
| 40 | TTS voices routes require API key (unauthenticated in JS) | `media_providers.rs:833-835,944-946,698-700,324-326` | With `require_login=true`, `GET /api/media-providers/tts/voices?provider=edge-tts` → 401 | Gate on dashboard-session-or-management-key like `/media-providers/{kind}/voices` |

### D. Config / Model Catalog / Capabilities

| # | Title | Rust Ref | Crash Scenario | Fix Hint |
|---|-------|----------|----------------|----------|
| 41 | `gpt-6-astra`, GPT-5.6 Sol/Terra/Luna absent from codex catalog | `model/provider_catalog.json:180` | `cx/gpt-6-astra` → no catalog entry → upstream model stays `gpt-6-astra` (wrong) | Regenerate catalog from v0.5.75 registry or patch cx entry |
| 42 | GPT Image 2.5/Flare/Sunburst missing from codex + openai catalogs | `catalog.json:180,1583` | New image model IDs absent from `/v1/models` and capability metadata (multiImage) | Add 5 codex image IDs (capabilities/params/kind) + 3 openai IDs |
| 43 | Codex review models lack `upstreamModelId` and `-review` fallback | `chat/mod.rs:136-152`, `catalog.json:188` | `cx/gpt-5.5-review` dispatches `gpt-5.5-review` upstream → Codex rejects | Populate `upstreamModelId` on every cx `-review` entry (base ID) |
| 44 | Capacity adapter defaults empty; vision/audio auto-routing off | `types/mod.rs:725` | Fresh install: image request to text-only combo never gets adapter prepended | Seed Settings default `capacity_adapter` with 4 JS pool entries |
| 45 | Capability reorder uses prefix heuristic, ignores capability table | `combo/mod.rs:480` | Vision request on `[gpt-4.1, gpt-4]` → JS floats `gpt-4.1` (vision=true), Rust demotes it | Replace `model_has_capability` with `combo::capabilities::get_model_capabilities` |
| 46 | Round-robin fail-fast on saturated providers has no JS equivalent | `combo/mod.rs:920` | Saturated providers → 503 "All combo providers at max in-flight" even though retry would succeed | Gate fail-fast behind opt-in flag; default should try all rotated members |
| 47 | Missing solo-model capacity-adapter failover | `chat.rs:744` | Single model needing vision (e.g. `openai/gpt-4` with image) never gets adapter prepended | Add solo-augment check in Direct branch mirroring JS lines 143-158 |
| 48 | `auth_mode` `'saml'`/`'sso'` silently coerced to `'password'` | `types/mod.rs:1011-1016` | Admin configures SAML → Rust normalizes to `password` → login page shows password form | Extend `normalize_auth_mode` to accept `saml`/`sso` |
| 49 | Settings struct missing all SAML configuration fields | `types/mod.rs:630-662` | SAML config cannot persist/read | Add `saml_entry_point`, `saml_issuer`, `saml_cert`, `saml_private_key`, `saml_sp_entity_id`, `saml_name_id_format` |
| 50 | Tunnel enable response shape wrong (no `tunnelUrl`/`publicUrl`/`shortId`) | `tunnel.rs:151-157` | `POST /api/tunnel/enable` from Endpoint page → "No tunnel URL returned" | Return `{success, tunnelUrl, shortId, publicUrl}` |

---

## P1 — Behavior Divergence (176)

### High-Impact P1s (excerpt)

| # | Title | Rust Ref | Impact |
|---|-------|----------|--------|
| 1 | Qoder: no `/api/v2/image/upload`, no image_url preservation, no oversized stubs | `qoder.rs:728-781` | Vision requests reach Qoder with image removed |
| 2 | Qoder: context-window tier auto-escalation absent (200K/400K/1M) | `qoder.rs` ABSENT | Long sessions past ~180K tokens rejected upstream |
| 3 | Qoder: SSE usage coalescer missing — usage/finish_reason never surfaced | `qoder.rs:1193-1232`, `chat.rs:3655-3692` | Clients get no token usage; may not detect stream completion |
| 4 | Codex executor: no User-Agent identity header | `codex.rs:184-232` | All requests reach chatgpt.com with no UA |
| 5 | Kiro: missing `kiro/auto-import` clientId/clientSecret/region/profileArn | `oauth.rs:23` | Auto-import for AWS IAM Identity Center fails |
| 6 | Kiro: `kiro/import` cannot perform IDC refresh, ignores profileArn/authMethod | `oauth.rs:23` | IDC refresh token flow hits social endpoint → fails |
| 7 | Web fetch: no `ollama` arm in `build_fetch_request` | `web_fetch.rs:462-466` | `provider: "ollama"` → 400 "Unsupported web fetch provider" |
| 8 | Web fetch: no `ollama` arm in `normalize_fetch_response` | `web_fetch.rs:617` | Even if request worked, empty content/title/links |
| 9 | OpenCode Go: missing `muse-spark-1.3-contributor` + parallel tool calls fix | `opencode_go.rs` catalog | New model not in catalog; parallel tools broken |
| 10 | Antigravity: weekly quota tracking + free-tier handling absent | `executor/antigravity.rs` | No weekly Gemini/Claude/GPT quota surfacing |
| 11 | Antigravity: OAuth token-refresh Google anti-abuse protection missing | `oauth/antigravity.rs` | Multi-account refresh hits Google rate limits |
| 12 | Provider health: stale `modelLock_*`, `backoffLevel`, `rateLimitedUntil` not cleared on re-validation | `connection_repo.rs` | Re-validated connection retains stale locks → stays unavailable |
| 13 | Fable weekly limit parsed from `limits[]` not fabricated | `usage/quota_fetcher.rs` | Weekly Fable row fabricated instead of parsed from provider response |
| 14 | OpenCode Go quota tracking missing | `usage/quota_fetcher.rs` | OCg usage not tracked |
| 15 | Groq `x-ratelimit-*` headers not parsed for rate-limit tracking | `usage/tracker.rs` | Groq rate limits not tracked |
| 16 | Responses `cached_tokens` not read for non-streaming usage | `translator/response/non_streaming.rs` | Non-streaming Responses usage missing cached tokens |
| 17 | SSRF guard bypasses (IPv6 alt encodings, trailing dots, wildcard DNS, redirect) | `bypass_handler.rs` / `mitm/` | SSRF bypass possible on outbound fetches |
| 18 | Cowork MCP tools probe SSRF guard missing | `mcp_server.rs` | Cowork MCP probe can hit internal targets |
| 19 | `/responses` rewrite missing API-key validation in dashboard guard | `chat.rs` | Root `/responses` rewrite bypasses auth |
| 20 | 503 when all credentials rate-limited (vs current 500) | `chat.rs:1299` | All creds limited → should return 503 Service Unavailable |

*(Full P1 list: 176 entries in audit transcripts — see agent outputs)*

---

## P2 — Polish / Observability (117)

Notable P2s:
- Qoder `model_config` sent as 3-field stub instead of full live catalog entry
- Usage stream: missing provider connection ID correlation
- Dashboard: theme flash on reload (JS applies before first paint)
- Dashboard: Quota Tracker should group Antigravity Gemini + Claude
- i18n: zh-CN only 872→1343 keys (JS has 1391 Indonesian complete)
- pxpipe CLI subcommand missing (web+API exist)
- Provider test requires user `baseUrl` even for providers with hardcoded default
- SSE usage live: missing `finish_reason` propagation on some paths
- CLI: missing `clone` subcommand for provider connections

---

## Fixes Applied This Session

| File | Change | Parity Gap Addressed |
|------|--------|---------------------|
| `src/server/api/chat.rs` | Wire `strip_model_context_marker` at chat entry (lines 276-295) | P0 #30: `[1m]` marker never stripped |
| `src/core/utils/thinking_suffix.rs` | Fold `auto` effort to `high` in `ClaudeAdaptive` (line 432) | P0 from v0.5.69 #3792: `auto` effort rejected by Anthropic |
| `src/core/executor/codex.rs` | Add `strip_codex_tool_patterns` + wire into request build | P0 #4: Unicode property escapes in tool schemas 400 on Codex |
| `src/core/translator/request/openai_to_gemini.rs` | `include_thoughts` → `includeThoughts` (4×) | P0 #32: snake_case field rejected by Gemini API |
| `src/core/combo/mod.rs` | `Permanent` → failover with `LONG_COOLDOWN` (line 599) | P0 #10: Combo stops on 4xx instead of failing over |

---

## Next Recommended Fixes (Priority Order)

1. **Add `/codex/:path*` routes** — Codex CLI completely broken without this (P0 #1)
2. **Add `requireApiKey` gate to `/responses`** — Auth bypass on Responses API (P0 #2)
3. **Fix Qoder COSY RSA encryption** — Every Qoder request fails (P0 #9)
4. **Fix Qoder catalog parsing (`chat` array)** — Every Qoder model list call fails (P0 #10)
5. **Implement SAML 2.0 SSO** — Enterprise auth completely blocked (P0 #3)
6. **Implement `xiaomi-mimo` OAuth routes** — Provider unusable (P0 #4, #5)
7. **Implement `grok-cli/bulk-import`** — Bulk credential import broken (P0 #6)
8. **Fix `codex/bulk-import` body normalization** — Common import patterns 400 (P0 #7)
9. **Fix `codex/import-token` JWT decode** — Pasted tokens store no metadata (P0 #8)
10. **Port `compat input_to_messages` full input-item switch** — Multi-turn Codex sessions broken (P0 #25)
11. **Fix streaming Responses `finish_reason:null` close** — Every streaming `/v1/responses` truncated (P0 #26)
12. **Port parallel tool call `item_id`→index map** — Parallel tools concat into one input (P0 #27)
13. **Normalize Responses string/empty-array input** — Clients sending raw string/[] get 400 (P0 #28)
14. **Port Claude cache re-anchoring + 4-marker cap** — Passthrough with 5+ markers 400s (P0 #29)
15. **Fix Kiro `systemPrompt` emission + repair loop** — All Kiro requests 400 (P0 #32, #33)
16. **Port Kiro runtime-surface headers** — Modern Kiro gateway rejects requests (P0 #34)
17. **Fix Kiro base-URL ordering for OAuth** — OAuth accounts hit wrong endpoint first (P0 #35)
18. **Unwrap Cline/ClinePass non-stream envelope** — `stream:false` returns "no choices" (P0 #35)
19. **Add `workos:` prefix to Cline/ClinePass OAuth** — All OAuth requests 401 (P0 #36)
20. **Gate TTS voices routes on dashboard-session** — Dashboard TTS panel 401s (P0 #37)

---

## Verification Commands

```bash
# Quick compile check
cargo check --lib -p openproxy

# Run specific tests for fixed areas
cargo test strip_model_context_marker -- --nocapture
cargo test claude_adaptive_auto_effort -- --nocapture
cargo test codex_tool_pattern_strip -- --nocapture
cargo test gemini_include_thoughts -- --nocapture
cargo test combo_fallback_4xx -- --nocapture
```

---

## Notes

- **Golden rule:** This is a JS→Rust port — a single wrong detail (header, URL, field, order) breaks runtime. Every fix must be checked against the source at `.tmp/9router` before coding.
- The audit covered v0.5.65 → v0.5.75 delta (48 commits) plus long-standing gaps from v0.5.50 baseline.
- 23 primary clusters + 4 critic-suggested extra areas = 27 finders → 343 verified gaps.
- Adversarial verification: 2 lenses (JS-skeptic, Rust-skeptic) per gap; confirmed only if ≥1 lens verifies with quoted evidence.

---

*Generated from 36-agent workflow audit. Full agent transcripts in `.claude/projects/.../wf_1f085845-a26/agent-*.jsonl`.*