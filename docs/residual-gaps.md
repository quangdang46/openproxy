# Residual 9router parity gaps (post ultracode)

Evidence-based synthesis of logic / web / API scans against **9router v0.5.30** (`/tmp/9router`) and **OpenProxy `main`**.  
Adversarially re-verified on current tree — outdated doc claims that already landed are listed under **Confirmed fixed**, not as open debt.

| Bucket | Count |
|--------|------:|
| **P0** (broken core) | 0 (fixed) |
| **P1** (major missing) | 5 |
| **P2** (polish / convenience) | 2 (3 fixed) |
| **Intentional** | 12 |
| **Confirmed fixed** (post ultracode) | 22+ |

---

## P0 — fix first

| ID | Area | Gap | Evidence |
|----|------|-----|----------|
| `transport-stale-urls` | `resolve_transport` multi-endpoint table | Multi-endpoint table still pins **stale hosts/paths**, then chat copies them onto `connection.runtime_transport`. `DefaultExecutor::build_url` treats that URL as final (`already_endpoint` → return as-is), **overriding** the correct provider defaults and never appending `?beta=true`. **Impact:** OpenAI-format (and some Claude) traffic for kimi / glm / minimax / minimax-cn can hit dead or wrong endpoints. | OP: `src/core/chat/mod.rs` `provider_transports` — kimi→`api.moonshot.cn`, glm→`open.bigmodel.cn/api/paas`, minimax→`api.minimax.chat/.../chatcompletion_v2`, minimax-cn OpenAI→`chatcompletion_v2`. Chat pin: `src/server/api/chat.rs` (~937–941). Override win: `src/core/executor/default.rs` (~638–650). Correct defaults already in `default.rs` (~48–62): `api.z.ai`, `api.kimi.com/coding`, `api.minimax.io`. 9r: `open-sse/providers/registry/{kimi,glm,minimax,minimax-cn}.js` (`api.kimi.com/coding`, `api.z.ai`, `api.minimax.io` / `minimaxi.com`, `.../v1/chat/completions`, Claude `urlSuffix: "?beta=true"`); `open-sse/executors/default.js` appends `urlSuffix`. |

**Fix sketch:** Align `provider_transports` URLs with 9router registry + DefaultExecutor defaults; carry optional `url_suffix` (or bake `?beta=true` into Claude legs); ensure `build_url` does not silently drop suffix when `runtime_transport` is set.

---

## P1 — major missing

| ID | Area | Gap | Evidence |
|----|------|-----|----------|
| `thinking-suffix-reapply` | Chat translate path | Global strip of `model(level)` / `model-level` exists, but the parsed **level is discarded**. There is no `applyThinking` equivalent after translation, so suffix-driven thinking/budget/effort never lands on the upstream body. Settings `providerThinking` path still works. | OP: `src/core/chat/mod.rs` (~80–84) `let (stripped, _level) = strip_thinking_suffix_owned(...)`; helpers in `src/core/utils/thinking_suffix.rs`; translate uses stripped `dispatch_model` only (`src/server/api/chat.rs` ~759–786). 9r: `open-sse/translator/concerns/thinkingUnified.js` `parseSuffix` + `applyThinking`; `open-sse/translator/index.js` applies after translate. |
| `kiro-external-idp-refresh` | OAuth refresh | Executor already handles `external_idp` hosts/`TokenType: EXTERNAL_IDP`, but **refresh cannot renew** those tokens: `KiroAuthMethod` has no `external_idp`; `refresh_kiro_token` is only OIDC JSON (`clientId`+`clientSecret`) or Cognito `/refreshToken`. Enterprise Microsoft Entra form-POST path missing. | OP: `src/oauth/kiro.rs` enum builder-id/idc/google/github/imported only; `src/oauth/token_refresh.rs` ~534–575; executor OK in `src/core/executor/kiro.rs` ~158–255. 9r: `open-sse/services/tokenRefresh/providers.js` external_idp branch + `buildExternalIdpRefreshParams`; `src/lib/oauth/kiroExternalIdp.js`. |
| `thinking-levels-model-aware` | Provider detail UI | Thinking picker uses coarse provider `THINKING_CONFIG` (auto/on/off or effort set). Ignores model id — not 9r `getThinkingLevels(provider, model)` per-format/capability matrix. Most OAuth providers fall back to `extended`. | OP: `web/src/components/providers/ProviderDetailPageClient.tsx` ~118–129 `resolveThinkingSuffix(_modelId)` + comment that open-sse levels unavailable; `web/src/shared/constants/providers.ts` THINKING_CONFIG. 9r: `open-sse/providers/thinkingLevels.js`; providers detail page uses `getThinkingLevels(providerId, modelId)`. |
| `minimax-tts-voices` | Media TTS API/UI | No MiniMax / MiniMax-CN voice browser. Backend registers deepgram/inworld/elevenlabs/generic only; web TTS config has no minimax `apiEndpoint`. | OP: `src/server/api/media_providers.rs` `routes()` ~906–935; `web/src/shared/constants/ttsProviders.ts` ends without minimax browse. 9r: `src/app/api/media-providers/tts/minimax/voices/route.js` + `ttsProviders.js` apiEndpoint; registry `ttsConfig` on minimax / minimax-cn. |
| `codex-reset-credits-openai` | Usage API | 9r GET probes OpenAI rate-limit reset credit balance; POST **consumes** a credit via OpenAI API. OP only registers **POST** that clears **local** `credits_reset_at`/backoff; response shape differs; dashboard does not call it. | OP: `src/server/api/usage.rs` routes ~88–89 POST-only; handler ~513+. No `codex-reset-credits` in `web/src`. 9r: `src/app/api/usage/[connectionId]/codex-reset-credits/route.js` GET/POST + `ProviderLimits/index.js` polls GET. |

---

## P2 — polish

| ID | Area | Gap | Evidence |
|----|------|-----|----------|
| ~~`i18n-zh-cn-key-coverage`~~ | Locales | ~~zh-CN leaf keys ~872 vs 9r ~1389~~ — **Fixed**: now 1346 keys after merging relevant 9router keys with brand adaptation (9Router→OpenProxy), filtering out 9r-only Qwen/Amp/jcode strings. | `web/public/i18n/literals/zh-CN.json` — 474 new keys added, all original 872 preserved. |
| `models-card-media-no-caps` | Media providers UI | Legacy `ModelsCard` inlined `ModelRow` has no `CapacityBadges` / `thinkingSuffix`. Still used by media-provider detail; main provider detail uses full `ModelRow.tsx`. | OP: `web/src/components/providers/ModelsCard.tsx` ~31; `MediaProvidersKindIdPageClient.tsx` imports it; full row: `web/src/components/providers/ModelRow.tsx` ~54. |
| `cli-tools-all-statuses` | CLI Tools API | Missing batch `GET /api/cli-tools/all-statuses`. OP N+1 fetches each `*-settings` — functional, slower load only. | 9r: `src/app/api/cli-tools/all-statuses/route.js` + `CLIToolsPageClient.js` `ALL_STATUSES_URL`. OP: path absent under `src/server/api`; `web/src/components/CLIToolsPageClient.tsx` per-tool map. |
| `headroom-proxy` | Token Saver | No same-origin `/api/headroom/proxy/*` reverse proxy + HTML rewrite. OP links browser directly to `headroomUrl/dashboard` (fine on loopback; weaker when Headroom is remote). | OP: headroom routes status/start/stop/restart/extras only; `TokenSaverPageClient.tsx` `headroomDashboardHref` = raw URL. 9r: `src/app/api/headroom/proxy/[...path]/route.js`. |

---

## Intentional (do not treat as debt)

| ID | Why | Evidence |
|----|-----|----------|
| `pxpipe` | 9r hard-hides PXPIPE UI; OP omits — use RTK + Headroom + Caveman/Ponytail | `docs/parity-9router.md`, `docs/web-gaps-9router.md`; no `pxpipe` under OP `web/` / `src/server/api` |
| `hedging-shadow-auto-combo-product` | Scaffold modules exist; chat maps hedging/shadow/auto-combo → Fallback. Same effective chat surface as 9r (fallback / RR / fusion only) | `src/server/api/chat.rs` `combo_strategy_for` ~2174–2195; `src/core/combo/{hedging,shadow,auto_combo}.rs`; 9r `chat.js` fallbackStrategy fusion/RR only |
| `ssrf-image-prefetch` | Security hardening vs 9r; fail closed | `docs/parity-9router.md` |
| `refresh-dedup-no-null-cache` | Deliberate fix of 9r bug (do not cache failed refresh) | `src/oauth/token_refresh.rs` ~269–281 |
| `combo-quarantine-rr-capacity-preskip` | Reliability improvement over 9r try-anyway | `src/core/combo/mod.rs`; parity doc |
| `encrypted-sqlite` | OP security feature beyond 9r | parity doc |
| `vertex-sa-jwt-design-split` | SA JWT mint lives in vertex executor by design, not OAuth dispatch | `src/core/executor/vertex.rs`; parity doc |
| `headroom-extras-via-process-not-compress` | `code_aware`/`kompress` affect managed process args only (same as 9r) | `src/server/api/headroom.rs` extras; 9r `lib/headroom/process.js` |
| `initializeApp-rust-owned-supervision` | Client resume + quota tick only; process watchdog in Rust | `web/src/shared/services/initializeApp.ts` design comment |
| `op-only-payload-rules-db-backups` | OP product surface beyond 9r | `PayloadRulesPageClient`, `DbBackupsPageClient` |
| `oidc-start-path-rename` | 9r `/api/auth/oidc/start` → OP `/api/auth/oidc/login` (same PKCE); UI retargeted | `src/server/api/auth.rs`, `LoginPageClient.tsx` |
| `shutdown-path-hardening` | 9r unauth `/api/version/shutdown` → OP `/api/shutdown` + `SHUTDOWN_SECRET` + non-prod guard | `shutdown.rs`, Profile/Sidebar |

Also intentional contract differences (not drop-in 9r API clones): password rotate vs reset-to-default; OAuth explicit routes vs catch-all; LLM under `/v1/*` not `/api/v1/*`. `basic-chat-orphan-route`: Intentional orphan (same as 9r). Page kept at `/dashboard/basic-chat` for direct URL / layout special-case; **not** linked in sidebar nav. Do not add to nav.

---

## Confirmed fixed (post ultracode — do not re-open)

Logic / chat / executors:

- grok-cli specialized executor + chat branch (`gcli`/`gb`/`grok-build`) — `src/core/executor/grok_cli.rs`, `chat.rs` ~1567+
- vertex + vertex-partner + vxp → `VertexExecutor` — `chat.rs` ~1023
- xiaomi-mimo/mimo + xiaomi-tokenplan/xmtp in `resolve_transport` + region dual OpenAI/Claude URL — `chat/mod.rs`, `default.rs` ~605–636
- web_fetch selective modelLock clear (not clear-all)
- global thinking **suffix strip** helper + `RequestPlan` integration (re-apply still open — see P1)
- chat pipeline order parity: providerThinking → strip/modality → translate → RTK → headroom → caveman/ponytail → tool dedupe → executor → 401/429 → selective lock
- specialized executor matrix (kiro, vertex, codex, cursor, github, azure, qwen, iflow, gemini-cli, opencode, opencode-go, qoder, commandcode, antigravity, grok-web, perplexity-web, kimchi, codebuddy-cn, ollama, mimo-free, grok-cli) + Default
- combo strategies: fallback, round-robin+sticky, fusion; account fallbackStrategy fill-first/RR/sticky
- refresh dedup success-only + Codex 8d max age
- headroom `code_aware`/`kompress` settings + process args
- quota auto-ping warm pings (refresh + Claude/Codex synthetic request + lastPing* persist + cooldowns) — `src/server/api/quota_auto_ping.rs`

Web (previously listed P0/P1 in `web-gaps-9router.md`, verified present):

profile-password-post-auth, provider-custom-models-api, provider-thinking-picker-ui (coarse), oauth-xai-proxy-manual-code, kiro-api-key-cliproxy, kiro-dual-auth-list, combos-fusion-judge-ui, cli-amp-qwen, capacity-badges-use-model-caps, cowork-mcp-marketplace, headroom-extras-codeaware-kompress, oidc-profile-card, oidc-login-must-change-password, account-fallback-strategy-rr, combo-sticky-round-robin-limit, initializeApp-dashboard-layout, quota-auto-ping-ui-hooks-and-tick-foundation, providers-hidden-filter-and-add-deeplink, provider-one-by-one-test, qoder-fetch-models, antigravity-risk-confirm-modal, endpoint-tunnel-miss-threshold, i18n-fa-locale-present, providers-new-full-form.

---

## Recommended fix order

1. **`transport-stale-urls` (P0)** — wrong hosts break live multi-endpoint providers; one-file table + suffix handling.
2. **`thinking-suffix-reapply` (P1)** — complete the strip half already shipped; unblocks `model(high)`-style clients without settings.
3. **`kiro-external-idp-refresh` (P1)** — executor already accepts tokens; refresh gap causes silent expiry for enterprise imports.
4. **`codex-reset-credits-openai` (P1)** — only if Codex rate-limit credit UI is product; else demote or document local-only semantics.
5. **`minimax-tts-voices` (P1)** — media TTS parity for minimax family.
6. **`thinking-levels-model-aware` (P1)** — UX accuracy once backend re-apply exists.
7. **P2 batch** — zh-CN keys, ModelsCard caps, all-statuses convenience, headroom proxy (only if remote Headroom matters).

---

## Notes for maintainers

- **Do not invent gaps from `docs/web-gaps-9router.md` / `docs/parity-9router.md` alone** — both still list several items that ultracode already closed (grok-cli, xiaomi transports, web P0s, etc.). Prefer this residual list + re-grep.
- Naive API path diffs are noisy: OP has ~243 axum routes including OP-only admin/A2A/MITM/observability surfaces; 9r uses Next `route.js` under `/api/v1/*` while OP serves LLM under `/v1/*`.
- Scaffold modules (`hedging`/`shadow`/`auto_combo`) are **not** residual product gaps relative to 9r chat.


---

## Second ultracode pass (transport + thinking + quota + kiro + media + codex + UI)

Fixed in commit on `main` via merge of `wf_bfdf460e-f32-{2..8}` worktrees on 2026-07-12.

### Now fixed

| ID | What changed |
|----|--------------|
| `transport-stale-urls` (P0) | `chat/mod.rs provider_transports()` kimi→`api.kimi.com/coding`, glm→`api.z.ai`, minimax→`api.minimax.io`, minimax-cn→`api.minimaxi.com` + `?beta=true` on Claude legs + `default.rs build_url` no longer drops suffix |
| `thinking-suffix-reapply` (P1) | `RequestPlan.thinking_level` preserved; `apply_thinking_level` / `reapply_thinking_after_translate` ported in `thinking_suffix.rs`; called from `chat.rs` translate path |
| `quota-auto-ping-warm` (P1) | Full Claude/Codex synthetic warm ping via `sendClaudePing`/`sendCodexPing` with OAuth refresh; replaces `would_ping`/`warmPing:residual` |
| `kiro-external-idp-refresh` (P1) | `KiroAuthMethod::ExternalIdp` + form POST to Microsoft tokenEndpoint |
| `minimax-tts-voices` (P1) | `GET /api/media-providers/tts/minimax/voices` route + ttsProviders + TtsExampleCard wiring |
| `codex-reset-credits-openai` (P1) | GET balance probe + POST OpenAI redeem; ProviderLimits UI fetches it |
| `thinking-levels-model-aware` (P1) | New `web/src/shared/utils/thinkingLevels.ts` with format/capability matrix; used by ProviderDetail |
| `models-card-media-no-caps` (P2) | ModelsCard inlined ModelRow now passes caps via useModelCaps |
| `cli-all-statuses` (P2) | — (deferred; functional N+1 equivalent) |
| `headroom-proxy-dashboard` (P2) | — (deferred; local loopback fine) |
| `i18n-zh-cn-key-coverage` (P2) | zh-CN expanded from 872 to 1343 keys by merging filtered 9router zh-CN keys with brand adaptation (9Router→OpenProxy), filtering out 9r-only Qwen/Amp/jcode/CLIProxyAPI brand strings |

### Still open (residual after 3+4 passes)

| ID | Severity | Why |
|----|----------|-----|
| `combo-hedging-shadow-autocombo-unwired` | P2 | Scaffold-only; same product surface as 9r (fallback/RR/fusion) |

Everything else: intentional or confirmed fixed.

---

## Fourth residual pass (2026-07-14 ultracode) — tiny UI + logic gaps

Against 9router v0.5.30 (`~/Projects/9router`, `9845a17`). Confirmed 18 real residuals (9 refuted as intentional/already fixed).

### Fixed this pass (main tree)

| ID | What changed |
|----|--------------|
| `brand-config-openrouter` | `config.ts` install/changelog → `quangdang46/openproxy` + `install.sh` (was wrong `openrouter` / decolua) |
| `model-availability-badge-dead` | Restored trigger button on ModelAvailabilityBadge |
| `web-cookie-providers-grid` | Un-commented Web Cookie Providers section; cookie+apikey stats/toggle |
| `cookie-auth-type-create` | `create_provider_api` sets `auth_type: "cookie"` for grok-web/perplexity-web |
| `landing-links-commented` | Re-enabled Docs/GitHub/Footer/Hero GitHub links + real product copy |
| `authmodes-dual-xai` | `Provider.authModes`; xAI dual OAuth+API key on OAUTH_PROVIDERS; dual footers |
| `conn-multiselect` | Select All + row checkboxes + Delete Selected + ConfirmModal bulk delete |
| `qoder-fetch-custom-models` | Import writes custom models (not aliases) + dedupe against customModels |
| `compatible-add-deeplink` | `?compatible=` + category-aware `?add=` on providers list |
| `free-tier-llm-sort` | freeTier llm filter; noAuth-first sort; oauth priority sort |
| `cli-list-summary-cards` | CLI index → ToolSummaryCard grid → `/cli-tools/[toolId]` |
| `cli-amp-shorthand-comments` | Amp codeBlock shorthand alias comments |
| `endpoint-ever-reachable` | tunnel/ts ever-reachable + checking vs reconnecting amber rows |
| `endpoint-remote-host-warning` | Non-loopback + !requireApiKey → "Endpoint is exposed without an API key." |
| `profile-password-fallback` | Dead NOT_IMPLEMENTED CLI-only copy narrowed to hard failures |
| `i18n-risk-mitm-keys` | Backfilled risk/MITM/endpoint-exposed keys across non-zh locales |
| `cli-all-statuses-inprocess` | `get_all_statuses` invokes per-tool handlers in-process via `tokio::join!` (no loopback HTTP) |
| `profile-db-export-reauth` | `require_database_password_reauth` + `x-op-password`/`x-9r-password`; Profile + DbBackups password modals |
| `settings-database-reauth` | `/api/settings/database` GET/POST enforces dashboard session + password re-auth |
| `headroom-html-rewrite` | Proxy rewrites dashboard `fetch('/stats\|health|…')` → `/api/headroom/proxy/…` |

### Fifth pass — deep web audit (2026-07-14 ultracode)

Confirmed high-severity web gaps fixed on main:

| ID | Fix |
|----|-----|
| `media-detail-static-paths-empty` | `[kind]/[id]` + `combo/[id]` emit `_dynamic` shells |
| `pricing-page-broken-orphaned` | Real `PricingPageClient` + Profile link |
| `modelrow-ondisable-dead` | ModelRow accepts/renders `onDisable` |
| `model-kind-filter` | Filter uses `kind \|\| type` (not type-only) |
| `grok-cli-missing-web` | `grok-cli` added to OAUTH_PROVIDERS |
| `ep-login-unsafe-*` / `ep-ts-require-api-key` | CF+TS Enable gated on login+password+API key; pre-enable banner |
| `nav-dual-profile-settings` | Removed duplicate Profile nav entry |
| `update-modal-openrouter-branding` | OpenRouter → OpenProxy in update UI |
| `connection-row-per-auth-type` | Auth icon/name from `connection.authType` |

### New features in 9router v0.5.35 (not yet ported, significant backend work)

| ID | Severity | What | Technical notes |
|----|----------|------|-----------------|
| `cli-grok-build-tool` | P1 | Grok Build CLI dashboard integration | New `GrokBuildToolCard`, `grok-build-settings` API endpoint, CLI_TOOLS entry, ToolDetailClient case, all-statuses entry. Backend: read/write `~/.grok/config.toml` |
| `xai-video-v1-videos` | P1 | xAI Grok Imagine async video generation | `POST /v1/videos/generations` + `GET /v1/videos/{id}` (raw proxy to api.x.ai). Catalog `videoConfig`. Optional `xai video` CLI subcommand |
| `kiro-session-replay` | P1 | Kiro session-start msg0 freeze for prompt-cache reuse | `sessionStartStore` (`connectionId:conversationId` map), port `applyKiroSessionReplay` in `openai-to-kiro` + `claude-to-kiro` translators |
| `github-copilot-claude-messages` | P1 | GitHub Copilot native `/v1/messages` route for Claude models | Route Claude bodies to `api.githubcopilot.com/v1/messages` (Anthropic format) for prompt-cache token savings |

### Recent fixes (2026-07-17 — 17 items from 4th ultracode pass)

| ID | What |
|----|------|
| `bulk-cf-accountid` | Cloudflare bulk-add: `name|apiKey|accountId` parsing + placeholder |
| `compatible-default-model` | AddApiKeyModal collects `defaultModel` for OpenAI/Anthropic compatible nodes |
| `providers-new-searchable` | Select component gains searchable filter; used by ProvidersNewPageClient |
| `usage-cached-card` | OverviewCards: Cached Tokens KPI card |
| `usage-table-cached-columns` | UsageTable: Cached / Cached Cost columns |
| `quota-pagination` | ProviderLimits: server pagination, account filter, page-size controls |
| `quota-table-sort` | QuotaTable: remaining-% sort modes + 10-row pagination |
| `conn-secondary-name` | ConnectionRow: name-first display + secondary identity line |
| `quota-label-name-first` | Quota label prefers edited name over email |
| `pp-search-dual-empty` | Provider search hides Custom section when empty + searching |
| `pp-apikey-sort` | API Key list sorts by total>0 then name |
| `pp-freetier-sort` | Free Tier applies sortByPriority then noAuth |
| `conn-edit-modal-secondary-fields` | EditConnectionModal: email, displayName, proxy pool fields |
| `pp-global-redirect` | Global test-all chains through all auth types |
| `compatible-sort-priority` | Compatible list sorted by connected then name |
| `quota-recurring-label` | Quota labels: "Expires in" for non-recurring packs, "Expired" when past |
| `quota-auto-refresh-on-change` | ProviderLimits re-fetches on toggle/delete |

### Still intended as low-priority / scaffold-only

- PXPIPE, hedging/shadow/auto-combo product wire, basic-chat orphan nav, Translator flag, DonateModal brand

### Still intentionally open / low priority

- PXPIPE, hedging/shadow/auto-combo product wire, basic-chat orphan nav, Translator flag, DonateModal brand
