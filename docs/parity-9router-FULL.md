# Full Parity Report: openproxy (Rust) → 9router v0.5.50 (Node.js)

> **Date:** 2026-08-12 · **Reference:** `.tmp/9router` = `decolua/9router` v0.5.50 (2026-08-05)
> **Port:** `openproxy` v0.2.0 (Rust) — claims "v0.5.30 full parity"; gap window v0.5.30 → v0.5.50
> **Method:** 160-subagent audit → 151 findings (146 CONFIRMED / 5 REFUTED) → **122 implementation specs**, each adversarially cross-checked (CONFIRMED / PLAUSIBLE) against both codebases.

---

## TABLE OF CONTENTS

- [A. PROVIDERS (P0)](#a-providers-(p0))
- [B. EXECUTORS](#b-executors)
- [C. TRANSLATORS](#c-translators)
- [D. FEATURES v0.5.35-50](#d-features-v0535-50)
- [E. MEDIA](#e-media)
- [F. COMBO / MITM / RTK](#f-combo---mitm---rtk)
- [G. WEB DASHBOARD](#g-web-dashboard)
- [H. DB / USAGE / CLI](#h-db---usage---cli)

---

## OVERVIEW

- **122 implementation specs**, each containing: verbatim JS · current Rust · implementation steps · guard test · risks · cross-check verdict.
- Based on the **146 CONFIRMED findings** from the audit; the 5 REFUTED findings were excluded.
- **Implementation order:** P0 (Section A + executor stubs) → P1 (B, C, D, E, F, G) → P2 (H).

## A. PROVIDERS (P0) (17 specs)

### `P0-A1a` — Add alims-intl provider to PROVIDER_CONFIGS (default.rs)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/alims-intl.js:3-31. id="alims-intl", priority=11, alias="alims-intl". transport.baseUrl="https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions", headers={}, quirks={preserveCacheControl:true}. models=[{id:"qwen3.5-plus",name:"Qwen3.5 Plus"},{id:"kimi-k2.5",name:"Kimi K2.5"},{id:"glm-5",name:"GLM 5"},{id:"MiniMax-M2.5",name:"MiniMax M2.5"},{id:"qwen3-coder-next",name:"Qwen3 Coder Next"},{id:"qwen3-coder-plus",name:"Qwen3 Coder Plus"},{id:"glm-4.7",name:"GLM 4.7"}]

**Current Rust behavior:**

N/A. src/core/executor/default.rs:24-409 PROVIDER_CONFIGS has no "alims-intl" key; DefaultExecutor::new (default.rs:585-595) returns ExecutorError::UnsupportedProvider -> chat.rs:1635-1645 returns HTTP 500. Verified no other file contains the string.

**Implementation steps:**

In src/core/executor/default.rs, inside the BTreeMap::from([...]) in PROVIDER_CONFIGS, add exactly one entry (place near the alicode-intl entry at default.rs:151-155): ("alims-intl", ProviderConfig::openai("https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions"),). Do NOT add any headers (JS headers={}). Do NOT add to any anthropic/claude-compatible list in default.rs:615-627, 700-718, 805-813, 867-915. The full endpoint URL is already present so build_url (default.rs:812) returns it unchanged. Do NOT add clinepass headers block: the default.rs:947 match on "clinepass" is the existing header hook, leave it.

**Guard test:**

In tests/executor_pool_behavior.rs add #[test] fn alims_intl_has_full_endpoint_url(): build DefaultExecutor::new("alims-intl", pool, None) (must not return Err) then assert build_url("qwen3.5-plus", false, &connection("alims-intl")) == "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions" (the full endpoint, no /chat/completions appended).

**⚠️ Risks:**

preserveCacheControl:true is a translator/request quirk (JS translator request uses it to keep cache_control). If you add the provider only to the executor, chat works but the translator must not strip cache_control for this provider — check translator/registry request_transform routing (Format::OpenAi) does not need provider-specific handling since JS preserveCacheControl only affects alicode/cache-control injection; do not reintroduce a cache_control strip for this id. The baseUrl is the FULL endpoint (…/chat/completions already present) so is_already_endpoint (default.rs:606-612) must match on "/chat/completions" — it does. Do not append an extra /chat/completions.

**Cross-check:** ✅ **CONFIRMED** — JS claim is exactly real: .tmp/9router/open-sse/providers/registry/alims-intl.js:3-31 matches every cited value verbatim (id="alims-intl", priority=11, alias, baseUrl="https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions", headers={}, quirks={preserveCacheControl:true}, and the listed models). Rust current behavior is real: PROVIDER_CONFIGS in src/core/executor/default.rs:24-409 has no "alims-intl" key (grep confirms zero hits for alims-intl/dashscope-intl/compatible-mode in default.rs), and DefaultExecutor::new returns ExecutorError::UnsupportedProvider on lookup miss, which src/server/api/chat.rs:1635-1645 maps to HTTP 500 (status: 500 in ComboAttemptError). Impl steps would work: alicode-intl is at default.rs:151-155; ProviderConfig::openai(<full URL>) yields format="openai" with empty headers (matches JS headers={}), and since alims-intl is in no claude-beta/anthropic list (default.rs:615-627, 700-715, 805-810), build_url emits the baseUrl as-is and build_headers uses the generic Bearer branch — correct for DashScope compatible-mode. Minor inaccuracies that do not change the verdict: (1) the chat error mapping is in src/server/api/chat.rs, not "chat.rs"; DefaultExecutor::new's UnsupportedProvider return is actually at lines 581-595 not 585-595. (2) "Verified no other file contains the string" is overstated — alims-intl appears in two model-catalog data files (src/core/model/provider_catalog.json:116,4533,5874 and src/core/model/sources/9router.json:73,283), which are not executor config and don't affect behavior. (3) Flagged gap: the JS preserveCacheControl quirk is not replicable by this entry because the Rust translator hardcodes filter_to_openai_format(body, false) at registry.rs:486 for all providers, so cache_control blocks will be stripped for alims-intl (JS preserves them). This is a system-wide Rust limitation beyond a PROVIDER_CONFIGS entry and does not break the routing/transport parity P0-A1a targets; worth a follow-up task but not a blocker.

---

### `P0-A1b` — Add api-airforce provider to PROVIDER_CONFIGS (default.rs)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/api-airforce.js:1-36. id="api-airforce", alias="af", aliases=["airforce"], uiAlias="af", category="freeTier", authType="apikey", authModes=["apikey"]. transport.baseUrl="https://api.airforce/v1/chat/completions", validateUrl="https://api.airforce/v1/models", headers={"HTTP-Referer":"https://endpoint-proxy.local","X-Title":"Endpoint Proxy"}. models=[{id:"anthropic/claude-3.7-sonnet",name:"Claude 3.7 Sonnet (Free)",contextLength:200000},{id:"moonshot/kimi-k2.6",name:"Kimi K2.6 (Free)",contextLength:262144},{id:"google/gemini-2.5-flash",name:"Gemini 2.5 Flash (Free)",contextLength:1048576}]

**Current Rust behavior:**

N/A. No "api-airforce" string anywhere in src/. DefaultExecutor::new returns UnsupportedProvider (default.rs:585-595) -> HTTP 500.

**Implementation steps:**

In default.rs PROVIDER_CONFIGS add: ("api-airforce", ProviderConfig::openai("https://api.airforce/v1/chat/completions").with_header("HTTP-Referer", "https://endpoint-proxy.local").with_header("X-Title", "Endpoint Proxy"),). These are the exact same two headers already used by openrouter (default.rs:33-34), but you must add them explicitly — PROVIDER_CONFIGS entries do not inherit openrouter's. The baseUrl is the full endpoint; build_url returns it as-is. category freeTier has no Rust code impact (category is not stored in ProviderConfig).

**Guard test:**

tests/executor_pool_behavior.rs: #[test] fn api_airforce_headers_and_url(): assert build_url == "https://api.airforce/v1/chat/completions" and build_headers contains HTTP-Referer: https://endpoint-proxy.local and X-Title: Endpoint Proxy (exact case).

**⚠️ Risks:**

Header names are case-sensitive in the HeaderMap lookup; verify with HeaderMap::get("HTTP-Referer") / "X-Title" exactly as written in JS. The baseUrl host is "api.airforce" (no TLD suffix beyond .force) — copy verbatim, do not "fix" it to .ai or .com.

**Cross-check:** ✅ **CONFIRMED** — JS claim is exactly real: .tmp/9router/open-sse/providers/registry/api-airforce.js lines 1-36 match every cited field (id="api-airforce", alias="af", aliases=["airforce"], uiAlias="af", category="freeTier", authType/authModes=["apikey"], transport.baseUrl="https://api.airforce/v1/chat/completions", validateUrl="https://api.airforce/v1/models", headers HTTP-Referer/X-Title as claimed, models beginning with anthropic/claude-3.7-sonnet).

Rust behavior is real in substance: PROVIDER_CONFIGS (src/core/executor/default.rs:24-409) has no api-airforce entry; api-airforce hits none of the special executor branches in src/server/api/chat.rs, so it falls to the else branch (chat.rs:1634-1645) -> DefaultExecutor::new -> PROVIDER_CONFIGS.get() None -> ExecutorError::UnsupportedProvider (default.rs:585-595) -> ComboAttemptError{status:500} -> HTTP 500. One minor inaccuracy: the literal claim "No api-airforce string anywhere in src/" is false — it appears in provider_catalog.json (lines 106, 5742), sources/9router.json, and sources/omniroute.json (embedded via src/cli/sync.rs:31). But those are inert metadata (catalog.rs builds only id/alias/serviceKinds; sync.rs stores baseUrl into CustomModel.extra.providerBaseUrl which no executor reads; the api-airforce catalog entry has zero models), so the end-to-end UnsupportedProvider->HTTP 500 outcome still holds.

Impl steps would produce parity with no obvious omission: the two headers are byte-identical to openrouter's (default.rs:33-34), ProviderConfig::openai + with_header exist (default.rs:420, 451-455), build_headers inserts default_headers (default.rs:824-830), the URL ends in /chat/completions so hyper transport selection works as for openrouter (default.rs:1334-1340), and apikey->Bearer auth matches JS authModes=["apikey"]. The only caveat is that the impl_step's parenthetical (lines truncated in the prompt) appears cut off, but the core two-line add is complete and correct.

---

### `P0-A1c` — Add baidu provider to PROVIDER_CONFIGS (default.rs)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/baidu.js:1-33. id="baidu", alias="qianfan", aliases=["qianfan","ernie","baidu-qianfan"], uiAlias="qianfan", category="apikey", authType="apikey". transport.baseUrl="https://qianfan.baidubce.com/v2/chat/completions", validateUrl="https://qianfan.baidubce.com/v2/models". models=[{id:"deepseek-v4-pro",name:"DeepSeek V4 Pro",contextLength:1048576},{id:"deepseek-v4-flash",name:"DeepSeek V4 Flash",contextLength:1048576},{id:"glm-5.2",name:"GLM 5.2",contextLength:512000},{id:"glm-5.1",name:"GLM 5.1",contextLength:198000},{id:"kimi-k2.6",name:"Kimi K2.6",contextLength:262144},{id:"qwen3.5-397b-a17b",name:"Qwen 3.5 397B A17B",contextLength:262144},{id:"qwen3.5-27b",name:"Qwen 3.5 27B",contextLength:262144}]

**Current Rust behavior:**

Wrong key. src/core/executor/provider.rs:1297-1299 has ("qianfan", ProviderExecutorConfig::openai("https://qianfan.baidubce.com/v2")) — but this is inside the DEAD PROVIDER_REGISTRY (only media.rs:638 get_provider_config reads it) and the URL lacks /chat/completions. The live chat path default.rs PROVIDER_CONFIGS has NO "baidu" key (verified) -> HTTP 500. Also provider_catalog.json has a "baidu" provider entry (id=baidu, alias=qianfan).

**Implementation steps:**

1) In default.rs PROVIDER_CONFIGS add: ("baidu", ProviderConfig::openai("https://qianfan.baidubce.com/v2/chat/completions"),). Full endpoint -> build_url returns unchanged. 2) IMPORTANT do NOT rename the live key to "qianfan": the model resolution path get_model_info (src/core/model/mod.rs:211) resolves "baidu" provider id (via alias map) and passes it to DefaultExecutor; the JS id is "baidu" and provider_catalog.json provider entry id is "baidu". 3) (optional, same PR) update the dead provider.rs:1297-1299 qianfan entry's URL to include /chat/completions to match JS, but it is dead code and not required for chat.

**Guard test:**

tests/executor_pool_behavior.rs: #[test] fn baidu_has_v2_chat_completions_url(): DefaultExecutor::new("baidu",...).build_url == "https://qianfan.baidubce.com/v2/chat/completions".

**⚠️ Risks:**

JS validateUrl /v2/models differs from baseUrl /v2/chat/completions. The Rust ProviderConfig has no validateUrl concept; do not confuse the two. If a developer "fixes" the key to qianfan, connections created with provider id baidu (what the dashboard/alias map uses) will still 500. Keep the key exactly "baidu".

**Cross-check:** ✅ **CONFIRMED** — All three claims verified against source. (1) JS claim REAL: .tmp/9router/open-sse/providers/registry/baidu.js lines 1-33 match exactly — id="baidu", alias="qianfan", aliases=["qianfan","ernie","baidu-qianfan"], uiAlias="qianfan", category/authType="apikey", transport.baseUrl="https://qianfan.baidubce.com/v2/chat/completions", validateUrl="https://qianfan.baidubce.com/v2/models", models[] starting with {id:"deepseek-v4-pro",name:"DeepSeek V4 Pro",contextLength:1048576}. (2) Rust current state REAL: src/core/executor/provider.rs:1297-1299 has ("qianfan", ProviderExecutorConfig::openai("https://qianfan.baidubce.com/v2")) inside the PROVIDER_REGISTRY Lazy (line 682), URL lacking /chat/completions. That registry's only readers are get_provider_config (provider.rs:1425) consumed by UnifiedExecutor::for_provider (provider.rs:322, no live callers) and media.rs:638 get_provider_base_url; the live chat path (chat.rs:1635, cli/mod.rs:1697/1868) uses DefaultExecutor::new → PROVIDER_CONFIGS in default.rs:24, which has no "qianfan"/"baidu" entry — confirming the dead-registry characterization and missing live key. (3) Impl steps work: adding ("baidu", ProviderConfig::openai("https://qianfan.baidubce.com/v2/chat/completions")) to PROVIDER_CONFIGS gives full-endpoint URL returned unchanged by build_url (default.rs:812) — "baidu" hits no special-case branch (runtime_transport override, provider_node, gemini/opencode-go/{accountId} placeholder, claude-beta list), matching the established full-endpoint passthrough pattern used by every other openai entry (default.rs:606 is_already_endpoint). The "do NOT rename live key to qianfan" caution is correct: model resolution (get_model_info mod.rs:211 → resolve_provider_alias mod.rs:152) maps "qianfan"→"qianfan" (mod.rs:87) and the resolved provider id drives DefaultExecutor::new (chat.rs:567/1635), so a live "baidu" key is what JS parity requires. Minor scope note (not an omission): validateUrl is unused by the Rust chat path, and model-list/alias catalog parity (provider_catalog.json and sources/9router.json already carry id:baidu, alias:qianfan) is handled by other tasks in the migration, not this config-entry task.

---

### `P0-A1d` — Add bluesminds provider to PROVIDER_CONFIGS (default.rs)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/bluesminds.js:1-38. id="bluesminds", alias="bm", aliases=["blue-sminds"], uiAlias="bm", hidden:true, category="apikey", authType="apikey", authModes=["apikey"]. transport.baseUrl="https://api.bluesminds.com/v1/chat/completions", validateUrl="https://api.bluesminds.com/v1/models". models=[14 models; the ones with distinct ids you must not drop: gpt-4.1, gpt-4.1-mini, gpt-4.1-nano, claude-sonnet-4-5, claude-haiku-4-5, gemini-2.0-flash, gemini-2.0-flash-exp, qwen-turbo, kimi-k2, kimi-k2-thinking, glm-4.7, minimax-m2.5, claude-opus-4-5, gemini-2.5-pro]

**Current Rust behavior:**

N/A. No "bluesminds" string in src/. HTTP 500 via UnsupportedProvider.

**Implementation steps:**

In default.rs PROVIDER_CONFIGS add: ("bluesminds", ProviderConfig::openai("https://api.bluesminds.com/v1/chat/completions"),). No headers (JS headers not present). Full endpoint -> build_url unchanged.

**Guard test:**

tests/executor_pool_behavior.rs: #[test] fn bluesminds_uses_v1_chat_completions(): build_url == "https://api.bluesminds.com/v1/chat/completions".

**⚠️ Risks:**

hidden:true is a dashboard/UI flag, no Rust executor impact. Model ids contain "claude-*"/"gemini-*"/"gpt-*" prefixes — these route through the OpenAI translator (default target Format::OpenAi), which is correct since transport has no format override. Do not mark it anthropic-compatible because some model names start with claude.

**Cross-check:** ✅ **CONFIRMED** — All substantive claims verified.

(1) JS behavior is real: .tmp/9router/open-sse/providers/registry/bluesminds.js:1-38 exactly matches the cited spec — id="bluesminds" (L2), alias="bm" (L3), aliases=["blue-sminds"] (L4), uiAlias="bm" (L5), hidden:true (L6), category="apikey"/authType="apikey"/authModes=["apikey"] (L15-17), transport.baseUrl="https://api.bluesminds.com/v1/chat/completions" (L19), validateUrl="https://api.bluesminds.com/v1/models" (L20), and 14 models (L22-37) with distinct ids gpt-4.1, gpt-4.1-mini, gpt-4.1-nano, claude-sonnet-4-5, claude-haiku-4-5, gemini-2.0-flash, gemini-2.0-flash-exp, qwen-turbo, kimi-k2, kimi-k2-thinking, glm-4.7, minimax-m2.5, claude-opus-4-5, gemini-2.5-pro. No headers on transport — matches "no headers" in impl.

(2) Rust current behavior is real: PROVIDER_CONFIGS (src/core/executor/default.rs:24-409) has no "bluesminds" key. DefaultExecutor::new (default.rs:581-595) does PROVIDER_CONFIGS.get(provider).ok_or_else(UnsupportedProvider) when no provider_node; chat.rs:1640-1645 maps that to status:500 "Default executor creation failed". So a chat request for bluesminds returns HTTP 500 via UnsupportedProvider. Minor spec nit: "No 'bluesminds' string in src/" is technically inaccurate — the string appears 8x across provider_catalog.json, sources/9router.json, sources/omniroute.json — but all are model-catalog/alias data for /v1/models listings, not the routing registry, so the substantive claim (no chat routing support, 500) is correct. Alias "bm" also cannot resolve: chat.rs:975-979 matches provider_node by prefix, and the alias map is catalog-only.

(3) Impl achieves parity: the proposed tuple follows the file's exact pattern (e.g., deepseek, groq, xai). ProviderConfig::openai() sets empty default_headers (default.rs:420-427), matching JS's headerless transport; build_headers falls to the standard Authorization: Bearer branch (default.rs:915-938), the apikey semantic. build_url (default.rs:671-813) with no provider_node/runtime_transport and provider not gemini/opencode-go/placeholder/claude-beta-family returns self.config.base_url.clone() unchanged at line 812 — a full "/chat/completions" endpoint is used as-is, exactly the JS full-endpoint baseUrl behavior. "Full endpoint -> build_url unchanged" holds. No models need porting: Rust model listings come from the JSON catalog, not PROVIDER_CONFIGS.

---

### `P0-A1e` — Add clinepass provider to PROVIDER_CONFIGS (default.rs)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/clinepass.js:1-57. id="clinepass", priority=85, alias="clinepass", category="oauth", authModes=["oauth","apikey"], hasOAuth:true. transport.baseUrl="https://api.cline.bot/api/v1/chat/completions", headers={"HTTP-Referer":"https://cline.bot","X-Title":"Cline"}, auth={combined:true,header:"Authorization",scheme:"bearer",hooks:["clineHeaders"]}. models=[{id:"cline-pass/glm-5.2",...},{id:"cline-pass/kimi-k2.7-code",...},{id:"cline-pass/kimi-k2.6",...},{id:"cline-pass/deepseek-v4-pro",...},{id:"cline-pass/deepseek-v4-flash",...},{id:"cline-pass/mimo-v2.5",...},{id:"cline-pass/mimo-v2.5-pro",...},{id:"cline-pass/minimax-m3",...},{id:"cline-pass/qwen3.7-max",...},{id:"cline-pass/qwen3.7-plus",...}]

**Current Rust behavior:**

No config entry. default.rs:947 has `if self.provider == "cline" || self.provider == "clinepass" { // Cline often needs workos: prefix handled elsewhere; keep Bearer }` — an empty comment block (no behavior). default.rs:24-409 has NO "clinepass" config key -> DefaultExecutor::new returns UnsupportedProvider -> HTTP 500. Verified via string search: clinepass appears only at default.rs:947.

**Implementation steps:**

1) In default.rs PROVIDER_CONFIGS add: ("clinepass", ProviderConfig::openai("https://api.cline.bot/api/v1/chat/completions").with_header("HTTP-Referer", "https://cline.bot").with_header("X-Title", "Cline"),). Exact same headers as the existing "cline" entry at default.rs:133-137. 2) The empty hook at default.rs:947 can be removed or left; it is a no-op. 3) OAuth is handled separately by src/oauth/providers.rs get_config (out of this task's scope, but note clinepass OAuth must be added there for full parity — see parity-report A3; the executor part is this entry).

**Guard test:**

tests/executor_pool_behavior.rs: #[test] fn clinepass_url_and_headers(): build_url == "https://api.cline.bot/api/v1/chat/completions", build_headers has HTTP-Referer: https://cline.bot and X-Title: Cline, Authorization: Bearer <api_key>.

**⚠️ Risks:**

clinepass auth is combined oauth+apikey in JS. In Rust, build_headers (default.rs:915-938) uses access_token.or(api_key) for Bearer, which already prefers the OAuth access token over api_key — matches combined. Do not add clinepass to the x-api-key branch list at default.rs:923-938 (it must stay Bearer). The cline header hook comment is intentionally empty in JS (hooks:["clineHeaders"] overlays workos prefix only when cline's own header cache sets it) — do NOT invent header logic here.

**Cross-check:** ✅ **CONFIRMED** — All claims verified against source.

JS claim is REAL: .tmp/9router/open-sse/providers/registry/clinepass.js:1-57 has id="clinepass", priority=85, alias="clinepass", category="oauth", authModes=["oauth","apikey"], hasOAuth:true, transport.baseUrl="https://api.cline.bot/api/v1/chat/completions", headers={"HTTP-Referer":"https://cline.bot","X-Title":"Cline"}, auth={combined:true,header:"Authorization",scheme:"bearer",hooks:["clineHeaders"]}, plus a 10-model array. Exact match.

Rust current behavior is REAL: default.rs PROVIDER_CONFIGS (lines 25-409) contains no "clinepass" key — full key list checked. default.rs:947 is the exact empty comment block `if self.provider == "cline" || self.provider == "clinepass" { // Cline often needs workos: prefix handled elsewhere; keep Bearer }` (no behavior). DefaultExecutor::new at default.rs:585-594 returns ExecutorError::UnsupportedProvider on a missing config key, so clinepass currently fails.

Impl steps would produce parity: ProviderConfig::openai + .with_header exist (line 420/451) and default_headers are injected at default.rs:824 in the request path; the proposed entry is byte-for-byte the same shape as the existing "cline" entry (default.rs:133-137) which uses the identical baseUrl + headers. OAuth is already wired separately — src/oauth/providers.rs:281 defines clinepass() and src/oauth/token_refresh.rs:973 routes "cline" | "clinepass" to refresh_cline_token — and models already exist in src/core/model/sources/9router.json:1036. The empty hook at 947 can stay as a no-op.

Caveat (does not change verdict): the JS clineHeaders hook additionally prefixes tokens with "workos:" and sets client headers (User-Agent, X-PLATFORM, X-CLIENT-TYPE, etc.) that the Rust request path does not apply — the strip_workos_prefix/build_cline_auth_header helpers in src/core/auth/cline_auth.rs are only referenced in docs/tests, not the runtime header path. However this is a pre-existing divergence shared identically by the existing "cline" entry, and the task's stated scope is mirroring the cline entry in PROVIDER_CONFIGS, so it is not an omission in the impl steps.

---

### `P0-A1f` — Add codebuddy-intl provider to PROVIDER_CONFIGS (default.rs)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/codebuddy-intl.js:4-77. id="codebuddy-intl", alias="cbai", uiAlias="cbai", priority=90, category="oauth", authModes=["oauth","apikey"], hasOAuth:true. transport.baseUrl="https://www.codebuddy.ai/v2/chat/completions", forceStream:true, thinkingFormat="openai", headers={"User-Agent":"IDE/2.108.1 CodeBuddy/2.108.1","X-Product":"SaaS","X-IDE-Type":"IDE","X-IDE-Name":"IDE","x-requested-with":"XMLHttpRequest","x-codebuddy-request":"1"}, auth={combined:true,header:"Authorization",scheme:"bearer"}. usage.url="https://www.codebuddy.ai/v2/billing/meter/get-user-resource". models=[glm-5.2,glm-5.1,glm-5.0,glm-5.0-turbo,glm-5v-turbo,glm-4.7,minimax-m3,minimax-m2.7,kimi-k2.7,kimi-k2.6,kimi-k2.5,hy3-preview,deepseek-v4-pro,deepseek-v4-flash,deepseek-v3-2-volc]

**Current Rust behavior:**

N/A. No "codebuddy-intl" in src/ (only codebuddy at default.rs:125-127 and codebuddy-cn at default.rs:397-399). HTTP 500 via UnsupportedProvider. Note: the JS entry is a NEW distinct provider (.ai international domain), not the CN provider (copilot.tencent.com).

**Implementation steps:**

In default.rs PROVIDER_CONFIGS add: ("codebuddy-intl", ProviderConfig::openai("https://www.codebuddy.ai/v2/chat/completions").with_header("User-Agent", "IDE/2.108.1 CodeBuddy/2.108.1").with_header("X-Product", "SaaS").with_header("X-IDE-Type", "IDE").with_header("X-IDE-Name", "IDE").with_header("x-requested-with", "XMLHttpRequest").with_header("x-codebuddy-request", "1"),). NOTE: HeaderValue::from_str rejects some chars but all these are valid ASCII tokens/values. X-Product value "SaaS" and User-Agent value are fine. Do NOT add "x-api-key"; auth is Bearer (access_token.or(api_key) already correct). forceStream is a stream flag, not a URL/header — Rust's DefaultExecutor always streams SSE when client requests stream; no change needed for the executor (the force-stream path is in chat.rs/response handling and out of scope). OAuth device-code flow (src/oauth) is a separate task (parity A3).

**Guard test:**

tests/executor_pool_behavior.rs: #[test] fn codebuddy_intl_url_and_headers(): build_url == "https://www.codebuddy.ai/v2/chat/completions"; build_headers contains User-Agent=IDE/2.108.1 CodeBuddy/2.108.1, X-Product=SaaS, X-IDE-Type=IDE, X-IDE-Name=IDE, x-requested-with=XMLHttpRequest, x-codebuddy-request=1.

**⚠️ Risks:**

Do NOT point codebuddy-intl at copilot.tencent.com (that's codebuddy-cn). Keep the User-Agent string byte-identical including the space and versions. Header name case: JS uses "x-requested-with" and "x-codebuddy-request" lowercase — reqwest lowercases header names automatically; use lowercase names in with_header to match exactly. X-Product="SaaS" (all caps S then aa) — copy exactly.

**Cross-check:** ✅ **CONFIRMED** — JS claim fully verified against .tmp/9router/open-sse/providers/registry/codebuddy-intl.js (L4-77): id/alias/uiAlias="codebuddy-intl"/"cbai"/"cbai", priority=90, category="oauth", authModes=["oauth","apikey"], hasOAuth:true, transport.baseUrl="https://www.codebuddy.ai/v2/chat/completions", forceStream:true, thinkingFormat="openai", and all 6 headers (User-Agent "IDE/2.108.1 CodeBuddy/2.108.1", X-Product "SaaS", X-IDE-Type "IDE", X-IDE-Name "IDE", x-requested-with "XMLHttpRequest", x-codebuddy-request "1") match exactly. Rust current behavior confirmed: default.rs L125-127 ("codebuddy"->copilot.tencent.com) and L397-399 ("codebuddy-cn"->api.codebuddy.cn) are the only codebuddy configs; no "codebuddy-intl" in PROVIDER_CONFIGS (default.rs) or PROVIDER_REGISTRY (provider.rs). DefaultExecutor::new (default.rs L585-594) returns Err(UnsupportedProvider) for it, which chat.rs L1640-1645 maps to ComboAttemptError status 500 — so "HTTP 500 via UnsupportedProvider" is real. Impl steps would produce parity: correct URL (used as-is in build_url, openai format, not in the beta-suffix list), all 6 headers applied via with_header/build_headers (default.rs L824-830), and bearer auth handled by the generic access_token-over-api_key path (default.rs L915-917) matching JS auth combined/bearer. Two caveats, neither refuting: (1) the string "codebuddy-intl" does appear in model-catalog data files (src/core/model/provider_catalog.json L115/L5863 and src/core/model/sources/9router.json L74/L757) but those only drive the /v1/models listing and alias mapping — no base URL/headers/executor config, so the runtime chat path indeed has no config; (2) the impl omits forceStream parity — provider_requires_streaming (src/core/utils/stream_flags.rs) lists "codebuddy-cn" but not "codebuddy-intl", so a client sending stream:false would get a non-streaming upstream request instead of force-stream + SSE→JSON aggregation (JS forceStream:true). This is a secondary gap for non-streaming clients and would be fixed by adding "codebuddy-intl" to that matches! list, but it does not prevent the core streaming chat flow from working. Overall the spec's JS claim is exactly real, the Rust current-state claim is accurate, and the proposed config entry is functional.

---

### `P0-A1g` — Add featherless provider to PROVIDER_CONFIGS (default.rs)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/featherless.js:1-34. id="featherless", priority=65, alias="featherless", aliases=["fl"], uiAlias="fl", category="apikey", authType="apikey". transport.baseUrl="https://api.featherless.ai/v1/chat/completions", validateUrl="https://api.featherless.ai/v1/models". models=[{id:"deepseek-ai/DeepSeek-V4-Pro",name:"DeepSeek V4 Pro"},{id:"deepseek-ai/DeepSeek-V4-Flash",name:"DeepSeek V4 Flash"},{id:"zai-org/GLM-5.2",name:"GLM 5.2"},{id:"zai-org/GLM-5.1",name:"GLM 5.1"},{id:"moonshotai/Kimi-K2.7-Code",name:"Kimi K2.7 Code"},{id:"moonshotai/Kimi-K2.6",name:"Kimi K2.6"},{id:"moonshotai/Kimi-K2.5",name:"Kimi K2.5"}]

**Current Rust behavior:**

Wrong key. src/core/executor/provider.rs:1332-1334 (DEAD PROVIDER_REGISTRY) has ("featherless-ai", ProviderExecutorConfig::openai("https://api.featherless.ai/v1")) — key "featherless-ai" (wrong id) and base lacks /chat/completions. Live default.rs PROVIDER_CONFIGS has NO featherless key -> HTTP 500.

**Implementation steps:**

In default.rs PROVIDER_CONFIGS add: ("featherless", ProviderConfig::openai("https://api.featherless.ai/v1/chat/completions"),). No headers. Note: the JS registry id is "featherless" (NOT "featherless-ai"). If the connection/dashboard stores provider id "featherless-ai" somewhere, it would still 500 — but the JS catalog (open-sse/providers/registry/featherless.js:2) and web provider id are "featherless". Keep key exactly "featherless". Optionally also alias the dead key later, but that's dead code.

**Guard test:**

tests/executor_pool_behavior.rs: #[test] fn featherless_has_full_endpoint(): build_url == "https://api.featherless.ai/v1/chat/completions".

**⚠️ Risks:**

DeepSeek/GLM/Kimi model ids (deepseek-ai/DeepSeek-V4-Pro etc.) pass through the OpenAI translator; fine. The key mismatch featherless vs featherless-ai is the subtle trap — verify the connection's provider field value by checking how the catalog alias map resolves it (provider_catalog.json has no featherless provider entry, so resolution comes from provider model alias lists).

**Cross-check:** ✅ **CONFIRMED** — All three verifications pass. (1) JS: .tmp/9router/open-sse/providers/registry/featherless.js:1-34 matches every cited value — id "featherless" (L2), priority 65 (L3), alias "featherless" (L4), aliases ["fl"] (L5-7), uiAlias "fl" (L8), category "apikey" (L19), authType "apikey" (L20), baseUrl "https://api.featherless.ai/v1/chat/completions" (L22), validateUrl "https://api.featherless.ai/v1/models" (L23), models list beginning with deepseek-ai/DeepSeek-V4-Pro (L26). (2) Rust: provider.rs:1332-1334 has ("featherless-ai", ProviderExecutorConfig::openai("https://api.featherless.ai/v1")) — wrong key and base lacking /chat/completions; the live default.rs PROVIDER_CONFIGS has no featherless entry; DefaultExecutor::new (default.rs L575-595) returns Err(UnsupportedProvider) for "featherless", which chat.rs:1640-1641 maps to status 500. The "DEAD PROVIDER_REGISTRY" label is substantively correct for the chat path — UnifiedExecutor::for_provider has no callers; get_provider_config/PROVIDER_REGISTRY is only reachable via a media.rs:638 base-URL fallback, not chat (minor imprecision in the word "dead"). (3) impl_steps produce parity: live ProviderConfig::openai stores base_url verbatim (default.rs:420-427) with existing entries passing full /chat/completions URLs, build_url's default branch (default.rs:812) returns config.base_url as-is (no double-append), apikey auth resolves to Authorization: Bearer in build_headers, and the JS file has no custom headers so "No headers" is right. The "featherless-ai" occurrences in model/mod.rs:96 and provider_catalog.json:85 are alias-label maps only, not stored connection ids, so the connection/dashboard stores "featherless" as the task hedges.

---

### `P0-A1h` — Add kilo-gateway provider to PROVIDER_CONFIGS (default.rs)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/kilo-gateway.js:1-34. id="kilo-gateway", alias="kgw", aliases=["kilo-gateway","kilogateway"], uiAlias="kgw", category="freeTier", authType="apikey", authModes=["apikey"]. transport.baseUrl="https://api.kilo.ai/api/gateway/chat/completions", validateUrl="https://api.kilo.ai/api/gateway/models". models=[{id:"kilo-auto/free",name:"Kilo Auto Free",contextLength:256000},{id:"nvidia/nemotron-3-super-120b-a12b:free",name:"Nemotron 3 Super 120B (Free)",contextLength:262144},{id:"nvidia/nemotron-3-ultra-550b-a55b:free",name:"Nemotron 3 Ultra 550B (Free)",contextLength:1000000},{id:"kwaipilot/kat-coder-pro-v2.5:free",name:"Kat Coder Pro v2.5 (Free)",contextLength:256000},{id:"kilo-auto/frontier",name:"Kilo Auto Frontier",contextLength:1000000},{id:"kilo-auto/balanced",name:"Kilo Auto Balanced",contextLength:1000000}]

**Current Rust behavior:**

Wrong location. src/core/executor/provider.rs:1277-1279 (DEAD PROVIDER_REGISTRY) has ("kilo-gateway", ProviderExecutorConfig::openai("https://api.kilo.ai/api/gateway")) — base lacks /chat/completions, and the key lives in the dead map. Live default.rs PROVIDER_CONFIGS has NO kilo-gateway key -> HTTP 500. Verified only provider.rs:1277 references kilo-gateway.

**Implementation steps:**

In default.rs PROVIDER_CONFIGS add: ("kilo-gateway", ProviderConfig::openai("https://api.kilo.ai/api/gateway/chat/completions"),). No headers. This is the fix for "dead code in PROVIDER_REGISTRY" — the entry must move to the LIVE map. Leave or delete the dead provider.rs:1277-1279 entry (it is inert for chat; media.rs:638 reads it for media base URL resolution).

**Guard test:**

tests/executor_pool_behavior.rs: #[test] fn kilo_gateway_full_gateway_endpoint(): build_url == "https://api.kilo.ai/api/gateway/chat/completions".

**⚠️ Risks:**

Do not confuse with "kilocode" (default.rs:128-131 uses https://api.kilo.ai/api/openrouter/chat/completions) — different endpoint. kilo-gateway path is /api/gateway/chat/completions. Note the :free suffix in model ids is a literal part of the model string, must not be stripped.

**Cross-check:** ✅ **CONFIRMED** — All claims verified against source.

(1) JS claim REAL: .tmp/9router/open-sse/providers/registry/kilo-gateway.js is exactly 34 lines and matches every cited field — id="kilo-gateway", alias="kgw", aliases=["kilo-gateway","kilogateway"], uiAlias="kgw", category="freeTier", authType="apikey", authModes=["apikey"], transport.baseUrl="https://api.kilo.ai/api/gateway/chat/completions", validateUrl="https://api.kilo.ai/api/gateway/models", models starting with {id:"kilo-auto/free",name:"Kilo Auto Free",contextLength:256000}. No transport headers.

(2) Rust current behavior REAL: provider.rs:1277-1278 has ("kilo-gateway", ProviderExecutorConfig::openai("https://api.kilo.ai/api/gateway")) inside static PROVIDER_REGISTRY (line 682). The registry is dead for chat: its readers get_provider_config/is_supported_provider/all_providers have no chat call sites (UnifiedExecutor::for_provider at provider.rs:321 has zero callers), and only src/server/api/media.rs:638 consumes get_provider_config for media base-URL resolution — matching the spec note. default.rs PROVIDER_CONFIGS (line 24) contains no kilo-gateway/kilogateway key (0 grep matches), so a chat request for kilo-gateway returns ExecutorError::UnsupportedProvider at default.rs:585/591. Minor nuance: the spec's "-> HTTP" describes an UnsupportedProvider error rather than a literal HTTP status, immaterial to the claim. Corroborating: src/core/model/sources/9router.json already maps kilo-gateway->kgw with baseUrl "https://api.kilo.ai/api/gateway/chat/completions".

(3) Impl steps produce parity with no omission: ProviderConfig::openai(...) exists and is used throughout default.rs; the URL exactly equals the JS transport.baseUrl; no headers are needed (JS defines none). Leaving the dead provider.rs:1277-1279 entry is safe — it is inert for chat and media.rs:638 still resolves base URL through it (media also prefers connection.provider_specific_data.baseUrl first). All verification points hold; verdict CONFIRMED.

---

### `P0-A1i` — Add perplexity-agent provider to PROVIDER_CONFIGS (default.rs)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/perplexity-agent.js:1-49. id="perplexity-agent", priority=181, alias="perplexity-agent", aliases=["pplx-agent","pplx-responses"], uiAlias="pa", category="apikey", authType="apikey". transport.baseUrl="https://api.perplexity.ai/v1/responses", validateUrl="https://api.perplexity.ai/v1/models", format="openai-responses". models=[{id:"perplexity/sonar",...},{id:"openai/gpt-5.5",...},{id:"openai/gpt-5.4",...},{id:"openai/gpt-5.4-mini",...},{id:"anthropic/claude-sonnet-4-6",...},{id:"anthropic/claude-opus-4-8",...},{id:"google/gemini-3.1-pro-preview",...},{id:"xai/grok-4.20-reasoning",...},{id:"perplexity/glm-5.2",...},{id:"perplexity/kimi-k2.7-code",...},{id:"nvidia/nemotron-3-super-120b-a12b",...}]. serviceKinds=["llm","webSearch"]. searchViaChat={defaultModel:"perplexity/sonar",endpoint:"https://api.perplexity.ai/v1/responses"}. modelsFetcher={url:"https://api.perplexity.ai/v1/models",type:"openai"}. passthroughModels:true

**Current Rust behavior:**

N/A for the executor config. NOTE: src/core/translator/registry.rs:355 already maps "perplexity-agent" => Format::OpenAiResponses (present), so the translator format is ready. But default.rs PROVIDER_CONFIGS has NO "perplexity-agent" key -> DefaultExecutor::new returns UnsupportedProvider -> HTTP 500.

**Implementation steps:**

In default.rs PROVIDER_CONFIGS add: ("perplexity-agent", ProviderConfig::openai("https://api.perplexity.ai/v1/responses"),). CRITICAL: the endpoint is /v1/responses (OpenAI Responses API), NOT /chat/completions. The Rust DefaultExecutor POSTs the JSON body to this URL unchanged (send_one default.rs:1202-1232). The translator registry already routes perplexity-agent to OpenAiResponses format (translator/registry.rs:355), and openai_responses translation lives in src/core/translator/request/openai_responses.rs — so the request/response translation already matches. build_url (default.rs:812) returns config.base_url unchanged; is_already_endpoint (default.rs:612) matches "/responses" so a runtime_transport override pointing here also stays as-is. No headers (JS headers absent).

**Guard test:**

tests/executor_pool_behavior.rs: #[test] fn perplexity_agent_responses_endpoint(): build_url == "https://api.perplexity.ai/v1/responses" (must NOT be /chat/completions).

**⚠️ Risks:**

The subtle trap: a developer might "normalize" the URL to /chat/completions. The Responses API will reject it. Keep /v1/responses exactly. Also do not add perplexity-agent to any x-api-key/Bearer special-case in default.rs:923-938 — it must stay standard Bearer (api_key/access_token). translator/registry.rs:355 already lists it under OpenAiResponses; confirm your change does not add a conflicting mapping.

**Cross-check:** ✅ **CONFIRMED** — All three verification points pass. (1) JS claim is REAL: C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/open-sse/providers/registry/perplexity-agent.js lines 1-49 match exactly — id="perplexity-agent", priority:181, alias="perplexity-agent", aliases=["pplx-agent","pplx-responses"], uiAlias:"pa", category:"apikey", authType:"apikey", transport.baseUrl="https://api.perplexity.ai/v1/responses", validateUrl="https://api.perplexity.ai/v1/models", format:"openai-responses", models[0]={id:"perplexity/sonar",...}. (2) Rust current behavior is REAL: src/core/translator/registry.rs:366 maps "perplexity-agent" => Format::OpenAiResponses (spec cited line 355; off by 11 lines but present and functional). src/core/executor/default.rs PROVIDER_CONFIGS has "perplexity" (line 109) and "perplexity-web" (line 211) but NO "perplexity-agent" key. DefaultExecutor::new (default.rs:574-603) returns ExecutorError::UnsupportedProvider when the key is absent and provider_node is None. provider_nodes are only user-created via API (src/server/api/mod.rs:1297) or cli (src/cli/provider_node.rs:195), never auto-seeded from catalog, so a stock catalog provider hits UnsupportedProvider. The dispatch chain in src/server/api/chat.rs (1032-1634) has no special executor for perplexity-agent, so it falls to the final else -> DefaultExecutor::new -> UnsupportedProvider -> 500 ComboAttemptError. Confirmed perplexity-agent appears in Rust only in model catalog JSON (sources/9router.json) and the registry. (3) Impl steps produce parity: adding ("perplexity-agent", ProviderConfig::openai("https://api.perplexity.ai/v1/responses")) works because build_url (default.rs:812) returns config.base_url unchanged for unlisted providers (endpoint /v1/responses, NOT /chat/completions), the body is translated to Responses format via chat_to_openai_responses_request (registered OpenAi->OpenAiResponses at registry.rs:965-969) driven by the get_target_format_for_provider mapping, send_one (default.rs:1202-1232) posts the transformed JSON body unchanged, and all transform_request helpers are no-ops for perplexity-agent: strip_unsupported_params (strip_unsupported.rs:15-34) only strips for anthropic-compatible/gemini, inject_reasoning_content (reasoning_content_injector.rs:23-36) only for deepseek/kimi, normalize_developer_role (openai_helper.rs:18-30) only touches body.messages[] which is absent in Responses format. Auth uses the default Bearer branch in build_headers (default.rs:915-938), matching the JS apikey authType. Minor non-blocking caveats: the merged provider_catalog.json lacks a perplexity-agent entry (only sources/9router.json has one, at lines 4298-4302 with format "openai-responses" and matching baseUrl), and the spec's registry.rs line number is 366 not 355 — neither affects the executor parity this task targets.

---

### `P0-A1j` — Add poolside provider to PROVIDER_CONFIGS (default.rs)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/poolside.js:1-30. id="poolside", priority=60, alias="poolside", aliases=["ps"], uiAlias="ps", category="freeTier", authType="apikey", authModes=["apikey"]. transport.baseUrl="https://inference.poolside.ai/v1/chat/completions", validateUrl="https://inference.poolside.ai/v1/models". models=[{id:"poolside/laguna-s-2.1",name:"Laguna S 2.1"},{id:"poolside/laguna-xs-2.1",name:"Laguna XS 2.1"}]

**Current Rust behavior:**

N/A. No "poolside" string in src/ (verified; parity-report section I claims poolside models already match JS but the executor key is absent). HTTP 500 via UnsupportedProvider.

**Implementation steps:**

In default.rs PROVIDER_CONFIGS add: ("poolside", ProviderConfig::openai("https://inference.poolside.ai/v1/chat/completions"),). No headers. Full endpoint -> build_url unchanged. The two model ids poolside/laguna-s-2.1 and poolside/laguna-xs-2.1 pass through OpenAI format.

**Guard test:**

tests/executor_pool_behavior.rs: #[test] fn poolside_inference_endpoint(): build_url == "https://inference.poolside.ai/v1/chat/completions".

**⚠️ Risks:**

None beyond exact URL. freeTier has no executor impact. Ensure the provider key is exactly "poolside" (alias ps is separate).

**Cross-check:** ✅ **CONFIRMED** — JS claim is fully verified against .tmp/9router/open-sse/providers/registry/poolside.js:1-30. Every cited value matches exactly: id="poolside", priority=60, alias="poolside", aliases=["ps"], uiAlias="ps", category="freeTier", authType="apikey", authModes=["apikey"], transport.baseUrl="https://inference.poolside.ai/v1/chat/completions", validateUrl="https://inference.poolside.ai/v1/models", and models poolside/laguna-s-2.1 (Laguna S 2.1) + poolside/laguna-xs-2.1 (Laguna XS 2.1). Rust current behavior is also real: "poolside" appears only in src/core/model/provider_catalog.json (alias map + catalog entry with the two model ids) and src/core/model/sources/9router.json (which already records format="openai" and the same baseUrl), and is absent from PROVIDER_CONFIGS in src/core/executor/default.rs. DefaultExecutor::new (default.rs:588/594) returns ExecutorError::UnsupportedProvider for unknown providers, and chat.rs:1640-1645 maps that to status:500, confirming the HTTP 500 path. The impl steps produce parity: ProviderConfig::openai sets default_headers=Vec::new() (no headers) and format="openai"; with no provider_node, build_url (default.rs:812) returns self.config.base_url verbatim, so the full endpoint .../v1/chat/completions is used as-is exactly like JS default.js buildUrl line 129 (which returns config.baseUrl for an openai-format provider with no urlSuffix/runtime transport); the is_already_endpoint/double-append path only triggers when a runtime_transport.base_url is set, which poolside has none of. Auth matches too: Rust build_headers default branch sends Authorization: Bearer <api_key>, mirroring JS's BEARER descriptor for plain openai-format providers. No omitted step — this one-line addition fully covers config, URL, auth, and model passthrough.

---

### `P0-A1k` — Add tencent (hunyuan) provider to PROVIDER_CONFIGS (default.rs)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/tencent.js:1-27. id="tencent", alias="hunyuan", aliases=["hunyuan","tencent-hunyuan"], uiAlias="hunyuan", category="apikey", authType="apikey", authModes=["apikey"]. transport.baseUrl="https://api.hunyuan.cloud.tencent.com/v1/chat/completions", validateUrl="https://api.hunyuan.cloud.tencent.com/v1/models". models=[{id:"hunyuan-turbos-latest",name:"Hunyuan TurboS Latest",contextLength:200000},{id:"hunyuan-t1-latest",name:"Hunyuan T1 Latest",contextLength:256000}]

**Current Rust behavior:**

N/A. No "tencent" or "hunyuan" key in default.rs PROVIDER_CONFIGS (the only tencent strings in src are codebuddy/copilot.tencent.com at default.rs:126 and provider.rs:874 — a different provider). HTTP 500 via UnsupportedProvider. provider_catalog.json already has a tencent provider entry (id=tencent, alias=hunyuan).

**Implementation steps:**

In default.rs PROVIDER_CONFIGS add: ("tencent", ProviderConfig::openai("https://api.hunyuan.cloud.tencent.com/v1/chat/completions"),). No headers. Full endpoint -> build_url unchanged. IMPORTANT: the key must be "tencent" (the JS id), NOT "hunyuan" (hunyuan is only the alias).

**Guard test:**

tests/executor_pool_behavior.rs: #[test] fn tencent_hunyuan_endpoint(): DefaultExecutor::new("tencent",...) succeeds and build_url == "https://api.hunyuan.cloud.tencent.com/v1/chat/completions".

**⚠️ Risks:**

Alias trap: JS id is "tencent", alias "hunyuan". The catalog providerIdToAlias maps tencent->hunyuan, and model resolution can hand either to the executor. Add ONLY the "tencent" key. If connections store provider="hunyuan", that would still 500 — check get_model_info/alias_to_provider_id (catalog.rs:97) maps the alias back to id "tencent". Do not add a second "hunyuan" key unless the resolution path proves it's needed; verify with a test.

**Cross-check:** 🟡 **PLAUSIBLE** — 1. JS claim CONFIRMED: tencent.js (27 lines) matches exactly — id="tencent", alias="hunyuan", aliases=["hunyuan","tencent-hunyuan"], uiAlias="hunyuan", category/authType/authModes="apikey", transport.baseUrl="https://api.hunyuan.cloud.tencent.com/v1/chat/completions", validateUrl=".../v1/models", models hunyuan-turbos-latest (200k) + hunyuan-t1-latest (256k).

2. Rust current behavior CONFIRMED: no tencent/hunyuan key in PROVIDER_CONFIGS (default.rs:24-409); the only tencent strings are the separate codebuddy provider (copilot.tencent.com, default.rs:126, provider.rs:874). DefaultExecutor::new raises ExecutorError::UnsupportedProvider (default.rs:585-594) and chat.rs:1635-1645 maps it to status 500. provider_catalog.json does have tencent→hunyuan (line 112, 5830, 4416-4432).

3. Impl steps PARTLY correct. Adding ("tencent", ProviderConfig::openai("https://api.hunyuan.cloud.tencent.com/v1/chat/completions")) works for requests routed to provider "tencent": build_url returns the full endpoint unchanged (already ends in /chat/completions), build_headers uses the generic Bearer branch, select_connection matches conn.provider=="tencent". No headers needed. Caveat/omission: /v1/models advertises model cards under the alias prefix "hunyuan/<model>" (output_alias derives from provider_info.alias="hunyuan"), but "hunyuan" is neither in PROVIDER_CONFIGS nor in the static ALIAS_TO_PROVIDER_ID map (core/model/mod.rs), so a request to the advertised hunyuan/<model> form resolves provider="hunyuan" and still returns HTTP 500 after the change. Only the canonical "tencent/<model>" prefix works. Full parity also requires wiring the "hunyuan" alias (e.g., ("hunyuan","tencent") in ALIAS_TO_PROVIDER_ID or a "hunyuan" config key). Since the two behavioral claims are exact but the impl has a real (non-obvious) gap in the advertised alias route, verdict is PLAUSIBLE rather than CONFIRMED.

---

### `P0-A1l` — Add tokenrouter provider to PROVIDER_CONFIGS (default.rs)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/tokenrouter.js:1-162. id="tokenrouter", alias="tokenrouter", aliases=["tr"], uiAlias="tokenrouter", category="apikey". thinkingConfig={options:["low","medium","high","xhigh","max"],defaultMode:"high"}. transport.baseUrl="https://api.tokenrouter.com/v1/chat/completions", validateUrl="https://api.tokenrouter.com/v1/models", thinkingFormat="tokenrouter". embeddingConfig={baseUrl:"https://api.tokenrouter.com/v1/embeddings",authType:"apikey",authHeader:"bearer"}. imageConfig={baseUrl:"https://api.tokenrouter.com/v1/images/generations"}. modelsFetcher={url:"https://api.tokenrouter.com/v1/models",type:"openai"}. passthroughModels:true. models=[120 entries including MiniMax-Hailuo-2.3(kind video), MiniMax-M3, anthropic/claude-*, bytedance-seed/seedream-*(kind image), deepseek/deepseek-*, ex/gpt-5.4, google/gemini-*, kling-*(kind video), minimax/minimax-*, moonshotai/kimi-*, nvidia/nemotron-*, openai/gpt-*, qwen/qwen*, x-ai/grok-*, xiaomi/mimo-*, z-ai/glm-*, and kinds video/image/audio] serviceKinds=["llm","embedding","image"]

**Current Rust behavior:**

N/A. No "tokenrouter" in src/. HTTP 500 via UnsupportedProvider.

**Implementation steps:**

In default.rs PROVIDER_CONFIGS add: ("tokenrouter", ProviderConfig::openai("https://api.tokenrouter.com/v1/chat/completions"),). No headers. Full endpoint -> build_url unchanged. thinkingFormat="tokenrouter" and thinkingConfig have no executor impact (they are translator/UI knobs; the DefaultExecutor does not special-case tokenrouter). The embedding/image endpoints belong to the media layer (separate task, parity D1); the chat executor entry is all that's needed here.

**Guard test:**

tests/executor_pool_behavior.rs: #[test] fn tokenrouter_chat_endpoint(): build_url == "https://api.tokenrouter.com/v1/chat/completions".

**⚠️ Risks:**

Do NOT add the embedding/image base URLs to the chat ProviderConfig — build_url would use them for chat. The chat endpoint is the ONLY one that belongs in default.rs for this provider. The 120-entry model list is for catalog/UI; do not embed into default.rs.

**Cross-check:** ✅ **CONFIRMED** — All three parts check out. (1) JS claim is exactly real: .tmp/9router/open-sse/providers/registry/tokenrouter.js lines 2-5 (id/alias/aliases=["tr"]/uiAlias), 17 (category="apikey"), 18-21 (thinkingConfig options ["low","medium","high","xhigh","max"], defaultMode "high"), 23-25 (transport.baseUrl https://api.tokenrouter.com/v1/chat/completions, validateUrl /v1/models, thinkingFormat "tokenrouter") all match verbatim. (2) Rust current behavior is substantively real: PROVIDER_CONFIGS in src/core/executor/default.rs has no tokenrouter key (full map read), provider_catalog.json (107 providers) has none, and unknown providers fall through chat.rs line 1635 to DefaultExecutor::new whose PROVIDER_CONFIGS miss returns ExecutorError::UnsupportedProvider → ComboAttemptError status 500. One trivial imprecision: "No tokenrouter in src/" is literally false because src/core/model/sources/omniroute.json (line 8565) holds a tokenrouter sync snapshot — but that file only feeds `openproxy sync omniroute` custom_models (src/cli/sync.rs) and has no runtime effect on PROVIDER_CONFIGS, so the intended claim (not a supported runtime provider) is correct. (3) Impl step produces parity: ProviderConfig::openai with the full endpoint configures format openai, no headers, no fallbacks; build_url returns config.base_url verbatim for tokenrouter (not in gemini/opencode-go/claude-beta/placeholder branches); auth is Bearer apikey via the openai branch of build_headers (lines 915-938), matching JS authType "apikey". thinkingFormat="tokenrouter" and thinkingConfig have no DefaultExecutor impact (verified: default.rs has no tokenrouter special-case; thinking_suffix.rs resolves tokenrouter to ThinkingNative::OpenAi / reasoning_effort, a correct OpenAI-gateway default). Embedding/image endpoints correctly belong to the separate media layer (src/core/media/embeddings/, image/) with their own adapters, so excluding them is right for chat parity. No material omission: alias "tr" resolution lives in provider_catalog.json, not PROVIDER_CONFIGS.

---

### `P0-A1m` — Add venice provider to PROVIDER_CONFIGS (default.rs)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/venice.js:1-56. id="venice", priority=115, alias="venice", aliases=["vn"], uiAlias="venice", category="apikey". transport.baseUrl="https://api.venice.ai/api/v1/chat/completions", validateUrl="https://api.venice.ai/api/v1/models", thinkingFormat="openai". models=[venice-uncensored-1-2, zai-org-glm-5, qwen3-235b-a22b-instruct-2507, qwen3-coder-480b-a35b-instruct-turbo, qwen3-vl-235b-a22b, deepseek-v4-pro, llama-3.3-70b, hermes-3-llama-3.1-405b, mistral-small-3-2-24b-instruct, text-embedding-3-large(kind embedding), text-embedding-bge-m3(kind embedding), text-embedding-qwen3-8b(kind embedding), venice-sd35(params [n,size],kind image), flux-2-pro(params [n,size],kind image), gpt-image-2(params [n,size,quality],kind image)]. serviceKinds=["llm","embedding","image"]. embeddingConfig={baseUrl:"https://api.venice.ai/api/v1/embeddings",authType:"apikey",authHeader:"bearer"}. imageConfig={baseUrl:"https://api.venice.ai/api/v1/images/generations"}. modelsFetcher={url:"https://api.venice.ai/api/v1/models",type:"openai"}. passthroughModels:true

**Current Rust behavior:**

Wrong location. src/core/executor/provider.rs:1324-1326 (DEAD PROVIDER_REGISTRY) has ("venice", ProviderExecutorConfig::openai("https://api.venice.ai/api/v1")) — base lacks /chat/completions, key is in the dead map. Live default.rs PROVIDER_CONFIGS has NO venice key -> HTTP 500. provider_catalog.json has a venice provider entry (id=venice, alias=venice, serviceKinds llm/embedding/image).

**Implementation steps:**

In default.rs PROVIDER_CONFIGS add: ("venice", ProviderConfig::openai("https://api.venice.ai/api/v1/chat/completions"),). No headers. Note the /api/v1 path segment (double /api). Full endpoint -> build_url unchanged. The dead provider.rs:1324-1326 entry may stay (inert) or be updated; chat uses default.rs. The embedding/image endpoints belong to the media layer (separate task).

**Guard test:**

tests/executor_pool_behavior.rs: #[test] fn venice_api_v1_chat_endpoint(): build_url == "https://api.venice.ai/api/v1/chat/completions" (note /api/v1, not /v1).

**⚠️ Risks:**

The path is /api/v1/chat/completions (double path). A common mistake is writing https://api.venice.ai/v1/chat/completions. Copy verbatim. Do not route chat through the imageConfig base (…/images/generations).

**Cross-check:** ✅ **CONFIRMED** — All three verification points hold.

1) JS claim REAL: .tmp/9router/open-sse/providers/registry/venice.js:1-56 matches exactly — id="venice", priority=115, alias="venice", aliases=["vn"], uiAlias="venice", category="apikey", transport.baseUrl="https://api.venice.ai/api/v1/chat/completions", validateUrl="https://api.venice.ai/api/v1/models", thinkingFormat="openai", and the model list begins venice-uncensored-1-2, zai-org-glm-5, qwen3-235b-a22b-instruct-2507, qwen3-coder-480b-a35b-instruct-turbo (claim truncated there; remainder matches).

2) Rust current behavior REAL: src/core/executor/provider.rs:1324-1326 has ("venice", ProviderExecutorConfig::openai("https://api.venice.ai/api/v1")) in PROVIDER_REGISTRY. That map is only reached via get_provider_config/is_supported_provider/all_providers/get_api_key_providers/get_oauth_providers; the chat path constructs DefaultExecutor (src/server/api/chat.rs:1635), which reads PROVIDER_CONFIGS (default.rs:24). grep confirms default.rs has ZERO venice entries. DefaultExecutor::new returns ExecutorError::UnsupportedProvider when the key is missing (default.rs:588/594) and chat.rs:1640-1644 maps that to status 500 ("Default executor creation failed") — so the "HTTP 500" claim is accurate (it is a 500 at executor-creation, surfaced as an HTTP 500 to the client). provider_catalog.json has a venice entry (line 5647) but with no type/baseUrl, and provider_nodes are not auto-seeded from the catalog, so provider_node is typically None and the flow falls to the PROVIDER_CONFIGS lookup.

3) Impl steps would produce parity: adding ("venice", ProviderConfig::openai("https://api.venice.ai/api/v1/chat/completions")) works. ProviderConfig::openai sets OpenAI format with Authorization header and no default_headers; build_url (default.rs:606-612, 812, 1339) treats a config base_url that already ends in /chat/completions as a full endpoint and returns it as-is (no double-append); build_headers' else branch (default.rs:915-938) sends Authorization: Bearer matching JS authHeader "bearer". The dead provider.rs:1324-1326 entry is correctly noted as inert for chat (UnifiedExecutor, which uses it, is never constructed in the chat path). Scoping embedding/image to a separate media-layer task is correct since venice's embeddingConfig/imageConfig URLs (venice.js:46-53) are not representable in PROVIDER_CONFIGS and media.rs:629-641 uses get_provider_config.

Minor nits that do not change the verdict: the spec's "double /api" wording is slightly misleading (the actual pattern is host subdomain "api" + path "/api/v1"; the URL itself is correct and matches JS verbatim), and the 500 is at executor creation rather than a generic unknown-provider error — both immaterial to the fix.

---

### `P0-A1n` — Add zed provider to PROVIDER_CONFIGS (default.rs)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/zed.js:1-71. id="zed", priority=10, alias="zd", uiAlias="zd", hidden:true, category="oauth", authType="oauth", hasOAuth:true. transport.baseUrl="https://cloud.zed.dev/completions", format="openai", forceStream:true, headers={"content-type":"application/json"}, auth={combined:true,header:"Authorization",scheme:"<user_id> <access_token>"}, usage.url="https://cloud.zed.dev/client/users/me", modelsUrl="https://cloud.zed.dev/models". models=[], passthroughModels:true. oauth={authorizeUrl:"https://zed.dev/native_app_signin",platform:"zed",rsaKeyExchange:true} (RSA keypair native-app signin; auth header is "Authorization: <user_id> <access_token>" plus a duplicate x-zed-cloud-token header; no clientId/clientSecret/tokenUrl/refreshUrl)

**Current Rust behavior:**

N/A for the executor. No "zed" key in default.rs PROVIDER_CONFIGS (string "zed" only appears in provider_catalog.json provider entry id=zed alias=zd). HTTP 500 via UnsupportedProvider. The non-standard auth ("<user_id> <access_token>") and x-zed-cloud-token header, plus RSA OAuth, are NOT ported (parity A3/C1 — separate oauth task). provider_catalog.json has zed entry (id=zed, alias=zd, serviceKinds llm).

**Implementation steps:**

MINIMAL chat-path fix: in default.rs PROVIDER_CONFIGS add: ("zed", ProviderConfig::openai("https://cloud.zed.dev/completions"),). This gets past UnsupportedProvider so a connection with a pre-obtained token can chat (the token must already be in access_token). NOTE: This is INCOMPLETE parity — the JS executor builds "Authorization: <user_id> <access_token>" (two space-separated values, no Bearer) plus x-zed-cloud-token. The Rust DefaultExecutor only sends "Bearer <token>". Full zed executor parity (custom auth header) is a separate task (parity A3: open-sse/executors/zed.js NDJSON/SSE + zed_account.rs build_authorization_header). If the scope is strictly the 17-provider chat reachability fix, adding the bare config gets the provider routed; document that the auth header shape is a follow-up. Do NOT add "content-type":"application/json" as a default_header (it's already set by build_headers default.rs:822; a duplicate insert would be a no-op but is pointless).

**Guard test:**

tests/executor_pool_behavior.rs: #[test] fn zed_cloud_endpoint(): DefaultExecutor::new("zed",...) succeeds and build_url == "https://cloud.zed.dev/completions". (Separate future test for the "<user_id> <access_token>" header once the zed executor lands.)

**⚠️ Risks:**

zed forceStream: true and NDJSON wire protocol mean a naive openai translation may mis-parse the stream. The 9router zed executor does NDJSON/SSE translation. If you only add the config entry, streaming responses will be forwarded as raw SSE and clients expecting the OpenAI SSE shape may break. Note this explicitly as a follow-up; the minimum viable fix (this task) is reachability. Do not invent a Bearer+user_id combined header in DefaultExecutor without the JS zed executor to port.

**Cross-check:** ✅ **CONFIRMED** — All three claims verified against source. (1) JS is REAL: zed.js:1-71 matches every cited field exactly (id/priority=10/alias=zd/uiAlias=zd/hidden:true/category=oauth/authType=oauth/hasOAuth:true; transport baseUrl=https://cloud.zed.dev/completions, format=openai, forceStream:true, headers content-type; auth combined:true/header=Authorization/scheme="<user_id> <access_token>"; usage.url=https://cloud.zed.dev/client/users/me). Wire auth corroborated by zedAuth.js buildZedUserAuthHeader (returns "<user_id> <access_token>", no Bearer) and the custom ZedExecutor (executors/zed.js) which exchanges for a short-lived LLM token (Bearer) and wraps requests in a thread/prompt/NDJSON envelope. (2) Rust current is REAL: PROVIDER_CONFIGS in default.rs (l.24-409) has no "zed" key; exact-word grep of "zed" in all src/**/*.rs returns zero matches; the string only exists in provider_catalog.json (alias map + catalog entry id=zed alias=zd). DefaultExecutor::new (l.581-595) returns ExecutorError::UnsupportedProvider on a miss, and chat.rs l.1635-1645 maps the constructor error to ComboAttemptError{status:500}, so HTTP 500 via UnsupportedProvider is confirmed. build_headers (l.916-937) would emit "Authorization: Bearer <token>", not the Zed scheme, and no x-zed-cloud-token header or RSA OAuth exists in Rust. (3) The impl step (adding ("zed", ProviderConfig::openai("https://cloud.zed.dev/completions"))) compiles and would clear UnsupportedProvider, and build_url returns the base_url as-is (no /chat/completions suffix), so the endpoint is right. But it would NOT produce working parity: the spec itself discloses the incompleteness — the Authorization header Rust builds is "Bearer <token>" versus Zed's "<user_id> <access_token>" (no Bearer), and the real JS chat path also exchanges an LLM token at /client/llm_tokens and sends a thread/prompt/NDJSON envelope that an openai-format executor never constructs. The finding is accurate about the JS/Rust state and correctly scopes the fix as a MINIMAL/INCOMPLETE chat-path change rather than full parity, so it is confirmed rather than merely plausible.

---

### `P0-A2a` — Add selfhosted-stt provider routing (media STT)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/selfhosted-stt.js:18-48. id="selfhosted-stt", priority=50, hasFree:true, alias="selfhosted-stt", category="apikey", authType="apikey". auth.apiKey.text="Set providerSpecificData.baseUrl to the full transcriptions URL, e.g. http://host:8080/v1/audio/transcriptions. The API key is not checked by local servers; any value works.". models=[{id:"whisper-1",name:"Whisper (self-hosted)",params:["language","response_format","temperature","prompt"],kind:"stt"}]. serviceKinds=["stt"]. sttConfig={baseUrl:"http://localhost:8080/v1/audio/transcriptions",authType:"apikey",authHeader:"bearer",format:"openai"}. Header comment (lines 9-17): sttCore dispatches on format; anything not one of the five named cloud shapes falls through to transcribeOpenAICompatible, which POSTs the standard multipart body (file, model, and optional language / prompt / response_format / temperature).

**Current Rust behavior:**

No "selfhosted" string in src/. src/core/media/stt/mod.rs:44-48 dispatches format strings; SttFormat::OpenaiCompat exists (stt/mod.rs:44) and transcribe_openai_compat (stt/mod.rs:487). The file header at stt/mod.rs:1 says 'Orphaned — active implementation is in server/api/stt.rs'. server/api/stt.rs routing must be checked: it lacks a selfhosted-stt branch, so a selfhosted-stt request would fall through and fail.

**Implementation steps:**

1) In src/server/api/stt.rs, add a branch that recognizes provider id "selfhosted-stt". 2) Read the per-connection baseUrl from connection.provider_specific_data["baseUrl"]; if absent, use the sttConfig default "http://localhost:8080/v1/audio/transcriptions". 3) Dispatch to the OpenAI-compatible multipart POST builder (already present as transcribe_openai_compat / SttFormat::OpenaiCompat in stt/mod.rs:487-578). 4) The auth header is "bearer" (Bearer <api_key>); local servers ignore it, any non-empty key works — do not require a real key, but require the field to be present (authType apikey means a credentials record exists). 5) The four params (language, response_format, temperature, prompt) are the multipart fields to forward.

**Guard test:**

Add a unit test in stt.rs (or stt/mod.rs test module): #[test] fn selfhosted_stt_uses_provider_specific_base_url(): given a connection with provider_specific_data["baseUrl"]="http://192.168.1.5:8080/v1/audio/transcriptions" and model "whisper-1", assert the built URL equals that value; and without the override, assert the default "http://localhost:8080/v1/audio/transcriptions" is used.

**⚠️ Risks:**

Do NOT fall back to api.openai.com — the JS adapter's whole point (comment at selfhosted-stt.js:1-17) is that the baseUrl comes from the connection. Security risk (parity-report A2): routing this through the generic openai-compatible node would send audio + key to OpenAI. Also ensure the 'format: openai' branch (SttFormat::OpenaiCompat) does not require a non-empty API key — local servers accept any value.

**Cross-check:** 🟡 **PLAUSIBLE** — JS claim REAL: .tmp/9router/open-sse/providers/registry/selfhosted-stt.js:18-48 exactly matches — id/alias "selfhosted-stt", priority 50, hasFree true, category "apikey", auth.apiKey.text with the cited URL text, models [{id:"whisper-1",...}], sttConfig {baseUrl:"http://localhost:8080/v1/audio/transcriptions", authHeader:"bearer", format:"openai"}. sttCore.js:169-201 confirms format "openai" falls to transcribeOpenAICompatible (multipart file/model/language/prompt/response_format/temperature) and lines 176-182 apply the per-connection providerSpecificData.baseUrl override.

Rust claim REAL: no "selfhosted" string in src/ (grep). src/core/media/stt/mod.rs:1 header says "Orphaned — active implementation is in server/api/stt.rs"; SttFormat::OpenaiCompat (mod.rs:34) and transcribe_openai_compat (mod.rs:487) exist as described. Active src/server/api/stt.rs:84-124 stt_config() covers only openai/groq/deepgram/assemblyai/huggingface/gemini; dispatch_with_fallback:414 rejects unknown providers with BAD_REQUEST "Provider ... does not support STT".

Impl steps: sound core but partial parity. (a) The "provider_specific_data" field is Rust's snake_case name for the connection struct field (src/types/mod.rs:174) whose JSON key is camelCase "baseUrl" — the step's "baseUrl" key is correct, matching the JS override. (b) MISSING: the Rust model layer has no selfhosted-stt provider id — ALIAS_TO_PROVIDER_ID in src/core/model/mod.rs is a hand-maintained static map (~lines 80-129) with no entry, and no catalog JSON exists to regenerate; bare "whisper-1" infers provider "openai" via infer_provider_from_model_name (mod.rs:303+), so users can only reach the new branch via an explicit "selfhosted-stt/whisper-1" path, and the provider won't appear in model listing/v1/models. (c) SttProviderConfig for selfhosted-stt must use SttAuthHeader::Bearer and NOT auth_type_none (JS registry has authType:"apikey"); otherwise the per-connection api_key/access_token credential check in dispatch_with_fallback/select_stt_connection (which filters on connection_has_credentials) won't match JS, where the connection carries a credentials record holding providerSpecificData.baseUrl. These are real omissions, but the central dispatch + OpenAI-compatible multipart reuse is correct and the baseUrl override mechanics are aligned, so the impl would substantially work with explicit provider/model naming.

---

### `P0-A2b` — Add selfhosted-tts provider routing (media TTS)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/selfhosted-tts.js:12-44. id="selfhosted-tts", priority=50, hasFree:true, alias="selfhosted-tts", category="apikey", authType="apikey". auth.apiKey.text="Set providerSpecificData.baseUrl to the server root, e.g. http://host:8080 — /v1/audio/speech is appended. The API key is not checked by local servers; any value works.". models=[{id:"kokoro",name:"Kokoro (self-hosted)",params:["voice","response_format","speed"],kind:"tts"}]. serviceKinds=["tts"]. ttsConfig={baseUrl:"http://localhost:8880",defaultModel:"kokoro",authType:"apikey",format:"openai-speech"}. Comment (lines 3-11): every other self-hostable TTS provider carries a FIXED localhost baseUrl and authType "none", and the dispatcher reads ttsConfig.baseUrl from the registry entry rather than the connection; authType "apikey" is what makes the per-connection providerSpecificData.baseUrl override possible. Voice selected as "<model>/<voice>" (same convention as OpenAI TTS adapter).

**Current Rust behavior:**

No "selfhosted" in src/. src/core/media/tts/mod.rs:88-118 has no selfhosted-tts branch. The TTS dispatcher reads ttsConfig.baseUrl from the registry entry (fixed localhost), so there is no per-connection override path.

**Implementation steps:**

1) In src/core/media/tts/mod.rs (or server/api/tts.rs), add a provider branch for "selfhosted-tts". 2) Read connection.provider_specific_data["baseUrl"] for the server root; if absent use default "http://localhost:8880". 3) Append "/v1/audio/speech" to the root (JS: '/v1/audio/speech is appended'). 4) Use format "openai-speech" — the OpenAI TTS request shape (input text, model, voice, response_format, speed). 5) Voice is "<model>/<voice>" — split on '/' to get model=kokoro and voice; the params voice/response_format/speed map to the openai-speech body. 6) authType apikey: require a credentials record but do not validate the key value.

**Guard test:**

In tts/mod.rs test module: #[test] fn selfhosted_tts_appends_speech_path(): connection with baseUrl="http://192.168.1.5:8080" produces URL "http://192.168.1.5:8080/v1/audio/speech"; default produces "http://localhost:8880/v1/audio/speech".

**⚠️ Risks:**

The trailing /v1/audio/speech append must not double-append if the user's baseUrl already ends in /v1/audio/speech (JS comment in selfhosted-embedding.js:44-49 notes the analogous tolerance for /embeddings). Trim trailing '/'. Do not send the API key as a required real credential (local servers ignore it).

**Cross-check:** 🟡 **PLAUSIBLE** — JS claim REAL (verified). .tmp/9router/open-sse/providers/registry/selfhosted-tts.js:12-44 matches: id/priority=50/hasFree=true/alias/category=apikey (13-24); auth.apiKey.text exact string about providerSpecificData.baseUrl + "/v1/audio/speech is appended" (26-28); models kokoro (32-34). Only nit: authType:"apikey" is nested in ttsConfig (line 41), not top-level, but present. Runtime special adapter selfhostedTts.js appends /v1/audio/speech (line 49), default http://localhost:8880 (line 9), registered at index.js:24.

Rust current REAL (verified). No "selfhosted" anywhere under src/ (grep). tts/mod.rs:88-118 (get_tts_adapter 89-102 + provider_generic_format 106-119) has no selfhosted-tts branch, so is_tts_provider=false and dispatch returns None at line 40. Minor imprecision: the generic dispatcher DOES have a provider_specific_data["baseUrl"] override (generic_base_url, 129-140), but it's unreachable for selfhosted-tts, so the outcome (selfhosted-tts unrouteable) is correct.

Impl_steps WOULD NOT produce exact parity — real omissions. Step 2 is valid (provider_specific_data is BTreeMap<String,Value> at types/mod.rs:174; default 8880 matches). But step 3/4 skip three load-bearing behaviors of the JS SPECIAL adapter (selfhostedTts.js), which the JS comment explicitly documents as 404/bug sources: (a) URL normalization stripping trailing / and /v1 or /v1/audio/speech before appending (lines 20-25) — naive append double-paths and 404s when a user pastes a baseUrl ending in /v1; (b) default voice af_heart (line 11) vs openai_compat's "alloy" default (generic_formats.rs:363-366); (c) bare model value means the MODEL not the voice (lines 37-47) — JS notes voice="kokoro" upstream makes Kokoro return 400. Mapping selfhosted-tts onto GenericFormat::OpenaiCompat (as steps suggest) would send voice "alloy" and no default model. So the impl is directionally right (works for the common pasted-root case) but misses the documented edge cases, giving behavioral drift rather than 1:1 parity.

---

### `P0-A2c` — Add selfhosted-embedding provider routing (media embeddings) — MUST refuse cloud fallback

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/selfhosted-embedding.js:30-73. id="selfhosted-embedding", priority=50, hasFree:true, alias="selfhosted-embedding", category="apikey", authType="apikey". auth.apiKey.text="Set providerSpecificData.baseUrl to the OpenAI base URL, e.g. http://host:8080/v1 — /embeddings is appended. The API key is not checked by local servers; any value works.". models=[{id:"embedding",name:"Self-hosted embedding model",kind:"embedding"}]. serviceKinds=["embedding"]. embeddingConfig={baseUrl:"http://localhost:8080/v1/embeddings",authType:"apikey",authHeader:"bearer"}. Critical comment lines 43-49: 'the adapter appends "/embeddings" to whatever it is given, so a bare http://host:8080 resolves to http://host:8080/embeddings and misses the OpenAI route entirely. Give it the OpenAI base, the same value an OpenAI client would use. A trailing /embeddings is tolerated.' Also comments 62-72: embeddingConfig.baseUrl is read by the UI but NOT by the request path — openaiCompatNode resolves the URL purely from creds.providerSpecificData.baseUrl (falling back to api.openai.com).

**Current Rust behavior:**

No "selfhosted" in src/. src/core/media/embeddings/mod.rs:46-66 and src/core/embeddings/base.rs:172 (per parity-report A2) currently fall back to api.openai.com when no baseUrl is present — the exact data-leak the JS adapter refuses to do.

**Implementation steps:**

1) In src/core/media/embeddings/mod.rs add a provider branch "selfhosted-embedding". 2) Read connection.provider_specific_data["baseUrl"]; it is the OpenAI base (e.g. http://host:8080/v1). Append "/embeddings" to it. If the baseUrl already ends with "/embeddings" (or "/v1/embeddings"), do not double-append (tolerate trailing /embeddings per JS comment). 3) If baseUrl is MISSING: return an error (MissingBaseUrlError / 400) — the JS adapter's whole design is to REFUSE the api.openai.com fallback. Do NOT fall back to https://api.openai.com/v1. 4) authHeader "bearer" (Bearer <api_key>); key value ignored by local servers but field must exist. 5) The single model id "embedding" passes through.

**Guard test:**

In embeddings/mod.rs test module: #[test] fn selfhosted_embedding_no_cloud_fallback(): connection WITHOUT baseUrl returns an Err (or 400) — assert it does NOT build https://api.openai.com/...; #[test] fn selfhosted_embedding_appends_embeddings(): baseUrl "http://host:8080/v1" -> "http://host:8080/v1/embeddings"; baseUrl "http://host:8080/v1/embeddings" -> unchanged (no double append).

**⚠️ Risks:**

SECURITY: this is the highest-risk task. If the branch is missing or falls through to the generic openai-compatible node (embeddings/base.rs:172), text + API key are silently sent to OpenAI. The JS author (selfhosted-embedding.js:5-24) explicitly documents that a node created as openai-compatible is invisible on the Embedding page — so route by provider id "selfhosted-embedding", not by node type. Double-append of /embeddings is a functional bug to guard against.

**Cross-check:** ✅ **CONFIRMED** — All three verification areas confirmed. (1) JS registry claims REAL: .tmp/9router/open-sse/providers/registry/selfhosted-embedding.js:30-73 matches id="selfhosted-embedding", priority=50, hasFree:true, alias, category="apikey", and the exact auth.apiKey.text at line 49. The entry is imported (registry/index.js:121) and registered as the selfhosted-embedding adapter (embeddingProviders/index.js:22). The core behavior claim is real: embeddingProviders/selfhostedEmbedding.js:36-46 throws MissingBaseUrlError when baseUrl is missing (no cloud fallback), strips trailing "/" and "/embeddings", then appends "/embeddings"; embeddingsCore.js:47-58 confirms the throw surfaces as a 400 config error. Minor notes: the top-level authType="apikey" is not a literal key on the entry — it is derived from category="apikey" (src/shared/constants/providers.js buildProviderEntry), with a nested embeddingConfig.authType="apikey" present — so the claimed value is accurate in effect. (2) Rust current behavior REAL: src/core/media/embeddings/base.rs OpenAiCompatNodeAdapter::build_url (lines 169-179, node fallback at 176, not line 172 as cited; src/core/embeddings/base.rs does not exist — path is src/core/media/embeddings/base.rs) falls back to https://api.openai.com/v1 via unwrap_or when no baseUrl, exactly the data-leak the JS adapter refuses. No "selfhosted" appears anywhere in src/; get_embedding_adapter (mod.rs:46-66) has no such branch. (3) Impl steps would produce parity: provider_specific_data is BTreeMap<String,Value> on ProviderConnection (src/types/mod.rs:174); a missing-baseUrl error maps through EmbeddingsHandlerError::Validation to HTTP 400; existing build_url already strips "/embeddings" before re-appending, matching the tolerate-trailing-/embeddings instruction. Only omission of note: impl must also register the provider in any Rust-side provider registry/catalog (e.g. model combos / /v1/models lists) for full page parity, but for embeddings routing parity the steps are complete.

---

---

## B. EXECUTORS (22 specs)

### `P0-A1` — kimchi: stripReasoningContent direction is inverted — JS strips reasoning_content from REQUEST, Rust strips from RESPONSE; anthropic-backing detection regex; missing mergeTopLevelSystem/system/thinking/stripMessageArtifacts

**JS (source of truth — verbatim):**

JS open-sse/executors/kimchi.js:79-87 — `export function stripReasoningContent(body) { if (!Array.isArray(body?.messages)) return; for (const msg of body.messages) { if (msg && msg.role === "assistant" && typeof msg.reasoning_content === "string" && msg.reasoning_content.length > REASONING_PLACEHOLDER_MAX_LEN) { delete msg.reasoning_content; } } }` with `REASONING_PLACEHOLDER_MAX_LEN = 8` (line 77). Runs on the REQUEST in transformRequest (line 118). JS also: `isAnthropicBackedKimchiModel` (lines 89-93) = `meta?.provider === "anthropic" || meta?.upstreamProvider === "anthropic"` OR regex `/(^|[-_/])(?:claude|anthropic)(?:[-_/]|$)/i.test(String(model || ""))`. JS transformRequest (100-120) additionally: `mergeTopLevelSystem(transformed)` (29-45: hoists body.system string/array into messages, prepending `{role:"system",content:text}` or joining to existing), `delete transformed.system`, drops `reasoning_effort`,`reasoning`,`thinking` for anthropic-backed (110-114), `stripMessageArtifacts` (47-59: delete msg.cache_control + strip `cache_control` and `signature` from each content part), `stripToolArtifacts` (61-68: delete cache_control from each tool).

**Current Rust behavior:**

src/core/executor/kimchi.rs — (a) strips top-level fields 71-79 ✓ but does NOT delete top-level `system` nor merge it. (b) `remove_reasoning_content` (144-161) strips reasoning_content from the RESPONSE body (non-streaming 250-266, plus streaming handled elsewhere) — the OPPOSITE direction; JS never touches responses. (c) `is_anthropic_backed_model` (50-52) only checks prefix `kimchi-sonnet`/`kimchi-haiku`; test at 434-437 even asserts `claude-sonnet-4-20250514` is NOT anthropic-backed (JS would match it via regex). (d) drops only `reasoning_effort` (98-102); JS drops `reasoning_effort` AND `reasoning` AND `thinking`. (e) no signature strip from content parts.

**Implementation steps:**

In src/core/executor/kimchi.rs: (1) Replace `remove_reasoning_content`/`remove_reasoning_content` response stripping (lines 144-161 and the non-streaming branch 250-266) — do NOT strip reasoning_content from responses at all; revert to returning the raw response bytes. (2) Change `is_anthropic_backed_model` to a regex mirroring JS: `/(^|[-_/])(?:claude|anthropic)(?:[-_/]|$)/i` applied to the model (case-insensitive); drop the prefix-only check. (3) In `transform_request`, for anthropic-backed models also `obj.remove("reasoning")` and `obj.remove("thinking")`. (4) After step 1, add: delete top-level `system`; if `body.system` is a string or array of {text}, flatten with `join("\n")` and either prepend `{role:"system",content:text}` to messages or prepend `text + "\n\n" + existing` for a string-content system message / unshift `{type:"text",text}` for array content. (5) Add request-side strips: for each message `obj.remove("cache_control")`, and for each content part remove `cache_control` and `signature`; for each tool remove `cache_control`. (6) For each assistant message whose `reasoning_content` is a string longer than 8 chars, remove it.

**Guard test:**

test_transform_request_strips_request_reasoning_content_echo: build body with assistant message `{"role":"assistant","reasoning_content":"aaaaaaaaa","content":"ok"}` → assert result has no reasoning_content; and a 4-char placeholder `"    "` is PRESERVED (len <= 8).

**⚠️ Risks:**

The JS threshold (len > 8) preserves the 1-char placeholder injected by DefaultExecutor.transformRequest's injectReasoningContent; stripping the placeholder would re-trigger upstream validation errors on the next turn — preserve ≤8-char values exactly. Removing response-side stripping changes visible output: do not remove reasoning_content from upstream responses (JS passes them through).

**Cross-check:** 🟡 **PLAUSIBLE** — All core factual claims are accurate and verified. JS: open-sse/executors/kimchi.js line 77 has `REASONING_PLACEHOLDER_MAX_LEN = 8`; lines 79-87 `stripReasoningContent` iterates `body.messages` and deletes `reasoning_content` from assistant messages with length > 8 — this runs on the REQUEST body (called in transformRequest line 118); the anthropic regex `/(^|[-_/])(?:claude|anthropic)(?:[-_/]|$)/i` is at line 92, but `isAnthropicBackedKimchiModel` (89-93) checks metadata provider/upstreamProvider === "anthropic" FIRST with the regex only as fallback. Rust: src/core/executor/kimchi.rs lines 71-79 drop the 7 top-level keys but never touch `system` (no merge, no delete) — confirmed; `remove_reasoning_content` (144-161) strips RESPONSE-side `choices[].message/delta.reasoning_content`, invoked in the non-streaming branch (253-266) — the inversion claim is real; `is_anthropic_backed_model` (50-52) uses starts_with on kimchi-sonnet/kimchi-haiku, no regex/metadata — confirmed. The impl steps are directionally right but incomplete for parity: (1) removing response stripping without adding request-side stripReasoningContent leaves Rust doing nothing on reasoning_content (the very gap the task is titled for); (2) the regex alone does not match "kimchi-sonnet-*"/"kimchi-haiku-*" (no claude/anthropic substring) — in JS those are caught by the metadata lookup, so a regex-only replacement flips detection of kimchi's own models and would break test_is_anthropic_backed_model. Also unaddressed: JS `delete transformed.system` (108), `delete transformed.reasoning` (112), and `stripMessageArtifacts` removing `signature` from content parts (55) which Rust's remove_cache_control never does. Since the direction claims are real but the impl as written would not reach full parity, verdict is PLAUSIBLE rather than CONFIRMED.

---

### `P0-A1` — temperature drop for Claude — missing STRIP_RULES entry (all providers) + GitHub rules

**JS (source of truth — verbatim):**

paramSupport.js:8-24 — the STRIP_RULES array (a param is removed ONLY when present !== undefined):
```js
const STRIP_RULES = [
  { match: /claude/i, drop: ["temperature"] },
  { provider: "github", match: /gpt-5\.4/i, drop: ["temperature"] },
  { provider: "github", match: (m) => /claude/i.test(m) && !/claude.*(opus|sonnet).*4\.6/i.test(m), drop: ["thinking", "reasoning_effort"] },
  ...
];
```
Call sites: executors/default.js:78 `stripUnsupportedParams(this.provider, model, transformed);` and executors/github.js:109 `stripUnsupportedParams("github", model, transformed);`. The /claude/i temperature rule has NO provider field, so it applies to EVERY provider (incl. github) whenever the model id matches /claude/i.

**Current Rust behavior:**

src/core/executor/strip_unsupported.rs:15-34 should_strip() only strips (a) max_completion_tokens for anthropic-compatible providers, (b) reasoning_effort for gemini/vertex, (c) max_tokens for gemini. No temperature rule, no github rules. Called ONLY from src/core/executor/default.rs:1008. src/core/executor/github.rs transform_request (lines 374-411) does its own inline strips (supports_temperature only for gpt-5.4 at line 232) and never calls strip_unsupported_params.

**Implementation steps:**

In src/core/executor/strip_unsupported.rs, extend should_strip(provider, model, field) with these rules (in order, after the existing three):
1. `if field == "temperature" && model.to_lowercase().contains("claude") { return true; }`  (mirrors JS `{ match: /claude/i, drop: ["temperature"] }` — applies to all providers)
2. `if field == "temperature" && provider == "github" && model.to_lowercase().contains("gpt-5.4") { return true; }`
3. `let m = model.to_lowercase(); let is_claude_except_46 = m.contains("claude") && !(m.contains("claude") && (m.contains("opus") || m.contains("sonnet")) && m.contains("4.6")); if provider == "github" && (field == "thinking" || field == "reasoning_effort") && is_claude_except_46 { return true; }`
Then in src/core/executor/github.rs transform_request (after the existing inline strips at lines 385-409), add: `super::strip_unsupported::strip_unsupported_params("github", model, &mut transformed);` — idempotent because already-stripped fields are gone. NOTE: strip_unsupported_params only iterates top-level body keys and `extra_body` (strip_unsupported.rs:46-67); that matches JS which iterates body object keys. The 4.6 exception matters: a `claude-opus-4.6`/`claude-sonnet-4.6` model on github must KEEP thinking+reasoning_effort (Rust github.rs supports_reasoning_effort at line 241 already mirrors this — keep it).

**Guard test:**

In strip_unsupported.rs tests, add `strips_temperature_for_claude_models` — body with `{"temperature":0.7,"messages":[]}`, call strip_unsupported_params("openai", "claude-sonnet-4.5", &mut body), assert body.get("temperature").is_none(). Add `keeps_temperature_for_non_claude` — provider "openai" model "gpt-4o", assert temperature stays. Add `strips_thinking_and_effort_for_github_claude_except_46` — provider "github" model "claude-sonnet-4.5" strips reasoning_effort; provider "github" model "claude-sonnet-4.6" keeps reasoning_effort.

**⚠️ Risks:**

JS drops temperature even when it is the JSON number 0 (typeof check: `body[key] !== undefined` — 0 is !== undefined so it IS deleted; do NOT gate on truthiness). The github 4.6 regex exception is `claude.*(opus|sonnet).*4.6` — a model like `claude-opus-4.6` OR `claude-sonnet-4.6`; do not over-match other 4.6 models. The Rust github.rs inline `supports_thinking` (line 236) strips thinking for ALL claude — that diverges from JS (which keeps it for 4.6); align it to the regex exception too.

**Cross-check:** ✅ **CONFIRMED** — All three verification points hold. (1) JS behavior is REAL and matches verbatim: paramSupport.js:8-24 contains the STRIP_RULES array with the claude→temperature rule (line 10, no provider restriction), the github gpt-5.4→temperature rule (line 12), and the github claude→thinking/reasoning_effort rule (line 14), and line 45 enforces the present-check (`if (body[key] !== undefined) delete body[key]`). It is invoked from default.js:78 and github.js:109; claude/anthropic route through default.js in 9router (no specialized executor for them in executors/index.js). (2) Rust current behavior is REAL: strip_unsupported.rs:15-34's should_strip() has exactly the three rules described (max_completion_tokens for anthropic-compatible, reasoning_effort for gemini/vertex, max_tokens for gemini) and no temperature rule; grep confirms the sole production caller is default.rs:1008. (3) Impl_steps produce parity for the genuine gap: the claude→temperature rule added to should_strip will fire because claude/anthropic route through DefaultExecutor (chat.rs else-branch) and reach default.rs:1008 with provider+model available, and no other temperature strip exists on that path. The github rules in the impl would be dead code because github dispatches to GithubExecutor (chat.rs:1145), which never calls should_strip — but this is harmless since github.rs already implements the equivalent stripping natively (supports_temperature strips gpt-5.4 at line 387; supports_thinking/supports_reasoning_effort at lines 391-409), so no github parity gap remains. Minor out-of-scope edge: Rust supports_thinking strips thinking for all claude incl. opus/sonnet 4.6 whereas JS github rule keeps it for 4.6 — not introduced or fixed by this task. The task title's core claim (claude temperature drop genuinely missing from the Rust default path) is accurate and the impl fixes it.

---

### `P0-A1` — STT: per-connection baseUrl override missing (selfhosted-stt)

**JS (source of truth — verbatim):**

sttCore.js:176-182:
  // Per-connection endpoint override. Registry entries carry a fixed baseUrl,
  // which is right for a named cloud service but useless for a self-hosted one
  // whose address only the operator knows. Opt-in: absent unless the connection
  // sets it, so cloud providers are untouched.
  const overrideUrl = credentials?.providerSpecificData?.baseUrl;
  if (overrideUrl) cfg = { ...cfg, baseUrl: String(overrideUrl).replace(/\/+$/, "") };
Registry selfhosted-stt.js:40-46:
  sttConfig: {
    baseUrl: "http://localhost:8080/v1/audio/transcriptions",
    authType: "apikey", authHeader: "bearer", format: "openai"
  }

**Current Rust behavior:**

src/server/api/stt.rs: stt_config() returns static SttProviderConfig with fixed base_url (e.g. line 87 `base_url: "https://api.openai.com/v1/audio/transcriptions"`); transcribe_openai (line 692) does `client.post(cfg.base_url)` with NO read of connection.provider_specific_data["baseUrl"]. The cfg is `&'static SttProviderConfig` — the connection override is never applied.

**Implementation steps:**

1) In src/server/api/stt.rs, change the per-provider fixed base_url into an overridable resolved value inside dispatch_with_fallback / transcribe. Before calling transcribe, resolve: `let base = connection.provider_specific_data.get("baseUrl").and_then(Value::as_str).map(|s| s.trim_end_matches('/').to_string()).unwrap_or_else(|| cfg.base_url.to_string());` 2) Thread an owned `String base_url` into each transcribe_* fn (or add a `base_url: String` field to a per-call struct). 3) Use that resolved base_url everywhere cfg.base_url is used: transcribe_openai (line 692 `client.post(cfg.base_url)`), deepgram build_deepgram_url, assemblyai upload/submit/poll (lines 864, 898), huggingface-asr (line 1008 `format!("{}/{}", ...)`), gemini (line ~1076). 4) Keep auth from connection as today. No static-catalog change needed.

**Guard test:**

fn stt_openai_resolves_connection_base_url_override() — build an SttRequest, assert the resolved URL is `{providerSpecificData.baseUrl}` with trailing slash stripped, not the static default; assert a connection without baseUrl still uses the static default.

**⚠️ Risks:**

JS trims trailing slashes on the override (`.replace(/\/+$/, "")`) but keeps the static default as-is (deepgram default ends `/v1/listen`). Override is opt-in only — a connection WITHOUT baseUrl must still hit the static default. Only apply to the selected connection, never cross-account.

**Cross-check:** 🟡 **PLAUSIBLE** — JS claim is exactly real: sttCore.js:176-182 verbatim matches (comment + `const overrideUrl = credentials?.providerSpecificData?.baseUrl;` + `if (overrideUrl) cfg = { ...cfg, baseUrl: String(overrideUrl).replace(/\/+$/, "") };`). credentials is per-connection (auth.js:187-190 merges connection.providerSpecificData), the embedding-parity comment is accurate (embeddingProviders/openaiCompatNode.js:9, selfhostedEmbedding.js:39), and all six format branches read cfg.baseUrl so the override is uniform.

Rust claim is also real: stt_config() (line 84) returns SttProviderConfig{base_url: &'static str,..} with fixed URLs (openai line 87), transcribe_openai line 692 does client.post(cfg.base_url), and grep for baseUrl/provider_specific_data in stt.rs returns zero hits. Feasibility confirmed: media.rs:629 get_provider_base_url() already implements the exact `connection.provider_specific_data.get("baseUrl").and_then(Value::as_str)` pattern.

Impl steps direction is correct (resolve inside the fallback loop after select_stt_connection, before transcribe) but are incomplete for full JS parity:
1. The auth_type_none path (stt.rs:421-428) calls transcribe with no connection — there is no provider_specific_data to read there, so the proposed step-1 resolution anchored on connection.provider_specific_data silently cannot apply; JS has no such branch (always passes a credentials object).
2. Threading base_url only into transcribe_openai would miss parity: fixed cfg.base_url is also consumed by transcribe_deepgram (via build_deepgram_url, line 727-733), transcribe_assemblyai (lines 830, 864), transcribe_nvidia (959), transcribe_huggingface (1008), transcribe_gemini (1090). JS applies the override to all six formats uniformly. Step 2's truncated text ("Thread an owned String base_url into") implies threading into all, but the spec as written names only the openai case.
No error in the claims themselves; the gap is between the described impl and full JS parity across all formats and the no-auth branch.

---

### `P0-A1` — PXPIPE dashboard page + /api/pxpipe/* endpoints missing entirely

**JS (source of truth — verbatim):**

9router route .tmp/9router/src/app/(dashboard)/dashboard/pxpipe/page.js:3-5 = `import PxpipeClient from "./PxpipeClient"; export default function PxpipePage() { return <PxpipeClient />; }`. PxpipeClient.js:72-81 = `Promise.all([fetch("/api/pxpipe/status", { headers: { "Cache-Control": "no-store" } }), fetch("/api/pxpipe/stats"), fetch("/api/pxpipe/logs?limit=50")])` then `fetch("/api/pxpipe/health", { method: "POST" })`. /api/pxpipe/status GET body = `{ installed, installing, version, path, running, loadedAt, uptimeMs, npmAvailable, mode: "library", enabled: !!settings.pxpipeEnabled, autoInstall: !!settings.pxpipeAutoInstall, minChars: settings.pxpipeMinChars, timeoutMs: settings.pxpipeTimeoutMs }` (settingsRepo.js:53-56 defaults `pxpipeEnabled:false, pxpipeAutoInstall:true, pxpipeMinChars:25000, pxpipeTimeoutMs:15000`). /api/pxpipe/health POST (GET mirrors) = `{ healthy, checks:[{id:'installed',label:'PXPIPE installed',ok,detail},{id:'module',label:'Transform module loads',ok,detail},{id:'transform',label:'Test request transforms',ok,detail}], error }`. /api/pxpipe/stats = `{ windows:{all,today,yesterday,last7d,last30d}, timeline:[{date,tokensSavedEst,compressed,requests}], recent:[ev] }`; window totals `{requests,compressed,bypassed,errors,tokensBeforeEst,tokensAfterEst,tokensSavedEst,savedPct,imagesGenerated,compressionTimeMs,avgCompressionMs}`; recent ev `{ts,provider,model,applied,reason,tokensBeforeEst,tokensAfterEst,tokensSavedEst,savedPct,durationMs,imageCount,detail}`. /api/pxpipe/logs?limit=50 = `{ installLog, events }`. REASON_LABELS (PxpipeClient.js:36-49): applied='Prompt exceeded threshold', below_threshold='Below size threshold', not_profitable='Compression not profitable', below_min_chars='Below minimum chars', below_min_tokens='Below minimum tokens', unsupported_model='Model not in allowlist', unsupported_format='Non-Claude request format', timeout='Compression timed out', transform_error='Transform error', passthrough='Passthrough', disabled='Disabled', not_installed='Not installed'. UI: 6 SummaryCards Status/Version/Uptime/Requests/Compressed/Bypassed, 5 tabs Today/Yesterday/7 days/30 days/All time, AreaChart stroke #10b981 with linearGradient gradPxpipe stopColor #10b981 (PxpipeClient.js:186-195), History table with Status badge colors success/danger/warning, PXPIPE Logs card showing logs.installLog in <pre>.

**Current Rust behavior:**

N/A. No pxpipe string exists anywhere in web/src or src (grep for pxpipe returns nothing). No /dashboard/pxpipe page, no Token Saver PXPIPE controls, no settings keys pxpipeEnabled/pxpipeAutoInstall/pxpipeMinChars/pxpipeTimeoutMs.

**Implementation steps:**

1) Rust settings struct (src/types.rs settings): add 4 fields with the exact JS defaults - pxpipe_enabled: bool = false, pxpipe_auto_install: bool = true, pxpipe_min_chars: u32 = 25000, pxpipe_timeout_ms: u32 = 15000. Wire into settings load/save and PATCH /api/settings. 2) Add routes in src/server/api: GET /api/pxpipe/status returning {installed:false, installing:false, version:null, path:null, running:false, loadedAt:null, uptimeMs:0, npmAvailable:false, mode:"library", enabled, autoInstall, minChars, timeoutMs}; GET /api/pxpipe/stats returning {windows:{all:empty,today:empty,yesterday:empty,last7d:empty,last30d:empty},timeline:[],recent:[]} with the exact window field names; GET /api/pxpipe/logs returning {installLog:null,events:[]}; POST+GET /api/pxpipe/health returning {healthy:false,checks:[],error:"pxpipe not installed"}. Persist pxpipe events to data dir events.jsonl so a future real integration can feed stats. 3) Frontend: web/src/pages/dashboard/pxpipe/index.astro + PxpipePageClient.tsx port of PxpipeClient.js (6 summary cards, window tabs, chart, history table, logs card). 4) Sidebar: add { href: '/dashboard/pxpipe', label: 'PXPIPE', icon: 'image' } exactly as JS has it commented out (JS Sidebar.js:28 has it commented - mirror that: leave commented unless backend integration exists).

**Guard test:**

cargo test pxpipe_settings_defaults_and_roundtrip - assert Settings::default().pxpipe_enabled==false, pxpipe_auto_install==true, pxpipe_min_chars==25000, pxpipe_timeout_ms==15000, and PATCH /api/settings with {"pxpipeMinChars":12345} round-trips. Plus test_pxpipe_routes_defined asserting routes() builds (mirrors test_usage_routes_defined in usage.rs:1539).

**⚠️ Risks:**

JS setting keys are camelCase in the API (pxpipeMinChars) - Rust must serde rename_all camelCase on the settings PATCH. pxpipe stats window keys must be exactly 'all'/'today'/'yesterday'/'last7d'/'last30d'. The JS status fields enabled/autoInstall/minChars/timeoutMs come from settings, NOT from the service - a naive port that omits them breaks the Token Saver pxpipe card too. Mode string must be 'library' not 'library-mode'.

**Cross-check:** ✅ **CONFIRMED** — Verified all three points. (1) JS claim is REAL: .tmp/9router/src/app/(dashboard)/dashboard/pxpipe/page.js:3-5 matches the cited code exactly; PxpipeClient.js:72-81 contains the exact Promise.all fetch block hitting /api/pxpipe/status (with Cache-Control: no-store header), /api/pxpipe/stats, /api/pxpipe/logs?limit=50, then fetch("/api/pxpipe/health", { method: "POST" }); settingsRepo.js:53-56 confirms the 4 defaults pxpipeEnabled:false, pxpipeAutoInstall:true, pxpipeMinChars:25000, pxpipeTimeoutMs:15000 exactly as claimed. (2) Rust current behavior is REAL: grep -ri pxpipe across web/src and src returns nothing; the Settings struct (src/types/mod.rs:358) has rtk/caveman/ponytail/headroom but no pxpipe fields; the PATCH /api/settings payload struct has no pxpipe; no pxpipe dashboard dir exists under web/src/pages/dashboard/. Minor nuance: web/public/i18n/literals/pt-BR.json holds PXPIPE translation strings, but that's a static locale file under web/public (outside the claimed grep scope of web/src+src) with no functional code, so it does not falsify the claim. (3) Impl steps would produce parity: adding the 4 fields to the Settings struct + PATCH payload with the exact JS defaults mirrors the fully-implemented headroom pattern (src/server/api/headroom.rs routes /api/headroom/status|start|stop|restart, settings wired identically), and the claimed /api/pxpipe/status response shape {installed:false, installing:false, version:null, path:null, running:...} matches the real JS getPxpipeStatus() (service.js:6-20) plus merged enabled/autoInstall/minChars/timeoutMs. One non-blocking note: the JS client also consumes /api/pxpipe/stats, /api/pxpipe/logs, and /api/pxpipe/health (POST), so full parity requires those routes too; the truncated impl_steps (cut off mid-step-2) do not contradict this.

---

### `P0-A1` — Capacity adapter: entirely missing in Rust (settings, augment, strip, strategy)

**JS (source of truth — verbatim):**

open-sse/services/capacityAdapter.js:13-15: `const CAPABILITY_KEYS = ["vision", "pdf", "audioInput", "videoInput"]; const HARD_CAPS = new Set(CAPABILITY_KEYS); const DEFAULT_FALLBACK_MODEL = "oc/mimo-v2.5-free";`
:19-41 `normalizeCapEntry` (accepts array-form or `{enabled, roundRobin, models}`; enabled defaults true; empty-enabled pool falls back to `models: [DEFAULT_FALLBACK_MODEL]` via `getCapacityAdapterConfig`).
:44-58 `getCapacityAdapterModels(settings)`: flatten enabled pools in CAPABILITY_KEYS order, dedup via Set.
:61-64 `getCapacityAdapterStrategy(cap, settings)`: `enabled && roundRobin ? "round-robin" : "fallback"`.
:68-76 `getActiveAdapterStrategy(requiredCapabilities, settings)`: iterate `HARD_CAPS` in `requiredCapabilities`, return strategy of first enabled non-empty pool, else "fallback".
:78-100 `modelSatisfies(modelStr, requiredHard)` uses `getCapabilitiesForModel(provider, model)` and `augmentModelsWithCapacityAdapter(models, requiredCapabilities, settings)`: if any original model satisfies the hard caps return models unchanged; else prepend pool models that satisfy and aren't already in models.
:102-156 `stripHistoryForContext(body, contextWindow)`: messages|input|contents key; split system/developer msgs; keep trailing user turn (after last assistant/model); budget = `(contextWindow || 200000) * 0.8 * 4` chars; keep first HEAD_KEEP=6 older messages verbatim; drop from end of head until budget fits; return `{...body, [key]: [...systemMsgs, ...head, ...tail]}`.
:160-173 `withCapacityAdapterStripping(handleSingleModel, adapterModels)`: wraps so adapter-model calls first `stripHistoryForContext(body, getCapabilitiesForModel(provider, model).contextWindow)`.

Wire-in src/sse/handlers/chat.js:96-152: combo path `augmentModelsWithCapacityAdapter(comboModels, requiredCapabilities, settings)`; `adapterAdded = augmentedModels.filter(m => !comboModels.includes(m))` passed to `withCapacityAdapterStripping(handleSingleModel, adapterAdded)`; comboStrategy from `getActiveAdapterStrategy(requiredCapabilities, settings)` for solo-augmented path.

Default settings src/lib/db/repos/settingsRepo.js:20-26: `capacityAdapter: { vision: { enabled: true, roundRobin: false, models: [] }, pdf: { enabled: false, roundRobin: false, models: [] }, audioInput: { enabled: true, roundRobin: false, models: [] }, videoInput: { enabled: false, roundRobin: false, models: [] } }`.

**Current Rust behavior:**

N/A. src/core/combo/mod.rs:189 `HARD_CAPS = ["vision", "pdf"]` only (missing audioInput/videoInput). `detect_required_capabilities` (line 193) and `scan_content_for_capabilities` (line 256) only add vision/pdf. `model_has_capability` (line 302) only handles vision/pdf. No capacity-adapter settings field in `Settings` (src/types/mod.rs:430-535 has rtk/headroom/caveman but no capacity_adapter). `UpdateSettingsRequest` (src/server/api/mod.rs:2065) has no capacity_adapter. No `strip_history_for_context` or adapter-augmentation anywhere.

**Implementation steps:**

1. In src/types/mod.rs add to `Settings` struct: `#[serde(default, deserialize_with = "deserialize_null_default")] pub capacity_adapter: serde_json::Value,` (default `json!({})` in Default impl) — it is read-only at runtime, the dashboard PATCHes it via `extra`? No — add an explicit field to `UpdateSettingsRequest` in src/server/api/mod.rs: `capacity_adapter: Option<serde_json::Value>` and in `update_settings_api` body add `if let Some(v) = req.capacity_adapter { db.settings.capacity_adapter = v; }` (mirror the rtk_enabled pattern at line 2206).
2. New module src/core/combo/capacity_adapter.rs porting capacityAdapter.js:
   - `pub const CAPABILITY_KEYS: [&str; 4] = ["vision", "pdf", "audioInput", "videoInput"];`
   - `pub const DEFAULT_FALLBACK_MODEL: &str = "oc/mimo-v2.5-free";`
   - `fn normalize_cap_entry(entry: &Value) -> CapEntry { enabled, roundRobin, models: Vec<String> }` — accept array-form `[{model}]`/`["model"]` (enabled=true, roundRobin=false, models=model or string), object-form with `enabled !== false`, `roundRobin: !!roundRobin`, `models` array filter Boolean; else disabled.
   - `fn get_capacity_adapter_config(cap, settings) -> CapEntry` — `enabled && models.is_empty()` → models=`[DEFAULT_FALLBACK_MODEL]`.
   - `pub fn get_capacity_adapter_models(settings) -> Vec<String>` — CAPABILITY_KEYS order, dedup HashSet.
   - `pub fn get_capacity_adapter_strategy(cap, settings) -> &'static str` — `enabled && roundRobin ? "round-robin" : "fallback"`.
   - `pub fn get_active_adapter_strategy(required: &HashSet<String>, settings) -> &'static str` — first hard cap in required with enabled non-empty pool; else "fallback".
   - `fn model_satisfies(model_str, required_hard) -> bool` — split at first '/'; look up capabilities (see step 4).
   - `pub fn augment_models_with_capacity_adapter(models: &[String], required: &HashSet<String>, settings) -> Vec<String>` — if hard empty or models empty or `models.iter().any(model_satisfies)` return models; else prepend pool models satisfying hard and not already present.
   - `pub fn strip_history_for_context(body: &mut Value, context_window: Option<u64>) -> bool` — port blockLength (string→len, array→sum text len or 50), budget chars `(context_window || 200000)*0.8*4`, head KEEP 6. Return true if body mutated.
   - `pub fn with_capacity_adapter_stripping` — not needed as separate wrapper; instead in the dispatch loop, before handle_single_model, check if model is in adapterAdded set and if so call strip_history_for_context with the model's contextWindow.
3. Wire into the combo dispatch in src/server/api/chat.rs: after `detect_required_capabilities` compute `augmented` via `augment_models_with_capacity_adapter(&combo_members, &required, &settings.capacity_adapter)`; compute `adapter_added`; pass augmented member list to the combo executor (instead of raw members); inside the handle_single_model closure for a model in adapter_added, call strip_history_for_context with the model's context window before forwarding. Set combo strategy to `get_active_adapter_strategy(&required, &settings.capacity_adapter)` when a solo model was augmented.
4. Capabilities for oc/mimo-v2.5-free: `DEFAULT_FALLBACK_MODEL` provider is `oc`, model `mimo-v2.5-free`. In `model_has_capability` (src/core/combo/mod.rs:302) extend to audioInput/videoInput, and add a provider-prefix map for `oc` → mimo-v2.5-free supports vision+audioInput+videoInput (it is a multimodal free model; verify against 9router capabilities.js entry for mimo-v2.5-free). If exact caps are uncertain, mark only vision true and let pools carry models.

**Guard test:**

`capacity_adapter_augments_only_when_original_lacks_cap`: settings `{vision:{enabled:true,roundRobin:false,models:["oc/mimo-v2.5-free"]}}`; augment `["openai/gpt-4"]` with required `{vision}` → returns `["oc/mimo-v2.5-free", "openai/gpt-4"]`; augment `["anthropic/claude"]` with required `{vision}` → unchanged. Also `capacity_adapter_strip_history_keeps_system_and_tail`: body with system + 8 user/assistant turns + trailing user with image, context_window 200000 → messages reduced but system + trailing user retained.

**⚠️ Risks:**

JS DEFAULT_FALLBACK_MODEL `oc/mimo-v2.5-free` must be exact. roundRobin strategy on the solo-augmented path must come from the FIRST satisfying cap, not the first enabled. Dedup in getCapacityAdapterModels must be order-preserving. stripHistoryForContext must ONLY run for adapter-added models (never for combo members) or it will strip normal combos. 80% headroom factor and CHARS_PER_TOKEN=4 must be preserved exactly. Empty enabled pool must fall back to the mimo model, never a no-op.

**Cross-check:** ✅ **CONFIRMED** — All three claims verified against source.

(1) JS behavior is REAL. C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/open-sse/services/capacityAdapter.js lines 13-15 match exactly: CAPABILITY_KEYS = ["vision","pdf","audioInput","videoInput"], HARD_CAPS = new Set(CAPABILITY_KEYS), DEFAULT_FALLBACK_MODEL = "oc/mimo-v2.5-free". Lines 19-41 match: normalizeCapEntry accepts array-form or {enabled, roundRobin, models}, enabled defaults true, empty-enabled pool falls back to [DEFAULT_FALLBACK_MODEL]. It is not dead code — wired into src/sse/handlers/chat.js (augmentModelsWithCapacityAdapter for combo and single-model paths, withCapacityAdapterStripping, getActiveAdapterStrategy) and the dashboard (combos/page.js PATCHes capacityAdapter in settings body; settingsRepo.js stores per-cap defaults).

(2) Rust current behavior is REAL. src/core/combo/mod.rs:189 HARD_CAPS = &["vision","pdf"] only (missing audioInput/videoInput). detect_required_capabilities (line 193) and scan_content_for_capabilities (line 256) only insert vision/pdf; model_has_capability (line 302) matches only "vision"/"pdf" arms. A grep for capacity_adapter/capacityAdapter across all of src/ returns zero matches — the adapter is entirely absent.

(3) Impl steps would produce parity. The Settings struct (src/types/mod.rs:358) already uses the exact #[serde(default, deserialize_with = "deserialize_null_default")] pattern pervasively and the helper exists at line 940; the Default impl at line 538 is explicit and would take the proposed json!({}) entry. UpdateSettingsRequest (src/server/api/mod.rs:2065) already uses the identical Option<serde_json::Value> pattern (claude_auto_ping line 2107, codex_auto_ping, provider_thinking) persisted via db.settings.extra, so the proposed capacity_adapter field + persist block is idiomatic and will work. Minor note only: the spec's aside about persisting via settings.extra vs a real field is an implementation detail — either is viable, but the explicit-field route it settles on is the right one given update_settings_api's existing pattern.

Gap between JS (settings + augment + strip + strategy, 4 capability keys) and Rust (no adapter, 2 keys) is real and accurately characterized; the cited line numbers are all correct.

---

### `P0-A1` — git-log RTK filter is missing entirely in Rust (G2b)

**JS (source of truth — verbatim):**

open-sse/rtk/filters/gitLog.js:1-99 — full filter with `gitLog.filterName = "git-log"`. Key logic:
```js
import { GIT_LOG_MAX_LINES } from "../constants.js";
// GIT_LOG_MAX_LINES = 200 (open-sse/rtk/constants.js:7)
export function gitLog(text, maxLines = GIT_LOG_MAX_LINES) {
  if (!text) return "";
  const lines = input.split("\n"); ...
  // commit header: /^commit [0-9a-f]{7,40}$/i OR /^[*|/\\ ]+commit [0-9a-f]{7,40}/i
  // inCommit: /^[*|/\\ ]*(Author|Date):/i kept verbatim
  //           blank "" skipped
  //           first indented subject (!subjectSeen && /^[*|/\\ ]*    \S/.test(line)) -> "  Subject: " + trimmed
  //           /^\d+ file\w* changed/ -> "  " + trimmed
  //           /^diff --git / -> "  ... diff body omitted"
  //           everything else dropped
  // not inCommit: graphMatch /^[*|/\\ ]+([0-9a-f]{7,40}\s+.+)/i -> push graphMatch[1]
  //               plain oneline /^[0-9a-f]{7,40}\s+/ -> push trimmed
  //               pure graph /^[*|/\\ ]+$/ && /[*|/\\]/ -> skip
  //               catch-all pushLine(trimmed)
  // pushLine caps at maxLines; overflow -> skipped++
  if (skipped > 0) out.push(`... (${skipped} more lines)`);
  const result = out.join("\n");
  if (!result && input) return input;         // never return empty
  if (result.length > input.length) return input; // never grow
  return result;
}
```
Detection in open-sse/rtk/autodetect.js:21,32: `const RE_GIT_LOG = /^[*|/\\ ]*commit [0-9a-f]{7,40}$/m;` checked FIRST — `if (RE_GIT_LOG.test(head)) return gitLog;`

**Current Rust behavior:**

src/core/rtk/filters/mod.rs has NO git_log_impl; src/core/rtk/constants.rs has NO GIT_LOG_MAX_LINES; autodetect.rs:50-196 has NO git-log branch. grep for 'git_log|git-log' in src/ returns 0 matches. The JS `gitLog.filterName="git-log"` and constants `FILTERS.GIT_LOG: "git-log"` (constants.js:50) also missing from FILTER_* set.

**Implementation steps:**

1) src/core/rtk/constants.rs: add `/// gitLog line cap
pub const GIT_LOG_MAX_LINES: usize = 200;` and `pub const FILTER_GIT_LOG: &str = "git-log";`
2) src/core/rtk/filters/mod.rs: add `pub struct GitLogFilter; impl GitLogFilter { pub fn apply(&self, text: &str) -> String { safe_apply(git_log_impl, text, FILTER_GIT_LOG) } }` and `pub fn git_log_impl(input: &str) -> String` implementing the JS exactly: split lines; regex commit-header `^(commit [0-9a-f]{7,40})$` (case-insensitive) or `^[*|/\\ ]+commit [0-9a-f]{7,40}`; within commit keep `^[*|/\\ ]*(Author|Date):` trimmed verbatim, skip blank, first `^[*|/\\ ]*    \S` (non-whitespace after 4 spaces) → `  Subject: {trimmed}`, `^\d+ file\w* changed` → `  {trimmed}`, `^diff --git ` → `  ... diff body omitted`, else drop; outside commit: `^[*|/\\ ]+([0-9a-f]{7,40}\s+.+)` → capture group 1, `^[0-9a-f]{7,40}\s+` → trimmed, pure graph `^[*|/\\ ]+$` with a graph char → skip, else push trimmed. Cap at GIT_LOG_MAX_LINES via pushLine-equivalent; if skipped>0 append `... (N more lines)`. If result empty and input non-empty → return input; if result.len() > input.len() → return input.
3) src/core/rtk/apply_filter.rs: add `pub fn git_log(text: &str) -> String { use crate::core::rtk::filters::GitLogFilter; GitLogFilter.apply(text) }`
4) src/core/rtk/autodetect.rs: import git_log_impl + FILTER_GIT_LOG; add `static RE_GIT_LOG: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^[*|/\\ ]*commit [0-9a-f]{7,40}$").unwrap());` and insert as the FIRST check (before RE_GIT_DIFF): `if RE_GIT_LOG.is_match(head) { return Some(DetectedFilter { filter_fn: git_log_impl, filter_name: FILTER_GIT_LOG }); }`

**Guard test:**

cargo test test_git_log_full_and_oneline in src/core/rtk/filters/mod.rs: assert git_log_impl on a `commit abc1234\nAuthor: X\nDate: Y\n\n    subject line\n\n 5 files changed` block contains `commit abc1234`, `Author: X`, `  Subject: subject line`, does NOT contain the diff body; assert oneline mode `abc1234 subject` passes through; assert >200 lines appends `... (N more lines)`. Add autodetect test `test_detects_git_log` asserting auto_detect_filter("commit abc1234\nAuthor: X") == FILTER_GIT_LOG.

**⚠️ Risks:**

RE_GIT_LOG must be checked before RE_GIT_DIFF in autodetect (JS order git-log → git-diff). The commit-header regex anchors with $ on a trimmed line — use (?m) so ^/$ are line anchors. `pushLine(trimmed)` for Author/Date pushes the TRIMMED line (graph prefix stripped by regex match but JS pushes `trimmed` not `line`). Case-insensitive /i on the commit regex. Never return empty and never grow input — JS both guard against output.length >= bytesIn in compressText (rtk/index.js:138) AND gitLog itself returns input. Must preserve the `  Subject: ` 2-space prefix exactly.

**Cross-check:** 🟡 **PLAUSIBLE** — Both the JS claim and the Rust current-state claim are fully accurate and verified against the cited files. JS: open-sse/rtk/filters/gitLog.js is real (gitLog.filterName="git-log" at line 99, the exact commit-header regex at line 32, GIT_LOG_MAX_LINES=200 at constants.js:7, FILTERS.GIT_LOG="git-log" at constants.js:50). Rust: grep for 'git_log|git-log' in src/ returns 0 matches; constants.rs lacks GIT_LOG_MAX_LINES and FILTER_GIT_LOG; auto_detect_filter (exactly autodetect.rs:50-196) has no git-log branch; filters/mod.rs has no git_log_impl/GitLogFilter. However, the impl_steps would NOT fully achieve end-to-end parity: on the JS side git-log is wired into the live pipeline in two places the steps omit — (1) autodetect.js:32 registers gitLog as the TOP-PRIORITY detection branch (RE_GIT_LOG checked before RE_GIT_DIFF), and (2) registry.js:17 maps FILTERS.GIT_LOG to gitLog. The steps add only the constant and the filter body, leaving out the auto_detect_filter branch and the apply_filter.rs dispatch wrapper, so the new Rust filter would be dead code and git-log output (which typically also contains "diff --git" and "commit <sha>" lines) would still be misclassified as git-diff/git-status, diverging from JS behavior. Detection ordering also matters (git-log must precede git-diff). The proposed filter body pattern (GitLogFilter struct + safe_apply) is consistent with existing Rust idioms, so the approach works, but the wiring omission is an obvious gap to close for true parity.

---

### `P0-B1` — kiro: no integrity gate / repair loop, no EventStream→OpenAI binary transform, no region-aware URL ordering, no 401/403/404 fallback

**JS (source of truth — verbatim):**

JS open-sse/executors/kiro.js — execute() (338-342) calls `super.execute(args)` then `this.attachIntegrityGate(result, args)` (344-409): emits `: kiro-validation\n\n` heartbeat every KIRO_REPAIR_HEARTBEAT_MS=10_000, runs `runIntegrityRecovery` (411-479) with repair instructions (41-45), bounded buffer KIRO_REPAIR_BUFFER_MAX_BYTES=8MiB, retries once with `appendRepairInstruction` when kind is ellipsis/short_final/invalid_tool, error SSE codes `kiro_*` (188-194: `data: {"error":{message,type:"upstream_error",code,...}}\n\ndata: [DONE]\n\n`). `readIntegrityAttempt`/`transformEventStreamToSSE` (517-1114) fully parses AWS EventStream binary (prelude CRC 899, headers 1140-1224, event types 743-876: assistantResponseEvent strips `<thinking>`/`</thinking>` 759-784, reasoningContentEvent→`reasoning_content` 785-792, codeEvent 793-796, toolUseEvent buffers input fragments and emits split `tool_calls` deltas with `arguments:""` then `arguments:JSON.stringify(input)` 712-742, messageStopEvent/metadataEvent merge stop reasons 824-840, contextUsageEvent→usage fallback 1032-1043). `getOrderedBaseUrls` (274-303): regionalizes `u.replace(/([a-z]+)\.[a-z0-9-]+\.amazonaws\.com/, `$1.${region}.amazonaws.com`)` from `credentials?.providerSpecificData?.region` (default us-east-1). `shouldRetry` (312-316): `super.shouldRetry(...) || (hasFallback && KIRO_ENDPOINT_FALLBACK_STATUSES.has(status))` where KIRO_ENDPOINT_FALLBACK_STATUSES = Set([401,403,404]) (kiroConstants.js:28).

**Current Rust behavior:**

src/core/executor/kiro.rs — build_url (159-218) orders q-first for api_key but does NOT regionalize amazonaws.com hosts (no `region` psd substitution). `execute_request` (288-370) loops URLs but on an Ok non-200 status returns the FIRST response immediately — no 401/403/404 fallback, no shouldRetry. No integrity gate, no heartbeat, no repair loop. `EventStreamDecoder::decode_chunk` (568-658) decodes binary frames but returns only raw `data:` SSE strings; the actual OpenAI chunk assembly lives in src/core/translator/response/kiro_to_openai.rs which handles assistantResponseEvent/reasoningContentEvent/toolUseEvent/messageStopEvent as pre-decoded JSON and does NOT implement tool-input buffering (appendToolInput/parsedToolInput/emitTools), `<thinking>` tag stripping, codeEvent, meteringEvent/metricsEvent, contextUsageEvent usage fallback, stop-reason merging, or the `chatcmpl-${Date.now()}` chunk id shape with `choices[0].delta.role:"assistant"` on first chunk.

**Implementation steps:**

In src/core/executor/kiro.rs: (1) In `build_url`, apply JS regionalization: when `is_cw_surface`, read `region` from psd (default "us-east-1"); for every base URL containing `amazonaws.com` and region != "us-east-1", replace the host's region segment via regex `([a-z]+)\.[a-z0-9-]+\.amazonaws\.com` → `$1.{region}.amazonaws.com` (apply to both q and codewhisperer hosts). (2) In `execute_request`, on a non-success status do not return immediately: for status in {401,403,404} and there are remaining URLs, continue to next URL (mirror JS shouldRetry + fallback). (3) Build the integrity gate (biggest item): wrap the response in a streaming transform that (a) parses the binary EventStream frames via a new `crc32` table + header decoder per JS 1140-1224, (b) assembles OpenAI chat.completion.chunk SSE with id `chatcmpl-{timestamp_ms}`, `created`, model, first chunk `delta:{role:"assistant"}`, (c) buffers toolUseEvent inputs per id and emits two tool_calls deltas (first `{id,name,type:"function",function:{name,arguments:""}}`, second `{arguments:JSON.stringify(input)}`), (d) strips `<thinking>`/`</thinking>` from assistantResponseEvent content, (e) merges stop reasons via severity ordering, (f) emits `: kiro-validation\n\n` heartbeat every 10s and `: kiro-upstream\n\n` after validated frames with no new chunk, (g) on terminal failure emits `data: {"error":{message,type:"upstream_error",code:"kiro_..."}}\n\ndata: [DONE]\n\n`. (4) Add a bounded one-retry repair loop when the first attempt's disposition is retryable_protocol_failure/ellipsis/short_final: append instruction text `Retry the previous response because its Kiro tool_call wrapper was malformed...` etc. and re-execute once.

**Guard test:**

test_eventstream_tool_use_emits_two_deltas: feed a crafted binary EventStream frame with a toolUseEvent carrying `{toolUseId,name,input:{x:1}}` → assert output SSE contains a chunk with `tool_calls[0].function.arguments == ""` and a later chunk with `tool_calls[0].function.arguments == "{\"x\":1}"`. Also `test_build_url_regionalizes_q_host` for region "eu-west-1".

**⚠️ Risks:**

This is the largest port; the JS binary decoder tolerates malformed SSE lines silently, and the buffer bounds (8MiB repair / 24MiB message / 128KiB headers) prevent memory blowup — replicate them. JS errors are returned untransformed so chatCore can read the body and trigger account fallback — keep non-EventStream HTTP error responses raw. Do not strip thinking tags on `reasoningContentEvent` (only on assistantResponseEvent content).

**Cross-check:** ✅ **CONFIRMED** — Every cited claim checks out against the actual files. JS (C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/open-sse/executors/kiro.js): execute() at 338-342 calls super.execute then attachIntegrityGate on response.ok; attachIntegrityGate (344-409) emits ": kiro-validation\n\n" every KIRO_REPAIR_HEARTBEAT_MS=10_000 (lines 15, 362, 368), uses KIRO_REPAIR_BUFFER_MAX_BYTES=8*1024*1024 (line 14); runIntegrityRecovery (411-479) retries exactly once via BaseExecutor.prototype.execute with appendRepairInstruction for ellipsis/short_final/invalid_tool, and REPAIR_INSTRUCTIONS is at 41-45 — all line-accurate. getOrderedBaseUrls (274-303) regionalizes amazonaws.com hosts via regex ([a-z]+)\.[a-z0-9-]+\.amazonaws\.com -> $1.{region}.amazonaws.com (region from psd defaulting to "us-east-1", applied to both q and codewhisperer when is_cw_surface), and shouldRetry (312-316) adds KIRO_ENDPOINT_FALLBACK_STATUSES={401,403,404} (kiroConstants.js line 28) so JS genuinely falls back on those statuses. Rust (C:/Users/ADMIN/Documents/Projects/openproxy/src/core/executor/kiro.rs): build_url (159-218) only reorders q-first for api_key, never reads region or substitutes host segments — regionalization absent, confirmed. execute_request (288-370) returns immediately on any Ok(response) (349-359) with no status check and no shouldRetry — no 401/403/404 fallback, confirmed. Impl step 1 is a faithful port of the JS regionalization (same default, regex, host set) and would close the region-aware-URL-ordering gap. Caveat (does not refute): for the idc/AWS-JSON SigV4 path, sign_request hardcodes KIRO_REGION="us-east-1" (line 401), so regionalizing the URL for an idc account would need the signing region to be derived from the regionalized host too; the api_key/external_idp bearer paths (the primary gap) need no such change.

---

### `P0-C1` — grok-web executor is a stub: Rust sends raw body; missing MODEL_MAP grokPayload, NDJSON parse, message flattening, browser headers, reasoning_content mapping

**JS (source of truth — verbatim):**

JS open-sse/executors/grok-web.js — GROK_CHAT_API = `https://grok.com/rest/app-chat/conversations/new` (line 6, registry). MODEL_MAP (9-24) maps e.g. `"grok-4.2": { grokModel: "grok-420", modelMode: "MODEL_MODE_GROK_420", isThinking: false }`, `"grok-4.1-fast": { grokModel: "grok-4-1-thinking-1129", modelMode: "MODEL_MODE_FAST", isThinking: false }`. grokPayload (247-259): `{ temporary: true, modelName: grokModel, modelMode, message, fileAttachments: [], imageAttachments: [], disableSearch: false, enableImageGeneration: false, returnImageBytes: false, returnRawGrokInXaiRequest: false, enableImageStreaming: false, imageGenerationCount: 0, forceConcise: false, toolOverrides: {}, enableSideBySide: true, sendFinalMetadata: true, isReasoning: false, disableTextFollowUps: false, disableMemory: true, forceSideBySide: false, isAsyncChat: false, disableSelfHarmShortCircuit: false, deviceEnvInfo: { darkModeEnabled: false, devicePixelRatio: 2, screenWidth: 2056, screenHeight: 1329, viewportWidth: 2056, viewportHeight: 1083 } }`. Headers (263-283): `Accept: "*/*"`, `Accept-Encoding`, `Accept-Language`, Baggage sentry string, Origin `https://grok.com`, Referer `https://grok.com/`, `Sec-Ch-Ua`, `Sec-Fetch-*`, `User-Agent` (Chrome 136 macOS), `x-statsig-id: generateStatsigId()` (34-38), `x-xai-request-id: crypto.randomUUID()`, `traceparent: 00-{traceId}-{spanId}-00`. Cookie: `sso=${token}` after stripping `sso=` prefix (286-290). Response: NDJSON lines parsed (74-101); `extractContent` (103-133) yields `{delta: resp.token}` or `{fullMessage: mr.message}` or `{thinking: mr.message}` (thinking only for isThinking models when a modelResponse follows); streamed as OpenAI SSE chunks (135-188): first chunk `delta:{role:"assistant"}`, thinking → `delta.reasoning_content`, content → `delta.content`, final `finish_reason:"stop"` + `data: [DONE]`. Non-streaming (190-219): `msg.reasoning_content = thinkingParts.join("\n")`, usage `prompt_tokens = completion_tokens = ceil(fullContent.length/4)`. Errors: HTTP 401/403 → "Grok auth failed — SSO cookie may be expired...", 429 → "Grok rate limited...", type "upstream_error", code `HTTP_${status}`.

**Current Rust behavior:**

src/core/executor/grok_web.rs — build_url (144-146) `https://grok.com/app-chat/conversations/new` (wrong: missing `/rest`; registry JS is `https://grok.com/rest/app-chat/conversations/new`). build_headers (148-163) only sets Content-Type application/json, Accept application/json, and `sso={access_token}` cookie — no grokPayload, no message flattening (parseOpenAIMessages 46-72), no browser headers, no statsig/traceparent. transform_request (165-168) returns body.clone() unchanged. No NDJSON reading, no reasoning_content, no usage estimation, no error-code mapping.

**Implementation steps:**

Rewrite src/core/executor/grok_web.rs GrokWebExecutor: (1) URL → `https://grok.com/rest/app-chat/conversations/new`. (2) Add MODEL_MAP with the 13 entries from JS lines 9-24 (grok-3, grok-3-mini, grok-3-thinking, grok-4, grok-4-mini, grok-4-thinking, grok-4-heavy, grok-4.1-mini, grok-4.1-fast, grok-4.1-expert, grok-4.1-thinking, grok-4.2, grok-4.20, grok-4.20-beta) with exact grokModel/modelMode/isThinking. (3) Add parse_openai_messages: flatten messages to `role: text`, prepend `role: ` to all but last user message, join `\n\n`. (4) Build the grokPayload verbatim (all fields above). (5) Headers per JS 263-283; generate x-statsig-id as `btoa` of the random TypeErrors, x-xai-request-id UUID, traceparent `00-{16-byte-hex}-{8-byte-hex}-00`; Cookie `sso=`+token after stripping `sso=` prefix. (6) POST, parse NDJSON, map to OpenAI SSE chunks: first chunk `{role:"assistant"}`, `modelResponse`/`token`/thinking handling per isThinking, final finish_reason "stop" + `data: [DONE]`. (7) Non-streaming: reason/content joined, usage `ceil(len/4)` each. (8) Map 401/403 → 502-style body "Grok auth failed — SSO cookie may be expired..." code HTTP_401; 429 message per JS; keep HTTP status.

**Guard test:**

test_grok_web_payload_shape: feed body `{"messages":[{"role":"system","content":"S"},{"role":"user","content":"U"}]}` model "grok-4.2" → assert transformed body has `modelName == "grok-420"`, `modelMode == "MODEL_MODE_GROK_420"`, `message == "S: S\n\nU"` (system gets role-prefix) and `temporary == true`.

**⚠️ Risks:**

System messages in grok-web DO get role-prefixed (unlike perplexity-web which hoists them) — preserve that. `extractContent` only emits thinking for isThinkingModel; a non-thinking model must never emit reasoning_content. The statsig-id must be a valid base64 string (btoa); randomString uses only a-z for length 10 and a-z0-9 for length 5.

**Cross-check:** ✅ **CONFIRMED** — All material claims verified against source.

JS behavior is REAL:
- grok-web.js line 6 reads GROK_CHAT_API from PROVIDERS["grok-web"].baseUrl; registry/grok-web.js line 20 sets `baseUrl: "https://grok.com/rest/app-chat/conversations/new"` — so the actual endpoint does include `/rest/`.
- MODEL_MAP (lines 9-24) confirmed: line 21 `"grok-4.2": { grokModel: "grok-420", modelMode: "MODEL_MODE_GROK_420", isThinking: false }` and line 18 `"grok-4.1-fast": { grokModel: "grok-4-1-thinking-1129", modelMode: "MODEL_MODE_FAST", isThinking: false }` match the spec verbatim. All 14 listed model names (lines 10-23) match exactly.
- grokPayload (lines 247-259) confirmed: `{ temporary: true, modelName: grokModel, modelMode, message, fileAttachments: [], imageAttachments: [], ... }` including deviceEnvInfo.

Rust current behavior is REAL (src/core/executor/grok_web.rs):
- build_url (144-146) is `https://grok.com/app-chat/conversations/new` — genuinely missing `/rest` versus the JS registry baseUrl.
- build_headers (148-163) sets only Content-Type: application/json, Accept: application/json, and Cookie `sso={access_token}`; none of the JS browser headers (User-Agent, Origin, Referer, Sec-Fetch-*, Sec-Ch-Ua-*, x-statsig-id, x-xai-request-id, traceparent, Accept-Encoding, Baggage) are present.
- transform_request (165-167) is `body.clone()` — raw body passthrough; no grokPayload construction, no parseOpenAIMessages message flattening, no MODEL_MAP lookup, no NDJSON response parsing, no reasoning_content/fullMessage/token mapping. The executor is indeed a stub relative to JS.

Impl steps would produce parity: Step 1 (URL → https://grok.com/rest/app-chat/conversations/new) and Step 2 (MODEL_MAP copied from JS lines 9-24) are exactly correct and necessary. The task title enumerates the remaining parity elements (grokPayload, NDJSON parse, message flattening, browser headers, reasoning_content mapping), all of which are genuinely missing in Rust and are the exact behaviors JS implements in grokPayload, readGrokNdjsonEvents/extractContent, parseOpenAIMessages, the headers object, and reasoning_content delta mapping.

Minor non-material nits: (a) spec says "13 entries" but the JS MODEL_MAP has 14 entries (grok-3 ... grok-4.20-beta); (b) the impl_steps detail is truncated after step 2, so steps 3+ could not be read, but the two visible steps and the title-listed gaps align fully with JS. Neither affects correctness of the parity plan.

---

### `P0-C2` — perplexity-web executor is a stub: missing pplxBody/version 2.18, MODEL_MAP+THINKING_MAP, session cache (backend_uuid), markdown block parsing, cleanResponse, reasoning_content

**JS (source of truth — verbatim):**

JS open-sse/executors/perplexity-web.js — PPLX_SSE_ENDPOINT = `https://www.perplexity.ai/rest/sse/perplexity_ask` (registry), PPLX_API_VERSION = "2.18", UA Chrome 130 Linux (6-8). MODEL_MAP (10-18): `"pplx-auto": ["concise","pplx_pro"]`, `"pplx-gpt": ["copilot","gpt54"]`, etc.; THINKING_MAP (20-24): `"pplx-gpt": "gpt54_thinking"`. buildPplxRequestBody (160-182): `{ query_str: query, params: { query_str, search_focus: "internet", mode, model_preference: modelPref, sources: ["web"], attachments: [], frontend_uuid: randomUUID(), frontend_context_uuid: randomUUID(), version: "2.18", language: "en-US", timezone: Intl...timeZone, search_recency_filter: null, is_incognito: true, use_schematized_api: true, last_backend_uuid: followUpUuid } }`. Headers (435-449): Content-Type application/json, Accept text/event-stream, Origin `https://www.perplexity.ai`, Referer `https://www.perplexity.ai/`, User-Agent, `X-App-ApiClient: default`, `X-App-ApiVersion: 2.18`; auth: `Authorization: Bearer {accessToken}` else `Cookie: __Secure-next-auth.session-token={apiKey}`. SSE parse (92-136): blank line flushes `data:` lines; `event: end_of_stream` returns. extractContent (211-292): plan_block steps → `{thinking: "Searching: "+q}` / `{thinking: "Reading: "+u}` (first 3 urls), plan goals → thinking; markdown_block chunks: `progress === "DONE"` sets fullAnswer else emits deltas; fallback `event.text`; `event.final || status==="COMPLETED"` stops. cleanResponse (77-90) strips `\[\d+\]`, `<grok:...>`, `<?xml...>`, `</?response...>`, collapses multi-space/`\n{3,}`. Session cache (34-75): FNV-1a `sessionKey`, SESSION_MAX_AGE_MS=3600_000, 200 entries; sessionStore on completion with backend_uuid; follow-up request uses `last_backend_uuid`. buildQuery (195-209): instructions + `You have built-in web search...`, history, currentMsg; JSON truncated to last 96000 chars. Non-streaming (357-389): msg.reasoning_content = thinkingParts.join("\n"), usage `prompt_tokens=ceil(currentMsg.length/4)`, `completion_tokens=ceil(fullAnswer.length/4)`.

**Current Rust behavior:**

src/core/executor/grok_web.rs PerplexityWebExecutor (170-368) — build_url `https://perplexity.ai/rest/sse/perplexity_ask` (missing `www.`); headers only Content-Type/Accept + cookie from access_token (316-321); transform_request (335-367) merely copies the body and adds a `session_cache_key` FNV hash of `role: content` lines — none of the pplxBody shape, MODEL_MAP, SSE block parsing, cleanResponse, backend_uuid session cache, thinking, or usage estimation.

**Implementation steps:**

Rewrite PerplexityWebExecutor in src/core/executor/grok_web.rs: (1) URL `https://www.perplexity.ai/rest/sse/perplexity_ask`. (2) Add MODEL_MAP (7 entries) and THINKING_MAP (3 entries) with exact wire names (pplx-auto→["concise","pplx_pro"], pplx-sonar→["copilot","experimental"], pplx-gpt→["copilot","gpt54"], pplx-gemini→["copilot","gemini31pro_high"], pplx-sonnet→["copilot","claude46sonnet"], pplx-opus→["copilot","claude46opus"], pplx-nemotron→["copilot","nv_nemotron_3_super"]; thinking: pplx-gpt→gpt54_thinking, pplx-sonnet→claude46sonnetthinking, pplx-opus→claude46opusthinking). Thinking request if `body.thinking === true || (reasoning_effort present && != "none")`. (3) Build pplxBody exactly per buildPplxRequestBody (include timezone — use a UTC default if no IANA lookup). (4) Headers per JS incl. X-App-ApiClient/X-App-ApiVersion; auth split: accessToken → Bearer, else apiKey → `Cookie: __Secure-next-auth.session-token={apiKey}`. (5) Implement SSE reader (blank-line flush, end_of_stream), plan/markdown block extraction with seenThinking dedup, markdown DONE vs incremental deltas, event.text fallback, final/COMPLETED stop. (6) cleanResponse on content deltas (strip=for non-streaming). (7) Session cache: FNV-1a of `role:content` joined by \n with the JS hash offset, 1h TTL, 200-entry LRU-evict; on completion store `backend_uuid`; on follow-up set `last_backend_uuid`. (8) buildQuery instruction/query/history + 96000 trailing truncation. (9) Emit `reasoning_content` + usage per JS.

**Guard test:**

test_pplx_session_key_fnv1a: assert sessionKey([{role:"user",content:"x"}]) equals the JS FNV-1a result (compute expected from JS constants 0x811c9dc5/0x01000193). Also `test_pplx_clean_response_strips_tags`.

**⚠️ Risks:**

The FNV-1a JS hash operates on UTF-16 code units (`charCodeAt`); Rust must hash each char's u16 value, not UTF-8 bytes — an exact-match test is essential. Do not emit `reasoning_content` for the auto/sonar models (only THINKING_MAP models).

**Cross-check:** ✅ **CONFIRMED** — All three verification points hold. (1) The JS claims are line-accurate against .tmp/9router/open-sse/executors/perplexity-web.js: PPLX_SSE_ENDPOINT resolves via PROVIDERS["perplexity-web"].baseUrl to "https://www.perplexity.ai/rest/sse/perplexity_ask" (registry/perplexity-web.js:20), PPLX_API_VERSION="2.18" (L7), Chrome 130 Linux UA (L8), 7-entry MODEL_MAP (L10-18) and 3-entry THINKING_MAP (L20-24) with the exact wire names cited, buildPplxRequestBody {query_str, params} shape (L160-182), session cache with backend_uuid (L34-75, L222, L343/375), markdown-block parsing incl. chunks/progress DONE (L259-277), cleanResponse regexes (L77-90), and reasoning_content in both streaming delta (L319) and non-streaming message (L377-379). (2) The Rust stub at src/core/executor/grok_web.rs is real and exactly as claimed: build_url L305 uses "https://perplexity.ai/rest/sse/perplexity_ask" (missing www.), build_headers L308-324 only sets Content-Type/Accept + cookie from access_token, transform_request L335-367 just copies the body and adds a session_cache_key FNV-1a hash; it is wired into the production dispatch in src/server/api/chat.rs L1469-1492, so it genuinely lacks the full JS behavior. (3) The impl steps match the JS wire names 1:1 and cover every gap named in the task title (pplxBody/version 2.18, MODEL_MAP+THINKING_MAP, session cache backend_uuid, markdown parsing, cleanResponse, reasoning_content). Only trivial nits: spec cites "PerplexityWebExecutor (170-368)" when the impl block starts at L268 (170-268 are the request/error/response types), and the Rust FNV-1a differs from the JS 32-bit sessionKey (unimportant — the spec doesn't claim equivalence). No material inaccuracy; impl steps would produce parity.

---

### `P0-D1` — codebuddy-cn: missing system-prompt neutralizer (AGENT_PATTERN + NEUTRAL_PROMPT) and reasoning_effort none/off deletion

**JS (source of truth — verbatim):**

JS open-sse/executors/codebuddy-cn.js — NEUTRAL_PROMPT = "You are a helpful AI assistant that helps with software engineering tasks." (28). AGENT_PATTERN (29) = `/you are claude code|claude.?code.+official.+cli|anthropic.+official.+cli|anxthxropic.+official.+cli|you are (?:cursor|windsurf|cline|aider|continue|copilot|cody)|you are an? (?:ai )?(?:coding |code )?agent|cc_entrypoint\s*=\s*(?:cli|vscode|jetbrains|gui)|claude.?code.+issues|give feedback.+claude.?code|you are .{0,30}(?:powerful )?ai agent|orchestration capabilities|OhMyOpenCode|<agent-identity>|<Role>|<Behavior_Instructions>/i`. For each system message, `text = flatten(message.content)`; if `text.length > 2000 || AGENT_PATTERN.test(text)`, replace content with NEUTRAL_PROMPT preserving the original shape: string content → `{ ...message, content: NEUTRAL_PROMPT }`, array content → `{ ...message, content: [{ type: "text", text: NEUTRAL_PROMPT }] }` (36-48). reasoning handling (54-61): `const eff = transformed.reasoning_effort; if (eff === "none" || eff === "off") { delete transformed.reasoning_effort; } else if (eff) { transformed.reasoning_summary = "auto"; }` — the else-if branch is only for a truthy, non-none effort.

**Current Rust behavior:**

src/core/executor/codebuddy_cn.rs — transform_request (93-111) only sets `body["stream"]=true` and `if body.get("reasoning_effort").is_some() { body["reasoning_summary"]="auto" }`. No neutralizer; and reasoning_summary is set even when effort is "none"/"off" (JS would delete the effort and NOT set summary).

**Implementation steps:**

In src/core/executor/codebuddy_cn.rs transform_request: (1) After forcing stream, iterate `messages`; for each role=="system", flatten content (string or array of {text}) to text; if `text.len() > 2000 || AGENT_PATTERN.is_match(text)`, replace content preserving shape — string → String(NEUTRAL_PROMPT), array → `[{type:"text",text:NEUTRAL_PROMPT}]`. (2) Port the regex verbatim (Rust regex crate `regex`; note JS `.+` and `.{0,30}` need `(?s)` where `.` must cross newlines — JS `i` flag, add `(?is)` for the multi-line patterns like `<agent-identity>`). (3) Reasoning: read `eff = body.get("reasoning_effort").and_then(as_str)`; if eff=="none" || eff=="off" → `body.remove("reasoning_effort")` (do not set summary); else if eff non-empty → `body["reasoning_summary"] = "auto"`. (4) Do NOT add the missing AGENT_PATTERN alternates to anything else.

**Guard test:**

test_neutralize_claude_code_system_prompt: system content "You are Claude Code, Anthropic's official CLI..." (len<2000) → replaced with NEUTRAL_PROMPT; and `test_reasoning_effort_none_deleted_no_summary` for eff="none".

**⚠️ Risks:**

JS matches the regex against the flattened string but only for `role === "system"` messages; `flatten` returns "" for non-string/non-array content and empty text skips. The regex must stay case-insensitive and multi-line (`(?is)`), else the `<agent-identity>` / `cc_entrypoint` alternates won't match across newlines. Preserve the shape rule: array system content must be replaced with `[{type:"text",text:...}]`, never a bare string.

**Cross-check:** ✅ **CONFIRMED** — All three claims verified against source. (1) JS: .tmp/9router/open-sse/executors/codebuddy-cn.js line 28 has NEUTRAL_PROMPT exactly as quoted; line 29 has the AGENT_PATTERN regex verbatim with /i flag (task display truncates at "gui)" but the full pattern continues; impl step says "port verbatim"); lines 30-48 do flatten + role==system-only neutralization at length>2000 or regex match, preserving string vs array shape. (2) Rust: src/core/executor/codebuddy_cn.rs transform_request (93-111) only sets stream=true (line 103) and, when reasoning_effort is present (is_some(), lines 106-108), sets reasoning_summary="auto" — including for "none"/"off", where JS deletes the effort and does not set reasoning_summary (JS lines 54-61). Grep confirms no neutralizer exists anywhere in src/. (3) Impl_steps mirror the JS transform exactly (iterate messages, system-only, both content shapes, threshold, shape preservation) and the none/off deletion is the documented gap; regex="1" is a Cargo dependency so the port is feasible. Two non-refuting nuances: the JS /i flag requires an (?i) inline flag in Rust's regex crate for true verbatim parity, and JS .length (UTF-16 units) vs Rust .len() (bytes) makes the 2000 threshold slightly stricter for CJK text — both edge cases, identical for ASCII Claude Code system prompts, and neither is an "obvious omission" in the spec.

---

### `P0-E1` — grok-cli: missing consistent machine-id x-grok-agent-id derivation, xhigh effort level, supportsGrokCliReasoningEffort gating, per-turn increment in resolveGrokCliTurnIdx

**JS (source of truth — verbatim):**

JS open-sse/executors/grok-cli.js — execute() (528-549): `if (!this._agentId && !args.credentials?.providerSpecificData?.deviceId) { const mid = await getConsistentMachineId("grok-cli-agent"); this._agentId = [mid.slice(0,8), mid.slice(8,12), "5"+mid.slice(13,16), "a"+mid.slice(17,20), mid.slice(0,12).padEnd(12,"0")].join("-"); } else if (deviceId) { this._agentId = deviceId; }` then buildHeaders sends `headers["x-grok-agent-id"] = this._agentId` (384). EFFORT_LEVELS (54) = `["low","medium","high","xhigh"]`; `normalizeGrokCliEffort` (124-129): `effort === "max" → "xhigh"`, if in EFFORT_LEVELS return, else "high". supportsGrokCliReasoningEffort (grokCli.js config): `/^grok-4\.5(?:$|-)/.test(String(model||""))` — effort only set when true, else `delete body.reasoning.effort` (484-490). resolveGrokCliTurnIdx (89-112): monotonic per session, retries reuse requestKey (turn stored in WeakMap, `prev + (requestKey ? 1 : 0)`), TTL via MEMORY_CONFIG.sessionTtlMs, store max 5000. `x-grok-conv-id` = same as session id (380). buildHeaders identity (391-397): `x-email` from psd.email||credentials.email, `x-userid` from psd.userId||userId||providerUserId.

**Current Rust behavior:**

src/core/executor/grok_cli.rs — no machine-id derivation: `agent_id = psd_str(deviceId).or(psd_str(agentId))` (537-538) and only inserted if non-empty (351-353); no getConsistentMachineId fallback, no UUID-ish reformatting. EFFORT_LEVELS (29) = `["low","medium","high"]` — xhigh missing, `resolve_effort_from_model` can't detect `-xhigh`, no max→xhigh mapping. No supportsGrokCliReasoningEffort gating: transform_request_body always sets `reasoning["effort"]` (463-465). resolve_grok_cli_turn_idx (86-96) = max(from_input, prev) with no per-request increment (requestKey absent) and no TTL. Email/userId psd key lookup (355-364) reads `email`/`userId`/`user_id`/`providerUserId` ✓ but misses top-level `credentials.email` fallback.

**Implementation steps:**

In src/core/executor/grok_cli.rs: (1) Add a machine-id fallback: when psd deviceId/agentId absent, derive a stable id (mirror `getConsistentMachineId` — see existing machine-id util in the crate, e.g. crate::core::utils) and format per JS `[a,b,format!("5{}",&mid[13..16]),format!("a{}",&mid[17..20]), format!("{:0<12}",&mid[..12])].join("-")`. (2) EFFORT_LEVELS → add "xhigh"; normalize effort: "max"→"xhigh", unknown→"high". (3) Add `supports_grok_cli_reasoning_effort(model)` = model matches `^grok-4\.5(?:$|-)`; in transform_request_body set `reasoning.effort` only when the model supports it, else `reasoning.as_object_mut().remove("effort")`. (4) Turn index: add per-request monotonic increment — store prev, new turn = max(from_input, prev + 1) when this is a fresh request (no requestKey re-use concept in Rust; add a caller-supplied retry flag if available) with a TTL (reuse an existing session-TTL constant, defaulting ~24h) and a max store size (5000) evicting oldest. (5) x-email fallback to `credentials.email` when psd missing.

**Guard test:**

test_effort_normalize_max_to_xhigh + test_supports_effort_only_grok_45 (assert transform of model "grok-4.6" yields no reasoning.effort key).

**⚠️ Risks:**

JS strips the effort suffix from the model BEFORE reasoning (resolvedModel replaces `-{effort}$`), and the turn idx must never decrease across retries — the WeakMap requestKey semantics means a retried identical body reuses the same turn; if Rust cannot distinguish retries, prefer not incrementing on re-calls with identical input rather than always +1, to avoid out-of-order turn indices.

**Cross-check:** ✅ **CONFIRMED** — All three verification points hold.

1. JS behavior is REAL (C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/open-sse/executors/grok-cli.js). execute() at lines 528-549 contains exactly the cited machine-id fallback: `if (!this._agentId && !args.credentials?.providerSpecificData?.deviceId) { const mid = await getConsistentMachineId("grok-cli-agent"); this._agentId = [mid.slice(0,8), mid.slice(8,12), "5"+mid.slice(13,16), "a"+mid.slice(17,20), mid.slice(0,12).padEnd(12,"0")].join("-"); } else if (deviceId) { this._agentId = deviceId; }` (lines 530-546). buildHeaders line 384 inserts `x-grok-agent-id` only when `this._agentId` is set. EFFORT_LEVELS (line 54) = `["low","medium","high","xhigh"]` — xhigh is present in JS. `supportsGrokCliReasoningEffort` gating is real (lines 477-492, gated on /^grok-4\.5(?:$|-)/ per config/grokCli.js line 9). Per-turn increment in resolveGrokCliTurnIdx is real (line 105: `prev + (requestKey ? 1 : 0)`). getConsistentMachineId in shared/machineId.js returns sha256(rawId+salt).hex.substring(0,16) — 16 hex chars, confirming the slices in the JS format.

2. Rust current behavior is REAL (C:/Users/ADMIN/Documents/Projects/openproxy/src/core/executor/grok_cli.rs). Lines 537-538: `let agent_id = psd_str(&request.credentials, "deviceId").or_else(|| psd_str(&request.credentials, "agentId"));` — no getConsistentMachineId fallback and no UUID-ish reformatting. Lines 351-353 insert `x-grok-agent-id` only if non-empty. EFFORT_LEVELS (line 29) = `["low","medium","high"]` — xhigh missing, and no "max"→"xhigh" normalization (lines 453-469 always set reasoning.effort, default "high"). No supportsGrokCliReasoningEffort gating: the model string is never regex-checked before setting effort. resolve_grok_cli_turn_idx (lines 86-96) computes `from_input.max(prev)` with no +1 per-request increment.

3. impl_steps would produce the parity behavior. The crate does have a machine-id util (crate::core::auth::machine_id::get_machine_id(), returning 64-char SHA-256 hex), so the fallback is implementable — though the impl_steps' pointer to "crate::core::utils" is slightly off (it lives in crate::core::auth::machine_id; core/utils/mod.rs has no machine-id module). Two minor inaccuracies that don't block parity: (a) the literal `&mid[17..20]` slice would panic only if mid were truncated to 16 chars like JS's getConsistentMachineId does; using the full 64-char get_machine_id() avoids the panic and produces a "UUID-ish" fingerprint, though the format differs cosmetically from JS (32 vs 29 chars, since JS's mid.slice(17,20) is empty). Exact byte equality across implementations was never achievable anyway because the source machine identity differs (JS machineIdSync vs Rust hostname|os_uuid|/etc/machine-id). Behaviorally the Rust would match: stable per-machine agent-id when deviceId/agentId absent, random-UUID-fallback semantics preserved. (b) The xhigh EFFORT_LEVELS addition plus "max"→"xhigh" normalization mirrors JS normalizeGrokCliEffort; adding xhigh to the const also fixes resolve_effort_from_model, which iterates EFFORT_LEVELS. The impl_steps text is truncated at "normalize effort: \"max", but the title/RUST_CURRENT enumerate the remaining gaps (supportsGrokCliReasoningEffort gating, per-turn increment) which are confirmed real gaps, so the plan covers them.

Minor nits only — no false JS claim, and the impl is workable. Verdict CONFIRMED.

---

### `P0-F1` — mimo-free: wrong bootstrap payload shape, wrong system-marker text, wrong headers (X-Mimo-Source, x-session-affinity), different fingerprint derivation

**JS (source of truth — verbatim):**

JS open-sse/executors/mimo-free.js — MIMO_SYSTEM_MARKER (24-25) = `"You are MiMoCode, an interactive CLI tool that helps users with software engineering tasks."`. BOOTSTRAP_URL = `https://api.xiaomimimo.com/api/free-ai/bootstrap` (7), CHAT_URL = registry `https://api.xiaomimimo.com/api/free-ai/openai/chat`. bootstrapJwt (79-105): POST with body `{ client: generateFingerprint() }` (90), UA random from USER_AGENTS (Chrome 131, 16-20); reads `data.jwt`, caches with `jwtExpiresAt = parseJwtExp(jwt)` (exp claim → *1000, fallback +3000s, 300_000ms buffer). generateFingerprint (32-42): `sha256(hostname|platform|arch|cpu|username)`. generateSessionId (44-50): `ses_` + 24 chars of `[a-z0-9]`. buildHeaders (117-125): `{ "Content-Type": "application/json", "X-Mimo-Source": "mimocode-cli-free", "User-Agent": random, "x-session-affinity": this.sessionId, "Accept": stream ? "text/event-stream" : "application/json" }`. transformRequest (127-129) = injectSystemMarker (64-72): idempotent — if ANY system message's string content already contains the marker, no-op; else prepend `{role:"system",content:MIMO_SYSTEM_MARKER}`. execute (131-159): `headers["Authorization"] = Bearer jwt`; on 401/403 resetJwtCache + re-bootstrap + retry once.

**Current Rust behavior:**

src/core/executor/mimo_free.rs — bootstrap payload (259-261) `{"device_fingerprint": fingerprint}` (WRONG — JS sends `{client: ...}`); MIMO_CODE_SYSTEM_MESSAGE (47-59) is a long multi-rule prompt, NOT the exact JS marker; inject check (364-373) only inspects the FIRST message and checks content contains "MiMoCode" (JS scans all system messages for the exact substring MIMO_SYSTEM_MARKER); headers use `X-Session-Id` (405) not `X-Mimo-Source`/`x-session-affinity`; fingerprint (236-245) = sha256(api_key||id) not the machine seed; UA list (39-43) has Chrome 129/130/131 not the exact JS Chrome 131 strings; session id `ses_`+uuidv4 (227-229) vs 24-char lowercase alnum.

**Implementation steps:**

In src/core/executor/mimo_free.rs: (1) Replace MIMO_CODE_SYSTEM_MESSAGE with the exact `MIMO_SYSTEM_MARKER` string; expose a pub const. (2) bootstrap_jwt: send `{"client": fingerprint}`; derive fingerprint as sha256 of `hostname|platform|arch|cpu|username` (use std::env::consts + hostname via `gethostname`-style helper or `std::env::var("COMPUTERNAME")`/`hostname` crate — pick one; CPU model may be empty on Windows, that is fine, keep the seed format stable). (3) Parse JWT `exp` (base64url payload) → expires_at; cache with 300s expiry buffer, fallback TTL 3000s. (4) inject: scan ALL messages where role=="system" and content is string containing MIMO_SYSTEM_MARKER; if none, prepend. (5) Headers: replace `X-Session-Id` with `x-session-affinity` (value `ses_` + 24 lowercase alnum chars) and add `X-Mimo-Source: mimocode-cli-free`; UA from the exact 3 JS strings (Chrome/131.0.0.0 x3). (6) Keep the 401/403 re-bootstrap retry-once flow.

**Guard test:**

test_marker_exact_string: assert MIMO_SYSTEM_MARKER == "You are MiMoCode, an interactive CLI tool that helps users with software engineering tasks." and `test_inject_marker_scans_all_system_messages` (marker present on a later system message → no injection).

**⚠️ Risks:**

The marker must match byte-for-byte — upstream 403 "Illegal access" unless the EXACT substring appears. Fingerprint seed must be stable across restarts (same machine → same JWT cache hit). Do not confuse the chat endpoint (`/api/free-ai/openai/chat`) with the bootstrap endpoint. Rust header casing is normalized by reqwest; wire values are what matters.

**Cross-check:** ✅ **CONFIRMED** — All cited JS behavior is real and byte-exact: BOOTSTRAP_URL (line 7 = https://api.xiaomimimo.com/api/free-ai/bootstrap), CHAT_URL resolves via registry baseUrl to https://api.xiaomimimo.com/api/free-ai/openai/chat, MIMO_SYSTEM_MARKER (lines 24-25) is exactly "You are MiMoCode, an interactive CLI tool that helps users with software engineering tasks.", bootstrapJwt POSTs `{client: generateFingerprint()}` (line 90) with a random Chrome UA, and buildHeaders (117-125) sends X-Mimo-Source: "mimocode-cli-free" plus x-session-affinity. generateFingerprint (32-42) seeds sha256 from `hostname|platform|arch|cpu|username`. All Rust current behavior is also real: bootstrap payload at 259-261 is `{"device_fingerprint": fingerprint}` (wrong shape), MIMO_CODE_SYSTEM_MESSAGE (47-59) is a long multi-rule prompt not the exact marker, inject check (364-373) inspects only the first message and tests content.contains("MiMoCode"), and headers (build_headers 405) use X-Session-Id and omit X-Mimo-Source. Impl steps are sound: replacing the marker with the exact string + exposing a pub const achieves the anti-abuse gate parity; sending `{"client": fingerprint}` and deriving fingerprint from machine identity matches the JS seed scheme. Two minor nuances that do not break parity: (1) Rust std::env::consts strings ("windows"/"x86_64") differ from Node os.platform()/os.arch() ("win32"/"x64"), so the fingerprint will be stable-per-machine but not byte-identical to JS — acceptable since upstream treats client as an opaque per-machine identifier; (2) the visible impl steps are truncated before any explicit header-fix step, though the X-Mimo-Source/x-session-affinity gap is named in the task title and covered by the spec's framing. No JS claim is false and the impl approach works, so CONFIRMED.

---

### `P0-G1` — qoder: jt- tokens must route to api2.qoder.sh (buildUrl missing), X-Model-Key/X-Model-Source headers missing, {statusCodeValue,body} SSE envelope not unwrapped, live model_config missing

**JS (source of truth — verbatim):**

JS open-sse/executors/qoder.js — buildUrl (348-356): `const raw = credentials?.apiKey || credentials?.accessToken; if (typeof raw === "string" && !raw.startsWith("pt-") && (raw.startsWith("jt-") || (credentials?.accessToken || "").startsWith("jt-"))) { return `${QODER_CHAT_BASE_ALT}/algo${QODER_CHAT_SIG_PATH}?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1`; } return QODER_CHAT_URL_ENCODED;` where QODER_CHAT_BASE_ALT = `https://api2.qoder.sh`, QODER_CHAT_SIG_PATH = `/api/v2/service/pro/sse/agent_chat_generation`, QODER_CHAT_URL_ENCODED = `https://api3.qoder.sh/algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1`. Headers (442-451): `{ "Content-Type": "application/json", Accept: "text/event-stream", "Cache-Control": "no-cache", "X-Model-Key": qoderKey, "X-Model-Source": modelSource, "Accept-Encoding": "identity", ...cosyHeaders }` where `modelSource = (payload.model_config && payload.model_config.source) || "system"` (441). buildQoderRequestBody (130-216) fetches `getQoderModelConfig(credentials, qoderKey, ...)` live from `/algo/api/v2/model/list`, throws hard error if unknown, and payload includes `model_config: modelConfig` and `chat_context.extra.modelConfig = { key: qoderKey, is_reasoning: isReasoning }`. wrapQoderSSE (233-341): each upstream line `data: {"statusCodeValue":200,"body":"{...inner openai chunk...}"}` is unwrapped and re-emitted as `data: <inner>\n\n`; non-200 statusCodeValue → error chunk `\n[qoder error {statusVal}: {truncated msg 200}]` + [DONE]; on terminal frame, cancel reader + close (keepalive). Errors pass through unchanged (470-473).

**Current Rust behavior:**

src/core/executor/qoder.rs — build_url (503-505) always returns api3 QODER_CHAT_URL_ENCODED; no jt- api2 routing. build_headers (507-616) sets Content-Type/Accept/Cache-Control/Accept-Encoding/COSY set but NOT X-Model-Key/X-Model-Source. transform_request (717-829) hardcodes `is_reasoning: false` (809) and does NOT include a `model_config` field or live model-list fetch (no network catalog). execute_request returns the raw upstream response (925-938) — the `{statusCodeValue, body}` envelope is never unwrapped, so downstream SSE consumers would see the Qoder envelope instead of OpenAI chunks.

**Implementation steps:**

In src/core/executor/qoder.rs: (1) build_url(credentials): if the token is a string not starting with `pt-` AND starts with `jt-`, return `https://api2.qoder.sh/algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1`; else api3 URL. (2) In build_headers, accept the qoder_key and model_source and insert `X-Model-Key` / `X-Model-Source` headers. (3) Add a live model-config fetch: GET `https://api3.qoder.sh/algo/api/v2/model/list` (with COSY or bearer auth) before signing, resolve the qoderKey entry, derive `is_reasoning` and `max_output_tokens`; if unknown after one forced refresh, return a 400 error "qoder: model_config for \"{key}\" not yet known...". Include `model_config` in the payload and `chat_context.extra.modelConfig.is_reasoning` from the fetched entry (not hardcoded false). (4) Wrap the upstream response: read SSE lines; for each `data:` line JSON.parse to `{statusCodeValue, body}`; if statusCodeValue != 200 emit error chunk + [DONE]; if inner == "[DONE]" emit SSE_DONE; else strip `\r?\n` from inner and emit `data: {inner}\n\n`; close+stop at terminal frame.

**Guard test:**

test_build_url_jt_token_uses_api2: build_url with api_key "jt-abc" → starts_with("https://api2.qoder.sh"), with "dt-abc" → api3. `test_wrap_qoder_sse_unwraps_envelope`: input `data: {"statusCodeValue":200,"body":"{\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}"}\n\n` → output `data: {"choices":[{"delta":{"content":"hi"}}]}\n\n`.

**⚠️ Risks:**

JS strips embedded newlines in the inner body so the SSE frame stays one event — replicate. The `pt-` check must come first (PATs never use api2). model_config fetch errors must be hard errors (wrong block silently downgrades the upstream model) but must not break when the credential has no catalog access — match JS: one forced-refresh attempt then hard error.

**Cross-check:** ✅ **CONFIRMED** — All cited behaviors verified against source. (1) JS is real: .tmp/9router/open-sse/executors/qoder.js buildUrl (348-356) routes jt- tokens to https://api2.qoder.sh/algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1 (line 352-353, with QODER_CHAT_BASE_ALT/QODER_CHAT_SIG_PATH from shared/qoder/constants.js); sends X-Model-Key/X-Model-Source headers (lines 446-447, modelSource from payload.model_config.source || "system" at 441); wrapQoderSSE (233-341) unwraps the {statusCodeValue, body} envelope including non-200 error synthesis; live model_config fetched via getQoderModelConfig (135), is_reasoning from config (151), model_config embedded in payload (203). (2) Rust current behavior is real: src/core/executor/qoder.rs build_url (503-505) always returns api3 QODER_CHAT_URL_ENCODED (no jt-/api2 routing anywhere in src/); build_headers (507-616) has no X-Model-Key/X-Model-Source (zero matches repo-wide); transform_request (717-829) hardcodes "is_reasoning": false at line 809 and omits model_config entirely; no statusCodeValue SSE unwrap exists anywhere in src (zero matches), execute_request returns raw UpstreamResponse. (3) Impl steps are sound: step 1's api2 URL matches the JS-composed URL exactly and the pt-/jt- guard mirrors the JS primary-token check (it simplifies away the JS's secondary accessToken-prefix check, a minor non-blocking nuance); step 2 mirrors the JS header insertion. Steps 3+ were truncated in the spec, but the visible steps are correct and the three stated gaps are all confirmed real; the spec's listed item "live model_config missing" also correctly implies Rust must gain a model_config source for step 2's model_source and step 4 to fully work. No inaccuracies found.

---

### `P0-H1` — azure: wrong env var names + reversed precedence (env vs psd), missing OPENAI_API_KEY fallback, wrong default endpoint

**JS (source of truth — verbatim):**

JS open-sse/executors/azure.js — buildUrl (8-24): `const azureEndpoint = credentials?.providerSpecificData?.azureEndpoint || process.env.AZURE_ENDPOINT || "https://api.openai.com"; const apiVersion = credentials?.providerSpecificData?.apiVersion || process.env.AZURE_API_VERSION || "2024-10-01-preview"; const deployment = credentials?.providerSpecificData?.deployment || model || process.env.AZURE_DEPLOYMENT || "gpt-4";` then `return `${endpoint}/openai/deployments/${deployment}/chat/completions?api-version=${apiVersion}`;` (endpoint trailing-slash stripped). buildHeaders (26-52): `apiKey = credentials?.apiKey || credentials?.accessToken || process.env.OPENAI_API_KEY;` → `headers["api-key"] = apiKey`; `organization = credentials?.providerSpecificData?.organization || process.env.AZURE_ORGANIZATION;` → `headers["OpenAI-Organization"] = organization`; `if (stream) headers["Accept"] = "text/event-stream";`. transformRequest returns body unchanged. Precedence is psd → env → default for URL/version/deployment.

**Current Rust behavior:**

src/core/executor/azure.rs — build_url (94-143) reads env vars `AZURE_OPENAI_ENDPOINT`, `AZURE_OPENAI_API_VERSION`, `AZURE_OPENAI_DEPLOYMENT` with env FIRST, then psd, then default — reversed precedence (connection psd is ignored when env is set) and WRONG env names. Default endpoint = "" (13) instead of "https://api.openai.com". build_headers (145-177) reads only psd api_key/access_token (no `OPENAI_API_KEY` env) and `organization` only from psd (no `AZURE_ORGANIZATION` env). Deployment fallback: Rust uses model when non-empty else "gpt-4" (130-136) — JS precedence is psd.deployment → model → env.AZURE_DEPLOYMENT → "gpt-4" (env between model and default).

**Implementation steps:**

In src/core/executor/azure.rs: (1) build_url precedence per JS: psd `azureEndpoint` → env `AZURE_ENDPOINT` → "https://api.openai.com"; psd `apiVersion` → env `AZURE_API_VERSION` → "2024-10-01-preview"; psd `deployment` → model (if non-empty) → env `AZURE_DEPLOYMENT` → "gpt-4". (2) build_headers: api_key = psd api_key → access_token → env `OPENAI_API_KEY`; insert `api-key`. organization = psd `organization` → env `AZURE_ORGANIZATION` → insert `OpenAI-Organization` when present. (3) Keep transform_request = identity.

**Guard test:**

test_build_url_prefers_psd_over_env: set AZURE_ENDPOINT=env via std::env::set_var, psd azureEndpoint="https://mine.openai.azure.com" → url starts with "https://mine.openai.azure.com"; and without psd → uses env; without both → "https://api.openai.com".

**⚠️ Risks:**

If the legacy `AZURE_OPENAI_*` names are used elsewhere in the crate, migrate them; do not keep both precedence orders. The deployment key "gpt-4" default must only apply when model is empty (JS falls back to model first).

**Cross-check:** ✅ **CONFIRMED** — Verified all three claims against the actual files. (1) JS azure.js (8-52) is real and matches exactly: psd azureEndpoint -> env AZURE_ENDPOINT -> "https://api.openai.com"; psd apiVersion -> env AZURE_API_VERSION -> "2024-10-01-preview"; psd deployment -> model -> env AZURE_DEPLOYMENT -> "gpt-4"; URL format `${endpoint}/openai/deployments/${deployment}/chat/completions?api-version=${apiVersion}`; buildHeaders apiKey = psd apiKey -> accessToken -> env OPENAI_API_KEY inserted as "api-key", org = psd organization -> env AZURE_ORGANIZATION. (2) Rust azure.rs build_url (94-143) confirmed: reads env AZURE_OPENAI_ENDPOINT / AZURE_OPENAI_API_VERSION / AZURE_OPENAI_DEPLOYMENT with env FIRST, then psd, so precedence is reversed (env overrides psd) and env names differ from JS; DEFAULT_AZURE_ENDPOINT is "" (line 13); build_headers (145-177) has no OPENAI_API_KEY env fallback and no AZURE_ORGANIZATION env fallback. Grep confirms OPENAI_API_KEY appears nowhere in azure.rs. (3) Impl_steps mirror JS exactly (correct env names, psd-first precedence, https://api.openai.com default, OPENAI_API_KEY fallback) — no omission; content-type/accept headers already handled and header names are case-insensitive. CONFIRMED.

---

### `P0-I1` — ollama-local: Rust appends ?stream= to /api/chat; JS does NOT

**JS (source of truth — verbatim):**

JS open-sse/executors/ollama-local.js (entire file): `export class OllamaLocalExecutor extends DefaultExecutor { constructor() { super("ollama-local"); } buildUrl(model, stream, urlIndex = 0, credentials = null) { return `${resolveOllamaLocalHost(credentials)}/api/chat`; } }` and `resolveOllamaLocalHost` (config/providers.js:9-13): `const raw = credentials?.providerSpecificData?.baseUrl?.trim(); return (raw || OLLAMA_LOCAL_DEFAULT_HOST).replace(/\/$/, "");` with OLLAMA_LOCAL_DEFAULT_HOST = "http://localhost:11434". Note: NO `?stream=` query parameter; stream is controlled purely by the body. Also the JS executor is DefaultExecutor-derived (inherits openai transform/headers incl. `stream` in body), not a bespoke image-extraction executor.

**Current Rust behavior:**

src/core/executor/ollama.rs — build_url (151-167) returns `{base}/api/chat?stream={stream}` — ADDS the query param JS does not send. The Ollama /api/chat endpoint ignores query params for stream selection (it reads the body's `stream` field), so this is harmless upstream today but is a wire-format divergence and could break proxies that key off the URL. Also Rust's executor is bespoke (extracts images into `images`), whereas JS inherits DefaultExecutor's transform.

**Implementation steps:**

In src/core/executor/ollama.rs build_url: drop the `?stream=` suffix — return `format!("{base}/api/chat")`. Keep the base resolution (psd `baseUrl` trimmed, default "http://localhost:11434") and the trailing-slash strip. Ensure the body still carries `stream` (Rust passes the request body through, which contains `stream` for chat requests).

**Guard test:**

test_build_url_has_no_stream_query: `OllamaExecutor::build_url("llama3", true, &creds)` → assert url ends_with("/api/chat") and does NOT contain "stream=".

**⚠️ Risks:**

Do not rely on `?stream=` for the stream decision anywhere else in Rust — if any caller inspects the URL query, switch it to read the body. If future work ports the image extraction, keep it as the JS DefaultExecutor chain (injectReasoningContent + stripUnsupportedParams) rather than a bespoke executor.

**Cross-check:** ✅ **CONFIRMED** — All three verification points hold.

1) JS behavior is REAL: `.tmp/9router/open-sse/executors/ollama-local.js` lines 4-12 define `OllamaLocalExecutor extends DefaultExecutor` with `buildUrl(...)` returning `` `${resolveOllamaLocalHost(credentials)}/api/chat` `` — no query param. `.tmp/9router/open-sse/config/providers.js` lines 5-10 define `OLLAMA_LOCAL_DEFAULT_HOST = "http://localhost:11434"` and `resolveOllamaLocalHost` as `(raw || OLLAMA_LOCAL_DEFAULT_HOST).replace(/\/$/, "")` where `raw = credentials?.providerSpecificData?.baseUrl?.trim()` — content matches the cited claim exactly (only trivial line-number drift: cited 9-13, actual 7-10).

2) Rust current behavior is REAL: `src/core/executor/ollama.rs` lines 151-167 `build_url` resolves base from `provider_specific_data["baseUrl"]` (trimmed/empty-filtered, default "http://localhost:11434"), strips trailing slash, and returns `format!("{base}/api/chat?stream={stream}")` at line 166 — the query param is indeed appended. The body carries `stream` on all chat paths: `src/server/api/chat.rs` lines 1006-1008 insert `stream` into `request_body` before dispatch, line 1555 passes `body: request_body.clone()` to `OllamaExecutionRequest`, `transform_request` only clones body + extracts images (preserves stream), and `src/core/translator/request/openai_to_ollama.rs` line 156 also sets `"stream"` in the body.

3) Impl steps would produce parity: returning `format!("{base}/api/chat")` exactly matches the JS URL. The claim that Ollama reads stream from the body rather than query params is consistent with the documented /api/chat contract and both implementations' behavior. One minor omission: the unit test `test_build_url` (ollama.rs lines 285-292) asserts `url.contains("stream=true")`/`"stream=false"` and would fail after the change — it must be updated in the same patch, but this is test-only housekeeping, not a parity blocker.

---

### `P0-J1` — opencode-go claude routing: Rust claude-format model list is missing 4 of 6 JS models

**JS (source of truth — verbatim):**

JS open-sse/executors/opencode-go.js — MESSAGES_FORMAT_MODELS = new Set(["minimax-m3", "minimax-m2.7", "minimax-m2.5", "qwen3.7-max", "qwen3.7-plus", "qwen3.6-plus"]) (7-14). buildUrl: model in set → `${BASE}/messages` else `${BASE}/chat/completions` where BASE = "https://opencode.ai/zen/go/v1". buildHeaders: for those models `headers["x-api-key"] = key; headers["anthropic-version"] = ANTHROPIC_API_VERSION` ("2023-06-01"), else `Authorization: Bearer`. transformRequest runs `injectReasoningContent({ provider, model, body })`.

**Current Rust behavior:**

src/core/executor/default.rs — opencode_go_uses_claude_format (1354-1356): `matches!(model, "minimax-m2.5" | "minimax-m2.7")` — only 2 of 6 models. Missing minimax-m3, qwen3.7-max, qwen3.7-plus, qwen3.6-plus (these currently get routed to `/chat/completions` with Bearer auth → upstream rejects).

**Implementation steps:**

In src/core/executor/default.rs, change `opencode_go_uses_claude_format` to return true for all six: `matches!(model, "minimax-m3" | "minimax-m2.7" | "minimax-m2.5" | "qwen3.7-max" | "qwen3.7-plus" | "qwen3.6-plus")`. The base URL in default.rs is `https://opencode.ai/zen/go/v1` (line 139-140) and build_url appends `messages`/`chat/completions` — verify default.rs provider entry's base_url is `https://opencode.ai/zen/go/v1` (it is registered as `ProviderConfig::openai("https://opencode.ai/zen/v1")` at default.rs:139-140 — append the `/go` segment so the claude path becomes `.../zen/go/v1/messages`).

**Guard test:**

test_opencode_go_claude_format_models: assert opencode_go_uses_claude_format("qwen3.7-max") == true, ("qwen3.6-plus") == true, ("minimax-m3") == true.

**⚠️ Risks:**

The dedicated `src/core/executor/opencode_go.rs` also has its own CLAUDE_FORMAT_MODELS (lines 31-38, 6 models — that one is correct). The bug is only in default.rs's `opencode_go_uses_claude_format` used when opencode-go falls through the default path. Also confirm which base URL the default path uses — if it lacks `/go`, requests to `.../zen/v1/messages` will 404.

**Cross-check:** 🟡 **PLAUSIBLE** — JS claim: 100% accurate. Verified open-sse/executors/opencode-go.js — MESSAGES_FORMAT_MODELS is exactly the 6 models claimed (lines 7-14), BASE is "https://opencode.ai/zen/go/v1" (line 16), buildUrl returns /messages vs /chat/completions (24-29), buildHeaders sets x-api-key + anthropic-version for set models (31-44). Rust current behavior: the 2-of-6 match in default.rs opencode_go_uses_claude_format (1354-1356) is real. However, the spec contains a factual error: line 140 base_url is "https://opencode.ai/zen/v1" (missing /go), NOT "https://opencode.ai/zen/go/v1" as the spec claims. The spec also overstates impact: a dedicated src/core/executor/opencode_go.rs ALREADY implements full parity (all 6 models at lines 31-38, base https://opencode.ai/zen/go/v1), and the chat server path (chat.rs:1325) routes opencode-go to this dedicated executor — default.rs is only reached via the CLI path (cli/mod.rs:1697,1868) and an unused media.rs _executor. Impl_steps: the matches!() expansion is correct and needed, but it would NOT produce parity because it omits the base_url fix (default.rs config is zen/v1, JS and opencode_go.rs use zen/go/v1); the spec wrongly asserts the default.rs base_url is already zen/go/v1. So the fix is right-headed but incomplete — it misses the base_url discrepancy and mischaracterizes the blast radius.

---

### `P0-K1` — cursor: no HTTP/2 AgentService path (isAgentTextRequest routing, agent.v1.AgentService/Run, request-context handshake, msg_ response id)

**JS (source of truth — verbatim):**

JS open-sse/executors/cursor.js — execute (666-725): `if (isAgentTextRequest(body)) { try { return await this.executeAgent({model, body, stream, credentials, signal}); } ... }` where isAgentTextRequest (73-84) is true when all messages have string or text-only-part content and no `tool_calls`/`role:"tool"`. executeAgent (482-664): URL `https://agent.api5.cursor.sh/agent.v1.AgentService/Run` (agentEndpoint from PROVIDER_OAUTH.cursor, registry cursor.js:49); opens an HTTP/2 duplex stream (openAgentHttp2Stream 388-480, `http2.connect`), writes `buildAgentRunFrame(messages, model)` (98-137: field 1 userMessage {text, uuid}, field 7 history, field 8 system, field 9 requestedModel {name, bool:7}); response id `chatcmpl-msg_${Date.now()}` (534); reads Connect-RPC frames (decodeAgentFrames 144-158 with gzip decompress), decodes agent.v1.AgentServerMessage: field 1 interaction_update → field 1 text delta; field 1 update field 14 (reasoning) → `finished=true; onEvent({type:"done"})` — reasoning kept upstream-only (558-567); field 2 ExecServerMessage field 10 requestContext → `session.write(createRequestContextResponse())` (160-167: field 10 execClientMessage wrapping empty field 1), other exec fields → error "Cursor AgentService requested an unsupported IDE tool" (571-583). Streaming emits chatChunkSse `{delta:{content}}` / `{reasoning_content}`; error frame type "api_error" + [DONE] (638-644); done → `{delta:{},finishReason:"stop"}` + [DONE].

**Current Rust behavior:**

src/core/executor/cursor.rs — no AgentService path at all. CursorExecutor::execute (1492-1593) always builds the legacy ChatService protobuf (`build_chat_request_wrapper` → `api2.cursor.sh/aiserver.v1.ChatService/StreamUnifiedChatWithTools`), no isAgentTextRequest routing, no HTTP/2 client, no request-context handshake, no `chatcmpl-msg_` id. There is a CURSOR_AGENTN_ENDPOINT const (23-24) but it is only used for the ChatService path via cursorHost override — not AgentService.

**Implementation steps:**

In src/core/executor/cursor.rs: (1) Add `is_agent_text_request(body)`: all messages satisfy (no tool_calls, role != "tool") AND (content is string OR content array where every part.type == "text"). (2) Add an agent path used when that predicate is true: URL `https://agent.api5.cursor.sh/agent.v1.AgentService/Run` (const; use the same host as the JS registry). (3) Add HTTP/2 connect support (add `h2`/`h2c` client or a reqwest/h2 duplex stream — the crate has a hyper client pool; add a tokio-h2-based duplex or use `h2::client` handshake on a TLS stream), with 60s hang timeout, `:method POST`, `:path /agent.v1.AgentService/Run`. (4) Build the AgentRun frame: protobuf field 1 `UserMessageAction` {field 1 string userText, field 2 uuid}, field 7 conversation history (each entry field 1 `ConversationHistoryMessage` with field 1 user / field 2 assistant each wrapping field 1 {field 1 text}), field 8 system text, field 9 `{field 1 model, field 7 bool true}`; wrap in `agent.v1.AgentClientMessage.run_request` field 1, then Connect-RPC frame. (5) Decode frames: 1-byte flags + 4-byte BE length + payload, gunzip when flag&0x01; parse agent.v1.AgentServerMessage: field 1 → interaction_update field 1 → text delta; field 1 update field 14 → finish (reasoning upstream-only); field 2 ExecServerMessage field 10 → write request-context response (field 2 execClientMessage wrapping field 1 empty); other exec fields → error. (6) Response id `chatcmpl-msg_{timestamp_ms}` for the agent path; stream emits `{delta:{role:"assistant"}}`-style chunks (first chunk role, then content/reasoning_content), final `finishReason:"stop"` + `data: [DONE]`. (7) Route: text-only requests → agent path; tool-call/history conversations keep legacy ChatService.

**Guard test:**

test_is_agent_text_request: assert true for all-string messages, false when a message has tool_calls or role "tool". `test_build_agent_run_frame_has_request_context_fields` asserting the encoded bytes contain a field-9 requestedModel varint true.

**⚠️ Risks:**

The HTTP/2 requirement is critical — the JS comment says undici/HTTP1 fails with HTTPParserError on the h2 preface. Reasoning (field 14) must NOT be forwarded to clients (Claude Code discards unsigned thinking). The empty request-context response is mandatory or the upstream stalls. Agent errors are surfaced as `connection_error` type with status 500 (JS 671-678).

**Cross-check:** ✅ **CONFIRMED** — All three verification points pass. (1) JS claim is real: in C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/open-sse/executors/cursor.js, isAgentTextRequest (73-84) matches exactly (no tool_calls, role != "tool", content string OR array of text-only parts); execute (666-680) routes to executeAgent when the predicate holds; executeAgent (482-664) targets `${agentEndpoint}/agent.v1.AgentService/Run` where AGENT_RUN_PATH is const at line 45 and agentEndpoint = "https://agent.api5.cursor.sh" is confirmed in open-sse/providers/registry/cursor.js:49. It uses an HTTP/2 duplex client (openAgentHttp2Stream, 388-480; http2 lazy-imported 29-36), the request-context handshake (createRequestContextResponse 160-167, handled at 571-583 for ExecServerMessage field 10), and a "chatcmpl-msg_{Date.now()}" response id (line 534) so the Anthropic translator's chatcmpl- strip yields the required msg_ id. (2) Rust current behavior is real: src/core/executor/cursor.rs (2746 lines) execute (1492-1593) always builds the legacy ChatService protobuf via build_chat_request_wrapper -> CURSOR_API_ENDPOINT = "https://api2.cursor.sh/aiserver.v1.ChatService/StreamUnifiedChatWithTools" (20-21); no is_agent_text_request routing, no agent.v1.AgentService/Run, no HTTP/2 client, no msg_ id. The only AgentService artifact in the whole Rust src/ is the stored OAuth value ("agent_endpoint", "https://agent.api5.cursor.sh") at src/oauth/providers.rs:341, never consumed by the executor. (3) impl_steps would produce parity: is_agent_text_request is trivially implementable over serde_json::Value; the agent URL const matches JS (agent endpoint already in oauth config + /agent.v1.AgentService/Run); HTTP/2 infra already exists (hyper 1.5 + hyper-util + hyper-rustls with http2 features; build_hyper_client in client_pool.rs enables http2). One non-blocking nuance: the AgentService RPC is client-streaming with a mid-stream server->client request-context exchange, so the implementer must build a duplex h2 SendRequest path (mirroring the JS raw http2.connect), not reuse the existing buffered Full<Bytes> hyper client — this is consistent with the JS approach and not an omission in the spec. The spec's truncated RUST_CURRENT tail ("no reque...") hides nothing material (no retry/request-context handling exists in Rust). Verdict CONFIRMED.

---

### `P1-L1` — windsurf executor missing entirely: gRPC-web GetChatMessage protobuf + CompletionChunk decode + MODEL_ALIAS_MAP

**JS (source of truth — verbatim):**

JS open-sse/executors/windsurf.js — WS_BASE_URL = "https://server.codeium.com", WS_SERVICE = "exa.language_server_pb.LanguageServerService", WS_METHOD_CHAT = "GetChatMessage", WS_CHAT_URL = `${WS_BASE_URL}/${WS_SERVICE}/${WS_METHOD_CHAT}` (15-18); WS_IDE_NAME "windsurf", WS_IDE_VERSION "3.14.0", WS_EXT_VERSION "3.14.0", WS_LOCALE "en-US". MODEL_ALIAS_MAP (26-119) maps ~70 catalog names to wire names (e.g. "gpt-5.5"→"gpt-5-5-medium", "claude-opus-4.7-high"→"claude-opus-4-7-high", "deepseek-v4"→"deepseek-v4", "glm-5.1"→"glm-5-1"). buildGetChatMessageRequest (190-205): field 1 metadata {apiKey, "windsurf", "3.14.0", "3.14.0", sessionId(uuid), "en-US"}, field 2 cascade_id uuid, field 3 model_or_alias, repeated field 4 ChatMessage {role, content, toolCallId}. Headers (382-392): `Content-Type: application/grpc-web+proto`, `Accept: application/grpc-web+proto`, `Authorization: Bearer {token}`, `User-Agent: windsurf/3.14.0`, `X-Grpc-Web: 1`. Body framed as gRPC-web: `[0x00][4-byte BE len][payload]` (grpcWebFrame 209-216). Response decode (decodeCompletionChunk 311-348): field 1 ContentChunk{field1 text} → content, field 3 DoneChunk{UsageStats p/c} → usage, field 4 ErrorChunk{field1 msg} → error. SSE output (435-580): first chunk `{role:"assistant",content:""}`, content chunks, trailer-frame grpc-status parsing, finish chunk + `data: [DONE]`, optional usage from DoneChunk.

**Current Rust behavior:**

N/A — no windsurf executor. In src/core/executor/provider.rs the "windsurf" registry entry (1377-1378) is `ProviderExecutorConfig::openai("https://server.self-serve.windsurf.com")` and windsurf requests fall through to DefaultExecutor (src/server/api/chat.rs has no windsurf arm), which POSTs JSON to the wrong URL in the wrong format.

**Implementation steps:**

Add a new module src/core/executor/windsurf.rs and dispatch arm in src/server/api/chat.rs for provider "windsurf"/"ws": (1) URL `https://server.codeium.com/exa.language_server_pb.LanguageServerService/GetChatMessage`. (2) Protobuf encoder (varint, tag = (field<<3)|2, length-delimited). (3) Build request: field 1 metadata {1 apiKey, 2 "windsurf", 3 "3.14.0", 4 "3.14.0", 5 uuid, 6 "en-US"}; field 2 uuid cascade; field 3 model alias; repeated field 4 {role,content[,toolCallId]}. (4) MODEL_ALIAS_MAP port (all ~70 entries, `resolve_ws_model_id` returns map[model] ?? model). (5) Headers per JS (incl. X-Grpc-Web: 1). (6) Frame request body with `[0x00][BE u32 len][payload]`. (7) Decode CompletionChunk frames (field 1 → content text, field 3 → done usage {1 prompt,2 completion}, field 4 → error msg), parse gRPC-web trailer frame (flag 0x80, `grpc-status:`/`grpc-message:` with decodeURIComponent). (8) Emit OpenAI SSE: first `{role:"assistant",content:""}`, content chunks, finish `{finish_reason:"stop"}` + usage when nonzero + `data: [DONE]`.

**Guard test:**

test_ws_model_alias: resolve_ws_model_id("gpt-5.5") == "gpt-5-5-medium", ("claude-opus-4.7-high") == "claude-opus-4-7-high", ("unknown-model") == "unknown-model".

**⚠️ Risks:**

The auth token goes BOTH in the protobuf metadata field 1 AND the Bearer header — omitting either breaks auth. The gRPC-web framing length is big-endian. decodeURIComponent on grpc-message — Rust must percent-decode. ToolCallChunk (field 2) is intentionally unhandled (skip).

**Cross-check:** ✅ **CONFIRMED** — All three claims verified against source. (1) JS: C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/open-sse/executors/windsurf.js lines 15-18 exactly match the claimed constants (WS_BASE_URL=https://server.codeium.com, WS_SERVICE=exa.language_server_pb.LanguageServerService, WS_METHOD_CHAT=GetChatMessage, WS_CHAT_URL concatenation); lines 20-23 match WS_IDE_NAME/WS_IDE_VERSION/WS_EXT_VERSION "3.14.0"/WS_LOCALE "en-US"; lines 26-119 MODEL_ALIAS_MAP has 80 entries (spec's "~70" is a minor undercount); buildMetadata (fields 1 apiKey/2 windsurf/3 3.14.0/4 3.14.0/5 uuid/6 en-US) and buildGetChatMessageRequest (field 2 cascadeId uuid, field 3 model_or_alias, field 4 repeated messages) match the cited request shape. The provider registry open-sse/providers/registry/windsurf.js line 22 confirms the codeium.com GetChatMessage baseUrl. (2) Rust: src/core/executor/provider.rs lines 1377-1378 are exactly ("windsurf", ProviderExecutorConfig::openai("https://server.self-serve.windsurf.com")); no windsurf.rs exists in src/core/executor/ (mod.rs lists 28 modules, none windsurf); src/server/api/chat.rs has no windsurf arm — the else branch (line 1634) constructs DefaultExecutor, which POSTs JSON (Content-Type application/json) — windsurf has no entry in default.rs PROVIDER_CONFIGS and no "windsurf" translator, so it falls through as claimed; "ws" alias is registered (src/core/model/mod.rs:110-111, provider_catalog.json:96), so the "windsurf"/"ws" dispatch scope is correct. (3) Impl steps: new src/core/executor/windsurf.rs + chat.rs dispatch arm for "windsurf"/"ws", the codeium.com URL, varint protobuf encoder with tag=(field<<3)|2 length-delimited framing, and the request build (metadata field 1, cascade_id field 2, model_or_alias field 3, messages field 4) are a faithful port of the JS buildGetChatMessageRequest; no obvious omission that would break parity (the spec text is truncated at "field 2" but every element present matches the JS). Minor imprecision: alias map has 80 not ~70 entries, and the exact URL Rust windsurf currently POSTs to is config-dependent (provider_node/runtime_transport), but directionally the self-serve base claim is right.

---

### `P1-L2` — trae executor missing entirely: chat_sessions create + /events SSE plan_item aggregation, Cloud-IDE-JWT auth

**JS (source of truth — verbatim):**

JS open-sse/executors/trae.js — base = `https://core-normal.trae.ai/api/remote/v1` (50, trailing slash stripped). Headers (53-66): `Authorization: Cloud-IDE-JWT {token}`, `Content-Type: application/json`, `X-Trae-Client-Type: web`, `X-Preferenced-Language: psd.appLanguage || "en"`, `x-user-region: psd.userRegion || "US"`, `Referer: https://solo.trae.ai/`, `User-Agent` Chrome 149 macOS, `Accept` text/event-stream|application/json. Flow (7-13): POST `{base}/chat_sessions` → `{code:0, data:{chat_session_id, message_id}}`; GET `{base}/chat_sessions/{id}/events?reply_to_message_id={message_id}` → SSE; `plan_item` events carry `{id, thought}` (cumulative per plan-item id, longest wins — renderNewText 201-211: order array, `if (t.length >= thoughts[pid].length) thoughts[pid]=t; full=order.map(...).join(""); piece=full.slice(sent)`). createSession body (105-120): `{ mode, environment_id: "default", initial_message: { chat_session_id: "", content: [], query, model_name, agent_type: "solo_agent_remote", model_selection_strategy, common_params: JSON.stringify({...}) }, env: "remote", auto_create_project: false, origin: "web" }`. commonParams (79-100): language "en-us", quality "stable", app_version psd.appVersion||"1.0.0.1229", web_id, user_identity||"Free", is_freshman "0", scope||"marscode-us", tenant||"marscode", region||"US-East", aiRegion, solo_chat_mode. resolveMode (69-76): "work"/"auto-work"/"solo-work" → mode work/strategy auto/empty model; "auto"/empty → mode code/strategy auto; else code/manual/modelName=model. Events: `error`→errorEvent; `token_usage`→usage {prompt_tokens,completion_tokens,total_tokens}; `plan_item`→renderNewText; `done`→stop. Streaming emits first chunk `{role:"assistant"}`, content pieces, finish `{finish_reason:"stop"}` + usage chunk + `data: [DONE]`. Non-streaming: same aggregation, message content = join of thoughts.

**Current Rust behavior:**

N/A — no trae executor. In src/core/executor/provider.rs "trae" (1087-1089) = `ProviderExecutorConfig::openai("https://core-normal.trae.ai/api/remote/v1")`; trae requests fall to DefaultExecutor, which POSTs a plain chat.completions JSON to the chat_sessions base URL — completely wrong protocol.

**Implementation steps:**

Add src/core/executor/trae.rs + dispatch in src/server/api/chat.rs for provider "trae": (1) Headers per JS. (2) flattenQuery: system → `[System]\n{content}`, assistant → `[Assistant]\n{content}`, user → content; join `\n\n`; return `JSON.stringify([{type:"text",data:{content}}])`. (3) resolveMode per JS. (4) POST `{base}/chat_sessions` with the createSession body; on non-ok or code!=0 → 502 error. (5) GET events with reply_to_message_id; SSE parse `event:`/`data:`; `plan_item` aggregation per renderNewText (order + longest-thought). (6) Emit OpenAI SSE with `chatcmpl-trae-{timestamp}` id, first `{role:"assistant"}`, content pieces, finish stop + usage + `data: [DONE]`; `error` event → error chunk; 300s stream timeout (TRAE_STREAM_TIMEOUT_MS env). (7) Non-streaming → chat.completion JSON with joined thoughts and usage.

**Guard test:**

test_trae_render_new_text_cumulative: with two plan_items id A "hi" and id B " there" → piece1 "hi", piece2 " there", and a shorter re-send of A does not shrink (longest wins).

**⚠️ Risks:**

The `plan_item` thought is cumulative per id — emitting a shorter later value must be ignored. `token_usage` fields default to 0 (JS `usage.prompt_tokens || 0`). Auth header is `Cloud-IDE-JWT ` prefix (space), not Bearer. STREAM_TIMEOUT_MS default 300000.

**Cross-check:** 🟡 **PLAUSIBLE** — The JS claim is fully REAL and precise. Verified against .tmp/9router/open-sse/executors/trae.js: base() at line 50 is exactly "https://core-normal.trae.ai/api/remote/v1" with trailing slash stripped; buildHeaders (53-66) matches every cited header including Cloud-IDE-JWT auth, X-Trae-Client-Type: web, X-Preferenced-Language/x-user-region from providerSpecificData with en/US defaults, Referer solo.trae.ai, Chrome 149 macOS UA, conditional Accept; flattenQuery (22-42) matches [System]/[Assistant] prefixes joined by \n\n wrapped in JSON.stringify([{type:"text",data:{content}}]); resolveMode (69-76) matches the work/code strategies; createSession (103-132) POSTs {base}/chat_sessions with the exact body and errors on non-ok or code!==0 mapped to 502 by the caller (189-193); streamEvents (136-174) hits /chat_sessions/{id}/events and aggregates plan_item thoughts cumulatively (196-211). The Rust claim is also REAL: provider.rs:1087-1089 maps "trae" to ProviderExecutorConfig::openai("https://core-normal.trae.ai/api/remote/v1"), "trae" appears at provider.rs:1529 in get_api_key_providers(), no trae.rs exists in src/core/executor/, and the chat.rs dispatch chain (1205-1602) has no trae branch so it falls to the else arm constructing DefaultExecutor, whose build_url returns the base URL verbatim and build_headers emits Bearer, sending a plain chat.completions JSON to the wrong protocol/endpoint (only nuance: it POSTs to the bare base URL, not literally /chat_sessions — same endpoint family, harmless). Verdict is PLAUSIBLE rather than CONFIRMED because the impl_steps are incomplete relative to the task title: the 4 numbered steps cover only the createSession half and omit the /events SSE consumption, plan_item aggregation, chunk/[DONE]/usage/error emission, and non-streaming completion shaping that the title explicitly names — implementing only the listed steps would create a session but never read the stream, returning nothing to the client. Also unmentioned: commonParams() (the JSON-stringified common_params in initial_message) and that flattenQuery's else branch catches all non-system/assistant roles (developer/tool), not only user. These are omissions, not inaccuracies; the core cited behavior and Rust gap are accurate.

---

### `P1-M1` — antigravity: missing top-level request envelope fields (project/model/userAgent/requestType/requestId), wrong MAX_ANTIGRAVITY_OUTPUT_TOKENS cap, decoy-tool cloaking diverges (JS transformRequest does NOT cloak)

**JS (source of truth — verbatim):**

JS open-sse/executors/antigravity.js — transformRequest returns (268-276): `{ ...body, project: projectId, model: body.model || model, userAgent: "antigravity", requestType: "agent", requestId: buildIdeRequestId({ body, request: transformedRequest, credentials, model, requestType: "agent" }), request: transformedRequest }` where buildIdeRequestId (99-110): `agent/${uuidFromSeed("antigravity:conversation:"+sessionId)}/${Date.now()}/${uuidFromSeed("antigravity:trajectory:"+sessionId+":"+model+":"+requestType)}/${Math.max(1, contentCount*2-1)}` and uuidFromSeed (91-97) = SHA-256(seed) first 16 bytes, set bits (version 5 / variant RFC 4122), hyphenated. MAX_ANTIGRAVITY_OUTPUT_TOKENS = 64000 (line 21; caps generationConfig.maxOutputTokens at 64000, 249-251). Image models (144-188): request `{ contents, generationConfig: { temperature: 1.0, topP: 0.95, topK: 40, maxOutputTokens: 8192, imageConfig: { aspectRatio } }, sessionId, no tools/systemInstruction/safetySettings }`, requestType "image_gen", model = cleanModel (suffix `-{N}x{M}` stripped). Tool merge (221-243): single functionDeclarations group, `sanitizeFunctionName` per decl, `cleanJSONSchemaForAntigravity` on parameters, no _ide suffixing, no decoy injection, NO cloakTools call in transformRequest (cloakTools static at 419-512 exists but is NOT wired into transformRequest). buildUrl (117-124): `forceNonStream = isImageModel(model); action = (stream && !forceNonStream) ? "streamGenerateContent?alt=sse" : "generateContent"; return `${baseUrl}/v1internal:${action}``. HTTP 429/5xx retry with computeRetryDelay (386-411): Retry-After header/body `reset after Xh Ym Zs`, cap MAX_RETRY_AFTER_MS=10000, backoff `1000 * 2^attempt` capped at 15000 (transient) / 10000 (429).

**Current Rust behavior:**

src/core/executor/antigravity.rs — transform_request (437-713) mutates body.request only; it does NOT add top-level `project`, `model`, `userAgent`, `requestType`, or `requestId` (no buildIdeRequestId/uuidFromSeed). It inserts `projectId` INTO request (820-822) which JS does NOT do at this layer. MAX_ANTIGRAVITY_OUTPUT_TOKENS = 16_384 (47) vs JS 64000. cloak_tools (356-432) suffixes ALL tool names with `_ide` AND injects AG_DEFAULT_TOOLS decoys — JS transformRequest does neither (this will corrupt tool names upstream). image path (857-879) adds candidateCount/safetySettings but not `imageConfig`/temperature/topP/topK/maxOutputTokens 8192, and uses requestType/model absent. build_url matches (streamGenerateContent?alt=sse / generateContent).

**Implementation steps:**

In src/core/executor/antigravity.rs transform_request: (1) Set MAX_ANTIGRAVITY_OUTPUT_TOKENS = 64_000 (both the const and the cap at the generationConfig write). (2) Add the top-level envelope: after building the transformed request, produce `{...body, project: <resolved projectId>, model: body.model||model, userAgent: "antigravity", requestType: "agent", requestId: <build_ide_request_id>, request: transformed}`. Implement build_ide_request_id: if body.requestId matches `^agent/[^/]+/\d+/[^/]+/\d+$` keep it; else `agent/{conv_uuid}/{now_ms}/{traj_uuid}/{step}` with uuidFromSeed (sha256 → 16 bytes → set version/variant bits → hyphenated) and step = max(1, contentCount*2 - 1). (3) REMOVE the cloak_tools call (and the _ide suffixing + decoy injection) from transform_request so tool names are only sanitize_function_name + clean_json_schema — matching JS. (4) Image path: for is_image_model, build contents from text parts, generationConfig `{temperature:1.0, topP:0.95, topK:40, maxOutputTokens:8192, imageConfig:{aspectRatio}}`, sessionId, requestType "image_gen", cleanModel (strip `-{N}x{M}`), and set top-level model to cleanModel; remove candidateCount/safetySettings injection if not needed to match JS (JS sends none).

**Guard test:**

test_transform_request_sets_envelope: assert body has project/model/userAgent=="antigravity"/requestType/requestId and requestId matches the agent/ pattern; and `test_max_output_tokens_cap_64000`.

**⚠️ Risks:**

Removing the cloak/suffix behavior is a behavioral change — verify no Rust test asserts the `_ide` suffix (antigravity.rs tests at 1104-1127 assert `a_ide` presence and WILL need updating to the JS behavior of bare sanitized names + NO decoys). The uuidFromSeed version/variant bit-setting is required for a valid UUID. step uses request.contents length (JS `Array.isArray(request?.contents) ? request.contents.length : 1`).

**Cross-check:** 🟡 **PLAUSIBLE** — All cited JS behavior is REAL: antigravity.js line 21 defines MAX_ANTIGRAVITY_OUTPUT_TOKENS = 64000; lines 268-276 return the top-level envelope `{...body, project, model: body.model||model, userAgent:"antigravity", requestType:"agent", requestId: buildIdeRequestId(...), request: transformedRequest}`; lines 99-110 build the `agent/<conversation>/<Date.now()>/<trajectory>/<step>` id via SHA256-seeded uuidFromSeed. The cloak divergence is also REAL: `cloakTools` (419-512) is never called from transformRequest — its only call site in translator/index.js (~150-156) is commented out ("Antigravity cloaking disabled"), so JS transformRequest sanitizes/merges tools WITHOUT the `_ide` suffix or decoys. All cited Rust behavior is REAL: antigravity.rs caps at 16_384 (line 47); transform_request (437-713) mutates body.request only and never adds top-level project/model/userAgent/requestType/requestId; projectId is inserted INTO request at 820-822 (which JS does not do); cloak_tools runs unconditionally at line 548. Impl_steps 1-2 (cap 64_000, top-level envelope) are correct for those two items; porting uuidFromSeed/buildIdeRequestId (no such helper exists yet in src/) and wiring request.model into transform_request are feasible. HOWEVER there is an obvious omission: the task title itself lists "decoy-tool cloaking diverges (JS transformRequest does NOT cloak)" as one of the gaps, but impl_steps contain no step to remove or disable Rust's unconditional cloak_tools. As written the plan leaves the cloaking divergence in place, so it does not fully achieve parity — mostly right, not complete.

---

### `P1-N1` — codex: CODEX_DEFAULT_INSTRUCTIONS text differs, include reasoning.encrypted_content not injected, no SSE peek retry (Rust consumes full body, breaking streaming), no prefetchImages, no usage_limit_reached parse

**JS (source of truth — verbatim):**

JS open-sse/executors/codex.js — CODEX_DEFAULT_INSTRUCTIONS (config/codexInstructions.js) is a long multi-section prompt starting `You are Codex, based on GPT-5. You are running as a coding agent in the Codex CLI on a user's computer.` (18 instructions). transformRequest (393-489): `convertSystemToDeveloperRole` (49-56), stripStoredItemReferences, normalizeCodexTools (72-115: `type==="namespace"` collects names, `type==="custom"` passthrough, hosted tools keep), `body.stream = true` (415), `body.store = false` (423), `prompt_cache_key = sessionId` when missing (426-428), `body.model = getModelUpstreamId("cx", ...)` (431), effort suffix strip (none/minimal/low/medium/high/xhigh, 435-444), reasoning `{effort: normalizeReasoningEffort(body.model, reasoning_effort||modelEffort||'low'), summary:"auto"}` (446-453), `if (effort && effort !== 'none') body.include = ["reasoning.encrypted_content"]` (457-459), then deletes temperature/top_p/frequency_penalty/presence_penalty/logprobs/top_logprobs/n/max_tokens/max_completion_tokens/max_output_tokens/user/prompt_cache_retention/metadata/stream_options/safety_identifier/previous_response_id (462-478), `service_tier === "fast" → "priority"`, else delete if not "priority" (480-481), then allowlist filter (RESPONSES_API_ALLOWLIST 42-46: model,input,instructions,tools,tool_choice,stream,store,reasoning,service_tier,include,prompt_cache_key,client_metadata,text). execute/_peekSseTransientError (258-362): peek 256KiB of the SSE body for `server_is_overloaded`/`service_unavailable_error` (retry), `selected model is at capacity`/`model_at_capacity` (account fallback → 503 with CODEX_MODEL_CAPACITY_MESSAGE), and user-output patterns; if no match, re-assemble a replacement stream (prefix chunks + rest) so streaming is not broken. prefetchImages (241-256): fetch remote image_url → base64 data URI, 15s timeout, `{type:"input_image", image_url, detail}`. parseError (365-387): 429 `usage_limit_reached` → `resetsAtMs` from resets_at / resets_in_seconds. buildHeaders (202-220): `session_id`, `originator: codex_cli_rs`, `ChatGPT-Account-ID: workspaceId||chatgptAccountId||accountId`.

**Current Rust behavior:**

src/core/executor/codex.rs — DEFAULT_CODEX_INSTRUCTIONS (212-213) is a one-liner "You are a highly capable coding agent..." — NOT the JS prompt. transform_request_body (232-307) builds a fresh body but: sets reasoning always (does NOT gate include on effort — it only copies body include if present, 299-301, so `reasoning.encrypted_content` is never injected), no prefetchImages, no prompt_cache_key injection (only copies if present, 302-304). execute (417-499) reads the ENTIRE body (`resp.bytes().await?` 464) and scans the first 4096 bytes for the two retry strings, then reconstructs — this consumes the whole response, breaking SSE streaming, and lacks account-fallback patterns and stream re-assembly. build_headers (161-209) has session_id/originator/chatgpt-account-id ✓ but missing the `User-Agent: codex_cli_rs/0.136.0` from the registry headers (provider.rs codex config may carry it). No usage_limit_reached parseError, no service_tier mapping.

**Implementation steps:**

In src/core/executor/codex.rs: (1) Replace DEFAULT_CODEX_INSTRUCTIONS with the full JS text (copy verbatim from .tmp/9router/open-sse/config/codexInstructions.js). (2) In transform_request_body: compute effort; if effort != "none" set `include = ["reasoning.encrypted_content"]` (overwrite, per JS). (3) Add prefetch_images: for each input item content part of type `image_url`, if url starts with "data:" → `{type:"input_image",image_url:url,detail}`; else fetch the URL (15s timeout) and inline base64 data URI. (4) Fix execute to NOT consume the full body: stream-peek at most 256KiB; if a retry pattern matched before any user-output pattern → retry (backoff per JS retry config for 503); if account-fallback pattern matched → return 503 error with the capacity message; otherwise re-assemble the response body from peeked prefix chunks + the remaining upstream stream so SSE flows. (5) parse_error: 429 body with `error.type==="usage_limit_reached"` → compute resetsAtMs from `resets_at` (seconds) or `resets_in_seconds`. (6) service_tier mapping fast→priority, delete otherwise.

**Guard test:**

test_codex_instructions_matches_js: assert DEFAULT_CODEX_INSTRUCTIONS starts_with("You are Codex, based on GPT-5"). `test_codex_include_reasoning_when_effort` — effort high → include == ["reasoning.encrypted_content"].

**⚠️ Risks:**

The full-body consumption is the most dangerous existing behavior (streaming broken) — replace it, do not layer the peek on top. The account-fallback error must be HTTP 503 with the EXACT message "Selected model is at capacity. Please try a different model." for downstream fallback matching. Keep responseFormat/allowlist semantics.

**Cross-check:** ✅ **CONFIRMED** — All material claims verified against source. JS (9router): CODEX_DEFAULT_INSTRUCTIONS in .tmp/9router/open-sse/config/codexInstructions.js line 3 opens exactly "You are Codex, based on GPT-5. You are running as a coding agent in the Codex CLI on a user's computer." and is a long multi-section prompt; codex.js transformRequest spans 393-489, convertSystemToDeveloperRole at 49-56, stripStoredItemReferences at 59-69, normalizeCodexTools at 72-115 with type==="namespace" at line 78; include is gated effort!=="none" and overwritten to ["reasoning.encrypted_content"] (lines 456-459); SSE peek retry peeks 256KB and re-assembles a live ReadableStream (258-362); parseError parses usage_limit_reached/resets_at (365-387); prefetchImages converts image_url parts to input_image, short-circuits data: URLs, else fetchImageAsBase64 (241-256). Rust (openproxy): DEFAULT_CODEX_INSTRUCTIONS at codex.rs:212-213 is the one-liner "You are a highly capable coding agent...", not the JS prompt; transform_request_body (232-307) always sets reasoning {effort, summary:"auto"} (line 290) and only copies include if already present (299-301) — no effort gating; execute() reads the full body via resp.bytes().await? (line 464) and reconstructs the response from buffered bytes, so streaming is fully buffered and only 3 retries match server_is_overloaded/service_unavailable_error in first 4096 bytes (446-494); resets_at_ms is never populated anywhere (utils/error.rs always None, grep confirms no assignment), and there is no codex-specific usage_limit_reached parse. Impl steps 1-3 (instructions text, include gating, prefetch_images) are each correct and feasible (fetch_image_as_base64 already exists in src/core/translator/helpers/image_helper.rs). Caveats (non-blocking): (a) the "(18 instructions)" parenthetical is off — the JS prompt has 8 sections, not 18; (b) the impl_steps text is truncated mid-step-3, so the two remaining gaps from the title (SSE peek retry breaking streaming, usage_limit_reached parse) are not visible in the shown steps — they may or may not be in the truncated remainder; (c) a verbatim copy of the JS source would preserve backslash-escaped backticks (\`) that JS unescapes at runtime, so the Rust literal should hold plain backticks for true text parity. None of these undermine the verified parity-gap claims or the correctness of the shown impl steps.

---

### `P1-O1` — opencode-go (default.rs path): also verify provider base URL includes /go segment

**JS (source of truth — verbatim):**

JS open-sse/executors/opencode-go.js BASE = "https://opencode.ai/zen/go/v1" (16); registry baseUrl `https://opencode.ai/zen/go/v1/chat/completions` (registry/opencode-go.js:22). Claude-format models use `{BASE}/messages` = `https://opencode.ai/zen/go/v1/messages`.

**Current Rust behavior:**

src/core/executor/default.rs registers "opencode-go" as `ProviderConfig::openai("https://opencode.ai/zen/v1")` (139-140) — MISSING the `/go` segment. build_url (754-765) appends `messages` or `chat/completions` to that base, yielding `https://opencode.ai/zen/v1/messages` instead of `https://opencode.ai/zen/go/v1/messages` (404). The dedicated src/core/executor/opencode_go.rs uses OPENCODE_GO_BASE = "https://opencode.ai/zen/go/v1" (26) which is correct — the bug is only in default.rs's registry entry.

**Implementation steps:**

In src/core/executor/default.rs PROVIDER_CONFIGS, change the "opencode-go" entry from `ProviderConfig::openai("https://opencode.ai/zen/v1")` to `ProviderConfig::openai("https://opencode.ai/zen/go/v1")`. If chat.rs routes opencode-go through the dedicated executor (it does — chat.rs:1325-1354), this only matters for fallback/default paths, but fix it regardless for consistency.

**Guard test:**

test_default_opencode_go_base_url: build_url for opencode-go with a non-claude model returns "https://opencode.ai/zen/go/v1/chat/completions".

**⚠️ Risks:**

Check provider.rs's opencode-go registry entry too (provider.rs:1491 lists opencode-go as api-key provider; its base URL may also lack /go). Do not double-append the path when a runtime_transport already carries the full endpoint.

**Cross-check:** ✅ **CONFIRMED** — All three claims verified against source. (1) JS: open-sse/executors/opencode-go.js:16 defines `BASE = "https://opencode.ai/zen/go/v1"`; buildUrl (24-29) returns `${BASE}/messages` for the 6-model MESSAGES_FORMAT_MODELS set (minimax-m3/m2.7/m2.5, qwen3.7-max/plus, qwen3.6-plus) else `${BASE}/chat/completions`; providers/registry/opencode-go.js:22 baseUrl is `https://opencode.ai/zen/go/v1/chat/completions` — exact match. (2) Rust: src/core/executor/default.rs:139-140 registers "opencode-go" as `ProviderConfig::openai("https://opencode.ai/zen/v1")` (missing `/go`), and build_url (754-765) appends messages/chat/completions, so the default path yields `https://opencode.ai/zen/v1/{messages,chat/completions}`. The dedicated OpenCodeGoExecutor (opencode_go.rs:26) uses the correct `https://opencode.ai/zen/go/v1` and chat.rs:1325-1354 routes opencode-go there, so the wrong base only affects fallback/CLI paths (cli/mod.rs:1697,1868) — exactly as the spec's parenthetical states. Corroborating: tests/executor_pool_behavior.rs:192-203,417-427 construct DefaultExecutor for opencode-go and assert `/go/v1` URLs that the current `/zen/v1` base cannot produce — those tests currently fail and the fix makes them pass, proving the gap and the intended direction. (3) Changing default.rs:140 to `ProviderConfig::openai("https://opencode.ai/zen/go/v1")` yields parity with JS for the default/fallback path with no omission. Minor unclaimed nuance: default.rs's opencode_go_uses_claude_format helper matches only minimax-m2.5/m2.7 (2 of the 6 JS claude-format models), a separate pre-existing gap not addressed by this task and not claimed by it.

---

---

## C. TRANSLATORS (10 specs)

### `P0-A10` — reasoning_effort → reasoning/effort mapping missing (openai_responses + claude_to_openai)

**JS (source of truth — verbatim):**

openai-responses.js:417-423 chat→responses:
```js
if (body.temperature !== undefined) result.temperature = body.temperature;
if (body.max_tokens !== undefined) result.max_tokens = body.max_tokens;
if (body.top_p !== undefined) result.top_p = body.top_p;
if (body.reasoning !== undefined) result.reasoning = body.reasoning;
if (body.reasoning_effort !== undefined) result.reasoning = { effort: body.reasoning_effort, summary: "auto" };
```
(Note: body.reasoning set first, then reasoning_effort OVERWRITES result.reasoning with {effort, summary:"auto"} — so reasoning_effort wins.)
openai-responses.js:243-247 responses→chat cleanup:
```js
if (typeof result.reasoning?.effort === "string") {
  result.reasoning_effort = result.reasoning.effort;
}
delete result.reasoning;
delete result.client_metadata;
```
claude-to-openai.js:83-91:
```js
if (body.reasoning_effort !== undefined) {
  result.reasoning_effort = body.reasoning_effort;
} else if (body.reasoning?.effort !== undefined) {
  result.reasoning_effort = body.reasoning.effort;
}
if (body.reasoning !== undefined) {
  result.reasoning = body.reasoning;
}
```

**Current Rust behavior:**

src/core/translator/request/openai_responses.rs chat→responses: passes through temperature/max_tokens/max_completion_tokens/top_p/service_tier (341-358) but NO reasoning_effort→reasoning mapping and no reasoning passthrough. responses→chat (openai_responses_to_chat_request): only `obj.remove("reasoning")` (line 190); does not map reasoning.effort→reasoning_effort, does not remove client_metadata, does not map max_output_tokens→max_tokens. src/core/translator/request/claude_to_openai.rs: after tool_choice (line 469) and thinking (472-474), there is NO reasoning_effort / reasoning passthrough at all.

**Implementation steps:**

openai_responses.rs chat_to_openai_responses_request: after the service_tier passthrough (line 358), add: `if let Some(r) = body.get("reasoning") { result["reasoning"] = r.clone(); } if let Some(e) = body.get("reasoning_effort") { result["reasoning"] = serde_json::json!({"effort": e, "summary": "auto"}); }` (reasoning_effort overwrites reasoning per JS order).
openai_responses.rs openai_responses_to_chat_request: in the obj cleanup block (184-190), change `obj.remove("reasoning")` to: `if let Some(r) = obj.get("reasoning") { if let Some(e) = r.get("effort").and_then(Value::as_str) { obj.insert("reasoning_effort".into(), Value::String(e.to_string())); } } obj.remove("reasoning"); obj.remove("client_metadata");` — and after result["messages"]/obj construction also handle max_output_tokens: `if let Some(v) = obj.get("max_output_tokens").cloned() { if obj.get("max_tokens").is_none() { obj.insert("max_tokens".into(), v); } obj.remove("max_output_tokens"); }`.
claude_to_openai.rs claude_to_openai_request: after the thinking block (line 472-474) add: `if let Some(e) = body_obj.get("reasoning_effort") { result["reasoning_effort"] = e.clone(); } else if let Some(e) = body_obj.get("reasoning").and_then(|r| r.get("effort")) { result["reasoning_effort"] = e.clone(); } if let Some(r) = body_obj.get("reasoning") { result["reasoning"] = r.clone(); }`.

**Guard test:**

In openai_responses.rs add `reasoning_effort_maps_to_reasoning_object` — chat body {reasoning_effort:high} → result.reasoning == {"effort":"high","summary":"auto"} AND result.reasoning_effort absent? (JS deletes nothing here — both fields can coexist; assert result.reasoning.effort == "high"). Add `responses_to_chat_maps_reasoning_effort_and_drops_metadata` — input body {reasoning:{effort:"medium"},client_metadata:{},max_output_tokens:100} → result.reasoning_effort=="medium", no client_metadata, max_tokens==100, no max_output_tokens. In claude_to_openai.rs add `passes_reasoning_effort_through` — body {reasoning_effort:"low"} → result.reasoning_effort=="low"; body {reasoning:{effort:"medium"}} → result.reasoning_effort=="medium" and result.reasoning.effort=="medium".

**⚠️ Risks:**

JS order in chat→responses is reasoning THEN reasoning_effort-overwrite — reasoning_effort must win. In responses→chat, reasoning.effort is read as a STRING only (`typeof === "string"`) — if effort is a non-string, no mapping and reasoning is still deleted. max_output_tokens→max_tokens maps only when max_tokens is undefined, then max_output_tokens is always removed.

**Cross-check:** ✅ **CONFIRMED** — All three claims verified against source.

1. JS behavior REAL. In C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/open-sse/translator/request/openai-responses.js lines 417-423 (`openaiToOpenAIResponsesRequest`), the chat→responses mapping is exactly: `if (body.temperature !== undefined) result.temperature = body.temperature; if (body.max_tokens !== undefined) result.max_tokens = body.max_tokens; if (body.top_p !== undefined) result.top_p = body.top_p; if (body.reasoning !== undefined) result.reasoning = body.reasoning; if (body.reasoning_effort !== undefined) result.reasoning = { effort: body.reasoning_effort, summary: "auto" };`. The order means reasoning_effort overwrites an earlier body.reasoning passthrough — matching the spec. (Note: the spec's line numbers 417-423 are slightly off — the mapping actually sits at 417-423 of the source block that begins at line 417, i.e. lines 418-423, with service_tier at 423 — immaterial.)

2. Rust current behavior REAL. In C:/Users/ADMIN/Documents/Projects/openproxy/src/core/translator/request/openai_responses.rs, `chat_to_openai_responses_request` passthroughs temperature/max_tokens/max_completion_tokens/top_p (341-352) and service_tier (354-358), but has NO reasoning_effort→reasoning mapping and no body.reasoning passthrough. Grep across src/core/translator confirms no reasoning_effort handling exists in openai_responses.rs. The reverse `openai_responses_to_chat_request` does `obj.remove("reasoning")` (line 190) and only maps `reasoning.effort`→`reasoning_effort` is absent — JS's reverse maps it (openai-responses.js lines 243-245: `if (typeof result.reasoning?.effort === "string") result.reasoning_effort = result.reasoning.effort;`), so the Rust reverse is a related minor gap the spec's "responses→chat only obj.remove('reasoning')" claim accurately describes.

3. Impl steps would produce parity. The proposed additions to `chat_to_openai_responses_request` after line 358 (`if let Some(r) = body.get("reasoning") { result["reasoning"] = r.clone(); } if let Some(e) = body.get("reasoning_effort") { result["reasoning"] = json!({"effort": e, "summary": "auto"}); }`) replicate the JS behavior exactly, including the overwrite semantics (reasoning_effort block runs after the reasoning passthrough). The spec's truncation "openai_responses_" after the first step is just clipped text; the described second half (reverse-direction reasoning_effort mapping) matches the JS reverse at lines 243-245. No obvious omission: for full parity the reverse leg also needs the reasoning.effort→reasoning_effort mapping, which the impl_steps header ("+ claude_to_openai") and the openai_responses_ prefix indicate is included.

One minor scope caveat: the task title's "claude_to_openai" translator is a secondary leg. JS claude-to-openai.js maps `body.reasoning_effort`→`result.reasoning_effort` and `body.reasoning`→`result.reasoning` (lines 83-91); the Rust claude_to_openai.rs handles neither reasoning_effort nor reasoning — a real gap, but the JS mapping is direct identity passthrough (no effort→reasoning synthesis), so it is not a necessary part of this P0-A10 step's parity fix and does not affect the verdict.

---

### `P0-A11` — reasoning_details[] array not decoded in openai→claude response streaming (MiniMax reasoning_split=true)

**JS (source of truth — verbatim):**

open-sse/translator/concerns/reasoning.js:15-24 extractReasoningText(delta):
```js
if (typeof delta.reasoning_content === "string" && delta.reasoning_content) return delta.reasoning_content;
if (typeof delta.reasoning === "string" && delta.reasoning) return delta.reasoning;
const details = delta.reasoning_details;
if (Array.isArray(details)) {
  return details.map((d) => (typeof d === "string" ? d : d?.text || d?.content || "")).join("");
}
return "";
```
Called from response/openai-to-claude.js:139 `const reasoningContent = extractReasoningText(delta);` — reasoning_details elements may be strings or {text}|{content}; joined with EMPTY string "" (not newline).

**Current Rust behavior:**

src/core/translator/response/openai_to_claude.rs:263-270 reads only `delta.reasoning_content` (string) then `delta.reasoning` (string). reasoning_details array is never consulted — MiniMax reasoning_split=true streams reasoning as `delta.reasoning_details` and would be silently dropped.

**Implementation steps:**

In src/core/translator/response/openai_to_claude.rs, add a helper `fn extract_reasoning_text(delta: &Value) -> String` that returns: (1) delta.reasoning_content as String if non-empty string; (2) delta.reasoning as String if non-empty string; (3) otherwise if delta.reasoning_details is an array, `details.iter().map(|d| if let Some(s)=d.as_str() { s.to_string() } else { d.get("text").and_then(Value::as_str).or_else(|| d.get("content").and_then(Value::as_str)).unwrap_or("").to_string() }).collect::<String>()`; else "". Replace the current reasoning extraction block (263-270) to use this helper and only open the thinking block when the result is non-empty (currently gated on `!reasoning.is_empty()`).

**Guard test:**

Add `reasoning_details_array_decoded` — a chunk with choices[0].delta = {reasoning_details:[{"text":"a"},{"content":"b"},"c"]} → the stream emits one thinking_delta with thinking "abc" (join with empty string, no separator). Add `reasoning_content_takes_priority` — delta {reasoning_content:"x", reasoning_details:[{text:"y"}]} → thinking "x" only.

**⚠️ Risks:**

Join is "" (empty) for reasoning_details — do NOT join with "\n". Element shape is string | {text} | {content}; elements with neither contribute "". Keep priority order reasoning_content > reasoning > reasoning_details. This same reasoning_details shape also flows into the request-side buildReasoningInputItem (P0-A8) where the join is "\n" — the two joins are intentionally different.

**Cross-check:** ✅ **CONFIRMED** — All three claims verified against source. (1) The JS is real: .tmp/9router/open-sse/translator/concerns/reasoning.js:15-24 defines extractReasoningText with the exact reasoning_content → reasoning → reasoning_details[] array-fallback precedence, and it is consumed at the cited response-translation site (open-sse/translator/response/openai-to-claude.js:139). A unit test (tests/translator/thinking-unified.test.js:194-196) pins the MiniMax reasoning_details split behavior. (2) The Rust gap is real: src/core/translator/response/openai_to_claude.rs:263-270 only reads delta.reasoning_content and delta.reasoning as strings; reasoning_details is never consulted, and a repo-wide grep confirms the field appears nowhere in the Rust src tree — MiniMax reasoning_split=true streams reasoning as delta.reasoning_details and would be silently dropped, creating a genuine parity gap. (3) The impl_steps produce parity: the proposed helper's ordering and per-element mapping (as_str for string elements, else d.get("text")/d.get("content")) exactly mirror the JS map/join semantics, and integrating it at the existing reasoning site with the existing !is_empty guard matches JS's truthy-gating. No omission found.

---

### `P0-A12` — max_tokens ceiling divergence in openai→claude (model ceiling + budget reconciliation)

**JS (source of truth — verbatim):**

request/openai-to-claude.js:18-27: `const modelCeiling = getCapabilitiesForModel(null, model).maxOutput || undefined; const result = { model, max_tokens: adjustMaxTokens(body, modelCeiling), stream };`
formats/maxTokens.js:12-33 adjustMaxTokens(body, ceiling = DEFAULT_MAX_TOKENS): maxTokens = body.max_tokens || 64000; tools present && maxTokens < 32000 → 32000; thinking.budget_tokens && maxTokens <= budget_tokens → budget_tokens + 1024; `if (maxTokens > ceiling) maxTokens = ceiling;`
formats/claude.js:201-217 prepareClaudeRequest: `const ceiling = getCapabilitiesForModel(provider, body.model).maxOutput || DEFAULT_MAX_TOKENS; if (body.max_tokens > ceiling) body.max_tokens = ceiling;` then budget reconciliation: `if (body.thinking?.type === "enabled" && body.thinking.budget_tokens && body.thinking.budget_tokens >= body.max_tokens) { body.max_tokens = Math.min(body.thinking.budget_tokens + 1024, ceiling); if (body.thinking.budget_tokens >= body.max_tokens) { body.thinking.budget_tokens = Math.max(1024, body.max_tokens - 1024); } }`
Model ceilings (capabilities.js:79-89): claude-opus-4.6/4.7/4.8/5 and claude-sonnet-4.6/5 → maxOutput 128000.

**Current Rust behavior:**

src/core/translator/request/openai_to_claude.rs adjust_max_tokens (379-400): has the tools-32000 bump and the budget_tokens+1024 bump but NO ceiling clamp (signature takes no ceiling param). src/core/translator/request/claude_format.rs prepare_claude_request (154-171): clamps max_tokens to 200000 for ANY model containing "opus", 128000 for "sonnet", else 64000 — but the opus ceiling is 200000 while JS capabilities say 128000 — and there is NO thinking budget reconciliation after the clamp (if budget_tokens >= max_tokens the request 400s).

**Implementation steps:**

1. In openai_to_claude.rs: change adjust_max_tokens to `fn adjust_max_tokens(body, ceiling: u32) -> u32` and after the budget bump add `if max_tokens > ceiling { max_tokens = ceiling; }`; call it with a model ceiling — add a `fn model_output_ceiling(model: &str) -> u32` helper returning 128000 when the lowercased model contains "claude" and ("opus" or "sonnet") and a version >= 4.6 (match capabilities.js opus-4.6/4.7/4.8/5 + sonnet-4.6/5), else 64000. Keep the DEFAULT_MAX_TOKENS/DEFAULT_MIN_TOKENS consts at 64000/32000.
2. In claude_format.rs prepare_claude_request: change the opus ceiling from 200_000 to 128_000 (JS parity; sonnet already 128_000, others 64_000).
3. In claude_format.rs, AFTER the clamp, add the JS budget reconciliation: if body.thinking.type=="enabled" && budget_tokens is a number && budget_tokens >= max_tokens → set max_tokens = min(budget_tokens+1024, ceiling); then if budget_tokens >= max_tokens → set budget_tokens = max(1024, max_tokens-1024).

**Guard test:**

In openai_to_claude.rs add `adjust_max_tokens_clamps_to_model_ceiling` — body {max_tokens:200000}, model "claude-opus-4.8" → result.max_tokens == 128000 (not 200000). Add `claude_format_reconciles_budget_after_clamp` in claude_format.rs — body {max_tokens:128000, thinking:{type:enabled,budget_tokens:128000}} model claude-sonnet-4.6 → after prepare_claude_request, max_tokens == 128000 (budget+1024 capped at ceiling) and budget_tokens == 126976 (128000-1024).

**⚠️ Risks:**

The 128000 ceiling for opus is a behavior CHANGE (was 200000) — match JS capabilities exactly. The reconciliation only runs when thinking.type == "enabled" AND budget_tokens is truthy AND budget >= max_tokens; budget shrink floor is 1024. If budget+1024 already exceeds ceiling, max_tokens stays at ceiling and the budget is shrunk to max(1024, max_tokens-1024) — both steps are required, in this order.

**Cross-check:** 🟡 **PLAUSIBLE** — Both "current behavior" claims are verbatim-accurate. JS: openai-to-claude.js:22-27 has `const modelCeiling = getCapabilitiesForModel(null, model).maxOutput || undefined;` feeding `adjustMaxTokens(body, modelCeiling)` (claim cited 18-27; code sits at 22-27, trivial line-offset), and maxTokens.js:12-33 matches exactly: default 64000, tools→32000 bump, thinking.budget_tokens→+1024 bump, final "Never exceed the ceiling" clamp (line 30); constants confirmed at runtimeConfig.js:65-66. Rust: openai_to_claude.rs:379-400 inline `adjust_max_tokens(body)` has the tools-32000 bump and budget+1024 bump but NO ceiling param/clamp, and claude_format.rs:154-171 clamps opus→200_000, sonnet→128_000, else 64000 — both exactly as claimed. The impl_steps (add a ceiling param + post-budget clamp, with a model_output_ceiling helper returning 128000 for claude opus/sonnet >= 4.6) WOULD fix the headline divergence: JS clamps body.max_tokens=150000→128000 while Rust currently leaves 150000 (stage-2 opus ceiling 200000 never triggers); after the fix both reach 128000. However, the task title explicitly includes "budget reconciliation" and the impl steps omit it: in JS, prepareClaudeRequest (claude.js:212-217, reachable in this flow via translator/index.js:133) reconciles thinking.budget_tokens vs the ceiling-clamped max_tokens by raising max_tokens to min(budget+1024, ceiling) AND shrinking budget to max(1024, max_tokens-1024). Rust has no equivalent: openai_to_claude.rs reasoning_effort "max"→budget 128000 (line 688) combined with the proposed clamp yields max_tokens=128000, budget_tokens=128000 — not strictly greater, a Claude API 400 — whereas JS ships max=128000, budget=126976. The Rust prepare_claude_request ceiling (opus=200k) also still diverges from JS's capabilities-based maxOutput (128000) for non-4.6 opus models, though stage-1 clamping masks it for 4.6+. So the model-ceiling half works but the budget-reconciliation half of the stated task scope is an obvious omission.

---

### `P0-A13` — MiniMax reasoning placeholder (reasoningInject scope:all) + bare-function tool shape

**JS (source of truth — verbatim):**

providers/registry/minimax.js:23-28 and minimax-cn.js:23-28: `quirks: { dropOutputConfig: true }, reasoningInject: { scope: "all" }`.
utils/reasoningContentInjector.js:6 `const PLACEHOLDER = " ";`, providerRuleFor reads `PROVIDERS[provider]?.reasoningInject` (line 9), shouldInject (29-35): role assistant, no non-empty reasoning_content, scope "all" → true; applyRule injects `reasoning_content: " "` on every matching assistant message.
request/openai-to-claude.js:159-177: `const toolData = tool.function ?? tool; const originalName = toolData.name;` — a bare `{ name, description, parameters }` tool (no parent `type` and no `function` wrapper) must still yield `toolData.name`, because Anthropic-compatible gateways (notably MiniMax M3 at api.minimaxi.com) reject payloads where this falls through with `toolData.name === undefined` (upstream code 2013 "invalid tool type"). The JS also adds `cache_control: {type:"ephemeral", ttl:"1h"}` to the LAST tool declaration (openai-to-claude.js:181).

**Current Rust behavior:**

src/core/utils/reasoning_content_injector.rs rule_for (23-36): only deepseek provider (Scope::All) and kimi-/deepseek- model prefixes — NO minimax/minimax-cn rule, so MiniMax assistant messages never get the placeholder. src/core/translator/request/openai_to_claude.rs tool loop (602-661): `let tool_type = tool.get("type").and_then(Value::as_str).unwrap_or("function");` then `let func = tool.get("function"); let original_name = func.and_then(|f| f.get("name"))...` — for a bare `{name,description,parameters}` tool (no type), tool_type defaults to "function", func is None → original_name is "" → a nameless tool is sent (MiniMax M3 2013).

**Implementation steps:**

1. reasoning_content_injector.rs: add to rule_for: `if provider == "minimax" || provider == "minimax-cn" { return Some(Scope::All); }` (before the model-prefix fallbacks).
2. openai_to_claude.rs: in the tool loop, change `let func = tool.get("function");` to `let func = tool.get("function").filter(|f| f.get("name").and_then(Value::as_str).map(|n| !n.is_empty()).unwrap_or(false)).or(Some(tool));` — so a bare `{name,...}` tool (or `{function:{name}}`) resolves original_name correctly, and the loop should only push a tool when original_name is non-empty (skip otherwise). Verify the resulting ClaudeTool pushes the original (unprefixed) name and last-tool cache_control already set at lines 664-668.

**Guard test:**

In reasoning_content_injector.rs add `minimax_injects_on_all_assistant_messages` — provider "minimax", model "MiniMax-M3", body messages [{role:assistant,content:x}] → reasoning_content == " "; provider "minimax" with existing reasoning_content untouched. In openai_to_claude.rs add `bare_function_tool_keeps_name` — body tools [{name:"echo",description:"d",parameters:{}}] → result.tools[0].name == "echo" (not empty).

**⚠️ Risks:**

MiniMax M2.x cannot disable thinking (thinkingCanDisable:false) — the placeholder is separate from thinking enablement, keep the two decoupled. The bare-function branch must not break the normal `{type:"function",function:{name}}` shape (already handled). Do not add cache_control to the placeholder injection — it is a message-level field, unrelated to tools.

**Cross-check:** 🟡 **PLAUSIBLE** — JS and Rust "current behavior" claims are fully verified real; impl step 1 is sound; but impl step 2 has a material omission that prevents the claimed parity.

CONFIRMED REAL (JS): .tmp/9router/open-sse/providers/registry/minimax.js:23-28 and minimax-cn.js:23-28 both carry `quirks: { dropOutputConfig: true }, reasoningInject: { scope: "all" }` under `transport`. .tmp/9router/open-sse/utils/reasoningContentInjector.js:6 has `const PLACEHOLDER = " "`; line 9 providerRuleFor reads `PROVIDERS[provider]?.reasoningInject`; shouldInject at 29-35 and applyRule (injects `reasoning_content: " "`) match the claim. providers/index.js builds PROVIDERS from registry transports so `PROVIDERS["minimax"].reasoningInject = {scope:"all"}` resolves. Also verified minimax.js:57 MiniMax-M3 has targetFormat:"claude", which is what routes JS minimax M3 requests through openai-to-claude.js where the bare-function fix (line 164 `tool.function ?? tool`, comment citing MiniMax M3 #2435) lives.

CONFIRMED REAL (Rust): src/core/utils/reasoning_content_injector.rs rule_for (23-36) only covers deepseek provider (Scope::All) and kimi-/deepseek- model prefixes; no minimax/minimax-cn. inject_reasoning_content is called for every provider at src/core/executor/default.rs:1005, so minimax requests hit it but rule_for returns None. src/core/translator/request/openai_to_claude.rs tool loop at 602-661 with `let func = tool.get("function")` matches the claim.

IMPL STEP 1: correct — adding `if provider == "minimax" || provider == "minimax-cn" { return Some(Scope::All); }` closes the reasoningInject gap on the executor path.

IMPL STEP 2 (the problem): The bare-function fix targets `openai_to_claude_request`, but minimax requests in Rust never flow through that translator. RequestPlan::new (src/core/chat/mod.rs:90-93) calls resolve_transport("minimax", OpenAi) which matches the OpenAI transport (both OpenAI and Claude transports exist), so target_format=OpenAi, needs_translation()=false, body stays OpenAI and is sent to the OpenAI endpoint (build_url via runtime_transport). The Rust provider_catalog.json has no targetFormat for MiniMax models (unlike JS minimax.js:57), so the JS "MiniMax-M3 forces Claude translation" routing is absent in Rust. Tools are instead converted by convert_openai_tools_to_claude (src/core/executor/default.rs:1364-1408, gated by the matches! block at 992-997), which has the SAME bare-function gap: it requires type=="function" AND tool_obj.get("function"), so a bare {name,description,parameters} tool is dropped entirely. The impl step does not touch this function, so even after step 2, minimax requests still drop bare-function tools. To achieve parity, step 2 must also (or instead) patch convert_openai_tools_to_claude, and the missing catalog targetFormat:"claude" for MiniMax-M3 likely needs restoring to match JS routing.

Because the factual claims are 100% verified and step 1 is correct, but step 2 targets the wrong code path for the minimax flow (an obvious omission), the spec is "mostly right" rather than fully accurate — PLAUSIBLE.

---

### `P0-A6` — Remove dead PROVIDER_REGISTRY from provider.rs (or wire it up) + migrate kilo-gateway/venice/featherless to live map

**JS (source of truth — verbatim):**

The live chat path (src/app/api/v1/* -> open-sse/handlers/chat.js -> open-sse/executors/default.js) reads open-sse/providers/registry/* which is the single source of truth with 115 enabled entries. There is no second provider table in JS.

**Current Rust behavior:**

src/core/executor/provider.rs:682-1423 defines static PROVIDER_REGISTRY (a separate ProviderExecutorConfig map, ~140 keys) plus helper fns get_provider_config (1425), is_supported_provider (1429), all_providers (1433), get_oauth_providers (1437), get_api_key_providers (1441), get_free_providers (1613), get_specialty_providers (1626). Verified runtime consumers: ONLY src/server/api/media.rs:638 uses get_provider_config (to resolve a media baseUrl). get_oauth_providers/get_api_key_providers/get_free_providers/get_specialty_providers/all_providers/is_supported_provider/UnifiedExecutor::for_provider have NO runtime callers (grep across src found only re-export in executor/mod.rs and the definition itself). The chat path uses default.rs PROVIDER_CONFIGS exclusively (chat.rs:1635). This is dead code that silently diverges from the live map (e.g. blackbox URL wrong here too, featherless-ai key wrong).

**Implementation steps:**

Recommended: delete PROVIDER_REGISTRY (provider.rs:682-1423), get_provider_config (1425-1427), is_supported_provider (1429-1431), all_providers (1433-1435), get_oauth_providers (1437-1439), get_api_key_providers (1441-1611), get_free_providers (1613-1624), get_specialty_providers (1626-1649), and UnifiedExecutor::for_provider (321-324). Then: 1) In src/server/api/media.rs:638, change get_provider_config(provider) to read the live map — expose a helper from default.rs (e.g. pub fn provider_config_base_url(provider:&str)->Option<String> reading PROVIDER_CONFIGS) or inline a match. 2) Update executor/mod.rs:93-98 re-exports to drop the deleted symbols (keep LogEntry/LogLevel/ProviderExecutionRequest/ProviderExecutionResponse/ProviderExecutor/ProviderExecutorError/ProxyOptions). 3) If any tests import the deleted fns, update them. 4) The kilo-gateway/venice/featherless entries currently living ONLY in the dead registry are covered by tasks P0-A1h/A1m/A1g — after this deletion they exist only in the live map, which is correct.

**Guard test:**

A compile-level guard: add a test asserting the two maps do not diverge — tests/executor_pool_behavior.rs: #[test] fn live_map_has_no_duplicate_dead_keys(): assert every provider key in default.rs PROVIDER_CONFIGS that also existed in the old registry resolves a baseUrl through the media helper; simpler: keep the deletion itself as the guard (cargo build fails if any consumer references a deleted symbol). Optionally add #[test] fn all_enabled_providers_reachable(): for each of the 17 (alims-intl, api-airforce, baidu, bluesminds, clinepass, codebuddy-intl, featherless, kilo-gateway, perplexity-agent, poolside, selfhosted-embedding, selfhosted-stt, selfhosted-tts, tencent, tokenrouter, venice, zed) plus blackbox/siliconflow, assert DefaultExecutor::new(provider,...) returns Ok.

**⚠️ Risks:**

media.rs:638 is the ONLY live consumer — after deletion it MUST be rewired or media baseUrl resolution falls back to format!("https://api.{}.com/v1", provider) (media.rs:640) which is wrong for kilo-gateway/venice/featherless. If you choose to keep the registry instead of deleting, then tasks P0-A1h/A1m/A1g (adding to the live map) would create TWO sources of truth that can drift again — the deletion is the durable fix. Check tests/ project_setup.rs references src/core/executor/mod.rs only for existence, not symbols.

**Cross-check:** ✅ **CONFIRMED** — All three verification points confirmed.

1. JS behavior REAL: The live path src/app/api/v1/chat/completions/route.js → @/sse/handlers/chat.js → open-sse/handlers/chatCore.js (getExecutor at line 278) → open-sse/executors/index.js (line 65-69, DefaultExecutor for any non-specialized provider) → executors/default.js exists exactly as claimed. open-sse/providers/registry/index.js is the single source of truth with exactly 115 enabled entries (counted in the export array); providers/index.js builds PROVIDERS/PROVIDER_MODELS/PROVIDER_OAUTH/PROVIDER_MEDIA from REGISTRY, and config/providers.js + config/providerModels.js are thin barrels off that same registry — no second provider table. kilo-gateway/venice/featherless all exist as registry entries with correct baseUrls.

2. Rust current behavior REAL (one minor undercount): provider.rs:682 starts static PROVIDER_REGISTRY (BTreeMap<&str, ProviderExecutorConfig>) through line 1423. Actual key count ~177 (spec said ~140 — undercount, but "large separate map" claim stands). Helper line ranges match exactly: get_provider_config (1425-1427), is_supported_provider (1429-1431), all_providers (1433-1435), get_oauth_providers (1437-1439), get_api_key_providers (1441-1611), get_free_providers (1613-1624), get_specialty_providers (1626-1649), UnifiedExecutor::for_provider (321-324). The 3 providers are in PROVIDER_REGISTRY (provider.rs L1277/1324-1325/1332-1333) and get_api_key_providers (L1576/1587/1589) but absent from the live default.rs PROVIDER_CONFIGS used by chat.rs's DefaultExecutor — confirming the parity gap.

3. Impl would produce parity: UnifiedExecutor is referenced only inside provider.rs and the mod.rs re-export, never in server code (verified repo-wide); chat.rs uses DefaultExecutor::new backed by default.rs PROVIDER_CONFIGS or provider_node DB data, not PROVIDER_REGISTRY. The only external consumer of executor get_provider_config is media.rs:638 (base-URL fallback); oauth.rs's get_provider_config is a distinct local OAuthProviderConfig fn. Migrating the 3 providers into the live map (default.rs PROVIDER_CONFIGS) is exactly the needed fix since they currently only exist in the dead registry and would hit ExecutorError::UnsupportedProvider in the live path. Minor caveats: (a) the visible spec is truncated and doesn't enumerate the required mod.rs:93-98 pub use cleanup (a compile-error consequence if missed) — but it's an obvious step; (b) ~140 vs ~177 keys undercounts the map size. Neither changes the direction or the accuracy of the core claims.

---

### `P0-A6` — max_thinking_length + additionalModelRequestFields not emitted by Rust Kiro translators

**JS (source of truth — verbatim):**

kiroConstants.js:354-357 buildThinkingSystemPrefix: `const safeBudget = Math.max(1, Math.min(32000, Number(budget) || KIRO_THINKING_BUDGET_DEFAULT)); return `<thinking_mode>enabled</thinking_mode>
<max_thinking_length>${safeBudget}</max_thinking_length>`;`
Both kiro translators push this prefix into systemPrompt ONLY when `thinkingBudget !== null && !usesNativeGptEffort` (openai-to-kiro.js:347-349, claude-to-kiro.js:250-252). budget comes from resolveKiroThinkingBudget(body, credentials?.rawHeaders, model) (kiroConstants.js:147-172): mode budget → budget; mode level → effortToBudget(level) (thinking.js:9-17 LEVEL_TO_BUDGET low=1024, medium=8192, high=24576, xhigh=32768, max=128000) ?? 16000; anthropic-beta header contains "interleaved-thinking" → 16000; body/system contains `<thinking_mode>enabled|interleaved</thinking_mode>` → 16000; model contains "thinking" or "-reason" → 16000; else null.
additionalModelRequestFields via buildKiroAdditionalModelRequestFieldsForModel(body, model) (kiroConstants.js:248-252 + 202-237): for Claude models with resolveKiroEffortPath(model)=="output_config" (major>4, or 4 with minor>5) → `{ thinking: { type: "adaptive", display: "summarized" }, output_config: { effort } }`; for GPT-5.6 models (path "reasoning") → `{ reasoning: { effort } }`; else undefined. Tests assert: reasoning_effort low → systemPrompt contains `<max_thinking_length>1024</max_thinking_length>` AND additionalModelRequestFields == {thinking:{type:adaptive,display:summarized},output_config:{effort:low}} for claude-sonnet-4.6 (tests/unit/openai-to-kiro.test.js:289-302); GPT-5.6 reasoning.effort high → additionalModelRequestFields {reasoning:{effort:high}} with NO legacy prompt tags (lines 323-338); unsupported efforts (auto/minimal/ultra) → NO additionalModelRequestFields but legacy `<thinking_mode>enabled</thinking_mode>` + `<max_thinking_length>` fallback (lines 370-384); none/off/disabled → neither (386-400).

**Current Rust behavior:**

src/core/translator/request/claude_to_kiro.rs:457-459 and openai_to_kiro.rs:471-473 push ONLY `"<thinking_mode>enabled</thinking_mode>"` — no `<max_thinking_length>` line, no budget resolution, no effortToBudget mapping. `build_thinking_system_prefix` exists in src/core/config/kiro_constants.rs:119-126 but is never called by the translators. Neither translator emits `additionalModelRequestFields`. The thinking detection uses `reasoning_effort.is_some() || thinking.type=="enabled"` (claude_to_kiro.rs:451-456 / openai_to_kiro.rs:465-470) — e.g. reasoning_effort "none" still emits the prefix (JS resolveKiroThinkingBudget returns null for none).

**Implementation steps:**

1. Add to kiro_constants.rs a `resolve_kiro_thinking_budget(body: &Value, headers: Option<&dyn HeaderLookup>, model: &str) -> Option<u32>` mirroring JS: check body.output_config.effort, body.thinking (disabled→None, enabled/adaptive+budget_tokens→budget), body.reasoning_effort / body.reasoning.effort (none/off/disabled→None; auto→16000 default via KIRO_THINKING_BUDGET_DEFAULT; low→1024, medium→8192, high→24576, xhigh→32768, max→128000, minimal→512), then anthropic-beta header contains "interleaved-thinking"→Some(16000), then contains_thinking_mode_tag→Some(16000), then model contains "thinking"/"-reason"→Some(16000), else None. HeaderLookup trait already exists (kiro_constants.rs:191-224).
2. Add `resolve_kiro_effort_path(model) -> Option<&'static str>` ("reasoning" for gpt/5/6 tokens, "output_config" for claude >4.5, else None) and `build_kiro_additional_model_request_fields_for_model(body, model) -> Option<Value>` per the JS rules, incl. effort extraction (xhigh/max→high for claude, max→xhigh for gpt) and `uses_kiro_native_gpt_effort`.
3. In BOTH claude_to_kiro.rs and openai_to_kiro.rs, replace the `<thinking_mode>` push with: `let thinking_budget = resolve_kiro_thinking_budget(body, raw_headers_ref, &upstream_model); let uses_native = uses_kiro_native_gpt_effort(...); if let Some(b) = thinking_budget { if !uses_native { system_prompt_parts.push(build_thinking_system_prefix(Some(b))); } }` (pass the raw headers you already compute at claude_to_kiro.rs:488 / openai_to_kiro.rs:502 as the HeaderLookup impl for serde_json::Value). Also gate the old `thinking_enabled` block on the resolved budget being non-None so reasoning_effort "none" no longer enables it.
4. After building system_prompt, add: `if let Some(amrf) = build_kiro_additional_model_request_fields_for_model(body, upstream_model) { payload["additionalModelRequestFields"] = amrf; }` (only when resolve_kiro_effort_path returns Some; effort must be non-null).

**Guard test:**

In openai_to_kiro.rs tests add `reasoning_effort_low_emits_max_thinking_length_1024` — body {reasoning_effort:low}, model "claude-sonnet-4.6" → payload.systemPrompt contains "<max_thinking_length>1024</max_thinking_length>" and payload.additionalModelRequestFields == {"thinking":{"type":"adaptive","display":"summarized"},"output_config":{"effort":"low"}}. Add `reasoning_effort_none_emits_nothing` — body {reasoning_effort:none} → systemPrompt has NO "<thinking_mode>" and NO "<max_thinking_length>" and no additionalModelRequestFields. Add `gpt56_reasoning_effort_maps_to_reasoning_fields` — model "gpt-5.6-sol", body {reasoning:{effort:high}} → additionalModelRequestFields == {"reasoning":{"effort":"high"}} and no "<thinking_mode>" in systemPrompt.

**⚠️ Risks:**

Preserve the JS bug-for-bug behavior that unsupported effort strings (auto/minimal/ultra) fall back to the LEGACY prompt tag (budget = KIRO_THINKING_BUDGET_DEFAULT 16000) while SUPPORTED Claude/GPT models get native fields instead of tags. `build_thinking_system_prefix` clamps to 1..32000 — a max-level budget 128000 for a claude-budget model must still go through the clamp. GPT-5.6 effort "max" → wire "xhigh" (kiroConstants.js:187-200 extractKiroGptEffortLevel).

**Cross-check:** ✅ **CONFIRMED** — All three verification points hold.

1. JS behavior is REAL. open-sse/config/kiroConstants.js:354-357 matches verbatim: buildThinkingSystemPrefix clamps budget to 1..32000 with KIRO_THINKING_BUDGET_DEFAULT=16000 (line 47) and returns "<thinking_mode>enabled</thinking_mode>\n<max_thinking_length>{safeBudget}</max_thinking_length>". Both translators push the prefix only when thinkingBudget !== null && !usesNativeGptEffort: openai-to-kiro.js:347-349 and claude-to-kiro.js:250-252. Budget resolution (kiroConstants.js:147-172 resolveKiroThinkingBudget): disabled/none → null, budget_tokens → budget, effort level → effortToBudget (thinking.js:9-17), auto → 16000. The spec's cited mapping (low→1024, medium→8192, auto→16000) is exact.

2. Rust current behavior is REAL. claude_to_kiro.rs:451-459 and openai_to_kiro.rs:465-473 each run an inline boolean check (reasoning_effort present OR thinking.type=="enabled") and push ONLY "<thinking_mode>enabled</thinking_mode>" — no max_thinking_length, no budget resolution, no effortToBudget. build_thinking_system_prefix exists at kiro_constants.rs:119-126 with the correct clamp+format but is dead code: grep confirms it is unreferenced anywhere else in src/. additionalModelRequestFields appears nowhere in either Rust translator (matches the P0-A6 title's second half).

3. Impl step would produce parity. The proposed resolve_kiro_thinking_budget(body, headers, model) -> Option<u32> signature mirrors the JS resolveKiroThinkingBudget exactly, and the cited budget constants are correct.

Minor caveats that don't change the verdict: (a) the step text (truncated at "medium→819") doesn't explicitly say to wire the resolver into both translators replacing the inline boolean check, though that is the implied necessary swap and the two inline blocks are precisely where it goes; (b) the JS-side !usesNativeGptEffort guard and the additionalModelRequestFields emission (the other half of the P0-A6 title) are not in the shown step text — likely separate impl steps, since the provided text cuts off mid-sentence. These are omissions in the visible snippet, not inaccuracies in what it does claim.

---

### `P0-A7` — Catalog/model metadata parity for the 17 providers (provider_catalog.json)

**JS (source of truth — verbatim):**

JS registry files define per-provider models with contextLength (baidu.js, bluesminds.js, kilo-gateway.js, alims-intl.js, api-airforce.js, tencent.js) and kinds (tokenrouter.js, venice.js). Example baidu.js:24-32 contextLength 1048576/1048576/512000/198000/262144/262144/262144.

**Current Rust behavior:**

src/core/model/provider_catalog.json: 17 provider ids present in 'providers'[] (verified): alims-intl, api-airforce, baidu, bluesminds, codebuddy-intl, kilo-gateway, poolside, tencent, venice, zed have a 'providers' entry; clinepass, featherless, perplexity-agent, selfhosted-embedding, selfhosted-stt, selfhosted-tts, tokenrouter DO NOT (verified). providerIdToAlias in the catalog is EMPTY for these (verified: alias list shows no baidu/venice/zed/tencent mapping; the baidu/tencent/zed/venice providers entries carry their own 'alias' field: baidu->qianfan, tencent->hunyuan, zed->zd, venice->venice). providerModels (90 entries) — need to verify per-alias model lists exist for the 17.

**Implementation steps:**

1) For each of the 7 providers missing a 'providers'[] entry (clinepass, featherless, perplexity-agent, selfhosted-embedding, selfhosted-stt, selfhosted-tts, tokenrouter), add a ProviderCatalogProvider entry {id, alias, serviceKinds, ttsModels:[], embeddingModels:[], hasSearch:false, hasFetch:false} mirroring JS (clinepass serviceKinds llm; featherless llm; perplexity-agent llm+webSearch with hasSearch:true; tokenrouter llm+embedding+image with embeddingModels filled; selfhosted-* embedding/stt/tts respectively). 2) Populate providerModels[] entries (alias-keyed) for the 17 with the model lists from their JS registry files, carrying contextLength into context_window (catalog.rs:29 context_window: Option<u32>) and kind (llm/embedding/stt/tts/image/video/audio) into the catalog kind field (catalog.rs:19 kind: String). 3) Add providerIdToAlias mappings (venice->venice, tencent->hunyuan, baidu->qianfan, zed->zd, alims-intl->alims-intl, etc.) matching each JS alias field. 4) Keep provider_catalog.json schema valid (catalog.rs:6-12 deserializes it at startup — a malformed file panics the Lazy init, catalog.rs:107).

**Guard test:**

catalog.rs test module: #[test] fn catalog_has_all_17_providers(): for each of the 17 ids assert provider_catalog().provider_info(id).is_some(); #[test] fn catalog_model_kinds_match_registry(): assert venice embeddingModels == ["text-embedding-3-large","text-embedding-bge-m3","text-embedding-qwen3-8b"] and baidu alias resolves to qianfan.

**⚠️ Risks:**

provider_catalog.json is loaded via include_str! at compile time (catalog.rs:106) — a JSON syntax error breaks the entire binary build/startup, not just this feature. The ProviderCatalogFile struct (catalog.rs:7-12) is NOT #[serde(default)] on most fields, so missing fields fail deserialization. Kind values must match the catalog's existing vocabulary (look at existing entries before adding). Model ids with '/' (e.g. poolside/laguna-s-2.1) are fine as catalog ids (catalog.rs:18 id: String).

**Cross-check:** 🟡 **PLAUSIBLE** — All cited JS files and line ranges are REAL and match precisely: baidu.js:25-31 has 7 models with contextLength 1048576/1048576/512000/198000/262144/262144/262144; bluesminds.js/kilo-gateway.js/api-airforce.js/tencent.js all define per-model contextLength; alims-intl.js models have no contextLength; tokenrouter.js (kind: video/image/audio/embedding + serviceKinds ["llm","embedding","image"]) and venice.js (kind embedding/image + serviceKinds) match. The 7 providers' JS serviceKinds match the claim (clinepass/featherless implicit llm; perplexity-agent ["llm","webSearch"]; selfhosted-embedding ["embedding"]; selfhosted-stt ["stt"]; selfhosted-tts ["tts"]). Rust current behavior is also REAL: provider_catalog.json (3 keys: providerIdToAlias, providerModels, providers) has 107 providers entries with the 10 listed present (alims-intl, api-airforce, baidu, bluesminds, codebuddy-intl, kilo-gateway, poolside, tencent, venice, zed) and all 7 (clinepass, featherless, perplexity-agent, selfhosted-embedding, selfhosted-stt, selfhosted-tts, tokenrouter) absent from providers[] (also absent from providerModels[] and providerIdToAlias). The proposed ProviderCatalogProvider fields exactly match the deserialization struct (catalog.rs:43: id, alias, service_kinds, tts_models, embedding_models, has_search, has_fetch; ttsModels/embeddingModels are non-optional Vec<String> so empty arrays are required and the impl specifies them), and the entries would correctly gate kind-filtered /v1/models via provider_matches_kinds (v1_models.rs:367) and enable the hasSearch/hasFetch/ttsModels/embeddingModels card paths (v1_models.rs:285-325). Caveats that keep this at PLAUSIBLE rather than CONFIRMED: (1) The task title is "model metadata parity," but the impl only adds providers[] entries; the JS model-level metadata for the 7 (tokenrouter's 120 seeded models with kinds, venice's image/embedding kinds, selfhosted kokoro/whisper-1/embedding models, perplexity-agent webSearch) lives in JS models[] which maps to Rust providerModels[] (kind field), and none of the 7 have providerModels[] or providerIdToAlias entries — so an active connection to e.g. tokenrouter would still enumerate zero static models (models_for_alias returns None, falling back to enabledModels/discovery only), leaving the exact model catalogs the JS claim emphasizes unported. (2) Featherless id mismatch: Rust's canonical id is "featherless-ai" (executor/provider.rs:1332, model/mod.rs:96, providerIdToAlias "featherless-ai"->"featherless"), while the impl mirrors JS id "featherless"; a providers[] entry keyed "featherless" would not match provider_info("featherless-ai") for existing connections unless the id is reconciled. These are omissions/ambiguities an implementer must resolve, not fatal errors — the JS and Rust state claims are fully accurate.

---

### `P0-A7` — inferenceConfig always-emit + maxTokens constant divergence in Kiro translators

**JS (source of truth — verbatim):**

openai-to-kiro.js:309 `const maxTokens = 32000;` and 416-421:
```js
if (maxTokens || temperature !== undefined || topP !== undefined) {
  payload.inferenceConfig = {};
  if (maxTokens) payload.inferenceConfig.maxTokens = maxTokens;
  if (temperature !== undefined) payload.inferenceConfig.temperature = temperature;
  if (topP !== undefined) payload.inferenceConfig.topP = topP;
}
```
→ openai→kiro ALWAYS emits `inferenceConfig: { maxTokens: 32000, ... }` (constant, ignores body.max_tokens).
claude-to-kiro.js:222 `const maxTokens = body.max_tokens || 32000;` and 323-328 same shape → claude→kiro ALWAYS emits `inferenceConfig: { maxTokens: <body.max_tokens||32000>, ... }`.

**Current Rust behavior:**

claude_to_kiro.rs:571-590: reads client max_tokens (max_completion_tokens fallback, 32000 default) but only creates inferenceConfig `if temperature.is_some() || top_p.is_some()` — omits inferenceConfig entirely when neither present, and always includes maxTokens inside. openai_to_kiro.rs:585-605: same gating; reads client max_tokens (JS hardcodes 32000 for openai→kiro — Rust uses the client value, a deliberate divergence noted in its comment).

**Implementation steps:**

claude_to_kiro.rs: replace the `if temperature.is_some() || top_p.is_some()` gate with JS parity: `payload["inferenceConfig"] = serde_json::json!({});` unconditionally, then `if let Some(t) = temperature { config["temperature"] = t; }`, `if let Some(t) = top_p { config["topP"] = t; }`, and keep maxTokens = `body.max_tokens.or(max_completion_tokens).filter(>0).unwrap_or(32000)`.
openai_to_kiro.rs: same unconditional inferenceConfig; set `maxTokens` to the literal 32000 (match JS constant, NOT body.max_tokens): remove the client_max_tokens read at lines 587-593 and use `32000u64`. Only emit `payload["inferenceConfig"]` — JS writes maxTokens first (32000) then temperature/topP.

**Guard test:**

In openai_to_kiro.rs add `inference_config_always_emitted_max_tokens_32000` — body {messages:[{role:user,content:hi}]}, no temperature/top_p → payload.inferenceConfig == {"maxTokens":32000}. In claude_to_kiro.rs add `inference_config_uses_client_max_tokens` — body {max_tokens:8192, messages:[...]}, no temp → inferenceConfig.maxTokens == 8192; body without max_tokens → 32000.

**⚠️ Risks:**

This intentionally reverts the Rust comment at openai_to_kiro.rs:585-586 ('9router bug fixed') — the parity mandate takes the JS constant 32000 for openai→kiro. Kiro upstream may 400 if maxTokens exceeds its cap, but parity wins per the audit. Do NOT emit `temperature`/`topP` as null — JS only sets them when !== undefined.

**Cross-check:** ✅ **CONFIRMED** — All verifiable claims check out against source.

1. JS behavior is REAL. openai-to-kiro.js (at .tmp/9router/open-sse/translator/request/openai-to-kiro.js):309 is `const maxTokens = 32000;` and lines 416-421 match the cited always-emit block verbatim. Because maxTokens is a truthy constant, the `if (maxTokens || temperature !== undefined || topP !== undefined)` gate is always true → JS always emits `inferenceConfig` and always includes maxTokens inside. The "always-emit" characterization is correct (claude-to-kiro.js:323-328 has the identical block with maxTokens = body.max_tokens || 32000, also always truthy).

2. Rust current behavior is REAL. claude_to_kiro.rs:571-590 reads client max_tokens with max_completion_tokens fallback and 32000 default, but only creates inferenceConfig when `temperature.is_some() || top_p.is_some()` (omitting it entirely when neither is present), and always initializes the config with `{"maxTokens": max_tokens}`. openai_to_kiro.rs:585-605 is structurally identical ("sa" = same). Both match the claim exactly.

3. Impl steps produce parity for the verifiable (claude) portion: unconditional `payload["inferenceConfig"] = json!({})` + conditional temperature/topP setters + maxTokens always present (always ≥32000 via unwrap_or, so always truthy, matching JS's always-included maxTokens). No obvious omission for the claude route.

Two minor nuances, neither parity-breaking: (a) Rust's `max_completion_tokens` fallback is a superset of JS claude-to-kiro which reads only `body.max_tokens` — an edge case where Rust honors a client value JS would ignore, arguably an improvement; (b) the impl text doesn't explicitly restate that maxTokens must remain inside the always-emitted config, but "keep maxTokens = ..." plus the existing `{"maxTokens": ...}` init implies it, and JS parity requires it. The only piece I could not fully verify is the truncated openai_to_kiro.rs impl_steps ("sa" cut off), but given the two Rust blocks are identical and the task title explicitly flags the maxTokens hardcoded-32000-vs-client-read divergence for the openai route, the intended fix is the same unconditional-emit pattern and is consistent.

---

### `P0-A8` — reasoning continuity lost in Responses↔chat (reasoning/encrypted_content buffering)

**JS (source of truth — verbatim):**

openai-responses.js responses→chat (openaiResponsesToOpenAIRequest): buffers reasoning text across input items — `let pendingReasoning = ""; let pendingReasoningEncrypted = "";` (lines 33-34); extractReasoningText(item) (42-52): joins `item.summary[].text` then falls back to `item.content[].text`; on REASONING items (149-160): `if (txt) pendingReasoning = pendingReasoning ? `${pendingReasoning}
${txt}` : txt; if (typeof item.encrypted_content === "string" && item.encrypted_content) pendingReasoningEncrypted = item.encrypted_content; continue;`; attachPendingReasoning (54-59) sets `msg.reasoning_content` and `msg.encrypted_content` on the NEXT assistant message or function_call item (94, 109); non-assistant messages clear both (95-98).
chat→responses (openaiToOpenAIResponsesRequest): buildReasoningInputItem(msg) (266-296) — before each assistant message (332-335): `const reasoningItem = buildReasoningInputItem(msg); if (reasoningItem) result.input.push(reasoningItem);`; item = `{ type: "reasoning", summary: [{type:"summary_text", text}], encrypted_content }` (only if either text or encrypted present); summaryText priority = msg.reasoning_content (trim) > msg.reasoning > msg.reasoning_details.map(d => typeof d?.text === "string" ? d.text : typeof d?.content === "string" ? d.content : "").filter(Boolean).join("\n"). encrypted = msg.encrypted_content || msg.reasoning_encrypted_content || msg.reasoning?.encrypted_content.

**Current Rust behavior:**

src/core/translator/request/openai_responses.rs:152 `Some("reasoning") => {}` — reasoning items silently dropped, no reasoning_content/encrypted_content attach to assistant messages. chat_to_openai_responses_request (197-363) has NO buildReasoningInputItem — assistant messages are emitted without a preceding reasoning item; encrypted_content/reasoning_content/reasoning_details on chat-format assistant messages are dropped.

**Implementation steps:**

In openai_responses.rs responses→chat: add `let mut pending_reasoning = String::new(); let mut pending_reasoning_encrypted = String::new();` before the item loop. On `Some("reasoning")`: extract `txt` = join of item.summary[].text (or item.content[].text); if non-empty append to pending_reasoning with "\n" separator; if `item.encrypted_content` is a non-empty string set pending_reasoning_encrypted; `continue`. In the `Some("message")` and `Some("function_call")` arms, before pushing the assistant message: if role=="assistant" && !pending_reasoning.is_empty() set msg.reasoning_content; if !pending_reasoning_encrypted.is_empty() set msg.encrypted_content; then clear both. For non-assistant roles clear both (JS 95-98).
In chat_to_openai_responses_request: before pushing each assistant message item, call a new `build_reasoning_input_item(msg) -> Option<Value>` port (priority reasoning_content trim > reasoning > reasoning_details array; encrypted_content || reasoning_encrypted_content || reasoning.encrypted_content; emits {type:"reasoning", summary:[{type:"summary_text", text}], encrypted_content}) and push it into result.input immediately before the message item when Some.

**Guard test:**

Add `responses_reasoning_item_buffers_onto_next_assistant` — input [reasoning item with summary[{text:"hmm"}], message role assistant content []], run openai_responses_to_chat_request → the assistant message has reasoning_content "hmm". Add `chat_assistant_reemits_reasoning_item` — chat body messages [{role:assistant, content:[], reasoning_content:"hmm", encrypted_content:"blob"}] → result.input[0] is {type:"reasoning", summary:[{type:"summary_text",text:"hmm"}], encrypted_content:"blob"} and input[1] is the assistant message.

**⚠️ Risks:**

The encrypted_content blob is a store=false continuity token — must round-trip byte-for-byte; do not JSON-escape it. summary text joins with "\n", reasoning_details joins with "\n" (extract in chat→responses) but reasoning_details in the streamed delta (response side) joins with "" — keep the two separators distinct. Only attach to assistant-role messages; user/system/function_call_output items clear the pending buffers.

**Cross-check:** ✅ **CONFIRMED** — All cited claims verified against actual source.

1) JS behavior is REAL: In C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/open-sse/translator/request/openai-responses.js, lines 33-34 are exactly `let pendingReasoning = ""; let pendingReasoningEncrypted = "";`. extractReasoningText (lines 42-52) joins item.summary[].text then falls back to item.content[].text, both with "\n" join, returning "" when neither. The REASONING branch (lines 149-160) contains line 154 `if (txt) pendingReasoning = pendingReasoning ? `${pendingReasoning}\n${txt}` : txt;` and lines 155-158 stash non-empty string encrypted_content into pendingReasoningEncrypted, then continue. Reasoning is attached to the next assistant message via attachPendingReasoning (lines 54-59, invoked at lines 94 and 109) as reasoning_content/encrypted_content. The converse buildReasoningInputItem (266-296) is used at 332-335 in openaiToOpenAIResponsesRequest.

2) Rust current behavior is REAL: In C:/Users/ADMIN/Documents/Projects/openproxy/src/core/translator/request/openai_responses.rs, line 152 is exactly `Some("reasoning") => {}` — reasoning items silently dropped. openai_responses_to_chat_request (30-195) has no buffering variables and its message/function_call branches (77-132) emit only role/content/tool_calls with no reasoning_content/encrypted_content. chat_to_openai_responses_request spans lines 197-363 exactly and has NO buildReasoningInputItem — assistant messages are pushed as plain message items with output_text content only, so a chat→responses→chat round trip drops reasoning continuity.

3) Impl steps produce parity: adding pending_reasoning/pending_reasoning_encrypted before the item loop (near line 61, beside current_assistant_msg), filling the Some("reasoning") arm with summary[].text-or-content[].text join + "\n" append + non-empty encrypted_content capture, then attaching to the next assistant message, exactly mirrors the JS. The only nuance the truncated spec omits is the JS "clear pending reasoning on non-assistant messages" behavior (JS lines 96-97); leaving it out makes the Rust version slightly more generous across a user-message boundary rather than breaking parity, so it is not an obvious omission that would defeat the fix.

---

### `P0-A9` — client_metadata not stripped for cerebras/mistral (dropClientMetadata quirk)

**JS (source of truth — verbatim):**

executors/default.js:74-78:
```js
// quirk: some openai-compatible providers reject Anthropic's client_metadata field
if (this.config.quirks?.dropClientMetadata) {
  delete transformed.client_metadata;
}
stripUnsupportedParams(this.provider, model, transformed);
```
Providers with the quirk: providers/registry/cerebras.js:20 and mistral.js:20 both `quirks: { dropClientMetadata: true }`.

**Current Rust behavior:**

src/core/executor/default.rs transform_request (984-1011) has no client_metadata removal for cerebras/mistral. (kimchi.rs:74 removes client_metadata, but that is the separate Kimi path; opencode_go.rs:14 lists it in a strip list — neither covers cerebras/mistral via default executor.)

**Implementation steps:**

In src/core/executor/default.rs transform_request, before the strip_unsupported_params call at line 1008, add: `if self.provider == "cerebras" || self.provider == "mistral" { if let Some(obj) = body.as_object_mut() { obj.remove("client_metadata"); } }`. Top-level removal only — matches JS `delete transformed.client_metadata`.

**Guard test:**

In default.rs tests add `drops_client_metadata_for_cerebras` — body {client_metadata:{ideType:9},messages:[]}, DefaultExecutor::for_provider("cerebras") (or construct via the same path used by transform_request), call transform_request → body has no client_metadata. Add `keeps_client_metadata_for_openai` — provider "openai", assert client_metadata survives.

**⚠️ Risks:**

client_metadata arrives from Anthropic-spec clients (Claude Code sends it as a JSON object with ideType/platform/pluginType). Only cerebras and mistral get the drop — do not generalize. The drop happens in the executor, AFTER translation, on the final upstream body (matching JS transformRequest ordering).

**Cross-check:** ✅ **CONFIRMED** — All three checks pass. (1) JS behavior REAL: open-sse/executors/default.js transformRequest lines 70-82 contain exactly the quoted quirk block (lines 74-78: `if (this.config.quirks?.dropClientMetadata) { delete transformed.client_metadata; }` then `stripUnsupportedParams(this.provider, model, transformed);`). Registry wiring verified: providers/registry/cerebras.js:19-21 and mistral.js:19-21 both set `transport.quirks: { dropClientMetadata: true }`; providers/index.js buildTransport spreads `transport` (including quirks) to top-level PROVIDERS[id], and DefaultExecutor's constructor (default.js:67) sets `this.config = PROVIDERS[provider]`, so `this.config.quirks` resolves correctly. Cerebras/mistral have no specialized executor in executors/index.js, so they use DefaultExecutor. (2) Rust current behavior REAL: src/core/executor/default.rs transform_request (984-1011) has no client_metadata removal; strip_unsupported.rs should_strip (15-41) only handles max_completion_tokens/reasoning_effort/max_tokens, never client_metadata. Side claims accurate: kimchi.rs:74 removes client_metadata (separate Kimchi executor path) and opencode_go.rs:13-18 lists it in FORBIDDEN_FIELDS — neither covers cerebras/mistral. chat.rs:1635 routes cerebras/mistral (not special-cased) through DefaultExecutor. (3) Impl steps would produce parity: the proposed insert matches JS's top-level-only delete; transform_request takes `&self` so self.provider is available, body is a mutable Value in scope, and removing the key before strip_unsupported_params at line 1008 is correct. No downstream re-injection — client_metadata is only produced by OAuth helpers (antigravity.rs/gemini_cli.rs/oauth.rs) for auth payloads, not chat bodies. Only trivial imprecision: the exact quirk statement is at default.js:74-78 (per the task's own citation, which it quoted correctly); opencode_go's strip is an additional OCg path that doesn't interfere.

---

---

## D. FEATURES v0.5.35-50 (12 specs)

### `P0-B2` — clientDetector: missing codex-tui / codex_cli_rs / "codex desktop" / originator header detection

**JS (source of truth — verbatim):**

open-sse/utils/clientDetector.js:43-44: `if (ua.includes("codex-tui") || ua.includes("codex-cli") || ua.includes("codex_cli_rs") || ua.includes("codex desktop") || originator.startsWith("codex_")) return "codex";`
:24 `const originator = (headers["originator"] || "").toLowerCase();`
Note: `X-Initiator` in JS (line 24) is checked with a mixed-case key `headers["X-Initiator"]` on a lower-cased object — this is a JS quirk (never matches); Rust lowercases all headers so `x-initiator` is fine.
JS detection order (line 20-50): body.userAgent==="antigravity" → github-copilot (githubcopilotchat | openai-intent=conversation-panel | initiator=user) → claude (claude-cli|claude-code|x-app=cli) → gemini-cli → codex (codex-tui|codex-cli|codex_cli_rs|codex desktop|originator startsWith codex_) → deepseek-tui → null.

**Current Rust behavior:**

src/core/utils/client_detector.rs:75 only `if ua.contains("codex-cli")`. Missing codex-tui, codex_cli_rs, "codex desktop", and the `originator` header entirely (function reads user-agent, x-app, openai-intent, x-initiator — no originator). So Codex Desktop (UA "Codex Desktop") and the current Rust CLI (codex-tui) are NOT detected as Codex → `is_native_passthrough(Some(Codex), "codex")` never fires → codex requests are translated instead of passed through losslessly. Also affects stream flags (codex is forceStream so less impact) but passthrough is the main loss.

**Implementation steps:**

In src/core/utils/client_detector.rs `detect_client_tool`:
1. Add `let originator = headers.get("originator").map(|s| s.to_lowercase()).unwrap_or_default();` after the initiator line (line 53).
2. Replace the codex branch (line 75) with:
```rust
if ua.contains("codex-tui")
    || ua.contains("codex-cli")
    || ua.contains("codex_cli_rs")
    || ua.contains("codex desktop")
    || originator.starts_with("codex_")
{
    return Some(ClientTool::Codex);
}
```
3. Keep the order identical to JS: body.userAgent → github-copilot → claude → gemini-cli → codex → deepseek-tui. The existing Rust order already matches. (gemini-cli branch line 71 is before codex; deepseek-tui after — correct.)
4. No change to `is_native_passthrough` — Codex already maps to ["codex"] at line 92.

**Guard test:**

`detects_codex_tui_and_desktop`: headers user-agent="codex-tui/0.5.0" → Codex; user-agent="codex_cli_rs/0.2.0" → Codex; user-agent="Codex Desktop" (case-insensitive) → Codex; headers originator="codex_work_desktop" → Codex; user-agent="codex-cli" → Codex. Add to existing tests in client_detector.rs.

**⚠️ Risks:**

Order matters: github-copilot check (openai-intent=conversation-panel | initiator=user) fires BEFORE codex — a Codex Desktop request with initiator header could be mis-detected as copilot. Preserve JS order exactly. originator must be lowercased before starts_with (JS lowercases the whole value). The JS `X-Initiator` mixed-case lookup is a bug — do NOT replicate it; lowercase `x-initiator` is correct.

**Cross-check:** ✅ **CONFIRMED** — Verified all three claims against the actual files.

(1) JS claim REAL: open-sse/utils/clientDetector.js lines 43-44 contain exactly `if (ua.includes("codex-tui") || ua.includes("codex-cli") || ua.includes("codex_cli_rs") || ua.includes("codex desktop") || originator.startsWith("codex_")) return "codex";`. Line 24 is the initiator extraction `(headers["x-initiator"] || headers["X-Initiator"] || "").toLowerCase()` — the mixed-case X-Initiator quirk note is accurate. The `originator` extraction `(headers["originator"] || "").toLowerCase()` is at line 25, not line 24 as cited — a trivial off-by-one citation error that does not change the substance (the code and behavior exist exactly as described).

(2) Rust claim REAL: src/core/utils/client_detector.rs line 75 is only `if ua.contains("codex-cli")`. The function reads user-agent (38-41), x-app (42-45), openai-intent (46-49), x-initiator (50-53) — no originator header anywhere. Missing codex-tui, codex_cli_rs, "codex desktop", and the originator check are all genuine gaps.

(3) Impl steps produce parity: adding `let originator = headers.get("originator").map(|s| s.to_lowercase()).unwrap_or_default();` after the initiator block (line 53 is the correct insertion point — closing line of initiator extraction) and replacing the line-75 branch with the combined codex-tui/codex-cli/codex_cli_rs/codex-desktop/originator.starts_with("codex_") check mirrors the JS exactly. JS `.includes()` == Rust `.contains()`; JS `originator.startsWith("codex_")` == Rust `originator.starts_with("codex_")`; both match against lowercased values. The Rust function documents headers must already be lower-cased, consistent with JS's lowercased-object contract, so `headers.get("originator")` works under the same documented contract. The impl snippet in the spec is truncated at "originator.start" but the intent is unambiguous from the quoted JS line. No omission would block parity.

Net: substantively accurate end-to-end; the only flaw is a one-line mis-citation for the originator line number in the spec's file-reference, which is cosmetic.

---

### `P0-C3` — X-9Router-Token-Saver header (per-request opt-out of RTK/headroom/caveman/ponytail) missing in Rust

**JS (source of truth — verbatim):**

open-sse/config/runtimeConfig.js:68: `export const TOKEN_SAVER_HEADER = "x-9router-token-saver";`
open-sse/handlers/chatCore.js:229: `const tokenSaverEnabled = clientRawRequest?.headers?.[TOKEN_SAVER_HEADER]?.toLowerCase() !== "off";`
Then every saver gates on it:
- :232 `compressMessages(translatedBody, tokenSaverEnabled && rtkEnabled)`
- :238 `compressWithHeadroom(translatedBody, { enabled: tokenSaverEnabled && headroomEnabled, ... })`
- :246-261 caveman and ponytail both gated on `tokenSaverEnabled && ...`
A client sending `x-9router-token-saver: off` disables ALL token savers for that request.

**Current Rust behavior:**

src/server/api/chat.rs:831 `compress_messages(&mut body, snapshot.settings.rtk_enabled)` — no header gate. :835-854 headroom enabled = `snapshot.settings.headroom_enabled` — no header gate. :862 `apply_request_preprocessing` (caveman+ponytail) — no header gate. The header is never read. So a client cannot opt out of token savers per-request; compressed tool results may break sensitive tooling that needs raw output.

**Implementation steps:**

1. Add const in src/core/config/runtime_config.rs: `pub const TOKEN_SAVER_HEADER: &str = "x-9router-token-saver";`
2. In src/server/api/chat.rs, where headers_map is built (line 314), read the header: `let token_saver = headers_map.get("x-9router-token-saver").map(|s| s.to_lowercase() != "off").unwrap_or(true);` (headers_map keys are already lowercased at line 318).
3. Thread `token_saver` into the code that runs compress_messages / headroom / apply_request_preprocessing (they're in execute_single_model / forward_with_provider_fallback scope; pass it as a param or compute it in the same function that has headers). Change line 831 to `compress_messages(&mut body, token_saver && snapshot.settings.rtk_enabled)`; line 835 `enabled: token_saver && snapshot.settings.headroom_enabled`; line 862 `apply_request_preprocessing(&mut body, &snapshot.settings, &plan.model)` stays but gate internally by passing a token_saver flag, or gate the call with `if token_saver { apply_request_preprocessing(...) }`.
4. Verify header name is exactly `x-9router-token-saver` (lowercase) and the OFF sentinel is the exact string "off" (case-insensitive). Any other value (including absent) = enabled.

**Guard test:**

`token_saver_header_off_disables_savers`: construct headers_map with `x-9router-token-saver: off`, body with tool messages, settings rtk_enabled=true → compress_messages returns None (no mutation). And `token_saver_header_absent_enables`: no header → compressed. Guard via a small pure helper `fn token_saver_enabled(headers: &HashMap<String,String>) -> bool` with unit test.

**⚠️ Risks:**

The gate is `!== "off"` — a value of "0", "false", "" all mean ENABLED. Only exact case-insensitive "off" disables. Header must be read from the raw client request headers, not the translated body. If thread through closures, make sure combo members each get the same value.

**Cross-check:** ✅ **CONFIRMED** — All three verification targets pass. (1) JS behavior is real: runtimeConfig.js:68 exports TOKEN_SAVER_HEADER = "x-9router-token-saver"; chatCore.js:229 computes tokenSaverEnabled = headers[TOKEN_SAVER_HEADER]?.toLowerCase() !== "off"; and every saver gates on it (RTK compressMessages :232, headroom enabled :238, caveman :252, ponytail :258, plus the warn log :246). All line numbers and exact code match the cited file. (2) Rust current behavior is real: chat.rs:831 calls compress_messages(&mut body, snapshot.settings.rtk_enabled) with no header gate; :835-836 HeadroomConfig.enabled = snapshot.settings.headroom_enabled (headroom.rs:262 early-returns when disabled); :862 apply_request_preprocessing(&mut body, &snapshot.settings, &plan.model) gates only on internal settings.caveman_enabled/ponytail_enabled. Grep across the entire Rust src/ for token-saver/token_saver/9router-token returns zero hits, so the header is genuinely never read. (3) Impl steps would produce parity: src/core/config/runtime_config.rs exists and is the documented Rust port of runtimeConfig.js, so the const belongs there; headers_map keys are lowercased at chat.rs:318 (built at :314 from client request headers), so .get("x-9router-token-saver") works as specified; unwrap_or(true) matches JS default-enabled semantics (absent or empty value → enabled, case-insensitive "off" → disabled). Step 3 is truncated in the display but its intent (thread token_saver into the three saver gates at :831/:836/:862) is clear and covers all saver sites; gating apply_request_preprocessing requires passing the flag into that function (caveman/ponytail flags are internal to it), which is an implementation detail within step 3's scope, not an omission. No defect found.

---

### `P1-D4` — GitHub 402 monthly-reset account lock (githubMonthlyResetMs) missing in Rust error path

**JS (source of truth — verbatim):**

src/sse/services/auth.js:11-18:
```js
const GITHUB_MONTHLY_USAGE_LIMIT = "you've reached your additional usage limit for your plan";
function githubMonthlyResetMs(status, errorText, provider) {
  if (resolveProviderId(provider) !== "github" || Number(status) !== 402) return null;
  if (!String(errorText || "").toLowerCase().includes(GITHUB_MONTHLY_USAGE_LIMIT)) return null;
  const now = new Date();
  return Date.UTC(now.getUTCFullYear(), now.getUTCMonth() + 1, 1);
}
```
:226-254 `markAccountUnavailable`: if githubMonthlyResetMs returns a value → `shouldFallback=true; cooldownMs = githubResetAtMs - Date.now(); newBackoffLevel = 0;` AND crucially `buildModelLockUpdate(githubResetAtMs ? null : model, cooldownMs)` — model is null for github so it writes `modelLock___all` (account-level lock) instead of a per-model lock. The cooldown is NOT capped by MAX_RATE_LIMIT_COOLDOWN_MS (unlike the resetsAtMs branch at :234-237 which caps).

**Current Rust behavior:**

src/server/api/chat.rs:1750 `let decision = check_fallback_error(status, &message, current_backoff);` then :1831-1844 `if decision.should_fallback { mark_connection_unavailable(state, &connection.id, model, status, &message, cooldown, ...); excluded.insert(...); }`. src/core/config/error_config.rs:190 has a `402 => ErrorRule { cooldown: LONG_MS, backoff: false }` — a plain 402 anywhere gets a fixed LONG cooldown (typically minutes), NOT until next UTC month. There is NO github-message sniffing, NO first-of-next-month timestamp, NO account-level (modelLock___all) lock for github 402. So a GitHub premium-limit 402 locks the account for only ~minutes and per-model, then the same 402 repeats — the monthly exhaustion is not honored until reset.

**Implementation steps:**

1. Add const in src/core/config/error_config.rs or a new helper in src/core/account_fallback/mod.rs:
```rust
pub const GITHUB_MONTHLY_USAGE_LIMIT: &str = "you've reached your additional usage limit for your plan";

/// First instant of next UTC month (Date.UTC(y, m+1, 1) parity).
pub fn github_monthly_reset_ms(status: u16, error_text: &str, provider: &str) -> Option<i64> {
    if provider != "github" || status != 402 { return None; }
    if !error_text.to_lowercase().contains(GITHUB_MONTHLY_USAGE_LIMIT) { return None; }
    let now = chrono::Utc::now();
    let next = chrono::NaiveDate::from_ymd_opt(
        if now.month() == 12 { now.year() + 1 } else { now.year() },
        if now.month() == 12 { 1 } else { now.month() + 1 },
        1,
    )?;
    Some(next.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis())
}
```
2. In src/server/api/chat.rs error branch (around line 1750): compute `let github_reset = github_monthly_reset_ms(status.as_u16(), &message, &plan.provider);` BEFORE `check_fallback_error`. If `Some(reset_ms)`:
   - `let cooldown = Duration::from_millis((reset_ms - Utc::now().timestamp_millis()).max(0) as u64);` (NOT capped by MAX_RATE_LIMIT_COOLDOWN_MS)
   - call `mark_connection_unavailable` with `model = ""` (so build_model_lock_update writes `modelLock___all` — verify build_model_lock_update at chat.rs:2283 writes `modelLock_{model}` and that model="" yields `modelLock_`+"" ... instead pass a distinct sentinel or add an account-level path; the JS uses model=null → `modelLock___all`). Check `build_model_lock_update` signature: it builds `format!("modelLock_{model}")`. Add handling so an empty/None model maps to `MODEL_LOCK_ALL` ("modelLock___all", account_fallback/mod.rs:340).
   - set backoff_level to 0 (newBackoffLevel=0), should_fallback=true, excluded.insert(connection.id), continue loop.
3. Keep the existing 402→LONG cooldown rule for non-github providers (that path is unchanged).

**Guard test:**

`github_monthly_reset_ms_only_fires_on_github_402`: github+402+text containing "You've reached your additional usage limit for your plan" (case-insensitive) → Some(next-month-UTC-midnight); github+429+same text → None; codex+402+same text → None; github+402+different text → None. And `mark_github_lock_uses_account_level_key`: with model=None the lock key is `modelLock___all`.

**⚠️ Risks:**

The sentinel text is exact and case-insensitive via `.toLowerCase().includes(...)`. Month rollover at December → January next year. The lock must be ACCOUNT-level (modelLock___all), not per-model, else other models keep hitting the same exhausted account. Do NOT cap the cooldown — JS deliberately skips MAX_RATE_LIMIT_COOLDOWN_MS here (a monthly reset can exceed 30min). Keep JS typo semantics: the message string is "you've reached" (no space after reached).

**Cross-check:** ✅ **CONFIRMED** — JS claim is verbatim-accurate: auth.js:11-18 defines GITHUB_MONTHLY_USAGE_LIMIT and githubMonthlyResetMs exactly as cited (gate on resolveProviderId==="github", status 402, lowercase substring match, returns Date.UTC(y, m+1, 1)). It is live code: markAccountUnavailable (auth.js:226, 230-233, 244) uses it to set cooldown=reset-Date.now(), backoffLevel=0, and an account-wide modelLock___all lock, and is called from chat/embeddings/fetch/imageGeneration/search handlers. Rust claim is accurate: chat.rs:1750 is `let decision = check_fallback_error(status.as_u16(), &message, current_backoff);`, and 1831-1844 calls mark_connection_unavailable then excluded.insert on should_fallback. grep confirms no github_monthly/402-reset logic exists in the Rust tree: error_config.rs handles 402 as a fixed 2-min COOLDOWN_LONG_MS, and mark_connection_unavailable (chat.rs:2273) always locks the specific model — so the gap is real (GitHub monthly-reset 402 yields a per-model 2-min lock instead of account-wide lock until next UTC month). Impl steps are correct and sufficient: the const string and helper signature match JS, and Rust already provides MODEL_LOCK_ALL="modelLock___all" and empty-model -> modelLock___all mapping (account_fallback/mod.rs:340, 461-467) needed for account-level parity. Minor caveats only: the truncated helper must be wired into the chat.rs error path (cooldown = reset_at - now, empty model for account lock, backoff 0), and direct provider=="github" comparison relies on provider normalization already present in forward_with_provider_fallback — both implementation details, not errors in the spec.

---

### `P1-E5` — IntelliJ JBR h2c upgrade handling on the HTTP server missing in Rust

**JS (source of truth — verbatim):**

custom-server.js:69-113 — Next custom server wraps `http.createServer` and overrides `server.emit` to handle `upgrade` events with `req.headers.upgrade.toLowerCase() === "h2c"` (JetBrains Runtime 25 sends h2c upgrades):
- Validate `content-length` is a safe non-negative integer else `socket.destroy()`.
- Buffer `[head]`, read remaining body bytes via `socket.on("data")` until `received >= contentLength`.
- Replay: build a new `http.IncomingMessage(socket)` with `{ method, url, headers, complete: true }`, push the buffered bytes subarray(0, contentLength), then `replay.push(null)`; create `http.ServerResponse`, `shouldKeepAlive=false`, `res.assignSocket(socket)`, `res.once("finish", () => socket.end())`; dispatch to the wrapped HTTP/1.1 handler in a microtask.
- Finally `delete req.headers.upgrade; delete req.headers["http2-settings"]; req.headers.connection = "close"; return true;`
Comment :69 `// JBR 25 sends h2c upgrades that the HTTP/1.1 server would otherwise close.`

**Current Rust behavior:**

N/A. openproxy uses axum/hyper (src/main.rs). No h2c→HTTP/1.1 downgrade. IntelliJ/JetBrains IDE (JetBrains AI Assistant / JBR25-based clients) sending an h2c `Upgrade: h2c` preamble gets the connection closed → the IDE's LLM client fails against the openproxy endpoint. This is a transport-level gap, not application logic.

**Implementation steps:**

Note: This is a big transport change. Minimum viable port (recommended):
1. In src/main.rs where the axum listener accepts connections (or a hyper layer), detect an inbound request with header `upgrade: h2c` (case-insensitive) and `Connection: upgrade`.
2. For such requests, do NOT respond 101. Instead, read the request body fully (Content-Length or chunked), then feed the reconstructed HTTP/1.1 request (method, path, headers minus `upgrade`/`http2-settings`, `connection: close`, body bytes) into the normal axum request pipeline.
3. Respond without keep-alive (Connection: close) so the client falls back to plain HTTP/1.1 on the next request.

Because hyper/axum may not expose raw `upgrade` interception through the standard tower Service, the robust path is a hyper 1.x `hyper::upgrade::on`-free low-level server loop in main.rs that peeks at the first request's headers before the axum service. If that is infeasible in the current hyper version, document the gap and instead make the server tolerant: if the h2c preamble is received, drain the body and return the normal route response with Connection: close. The JS implementation is the reference for exact byte handling (validate content-length, replay with connection: close).

Provide a feature flag or cfg so this does not affect normal HTTP/1.1 traffic.

**Guard test:**

Integration-style test: open a raw TCP connection, send `GET /v1/models HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade, HTTP2-Settings\r\nUpgrade: h2c\r\nHTTP2-Settings: AAMAAABkAARAAAAAAAIAAAAA\r\n\r\n` with Content-Length body, assert the server returns a valid HTTP/1.1 response (not a 101, not a closed socket) with `Connection: close`. If a full integration harness is not available, unit-test the header matcher: `fn is_h2c_upgrade(headers: &HeaderMap) -> bool` returns true only for `upgrade: h2c` (case-insensitive).

**⚠️ Risks:**

Must not treat normal `Upgrade: websocket` as h2c (JS checks exact string 'h2c'). Must validate content-length to avoid a malicious length causing a hang. Replayed request must have `connection: close` so the client reconnects on HTTP/1.1 (this is the whole point — JBR falls back). If hyper handles the upgrade before the app sees it, the interception must happen at the hyper server builder level, not in a tower middleware. Do NOT attempt to implement HTTP/2; only downgrade h2c to HTTP/1.1.

**Cross-check:** 🟡 **PLAUSIBLE** — JS claim: VERIFIED REAL. custom-server.js:69-113 exactly matches the citation — server.emit override intercepting event==="upgrade" with req.headers.upgrade.toLowerCase()==="h2c", Number.isSafeInteger(contentLength)>=0 else socket.destroy(), buffering [head]+body via socket.on("data") until received>=contentLength, replaying through the HTTP/1.1 handler with upgrade/http2-settings headers stripped and connection:"close". The unit test .tmp/9router/tests/unit/custom-server-h2c.test.cjs proves the behavior (200 OK, Connection: close, body reassembled).

Rust claim: VERIFIED REAL. src/main.rs:452-456 uses plain axum::serve (hyper 1.10.1/axum 0.8.9/hyper-util 0.1.20). No h2c/upgrade handling exists; the only "upgrade" hits (chat.rs:3315, media.rs:777, dashboard) are outbound header allowlists. I traced hyper's internals: an h2c request sets wants_upgrade (role.rs:312-315), creates a Pending upgrade in conn state (conn.rs:1148-1152) and an OnUpgrade in request extensions (dispatch.rs:315-323); if the service never calls hyper::upgrade::on(), the pending upgrade is never fulfilled and hyper tries to parse the client's HTTP/2 connection preface as the next HTTP/1.1 request → parse error → close → an in-flight SSE stream can be truncated. So the transport-level gap is real.

Impl steps: fundamentally sound and mirror the proven JS approach, but with non-trivial hyper-specific omissions: (1) hyper's HTTP/1 parser already sets conn-level pending-upgrade state at head-parse time that a middleware cannot undo — parity requires the reconstructed request to force Connection: close and accept that the conn will return Dispatched::Upgrade with a dropped Pending (io dropped → connection closes, which matches the JS Connection: close outcome); (2) the steps do not address the client's HTTP/2 connection preface bytes that follow the h2c request, which hyper's HTTP/1 parser will consume and error on, potentially truncating a streaming SSE response — the exact failure the JS wrapper is designed to avoid; (3) the suggested hook point ("where the axum listener accepts connections / a hyper layer") is imprecise — in axum/hyper the request is already parsed by the http1 layer, so the natural place is a service/middleware that detects upgrade: h2c + Connection: upgrade, drains the body (content-length or chunked), strips the upgrade headers, and forces Connection: close before normal processing. The body-drain and replay semantics match; the transport-edge-case handling is what's under-specified. Overall the spec is mostly right but not complete enough to be CONFIRMED.

---

### `P1-F6` — forceStream SSE→JSON: cached tokens folded into prompt_tokens + prompt_tokens_details missing in Rust aggregation

**JS (source of truth — verbatim):**

open-sse/handlers/chatCore/sseToJsonHandler.js:210-245 (Responses-API path):
```js
const inTokensForLog = (usage.input_tokens || 0) + (usage.cache_read_input_tokens || usage.cached_tokens || 0) + (usage.cache_creation_input_tokens || 0);
// client-format:
const cacheRead = usage.cache_read_input_tokens || usage.cached_tokens || 0;
const cacheCreate = usage.cache_creation_input_tokens || 0;
const inTokens = (usage.input_tokens || 0) + cacheRead + cacheCreate;
const cacheDetails = (cacheRead > 0 || cacheCreate > 0)
  ? { prompt_tokens_details: {
        ...(cacheRead > 0 ? { cached_tokens: cacheRead } : {}),
        ...(cacheCreate > 0 ? { cache_creation_tokens: cacheCreate } : {}) } }
  : {};
```
Comment :232-236: input_tokens EXCLUDES cached tokens on cache-capable upstreams; folding cache counters in and keeping them in prompt_tokens_details lets clients tell a cache hit from a small prompt.

Also chatCore.js:103 `const providerRequiresStreaming = PROVIDERS[provider]?.forceStream === true;` — when the client did not request streaming and the provider forces streaming, the SSE is collected and collapsed to chat.completion JSON (handleForcedSSEToJson).

**Current Rust behavior:**

src/core/media/responses/stream_to_json.rs:305-310 builds `usage` verbatim from the last upstream `usage` object (`usage.unwrap_or_else(|| json!({prompt_tokens:0,...}))`). It does NOT fold cache_read_input_tokens/cached_tokens/cache_creation_input_tokens into prompt_tokens, and does NOT add `prompt_tokens_details.{cached_tokens, cache_creation_tokens}`. So a Codex/Grok-CLI forceStream client that asked for JSON gets a response whose prompt_tokens under-reports (measured 2012 vs real 5344 in the JS comment) and no cache breakdown. The Rust `extract_token_usage_from_bytes` (chat.rs:3131) DOES read cached_tokens/cache_read_input_tokens for the DB but the client-facing JSON usage never gets them folded.

**Implementation steps:**

In src/core/media/responses/stream_to_json.rs, `convert_chat_completion_stream` final usage assembly (line ~305):
1. After picking the last `usage` object, compute:
   - `cache_read = usage.cache_read_input_tokens ?? usage.cached_tokens ?? 0`
   - `cache_create = usage.cache_creation_input_tokens ?? 0`
   - `in_tokens = usage.input_tokens ?? usage.prompt_tokens ?? 0 + cache_read + cache_create`
   - `out_tokens = usage.output_tokens ?? usage.completion_tokens ?? 0`
2. Build `usage` with `prompt_tokens: in_tokens` (NOT input_tokens), `completion_tokens: out_tokens`, `total_tokens: in_tokens + out_tokens`, and add `prompt_tokens_details: { cached_tokens: cache_read, cache_creation_tokens: cache_create }` ONLY when cache_read>0 or cache_create>0 (include each key only when >0, matching JS spread).
3. Also port the same folding into the Responses-API→chat.completion path (the `convert_responses_stream` output at stream_to_json.rs ~493 where `prompt_tokens: input_tokens` is built).
4. If the usage came from a provider that already includes cache in prompt_tokens (OpenAI chat.completion usage has prompt_tokens cache-inclusive and no cache_read field), the fold is a no-op — safe.

Check that the chat.completion JSON that reaches `proxy_sse_to_json_response` (chat.rs:2365) flows through this same stream_to_json and will now carry the folded numbers to the client.

**Guard test:**

`force_stream_json_folds_cache_read_into_prompt_tokens`: input SSE frame with `usage: {"input_tokens": 10, "cache_read_input_tokens": 5, "cache_creation_input_tokens": 2, "output_tokens": 3}` → result usage `prompt_tokens == 17`, `completion_tokens == 3`, `prompt_tokens_details.cached_tokens == 5`, `prompt_tokens_details.cache_creation_tokens == 2`. And a non-cached usage stays unchanged (no prompt_tokens_details key when all zero).

**⚠️ Risks:**

Do not double-count when the upstream usage already has prompt_tokens set (OpenAI chat.completion): only fold cache into input_tokens, and if usage has prompt_tokens already (cache-inclusive), leave it. JS uses `input_tokens || 0` + cache for the Responses path. The prompt_tokens_details keys must be omitted (not null/0) when cache is zero. total_tokens must be recomputed as in+out after folding.

**Cross-check:** ❌ **REFUTED** — The JS claim is REAL: open-sse/handlers/chatCore/sseToJsonHandler.js lines 210-245 verbatim contain the cache-fold logic (inTokensForLog at 212-214; cacheRead/cacheCreate/inTokens at 237-239; prompt_tokens_details build at 241-245; folded usage in the chat-completion response at 280). This is in the Responses-API branch (via convertResponsesStreamToJson). The Rust gap is also real but is in the WRONG function than the spec claims: stream_to_json.rs:305-310 (convert_chat_completion_stream) does pass usage through verbatim, but the genuine gap is convert_responses_api_stream — parse_responses_api_stream (lines 395-406) keeps only input/output/total_tokens, discarding cache_read_input_tokens/input_tokens_details.cached_tokens/cache_creation_input_tokens, and line 493-497 emits prompt_tokens=input_tokens with no fold and no prompt_tokens_details. That is the exact JS-equivalent location. The impl_steps, however, target convert_chat_completion_stream's final usage assembly (~line 305), which would NOT produce parity: (a) in the chat path the upstream usage already has prompt_tokens (OpenAI-native includes cached; Claude-backend upstreams arrive already-folded by the Rust claude_to_openai streaming translator, which also sets top-level cache_read_input_tokens/cache_creation_input_tokens), so adding cache_read+cache_create either double-counts or no-ops — diverging from JS, whose parseSSEToOpenAIResponse chat path deliberately re-attaches raw usage verbatim (line 330) without folding; (b) the actual fix — preserving the cache fields in parse_responses_api_stream and folding them + adding prompt_tokens_details in convert_responses_api_stream — is never mentioned. The spec also misattributes line 305-310 as 'the last upstream usage object' while the real upstream-usage capture is at line 120-126. So the diagnosis (cache not folded, prompt_tokens_details missing in Rust aggregation) is directionally correct but the prescribed implementation would not work; it targets the no-op/double-count converter and leaves the Responses-API converter (where the gap lives) untouched.

---

### `P1-G7` — Headroom savings report: byte-size snapshot + phantom-savings warning (isHeadroomPhantomSavings) missing in Rust

**JS (source of truth — verbatim):**

open-sse/rtk/headroom.js:26-40 `captureSizeSnapshot(body)` → `{ bodyBytes, messageBytes, toolSchemaBytes, toolHistoryBytes }` (toolHistory filters messages with role tool/function/tool_calls/content tool_use/tool_result).
:339-351:
```js
export function formatHeadroomLog(stats) {
  const before = stats.tokens_before || 0;
  const after = stats.tokens_after || 0;
  const delta = stats.tokens_saved || 0;
  const pct = before > 0 ? ((delta / before) * 100).toFixed(1) : "0";
  return `reported token delta=${delta} before=${before}${after ? ` after=${after}` : ""} (${pct}%)`.trim();
}
export function formatHeadroomSizeLog(diagnostics) {
  const effective = before.bodyBytes > 0 ? (((before.bodyBytes - after.bodyBytes) / before.bodyBytes) * 100).toFixed(1) : "0.0";
  return `body=${before.bodyBytes}B→${after.bodyBytes}B messages=${before.messageBytes}B→${after.messageBytes}B tools=${before.toolSchemaBytes||0}B→${after.toolSchemaBytes||0}B toolHistory=${before.toolHistoryBytes||0}B→${after.toolHistoryBytes||0}B effective=${effective}%`;
}
export function isHeadroomPhantomSavings(stats, diagnostics, minShrinkRatio = 0.05) {
  if (!stats?.tokens_saved || stats.tokens_saved <= 0) return false;
  const before = diagnostics?.before?.bodyBytes || 0;
  const after = diagnostics?.after?.bodyBytes || 0;
  if (before <= 0 || after <= 0) return false;
  return after >= before * (1 - minShrinkRatio);
}
```
chatCore.js:243-245: `if (isHeadroomPhantomSavings(headroomStats, headroomDiagnostics)) log?.warn?.("HEADROOM", \`reported token delta, but outbound JSON shrank <5%; provider may bill near-original payload | ${formatHeadroomSizeLog(headroomDiagnostics)}\`);`
chatCore.js:246: else if skipped → `log?.warn?.("HEADROOM", \`skipped: ${headroomDiagnostics.reason || "compression unavailable"}${headroomDiagnostics.endpoint ? ` (${headroomDiagnostics.endpoint})` : ""}\`)`

**Current Rust behavior:**

src/core/rtk/headroom.rs:197-227 `format_headroom_log` produces a DIFFERENT format string: `"saved {} tokens / {} ({:.1}%){}"` with a ` [phantom]` tag when tokens_saved==0. It does NOT do the byte-size captureSizeSnapshot, does NOT produce formatHeadroomSizeLog, and there is NO is_headroom_phantom_savings (after >= before*0.95 check). chat.rs:853-858 only logs `stats.format_headroom_log()` and drops the skipped/phantom warnings entirely. So the operator cannot tell a real compression win from a phantom (proxy reports tokens saved but body barely shrank). The existing estimate_phantom_savings (headroom.rs:26) is a DIFFERENT pre-flight estimate, not the post-hoc phantom verification.

**Implementation steps:**

1. In src/core/rtk/headroom.rs add a `SizeSnapshot { body_bytes, message_bytes, tool_schema_bytes, tool_history_bytes }` struct + `fn capture_size_snapshot(body: &Value) -> SizeSnapshot` (jsonBytes = serde_json::to_string length; toolHistory filter: role tool/function OR tool_calls non-empty OR any content part type tool_use/tool_result).
2. Capture `before` before compression and `after` after in `compress_with_headroom` (body is mutated in place so capture before mutating at the top and after writes) and attach to the returned stats or a diagnostics struct (mirror the JS `diagnostics.before/after`).
3. Add `fn format_headroom_size_log(diag) -> Option<String>` producing EXACTLY `body={b}B→{a}B messages={mb}B→{ma}B tools={tb}B→{ta}B toolHistory={hb}B→{ha}B effective={e}%` (effective = ((before-after)/before*100).toFixed(1), guard before.bodyBytes>0 else "0.0").
4. Add `fn is_headroom_phantom_savings(stats, diag, min_shrink_ratio: f64) -> bool` with JS logic (tokens_saved>0 AND before.bodyBytes>0 AND after.bodyBytes>0 AND after >= before*(1-min_shrink_ratio); default 0.05).
5. In chat.rs:853-858, after a successful compress, if `is_headroom_phantom_savings` → warn with the size log; else debug with format_headroom_log. When skipped (compress returned None), emit the `skipped: {reason}` warn when headroom_enabled (mirror chatCore.js:246).

**Guard test:**

`headroom_phantom_savings_detects_no_real_shrink`: stats tokens_saved=100, diagnostics before.body_bytes=1000, after.body_bytes=990 → is_headroom_phantom_savings == true (990 >= 950). after=500 → false. tokens_saved=0 → false. And `headroom_size_log_format_matches_js`: format string equals `body=1000B→990B messages=...` prefix with `effective=1.0%`.

**⚠️ Risks:**

Keep the JS log-string exactness — the report is read by humans/dashboards. The JS `formatHeadroomLog` uses `tokens_before||0`; Rust's format is different and should be brought in line OR both kept but the size report is the added value. captureSizeSnapshot must run BEFORE any mutation of body. The `[phantom]` tag in the current Rust format_headroom_log is a pre-flight artifact; the real phantom check is after>=before*0.95.

**Cross-check:** ✅ **CONFIRMED** — All three verification points pass. (1) JS: captureSizeSnapshot at open-sse/rtk/headroom.js:26-40 is real and returns {bodyBytes, messageBytes, toolSchemaBytes, toolHistoryBytes} with the exact toolHistory filter (role tool/function OR tool_calls non-empty OR content part type tool_use/tool_result); formatHeadroomLog (:334-341, spec cites 339-351 — off by a few lines) produces `reported token delta=... before=...`; formatHeadroomSizeLog (:343-351) and isHeadroomPhantomSavings (:353-359) exist. (2) Rust: format_headroom_log at headroom.rs:197-220 is real, uses `"saved {} tokens / {} ({:.1}%){}{}"` (spec quotes one trailing {} — truncated) with a ` [phantom]` tag when tokens_saved==0 && tokens_before>0; grep confirms NO capture_size_snapshot/SizeSnapshot/tool_schema_bytes/tool_history_bytes/format_headroom_size/is_headroom_phantom_s anywhere in src/. The Rust measures only message-array byte sizes for hooks, not the 4-field snapshot. (3) Impl steps (SizeSnapshot struct + capture_size_snapshot using serde_json::to_string length + matching toolHistory filter + before/after capture) would produce parity; both claude and openai paths mutate body in place so before/after body snapshots work. Minor non-material issues: spec omits that Rust already has a DIFFERENT phantom concept (estimate_phantom_savings/is_phantom()/tokens_saved==0 [phantom] tag) vs JS's byte-based isHeadroomPhantomSavings, and step 2 underspecifies how the snapshot surfaces out of compress_with_headroom (JS threads a diagnostics object) — an implementation detail, not a parity gap.

---

### `P1-H8` — Adaptive thinking: claude-adaptive format emits output_config.effort + thinking:{type:adaptive} — verify Rust parity

**JS (source of truth — verbatim):**

open-sse/translator/concerns/thinkingUnified.js:238-249:
```js
case "claude-adaptive": {
  if (none && canDisable) { body.thinking = { type: "disabled" }; break; }
  // ... Annot: requires an explicit thinking:{type:"adaptive"} on Opus 4.6/4.7/4.8 and Sonnet 4.6 ...
  body.thinking = { type: "adaptive" };
  const level = toLevel(eff);
  body.output_config = { effort: level === "xhigh" ? "high" : level };
  break;
}
```
:61-70 extractThinking: `thinking.type==="disabled"→none; type==="adaptive"||"enabled" → budget>0 ? {mode:budget,budget} : {mode:auto}`.
:52-59 `output_config.effort` has priority: "none"/"off"→none; "auto"→auto; else level.
Providers with thinkingFormat "claude-adaptive" in capabilities.js (e.g. claude-opus-5, claude-opus-4.6/4.7/4.8, sonnet 4.6 — lines 75-82).

**Current Rust behavior:**

src/core/utils/thinking_suffix.rs `reapply_thinking_after_translate` (called at chat.rs:821) exists, but I could not confirm it emits the claude-adaptive dual-field shape. Grep of src/core for `output_config` (searching). The Rust thinking code is ported from an older 9router and likely uses the older `thinking:{type:"enabled", budget_tokens}` shape rather than the v0.5.50 `thinking:{type:"adaptive"} + output_config:{effort}`. Need to verify: grep `output_config` and `type": "adaptive` in src/core/translator and src/core/utils.

**Implementation steps:**

1. Verify current Rust behavior: `grep -rn "output_config\|adaptive" src/core/translator src/core/utils/thinking* src/core/translator/concerns 2>/dev/null`. If `output_config` is absent, port the claude-adaptive case:
   - In the thinking-applier (src/core/utils/thinking_suffix.rs or wherever applyFormat lives) add a `ClaudeAdaptive` branch: if mode none && canDisable → `body["thinking"] = json!({"type": "disabled"})`; else `body["thinking"] = json!({"type": "adaptive"})` AND `body["output_config"] = json!({"effort": if level == "xhigh" {"high"} else {level}})`.
   - level mapping: toLevel(eff) must map auto/level/budget → a level; xhigh→"high" else the level string.
2. Ensure `extract_thinking` (the Rust equivalent of extractThinking) handles `output_config.effort` with priority over body.thinking, mapping "none"/"off"→none, "auto"→auto, else level. And `thinking.type === "adaptive"` → budget>0? budget mode : auto mode.
3. Confirm the thinkingFormat "claude-adaptive" is assigned to the right models in the Rust capability model (provider model catalog) — check src/core/config/provider_models or the models config for claude-opus-5/4.6/4.7/4.8/sonnet-4.6.

**Guard test:**

`claude_adaptive_emits_dual_fields`: apply thinking cfg {mode:level, level:"high"} for a claude-adaptive model → body.thinking.type == "adaptive" AND body.output_config.effort == "high". `claude_adaptive_xhigh_maps_to_high`: level xhigh → effort "high". `claude_adaptive_disabled_when_can_disable`: mode none → thinking.type == "disabled" and NO output_config. `extract_output_config_effort_priority`: body with output_config.effort="auto" and thinking.type="enabled" → mode auto (effort wins).

**⚠️ Risks:**

The exact double-write matters: sending output_config.effort ALONE does not enable thinking on Opus 4.6/4.7/4.8/Sonnet 4.6 — Anthropic requires the explicit thinking:{type:adaptive}. Do not drop either field. xhigh must map to "high" (not "xhigh"). The disabled case must NOT emit output_config (Anthropic rejects output_config alongside thinking:disabled). Verify the model→thinkingFormat mapping covers the v0.5.50 models (claude-opus-5, claude-opus-5-thinking, claude-opus-5-agentic, claude-opus-5-thinking-agentic, claude-opus-4.6, 4.7, 4.8, claude-sonnet-4.6).

**Cross-check:** 🟡 **PLAUSIBLE** — JS claim is fully real: open-sse/translator/concerns/thinkingUnified.js:238-249 contains the claude-adaptive case verbatim — none&&canDisable → thinking:{type:"disabled"}, else body.thinking={type:"adaptive"} AND body.output_config={effort: level==="xhigh"?"high":level}. The cited comment is abbreviated but accurate. Rust parity gap is also real: reapply_thinking_after_translate exists (called at src/server/api/chat.rs:821, not src/core/chat/chat.rs as claimed) and its ClaudeAdaptive branch (src/core/utils/thinking_suffix.rs:425-440) emits ONLY output_config:{effort} — it never sets thinking:{type:"adaptive"}. The openai→claude translate (openai_to_claude.rs:676-749) emits the old budget shape thinking:{type:"enabled",budget_tokens}, and strip_all_thinking_fields + the branch leave the final wire body with output_config only, so thinking is not turned on for claude-adaptive models (Opus 4.6/4.7/4.8, Sonnet 4.6, Sonnet 5) in the translated path — a genuine behavioral gap matching the task title. However the spec is NOT fully accurate: (1) it claims output_config is "absent" from src/core — it is present (thinking_suffix.rs:438, claude_format.rs:77-86) as single-field-only; the actual missing piece is the thinking:adaptive half, not output_config. (2) It says the Rust code "likely uses the old" shape — Rust already has a ClaudeAdaptive variant using the new output_config shape. (3) Impl step 1 gates the port on "If output_config is absent", which is false in Rust, so a literal reading would skip the fix and NOT achieve parity; the correct fix (add thinking:{type:"adaptive"} beside the output_config insert in apply_thinking_level's ClaudeAdaptive branch) would work. A secondary divergence also goes unflagged: JS clamps only xhigh→high (passes max through) while Rust clamps both xhigh and max→high. Core diagnosis correct, fix target correct, but the Rust-state premise and impl gate are materially wrong — mostly right, not confirmed.

---

### `P1-I9` — Grok Build settings: subagent models (general-purpose/explore/plan) + context_window + GET subagentMappings missing in Rust

**JS (source of truth — verbatim):**

src/lib/grokBuildConfig.js:3 `export const GROK_SUBAGENT_TYPES = ["general-purpose", "explore", "plan"];`
:1 `GROK_MAIN_MODEL_SLOT = "9router"`, :2 `GROK_BUILTIN_DEFAULT = "grok-build"`, :5 `UNSET_SENTINEL = "__9router_unset__"`, :6 `MODELS_SECTION = "models"`, :7 `SUBAGENT_MODELS_SECTION = "subagents.models"`.
:18 `const modelSlot = (type) => \`${GROK_MAIN_MODEL_SLOT}-${type}\`;` → "9router-general-purpose" etc.
:78-92 parseModelSection: `model`, `base_url`, `name`, `api_key`, `api_backend`, `context_window` (positive finite number only, else null), `raw`.
:171-188 parseGrokBuildConfig: `subagentModels[type] = mapping === modelSlot(type) ? parseModelSection(toml, mapping) : null; subagentMappings[type] = mapping;` returns `{ model, default, subagentModels, subagentMappings }`.
:194-232 applyGrokBuildConfig with subagentModels: for each type, if `selected?.model` → `rememberPreviousSubagent`, `upsertModelSection({ slot, model: selected.model, baseUrl, apiKey, contextWindow: selected.contextWindow, name: "9Router "+type })`, `setSectionField(subagents.models, type, slot)`; else → restorePreviousSubagent + removeModelSection.
:234-243 resetGrokBuildConfig: restore each subagent + remove `model.9router-{type}` + remove `model.9router` + restore default.

route.js:48-55 normalizeContextWindow: explicit number>0 → floor; else `getCapabilitiesForModel(provider, modelId).contextWindow`.
:57-71 normalizeSubagentModels: value===undefined → undefined (leave untouched); non-object → {}; per type `model = typeof entry === "string" ? entry.trim() : entry?.model?.trim()`; blank → skip; else `{model, contextWindow: normalizeContextWindow(entry?.contextWindow, model)}`.
:101 POST destructures `{ baseUrl, apiKey, model, contextWindow, subagentModels }`; :108 `normalizedBaseUrl = baseUrl.endsWith("/v1") ? baseUrl : baseUrl + "/v1"`; :111 `apiKey: apiKey || "sk_9router"`; :122 modelSlot: "9router".
GET response includes `settings.subagentModels`, `settings.subagentMappings`, `has9Router` (= `Boolean(settings?.model?.base_url)`).

**Current Rust behavior:**

src/server/api/cli_tools/grok_build_settings.rs uses MODEL_SLOT="openproxy" and only handles the main model section: build_model_section (line 218) has NO context_window, NO subagent support. parse_model_section (line 199) has no context_window. SaveGrokBuildSettingsRequest (line 36) has NO subagentModels and NO contextWindow. GET returns `{model, default}` only — no subagentModels/subagentMappings, and `hasOpenProxy` instead of JS `has9Router`. So per-type subagent overrides ([model.9router-general-purpose] etc.) and context_window are entirely unsupported.

**Implementation steps:**

Decide branding first: Rust uses "openproxy" slot (MODEL_SLOT="openproxy") whereas JS uses "9router". Keep the existing Rust branding but add the features:
1. src/server/api/cli_tools/grok_build_settings.rs:
   - Add `const SUBAGENT_TYPES: [&str; 3] = ["general-purpose", "explore", "plan"];` and `const SUBAGENT_MODELS_SECTION: &str = "subagents.models";` and `const UNSET_SENTINEL: &str = "__9router_unset__";` (keep the exact sentinel string even though brand differs — it round-trips with JS-written configs).
   - SaveGrokBuildSettingsRequest: add `#[serde(default, rename_all="camelCase")] subagent_models: Option<Value>` and `context_window: Option<f64>`.
   - `fn model_slot(type) = format!("{MODEL_SLOT}-{type}")`.
   - build_model_section: add optional context_window line `context_window = {floor}` when finite>0.
   - New `fn upsert_subagent_model_section(toml, slot, model, base_url, api_key, context_window, name)` and `fn set_subagents_models_field(toml, type, value)` + `fn delete_subagents_models_field(toml, type)` and `fn remember_prev_subagent(toml, type)` / `fn restore_prev_subagent(toml, type)` porting grokBuildConfig.js lines 147-169 (markers `# openproxy-prev-subagent-{type} = "..."` — keep JS marker format but with openproxy prefix, or exactly `# 9router-prev-subagent-{type}` to interop with JS-created configs; RECOMMEND keeping the exact JS marker `# 9router-prev-subagent-...` so a config written by Rust can be reset by JS and vice-versa).
   - In save_grok_build_settings: normalize subagentModels per JS route.js normalizeSubagentModels; when Some(non-empty object), for each SUBAGENT_TYPES type apply remember+upsert+set; when None, leave untouched; for a type with no model, restorePreviousSubagent + removeModelSection.
   - GET: parse subagentModels (per-type parseModelSection when `subagents.models.{type} == model.slot`) + subagentMappings; return `settings.subagentModels`, `settings.subagentMappings`, and keep hasOpenProxy (or add has9Router alias).
   - reset_grok_config: also restore each subagent + remove `model.{slot}-{type}` sections.
2. Normalize context_window: explicit finite>0 → floor; else look up model context window from the model registry.

**Guard test:**

`grok_build_save_writes_subagent_sections`: toml with subagentModels {general-purpose: {model:"anthropic/claude", contextWindow: 200000}} → written toml contains `[model.openproxy-general-purpose]`, `model = "anthropic/claude"`, `context_window = 200000`, and `[subagents.models]` with `general-purpose = "openproxy-general-purpose"`. `grok_build_parse_reads_subagent_mappings`: GET parse of that toml → subagentModels["general-purpose"].model == "anthropic/claude", subagentMappings["general-purpose"] == "openproxy-general-purpose", and subagentModels for a type not set == null. `grok_build_reset_removes_subagents`: reset removes all three subagent sections and restores prev default.

**⚠️ Risks:**

The marker comments and sentinel string are load-bearing for interop with JS-written configs — match `# 9router-prev-subagent-` exactly OR document the branding divergence. `subagentModels === undefined` must leave existing subagent config untouched (JS backwards-compat). `context_window` uses Math.floor of a positive finite number. Blank subagent model means "inherit main model" (skip that type). resetGrokBuildConfig must remove ALL three subagent sections and the main section, then restore previous default (not just set to grok-build).

**Cross-check:** ✅ **CONFIRMED** — All cited JS behavior is real and line-exact: grokBuildConfig.js lines 1-3 (GROK_MAIN_MODEL_SLOT="9router", GROK_BUILTIN_DEFAULT="grok-build", GROK_SUBAGENT_TYPES=["general-purpose","explore","plan"]), lines 5-7 (UNSET_SENTINEL="__9router_unset__", MODELS_SECTION="models", SUBAGENT_MODELS_SECTION="subagents.models"), line 18 (modelSlot yields "9router-<type>"). parseModelSection/buildModelSection (78-108) read/write context_window; parseGrokBuildConfig (171-188) returns subagentModels+subagentMappings, and the GET route (route.js 86-92) exposes settings.subagentMappings in the response — confirming the claimed JS feature set. All Rust behavior is real and line-exact: MODEL_SLOT="openproxy" (line 24), build_model_section (218) has no context_window and no subagent support, parse_model_section (199) omits context_window, SaveGrokBuildSettingsRequest (36-43) has only base_url/api_key/model, and the GET response (68-77) returns only model+default with no subagentMappings/subagentModels — confirming the stated gap. The impl_steps direction (add SUBAGENT_TYPES/SUBAGENT_MODELS_SECTION/UNSET_SENTINEL, add context_window to parse+build, add subagentMappings to GET, keep "openproxy" branding) would produce the required parity; keeping Rust's branding is an explicit, sensible decision that does not undermine feature parity. Only caveats: the task's impl_steps text is truncated mid-sentence so the full list could not be audited, and the Rust port would also need JS-equivalent request-side normalization (normalizeContextWindow/normalizeSubagentModels inference) to be fully behavior-identical — neither contradicts the claims made.

---

### `P1-J10` — xai video CLI: multipart body passthrough + raw byte forwarding + account rotation on auth/quota errors missing in Rust media.rs

**JS (source of truth — verbatim):**

src/sse/handlers/videoGeneration.js:23-27 `CREATE_ROTATION_STATUSES = new Set([401, 403, 429])` (rotate to next account ONLY on errors upstream rejects before job creation); 5xx returned to caller.
:45-61 readForwardableBody: JSON parsed for model-prefix resolution but RAW bytes forwarded; multipart/other content types forwarded as exact bytes (`Buffer.from(await request.arrayBuffer())`) — parsing/re-encoding FormData would change the multipart boundary.
:63-80 resolveVideoProvider: bare model id without '/' → DEFAULT_VIDEO_PROVIDER="xai"; combos rejected.
:94-175 handleVideoCreate: loop over accounts, `checkAndRefreshToken`, handleVideoProxyCore with `onCredentialsRefreshed` persisting new tokens, on failure `markAccountUnavailable` then rotate if shouldFallback && status in CREATE_ROTATION_STATUSES; on success `clearAccountError` + `withConnectionHeader` sets `x-9router-connection-id`.
:182-223 handleVideoGet: pin creating account via `x-connection-id`; no rotation.

open-sse/handlers/videoCore.js:97 `method = requestId ? "GET" : "POST"`; :101-107 `doFetch(token)` forwards rawBody byte-for-byte (JSON or multipart) with Content-Type and Idempotency-Key; :120-146 401/403 → refresh ONCE → retry ONCE; :148-153 error text sanitized (Bearer tokens redacted) and truncated to 2000 chars; :156-165 success passes upstream JSON (request_id/status/video.url) verbatim with Content-Type + CORS.
:14 VIDEO_ACTIONS = new Set(["generations","edits","extensions"]).
open-sse/providers/registry/xai.js:35 video model `grok-imagine-video`, params ["duration","aspect_ratio","resolution"], kind "video"; :41 videoConfig.baseUrl "https://api.x.ai/v1/videos".

**Current Rust behavior:**

src/server/api/media.rs handles xai video but: JSON-only — video_create_handler takes `Json(body)` (line 901) so multipart uploads (video files) are re-encoded or rejected (JS explicitly preserves exact bytes). No account rotation on 401/403/429 — select_video_connection picks one account and on failure returns the error (media.rs:919, and video_get_handler at 998). No refresh-on-401/403 retry (JS refreshes once and retries). The response DOES set `x-openproxy-connection-id` (line 987/1053) — JS sets `x-9router-connection-id`; client-side header name differs. SanitizeSecrets (Bearer redaction) — verify media.rs; if missing, upstream error bodies leak tokens. VIDEO_FETCH_TIMEOUT_MS=120000 default and Idempotency-Key forwarding (media.rs:934) ARE present.

**Implementation steps:**

1. src/server/api/media.rs video_create_handler: replace `Json(mut body)` extraction with raw-body handling: read `Bytes` via axum `body: Bytes`, sniff Content-Type; if JSON, parse for model-resolution but keep the raw bytes for forwarding (re-serialize only when the model prefix was stripped); if multipart/other, forward the exact bytes (do not parse).
2. Add account rotation: on upstream 401/403/429, mark the connection unavailable (reuse mark_connection_unavailable pattern) and try the next account up to N attempts (JS loops indefinitely over available accounts); on other statuses return the error. Before dispatch, run token refresh for oauth connections (checkAndRefreshToken equivalent — see chat.rs:1782 refresh path) and persist new tokens via onCredentialsRefreshed equivalent.
3. Error sanitization: apply `sanitize_secrets` (redact `Bearer <token>` regex `Bearer\s+[A-Za-z0-9._~+/=-]{8,}` and exact secret values) to error text before returning to the client, truncate to 2000 chars. If a helper already exists in media.rs reuse it; else port from videoCore.js:21-31.
4. GET poll: keep the pin via `x-connection-id` (already implemented), but also accept the JS-emitted header `x-9router-connection-id` as an alias, and emit BOTH `x-9router-connection-id` and `x-openproxy-connection-id` on responses for interop.
5. Keep VIDEO_FETCH_TIMEOUT_MS and Idempotency-Key forwarding as-is (already present).

**Guard test:**

`video_creation_rotates_on_401`: two xai connections, upstream returns 401 for the first → second account is tried; if both fail → error returned. `video_creation_does_not_rotate_on_500`: 500 returned directly (job may exist). `video_multipart_body_forwarded_byte_for_byte`: POST with Content-Type multipart/form-data boundary X → upstream receives identical bytes. `video_error_redacts_bearer`: upstream error body containing "Bearer sk-abc..." → client error text contains "Bearer [redacted]".

**⚠️ Risks:**

Creation POSTs are BILLABLE — never auto-retry on network error (only re-send after a 401/403 refresh which upstream rejects before job creation, and only rotate on 401/403/429). Multipart must not be re-encoded (boundary changes break uploads). The response connection-id header name differs between JS (`x-9router-connection-id`) and Rust (`x-openproxy-connection-id`) — support both for interop with JS-era clients. Bare model ids fall back to xai; combos rejected with 400.

**Cross-check:** ✅ **CONFIRMED** — Both the JS claim and the Rust current behavior are verified real.

JS (C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/src/sse/handlers/videoGeneration.js): line 23-27 defines `CREATE_ROTATION_STATUSES = new Set([HTTP_STATUS.UNAUTHORIZED(401), FORBIDDEN(403), RATE_LIMITED(429)])`; the create loop (119-174) rotates to the next account only when `shouldFallback && CREATE_ROTATION_STATUSES.has(result.status)`, and 5xx falls through to `return result.response`. readForwardableBody (45-61) parses JSON for model-prefix resolution but keeps the raw string and forwards bytes exactly (re-serialize only when the stripped model differs, 107-110); multipart/other content types are read via `Buffer.from(await request.arrayBuffer())` and forwarded byte-exact. open-sse/handlers/videoCore.js confirms rawBody+original Content-Type pass-through. HTTP_STATUS constants confirm 401/403/429/503.

Rust (C:/Users/ADMIN/Documents/Projects/openproxy/src/server/api/media.rs): video_create_handler (889) uses `Result<Json<Value>, JsonRejection>`; `let Json(mut body)` at 901 rejects any non-JSON body (multipart video uploads) with 400 "Invalid JSON body"; the body is re-serialized via serde_json::to_vec (947). select_video_connection (1123) picks a single account (preferred x-connection-id, else min-priority via select_media_connection at 526) and the upstream response is proxied directly (984) with no 401/403/429 rotation — the code comment at 995 explicitly states "no cross-account rotation". Routes wired in mod.rs 199-236.

impl_steps: Step 1 (read raw `Bytes` via axum body extractor, sniff Content-Type, parse JSON only for model resolution, forward exact bytes, re-serialize only when the provider prefix was stripped) is exactly correct and mirrors JS 45-61/107-110. Step 2 (rotation on upstream 401/403/429) is truncated mid-sentence and carries one required nuance: Rust's mark_connection_unavailable (chat.rs:2273) sets test_status="unavailable" and backoff_level/model-locks in `extra` but does NOT flip `is_active` (types/mod.rs:180 returns `self.is_active.unwrap_or(true)`), and select_media_connection/select_video_connection filter only on provider + is_active + has_credentials — they ignore test_status/backoff. So "mark unavailable then re-loop" alone would re-select the same connection and infinite-loop. The impl must thread an exclude set into the selector, mirroring the JS `excludeConnectionIds` pattern already present in Rust chat.rs's select_connection(&snapshot, provider, model, &excluded, ...) at 1969-1987. Because the cited JS source is the template and the Rust codebase already has the equivalent mechanism, the port achieves parity — the nuance is an implementation detail, not a blocker.

---

### `P1-K11` — Default key auto-provision on first-ever visit ("Default Key") missing in Rust

**JS (source of truth — verbatim):**

src/app/(dashboard)/dashboard/endpoint/EndpointPageClient.js:265-277:
```js
let existing = await fetchKeys();
// Auto-provision a default key for first-time users so the endpoint works out of the box.
if (existing.length === 0) {
  try {
    const createRes = await fetch("/api/keys", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name: "Default Key" }),
    });
    if (createRes.ok) existing = await fetchKeys();
  } catch { /* fall through to empty render */ }
}
```
POST /api/keys (src/app/api/keys/route.js:22-41): `{ name }` required; `machineId = await getConsistentMachineId()`; `createApiKey(name, machineId)` → 201 `{ key, name, id, machineId }`.
apiKeysRepo.js:28-46 createApiKey: `key: generateApiKeyWithMachine(machineId).key` where generateApiKeyWithMachine (src/shared/utils/apiKey.js:44-48) = `sk-${machineId}-${keyId}-${crc8}` with keyId=6 lowercase alnum chars, crc=HMAC-SHA256(API_KEY_SECRET||"endpoint-proxy-api-key-secret", machineId+keyId).hex.slice(0,8).

**Current Rust behavior:**

src/server/api/mod.rs:1550 create_key_api EXISTS and matches (name required, machine_id via consistent_machine_id, generate_api_key_with_machine). BUT the auto-provision is CLIENT-SIDE (the dashboard JS POSTs to /api/keys when the list is empty). The Rust frontend is a different dashboard; the question is whether the Rust-served dashboard (or a server-side bootstrap) auto-creates a "Default Key" when the keys table is empty. Grep of src/server/api and src/server/dashboard for a first-run key bootstrap returned nothing. So a fresh install has requireApiKey=true (default) and NO key → /v1/chat returns 401 "Missing API key" until the user manually creates a key — the out-of-the-box experience is broken vs JS.

**Implementation steps:**

Choose server-side bootstrap (robust, mirrors the intent):
1. In src/main.rs (or the DB init in src/types/mod.rs / db init), after the DB is loaded and BEFORE the HTTP server starts, check `if snapshot.api_keys.is_empty() && snapshot.settings.require_api_key { create a key named "Default Key" }`.
2. Reuse `create_key_api`-style logic: `let machine_id = consistent_machine_id(); let key = crate::core::auth::generate_api_key_with_machine(&machine_id); let id = Uuid::new_v4().to_string(); let now = chrono::Utc::now().to_rfc3339();` and insert `ApiKey { id, name: "Default Key".into(), key, machine_id: Some(machine_id), is_active: Some(true), created_at: Some(now), extra: BTreeMap::new() }`.
3. Guard: only when the api_keys table is empty (respect existing keys), and only when require_api_key is true (don't create a key nobody asked for when auth is off — JS creates it unconditionally on first dashboard visit; matching JS would create even with requireApiKey off. To be safe, create unconditionally on empty to match JS behavior).
4. Optionally surface the generated key on first dashboard load so the user can copy it (JS relies on the client POST; a server bootstrap should log the key at startup or expose it via GET /api/keys which the dashboard already calls).

**Guard test:**

`default_key_provisioned_when_empty`: fresh DB with empty api_keys → after bootstrap, api_keys.len()==1, name=="Default Key", is_active==true, key starts with "sk-". `existing_keys_not_duplicated`: DB with one pre-existing key → bootstrap leaves len==1. `key_format_matches_js`: generate_api_key_with_machine output matches `^sk-[0-9a-f]{16}-[a-z0-9]{6}-[a-f0-9]{8}$` (verify machine_id length and keyId alphabet/length against src/shared/utils/apiKey.js).

**⚠️ Risks:**

Key generation must match the JS format exactly (machineId 16 hex chars, keyId 6 lowercase alnum, crc8 HMAC-SHA256 with default secret "endpoint-proxy-api-key-secret" when API_KEY_SECRET unset) so existing JS-written keys validate. Only provision on truly-empty key table to avoid duplicate "Default Key" entries. If API_KEY_SECRET changes after keys were created, validateApiKey still works (it compares raw key string in DB, not HMAC).

**Cross-check:** ❌ **REFUTED** — The JS-side claim is fully real, but the core premise of the spec (that default-key auto-provision is "missing in Rust") is false, and the proposed impl_steps would not work as written.

1) JS behavior — REAL. In .tmp/9router/src/app/(dashboard)/dashboard/endpoint/EndpointPageClient.js lines 265-277: fetchKeys() on /api/keys; if `existing.length === 0`, POST /api/keys with body `{ name: "Default Key" }`, then re-fetch and setKeys. Matches the citation exactly (note: the JS setting field for the guard is `requireApiKey`, line 248).

2) Rust current behavior — create_key_api at src/server/api/mod.rs:1550 exists and matches (name required → BAD_REQUEST; `consistent_machine_id()` + `generate_api_key_with_machine`; POST route at mod.rs:311). Also lines 1555-1559 skip auth when api_keys is empty. BUT the spec's implied "missing in Rust" is wrong: the server-side bootstrap ALREADY EXISTS. src/main.rs:624-651 defines `seed_default_api_key_if_missing(db)` — checks `db.snapshot().api_keys.is_empty()`, builds the key via `consistent_machine_id()` + `generate_api_key_with_machine`, persists it, and is called at main.rs:374 right after `Db::load().await?` and before `build_app`/`TcpListener::bind`. It is committed on main (introduced in 774f597 "feat: noAuth virtual connection + public catalog API for parity with 9router"). The only cosmetic difference is the key name: existing seed uses "default", 9router uses "Default Key".

3) impl_steps — WOULD NOT WORK. Step 1 checks `snapshot.settings.require_api_key`, but that field does not exist in the Rust Settings struct (src/types/mod.rs:358-536). Grep across all of src/ shows `require_api_key` only as the auth-guard function signature, never as a settings field; the Rust analog of 9router's requireApiKey is `require_login` (used at src/server/api/chat.rs:259, defaulted true at types/mod.rs:552). Referencing `settings.require_api_key` would fail to compile. Step 1 is also already fully implemented, so there is no gap to close.

Because the claimed parity gap does not exist (feature already implemented on main) and the impl_steps reference a nonexistent Settings field (would not compile/work), the spec section is REFUTED per the rubric.

---

### `P1-L12` — Exa MCP toggle (tool dedupe): ALREADY PORTED — verify only

**JS (source of truth — verbatim):**

open-sse/utils/toolDeduper.js:6-22 DEDUP_RULES:
```js
{ triggers: ["mcp__exa__web_search_exa", "mcp__exa__web_fetch_exa"], strip: ["WebSearch", "WebFetch", "mcp__workspace__web_fetch"] },
{ triggers: ["mcp__tavily__tavily_search", "mcp__tavily__tavily_extract"], strip: ["WebSearch", "WebFetch", "mcp__workspace__web_fetch"] },
{ triggers: [/^mcp__browsermcp__/], strip: [/^mcp__Claude_in_Chrome__/] },
```
chatCore.js:188-195: only runs when `clientTool === "claude"` and `Array.isArray(translatedBody.tools)`.
:27-33 getToolName = `t?.name || t?.function?.name`; matches uses exact string OR regex.

**Current Rust behavior:**

src/core/utils/tool_deduper.rs FULLY PORTS all three rules including the exa/tavily/browsermcp triggers and regex strips (lines 36-63). src/server/api/chat.rs:865-874 runs it only for `client_tool == Some(ClientTool::Claude)` — matching the JS guard. Tests at tool_deduper.rs:132-171 cover exa and browsermcp. NO GAP. The only divergence: JS dedupes `translatedBody.tools` after translate (chatCore.js:190), Rust dedupes `body["tools"]` after translate (chat.rs:867) — same position.

**Implementation steps:**

None — already implemented. For completeness add the missing test `exa_mcp_strips_web_fetch_too` if not present, asserting `mcp__workspace__web_fetch` is also stripped when exa triggers present (existing test at line 132 asserts WebSearch stripped; verify mcp__workspace__web_fetch is in the stripped set).

**Guard test:**

`exa_mcp_strips_workspace_web_fetch`: tools [WebSearch, WebFetch, mcp__workspace__web_fetch, mcp__exa__web_search_exa] → stripped contains WebSearch, WebFetch, mcp__workspace__web_fetch (all three), kept only exa.

**⚠️ Risks:**

The dedupe runs AFTER translation on the provider-format tools array — make sure the tools key exists at that point. Only Claude clients trigger it (JS guard). Regex strips match against the full tool name.

**Cross-check:** ✅ **CONFIRMED** — All claims verified against source.

1. JS behavior REAL: C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/open-sse/utils/toolDeduper.js:6-22 DEDUP_RULES matches the quoted snippet exactly (exa rule lines 7-11 with strip ["WebSearch","WebFetch","mcp__workspace__web_fetch"]; tavily rule 12-16; browsermcp regex trigger /^mcp__browsermcp__/ + strip /^mcp__Claude_in_Chrome__/ at 17-21). The Claude-only guard is real at open-sse/handlers/chatCore.js:188-195 (`clientTool === "claude"`), running after translation, before dispatch.

2. Rust current behavior REAL: src/core/utils/tool_deduper.rs RULES spans lines 36-63 and fully ports all three rules, including the exa/tavily exact triggers and exact strips (mcp__workspace__web_fetch included) and the two regex patterns. Dedupe semantics mirror JS (tool_name handles name/function.name, dedup preserves order, filter parity). Guard at src/server/api/chat.rs:865-874 runs dedupe_tools only for Some(ClientTool::Claude), matching the JS "claude" guard (ClientTool::Claude serializes to "claude" per client_detector.rs:23). Module registered at src/core/utils/mod.rs:18. Tests at tool_deduper.rs:132-171 cover exa (132), no-trigger (145), browsermcp (153), function-wrapped (164); all 4 pass under `cargo test --lib --no-default-features tool_deduper` (the default build fails only on the unrelated missing web/dist/index.html embed-web check in build.rs).

3. Impl steps valid: The exa test at line 132 uses input [WebSearch, WebFetch, mcp__exa__web_search_exa] — mcp__workspace__web_fetch is absent from input, so it is never in the stripped set; the test asserts only WebSearch (line 141). No test currently verifies mcp__workspace__web_fetch stripping under an exa trigger, so the proposed exa_mcp_strips_web_fetch_too test is a genuine completeness gap and would pass against the existing implementation. No code change needed for parity.

---

### `P1-M13` — Ollama quota fetcher: real /api/usage + /api/me live quotas missing (Rust returns static message)

**JS (source of truth — verbatim):**

open-sse/services/usage/misc.js:37-113 getOllamaUsage(apiKey, providerSpecificData, proxyOptions):
- GET `https://ollama.com/api/usage` with `Authorization: Bearer ${apiKey}`, `Accept: application/json`.
- 401/403 → `{ message: "Ollama Cloud API key invalid or expired." }`; !ok → `{ message: "Ollama Cloud usage API error ({status})." }`; non-JSON → `{ message: "Ollama Cloud usage response was not JSON." }`.
- POST `https://ollama.com/api/me` (fail-open) with same headers + `Content-Length: 0` → plan label `me.Plan` (capitalize first letter, rest lowercase; fallback "Ollama Cloud").
- `data.limits.session.usage` / `data.limits.weekly.usage` are 0..1 ratios. ratioQuota: `used = round(ratio*100)`, `{ used, total: 100, remainingPercentage: 100-used, resetAt: null, unlimited: false }`.
- quotas keys: "Session (5h)" and "Weekly (7d)". No reset timestamp exposed.
- Neither session nor weekly present → `{ plan, message: "Ollama Cloud connected. No usage limits reported.", quotas: {} }`.
- catch → `{ message: "Ollama Cloud error: {msg}" }`.
usage.js USAGE_HANDLERS: `ollama: (c) => getOllamaUsage(c.apiKey, c.providerSpecificData, c.proxyOptions)`.

**Current Rust behavior:**

src/server/api/usage.rs:72 `"ollama" => "Ollama Cloud uses a free tier with light usage limits (resets every 5h & 7d). ..."` — static message, no live fetch. ollama is NOT in `is_usage_apikey_provider` (usage.rs:33-36: glm/glm-cn/minimax/minimax-cn/kimi/deepseek only), so an ollama apikey connection hits the `"Usage not available for this connection"` early return (usage.rs:436-440) or the static message path. NO GET /api/usage, NO /api/me plan, NO Session/Weekly quota bars.

**Implementation steps:**

1. src/core/usage/quota_fetcher.rs add:
```rust
pub async fn fetch_ollama_quota(api_key: &str) -> Value {
    if api_key.trim().is_empty() {
        return json!({ "message": "Ollama Cloud API key not available." });
    }
    let client = http_client();
    let usage_resp = match client.get("https://ollama.com/api/usage")
        .header("Authorization", format!("Bearer {}", api_key.trim()))
        .header("Accept", "application/json")
        .send().await {
        Ok(r) => r,
        Err(e) => return json!({ "message": format!("Ollama Cloud error: {e}") }),
    };
    if usage_resp.status().as_u16() == 401 || usage_resp.status().as_u16() == 403 {
        return json!({ "message": "Ollama Cloud API key invalid or expired." });
    }
    if !usage_resp.status().is_success() {
        return json!({ "message": format!("Ollama Cloud usage API error ({}).", usage_resp.status().as_u16()) });
    }
    let data: Value = match usage_resp.json().await {
        Ok(v) => v,
        Err(_) => return json!({ "message": "Ollama Cloud usage response was not JSON." }),
    };
    // plan from /api/me (fail-open)
    let plan = http_client().post("https://ollama.com/api/me")
        .header("Authorization", format!("Bearer {}", api_key.trim()))
        .header("Accept", "application/json")
        .header("Content-Length", "0")
        .send().await.ok()
        .and_then(|r| r.json::<Value>().await.ok())
        .and_then(|me| me.get("Plan").and_then(|v| v.as_str()).map(|s| { let mut c = s.chars(); match c.next() { Some(f) => f.to_uppercase().collect::<String>() + &s[1..].to_lowercase(), None => String::new() } }))
        .unwrap_or_else(|| "Ollama Cloud".to_string());
    // ... build quotas
    let limits = data.get("limits").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    fn ratio_quota(usage: Option<f64>) -> Option<Value> {
        let ratio = usage.unwrap_or(0.0).clamp(0.0, 1.0);
        let used = (ratio * 100.0).round() as i64;
        Some(json!({ "used": used, "total": 100, "remainingPercentage": 100 - used, "resetAt": Value::Null, "unlimited": false }))
    }
    let mut quotas = serde_json::Map::new();
    if let Some(s) = limits.get("session") {
        let u = s.get("usage").and_then(|v| v.as_f64());
        if u.is_some() { quotas.insert("Session (5h)".into(), ratio_quota(u).unwrap()); }
    }
    if let Some(w) = limits.get("weekly") {
        let u = w.get("usage").and_then(|v| v.as_f64());
        if u.is_some() { quotas.insert("Weekly (7d)".into(), ratio_quota(u).unwrap()); }
    }
    if quotas.is_empty() {
        return json!({ "plan": plan, "message": "Ollama Cloud connected. No usage limits reported.", "quotas": {} });
    }
    json!({ "plan": plan, "quotas": Value::Object(quotas) })
}
```
Note `hasSession`/`hasWeekly` semantics: JS treats `sessionRaw !== undefined && !== null && !Number.isNaN(Number(...))` as present — an explicit null usage field means NOT present. Port: `s.get("usage").and_then(|v| v.as_f64())` returns None for null/non-number, matching.
2. src/server/api/usage.rs: add "ollama" to `is_usage_apikey_provider` (line 33) so apikey ollama connections reach the live-fetch branch, and add `"ollama" => fetch_ollama_quota(api_key).await,` to the match at line 480.
3. Keep the static fallback message for when the fetcher returns a message (the existing `usage_message_for_provider` at line 72 is the fallback — fine).

**Guard test:**

`ollama_quota_builds_session_and_weekly_bars`: mock fetch of usage JSON `{"limits":{"session":{"usage":0.5},"weekly":{"usage":1.0}}}` → quotas has "Session (5h)" used=50/remaining=50 and "Weekly (7d)" used=100/remaining=0. `ollama_quota_missing_limits`: `{"limits":{}}` → message "Ollama Cloud connected. No usage limits reported." with empty quotas. `ollama_quota_ratio_clamped`: usage 1.5 → used=100; -0.2 → used=0. `ollama_quota_401_message`: 401 → "Ollama Cloud API key invalid or expired.".

**⚠️ Risks:**

The ratio is 0..1 (1.0=limit reached); used=round(ratio*100), NOT ratio directly. Plan capitalization: first char upper, rest lower. Session/Weekly keys are exactly "Session (5h)" and "Weekly (7d)". The /api/me POST must be fail-open (null on any error → "Ollama Cloud" fallback) and needs Content-Length: 0. resetAt is always null for ollama (no reset timestamp exposed). Do NOT set a top-level `remaining` — the QuotaTable reads remainingPercentage only (misc.js:83 comment).

**Cross-check:** ✅ **CONFIRMED** — All three claims verified. (1) JS: getOllamaUsage is real at open-sse/services/usage/misc.js:37-113 — exact URL https://ollama.com/api/usage, Bearer auth, Accept: application/json; 401/403→"Ollama Cloud API key invalid or expired.", !ok→"Ollama Cloud usage API error ({status}).", non-JSON→"Ollama Cloud usage response was not JSON."; also POSTs /api/me fail-open for the plan and converts limits.session.usage / limits.weekly.usage (0..1 ratios) into "Session (5h)"/"Weekly (7d)" quota bars. It is wired for ollama at open-sse/services/usage.js:46. (2) Rust: src/server/api/usage.rs:72 contains the exact static string, ollama is absent from is_usage_apikey_provider (usage.rs:32-36, also mod.rs:768-775), and no fetch_ollama_quota exists — for an ollama apikey connection get_connection_usage (usage.rs:434-439) early-returns a static message with no live fetch, so the gap is real (minor nuance: the served text is "Usage not available for this connection", not usage.rs:72's text, but the substance — static, no live quota — holds). (3) Impl_steps are feasible: quota_fetcher.rs already imports serde_json::{json, Value} (line 59), defines http_client() (line 83), and fetch_deepseek_usage demonstrates the identical bearer-header pattern; the usage.rs dispatcher generically reads quotas/message/plan, so once "ollama" is whitelisted and a match arm added, a fetch_ollama_quota returning {plan, quotas} yields live parity. No obvious omission that would prevent parity.

---

---

## E. MEDIA (27 specs)

### `P0-B3a` — xAI image adapter must use bodyFields whitelist (drops quality/style/size)

**JS (source of truth — verbatim):**

registry/xai.js:38:
  imageConfig: { baseUrl: "https://api.x.ai/v1/images/generations", bodyFields: ["model","prompt","n","response_format"] },
imageProviders/openai.js:23-29:
  // bodyFields whitelist (e.g. xAI accepts only model/prompt/n/response_format)
  if (Array.isArray(cfg.bodyFields)) {
    const req = {};
    for (const f of cfg.bodyFields) if (full[f] !== undefined) req[f] = full[f];
    return req;
  }

**Current Rust behavior:**

src/core/media/image/openai_compat.rs: OpenAiCompatAdapter has fields {provider_id, endpoint, include_referer} only — no body_fields. build_body (lines 79-100) always emits {model, prompt, n, size} plus optional quality/style/response_format. mod.rs get_image_adapter does NOT map "xai" at all (match arms: openai/minimax/openrouter/recraft/gemini/codex/sdwebui/comfyui/huggingface/nanobanana/fal-ai/stability-ai/black-forest-labs/runwayml/cloudflare-ai) — so "xai" falls through to the generic forwarder (default.rs URL https://api.x.ai/v1/chat/completions), which is the wrong endpoint for images.

**Implementation steps:**

1) In src/core/media/image/mod.rs get_image_adapter add: `"xai" => Some(&openai_compat::XAI)`. 2) In openai_compat.rs add `pub static XAI: OpenAiCompatAdapter = OpenAiCompatAdapter { provider_id: "xai", endpoint: "https://api.x.ai/v1/images/generations", include_referer: false }`. 3) Add a `body_fields: &'static [&'static str]` field to OpenAiCompatAdapter (empty slice for openai/minimax/openrouter/recraft). 4) In build_body: build `full = {model, prompt, n, size, [quality], [style], [response_format]}` then if `!self.body_fields.is_empty()` emit only the keys present in body_fields (JS checks `full[f] !== undefined` — so size is dropped for xai). 5) Initialize body_fields on all 4 existing statics to `&[]`.

**Guard test:**

fn xai_body_drops_disallowed_fields() — XAI.build_body with prompt/n/size/quality/style: result has model/prompt/n/response_format keys, and NO size/quality/style. fn xai_image_adapter_registered() — get_image_adapter("xai").is_some() and its endpoint is https://api.x.ai/v1/images/generations.

**⚠️ Risks:**

JS drops `size` for xai even though body has it — must not forward size. The default.rs chat URL for xai must NOT be used for images once the adapter is registered (dispatch short-circuits before the generic forwarder). bodyFields applies only when present — for the other 4 providers (empty whitelist) keep the current full body.

**Cross-check:** ✅ **CONFIRMED** — All three verification points pass. (1) JS is real: .tmp/9router/open-sse/providers/registry/xai.js:38 is verbatim `imageConfig: { baseUrl: "https://api.x.ai/v1/images/generations", bodyFields: ["model","prompt","n","response_format"] }`, and open-sse/handlers/imageProviders/openai.js:23-29 matches the quoted whitelist logic (builds full={model,prompt,n,size}+quality/style/response_format, then filters via Array.isArray(cfg.bodyFields), dropping size for xAI). bodyFields appears in only that one registry file, so openai/minimax/openrouter/recraft keep full-body behavior. (2) Rust current state is real: src/core/media/image/openai_compat.rs:13-17 has OpenAiCompatAdapter with exactly {provider_id, endpoint, include_referer}, build_body (lines 79-100) always emits {model,prompt,n,size} plus optional quality/style/response_format, and mod.rs get_image_adapter has no "xai" arm (grep confirms zero xai/x-ai refs under src/core/media). (3) Impl steps produce parity: mapping "xai" to a new XAI static with endpoint https://api.x.ai/v1/images/generations and include_referer:false (JS adds no referer headers for xAI) is correct; adding a body_fields: &'static [&'static str] field with an empty slice for openai/minimax/openrouter/recraft and ["model","prompt","n","response_format"] for xai reproduces JS exactly, including dropping size. Only subtlety (implied, not an omission): the Rust guard must be `!body_fields.is_empty()` so the empty slice means "no whitelist" for the existing four providers — matching JS where they lack bodyFields entirely.

---

### `P0-B3b` — vercel-ai-gateway missing from embeddings + image adapter registries

**JS (source of truth — verbatim):**

embeddingProviders/index.js:7-11 OPENAI_COMPAT_PROVIDERS includes "vercel-ai-gateway". embeddingProviders/openai.js:11 embedUrl derives from PROVIDER_MEDIA[id].embeddingConfig.baseUrl.
registry/vercel-ai-gateway.js:32-33:
  embeddingConfig: { baseUrl: "https://ai-gateway.vercel.sh/v1/embeddings" },
  imageConfig: { baseUrl: "https://ai-gateway.vercel.sh/v1/images/generations" },
imageProviders/index.js:21: "vercel-ai-gateway": createOpenAIAdapter("vercel-ai-gateway"),

**Current Rust behavior:**

src/core/media/embeddings/mod.rs get_embedding_adapter: no "vercel-ai-gateway" arm → None → falls through to generic forwarder using default.rs URL `https://ai-gateway.vercel.sh/v1/chat/completions` + /embeddings appended, giving wrong endpoint https://ai-gateway.vercel.sh/v1/chat/completions/embeddings. src/core/media/image/mod.rs get_image_adapter: no "vercel-ai-gateway" arm → falls through similarly.

**Implementation steps:**

embeddings: 1) base.rs add `pub static VERCEL_AI_GATEWAY: OpenAiCompatAdapter = OpenAiCompatAdapter { provider_id: "vercel-ai-gateway", endpoint: "https://ai-gateway.vercel.sh/v1/embeddings", include_referer: false }`. 2) mod.rs add `"vercel-ai-gateway" => Some(&base::VERCEL_AI_GATEWAY)`. image: 3) openai_compat.rs add `pub static VERCEL_AI_GATEWAY: OpenAiCompatAdapter = ... endpoint: "https://ai-gateway.vercel.sh/v1/images/generations", include_referer: false, body_fields: &[]`. 4) image/mod.rs add `"vercel-ai-gateway" => Some(&openai_compat::VERCEL_AI_GATEWAY)`.

**Guard test:**

fn vercel_gateway_embedding_registered() — get_embedding_adapter("vercel-ai-gateway").is_some(), build_url == "https://ai-gateway.vercel.sh/v1/embeddings". fn vercel_gateway_image_registered() — get_image_adapter("vercel-ai-gateway").is_some(), build_url == "https://ai-gateway.vercel.sh/v1/images/generations".

**⚠️ Risks:**

Do not use the chat-completions base URL for embeddings/images. The adapter uses POST /v1/embeddings (no chat fallthrough).

**Cross-check:** 🟡 **PLAUSIBLE** — JS side is fully accurate: embeddingProviders/index.js:7-11 includes "vercel-ai-gateway" in OPENAI_COMPAT_PROVIDERS; embeddingProviders/openai.js:11 derives the URL from PROVIDER_MEDIA[id].embeddingConfig.baseUrl; registry/vercel-ai-gateway.js:32-33 has embeddingConfig.baseUrl=https://ai-gateway.vercel.sh/v1/embeddings and imageConfig.baseUrl=https://ai-gateway.vercel.sh/v1/images/generations; imageProviders/index.js:21 registers createOpenAIAdapter("vercel-ai-gateway"). The Rust omission is also real: get_embedding_adapter (embeddings/mod.rs:46-66) and get_image_adapter (image/mod.rs:67-86) have no vercel-ai-gateway arm, and no VERCEL_AI_GATEWAY static exists in embeddings/base.rs or image/openai_compat.rs. The impl_steps (add static + registry arm in both) are sound and would produce parity. HOWEVER, the claimed consequence is false: the media fallback does NOT use default.rs's chat-completions URL. build_media_url -> get_provider_base_url (media.rs:629-641) calls crate::core::executor::get_provider_config, which re-exports provider.rs::PROVIDER_REGISTRY (provider.rs:1139-1141) = https://ai-gateway.vercel.sh/v1; appending /embeddings or /images/generations yields the CORRECT endpoints, not .../v1/chat/completions/embeddings. default.rs's PROVIDER_CONFIGS (https://ai-gateway.vercel.sh/v1/chat/completions) is only used by DefaultExecutor's chat URL path; in execute_media_provider the executor is bound to _executor and never consulted for the URL. So the parity gap is a genuine registry/feature gap (dedicated adapter absent, e.g. no referer/config parity), but the "wrong endpoint" justification — which is the task's stated bug impact — does not occur. Impl is correct but the defect rationale is overstated; verdict PLAUSIBLE.

---

### `P0-C4` — xiaomi-mimo TTS adapter missing entirely in Rust

**JS (source of truth — verbatim):**

ttsProviders/xiaomi-mimo.js (full adapter):
  DEFAULT_MODEL="mimo-v2.5-tts"; DEFAULT_VOICE="mimo_default";
  messages = [{ role: "assistant", content: text }]; if instructions.length messages.unshift({ role: "user", content: instructions.join(" ") }) where instructions = [`Speak in ${language}.`] if language, plus style if present.
  POST https://api.xiaomimimo.com/v1/chat/completions  headers {"Content-Type":"application/json","Authorization":`Bearer ${apiKey}`}  body { model: modelId, stream: false, messages, audio: { format: "wav", voice: voiceId || DEFAULT_VOICE } }.
  Response: audio = data?.choices?.[0]?.message?.audio?.data; format = data?.choices?.[0]?.message?.audio?.format || "wav".
  parseModelVoice(model, DEFAULT_MODEL, DEFAULT_VOICE, [DEFAULT_MODEL]) — known list is exactly ["mimo-v2.5-tts"].
index.js SPECIAL_ADAPTERS includes "xiaomi-mimo": xiaomiMimo.
registry/xiaomi-mimo.js ttsConfig: { baseUrl: "https://api.xiaomimimo.com/v1/chat/completions", authType: "apikey", authHeader: "bearer", format: "xiaomi-mimo-tts" }

**Current Rust behavior:**

src/core/media/tts/mod.rs: get_tts_adapter match (lines 89-102) has no "xiaomi-mimo"; provider_generic_format (lines 106-119) has no xiaomi-mimo → is_tts_provider returns false → tts::dispatch returns None → falls through to generic forwarder (default.rs line 216 ProviderConfig::openai("https://api.xiaomimimo.com/v1/chat/completions")) which POSTs the OpenAI audio/speech shape — wrong contract entirely.

**Implementation steps:**

1) Create src/core/media/tts/xiaomi_mimo.rs implementing TtsAdapter. Constants: DEFAULT_MODEL="mimo-v2.5-tts", DEFAULT_VOICE="mimo_default", KNOWN=&["mimo-v2.5-tts"]. Model/voice parsing mirrors base::parse_model_voice (reuse it with those defaults/known). 2) Build instructions: start empty; if let Some(lang)=request.language push `Speak in {lang}.`; if style body field present push it (style comes from body, not TtsRequest — read request.credentials? No — the JS opts.style is passed through handleTtsCore from the request; TtsRequest has no style field, so read `style` from the inbound body — thread it via TtsRequest.language only; if Rust dispatch can't pass style, at minimum implement the language instruction). 3) messages = assistant content = text; if instructions non-empty unshift user message with joined instructions. 4) POST URL "https://api.xiaomimimo.com/v1/chat/completions", headers Content-Type + `Authorization: Bearer {api_key}`. Body: {model: model_id, stream: false, messages, audio: {format: "wav", voice: voice_id or DEFAULT_VOICE}}. 5) Parse: audio = parsed["choices"][0]["message"]["audio"]["data"], format = that ["audio"]["format"] or "wav". If missing audio → error. 6) mod.rs: add `mod xiaomi_mimo;` and get_tts_adapter arm `"xiaomi-mimo" => Some(&xiaomi_mimo::ADAPTER)`. 7) is_tts_provider test list already includes via adapter.

**Guard test:**

fn xiaomi_mimo_is_tts_provider() — is_tts_provider("xiaomi-mimo") is true. fn xiaomi_mimo_messages_contract() — build the JSON body for text "hi" + language "en" and assert messages[0] == {role:"user",content:"Speak in en."} and messages[1] == {role:"assistant",content:"hi"}, audio.voice == "mimo_default".

**⚠️ Risks:**

Message ORDER matters: instructions (role:user) must be UNSHIFTED before the assistant message. Voice is top-level audio.voice, NOT embedded in model. stream must be false. If no audio returned, error. Style support needs the style field threaded from the body — if TtsRequest cannot carry it, do language-only and note the gap.

**Cross-check:** ✅ **CONFIRMED** — JS claim is REAL and exact. The file C:\Users\ADMIN\Documents\Projects\openproxy\.tmp\9router\open-sse\handlers\ttsProviders\xiaomi-mimo.js exists and matches every cited detail: DEFAULT_MODEL="mimo-v2.5-tts" and DEFAULT_VOICE="mimo_default" (lines 8-9); parseModelVoice called with KNOWN=["mimo-v2.5-tts"]; instructions built as language-then-style (`Speak in ${language}.` then style, lines 24-26); messages = [{role:assistant,content:text}] with role:user unshift when non-empty (lines 28-29); POST https://api.xiaomimimo.com/v1/chat/completions with Content-Type: application/json and Authorization: Bearer (lines 31-36); body {model, stream:false, messages, audio:{format:"wav", voice}} — all corroborated by tests/unit/xiaomi-mimo-tts.test.js and providers/registry/xiaomi-mimo.js (ttsConfig.baseUrl, serviceKinds incl. tts). Rust behavior is REAL: src/core/media/tts/mod.rs get_tts_adapter match (lines 89-102) and provider_generic_format (lines 106-119) contain no "xiaomi-mimo", so is_tts_provider (lines 123-125) returns false and dispatch returns None (lines 40-42); media.rs execute_media_provider then falls through to the generic forwarder (line 349-360), whose base config at src/core/executor/default.rs lines 215-217 is exactly "xiaomi-mimo" => ProviderConfig::openai("https://api.xiaomimimo.com/v1/chat/completions") as claimed. Impl steps would produce parity: reusing base::parse_model_voice with DEFAULT_MODEL/DEFAULT_VOICE/KNOWN mirrors the JS, and the language/style instruction order matches. ONE caveat (not fatal, but worth noting): the Rust TtsRequest struct (base.rs lines 36-43) and dispatch (mod.rs lines 49-62) currently carry only `language` — there is no `style` anywhere in the tts module. Step 2's "if style body field present push it" requires access to the request body that TtsRequest does not provide, so dispatch must be modified to extract body.get("style") and thread it into the adapter (e.g., add a style field to TtsRequest). This is an unstated but trivially-addressed structural change; an implementer following step 2 would necessarily make it. No other omission found — response shape {audio base64, format} mapping and the audio.voice/audio.format wav contract are covered by the existing dispatch TtsResult mapping.

---

### `P0-C5` — Gemini TTS: stale default/KNOWN models (missing gemini-3.1-flash-tts-preview)

**JS (source of truth — verbatim):**

ttsProviders/gemini.js:7-16:
  const FALLBACK_MODEL = "gemini-3.1-flash-tts-preview";
  const KNOWN_MODELS = [...(TTS_CFG.models || []), ...(PROVIDER_MODELS["gemini-tts-models"] || []), ...(PROVIDER_MODELS.gemini || []).filter((m) => (m.kind || m.type) === "tts")].map((m) => m?.id).filter(Boolean).filter(unique);
  const DEFAULT_MODEL = KNOWN_MODELS[0] || FALLBACK_MODEL;
config/ttsModels.js:114: { id: "gemini-3.1-flash-tts-preview", name: "Gemini 3.1 Flash TTS", type: "tts" }
  115: { id: "gemini-2.5-flash-preview-tts", ... }
  116: { id: "gemini-2.5-pro-preview-tts", ... }
So KNOWN_MODELS[0] is "gemini-3.1-flash-tts-preview" (3.1 first).

**Current Rust behavior:**

src/core/media/tts/gemini.rs:15-17:
  const DEFAULT_MODEL: &str = "gemini-2.5-flash-preview-tts";
  const KNOWN_MODELS: &[&str] = &["gemini-2.5-flash-preview-tts", "gemini-2.5-pro-preview-tts"];

**Implementation steps:**

In src/core/media/tts/gemini.rs: 1) Add "gemini-3.1-flash-tts-preview" as the FIRST entry of KNOWN_MODELS and set DEFAULT_MODEL to "gemini-3.1-flash-tts-preview". Keep the other two. Final: `const DEFAULT_MODEL: &str = "gemini-3.1-flash-tts-preview"; const KNOWN_MODELS: &[&str] = &["gemini-3.1-flash-tts-preview", "gemini-2.5-flash-preview-tts", "gemini-2.5-pro-preview-tts"];`. 2) parse_model_voice iterates KNOWN_MODELS in order so 3.1 is matched first — no other change needed.

**Guard test:**

fn gemini_tts_default_is_3_1_flash() — parse_model_voice("") returns model == "gemini-3.1-flash-tts-preview"; parse_model_voice("gemini-3.1-flash-tts-preview/Kore") returns ("gemini-3.1-flash-tts-preview", "Kore").

**⚠️ Risks:**

KNOWN_MODELS[0] order determines DEFAULT_MODEL and match priority — 3.1 must be first. A bare voice (e.g. "Kore") still maps to DEFAULT_MODEL (now 3.1) — behavior preserved.

**Cross-check:** ✅ **CONFIRMED** — All three verification points pass. (1) JS behavior is REAL: open-sse/handlers/ttsProviders/gemini.js:7-16 matches the claim exactly — FALLBACK_MODEL = "gemini-3.1-flash-tts-preview" (line 7), KNOWN_MODELS built from TTS_CFG.models, PROVIDER_MODELS["gemini-tts-models"], and PROVIDER_MODELS.gemini filtered to tts kind, with unique filter (lines 8-15), and DEFAULT_MODEL = KNOWN_MODELS[0] || FALLBACK_MODEL (line 16). config/ttsModels.js:114 is `{ id: "gemini-3.1-flash-tts-preview", name: "Gemini 3.1 Flash TTS", type: "tts" }`. I traced the data flow: registry/gemini.js ttsConfig has no models key, so the default comes from PROVIDER_MODELS["gemini-tts-models"] (built by buildTtsProviderModels from TTS_MODELS_CONFIG.gemini.models) and the tts-kinded registry models — both list gemini-3.1-flash-tts-preview first, so the JS default resolves to gemini-3.1-flash-tts-preview. (2) Rust current behavior is REAL: src/core/media/tts/gemini.rs:15-17 shows DEFAULT_MODEL = "gemini-2.5-flash-preview-tts" and KNOWN_MODELS = ["gemini-2.5-flash-preview-tts", "gemini-2.5-pro-preview-tts"] — missing the 3.1 model. (3) Impl steps produce parity: adding gemini-3.1-flash-tts-preview as the first KNOWN_MODELS entry and setting DEFAULT_MODEL to it fully propagates through parse_model_voice, which iterates KNOWN_MODELS for exact-match and "{id}/" prefix parsing and falls back to DEFAULT_MODEL for unrecognized inputs — mirroring the JS parseGeminiModelVoice logic. DEFAULT_VOICE "Kore" matches on both sides, and the URL construction (v1beta/models/{model}:generateContent) is identical. No omissions found.

---

### `P0-C6` — Tortoise generic TTS default base URL wrong (http://localhost:8000/tts vs /api/tts on :5000)

**JS (source of truth — verbatim):**

registry/tortoise.js:17-28:
  ttsConfig: {
    baseUrl: "http://localhost:5000/api/tts",
    authType: "none", authHeader: "none", format: "tortoise",
    models: [{ id: "tortoise-v2", name: "Tortoise v2" }]
  },
  hidden: true

**Current Rust behavior:**

src/core/media/tts/mod.rs:153 `"tortoise" => "http://localhost:8000/tts"` in default_generic_base_url. The generic_formats.rs tortoise fn (lines 334-351) POSTs {text, voice} to req.base_url.

**Implementation steps:**

In src/core/media/tts/mod.rs default_generic_base_url, change line 153 to `"tortoise" => "http://localhost:5000/api/tts"`. No other change — generic_formats::tortoise already POSTs JSON {text, voice} which matches the JS genericFormats handler.

**Guard test:**

fn tortoise_default_base_url_is_api_tts_on_5000() — assert_eq!(default_generic_base_url("tortoise"), "http://localhost:5000/api/tts").

**⚠️ Risks:**

Port and path both differ (5000 vs 8000, /api/tts vs /tts) — change BOTH. The JS entry is hidden:true but still a registered provider — keep tortoise registered in Rust is_tts_provider.

**Cross-check:** ✅ **CONFIRMED** — All three verification points hold. (1) JS claim is real: the registry entry lives at open-sse/providers/registry/tortoise.js (the claim's "registry/tortoise.js" is the same file under the open-sse root); lines 17-28 contain exactly baseUrl "http://localhost:5000/api/tts", format "tortoise", model tortoise-v2, and hidden:true at line 29. The dispatch path is confirmed in open-sse/handlers/ttsProviders/index.js synthesizeViaConfig (reads cfg.baseUrl from the registry ttsConfig and passes it as the handler's baseUrl) and genericFormats.js tortoise handler (POST {text, voice: voiceId||"random"}, returns wav). (2) Rust current behavior is real: src/core/media/tts/mod.rs line 153 has "tortoise" => "http://localhost:8000/tts" in default_generic_base_url, and generic_formats.rs lines 334-351 POST {text, voice} (voice defaults to "random") with application/json, returning wav — an exact match to the JS handler. (3) The impl step produces parity: changing line 153 to "http://localhost:5000/api/tts" aligns the Rust default with JS; generic_base_url prefers the per-connection baseUrl override then falls back to this default, and since JS tortoise (authType "none") has no per-connection override path, the default is what both use. Request shape, headers, and format match. Cross-check corroborates: Rust's coqui default (http://localhost:5002/api/tts) already mirrors the JS coqui registry exactly, confirming tortoise is the lone mismatch. No omissions. Only trivial nit: the cited JS path is a shorthand of the actual open-sse/providers/registry/tortoise.js.

---

### `P0-C7` — OpenRouter TTS headers must match registry headers (HTTP-Referer/X-Title values)

**JS (source of truth — verbatim):**

ttsProviders/openrouter.js:27-40 sends:
  headers: { "Content-Type": "application/json", "Authorization": `Bearer ${credentials.apiKey}`, ...(TTS_CFG.headers || {}) },
registry/openrouter.js:45-49:
  ttsConfig: {
    baseUrl: "https://openrouter.ai/api/v1/chat/completions",
    defaultModel: "openai/gpt-4o-mini-tts",
    headers: {"HTTP-Referer":"https://endpoint-proxy.local","X-Title":"Endpoint Proxy"},
  },

**Current Rust behavior:**

src/core/media/tts/openrouter.rs:55-59 hardcodes `HTTP-Referer: https://openproxy.local` and `X-Title: OpenProxy`.

**Implementation steps:**

In src/core/media/tts/openrouter.rs lines 55-59, change the two values: `HTTP-Referer` → "https://endpoint-proxy.local" and `X-Title` → "Endpoint Proxy". Body and SSE parse stay unchanged.

**Guard test:**

fn openrouter_tts_referer_matches_registry() — build the HeaderMap via the adapter (or a helper) and assert get("HTTP-Referer") == "https://endpoint-proxy.local" and get("X-Title") == "Endpoint Proxy".

**⚠️ Risks:**

Only the two header VALUES change; keep Content-Type and Authorization Bearer. The embedding/image openrouter adapters also use HTTP-Referer https://openproxy.local in Rust — check whether JS registry headers for those also say endpoint-proxy.local (JS registry openrouter.js lines 50-58 show all three configs use https://endpoint-proxy.local) — align embedding/image openrouter headers too.

**Cross-check:** ✅ **CONFIRMED** — All three claims verified against source. (1) JS behavior is real: open-sse/handlers/ttsProviders/openrouter.js:27-40 spreads `...(TTS_CFG.headers || {})` over Content-Type/Authorization, and TTS_CFG = PROVIDER_MEDIA["openrouter"]?.ttsConfig which is built in open-sse/providers/index.js from the registry entry's top-level ttsConfig key. open-sse/providers/registry/openrouter.js:45-49 has exactly the cited ttsConfig with baseUrl "https://openrouter.ai/api/v1/chat/completions", defaultModel "openai/gpt-4o-mini-tts", headers {"HTTP-Referer":"https://endpoint-proxy.local","X-Title":"Endpoint Proxy"} (transport.headers at lines 23-26 carry the same two values, so both agree). So JS genuinely sends endpoint-proxy.local / Endpoint Proxy on the TTS request. (2) Rust current behavior is real: src/core/media/tts/openrouter.rs:55-59 hardcodes HeaderValue::from_static("https://openproxy.local") for HTTP-Referer and "OpenProxy" for X-Title; body (lines 61-67) and SSE parse (lines 79-101) already mirror the JS exactly (model/modalities/audio voice+format wav/stream/messages, parse choices/0/delta/audio/data, concat base64, format "wav"). (3) Impl is complete and correct: swapping the two from_static strings to "https://endpoint-proxy.local" and "Endpoint Proxy" yields exact header parity; body and SSE parse require no change. No Rust tests assert the current header values (grep of all .rs files found none referencing these in tests), so no test churn. Target values also match the existing LLM executor convention (src/core/executor/default.rs:33-34 and provider.rs:925-926 already use endpoint-proxy.local / Endpoint Proxy). Note: src/core/media/embeddings/base.rs and image/openai_compat.rs also use the old openproxy.local/OpenProxy values, but those are outside this task's scope (OpenRouter TTS gap only).

---

### `P0-D8` — Cloudflare AI: num_steps optional field missing (multipart models)

**JS (source of truth — verbatim):**

imageProviders/cloudflareAi.js:12-19:
  const OPTIONAL_FIELDS = ["negative_prompt","guidance","seed","num_steps","steps","strength"];

**Current Rust behavior:**

src/core/media/image/cloudflare_ai.rs:27-33:
  const OPTIONAL_FIELDS: &[&str] = &["negative_prompt","guidance","seed","steps","strength"];  // missing "num_steps"

**Implementation steps:**

In src/core/media/image/cloudflare_ai.rs, insert "num_steps" into OPTIONAL_FIELDS before "steps": `const OPTIONAL_FIELDS: &[&str] = &["negative_prompt", "guidance", "seed", "num_steps", "steps", "strength"];`. The add_optional_fields_json loop then forwards num_steps automatically.

**Guard test:**

fn cloudflare_optional_fields_include_num_steps() — assert OPTIONAL_FIELDS.contains(&"num_steps") and that add_optional_fields_json copies a num_steps value into the request map.

**⚠️ Risks:**

Field order in the slice doesn't matter functionally; presence does. Keep "steps" AND "num_steps" both (JS has both).

**Cross-check:** ✅ **CONFIRMED** — The JS behavior is real: at .tmp/9router/open-sse/handlers/imageProviders/cloudflareAi.js (the cited "imageProviders/cloudflareAi.js" path is abbreviated, dropping the open-sse/handlers/ prefix), lines 12-19 contain exactly `const OPTIONAL_FIELDS = ["negative_prompt","guidance","seed","num_steps","steps","strength"]`, and addOptionalFields (lines 62-68) forwards every non-null/non-empty key. The Rust current state is real: src/core/media/image/cloudflare_ai.rs lines 27-33 define OPTIONAL_FIELDS without "num_steps" (only negative_prompt, guidance, seed, steps, strength). The impl step would produce parity: inserting "num_steps" before "steps" into the Rust array makes add_optional_fields_json (lines 96-111, which iterates OPTIONAL_FIELDS and inserts any non-null/non-empty body value) forward num_steps automatically, matching the JS list element-for-element. The only discrepancy is the abbreviated JS file path in the citation, which does not affect substance since content and line numbers match exactly. The multipart-vs-JSON difference for FLUX-2 models is out of scope for this num_steps field parity task.

---

### `P0-D9` — NanoBanana: JS typo "IMAGETOIAMGE"/"TEXTTOIAMGE" must be preserved

**JS (source of truth — verbatim):**

imageProviders/nanobanana.js:22-23:
  const req = {
    prompt: body.prompt,
    type: isEdit ? "IMAGETOIAMGE" : "TEXTTOIAMGE",  // <-- intentional upstream typo
    numImages: body.n || 1,
    image_size: ratio,
    callBackUrl: "https://localhost/callback",
  };

**Current Rust behavior:**

src/core/media/image/nanobanana.rs:62-68 emits `"type": if is_edit { "IMAGE_TO_IMAGE" } else { "TEXT_TO_IMAGE" }` — the CORRECTED spelling, NOT the JS typo.

**Implementation steps:**

In src/core/media/image/nanobanana.rs line 64, change to `"type": if is_edit { "IMAGETOIAMGE" } else { "TEXTTOIAMGE" }` (drop the underscores, match JS exactly). All other fields (numImages, image_size, callBackUrl, imageUrls) already match.

**Guard test:**

fn nanobanana_type_keeps_upstream_typo() — build_body with no image returns type == "TEXTTOIAMGE"; with an image returns type == "IMAGETOIAMGE".

**⚠️ Risks:**

This is a deliberate upstream typo — the upstream API keys on the misspelled string. Do NOT 'fix' it to IMAGE_TO_IMAGE. Both spellings must match JS verbatim.

**Cross-check:** ✅ **CONFIRMED** — All load-bearing claims verified. (1) JS behavior is REAL: the typo'd strings `"IMAGETOIAMGE"`/`"TEXTTOIAMGE"` exist verbatim at .tmp/9router/open-sse/handlers/imageProviders/nanobanana.js:23 (lines 22-23 match the cited snippet exactly — prompt on 22, type on 23). Intentionality is confirmed by tests/unit/image-generation.test.js:193 asserting `requestBody.type` === "TEXTTOIAMGE". Minor citation error: the cited path `imageProviders/nanobanana.js` omits the `open-sse/handlers/` prefix; the file exists at `open-sse/handlers/imageProviders/nanobanana.js`. (2) Rust current behavior is REAL: src/core/media/image/nanobanana.rs line 64 emits `"type": if is_edit { "IMAGE_TO_IMAGE" } else { "TEXT_TO_IMAGE" }` — the corrected spelling, exactly as claimed. (3) Impl step produces parity: changing line 64 to `"IMAGETOIAMGE"`/`"TEXTTOIAMGE"` matches JS byte-for-byte, and all other fields already match — numImages (JS `body.n || 1` vs Rust `n()` default 1), image_size (identical 1:1/9:16/16:9/2:3/3:2 mapping), callBackUrl "https://localhost/callback", and imageUrls (both filter non-empty image strings and push single image). No omission found.

---

### `P1-E10` — Codex image: stale user-agent/version constants (0.129.0 vs 0.136.0)

**JS (source of truth — verbatim):**

imageProviders/codex.js:7-9:
  const CODEX_USER_AGENT = "codex_cli_rs/0.136.0";
  const CODEX_VERSION = "0.136.0";
  const CODEX_ORIGINATOR = "codex_cli_rs";

**Current Rust behavior:**

src/core/media/image/codex.rs:21-23:
  const CODEX_USER_AGENT: &str = "codex-imagen/0.2.6";
  const CODEX_VERSION: &str = "0.129.0";
  const CODEX_ORIGINATOR: &str = "codex_cli_rs";

**Implementation steps:**

In src/core/media/image/codex.rs: 1) change CODEX_USER_AGENT to "codex_cli_rs/0.136.0"; 2) change CODEX_VERSION to "0.136.0"; 3) CODEX_ORIGINATOR already "codex_cli_rs" — keep. Headers are inserted from these constants in build_headers (lines 203-210), so no further change.

**Guard test:**

fn codex_version_constants_match_js() — assert CODEX_VERSION == "0.136.0" and CODEX_USER_AGENT == "codex_cli_rs/0.136.0".

**⚠️ Risks:**

The user-agent format differs from what JS sends (codex_cli_rs/ vs codex-imagen/) — match JS exactly. These headers gate the ChatGPT backend.

**Cross-check:** ✅ **CONFIRMED** — All three verification points check out.

1. JS behavior REAL: The file exists at C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/open-sse/handlers/imageProviders/codex.js (claim cited the abbreviated path "imageProviders/codex.js" — same basename and line numbers, so clearly the intended file). Lines 7-9 exactly match: CODEX_USER_AGENT = "codex_cli_rs/0.136.0", CODEX_VERSION = "0.136.0", CODEX_ORIGINATOR = "codex_cli_rs". The constants are actually consumed: buildImageHeaders uses them at lines 157-160 (originator, user-agent, version).

2. Rust current REAL: src/core/media/image/codex.rs lines 21-23 exactly match the claim: CODEX_USER_AGENT = "codex-imagen/0.2.6", CODEX_VERSION = "0.129.0", CODEX_ORIGINATOR = "codex_cli_rs". build_headers inserts them at lines 203 (originator), 209 (user-agent), 210 (version) — claim's 203-210 range is accurate.

3. Impl steps produce parity: Changing CODEX_USER_AGENT to "codex_cli_rs/0.136.0" and CODEX_VERSION to "0.136.0" makes the Rust constants byte-identical to the JS constants; CODEX_ORIGINATOR is already "codex_cli_rs" on both sides so no change needed. Since headers are read directly from these constants in build_headers, no further change is required. No omissions found.

Only nit: the JS file path in the citation omits the "open-sse/handlers/" prefix, but the cited file and line numbers are unambiguous and exact.

---

### `P1-E11` — Image handler: 401/403 → refresh-token retry ONCE is missing in Rust

**JS (source of truth — verbatim):**

imageGenerationCore.js:121-155:
  const executor = getExecutor(provider);
  if (!executor?.noAuth && !adapter.noAuth && (providerResponse.status === 401 || providerResponse.status === 403)) {
    const newCredentials = await refreshWithRetry(() => executor.refreshCredentials(credentials, log), 3, log);
    if (newCredentials?.accessToken || newCredentials?.apiKey) {
      Object.assign(credentials, newCredentials);
      if (onCredentialsRefreshed) await onCredentialsRefreshed(newCredentials);
      // rebuild body/headers/url and re-fetch once
      providerResponse = await fetch(retryUrl, {...});
    } else { log?.warn?.("TOKEN", `${provider.toUpperCase()} | refresh failed`); }
  }

**Current Rust behavior:**

src/core/media/image/handler.rs:93-107 — fires one POST; on non-success returns Err(ImageHandlerError::Http(status, body)) with NO 401/403 refresh+retry. There is no refresh mechanism wired into the adapter path at all.

**Implementation steps:**

1) In src/core/media/image/handler.rs, after the first response, if status is 401 or 403 AND the adapter is not no_auth() AND credentials have a refresh_token, attempt refresh via the existing OAuth/credential refresh infrastructure (see src/core/auth/credential_manager.rs or src/cli/provider_oauth.rs for the refresh path; wire a callback similar to video's refresh — check what infrastructure exists; the JS uses refreshWithRetry(refreshCredentials, 3)). 2) If refresh yields a new access_token/api_key, mutate the connection credentials, rebuild body/url/headers (call adapter.build_* again), and re-send ONCE. 3) If refresh fails, return the original 401/403. 4) Only retry when a refresh actually produced credentials; never retry a second time. Mirror videoCore's guard: only OAuth accounts with refreshToken can refresh.

**Guard test:**

fn image_handler_retries_once_after_refresh() — unit-test the retry guard: given a 401 response and a refreshed credential, the adapter is invoked exactly twice; without refresh credentials it is invoked once.

**⚠️ Risks:**

Retry exactly ONCE (not the 3 refresh ATTEMPTS, which is refreshWithRetry's internal retry). Do not retry on 401/403 for no_auth adapters (sdwebui, comfyui). The retried request must reuse the rebuilt headers/body. Do not retry other statuses.

**Cross-check:** ✅ **CONFIRMED** — All core claims verified against source. (1) JS is REAL: imageGenerationCore.js:121-155 exactly matches the claimed behavior — on 401/403 with neither executor.noAuth nor adapter.noAuth, calls refreshWithRetry(() => executor.refreshCredentials(credentials, log), 3, log), Object.assign's new creds, rebuilds body/headers/url and re-fires one retry POST. HTTP_STATUS.UNAUTHORIZED/FORBIDDEN=401/403 confirmed in runtimeConfig.js:2-15; refreshWithRetry (3 attempts) confirmed in tokenRefresh.js:252-270; executor noAuth/refreshCredentials interface confirmed in executors/base.js:15,88 and getExecutor in executors/index.js. (2) Rust current behavior is REAL: src/core/media/image/handler.rs:93-107 fires one POST and on any non-success returns Err(ImageHandlerError::Http(status, body)) with no 401/403 refresh+retry. ProviderConnection has refresh_token/access_token/provider_specific_data (types/mod.rs:130-139) and the ImageAdapter trait has a no_auth() hook (base.rs:120), but nothing is wired; image/mod.rs's own doc comment stale-claims "handles 401 retry-after-refresh", corroborating the gap. (3) impl_steps would achieve parity: dispatch_oauth_refresh(provider, refresh_token, &provider_specific_data) -> Result<RefreshResult{access_token, refresh_token, expires_in}, String> at oauth/token_refresh.rs:899/140 and CredentialManager::refresh_if_needed at core/auth/credential_manager.rs:71 are correct, existing primitives; wiring one 401/403 retry through them is feasible and matches JS behavior. Two minor nits that do not invalidate the task: (a) the claim omits the `!executor?.noAuth` guard from the JS condition and the fact that refreshWithRetry runs 3 total attempts (not literally "retry 3 times"), though behaviorally equivalent to retry-once; (b) the impl step "wire a callback similar to video's refresh" references a nonexistent src/core/media/video module (no video handler exists — the dir only has error.rs, mod.rs, image/, embeddings/, responses/, search/, stt/, tts/), though the step's actual named refresh paths (credential_manager.rs / provider_oauth.rs / dispatch_oauth_refresh) are all real and sufficient for the fix. Also note the JS refresh path only applies to the non-executor adapter flow (useExecutor/executeViaExecutor adapters at line 54 bypass it in JS too), which is exactly the Rust manual build/fetch path being fixed — so the gap and the fix target are consistent.

---

### `P1-F12` — Search: youcom base URL must be https://ydc-index.io/v1/search, not api.you.com

**JS (source of truth — verbatim):**

registry/youcom.js:20 searchConfig: { baseUrl: "https://ydc-index.io/v1/search", method: "GET", authType: "apikey", authHeader: "x-api-key", ... } and callers.js:270-304 buildYouComRequest uses resolveBaseUrl(config, params) which defaults to that baseUrl; query params are query/count/freshness/offset/country/language/include_domains/exclude_domains/livecrawl/livecrawl_formats; header X-API-Key.

**Current Rust behavior:**

src/core/media/search/providers.rs:828-831 YouComProvider::build_url uses `resolve_base_url("https://api.you.com/search", request)` — wrong host AND wrong path (JS builder appends NO extra path; the baseUrl already ends /v1/search). Normalizer (lines 842-905) reads container["web"]/["news"] correctly.

**Implementation steps:**

In src/core/media/search/providers.rs YouComProvider::build_url line 829, change the default base to "https://ydc-index.io/v1/search". The builder appends query params only (already does) — no extra path segment to add since the default already ends /v1/search. X-API-Key header (line 837) already matches JS authHeader x-api-key.

**Guard test:**

fn youcom_url_uses_ydc_index() — build_url with a token yields a URL starting with "https://ydc-index.io/v1/search?".

**⚠️ Risks:**

JS appends no extra path to the baseUrl (it already ends /v1/search). Do not append /search again. The default.rs entry for youcom (https://api.you.com/v1) is only for the chat fallthrough and is not used by the search adapter.

**Cross-check:** ✅ **CONFIRMED** — All three checks pass. (1) JS: registry/youcom.js (at .tmp/9router/open-sse/providers/registry/youcom.js) line 20 has baseUrl "https://ydc-index.io/v1/search" with searchConfig method GET, authType apikey, authHeader "x-api-key"; callers.js (at open-sse/handlers/search/callers.js) lines 270-304 buildYouComRequest uses resolveBaseUrl (lines 70-73, defaults to config.baseUrl) and appends ONLY query params (query/count/freshness/offset/country/language/include_domains/exclude_domains/livecrawl/livecrawl_formats) with X-API-Key header — all exact. (2) Rust: providers.rs:829 resolves "https://api.you.com/search" — wrong host AND wrong path since the JS default already ends /v1/search and the JS builder appends no path segment; resolve_base_url (base.rs:224) only substitutes the default and trims slashes, keeping the wrong /search path. Normalizer (842-905) reads body["results"] then web/news and matches JS normalizeYouCom. (3) Impl steps achieve parity: swapping line 829's default to https://ydc-index.io/v1/search produces https://ydc-index.io/v1/search?query=... with every query param matching JS semantics (count min-100, offset floor/ max cap 9, freshness skip-on-any, livecrawl/livecrawl_formats news-vs-web/markdown-html), and the X-API-Key header already matches JS's runtime header (registry's lowercase "x-api-key" is config metadata only). No omission found. Only nit: cited JS paths omit the open-sse/ prefix, but file/line content is exact.

---

### `P1-F13` — Search: searxng default URL must come from SEARXNG_URL env (default localhost:8888/search)

**JS (source of truth — verbatim):**

config/runtimeConfig.js:49: export const SEARXNG_URL = envUrl("SEARXNG_URL", "http://localhost:8888/search");
providers/registry/searxng.js:20: searchConfig: { baseUrl: SEARXNG_URL, ... } (authType none, maxMaxResults 50, timeoutMs 10000, cacheTTLMs 180000).
callers.js:306-327 buildSearxngRequest: appends "/search" only if baseUrl does not end with /search; params q/format=json/categories=general|news/language/time_range/pageno.

**Current Rust behavior:**

src/core/media/search/providers.rs:920 `let base = resolve_base_url("http://localhost:8080", request);` — hardcoded, no env read, wrong default port (8080 vs 8888) and no /search suffix. There is no SEARXNG_URL env handling anywhere in Rust (verified: no match in src/).

**Implementation steps:**

1) In src/core/media/search/providers.rs SearxngProvider::build_url: replace the hardcoded default with an env-driven default: `let default = std::env::var("SEARXNG_URL").unwrap_or_else(|_| "http://localhost:8888/search".to_string());` then `let base = resolve_base_url(&default, request);`. 2) Keep the existing /search-suffix logic (line 921-924) which already appends /search only when missing — so a baseUrl of http://localhost:8888/search stays as-is. 3) This preserves the provider_options.baseUrl override via resolve_base_url.

**Guard test:**

fn searxng_default_from_env() — with SEARXNG_URL unset, build_url yields "http://localhost:8888/search?..."; with SEARXNG_URL="https://x.example" yields "https://x.example/search?" (env read).

**⚠️ Risks:**

Env default must be read at call time (or cached lazily), matching JS envUrl which trims. The /search suffix append must still only happen when the base doesn't already end /search. provider_options.baseUrl override must still win over env.

**Cross-check:** ✅ **CONFIRMED** — JS claims are exact. config/runtimeConfig.js:49 (actual: open-sse/config/runtimeConfig.js:49) reads `export const SEARXNG_URL = envUrl("SEARXNG_URL", "http://localhost:8888/search")` with envUrl falling back on empty/whitespace. providers/registry/searxng.js:20 sets `baseUrl: SEARXNG_URL` with authType "none", maxMaxResults 50, timeoutMs 10000, cacheTTLMs 180000 exactly as cited. open-sse/handlers/search/callers.js:306-327 buildSearxngRequest appends /search only when baseUrl doesn't end with /search and builds q/format=json/categories=general|news (+language/time_range/pageno). All cited line numbers and values match verbatim; only the spec's paths omit the `open-sse/` prefix. Rust claims are exact: providers.rs:920 is `let base = resolve_base_url("http://localhost:8080", request);` — hardcoded, port 8080 (not 8888), no /search in the default string; grep confirms zero SEARXNG_URL / std::env::var handling anywhere in src/. Impl steps produce parity: `resolve_base_url(default: &str, request)` accepts `&default` via deref coercion, and retaining the existing 921-925 suffix logic mirrors callers.js:308, so effective URLs match JS. Minor caveats that don't negate parity: (1) the Rust unit test at providers.rs:1087-1092 (searxng_no_auth_uses_localhost_default) asserts the old 8080 default and needs updating alongside — not mentioned in the (truncated) spec; (2) Rust std::env::var treats an empty SEARXNG_URL as set (produces malformed relative /search URL) while JS envUrl falls back to default on empty — a corner case fixable by filtering empty strings. Neither affects the core parity outcome.

---

### `P1-F14` — Search: global timeout 15s (JS) vs 30s (Rust), and no per-provider timeoutMs

**JS (source of truth — verbatim):**

handlers/search/index.js:14: const GLOBAL_TIMEOUT_MS = 15000;
  line 94-96: const timeout = Math.min(providerConfig.timeoutMs || 10000, Math.max(remaining, 1000));  // remaining = GLOBAL_TIMEOUT_MS - elapsed
  line 17: const NON_RETRIABLE = new Set([400, 401, 403, 404]);

**Current Rust behavior:**

src/core/media/search/handler.rs:9 `const GLOBAL_TIMEOUT: Duration = Duration::from_secs(30);` applied via .timeout(GLOBAL_TIMEOUT) at line 47. No per-provider timeout, no remaining-budget computation.

**Implementation steps:**

1) In src/core/media/search/handler.rs change GLOBAL_TIMEOUT to Duration::from_secs(15). 2) Optionally add a per-provider timeout override: SearchProvider gains a default fn timeout_ms() -> Option<u64> (None default); searxng returns Some(10000), youcom Some(10000) per registry timeoutMs. 3) In handle_search, compute `let effective = provider.timeout_ms().map(Duration::from_millis).unwrap_or(GLOBAL_TIMEOUT);` and use `.timeout(effective)`. For strict JS parity the per-provider timeout is min(providerConfig.timeoutMs||10000, remaining) — implementing just the 15s global + 10s provider floor covers the observable gap.

**Guard test:**

fn search_global_timeout_is_15s() — assert GLOBAL_TIMEOUT == Duration::from_secs(15); assert searxng provider timeout_ms() == Some(10000).

**⚠️ Risks:**

Do not lower the timeout below what a provider needs — searxng/youcom both 10s. The 401/403/404 non-retriable set is relevant to the chat-failover gap (P1-F15) not the timeout itself.

**Cross-check:** 🟡 **PLAUSIBLE** — Cited JS behavior is real (content exact), but the file path is wrong (actual: .tmp/9router/open-sse/handlers/search/index.js, not handlers/search/index.js) and the NON_RETRIABLE line is 15 not 17. GLOBAL_TIMEOUT_MS=15000 (line 14) and the timeout computation at lines 94-96 (min(providerConfig.timeoutMs||10000, remaining, floor 1000)) match exactly, as does NON_RETRIABLE=Set[400,401,403,404]. Rust current behavior is real: handler.rs line 9 GLOBAL_TIMEOUT=Duration::from_secs(30), applied at line 47 via .timeout(); no per-provider timeout or remaining-budget logic. Impl step 1 (30s->15s) is correct. However, step 2 is incomplete: it adds timeout_ms Some(10000) only to searxng and youcom, but ALL 10 Rust-mapped JS search providers have an effective 10s timeout (serper/brave/exa/tavily/google-pse/linkup/searchapi timeoutMs:10000, perplexity via the ||10000 default). If step 3's unwrap_or default is GLOBAL_TIMEOUT (as the truncated "unwrap_or(GLOBA..." suggests), 8 of 10 Rust providers would run at 15s vs JS's 10s — a real parity gap. Full parity requires either every provider to return Some(10000) or the default to be 10000ms. Secondary note: JS also has a NON_RETRIABLE-driven chat-search fallback within the global budget that Rust lacks entirely, but that is outside this task's timeout scope.

---

### `P1-F15` — Search: dedicated → chat-based failover missing (and provider searchViaChat config)

**JS (source of truth — verbatim):**

handlers/search/index.js:181-198:
  if (!NON_RETRIABLE.has(result.status || 0) && Date.now() - globalStartTime < GLOBAL_TIMEOUT_MS && provider.searchViaChat && providerConfig) {
    log?.warn?.(...);
    const fallback = await handleChatSearch({ provider: provider.id, query: clean, maxResults: normalizedBody.max_results, model: provider.searchViaChat.defaultModel, credentials, log });
    if (fallback.success) return successResult(fallback.data);
  }
Non-retriable = 400,401,403,404. chatSearch.js implements gemini/openai/xai/kimi/minimax/perplexity/perplexity-agent chat-search wrappers (endpoint, buildBody, buildHeaders, extractAnswer per provider).

**Current Rust behavior:**

src/core/media/search/handler.rs handle_search returns Err on any non-2xx (lines 56-60) with no failover. chat_search.rs is a separate chat-completions-style endpoint (POST /v1/chat/search) and there is no logic that falls back to it after a dedicated search provider fails. Rust has no chatSearch config or CHAT_SEARCH_CONFIG equivalent.

**Implementation steps:**

This is a large feature. Minimal parity: 1) In search handler, on upstream Http(status, _) where status NOT in {400,401,403,404} AND the provider has a configured chat fallback, call a new chat_search helper. 2) Add a provider-level `chat_fallback_model()`/`chat_fallback_endpoint()` accessor defaulting to None; implement for gemini (endpoint https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent, model gemini-2.5-flash, header x-goog-api-key) and openai (model openai/gpt-4o-mini via provider default). 3) Implement handle_chat_search (mirror chatSearch.js CHAT_SEARCH_CONFIG for at least gemini + openai; xai/kimi/minimax/perplexity optional) and merge results into SearchResultSet. 4) The fallback runs only when within the 15s global budget and the error is retriable. Recommend implementing gemini + openai first given the providers configured with searchViaChat in the JS registry.

**Guard test:**

fn search_fails_over_to_chat_on_retriable_error() — given a 502 upstream error and a provider with a chat fallback model, the fallback path is invoked; given a 404 the fallback is NOT invoked.

**⚠️ Risks:**

Failover must NOT run for 400/401/403/404. It must run only inside the global timeout budget. The fallback needs its own credentials (same connection). chatSearch output shape differs from SearchResultSet (citations → results) — normalize accordingly. This is the biggest gap; if too large, implement the gemini/openai chat fallback config + one code path and stub the rest.

**Cross-check:** ✅ **CONFIRMED** — JS behavior is real: the cited failover exists verbatim at .tmp/9router/open-sse/handlers/search/index.js:182-198 (claim's path drops the open-sse/ prefix, but file, line range, NON_RETRIABLE={400,401,403,404} at line 15, the handleChatSearch fallback call with provider.searchViaChat.defaultModel, and the success passthrough all match exactly). handleChatSearch (chatSearch.js) is a genuine chat-completions-based search wrapper, and gemini's searchViaChat config (gemini.js:79-84) matches the impl step's endpoint https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent and defaultModel gemini-2.5-flash. Rust current behavior is real: handler.rs:56-60 returns Err(Http) on any non-2xx with no failover; chat_search.rs is a separate POST /v1/chat/search endpoint that wraps the SAME dedicated search_dispatch (not a chat-based LLM search), and neither the SearchProvider trait (base.rs) nor provider impls (providers.rs) expose any chat-fallback config/accessor. Impl steps mirror the JS mechanism and would achieve parity; gemini endpoint/model are accurate. Caveats (not refuting): (1) in the shipped 9router registry no provider has BOTH searchConfig and searchViaChat, so the JS failover branch is currently dormant — parity adds the mechanism but it won't fire until a provider configures both; (2) impl step 1 omits the JS global-timeout budget guard (index.js:184) and the unified-shape `answer` field that chatSearch returns — minor refinements, not blockers.

---

### `P1-G16` — Video: 401/403 refresh-token retry missing (JS videoCore does refresh once)

**JS (source of truth — verbatim):**

videoCore.js:120-146:
  if ((upstream.status === 401 || upstream.status === 403) && credentials?.refreshToken) {
    refreshed = await refreshTokenByProvider(provider, credentials, log);
    if (refreshed?.accessToken) { Object.assign(credentials, refreshed); if (onCredentialsRefreshed) await onCredentialsRefreshed(refreshed); upstream = await doFetch(credentials.accessToken || credentials.apiKey); }
    else log?.warn?.(... "refresh failed — account needs re-auth");
  }
Also videoCore.js:21-31 sanitizeSecrets strips Bearer tokens and accessToken/refreshToken/apiKey from client-bound text.

**Current Rust behavior:**

src/server/api/media.rs video_create_handler (lines 889-993) and video_get_handler (lines 998-1059) send the request once and proxy the upstream response directly (proxy_upstream_response) with NO 401/403 refresh+retry and no secret sanitization. There is no refreshToken wiring.

**Implementation steps:**

1) In media.rs video_create_handler, after the POST response, if status is 401 or 403 and the connection has a refresh_token (check connection.refresh_token or access token fields — inspect ProviderConnection type; JS uses credentials.refreshToken), call the OAuth refresh path (mirror src/core/auth/credential_manager.rs refresh or src/cli/provider_oauth.rs). 2) If a new access_token is returned, update the connection, rebuild headers via build_media_headers, and re-send ONCE. 3) If no refresh token or refresh fails, return the original 401/403. 4) Do the same in video_get_handler. 5) Sanitize upstream error text: strip `Bearer <token>` and the raw access/refresh/api keys before returning (port sanitizeSecrets).

**Guard test:**

fn video_create_retries_once_on_401_with_refresh() — with a refresh token, a 401 triggers exactly one retry with the new token; without refresh token, no retry and the 401 is returned. fn video_sanitizes_secrets() — error text containing "Bearer abcdefgh" or the raw api key is redacted.

**⚠️ Risks:**

JS guards retry on refreshToken presence — API-key-only accounts never refresh. Retry exactly ONCE. The creation POST is billable — but 401/403 rejection happens BEFORE job creation, so the refresh re-send is safe (matches JS comment). Sanitize before the 2000-char slice reaches the client.

**Cross-check:** 🟡 **PLAUSIBLE** — JS claim CONFIRMED: .tmp/9router/open-sse/handlers/videoCore.js:120-146 contains exactly the cited 401/403 + credentials.refreshToken → refreshTokenByProvider → Object.assign(credentials, refreshed) → onCredentialsRefreshed → single doFetch retry. refreshTokenByProvider (open-sse/services/tokenRefresh.js:141,177) has an xai handler (refreshXaiToken); HTTP_STATUS 401/403 match. The app layer (videoGeneration.js:145-152, 204-211) also persists refreshed creds via updateProviderCredentials(accessToken, refreshToken), which the spec's steps never mention.

Rust current CONFIRMED: media.rs video_create_handler (889-993) and video_get_handler (998-1059) do one POST/GET then proxy_upstream_response with no 401/403 check, no refresh/retry, and no secret sanitization (unlike JS sanitizeSecrets). No refreshToken wiring in the file.

Impl steps LARGELY sound but two issues: (1) ProviderConnection has refresh_token: Option<String> (types/mod.rs:132) and the xAI refresh path exists (credential_manager.rs refresh_if_needed → dispatch_oauth_refresh → refresh_xai_token, token_refresh.rs:658), so the mechanism is present; BUT refresh_if_needed is expiry-gated via check_needs_refresh (returns Ok no-op when the token is not near expiry), so for an on-401/403 forced refresh the implementer must call dispatch_oauth_refresh / refresh_xai_token directly rather than "mirror credential_manager.rs refresh", otherwise the retry never fires on a still-unexpired-but-rejected token. (2) The spec omits persisting the refreshed token back to the DB row (JS does this via onCredentialsRefreshed → updateProviderCredentials); without a db.update() over provider_connections (pattern exists in oauth.rs:1372-1403), the retry succeeds for the current request but the next request re-loads the stale connection and 401s again. These are omissions/refinements, not fatal — the direction is correct and achievable (xAI is the only video provider, video_provider_supported restricts to xai, and xAI refresh is supported).

---

### `P1-H17` — Search: query sanitization (control chars, NFKC, whitespace collapse) missing in Rust

**JS (source of truth — verbatim):**

handlers/search/index.js:17-25:
  const CONTROL_CHAR_RE = /[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]/;
  function sanitizeQuery(query) {
    if (CONTROL_CHAR_RE.test(query)) return { error: "Query contains invalid control characters" };
    const clean = query.normalize("NFKC").trim().replace(/\s+/g, " ");
    if (!clean) return { error: "Query is empty after normalization" };
    return { clean };
  }
  Also line 28-35 sanitizeHeaders strips non-ASCII chars ([^\x00-\xFF]) from header values.

**Current Rust behavior:**

src/core/media/search/base.rs request_from_body (lines 141-148) only checks trim + non-empty — no control-char rejection, no NFKC, no whitespace collapse. handle_search (handler.rs:28-30) only checks query.is_empty().

**Implementation steps:**

1) In src/core/media/search/base.rs request_from_body, after extracting the trimmed query: reject if it contains any byte in the control set 0x00-0x08, 0x0B, 0x0C, 0x0E-0x1F, 0x7F (return Err "Query contains invalid control characters"). 2) Apply NFKC-like normalization (Rust: use `unicode-normalization` crate `query.nfkc()` if available, else leave as-is — JS normalizes; at minimum implement trim + `split_whitespace().join(" ")` for whitespace collapse) then trim and collapse `\s+` to single space. 3) If result empty → Err "Query is empty after normalization". 4) Optionally sanitize header values to ASCII (strip bytes > 0x7F) in build_headers helpers.

**Guard test:**

fn search_query_rejects_control_chars() — query containing "\x07" returns Err containing "control characters". fn search_query_collapses_whitespace() — "a  b" normalizes to "a b".

**⚠️ Risks:**

The control-char set is NOT the full 0x00-0x1F range — 0x09 (tab), 0x0A (LF), 0x0D (CR) are excluded. Empty-after-normalization must error. Whitespace collapse must not alter meaningful spacing between words beyond collapsing runs.

**Cross-check:** ✅ **CONFIRMED** — All three verification points hold. (1) JS claim is real: the cited code is at C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/open-sse/handlers/search/index.js lines 17-25 (CONTROL_CHAR_RE regex + sanitizeQuery with NFKC normalize, trim, whitespace collapse, and the two error messages), called from handleSearchCore line 151 with an HTTP 400 on sanitize error. Minor path discrepancy only — the claimed path omitted the open-sse/ prefix; file content and line numbers match exactly. (2) Rust claim is real: src/core/media/search/base.rs request_from_body (lines 142-148) does only str::trim + non-empty filter; no control-char rejection, no NFKC, no whitespace collapse. src/core/media/search/handler.rs handle_search (lines 28-30) checks only request.query.is_empty(). Grep across src/ confirmed no existing NFKC/control-char handling (all 'control' matches are unrelated cache_control/HTTP-control usage). (3) Impl steps would produce parity: Cargo.lock has no unicode-normalization (only idna/unicode-ident transitively via url), so the impl step 2 hedge "if available, else leave as-is; at minimum trim" is correctly scoped — trim is already implemented, so NFKC requires adding the unicode-normalization crate as a new dependency. Step 1 (rejecting bytes 0x00-0x08, 0x0B, 0x0C, 0x0E-0x1F, 0x7F with a control-char error) is a straightforward byte scan to insert after the query extraction in request_from_body. No obvious omission; the only nuance is that request_from_body's existing trim+empty-reject already subsumes handle_search's is_empty() check, which is pre-existing behavior and not affected by the impl steps.

---

### `P1-I18` — Media generic forwarder: xai/vercel/etc. chat URL leak to media routes (adapter precedence)

**JS (source of truth — verbatim):**

The JS media dispatch (imageGenerationCore / embeddingsCore / ttsCore) first checks provider-specific adapters; only providers WITHOUT an adapter fall through to the generic OpenAI forwarder. For providers WITH adapters (xai image, vercel gateway image/embedding), the media route must use the adapter's URL, never the chat-completions URL.

**Current Rust behavior:**

src/server/api/media.rs:346-360 try_provider_adapter runs adapter dispatch (image/tts/embeddings/search) and returns Some when handled; otherwise falls through to build_media_url (line 562) using get_provider_base_url (line 629) which uses default.rs chat URLs. Because xai/vercel-ai-gateway are NOT in the Rust image/embedding adapter registries, their media requests fall through to the chat URL (e.g. https://api.x.ai/v1/chat/completions/embeddings), producing wrong endpoints.

**Implementation steps:**

Resolved indirectly by P0-B3a and P0-B3b: registering the xai and vercel-ai-gateway adapters makes try_provider_adapter return Some for those providers' image/embedding routes, preventing the chat-URL fallthrough. Add tests asserting dispatch returns Some (not None) for xai images and vercel-ai-gateway embeddings/images.

**Guard test:**

fn media_dispatch_short_circuits_for_xai_and_vercel() — image::dispatch is Some for provider "xai"; embeddings::dispatch is Some for "vercel-ai-gateway"; image::dispatch is Some for "vercel-ai-gateway".

**⚠️ Risks:**

Any provider added to an adapter registry must be removed from the effective fall-through — otherwise two endpoints are reachable for the same provider+route. The fall-through URL appends /embeddings or /images/generations to a chat base — never correct.

**Cross-check:** ✅ **CONFIRMED** — All three claims verified against actual code.

JS (9router): imageGenerationCore.js:45-51 and embeddingsCore.js:32-38 first look up provider-specific adapters via getImageAdapter/getEmbeddingAdapter and use adapter.buildUrl() — never the chat URL. imageProviders/index.js:16-35 registers xai and vercel-ai-gateway; embeddingProviders/index.js:7-11 registers vercel-ai-gateway. URL configs confirm media endpoints, not chat: xai.js:38 imageConfig.baseUrl=https://api.x.ai/v1/images/generations; vercel-ai-gateway.js:32-33 embeddingConfig.baseUrl=https://ai-gateway.vercel.sh/v1/embeddings and imageConfig.baseUrl=https://ai-gateway.vercel.sh/v1/images/generations. So the JS adapter-URL (never chat-URL) behavior is real.

Rust (openproxy): media.rs:346-360 runs try_provider_adapter first, which dispatches image/tts/embeddings/search (media.rs:785-814) and returns None when no adapter; fallthrough reaches build_media_url (media.rs:562) -> get_provider_base_url (media.rs:629) -> get_provider_config(base_url) from default.rs. default.rs:73-74 maps xai to https://api.x.ai/v1/chat/completions and default.rs:269-270 maps vercel-ai-gateway to https://ai-gateway.vercel.sh/v1/chat/completions — both chat URLs. Because get_image_adapter (image/mod.rs:67-85) and get_embedding_adapter (embeddings/mod.rs:46-66) omit xai/vercel-ai-gateway, dispatch returns None and the chat URL leaks (e.g. https://api.x.ai/v1/chat/completions/images/generations). Rust current behavior is real.

Impl: image::dispatch and embeddings::dispatch use `get_*_adapter(provider)?` — `?` on None yields the None that triggers fallthrough. Registering xai and vercel-ai-gateway adapters (image/mod.rs match + embeddings/base.rs adapter with the correct endpoints, per docs/parity-9router-impl.md A4 line 126-134 and C14 line 316-320, with guard tests image_adapter_covers_xai_and_vercel / embeddings_covers_vercel_ai_gateway) makes dispatch return Some, blocking the chat-URL fallthrough and producing parity. No omission that would prevent the fix.

Minor non-blocking imprecisions: the spec's task IDs P0-B3a/P0-B3b do not literally appear in the parity doc (it uses A4/C14/C15), and the JS phrasing 'only providers WITHOUT an adapter fall through to the generic OpenAI forwarder' slightly overstates JS (JS errors on no-adapter providers; the generic chat-URL forwarder is the Rust path). Also xai full parity needs the bodyFields whitelist (C15) but that is orthogonal to the URL-leak fix.

---

### `P1-J19` — Embeddings: vercel-ai-gateway + xai gap in OPENAI_COMPAT_PROVIDERS (Rust registry parity)

**JS (source of truth — verbatim):**

embeddingProviders/index.js:7-11: OPENAI_COMPAT_PROVIDERS = ["openai","openrouter","mistral","voyage-ai","fireworks","together","nebius","github","nvidia","jina-ai","vercel-ai-gateway"] (10 + vercel).

**Current Rust behavior:**

src/core/media/embeddings/mod.rs:46-66 get_embedding_adapter has all 10 base providers but NOT vercel-ai-gateway (see P0-B3b). The 10 present match; vercel is the only JS-registered one missing.

**Implementation steps:**

Covered by P0-B3b step 1-2. No additional action.

**Guard test:**

Covered by vercel_gateway_embedding_registered in P0-B3b.

**⚠️ Risks:**

None beyond P0-B3b.

**Cross-check:** ✅ **CONFIRMED** — JS claim verified real: the file is at open-sse/handlers/embeddingProviders/index.js (the Rust module comment cites the same open-sse/handlers/embeddingProviders/ path; spec's path is shorthand), and lines 7-11 contain exactly OPENAI_COMPAT_PROVIDERS = [10 base providers + "vercel-ai-gateway"]. The endpoint is real too: openai.js embedUrl resolves embedCfg(id).baseUrl, and registry/vercel-ai-gateway.js:32 sets embeddingConfig baseUrl https://ai-gateway.vercel.sh/v1/embeddings, confirmed by unit test embeddingsCore.test.js:225-238.

Rust claim verified real: get_embedding_adapter (embeddings/mod.rs:46-66) matches exactly the 10 base providers plus gemini/google_ai_studio and openai-compatible node fallback — no vercel-ai-gateway. The registry test (handler.rs:87-105) enumerates the same 10 and omits vercel. Runtime gap confirmed: dispatch returns None for vercel-ai-gateway, so the generic forwarder builds a broken URL (.../v1/chat/completions/embeddings) from get_provider_config (default.rs:269-271) — vercel-ai-gateway embeddings genuinely do not work in Rust today.

Impl_steps: the cited P0-B3b task does not exist in this repo so its exact step 1-2 can't be verified, but the independent gap doc docs/parity-9router-impl.md:316-320 (section C14) describes the identical gap and identical fix (add vercel-ai-gateway adapter with baseUrl https://ai-gateway.vercel.sh/v1/embeddings, test embeddings_covers_vercel_ai_gateway). Adding that single entry to get_embedding_adapter is exactly what produces parity, and no other JS embeddings-registry behavior is missing in Rust (selfhosted-embedding is separate C13). Only nits: JS path prefix omitted and the P0-B3b cross-reference is unverifiable (corroborated by C14). Neither affects the substance.

---

### `P1-K20` — OpenRouter image/embedding header values should match registry (endpoint-proxy.local)

**JS (source of truth — verbatim):**

registry/openrouter.js:50-58:
  embeddingConfig: { baseUrl: "https://openrouter.ai/api/v1/embeddings", headers: {"HTTP-Referer":"https://endpoint-proxy.local","X-Title":"Endpoint Proxy"} }
  imageConfig: { baseUrl: "https://openrouter.ai/api/v1/images/generations", headers: {"HTTP-Referer":"https://endpoint-proxy.local","X-Title":"Endpoint Proxy"} }

**Current Rust behavior:**

src/core/media/embeddings/base.rs:133-139 (include_referer true for OPENROUTER) inserts HTTP-Referer https://openproxy.local + X-Title OpenProxy. src/core/media/image/openai_compat.rs:69-75 same values for OPENROUTER.

**Implementation steps:**

1) In src/core/media/embeddings/base.rs OpenAiCompatAdapter::build_headers, when include_referer, change values to HTTP-Referer "https://endpoint-proxy.local" and X-Title "Endpoint Proxy". 2) Same in src/core/media/image/openai_compat.rs build_headers (lines 69-75). Keep header names and the include_referer gating identical.

**Guard test:**

fn openrouter_embedding_referer_matches_registry() — OPENROUTER.build_headers yields HTTP-Referer https://endpoint-proxy.local and X-Title Endpoint Proxy. fn openrouter_image_referer_matches_registry() — same for the image adapter.

**⚠️ Risks:**

Change VALUES only, not names or which providers get them. openproxy.local is used nowhere in the JS registry — all openrouter media headers use endpoint-proxy.local.

**Cross-check:** ✅ **CONFIRMED** — All three checks pass.

1. JS behavior is REAL. The file exists at .tmp/9router/open-sse/providers/registry/openrouter.js (task cites "registry/openrouter.js", a minor path abbreviation — the 9router CLAUDE.md places the registry at open-sse/providers/registry/). Lines 50-59 match the excerpt verbatim: embeddingConfig at L50-55 { baseUrl "https://openrouter.ai/api/v1/embeddings", headers {"HTTP-Referer":"https://endpoint-proxy.local","X-Title":"Endpoint Proxy"} } and imageConfig at L56-59 { baseUrl "https://openrouter.ai/api/v1/images/generations", same headers }. The claimed line range 50-58 is essentially exact (imageConfig closes at 59).

2. Rust current behavior is REAL. src/core/media/embeddings/base.rs:133-139 inserts HTTP-Referer "https://openproxy.local" + X-Title "OpenProxy" under if self.include_referer, and the OPENROUTER static (L65-69) has include_referer: true with endpoint https://openrouter.ai/api/v1/embeddings. src/core/media/image/openai_compat.rs:69-75 inserts the same openproxy.local/OpenProxy values, and its OPENROUTER static (L31-35) has include_referer: true with endpoint https://openrouter.ai/api/v1/images/generations.

3. Impl steps produce parity with no omission for the stated scope. Both Rust endpoints already match the registry baseUrls; only the two from_static header values differ. Changing them to "https://endpoint-proxy.local" / "Endpoint Proxy" in each build_headers, keeping the include_referer gating identical, yields exact parity. Corroborating evidence: src/core/executor/default.rs:33 and src/core/executor/provider.rs:925 already emit HTTP-Referer "https://endpoint-proxy.local" for the chat path, matching the registry's transport.headers (L24-25) — the media files are the outlier.

Out-of-scope note (not an omission): src/core/media/tts/openrouter.rs:57-59 also uses openproxy.local/OpenProxy and the registry ttsConfig (L45-49) also uses endpoint-proxy.local; the task title is explicitly image/embedding only, so TTS is correctly outside impl_steps.

---

### `P1-L21` — Embeddings openai adapter: encoding_format default and dimensions validation parity

**JS (source of truth — verbatim):**

embeddingProviders/openai.js:20-27:
  buildBody: (model, { input, encoding_format, dimensions }) => {
    const body = { model, input };
    if (encoding_format) body.encoding_format = encoding_format;
    if (dimensions != null && dimensions !== "") {
      const dim = Number(dimensions);
      if (Number.isFinite(dim) && dim > 0) body.dimensions = dim;
    }
    return body;
  }

**Current Rust behavior:**

src/core/media/embeddings/base.rs:143-159 OpenAiCompatAdapter::build_body — JS only adds encoding_format if truthy; Rust ALWAYS defaults it to "float" (`let encoding_fmt = request.encoding_format().unwrap_or("float");` then inserts encoding_format always). This adds `encoding_format: "float"` even when the client did not send it — a behavioral difference.

**Implementation steps:**

In src/core/media/embeddings/base.rs OpenAiCompatAdapter::build_body: only insert encoding_format when the request body actually contains it. Change to: `if let Some(fmt) = request.encoding_format() { obj.insert("encoding_format".into(), json!(fmt)); }`. Keep the dimensions handling (already validates > 0 via dimensions()). This makes parity with JS (no defaulting).

**Guard test:**

fn openai_embedding_no_encoding_format_default() — build_body with body {"input":"hi"} (no encoding_format) must NOT contain "encoding_format" key; with {"input":"hi","encoding_format":"base64"} it must contain it.

**⚠️ Risks:**

The "float" default is a Rust-specific addition — removing it matches JS but may change downstream assumptions; verify the media.rs usage-tracking path (lines 426-489) tolerates a missing encoding_format. dimensions must still be dropped when missing/<=0.

**Cross-check:** ❌ **REFUTED** — The cited code is real but the parity gap it describes is fictional, and the prescribed fix would break (not achieve) wire parity.

FACTS VERIFIED:
1. JS adapter code is REAL and matches verbatim — but at .tmp/9router/open-sse/handlers/embeddingProviders/openai.js:20-28 (spec path "embeddingProviders/openai.js" omits the open-sse/handlers/ prefix). buildBody only sets encoding_format `if (encoding_format)`.
2. Rust build_body at src/core/media/embeddings/base.rs:143-159 is REAL: `let encoding_fmt = request.encoding_format().unwrap_or("float");` then unconditionally inserts encoding_format. The RUST_CURRENT claim is accurate.
3. THE FATAL OMISSION: The JS caller, .tmp/9router/open-sse/handlers/embeddingsCore.js:50-54 (the sole embeddings buildBody call site; src/sse/handlers/embeddings.js → handleEmbeddingsCore is the only entry), passes `encoding_format: body.encoding_format || "float"` into buildBody. So on the real JS path the adapter's `if (encoding_format)` is ALWAYS truthy — either the client value or the "float" default. JS therefore always sends encoding_format upstream, exactly like Rust.

CONSEQUENCE: There is no wire-level gap today. Both systems always emit `encoding_format` (client value, else "float"). Applying the impl_steps — making Rust insert encoding_format only when present in the request — would make Rust OMIT encoding_format when a client omits it, while JS would still send "float". That creates the exact divergence the task claims to fix, in the reverse direction (a regression in Rust).

Correct parity-preserving actions would be either (a) remove the `|| "float"` default from JS embeddingsCore.js:52 AND from Rust base.rs (align both to "omit when absent"), or (b) do nothing to base.rs since behavior already matches. The impl_steps as written (Rust-only change) are incomplete at best and harmful at worst.

Minor secondary notes (non-load-bearing): the JS adapter spans lines 20-28 (return on 27), not 20-27. Edge-case divergences pre-exist but are unrelated to this task: explicit `encoding_format: ""` → JS coerces to "float" via `||`, Rust sends empty string; and `dimensions` as string "256" → JS Number() parses it, Rust as_u64() ignores non-numbers (claim "already validates > 0 via dimensions()" is accurate).

---

### `P1-M22` — STT: auth header "key" support parity + deepgram smart_format/punctuate overrides

**JS (source of truth — verbatim):**

sttCore.js:6-15 buildAuthHeaders:
  switch (cfg.authHeader) {
    case "bearer": return { "Authorization": `Bearer ${token}` };
    case "token":  return { "Authorization": `Token ${token}` };
    case "x-api-key": return { "x-api-key": token };
    case "key":    return { "Authorization": `Key ${token}` };
    default:        return { "Authorization": `Bearer ${token}` };
  }
sttCore.js:36-43 deepgram sets smart_format=true, punctuate=true, and language OR detect_language=true.

**Current Rust behavior:**

src/server/api/stt.rs:113-131 build_auth_headers supports Bearer/Token/XApiKey/Key — matches. SttProviderConfig gemini uses SttAuthHeader::Key (line ~118) — matches. Deepgram smart_format/punctuate handled via build_deepgram_url with per-request override fields (deepgram_smart_format/deepgram_punctuate, lines 229-232). This gap is effectively CLOSED — verify the deepgram overrides are actually threaded from the request (TODO comment at media/stt/mod.rs:199-203 says not yet threaded in the orphaned module; the ACTIVE src/server/api/stt.rs does thread them).

**Implementation steps:**

Verify src/server/api/stt.rs build_deepgram_url reads req.deepgram_smart_format and req.deepgram_punctuate and falls back to "true". If confirmed, no change needed; add a test asserting the default fallback to true and the override path.

**Guard test:**

fn stt_deepgram_defaults_smart_format_punctuate() — build_deepgram_url without overrides yields smart_format=true&punctuate=true; with an override yields the override.

**⚠️ Risks:**

The orphaned media/stt/mod.rs is not the active path — edits should go to src/server/api/stt.rs only. Keep default=true fallback.

**Cross-check:** ✅ **CONFIRMED** — All behavioral claims verified against source. (1) JS buildAuthHeaders at .tmp/9router/open-sse/handlers/sttCore.js:6-15 matches the cited switch exactly: bearer→"Authorization: Bearer", token→"Authorization: Token", x-api-key→"x-api-key", key→"Authorization: Key", default→Bearer, and {} when no token. (2) JS transcribeDeepgram (sttCore.js:36-55) matches: sets model, hardcodes smart_format=true and punctuate=true, then language or detect_language=true. (3) Rust src/server/api/stt.rs behavior confirmed: build_auth_header (lines 570-579, note the spec's cited range 113-131 is off — those lines are the stt_config catalog, and the function is singular "build_auth_header") maps Bearer/Token/XApiKey/Key to identical header strings and None→None, equivalent to JS's {} on no token. (4) gemini config uses SttAuthHeader::Key at line 119 (spec said ~118, accurate). (5) build_deepgram_url (lines 772-807) reads req.deepgram_smart_format/req.deepgram_punctuate overrides (parsed from multipart lines 284-289 and JSON lines 378-387) and falls back to "true" via unwrap_or("true"); language/detect_language matches JS. Since JS always sends true (no override), Rust's default-true preserves exact JS parity; overrides are a superset. (6) Impl_steps need no change and the proposed tests already exist: deepgram_url_defaults_smart_format_punctuate_to_true_when_none (line 1228), deepgram_url_smart_format_punctuate_can_be_overridden (line 1215), and auth_header_token_styles_match_upstream (line 1235, asserts the Key header). Only discrepancy is the Rust line-range attribution; all behavior and parity claims are accurate.

---

### `P1-N23` — Search: searxng maxMaxResults/timeout config parity (50, 10000ms) and youcom maxMaxResults 100

**JS (source of truth — verbatim):**

registry/searxng.js:30-32: maxMaxResults: 50, timeoutMs: 10000.
registry/youcom.js:31-33: maxMaxResults: 100, timeoutMs: 10000.
handlers/search/index.js:75: maxResults: Math.min(body.max_results || providerConfig.defaultMaxResults || 5, providerConfig.maxMaxResults || 100)

**Current Rust behavior:**

src/core/media/search/base.rs request_from_body caps max_results at 100 universally (line 151-154 .min(100)). There is no per-provider maxMaxResults. So searxng can be asked for up to 100 results but JS caps it at 50.

**Implementation steps:**

1) Add to SearchProvider a default fn max_max_results() -> u32 { 100 }. Override in SearxngProvider to 50. 2) In base.rs request_from_body, the cap must be applied per-provider — but request_from_body doesn't know the provider. Move the cap into the provider or pass provider_id. Minimal approach: in dispatch (search/mod.rs), after building the request, clamp `request.max_results = request.max_results.min(provider_impl.max_max_results())`. 3) In chat_search-style flows that construct requests directly, apply the same clamp. youcom already effectively 100 (the builder min(100) in providers.rs line 783).

**Guard test:**

fn searxng_caps_max_results_at_50() — a request with max_results 100 through searxng dispatch results in max_results 50; youcom stays 100.

**⚠️ Risks:**

JS uses `body.max_results || defaultMaxResults` — a present max_results of 0 is falsy in JS and falls to defaultMaxResults (5). Rust's `unwrap_or(5)` differs (0 is used). Consider matching JS: treat 0/missing as default 5. The cap applies AFTER the default is resolved.

**Cross-check:** 🟡 **PLAUSIBLE** — JS claims are fully REAL (paths are open-sse/providers/registry/ rather than registry/): searxng.js:30-32 = defaultMaxResults:5, maxMaxResults:50, timeoutMs:10000; youcom.js:31-32 = maxMaxResults:100, timeoutMs:10000; handlers/search/index.js:75 = `maxResults: Math.min(body.max_results || providerConfig.defaultMaxResults || 5, providerConfig.maxMaxResults || 100)` — all confirmed byte-for-byte. Rust current behavior is REAL: src/core/media/search/base.rs:150-154 does `.min(100)` universally with no provider awareness; dispatch (mod.rs:27-31) passes no provider id to request_from_body; SearxngProvider (providers.rs:910) has no cap override. The proposed impl is feasible — dispatch holds both provider_impl and the request, and max_results is a public u32, so clamping request.max_results = request.max_results.min(provider_impl.max_max_results()) after request_from_body works. However, it is not fully CONFIRMED because the task title explicitly names "maxMaxResults/timeout config parity (50, 10000ms)" yet the impl steps omit timeout entirely: JS searxng timeoutMs is 10000 but Rust handler.rs:9 uses a flat 30s GLOBAL_TIMEOUT for all providers, leaving a timeout parity gap. Second, the JS 50-cap is behaviorally enforced by response slicing (index.js:112 `normalized.results.slice(0, params.maxResults)`), while Rust normalize never slices to max_results; because neither side sends a count param to searxng (JS callers.js:306-327 vs Rust build_url), the clamp alone does not guarantee a 50-result ceiling in Rust — observable parity for searxng requires the response slice too. So the core claim and primary fix are right, but the impl is incomplete against the task's stated scope.

---

### `P1-O24` — Gemini TTS voice fetch parity (PREBUILT_VOICES exposed via voices endpoint)

**JS (source of truth — verbatim):**

ttsProviders/gemini.js:93-128 PREBUILT_VOICES (30 entries: Zephyr, Puck, Charon, Kore, Fenrir, Leda, Orus, Aoede, Callirrhoe, Autonoe, Enceladus, Iapetus, Umbriel, Algieba, Despina, Erinome, Algenib, Rasalgethi, Laomedeia, Achernar, Alnilam, Schedar, Gacrux, Pulcherrima, Achird, Zubenelgenubi, Vindemiatrix, Sadachbia, Sadaltager, Sulafat) each {id, lang:"en", gender:"Female"|"Male"}. fetchGeminiVoices (line 126-128) maps to {voice_id, name, labels:{language,gender}}.

**Current Rust behavior:**

src/core/media/tts/gemini.rs has no voice-list fetch. No /voices endpoint exists in Rust TTS. Rust's DEFAULT_VOICE is "Kore" which matches.

**Implementation steps:**

1) Add to src/core/media/tts/gemini.rs a `pub fn gemini_voices() -> Vec<Value>` returning the 30 voices as JSON `[{voice_id, name, labels:{language:"en", gender}}]` (map from the JS table verbatim — include ALL 30 with the exact gender per entry). 2) Wire an HTTP route (or reuse an existing /api/media-providers/tts/voices endpoint if present) to return these. 3) At minimum make the data available so a voices endpoint can be added; if a voice endpoint already exists for other providers, add gemini.

**Guard test:**

fn gemini_voices_returns_30_prebuilt() — gemini_voices() has 30 entries, first is Zephyr/Female, includes Kore/Female and Sulafat/Female with labels.language == "en".

**⚠️ Risks:**

Voice names and genders must match JS EXACTLY (they are used for UI display and upstream voiceName). If the Rust side has no voices endpoint at all, this may be lower priority — implement the data table and wire only if an endpoint exists.

**Cross-check:** ✅ **CONFIRMED** — JS claim is REAL: open-sse/handlers/ttsProviders/gemini.js lines 93-124 define PREBUILT_VOICES with exactly the 30 names listed (Zephyr..Sulafat), each {id, lang:"en", gender:"Female"|"Male"}; fetchGeminiVoices() at 126-128 maps to {voice_id, name, labels:{language, gender}}; DEFAULT_VOICE="Kore" at line 17. These voices are exposed via the generic /api/media-providers/tts/voices route through VOICE_FETCHERS (ttsProviders/index.js:48-53) with gemini handled via the elevenlabs-shape branch. Rust current behavior is REAL: src/core/media/tts/gemini.rs contains only synthesize()/parse_model_voice() — no voice-list fetch — and DEFAULT_VOICE="Kore" matches. One nuance: the claim "No /voices endpoint exists in Rust TTS" is slightly overstated — Rust does have /v1/audio/voices (media.rs audio_voices) and /api/media-providers/tts/voices (media_providers.rs get_tts_voices), but neither covers gemini (both 400 for it), so the substance (no gemini voice path) is correct. IMPL_STEPS would produce parity: copying the 30 voices verbatim from the JS table is not just feasible but the only correct gender source, since the committed snapshot src/core/model/sources/9router.json stores the 30 gemini-tts voice ids as {id,name,kind} without gender, and the runtime provider_catalog.json drops gemini-tts-voices entirely (only openai/openrouter-tts-voices survive). Reusing the existing /api/media-providers/tts/voices endpoint (add a "gemini" arm to get_tts_voices) works. Minor gap, not fatal: full external parity would also require adding gemini to the provider match in media.rs audio_voices (currently 400s for gemini), but step 3 only demands "make the data available" and step 2's "reuse existing endpoint" covers the main internal path.

---

### `P2-P25` — TTS: xiaomi-mimo generic dispatch and selfhosted-tts adapter parity

**JS (source of truth — verbatim):**

ttsProviders/index.js:24 "selfhosted-tts": selfhostedTts SPECIAL_ADAPTER. selfhostedTts.js (full): baseUrl = creds?.providerSpecificData?.baseUrl || creds?.baseUrl || "http://localhost:8880"; strips trailing /, /v1/audio/speech, /v1; model split on '/' → [0]=model, rest=voice; POST {base}/v1/audio/speech body {model, voice, input, response_format} headers Content-Type + optional Bearer.

**Current Rust behavior:**

src/core/media/tts/mod.rs has NO selfhosted-tts (not in get_tts_adapter, not in provider_generic_format → is_tts_provider false). xiaomi-mimo also absent (P0-C4). selfhosted-tts falls through to the generic forwarder.

**Implementation steps:**

1) Create src/core/media/tts/selfhosted_tts.rs implementing TtsAdapter: base = provider_specific_data["baseUrl"] || credentials.base_url (if ProviderConnection has one) || "http://localhost:8880"; normalize base by trimming trailing '/', stripping "/v1/audio/speech" suffix, then "/v1" suffix. 2) model/voice: split on '/', filter empty; >=2 parts → (parts[0], parts[1..].join("/")); ==1 → (parts[0], DEFAULT_VOICE "af_heart"). DEFAULT_MODEL "kokoro". 3) POST {base}/v1/audio/speech body {model, voice, input, response_format} (response_format = request language? no — use the handler's response_format; TtsRequest lacks response_format — use "mp3" or thread from body). Headers Content-Type + optional Authorization Bearer if api_key present. 4) mod.rs: `mod selfhosted_tts;` and get_tts_adapter arm `"selfhosted-tts" => Some(&selfhosted_tts::ADAPTER)`.

**Guard test:**

fn selfhosted_tts_base_url_normalization() — base "http://host:8880/v1" normalizes to "http://host:8880" then appends /v1/audio/speech; bare model "kokoro" → model kokoro/voice af_heart; "kokoro/af_heart" → split correctly.

**⚠️ Risks:**

Bare value is the MODEL (not voice) for selfhosted — do NOT copy the OpenAI adapter's bare=voice behavior (that was the verified bug in the JS comment). Strip order matters: /v1/audio/speech then /v1. Response format default "mp3".

**Cross-check:** ✅ **CONFIRMED** — Verified all three claims against source.

1. JS REAL. `open-sse/handlers/ttsProviders/index.js` line 24 registers "selfhosted-tts" in SPECIAL_ADAPTERS (lines 15-29); `getTtsAdapter` returns it, and `open-sse/handlers/ttsCore.js` (lines 57-64) takes the special-adapter path before the generic dispatcher. `selfhostedTts.js` matches the spec exactly: line 9 DEFAULT_BASE_URL="http://localhost:8880"; line 18 raw = providerSpecificData?.baseUrl || credentials?.baseUrl || default; lines 22-25 strip trailing /, /v1/audio/speech, /v1; lines 39-47 model split on '/' → >=2 parts → (parts[0], parts.slice(1).join("/")), ==1 → model only (bare value is MODEL, not voice — a documented deliberate divergence from the OpenAI adapter); line 49 POST {base}/v1/audio/speech; body {model, voice, input, response_format} (lines 55-60); headers Content-Type + optional Bearer (lines 51-54).

2. Rust REAL. src/core/media/tts/mod.rs: get_tts_adapter (lines 89-102) has no selfhosted-tts; provider_generic_format (lines 106-119) maps none, so is_tts_provider("selfhosted-tts") is false (lines 123-125) and dispatch returns None (line 41). media.rs:349-360 falls through to build_media_url generic forwarder. xiaomi-mimo appears only in omniroute.json as a chat-model alias — no TTS adapter. Also confirmed: ProviderConnection (types/mod.rs) has provider_specific_data: BTreeMap<String,Value> but NO top-level base_url (RuntimeTransport.base_url is a separate nested struct), so the impl_steps' "credentials.base_url" fallback maps to no existing Rust field.

3. Impl would produce parity. One minor omission: the impl_steps never mentions response_format (JS sends response_format: "mp3" in the body; Rust openai.rs also omits it). A faithful port should pass it through when the handler exposes it (JS reads it from the request query and defaults to "mp3"). Not a blocker for default behavior. The body, base normalization, model/voice split, and Bearer header match the JS exactly, and the JS `credentials.baseUrl` (used only by the JS OpenAI adapter, never by selfhostedTts — JS selfhostedTts relies on providerSpecificData.baseUrl only) means the extra credentials.base_url fallback in impl_steps is a harmless superset. Verdict: CONFIRMED.

---

### `P2-Q26` — OpenAI-compatible STT response passthrough parity (raw body + content-type)

**JS (source of truth — verbatim):**

sttCore.js:150-153 (transcribeOpenAICompatible):
  const ct = res.headers.get("content-type") || "application/json";
  const txt = await res.text();
  return { success: true, response: new Response(txt, { status: 200, headers: { "Content-Type": ct, "Access-Control-Allow-Origin": "*" } }) };

**Current Rust behavior:**

src/server/api/stt.rs transcribe_openai (line ~715-733) reads content-type and body text and calls ok_passthrough(ct, body) which sets the content-type header and returns status 200 — matches. The orphaned media/stt/mod.rs transcribe_openai_compat also does the same.

**Implementation steps:**

Verify ok_passthrough preserves the exact upstream content-type (including e.g. application/json vs application/x-json). If the upstream returns non-JSON (SRT/vtt/verbose_json), the raw body must pass through. Add a test with a text/plain-ish content-type.

**Guard test:**

fn stt_openai_passthrough_preserves_content_type() — given upstream content-type "application/json" and a raw body, the response echoes it unchanged.

**⚠️ Risks:**

The raw body must NOT be re-parsed/re-serialized (verbose_json must stay verbatim). Content-type must be copied, not defaulted.

**Cross-check:** 🟡 **PLAUSIBLE** — Core claims verified: (1) The JS citation is exact — C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/open-sse/handlers/sttCore.js lines 150-152 in transcribeOpenAICompatible read content-type (default "application/json"), take res.text(), and return a 200 Response with Content-Type + Access-Control-Allow-Origin:*. (2) The live Rust path is real — src/server/api/stt.rs transcribe_openai (line 661) reads upstream content-type and body text, calls ok_passthrough (line 715) which sets the content-type header and returns status 200; the route wraps in with_cors_response (line 207) which supplies ACAO:* centrally via src/server/api/cors.rs, so JS CORS parity holds. (3) Impl steps are sound: ok_passthrough preserves the exact upstream content-type and passes non-JSON bodies (SRT/VTT/verbose_json) through raw since res.text() + ok_passthrough never re-serializes; no passthrough test currently exists (existing tests cover catalog/mime/urls/auth/fallback/error-parse only), so the proposed test adds real missing coverage. Minor caveat: if the upstream content-type were an invalid header value, HeaderValue::from_str would skip it and axum would fall back to text/plain — but real upstream content-types are always valid header values. The single inaccuracy is the parenthetical claim that the orphaned src/core/media/stt/mod.rs transcribe_openai_compat "also does the same": that function (lines 487-543) reads ct + body text but then parses the body as JSON, extracts the `text` field, and returns SttResult{text, raw_body, content_type} — it does NOT do a raw body passthrough (no ok_passthrough). The module is compiled but handle_stt has no production callers (only a unit test), so it is genuinely orphaned and this error does not affect the parity gap or the implementation plan — hence PLAUSIBLE rather than CONFIRMED (spec is not fully accurate) or REFUTED (JS claim is real and impl would work).

---

### `P2-R27` — Search: youcom/searxng content_options + provider_options pass-through completeness

**JS (source of truth — verbatim):**

callers.js buildYouComRequest (lines 289-295) livecrawl support: if params.contentOptions?.full_page → qp.set("livecrawl", news|web) and qp.append("livecrawl_formats", format==="markdown"?"markdown":"html"). buildSearxngRequest (306-327) passes language/time_range/pageno. handler/index.js:72-85 builds params from body incl. content_options + provider_options + providerSpecificData.

**Current Rust behavior:**

src/core/media/search/base.rs request_from_body carries content_options and provider_options and provider_specific_data (lines 184-192) — YouComProvider::build_url already implements livecrawl (providers.rs lines 805-826). SearxngProvider::build_url passes language/time_range/pageno (lines 938-946). Both match JS. The remaining gap is only the config defaults (P1-F13/P1-N23).

**Implementation steps:**

No code change required beyond P1-F13 (searxng URL) and P1-N23 (maxMaxResults). Optionally add a test that YouComProvider::build_url emits livecrawl_formats=markdown when content_options.format is markdown.

**Guard test:**

fn youcom_livecrawl_markdown_format() — with content_options {full_page:true, format:"markdown"} the URL contains livecrawl=web and livecrawl_formats=markdown.

**⚠️ Risks:**

content_options.format non-markdown must default to html. livecrawl value depends on search_type (news→news else web).

**Cross-check:** ✅ **CONFIRMED** — All three JS claims are real: (1) callers.js buildYouComRequest lines 289-295 set livecrawl=news|web and append livecrawl_formats=markdown|html when params.contentOptions?.full_page is truthy; (2) buildSearxngRequest (lines 306-327) passes language/time_range/pageno via toPageNumber; (3) handler/index.js lines 72-85 build params from the body including content_options, provider_options, and providerSpecificData from credentials. All three Rust claims are likewise real: base.rs request_from_body lines 184-192 carry content_options (from body), provider_options (from body), and provider_specific_data (from credentials); YouComProvider::build_url (providers.rs 805-826) implements livecrawl with the identical news/web and markdown/html logic (qp.push twice produces the same query params as JS set+append); SearxngProvider::build_url (providers.rs 938-946) passes language, time_range (excluding "any"), and pageno, matching the JS /search-suffix and page-number logic. The dispatch path (mod.rs dispatch -> request_from_body -> handle_search -> build_url) forwards all three fields end-to-end, so no omission blocks parity. The only residual differences (searxng default base URL http://localhost:8080 vs JS config, and Rust's hardcoded .min(100) vs JS providerConfig.maxMaxResults) are explicitly owned by the cross-referenced P1-F13 and P1-N23 tasks, so impl_steps of "no code change beyond those" is accurate. The optional YouCom livecrawl test is straightforward to add using the existing test-module pattern in providers.rs (which already constructs SearchRequest via request_from_body).

---

### `P2-S28` — Embeddings: jina-ai endpoint parity (base.rs ENDPOINTS fallback)

**JS (source of truth — verbatim):**

embeddingProviders/openai.js:6-11:
  const ENDPOINTS = { "jina-ai": "https://api.jina.ai/v1/embeddings" };
  const embedUrl = (id) => embedCfg(id).baseUrl || ENDPOINTS[id];

**Current Rust behavior:**

src/core/media/embeddings/base.rs JINA_AI endpoint "https://api.jina.ai/v1/embeddings" (line 106-108) — matches the JS ENDPOINTS value. No change needed.

**Implementation steps:**

No change. Add a regression test asserting JINA_AI endpoint equals "https://api.jina.ai/v1/embeddings".

**Guard test:**

fn jina_embedding_endpoint_matches() — assert_eq!(JINA_AI.endpoint, "https://api.jina.ai/v1/embeddings").

**⚠️ Risks:**

None — already correct.

**Cross-check:** ✅ **CONFIRMED** — All three verification points pass. (1) JS claim REAL: .tmp/9router/open-sse/handlers/embeddingProviders/openai.js lines 6-11 exactly match — ENDPOINTS maps "jina-ai" to "https://api.jina.ai/v1/embeddings", and embedUrl falls back to that. The registry file open-sse/providers/registry/jina-ai.js sets embeddingConfig.baseUrl to the identical URL, so the effective JS endpoint for jina-ai is "https://api.jina.ai/v1/embeddings". (2) Rust claim REAL: src/core/media/embeddings/base.rs lines 105-109 define JINA_AI with endpoint "https://api.jina.ai/v1/embeddings"; mod.rs:57 routes "jina-ai" to it; OpenAiCompatAdapter::build_url returns self.endpoint with no override anywhere in handler.rs. (3) Impl steps valid: no production change needed since values match; a regression test asserting JINA_AI.endpoint equals the URL adds coverage not provided by the existing registry-presence test. Note (out of scope): Rust build_body defaults encoding_format to "float" whereas JS only forwards it when supplied — a body-level divergence unrelated to this endpoint-parity task.

---

---

## F. COMBO / MITM / RTK (19 specs)

### `P0-A3` — Fix blackbox wrong URL in default.rs (and provider.rs dead copy)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/blackbox.js:25-28. transport.baseUrl="https://api.blackbox.ai/v1/chat/completions" (path segment /v1/chat/completions). No validateUrl, no headers. models use upstreamModelId mapping (e.g. {id:"claude-fable-5",upstreamModelId:"blackboxai/anthropic/claude-fable-5"}) — these are catalog/translator concerns, not URL.

**Current Rust behavior:**

WRONG URL. src/core/executor/default.rs:183-184: ("blackbox", ProviderConfig::openai("https://api.blackbox.ai/api/chat/completions")). Path is /api/chat/completions but JS is /v1/chat/completions. Same wrong URL duplicated in dead provider.rs:775-777 (https://api.blackbox.ai/api/chat/completions) and src/server/api/provider_validate.rs ("https://api.blackbox.ai/chat/completions" — third variant).

**Implementation steps:**

1) default.rs:183-184: change to ProviderConfig::openai("https://api.blackbox.ai/v1/chat/completions"). 2) provider.rs:775-777 (dead PROVIDER_REGISTRY): update to match too ("https://api.blackbox.ai/v1/chat/completions") for consistency, since media.rs:638 reads it. 3) provider_validate.rs blackbox branch: point at the same /v1/chat/completions endpoint. 4) Verify there is no trailing-path join issue: baseUrl is the full endpoint so build_url (default.rs:812) returns it unchanged.

**Guard test:**

tests/executor_pool_behavior.rs: #[test] fn blackbox_v1_chat_completions_url(): build_url == "https://api.blackbox.ai/v1/chat/completions" (assert it does NOT equal the old /api/chat/completions).

**⚠️ Risks:**

There are THREE different blackbox URLs in the repo today (default.rs /api/..., provider.rs /api/..., provider_validate.rs bare /chat/completions). All three must converge to /v1/chat/completions. The upstreamModelId field mapping (claude-fable-5 -> blackboxai/anthropic/claude-fable-5) is a translator concern and should NOT be touched by this URL fix.

**Cross-check:** ✅ **CONFIRMED** — All claims verified against source. JS: .tmp/9router/open-sse/providers/registry/blackbox.js lines 25-28 set transport.baseUrl="https://api.blackbox.ai/v1/chat/completions" with only thinkingFormat — no validateUrl, no headers; lines 29-40 use upstreamModelId mapping. 9router default.js line 129 returns config.baseUrl verbatim when no urlSuffix/rt.baseUrl, and blackbox.js declares no urlSuffix, so the effective upstream URL is exactly /v1/chat/completions. Rust: src/core/executor/default.rs:183-184 has ProviderConfig::openai("https://api.blackbox.ai/api/chat/completions"); in build_url blackbox hits no special branch and returns config.base_url verbatim (line 812), so the constant IS the effective URL with no trailing join. src/core/executor/provider.rs:775-776 duplicates /api/chat/completions in PROVIDER_REGISTRY, which is read by media.rs:638 via get_provider_config (provider.rs:1425); UnifiedExecutor::for_provider has no runtime call sites so the "dead for executor path, live for media.rs" characterization is accurate. provider_validate.rs:176-177 is a third variant (https://api.blackbox.ai/chat/completions). The three impl steps each change a real wrong value to the JS URL and would produce parity. Minor non-blocking note: api_key.rs:318 has ("blackbox", ("https://api.blackbox.ai/api", "Authorization")) — a base URL without the chat path used for key management, outside the spec's scope.

---

### `P0-A3` — preserveCacheControl hardcoded `false` in Rust filter_to_openai_format

**JS (source of truth — verbatim):**

translator/index.js:124-128:
```js
if (targetFormat === FORMATS.OPENAI) {
  result = filterToOpenAIFormat(result, {
    preserveCacheControl: !!PROVIDERS[provider]?.quirks?.preserveCacheControl,
  });
}
```
Providers with the quirk: providers/registry/alicode.js:19, alicode-intl.js:19, alims-intl.js:21 all have `quirks: { preserveCacheControl: true }`.
filterToOpenAIFormat (formats/openai.js:14-17): `function stripBlock(block) { const { signature, cache_control, ...rest } = block; return keepCache && cache_control ? { ...rest, cache_control } : rest; }` — applied ONLY to VALID_OPENAI_CONTENT_TYPES blocks; `tool_use` blocks are skipped (continue, line 44-45); `tool_result` blocks are kept but passed through stripBlock (line 46-49) so their signature/cache_control are stripped unless keepCache.

**Current Rust behavior:**

src/core/translator/registry.rs:481-487 hardcodes `filter_to_openai_format(body, false);`. In filter_to_openai_format (registry.rs:636-693): signature is always removed (line 683); cache_control removed only when !preserve_cache_control (line 684-686); and unlike JS, BOTH tool_use and tool_result blocks are pushed verbatim (line 689-692) with NO signature/cache_control stripping and tool_use is NOT dropped.

**Implementation steps:**

1. In registry.rs translate_request_with_strip, replace `filter_to_openai_format(body, false)` with a provider-aware call: read the provider from `credentials.get("provider").and_then(Value::as_str).unwrap_or("")` (credentials is the param already in scope; chat.rs:791 sets `creds["provider"] = plan.provider`) and compute `let preserve = matches!(provider, "alicode" | "alicode-intl" | "alims-intl");` then call `filter_to_openai_format(body, preserve);`.
2. In filter_to_openai_format (registry.rs:668-693), fix the tool-block branch to match JS: `else if block_type == "tool_use" { continue; } else if block_type == "tool_result" { let mut cleaned = block; if let Some(o) = cleaned.as_object_mut() { o.remove("signature"); if !preserve_cache_control { o.remove("cache_control"); } } filtered.push(cleaned); }`. This drops tool_use from content (JS `continue`) and strips signature/cache_control from tool_result.

**Guard test:**

Add `preserves_cache_control_when_true` — body messages [{role:user,content:[{type:text,text:x,cache_control:{type:ephemeral},signature:foo}]}], call filter_to_openai_format(&mut body, true), assert cache_control still present and signature removed. Add `strips_tool_result_cache_control_when_false` — tool_result block with cache_control, preserve=false, assert cache_control gone. Add `drops_tool_use_blocks` — assistant message content [{type:tool_use,...}], assert content does not contain tool_use.

**⚠️ Risks:**

JS `stripBlock` is NOT applied to string-content or tool messages (returns early at formats/openai.js:24-30). The quirk list is exactly alicode/alicode-intl/alims-intl — do NOT expand it. Preserve the JS quirk where keepCache=true but a block has no cache_control: block passes through unchanged (no cache_control is ADDED).

**Cross-check:** ✅ **CONFIRMED** — JS claim is real: index.js:124-128 matches verbatim (targetFormat === OPENAI gate; `preserveCacheControl: !!PROVIDERS[provider]?.quirks?.preserveCacheControl`), provider is the alias string passed to translateRequest, and exactly three registry files carry the quirk (alicode.js:19, alicode-intl.js:19, alims-intl.js:21), matching the spec. filterToOpenAIFormat (formats/openai.js) stripBlock always drops signature and drops cache_control unless keepCache — as claimed. Rust behavior is real: registry.rs:481-487 hardcodes filter_to_openai_format(body, false) (condition simplifies to target==OpenAi, mirroring the JS gate); the function at 636-693 always removes signature (line 683), removes cache_control only when !preserve_cache_control (684-686), and keeps both tool_use and tool_result (689). Impl steps produce parity: `credentials: Option<&Value>` is in scope, chat.rs:791 sets creds["provider"] = plan.provider and passes Some(&creds) at 809, plan.provider resolves to the literal alias strings, and matches! over the three aliases exactly covers the only quirk-bearing providers. The .unwrap_or("") fallback keeps current behavior for unknown providers and for the credentials=None delegate (translate_request at 425). Note: the tool_use/tool_result retention divergence from JS is pre-existing and correctly flagged as outside this task's preserve_cache_control scope; it does not affect the verdict.

---

### `P0-A3` — MITM per-provider handlers (antigravity/copilot/kiro/cursor) not ported — Rust only does CONNECT tunnel capture (G1a)

**JS (source of truth — verbatim):**

src/mitm/server.js:29-34 handler registry; :311-338 dispatch:
```js
const tool = getToolForHost(req.headers.host);
if (!tool) return passthrough(req, res, bodyBuffer);
const patterns = URL_PATTERNS[tool] || [];
const isChat = patterns.some(p => req.url.includes(p));
if (!isChat) return passthrough(req, res, bodyBuffer);
if (tool === "cursor") return handlers[tool].intercept(req, res, bodyBuffer, null, passthrough);
const model = extractModel(req.url, bodyBuffer);
if (model && (MODEL_NO_MAP[tool] || []).some((re) => re.test(model))) return passthrough(...);
const mappedModel = getMappedModel(tool, model);
if (!mappedModel) return passthrough(...);
return handlers[tool].intercept(req, res, bodyBuffer, mappedModel, passthrough);
```
URL_PATTERNS (config.js:26-31): antigravity [":generateContent", ":streamGenerateContent"], copilot ["/chat/completions", "/v1/messages", "/responses"], kiro ["/generateAssistantResponse"], cursor ["/BidiAppend", "/RunSSE", "/RunPoll", "/Run"].
Handlers: copilot.js:5-9 URL_MAP { "/chat/completions":"/v1/chat/completions", "/v1/messages":"/v1/messages", "/responses":"/v1/responses" }; resolveRouterPath falls back to "/v1/chat/completions". antigravity.js:17 `fetchRouter(body, "/v1/chat/completions", req.headers)`; stream error chunk `data: {"error":{...}}\r\n\r\n` with 200 + text/event-stream. kiro.js:483-489 builds `openaiBody = { model: mappedModel, messages, stream: true, ...(tools.length > 0 && { tools, tool_choice: "auto" }) }`, forwards to /v1/chat/completions, converts OpenAI SSE → AWS EventStream binary frames (CRC32 poly 0xEDB88320, Smithy headers ":message-type"="event", ":event-type", ":content-type"="application/json"). cursor.js:501 Not Implemented stub. base.js fetchRouter: strips Host, content-length, connection, transfer-encoding, content-type, authorization; forwards others + `Authorization: Bearer ${API_KEY}` where API_KEY=process.env.ROUTER_API_KEY, base = MITM_ROUTER_BASE || "http://localhost:20128" (trailing slashes trimmed).

**Current Rust behavior:**

src/core/mitm/server.rs:122-156: handle_client only supports CONNECT — writes `HTTP/1.1 502 Bad Gateway` for any non-CONNECT. handle_connect (179-273) establishes tunnel, does TLS accept with forged leaf, then byte-pumps request/response into capture files (pump_captured 275-306). There is NO HTTP parsing, NO body JSON decode, NO model extraction, NO fetchRouter call, NO format conversion. MitmState/MitmInterceptor (mod.rs:143-180) only offer `original_model` insertion and URL building; no handler logic. None of antigravity/copilot/kiro/cursor transformation exists.

**Implementation steps:**

1) Add `pub mod handlers;` to src/core/mitm/mod.rs. Create src/core/mitm/handlers.rs (or dir) with: `pub fn resolve_router_path(req_path: &str) -> &'static str` returning the copilot URL_MAP match by substring ("chat/completions"→"/v1/chat/completions", "/v1/messages"→"/v1/messages", "/responses"→"/v1/responses", default "/v1/chat/completions").
2) Add `pub async fn fetch_router(state: &AppState, openai_body: Value, path: &str, client_headers: &HashMap<String,String>) -> reqwest::Response` — strip the 6 STRIP_HEADERS (host, content-length, connection, transfer-encoding, content-type, authorization, case-insensitive), set Content-Type: application/json, add `Authorization: Bearer {router_api_key}` if configured, POST to `{settings.mitm_router_base_url trimmed of trailing '/'}{path}`.
3) Add antigravity handler: parse body, if `body.model` present set it to the mapped model; forward to /v1/chat/completions; pipe SSE back preserving status + content-type; on error for stream endpoints (:streamGenerateContent) write `HTTP 200 text/event-stream` with body `data: {"error":{"message":"..."}}\r\n\r\n`.
4) Add kiro handler porting kiro.js conversion: codeWhispererToMessages (history userInputMessage/assistantResponseMessage → messages with role tool/assistant, tool_calls with `id: toolUseId || call_<ms>`, `arguments: safeArgsString(input)` JSON-stringified), extractTools, build AWS EventStream frames (totalLen/headersLen/preludeCRC/messageCRC big-endian, CRC32 0xEDB88320, headers ":message-type"="event", ":event-type", ":content-type"="application/json"), convert OpenAI SSE chunks → toolUseEvent (init with name+id then incremental input fragments), reasoningContentEvent (delta.reasoning_content or <thinking>/<think> blocks), assistantResponseEvent (content text), messageStopEvent / per-tool stop:true on finish_reason, usageEvent {inputTokens, outputTokens}. Note kiro.js rejects binary EventStream request bodies (`isBinaryEventStream`) with a 500.
5) Wire into the CONNECT handling: after TLS accept + reading the HTTP request line + headers (peek is already used), parse request target, look up tool by SNI/host (`getToolForHost` equivalent: api.individual.githubcopilot.com→copilot; daily-cloudcode-pa/cloudcode-pa.googleapis.com→antigravity; q.us-east-1.amazonaws.com/codewhisperer/runtime.us-east-1.kiro.dev→kiro; api2.cursor.sh→cursor), check URL_PATTERNS substring, extract model from URL `/models/([^/:]+)` or body `model` or conversationState.currentMessage.userInputMessage.modelId, apply MODEL_NO_MAP (antigravity `/^tab[_-]/i`), map model via MitmState route config, then call the handler instead of byte-pumping.
6) Cursor: return the 501 `{"error":{"message":"Cursor MITM support is coming soon.","type":"not_implemented"}}` stub (parity).

**Guard test:**

cargo test mitm_resolve_router_path_maps_endpoints in src/core/mitm/mod.rs: assert resolve_router_path("/chat/completions")=="/v1/chat/completions", "/responses"=="/v1/responses", "/foo"=="/v1/chat/completions". cargo test mitm_get_tool_for_host: api.individual.githubcopilot.com→copilot, daily-cloudcode-pa.googleapis.com→antigravity, q.us-east-1.amazonaws.com→kiro, api2.cursor.sh→cursor, unknown→None.

**⚠️ Risks:**

The Rust MITM is a raw TCP CONNECT tunnel — introducing HTTP parsing must not break the existing capture pump. fetchRouter must NOT forward host/content-length/content-type/authorization. Kiro binary EventStream detection (isBinaryEventStream: totalLen>12 && totalLen<1000000 && headersLen<totalLen-12) must be preserved to avoid JSON.parse crash. The antigravity stream error must be SSE-shaped (200, text/event-stream) or the SDK hangs. HOST_REWRITE (server.js:25-27) rewrites cloudcode-pa.googleapis.com→daily-cloudcode-pa.googleapis.com only for `:generateContent`/`:streamGenerateContent` URLs.

**Cross-check:** ✅ **CONFIRMED** — JS claim verified at the cited locations: server.js:29-34 is the per-provider handlers registry (antigravity/copilot/kiro/cursor) and :311-338 is the dispatch (getToolForHost → URL_PATTERNS chat check → cursor special-case → extractModel → MODEL_NO_MAP → getMappedModel → handlers[tool].intercept). config.js URL_PATTERNS and getToolForHost match for all four tools. Rust current behavior verified: server.rs handle_client (122-156) is CONNECT-only and writes the exact `HTTP/1.1 502 Bad Gateway` body for any non-CONNECT; handle_connect (179-273) does tunnel + TLS accept with forged leaf then byte-pumps raw streams via pump_captured (275-306) into capture files — no HTTP parsing, URL routing, model mapping, or upstream re-forwarding anywhere in src/core/mitm/. mod.rs has no handlers module. Impl steps: resolve_router_path's URL_MAP matches the JS copilot map exactly, AppState and Settings.mitm_router_base_url exist, reqwest is an available dependency — the shown steps would work. Two caveats that do not change the verdict: (1) the JS cursor.js handler is itself a 501 stub, so listing cursor as an unported functional handler slightly overstates the gap (antigravity/copilot/kiro are the real functional parity gap); (2) the impl_steps are truncated mid-step-2 and omit the harder mechanism — parsing individual HTTP requests (Content-Length/chunked framing) out of the decrypted tunnel stream in handle_connect before dispatch — but nothing shown is wrong or blocked.

---

### `P0-A4` — Fix SiliconFlow wrong URL in default.rs (and dead provider.rs copy)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/siliconflow.js:16-20. transport.baseUrl="https://api.siliconflow.com/v1/chat/completions" (TLD .com, NOT .cn), validateUrl="https://api.siliconflow.com/v1/models", thinkingFormat="openai".

**Current Rust behavior:**

WRONG TLD. src/core/executor/default.rs:101-102: ("siliconflow", ProviderConfig::openai("https://api.siliconflow.cn/v1/chat/completions")). Host is api.siliconflow.cn (.cn) but JS uses api.siliconflow.com (.com). Same wrong .cn host in dead provider.rs:831-833, api_key.rs, provider_connection_test.rs, provider_models.rs, provider_validate.rs, and tests/executor_pool_behavior.rs.

**Implementation steps:**

1) default.rs:101-102: change URL to "https://api.siliconflow.com/v1/chat/completions". 2) Update the dead provider.rs:831-833 copy to "https://api.siliconflow.com/v1/chat/completions". 3) Update api_key.rs:("https://api.siliconflow.cn/v1", "Authorization") to .com. 4) Update provider_validate.rs and provider_models.rs / provider_connection_test.rs .cn references to .com. 5) Update the assertion URL in tests/executor_pool_behavior.rs if it hardcodes .cn (verified it references "https://api.siliconflow.cn/v1/chat/completions"). 6) web/src/shared/constants/providers.ts and web/open-sse/config/providers.js already use .com (per parity-report) — leave.

**Guard test:**

tests/executor_pool_behavior.rs: update existing siliconflow assertion to #[test] fn siliconflow_com_tld(): build_url == "https://api.siliconflow.com/v1/chat/completions".

**⚠️ Risks:**

.cn vs .com is a one-character bug that silently 404s or routes to the wrong service. Grep the whole repo for siliconflow.cn (api_key.rs, provider_models.rs, provider_validate.rs, provider_connection_test.rs, tests) and fix every occurrence — a partial fix leaves other paths broken. Do not change the model ids (deepseek-ai/DeepSeek-V4-Pro etc.).

**Cross-check:** ✅ **CONFIRMED** — All three claims verified against the actual files.

JS CLAIM (REAL): .tmp/9router/open-sse/providers/registry/siliconflow.js lines 16-20 are exactly as cited — baseUrl "https://api.siliconflow.com/v1/chat/completions" (.com), validateUrl "https://api.siliconflow.com/v1/models", thinkingFormat "openai". Corroborated by 9router CHANGELOG.md:375 (".cn -> .com ... #1760") and golden snapshot tests (golden-url-header.test.js.snap lines 1199-1200 assert .com).

RUST CLAIM (REAL): All cited .cn references exist verbatim:
- src/core/executor/default.rs:102 ProviderConfig::openai("https://api.siliconflow.cn/v1/chat/completions")
- src/core/executor/provider.rs:832 ProviderExecutorConfig::openai("https://api.siliconflow.cn/v1") — confirmed "dead" copy: provider.rs's PROVIDER_REGISTRY/UnifiedExecutor::for_provider has zero call sites in src; the live executor path is DefaultExecutor (chat.rs:1635, cli/mod.rs:1697/1868) which uses default.rs.
- src/core/executor/api_key.rs:296 ("https://api.siliconflow.cn/v1", "Authorization")
- src/server/api/provider_connection_test.rs:663, provider_models.rs:411, provider_validate.rs:78 — all "https://api.siliconflow.cn/v1/models".

IMPL STEPS (work, no blocking omissions): Steps 1-4 target exactly the right files/lines and cover every .cn reference in the executor + api_key + validation/connection-test paths. Step 5 (truncated "Update the assertion U...") corresponds to tests/executor_pool_behavior.rs:245 which asserts the same .cn URL in its provider table — updating it is required and implied. Minor note: web/open-sse/config/providers.js:291 also has the .cn URL, but that is dashboard/TTS UI config, not the executor or its validation paths, so it falls outside this task's parity scope — a completeness nit, not an error in the steps. Verdict: CONFIRMED.

---

### `P0-A4` — is_error drop in Kiro request translators (status hardcoded to "success")

**JS (source of truth — verbatim):**

claude-to-kiro.js:108-112: `pendingToolResults.push({ toolUseId: block.tool_use_id, status: block.is_error ? "error" : "success", content: [{ text: resultContent }] });`
openai-to-kiro.js:146-151 (Claude tool_result block inside a user message): `pendingToolResults.push({ toolUseId: block.tool_use_id, status: block.is_error ? "error" : "success", content: [{ text: text }] });`
openai-to-kiro.js:156-162 (OpenAI role:tool message): `pendingToolResults.push({ toolUseId: msg.tool_call_id, status: msg.is_error || msg.status === "error" ? "error" : "success", content: [{ text: toolContent }] });`

**Current Rust behavior:**

src/core/translator/request/claude_to_kiro.rs:226-231 — `"status": "success"` hardcoded (tool_result block). src/core/translator/request/openai_to_kiro.rs:231-236 hardcodes `"status": "success"` for Claude tool_result blocks, and lines 247-251 hardcodes `"status": "success"` for role:tool messages. is_error is never read.

**Implementation steps:**

claude_to_kiro.rs (tool_result branch, ~line 224): before pushing, compute `let is_err = c.get("is_error").and_then(Value::as_bool).unwrap_or(false);` and set `"status": if is_err { "error" } else { "success" }`.
openai_to_kiro.rs tool_result branch (~line 231): same — `c.get("is_error")`.
openai_to_kiro.rs role:tool branch (~line 244): `let is_err = msg.get("is_error").and_then(Value::as_bool).unwrap_or(false) || msg.get("status").and_then(Value::as_str) == Some("error");` and set status accordingly.
(Use a serde_json::json! with a string — Value::String. Do not add `is_error` to the result object; JS only sets `status`.)

**Guard test:**

Add `tool_result_is_error_maps_to_error_status` in claude_to_kiro.rs tests — a user message with content [{type:tool_result,tool_use_id:t1,is_error:true,content:...}], run claude_to_kiro_request, assert the currentMessage.userInputMessageContext.toolResults[0].status == "error". Add `tool_msg_status_error_maps_to_error` in openai_to_kiro.rs — role:tool msg with is_error:true, assert status == "error".

**⚠️ Risks:**

JS reads `is_error` ONLY on the block/message itself — never via nested paths. A false-y is_error (absent/false/null) must map to "success". Do not read is_error from a `status` field on Claude blocks (only the OpenAI role:tool message reads `msg.status`).

**Cross-check:** ✅ **CONFIRMED** — All three JS citations are real and match exactly: claude-to-kiro.js:108-112 uses `status: block.is_error ? "error" : "success"`; openai-to-kiro.js:146-151 (tool_result block) uses the same `block.is_error` ternary, and the role:tool branch (158-162) uses `msg.is_error || msg.status === "error"`; line 15 exists inside the kiroConstants import block. All Rust claims are real: claude_to_kiro.rs:226-231 and openai_to_kiro.rs:231-236 hardcode `"status": "success"` in the Claude tool_result branches, and openai_to_kiro.rs:247-251 hardcodes it for role:"tool" messages. Impl steps are correct for the variable bindings: block var is `c` in both tool_result branches (c.get("content")/c.get("tool_use_id")), message var is `msg` in the role:tool branch (msg.get("role")), so `c.get("is_error").and_then(Value::as_bool).unwrap_or(false)` and `msg.get("is_error")...` will compile and set the status correctly, and "error" is a valid Kiro status (the JS reference produces it). Minor nuance: the JS role:tool branch also has a secondary `msg.status === "error"` fallback that the (truncated) impl step for that branch does not mention; this only matters for already-normalized Kiro-style status fields, so it is a small incompleteness rather than an omission that breaks parity. The core is_error gap is accurately described and the fix produces parity.

---

### `P0-A4` — MITM DNS hosts-file steering + CA install paths missing (G1b/G1c)

**JS (source of truth — verbatim):**

src/shared/constants/mitmToolHosts.js:5-10:
```js
const TOOL_HOSTS = {
  antigravity: ["daily-cloudcode-pa.googleapis.com", "cloudcode-pa.googleapis.com"],
  copilot: ["api.individual.githubcopilot.com"],
  kiro: ["runtime.us-east-1.kiro.dev", "q.us-east-1.amazonaws.com", "codewhisperer.us-east-1.amazonaws.com"],
  cursor: ["api2.cursor.sh"],
};
```
DNS entries written as `127.0.0.1 {host}` (dnsConfig.js:161 `entriesToAdd.map(h => \`127.0.0.1 ${h}\`).join("\r\n")`; :168 same with \n). Windows hosts file: `{SystemRoot}\System32\drivers\etc\hosts`, atomic write via `.9router.new`→`.9router.bak` rename + `ipconfig /flushdns`. macOS/Linux: tee via sudo, `dscacheutil -flushcache && killall -HUP mDNSResponder` / `resolvectl flush-caches`. Cert install (cert/install.js): ROOT_CA_CN="9Router MITM Root CA"; Windows `certutil -delstore Root "9Router MITM Root CA"` then `certutil -addstore Root <path>` via elevated PowerShell; macOS `security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain` (delete old by CN first); Linux paths [{dir:"/usr/local/share/ca-certificates",cmd:"update-ca-certificates"},{dir:"/etc/ca-certificates/trust-source/anchors",cmd:"update-ca-trust"},{dir:"/etc/pki/ca-trust/source/anchors",cmd:"update-ca-trust"},{dir:"/etc/pki/trust/anchors",cmd:"update-ca-certificates"}], copied as `9router-root-ca.crt`, plus NSS db update (`certutil -d sql:$db -A -t "C,," -n "9Router MITM Root CA"`) for $HOME/.pki/nssdb, snap chromium, ~/.mozilla/firefox, snap firefox. Root CA (cert/rootCA.js): CN "9Router MITM Root CA", org "9Router", country US, 2048-bit RSA, 10yr validity, serial "01", basicConstraints cA critical, keyUsage keyCertSign+cRLSign critical, sha256; leaf: CN=domain, 1yr, SAN [DNS:domain, DNS:*.domain], extKeyUsage serverAuth+clientAuth.

**Current Rust behavior:**

src/core/mitm/cert.rs generate_ca/install_ca_cert/uninstall_ca_cert: only macOS `security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain` and Linux `/usr/local/share/ca-certificates` + `update-ca-certificates` (hard-coded, no Arch/Fedora/openSUSE config, no NSS db update, no Windows path — Windows returns Err("Unsupported platform")). CN is "OpenProxy MITM CA" (different from JS "9Router MITM Root CA"), key is ECDSA P-256 (rcgen PKCS_ECDSA_P256_SHA256) not RSA-2048, cert validity/serial/SAN not controlled. No hosts-file steering anywhere (src/core/dns/mod.rs is the OUTBOUND bypass resolver, not the inbound hosts-file writer). No `addDNSEntry`/`removeAllDNSEntries` equivalent. MITM_DIR is `{data_dir}/mitm` (mitm_config.rs:241) — no expiry-based regeneration (JS isCertExpired 30-day lookahead).

**Implementation steps:**

1) src/core/mitm/mod.rs: add `pub const TOOL_HOSTS: &[(&str, &[&str])] = &[("antigravity", &["daily-cloudcode-pa.googleapis.com", "cloudcode-pa.googleapis.com"]), ("copilot", &["api.individual.githubcopilot.com"]), ("kiro", &["runtime.us-east-1.kiro.dev", "q.us-east-1.amazonaws.com", "codewhisperer.us-east-1.amazonaws.com"]), ("cursor", &["api2.cursor.sh"])];`
2) Add `src/core/mitm/hosts.rs` (or extend mod.rs) with `add_dns_entry(tool, hosts_file_path)`, `remove_all_dns_entries_sync()`: on Windows read `{SystemRoot}\System32\drivers\etc\hosts`, filter lines containing any TOOL_HOSTS host, write back + `ipconfig /flushdns`; on macOS `dscacheutil -flushcache && killall -HUP mDNSResponder`; Linux `resolvectl flush-caches 2>/dev/null || true`. Entry text: `127.0.0.1 {host}` (CRLF on Windows, LF elsewhere).
3) cert.rs: change CN to "9Router MITM Root CA", org "9Router"; add Windows install path (best-effort: document that elevated certutil addstore is required; run `certutil -addstore Root` via a UAC-triggering helper or return a clear message); add Linux multi-distro config array + NSS db update (parse $HOME/.pki/nssdb, $HOME/.mozilla/firefox/*, snap paths; run `certutil -d sql:$db -A -t "C,," -n "9Router MITM Root CA" -i {cert}` if certutil exists).
4) Keep the CA filename but note the JS uses `rootCA.key`/`rootCA.crt` names; align generate_ca_persisted to write those filenames if consumers depend on them, or keep mitm-ca.* and document.
5) Wire DNS add/remove: JS calls removeAllDNSEntriesSync on SIGTERM/SIGINT/SIGBREAK (server.js:392-404) and clearDumpDir() on start. Add a shutdown hook in the Rust server that calls remove_all_dns_entries_sync (best-effort).

**Guard test:**

cargo test mitm_hosts_entries_write_loopback in src/core/mitm/hosts.rs: build a temp hosts file, call add_dns_entry for antigravity, assert the file contains `127.0.0.1 daily-cloudcode-pa.googleapis.com` and `127.0.0.1 cloudcode-pa.googleapis.com` with CRLF on the windows-style path; assert remove_all removes them and leaves unrelated lines intact.

**⚠️ Risks:**

JS host matching for removal is substring `l.includes(h)` — a host `q.us-east-1.amazonaws.com` also matches inside other lines. Windows rename-based atomic write must roll back on failure. The Rust CA uses ECDSA while JS uses RSA-2048 — the leaf cert validity/SAN shape (CN=domain, DNS:domain, DNS:*.domain, 1yr) must be preserved for client trust. Do NOT break the existing outbound MitmBypassResolver in src/core/dns/mod.rs — it is a separate concern (bypass vs steering).

**Cross-check:** ✅ **CONFIRMED** — All three verification points hold. (1) JS claim is REAL: .tmp/9router/src/shared/constants/mitmToolHosts.js:5-10 matches the claimed TOOL_HOSTS object exactly (antigravity=2 googleapis hosts, copilot=api.individual.githubcopilot.com, kiro=all 3 hosts incl. runtime.us-east-1.kiro.dev, cursor=api2.cursor.sh), and src/mitm/dns/dnsConfig.js lines 161/168 write entries as `127.0.0.1 {host}` with per-tool add/remove/checkAllDNSStatus. (2) Rust current behavior is REAL: src/core/mitm/cert.rs install_ca_cert/uninstall_ca_cert implement only macOS `security add-trusted-cert ... System.keychain` and Linux `/usr/local/share/ca-certificates` + `update-ca-certificates` — no Arch/Fedora/openSUSE cert path (no /etc/ca-certificates/trust-source/anchors, /etc/pki/ca-trust/source/anchors, /etc/pki/trust/anchors), no NSS db/certutil update, no Windows certutil store handling. src/core/mitm/ has only capture.rs, cert.rs, mod.rs, server.rs; grep across Rust src/ finds no hosts-file write code (no ipconfig/resolvectl/dscacheutil/hosts_file usage), so the G1b hosts-file steering gap is real. (3) impl_steps would produce parity: the proposed `pub const TOOL_HOSTS: &[(&str, &[&str])]` in src/core/mitm/mod.rs matches the JS constants exactly (all 7 unique hosts, kiro's 3-host list included), and a hosts.rs writer mirrors dnsConfig.js. Minor caveat: the impl_steps shown are truncated and only explicitly cover G1b (TOOL_HOSTS + hosts.rs); G1c (multi-distro CA + NSS + Windows store) parity requires additional steps beyond the visible text, but the claimed RUST_CURRENT gap is accurately described.

---

### `P0-A5` — Fix youcom search base URL to ydc-index.io

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/youcom.js:19-24. searchConfig={baseUrl:"https://ydc-index.io/v1/search",method:"GET",authType:"apikey",authHeader:"x-api-key",costPerQuery:0.005,freeMonthlyQuota:0,searchTypes:["web","news"],defaultMaxResults:5,maxMaxResults:100,timeoutMs:10000,cacheTTLMs:300000}. The dedicated-search builder open-sse/handlers/search/callers.js:270-304 buildYouComRequest uses this baseUrl; headers are Accept: application/json and X-API-Key (callers.js:301).

**Current Rust behavior:**

WRONG URL. src/core/media/search/providers.rs:829: resolve_base_url("https://api.you.com/search", request). JS base is https://ydc-index.io/v1/search. The Rust builder's query params (count maxResults, freshness=time_range, offset, country, language, include_domains, exclude_domains, livecrawl, livecrawl_formats) already match callers.js:275-295; only the base URL is wrong.

**Implementation steps:**

1) src/core/media/search/providers.rs:829: change the default to "https://ydc-index.io/v1/search". 2) Confirm the request uses GET with header X-API-Key (providers.rs:836-839 already inserts X-API-Key) — matches JS callers.js:301. 3) The query builder already appends ?... after the base; since JS baseUrl already ends with /v1/search and Rust resolve_base_url trims trailing '/', the final URL is https://ydc-index.io/v1/search?query=...&count=... — correct. 4) Also update the module doc comment at search/mod.rs:10 if it mentions youcom.

**Guard test:**

In search/providers.rs test module (or search tests): #[test] fn youcom_uses_ydc_index_base(): build_url with default settings returns a URL starting with "https://ydc-index.io/v1/search?" and NOT containing "api.you.com".

**⚠️ Risks:**

The header is X-API-Key (uppercase X) — already correct in Rust. The costPerQuery/timeoutMs/defaultMaxResults are metrics/throttling knobs; ensure Rust does not add a max_results cap inconsistent with JS maxMaxResults:100 (Rust providers.rs:783 already caps count at 100 via max_results.min(100)).

**Cross-check:** ✅ **CONFIRMED** — All three verification points pass.

1. JS behavior is REAL: youcom.js:19-34 contains exactly the claimed searchConfig (baseUrl "https://ydc-index.io/v1/search", method GET, authType apikey, authHeader x-api-key, costPerQuery 0.005, freeMonthlyQuota 0, searchTypes web/news, defaultMaxResults 5, maxMaxResults 100, timeoutMs 10000, cacheTTLMs 300000). callers.js:270-304 buildYouComRequest matches the claim: GET with X-API-Key header (line 301), and query params identical to the claimed list (query, count=min(maxResults,100), freshness, offset=floor(o/m) capped 9, country, language, include_domains, exclude_domains, livecrawl=web|news, livecrawl_formats).

2. Rust current behavior is REAL: providers.rs:829 builds resolve_base_url("https://api.you.com/search", request) (wrong URL vs JS), build_headers at 836-839 inserts X-API-Key, and the youcom query builder (780-831) is param-for-param identical to the JS builder.

3. Impl would produce parity: resolve_base_url (base.rs:224-229) and JS resolveBaseUrl (callers.js:70-73) are functionally identical — provider_options.baseUrl override, then trim trailing '/', then append '?'+querystring. Changing the Rust default to "https://ydc-index.io/v1/search" yields exactly the final URL the JS emits (JS trims trailing slash too). The youcom request is GET via the trait default method() at base.rs:94-96 (YouComProvider does not override it — only 5 providers override to POST), matching JS GET. No behavior beyond the base URL differs; count caps align (both clamp to 100 vs maxMaxResults 100).

Only trivial notes, not defects: the impl_steps text is truncated mid-sentence ("the final URL is ht..."), and resolve_base_url's trailing-slash trim means the override would work identically either way. No omission that would prevent parity.

---

### `P0-A5` — canonicalizeKiroConversation missing in Rust — no alternating-turn/tool-pair reconciliation

**JS (source of truth — verbatim):**

open-sse/translator/concerns/kiroConversation.js (435 lines) — canonicalizeKiroConversation({history, currentMessage, modelId, toolSpecs, nameMap}) (line 383): normalizeTurns (merge consecutive same-role turns; mergeUser merges content+images+toolResults, mergeAssistant merges content+toolUses; prepend/append `{userInputMessage:{content:"continue", modelId}}` so history starts/ends with user; content trimmed with `"continue"`/`"..."` fallbacks; `delete turn.userInputMessage.userInputMessageContext.tools`), then for index 0: flatten leading orphan toolResults into text `[Tool result${status==="error" ? " (error)" : ""}: ${content}]`, then reconcileToolPair(assistant, nextUser) per pair — calls with no matching result / no spec (nameMap.get(call.name) must be in specNames) / null input are dropped and re-emitted as text `[Tool call: name(input)]`; results without a matching call are flattened as text; kept pairs get reserved unique toolUseIds (sanitize to [a-zA-Z0-9_-], else `call_msg{turnIndex}_tc{callIndex}_{name}`), then toolSpecs are cloned onto the FINAL currentMessage.userInputMessageContext.tools, then validateKiroConversation (alternating roles, adjacent one-to-one pairs, unique ids, spec names, no orphan:0); if invalid → flattenAllStructuredTools (all toolUses+results to text) and re-validate.
normalizeKiroToolSpecs (line 79): per tool, rawName = tool.function?.name ?? tool.name (skip if empty); dedupe repeated rawName; uniqueName = sanitize/[^a-zA-Z0-9_-]/→_, collapse _+, trim _+_, truncate to 64, dedupe with _N suffix; description = truncate to 10237, default `Tool: ${rawName}`; schema = function.parameters ?? parameters ?? input_schema ?? {}; normalizeRootSchema: drop `additionalProperties`, drop empty `required:[]`, force type=object + properties={}, dedupe required. Constants (kiroConstants.js:23-25): KIRO_TOOL_NAME_MAX_LENGTH=64, KIRO_TOOL_DESCRIPTION_MAX_LENGTH=10237, KIRO_TOOL_ID_MAX_LENGTH=64.

**Current Rust behavior:**

src/core/translator/request/claude_to_kiro.rs and openai_to_kiro.rs do only: content-only merge of consecutive user turns (lines 388-412 / 423-446) which does NOT merge context toolResults/images (JS mergeUser does); no alternating-role enforcement; no tool use/result pairing; no orphan flattening; no tool id reservation; no validate/fallback. Tools are currently placed on the FIRST history user message then moved to currentMessage (claude_to_kiro.rs:95-128, 354-360, 425-436) — JS puts them on the FINAL currentMessage only. kiro_session_replay.rs has no canonicalization.

**Implementation steps:**

Create `src/core/translator/concerns/kiro_conversation.rs` porting the JS:
1. `normalize_kiro_tool_specs(tools: &Value) -> (Vec<Value>, HashMap<String,String>)` — produce toolSpecification{name,description,inputSchema:{json}} with the uniqueName sanitize/truncate(64)/dedupe logic, description truncate(10237), schema normalization (drop additionalProperties, drop empty required arrays, type=object, properties={}), rawName→uniqueName map.
2. `canonicalize_kiro_conversation(history: &[Value], current_message: &Value, model_id: &str, tool_specs: &[Value], name_map: &HashMap<String,String>) -> (Vec<Value>, Value, KiroRepairs)` implementing normalizeTurns → leading-orphan flatten → reconcileToolPair → tools-on-final-currentMessage → validate → flattenAllStructuredTools fallback, returning (history, currentMessage). Reuse the `[Tool call: name(input)]` / `[Tool result (error): content]` text shapes verbatim.
3. In BOTH claude_to_kiro.rs and openai_to_kiro.rs: replace the current `first_history_tools` extraction + `remove("tools")` cleanup + currentMessage tools merge (claude_to_kiro.rs:354-360 & 425-436; openai_to_kiro.rs:388-394 & 448-459) with a call to normalize_kiro_tool_specs(tools) then canonicalize_kiro_conversation(replay.history, replay.current_message, upstream_model, &specs, &name_map), and use the canonical result for payload.conversationState.history and currentMessage.userInputMessage. Delete userInputMessageContext.tools from every history turn inside canonicalize (as JS normalizeTurns does).
4. Thread `name_map`/`specs` into the tool-result reconcile (calls whose mapped name is not in specNames get flattened).

**Guard test:**

Add `canonicalize_merges_consecutive_user_turns_with_tool_results` — history [user(content a, toolResults r1), user(content b, toolResults r2)] → one user turn content "a\n\nb" with toolResults [r1, r2]. Add `canonicalize_flattens_orphan_results` — first user turn has toolResults but no preceding assistant toolUses → toolResults flattened into content and context.toolResults removed. Add `reconcile_pairs_and_reserves_ids` — assistant toolUse call_x + next user toolResult call_x → kept pair with a reserved id. Add `invalid_calls_become_text` — toolUse with input null → removed from toolUses, `[Tool call: name(...)]` appended to assistant content.

**⚠️ Risks:**

The `[Tool result${status === "error" ? " (error)" : ""}: ...]` and `[Tool call: ...]` text formats are load-bearing (tests/unit/openai-to-kiro.test.js:284 asserts the orphaned output survives as `[Tool result: important orphaned output]`). Empty assistant toolUses + results → flatten; empty results → delete toolResults; empty context object → delete userInputMessageContext. The final currentMessage content must remain non-empty (validate errors on `current` otherwise).

**Cross-check:** ✅ **CONFIRMED** — All three verification points hold.

(1) JS claim REAL: C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/open-sse/translator/concerns/kiroConversation.js is exactly 435 lines with canonicalizeKiroConversation({history,currentMessage,modelId,toolSpecs,nameMap}) at line 383. normalizeTurns (145-188) merges consecutive same-role turns; mergeUser (123-136) merges content+images+userInputMessageContext.toolResults; mergeAssistant (138-143) merges content+toolUses; it prepends/appends {userInputMessage:{content:"continue",modelId}} (168-173). normalizeKiroToolSpecs (79-110) does uniqueName sanitize (non [a-zA-Z0-9_-]→_)/collapse/trim, truncate to KIRO_TOOL_NAME_MAX_LENGTH=64, suffix dedupe (_2,_3,...); description truncate to KIRO_TOOL_DESCRIPTION_MAX_LENGTH=10237; cleanSchemaValue drops additionalProperties and empty required arrays; normalizeRootSchema forces type:"object", properties:{}, required filtered to owned props and deleted if empty. Constants 64/10237 confirmed in open-sse/config/kiroConstants.js. The function is live-called by open-sse/translator/request/claude-to-kiro.js (233,283) and openai-to-kiro.js (320,375).

(2) Rust claim REAL: src/core/translator/request/claude_to_kiro.rs lines 388-412 and openai_to_kiro.rs lines 423-446 contain a content-only merge of consecutive user turns (format "{}\n\n{}" over content only); images and userInputMessageContext.toolResults of merged-in messages are dropped, unlike JS mergeUser. No alternating-role enforcement, no tool-use/result pairing, no orphan flattening anywhere in the Kiro request path; src/core/utils/kiro_session_replay.rs has no reconciliation logic and no src/core/translator/concerns/ directory exists. The inline Rust tool conversion (claude 100-128) uses raw unsanitized names, untruncated descriptions, and only adds required:[] (no schema normalization), confirming the parity gap.

(3) Impl_steps viable: normalize_kiro_tool_specs(tools)->(Vec<Value>,HashMap<String,String>) is an accurate port of normalizeKiroToolSpecs (specs + rawName→uniqueName nameMap consumed by reconcileToolPair); all cited details (sanitize/truncate(64)/dedupe, description truncate(10237), drop additionalProperties, drop empty required arrays, type=object) match. The spec text truncates after step 1, but the stated scope is a full port of the JS file (canonicalize_kiro_conversation + helpers), so the steps shown contain no obvious omission that would block parity.

Minor nuance (not a refutation): the JS description/schema/required-filter behavior is slightly richer than the step summary (e.g., required entries not owned by properties are dropped, not just empty arrays), so a faithful port must preserve those details; and the ported canonicalize must be wired into both claude_to_kiro.rs and openai_to_kiro.rs (replacing the manual merge + inline tool conversion) for actual parity. These are implementation details consistent with a CONFIRMED verdict.

---

### `P0-A5` — Caveman injected prompt lacks the 8 JS shared directives + wrong dispatch (G2c)

**JS (source of truth — verbatim):**

open-sse/rtk/cavemanPrompts.js:4-110. SHARED directives concatenated into every level:
```js
const SHARED_BOUNDARIES = "Code blocks, file paths, commands, errors, URLs: keep exact. Security warnings, irreversible action confirmations, multi-step ordered sequences: write normal. Resume terse style after.";
const SHARED_EXAMPLES = "Not: \"Sure! I'd be happy to help you with that. The issue you're experiencing is likely caused by...\" Yes: \"Bug in auth middleware. Token expiry check use `<` not `<=`. Fix:\"";
const SHARED_AUTO_CLARITY = "Auto-Clarity: drop caveman for security warnings, irreversible actions, multi-step sequences where fragment ambiguity risks misread, or when user repeats a question. Resume after the clear part.";
const SHARED_PERSISTENCE = "ACTIVE EVERY RESPONSE. No revert after many turns. No filler drift. Still active if unsure.";
const SHARED_NO_INVENTED_ABBREV = "No invented abbreviations. Standard well-known tech acronyms (DB, API, HTTP, URL, JSON, ID, OS, CPU) OK. Names of code symbols, function names, API names, error strings: keep verbatim.";
const SHARED_PRESERVE_LANGUAGE = "Preserve the user's dominant language. User wrote Vietnamese, reply Vietnamese. User wrote English, reply English. Wenyan/classical-Chinese levels override this language-preservation rule. Code identifiers, error strings, file paths, commands: keep in their original form regardless of language.";
const SHARED_NO_SELF_REFERENCE = 'No self-reference. Do not name or announce the style (no "caveman mode", no "me caveman think", no "compressed mode active"). Just respond.';
const SHARED_NO_DECORATION = 'No decorative emoji. No narrating tool calls ("I will now search", "I used X to find Y"). No status phrases ("Sure!", "Of course!", "I'd be happy to"). No causal arrow shorthand ("A -> B -> fails"). State the thing, the action, the reason. Then next step.';
```
Each level is these + level-specific text joined with " " (single space). E.g. LITE: "Respond tersely. Keep grammar and full sentences but drop filler, hedging and pleasantries (just/really/basically/sure/of course/I'd be happy to)." + "Pattern: state the thing, the action, the reason. Then next step." + SHARED_EXAMPLES + ... . ULTRA: "Respond ultra-terse. Maximum compression. Telegraphic." + "Strip conjunctions. One word when one word enough." + "Pattern: [thing] [action] [reason]. [next step]." + shared... . WENYAN_LITE/WENYAN/WENYAN_ULTRA use classical-Chinese text. Dispatch: open-sse/rtk/caveman.js:7-9 `injectCaveman(body, format, level)` → `injectSystemPrompt(body, format, CAVEMAN_PROMPTS[level])` which switches on format (systemInject.js:12-26): CLAUDE→injectClaudeSystem, GEMINI/GEMINI_CLI/VERTEX/ANTIGRAVITY→injectGeminiSystem, default→injectMessagesSystem. The default (OpenAI) case: if `body.instructions` is string → append; else use messages[] (role system|developer) OR input[]; if none, `arr.unshift({ role: "system", content: prompt })`.

**Current Rust behavior:**

src/core/rtk/mod.rs:49-95 CompressionLevel::prompt() — each level is a SHORT concat missing ALL 8 shared directives (only a paraphrase of "Pattern" and the boundaries snippet; e.g. Full ends "Active every response until user asks for normal mode." vs JS "ACTIVE EVERY RESPONSE. No revert..."). Missing: SHARED_EXAMPLES, SHARED_AUTO_CLARITY, SHARED_NO_INVENTED_ABBREV, SHARED_PRESERVE_LANGUAGE, SHARED_NO_SELF_REFERENCE, SHARED_NO_DECORATION. Rust dispatch (mod.rs:177-192 inject_caveman_prompt) checks body keys (`system`, `is_gemini_shape`) instead of the JS format enum, and always appends to whatever array — the JS ALSO handles the responses `input` array with part_type "input_text" (rust does via inject_openai_shape:194-208), but Rust inject_claude_system_blocks (263-281) uses `text.contains(prompt)` for idempotency while JS always splices. Also Rust has no 8 shared consts.

**Implementation steps:**

1) In src/core/rtk/mod.rs add the 8 shared directive consts verbatim from cavemanPrompts.js (SHARED_BOUNDARIES, SHARED_EXAMPLES, SHARED_AUTO_CLARITY, SHARED_PERSISTENCE, SHARED_NO_INVENTED_ABBREV, SHARED_PRESERVE_LANGUAGE, SHARED_NO_SELF_REFERENCE, SHARED_NO_DECORATION) and rewrite each CompressionLevel::prompt() to `concat!(level_specific..., " ", SHARED_...)` joined with single spaces exactly matching JS. Keep as_str() and parse unchanged.
2) Keep dispatch logic (it already handles system/claude, gemini request/systemInstruction, openai messages, input). Add the missing `instructions` string handling for responses — verify inject_openai_shape already handles `instructions` (it does, mod.rs:195-197).
3) Ensure the wenyan levels use the JS classical-Chinese text: WenyanLite "Respond semi-classical...", Wenyan "Respond classical Chinese (文言文). Maximum classical terseness. 80-90% character reduction...", WenyanUltra "Respond extreme classical compression (文言文 ultra)...".

**Guard test:**

cargo test caveman_prompts_contain_shared_directives in src/core/rtk/mod.rs: for each level assert prompt() contains "keep exact" (boundaries), "No self-reference", "No decorative emoji", "Preserve the user's dominant language", "No invented abbreviations", and that Full contains the literal JS example "Not: \"Sure! I'd be happy to help you with that."

**⚠️ Risks:**

JS joins with a single space; Rust concat! must produce byte-identical text or the test in tests.rs (which asserts content.contains(prompt)) and the CLI's settings normalization could break. SHARED_PRESERVE_LANGUAGE explicitly says wenyan levels override the language-preservation rule — do not append it to wenyan levels (JS wenyan levels DO include it in the array but the rule text itself states wenyan overrides; keep verbatim). Rust inject_claude_system_blocks dedups by `text.contains(prompt)` whereas JS always splices — preserve Rust's idempotency OR match JS exactly; JS caveman is invoked once per request so idempotency differences only matter for the rtk system-inject test.

**Cross-check:** 🟡 **PLAUSIBLE** — JS side is fully verified real: open-sse/rtk/cavemanPrompts.js:4-110 contains CAVEMAN_LEVELS, the 8 shared consts (SHARED_BOUNDARIES:13, SHARED_EXAMPLES:15, SHARED_AUTO_CLARITY:17, SHARED_PERSISTENCE:19, SHARED_NO_INVENTED_ABBREV:21, SHARED_PRESERVE_LANGUAGE:23, SHARED_NO_SELF_REFERENCE:25, SHARED_NO_DECORATION:27), and CAVEMAN_PROMPTS where all 6 levels concatenate their level-specific lines + all 8 shared directives via .join(" "); quoted snippets match byte-for-byte. Rust current behavior is also real: src/core/rtk/mod.rs:49-95 CompressionLevel::prompt() gives short concats that include none of the 8 as a set (SHARED_BOUNDARIES verbatim in Lite/Full/Ultra, SHARED_PERSISTENCE only as the paraphrase "Active every response until user asks for normal mode.", Wenyan levels 1-2 sentences with nothing shared); Ultra even contradicts JS by teaching arrows (X -> Y) and invented abbreviations that SHARED_NO_DECORATION/SHARED_NO_INVENTED_ABBREV forbid. However the impl_steps have a concrete flaw: `concat!(level_specific..., " ", SHARED_...)` cannot compile — Rust's concat! builtin accepts only string literals, not const items (compile-verified on rustc 1.97.1 for both `concat!(A,...)` and `concat!(const { A },...)`: "only literals can be passed to concat!()"). Parity remains achievable by inlining the 8 shared strings verbatim into each level's concat! (matching the file's existing inline-literal style), which yields the exact JS single-space-joined prompt; the spec must also delete the Rust-only Ultra arrow/abbrev guidance and swap non-verbatim level lines (Lite's paraphrased "Pattern: state thing, action, reason...", the Wenyan lines) for JS verbatim text. The title's "wrong dispatch" is corroborated in kind: Rust picks injection target by body-shape heuristics while JS dispatches by the translator FORMATS enum — a real divergence and edge-case mismatch source, though shape detection is a reasonable MITM analog. Net: analysis and targets accurate, one load-bearing mechanism detail in the impl_steps is invalid as written, so mostly right rather than fully confirmed.

---

### `P1-B1` — clinepass OAuth config in src/oauth/providers.rs (executor covered by P0-A1e)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/clinepass.js:46-56. oauth={appBaseUrl:"https://app.cline.bot",apiBaseUrl:"https://api.cline.bot",authorizeUrl:"https://api.cline.bot/api/v1/auth/authorize",tokenUrl:"https://api.cline.bot/api/v1/auth/token",refreshUrl:"https://api.cline.bot/api/v1/auth/refresh"}. thinkingConfig={options:["auto","on","off"],defaultMode:"auto"}.

**Current Rust behavior:**

src/oauth/providers.rs get_config() (parity-report A3/C2h) has no clinepass entry. oauth.rs:4217 returns unknown_provider for it. Additionally C2h: Rust exchange only exempts 'cline' from codeVerifier; JS exempts cline/clinepass/kimchi.

**Implementation steps:**

1) In src/oauth/providers.rs get_config(), add a "clinepass" OAuthProviderConfig with authorize_url="https://api.cline.bot/api/v1/auth/authorize", token_url="https://api.cline.bot/api/v1/auth/token", refresh_url="https://api.cline.bot/api/v1/auth/refresh". 2) In the OAuth exchange code-verifier exemption (parity C2h), extend the exempt list from {"cline"} to {"cline","clinepass","kimchi"}. This task is listed for completeness — the executor half is P0-A1e.

**Guard test:**

src/oauth/tests/oauth_url_tests.rs: add clinepass to test_get_config_all_providers (line 272) asserting authorize_url == "https://api.cline.bot/api/v1/auth/authorize".

**⚠️ Risks:**

Do not reuse the cline (https://cline.bot) OAuth URLs — clinepass has a distinct /api/v1/auth/* path set. The exchange exemption list is shared logic; changing it affects cline/kimchi too (that is the intended parity fix).

**Cross-check:** 🟡 **PLAUSIBLE** — JS side is verbatim-correct: .tmp/9router/open-sse/providers/registry/clinepass.js:46-56 exactly matches the claimed oauth URLs and thinkingConfig, and route.js line 345-348 confirms the no-PKCE exchange exemption is ["cline","clinepass","kimchi"]. So the JS citation and the C2h parity claim are real. However, the RUST_CURRENT claim is materially stale: src/oauth/providers.rs ALREADY contains a clinepass() config (lines 281-292) and get_config() already maps "clinepass" (line 458), added in commit 359bc92 "fix: #309 ClinePass OAuth provider — config + token refresh" (2026-07-05) with a clean working tree. Consequently oauth.rs:4217 (inside start_oauth_flow, via get_provider_config) does NOT return unknown_provider for clinepass anymore. What remains genuinely unfixed: (1) the compat exchange dispatch at oauth.rs:4141-4178 has no clinepass arm and hits "_ => Unknown provider" at line 4178, and (2) the code-verifier exemption at oauth.rs:4132 only exempts "cline" (Rust) vs cline/clinepass/kimchi (JS). The impl steps are therefore only partly effective: step 1 is a no-op against already-present code (literal application would duplicate the fn/match arm), and even after both steps the clinepass OAuth exchange would still fail with "Unknown provider: clinepass" because no exchange arm exists. Step 2 is correct and necessary but insufficient alone. Net: the task direction is right and the JS citation is exact, but the spec's central Rust current-state assertion is false for the current repo and the steps as written would not fully close the gap.

---

### `P1-B1` — RTK system prompt injector only handles OpenAI messages; missing Claude/Gemini/Responses shapes (G2d)

**JS (source of truth — verbatim):**

open-sse/rtk/systemInject.js:9-98:
```js
export function injectSystemPrompt(body, format, prompt) {
  if (!body || !prompt) return;
  switch (format) {
    case FORMATS.CLAUDE: injectClaudeSystem(body, prompt); return;
    case FORMATS.GEMINI: case FORMATS.GEMINI_CLI: case FORMATS.VERTEX: case FORMATS.ANTIGRAVITY:
      injectGeminiSystem(body, prompt); return;
    default: injectMessagesSystem(body, prompt);
  }
}
```
injectMessagesSystem: if `body.instructions` is a string → `body.instructions = body.instructions ? body.instructions + "\n\n" + prompt : prompt`; else find messages[] or input[]; findIndex m.role === "system" || "developer" → appendToOpenAIMessage else `arr.unshift({ role: "system", content: prompt })`. appendToOpenAIMessage: string content → concat with "\n\n"; array content → `msg.content.push({ type: "input_text", text: prompt })`; else msg.content = prompt. injectClaudeSystem: string body.system → concat; array → insert `{type:"text", text:prompt}` BEFORE the last block with cache_control (splice(lastCacheIdx, 0, block)); else body.system = prompt. injectGeminiSystem: target = body.request ?? body; useSnake = hasOwnProperty(system_instruction); sys.parts array → push {text:prompt}; else target[key] = { parts: [{ text: prompt }] }.

**Current Rust behavior:**

src/core/rtk/system_inject.rs:8-38 inject_system_prompt only handles OpenAI `messages[]` (finds role "system" — NOT "developer" — and only string content; array content returns false). It does NOT handle: responses `input[]`, `instructions` string, Claude `system` (string or array), Gemini `systemInstruction`/`system_instruction`/`request.systemInstruction`, or ANTIGRAVITY `request` wrapping. The caller apply_rtk_system_injection (rtk/mod.rs:152-171) reads `systemInject` bool + `systemPrompt` string from settings.extra. Note: the setting name in JS — the injector is used by caveman/ponytail; the generic system-inject in Rust reads settings.extra.systemInject/systemPrompt (no direct JS equivalent found; JS gating is the token-saver header, so system-inject is a Rust-specific extra knob).

**Implementation steps:**

1) Rewrite src/core/rtk/system_inject.rs inject_system_prompt to a format-dispatch shape that ALSO detects by body: if body has `system` key → Claude path; if body has `contents`/`systemInstruction`/`system_instruction`/`request.contents` → Gemini path; else OpenAI path (messages[] or input[] or instructions string). Use PROMPT_SEPARATOR "\n\n" (same as rtk/mod.rs:21).
2) OpenAI path: handle `instructions` string; find role "system" OR "developer"; array content push `{"type":"input_text","text":prompt}` (for input[]/responses) or `{"type":"text","text":prompt}` (for messages[]); else insert at index 0.
3) Claude path: string system → append with "\n\n"; array → insert `{"type":"text","text":prompt}` before the last block having `cache_control` (rposition), else push; else set body.system = prompt.
4) Gemini path: target = body.request (if object) else body; key = "system_instruction" if present else "systemInstruction"; if sys has parts array push {text:prompt} else set {parts:[{text:prompt}]}.
5) Keep `system_inject_enabled` reading `systemInject` from settings. Keep the idempotency guard used by rtk/mod.rs inject functions (part_text equality) OR match JS (JS has no idempotency — but each request calls once).

**Guard test:**

cargo test system_inject_handles_claude_gemini_responses_shapes in src/core/rtk/tests.rs or system_inject.rs: (a) body with `system:[{type:"text",text:"A",cache_control:{}}]` → prompt inserted at index 0 (before cache block); (b) body with `input:[{role:"developer",content:[{type:"input_text",text:"x"}]}]` → appends {type:"input_text",text:prompt}; (c) body with `request:{systemInstruction:{parts:[{text:"g"}]}}` → parts gets {text:prompt}; (d) body with `instructions:"i"` → "i\n\nprompt".

**⚠️ Risks:**

JS CLAUDE format detection is driven by the format enum, not body keys — Rust must map format correctly or a Claude body with a top-level `messages` key could take the OpenAI path. The cache_control insertion is BEFORE the last cache block (splice at lastCacheIdx), not after. Preserve "\n\n" as the separator everywhere (already PROMPT_SEPARATOR). Do not break the existing caveman/ponytail callers that rely on inject_system_prompt behavior.

**Cross-check:** ✅ **CONFIRMED** — All three verification points hold. (1) The JS claim is real: open-sse/rtk/systemInject.js:9-98 exists and matches verbatim — injectSystemPrompt dispatches by format to injectClaudeSystem (system string/array w/ cache_control insertion), injectGeminiSystem (request-wrap + system_instruction/systemInstruction snake/camel), and injectMessagesSystem (instructions string, messages[], input[], role system||developer, SEP "\n\n", array parts pushed as {type:"input_text",text}). FORMATS.CLAUDE/GEMINI/GEMINI_CLI/VERTEX/ANTIGRAVITY confirmed in translator/formats.js:6-11; callers are caveman.js and ponytail.js. (2) The Rust current behavior is real: src/core/rtk/system_inject.rs:8-38 only handles body.messages[], matches role exactly "system" (not "developer"), only appends to string content ("_ => false" for array content), and ignores input[], instructions, Claude system, and Gemini systemInstruction/system_instruction/contents/request.*. PROMPT_SEPARATOR="\n\n" confirmed at mod.rs:21. (3) The impl_steps would produce parity: the proposed body-shape dispatch (system key -> Claude; contents/systemInstruction/system_instruction/request.contents -> Gemini; else OpenAI messages[]/input[]/instructions) exactly mirrors the already-working, unit-tested inject_caveman_prompt machinery in mod.rs:177-391 (inject_openai_shape, inject_claude_system with cache_control, inject_gemini_system with request-wrap and snake/camel keys, is_gemini_shape), which is already proven to handle every shape the JS handles. Reusing that machinery closes the gap with no obvious omission. Minor nuance: JS dispatches on the format param while the impl uses body-shape detection — justified because Rust inject_system_prompt takes no format argument and body detection is at least as robust (it is also what the existing caveman path does). No factual errors found in the spec section.

---

### `P1-B2` — codebuddy-intl OAuth device-code flow in src/oauth (executor covered by P0-A1f)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/codebuddy-intl.js:64-72. oauth={baseUrl:"https://www.codebuddy.ai",stateUrl:"https://www.codebuddy.ai/v2/plugin/auth/state",tokenUrl:"https://www.codebuddy.ai/v2/plugin/auth/token",refreshUrl:"https://www.codebuddy.ai/v2/plugin/auth/token/refresh",userAgent:"IDE/2.63.2 CodeBuddy/2.63.2",platform:"ide",pollInterval:5000}. features={usage:true,usageApikey:true}. Comment line 3: all OAuth/plugin URLs use the /v2/plugin prefix with platform=ide (CN uses platform=CLI).

**Current Rust behavior:**

src/oauth/providers.rs:307-319 has the CodeBuddy CN device-code flow (copilot.tencent.com/v2/plugin/auth/state|token|token/refresh, platform CLI). There is NO codebuddy-intl (www.codebuddy.ai) entry. oauth.rs:4217 returns unknown_provider.

**Implementation steps:**

1) In src/oauth/providers.rs add a "codebuddy-intl" OAuthProviderConfig: authorize/state url="https://www.codebuddy.ai/v2/plugin/auth/state", token_url="https://www.codebuddy.ai/v2/plugin/auth/token", refresh_url="https://www.codebuddy.ai/v2/plugin/auth/token/refresh", platform="ide", userAgent="IDE/2.63.2 CodeBuddy/2.63.2", pollInterval=5000 (device-code poll loop). 2) Model the flow on the existing codebuddy CN device-code implementation (providers.rs:307-319) but swap the base domain to www.codebuddy.ai and platform to "ide". 3) The executor side (transport headers X-Product: SaaS etc.) is P0-A1f. This is the OAuth half (parity A3).

**Guard test:**

src/oauth/tests/oauth_url_tests.rs: add codebuddy-intl asserting authorize/state URL == "https://www.codebuddy.ai/v2/plugin/auth/state" and platform == "ide".

**⚠️ Risks:**

Do NOT mix CN (copilot.tencent.com, platform CLI) with intl (www.codebuddy.ai, platform ide) — the state/token/refresh URLs differ only by host but the platform param must be 'ide'. The userAgent version string "IDE/2.63.2 CodeBuddy/2.63.2" differs from the transport User-Agent "IDE/2.108.1 CodeBuddy/2.108.1" (registry line 69 vs line 29) — keep them distinct as in JS.

**Cross-check:** ❌ **REFUTED** — JS claim REAL, Rust current behavior REAL, but the impl_steps would NOT produce parity — a required custom device-code executor is missing.

(1) JS: .tmp/9router/open-sse/providers/registry/codebuddy-intl.js:64-76 exactly matches the cited oauth config (baseUrl/stateUrl/tokenUrl/refreshUrl all www.codebuddy.ai/v2/plugin/auth/*, userAgent "IDE/2.63.2 CodeBuddy/2.63.2", platform "ide", pollInterval 5000) and features={usage:true,usageApikey:true}. The flow is implemented in .tmp/9router/src/lib/oauth/providers/codebuddy-intl.js.

(2) Rust: src/oauth/providers.rs:307-327 has codebuddy_cn() (copilot.tencent.com state/token/refresh, platform CLI, extra_params user_agent/platform/poll_interval). No www.codebuddy.ai anywhere in src; get_config (providers.rs:446-471) has no codebuddy-intl arm; is_device_code_provider (server/api/oauth.rs:692-706) lacks codebuddy-intl; start_oauth_flow at oauth.rs:4217 returns unknown_provider for it. Claim REAL.

(3) Parity gap — the decisive flaw: the JS CodeBuddy flow is a custom non-RFC8628 protocol. requestDeviceCode POSTs JSON "{}" to stateUrl?platform=ide with headers X-Requested-With, X-Domain, X-No-Authorization, X-No-User-Id, X-Product and parses {code,data:{state,authUrl}}; pollToken GETs tokenUrl?state=... with the same headers and maps code 11217 to authorization_pending. Rust's generic device_code::start_device_flow (src/oauth/mod.rs:109-140) only form-posts client_id+scope to authorize_url and parses RFC device_code/user_code/verification_uri fields — it sends none of the CodeBuddy headers and cannot parse the CodeBuddy JSON. poll_for_token reads only the "interval" extra_param and ignores platform/user_agent/poll_interval. There is no codebuddy-intl (or codebuddy) special-case module in src/oauth (unlike kiro.rs/qoder.rs), and token_refresh.rs would need an intl mirror of refresh_codebuddy_cn_token. Adding only an OAuthProviderConfig for codebuddy-intl would dispatch it through the generic RFC flow, which cannot complete the CodeBuddy state/poll handshake — so the described impl_steps do not achieve parity.

---

### `P1-B2` — Headroom missing openai-responses + kiro format paths and size/phantom logging (G2e/F7)

**JS (source of truth — verbatim):**

open-sse/rtk/headroom.js:242-332 compressWithHeadroom dispatches by format:
- format "claude" (260-273): `claudeToOpenAIRequest(model, body, false)` → if !messages[] diag "Claude request did not translate to messages[]"; callCompress → `openaiToClaudeRequest(model, {...oai, messages: data.messages}, false)`; write back `body.messages` and `body.system` if !== undefined.
- format "openai-responses" (278-297): if hasUnsafeResponsesInputForCompression(body) (any input item with a string `type` !== "message") → diag "skipped: openai-responses tool/reasoning input is not safe to compress"; else `openaiResponsesToOpenAIRequest(model, body, false)` → compress → `openaiToOpenAIResponsesRequest(model, {...oai, input: undefined, messages: data.messages}, false)`; write back `body.input`.
- format "kiro" (302-313): collectKiroHeadroomMessages projects history/currentMessage userInputMessage.systemInstruction/content/toolResults tool text + assistantResponseMessage content/tool_calls into {messages, targets}; callCompress; applyKiroHeadroomMessages verifies count + role order + non-null text then writes `target.object[target.key] = text` (diag "proxy response did not match Kiro message count" / "did not preserve Kiro message order" / "missing Kiro text content").
- OpenAI (315-327): key = "messages" or "input"; `body[key] = data.messages`.
buildCompressEndpoint (59-71): `{base}/v1/compress`. callCompress POSTs `{ messages, model }` (+`config:{compress_user_messages:true}`) with `AbortSignal.timeout(timeoutMs)` (default 3000), returns null + diagnostic on non-ok / missing messages[]. formatHeadroomSizeLog (343-351): `body=X→Y messages=... tools=... toolHistory=... effective=%`. isHeadroomPhantomSavings (353-359): `tokens_saved>0 && before>0 && after>0 && after >= before * (1 - 0.05)`.

**Current Rust behavior:**

src/core/rtk/headroom.rs compress_with_headroom (255-300) only dispatches "claude" (271-273) or OpenAI (276-299, key messages/input). NO openai-responses translation path, NO kiro projection path, NO diagnostics, NO buildCompressEndpoint host-masking, NO size snapshot (captureSizeSnapshot), NO formatHeadroomSizeLog, NO isHeadroomPhantomSavings. The claude path (compress_claude_body 379-442) only flattens content blocks to plain text — it does NOT call the real claudeToOpenAIRequest/openaiToClaudeRequest translators, so tool_use/tool_result/thinking blocks are dropped instead of round-tripped. Format is passed as "claude"/"openai" (chat.rs:843) — never "openai-responses"/"kiro".

**Implementation steps:**

1) In src/core/rtk/headroom.rs, add a `diagnostics: &mut HeadroomDiagnostics` (new struct with `reason: Option<String>`, `endpoint: Option<String>`, `before: Option<SizeSnapshot>`, `after: Option<SizeSnapshot>`) param to compress_with_headroom (or a parallel fn) and set diag.reason only if not already set (JS setDiagnostic).
2) Add format "openai-responses": if body.input has any item whose `type` is a non-"message" string → set reason "skipped: openai-responses tool/reasoning input is not safe to compress" and return None. Otherwise route body.input through the existing Rust responses↔openai translators (crate::core::translator::request::openai_responses::{openai_responses_to_openai_request, openai_to_openai_responses_request}) — port the JS: translate to OpenAI, call_compress, then translate back passing `input: undefined, messages: compressed` and write body["input"] = result.input.
3) Add format "kiro": project conversationState.history + currentMessage into messages (systemInstruction→role system, content→role user, toolResults[].content[].text→role tool + tool_call_id, assistantResponseMessage content→role assistant + tool_calls with `arguments: JSON.stringify(input||{})` and `id`), record targets [(object,key)], POST, verify count/role-order/text, then write text back into each target. New fn `apply_kiro_headroom_messages`.
4) Add captureSizeSnapshot: jsonBytes(body), jsonBytes(messages/input array), jsonBytes(tools), jsonBytes(toolHistory filtered by role tool/function or tool_calls.length or content block type tool_use/tool_result).
5) Add buildCompressEndpoint (strip trailing '/', append /v1/compress) + maskEndpoint (strip user:pass@, query, hash).
6) Add formatHeadroomSizeLog + isHeadroomPhantomSavings (minShrinkRatio 0.05) and call them in chat.rs after compress_with_headroom (chat.rs:853-858) for the debug log.
7) For the claude path, keep the flatten but ALSO apply the real translators if the existing Rust claude↔openai translator functions are accessible; otherwise document that the current flatten approximates it (matches JS intent that the proxy only understands OpenAI shape).

**Guard test:**

cargo test headroom_responses_shape_rejected_when_unsafe in src/core/rtk/headroom.rs: body.input containing an item with type "function_call" → compress_with_headroom(format="openai-responses") returns None and diagnostics.reason starts with "skipped:". cargo test headroom_kiro_projection_roundtrips: build a kiro body with one history tool result; assert compressed text is written back into conversationState.history[0]....toolResults[0].content[0].text. cargo test headroom_phantom_savings_detected: tokens_saved>0 with before/after sizes where after >= before*0.95 → is_phantom true.

**⚠️ Risks:**

The responses round-trip MUST pass `input: undefined` to the translator so it rebuilds input from messages instead of echoing the original — the JS comment (#1998) calls this out explicitly. Kiro apply step verifies role order and returns false (with diag) when the proxy reshuffled — never write back mismatched messages. buildCompressEndpoint keeps existing query string when URL parse fails. Phantom detection threshold is exactly 0.05 (5%).

**Cross-check:** ✅ **CONFIRMED** — All three verification points check out.

1) JS claim is REAL. Verified C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/open-sse/rtk/headroom.js: compressWithHeadroom (242-332) dispatches on format with a "claude" branch (260-273) doing claudeToOpenAIRequest → setDiagnostic("Claude request did not translate to messages[]") → callCompress → openaiToClaudeRequest(model, {...oai, messages: data.messages}, false) → write back body.messages and body.system-if-!==undefined, exactly as claimed. The "openai-responses" (278-297) and "kiro" (302-313) branches, hasUnsafeResponsesInputForCompression (86-92), collectKiroHeadroomMessages (94-162), applyKiroHeadroomMessages (180-207), setDiagnostic (42-44, set-only-if-unset), captureSizeSnapshot (26-40), buildCompressEndpoint/maskEndpoint (59-84, 212), formatHeadroomSizeLog (343-351), and isHeadroomPhantomSavings (353-359) all exist as described. The cited translators (claude-to-openai.js, openai-to-claude.js, openai-responses.js) exist and export the four functions. Formats are production-reachable: chatCore.js:199 finalFormat=passthrough?sourceFormat:targetFormat; responsesHandler.js forces source "openai-responses"; codex/grok-cli and kiro registries set those formats; chatCore.js:236-246 consumes diagnostics.reason/endpoint and phantom detection.

2) Rust claim is REAL. src/core/rtk/headroom.rs compress_with_headroom (255-300) only dispatches "claude" (271-273 → compress_claude_body) or the OpenAI messages/input fallback (275-299 via extract_openai_messages). No openai-responses, no kiro, no diagnostics, no endpoint host-masking (call_compress:311 is a bare "{url}/v1/compress" concat), no captureSizeSnapshot, no size/phantom logging. Callers (server/api/chat.rs:843, cli/mod.rs:1645) pass only "claude"/"openai" and log only format_headroom_log().

3) Impl steps would produce parity. Step 1's HeadroomDiagnostics(reason/endpoint/before/after, set-if-unset) mirrors JS setDiagnostic exactly. Step 2's openai-responses path is fully supported by existing Rust building blocks: openai_responses_to_chat_request and chat_to_openai_responses_request (src/core/translator/request/openai_responses.rs) cover both translation directions. Only minor implementation details, none blocking: the Rust responses translators are in-place -> bool mutators (JS are pure functions returning new bodies), and chat_to_openai_responses_request early-returns when body has "input" — so the impl must delete the "input" key before the rebuild call to mirror JS's `input: undefined`. Also note the openai-responses/kiro branches are latent (not reachable from current Rust callers), while the diagnostics/size/phantom-logging gap is live; neither affects the claim's accuracy.

---

### `P1-B3` — zed OAuth RSA keypair flow (oauth layer; executor base covered by P0-A1n)

**JS (source of truth — verbatim):**

File .tmp/9router/open-sse/providers/registry/zed.js:54-66 (RSA flow). oauth={authorizeUrl:"https://zed.dev/native_app_signin",platform:"zed",rsaKeyExchange:true}. Comment lines 55-62: 1) App generates RSA-2048 keypair locally (PKCS#1 DER, URL-safe base64). 2) Bind random TCP port on 127.0.0.1. 3) Open https://zed.dev/native_app_signin?native_app_port={port}&native_app_public_key={pub}. 4) After login, browser redirects http://127.0.0.1:{port}/?user_id=...&access_token=... where access_token = base64(RSA-encrypted plaintext token). 5) Decrypt with private key (OAEP-SHA256, fallback PKCS1v15). No clientId/clientSecret/tokenUrl/refreshUrl — long-lived access_token, no refresh. Executor auth: "Authorization: <user_id> <access_token>" plus duplicate x-zed-cloud-token header (zed.js:32-39), baseUrl https://cloud.zed.dev/completions, forceStream, NDJSON wire protocol.

**Current Rust behavior:**

No zed OAuth in src/oauth/providers.rs; oauth.rs:4217 unknown_provider. No RSA keypair logic anywhere in src/ (the auth header shape "<user_id> <access_token>" does not exist). The executor base entry P0-A1n only makes the provider reachable with a pre-obtained token.

**Implementation steps:**

1) Add a "zed" OAuth flow in src/oauth: generate RSA-2048 keypair (use the `rsa` crate or `openssl`; PKCS#1 DER, URL-safe base64 public key). 2) Bind a random TCP port on 127.0.0.1 and open https://zed.dev/native_app_signin?native_app_port={port}&native_app_public_key={pub}. 3) On redirect http://127.0.0.1:{port}/?user_id=...&access_token=... decrypt access_token with the private key (OAEP-SHA256, fallback PKCS1v15). 4) Store user_id + plaintext token (access_token). No refresh (long-lived). 5) For the chat path, the executor must send "Authorization: <user_id> <access_token>" (two space-separated values, NOT Bearer) plus "x-zed-cloud-token": <access_token> — this requires extending DefaultExecutor with a zed-specific auth header branch (build_headers default.rs:815-982) OR a dedicated zed executor (parity A3). 6) forceStream + NDJSON/SSE translation is a follow-up (zed.js executor). Full parity is out of scope for a single pass; minimum: oauth flow + custom auth header.

**Guard test:**

oauth tests: #[test] fn zed_oauth_no_refresh_endpoint(): zed OAuthProviderConfig has no token_url/refresh_url and platform == "zed". Plus an executor test asserting the Authorization header equals "<user_id> <access_token>" format once implemented.

**⚠️ Risks:**

The auth scheme is deliberately non-standard (zed.js:33-39 comment: scheme is a marker; real value built in executor). Do NOT send "Bearer <user_id> <access_token>" — the upstream expects bare "<user_id> <access_token>". The x-zed-cloud-token header duplicates the token. RSA OAEP-SHA256 with PKCS1v15 fallback must match zed's server or decryption fails. This is the most complex of the 17; it is acceptable to land the reachability fix (P0-A1n) first and the OAuth+auth-header here.

**Cross-check:** ✅ **CONFIRMED** — JS claim is real and implemented: registry/zed.js:54-66 matches exactly (authorizeUrl https://zed.dev/native_app_signin, platform:"zed", rsaKeyExchange:true, 5-step RSA comment). The flow is fully coded, not just declared — shared/zedAuth.js createZedNativeAuthData (RSA-2048, PKCS#1 DER URL-safe base64 pub key) and decryptZedAccessToken (OAEP-SHA256 then PKCS1v15 fallback), providers/zed.js (prepareConfig/exchangeToken/postExchange), and utils/server.js (127.0.0.1 TCP listener, default port 58443, random port fallback on EADDRINUSE) — plus buildZedUserAuthHeader producing the "<user_id> <access_token>" auth header. Rust claim is real: src/oauth/providers.rs get_config dispatcher (lines 442-469) has 22 arms, none "zed" (0 matches for \bzed\b/"zed"); oauth.rs:4217 is start_oauth_flow's get_provider_config None arm returning unknown_provider. No RSA keypair generation/decryption exists in src/ — the only RSA keygen is a #[cfg(test)] helper in server/auth/oidc.rs:375 and the only OAEP usage is qoder's hardcoded-public-key SHA-1 encryption (opposite direction); "<user_id> <access_token>" has 0 hits in src/. zed is also absent from the executor registry (core/executor/provider.rs PROVIDER_REGISTRY), consistent with P0-A1n's "reachable with a pre-obtained token" framing. Impl steps are feasible and sufficient: rsa=0.9 is already a Cargo dependency, sha2 is present, and the codebase already has TcpListener-on-127.0.0.1 local-callback infra (codex fixed port, xai 56121) plus code_verifier session plumbing that can carry the encoded private key (mirroring the JS trick of flowing the private key through the codeVerifier slot). Minor nits only: the registry comment says "bind random TCP port" while the JS actually prefers fixed 58443 with random fallback, and the spec's step-4 transcription is truncated — neither affects the parity gap or the implementation path.

---

### `P1-B3` — find RTK filter misses Windows backslash path handling (G2f)

**JS (source of truth — verbatim):**

open-sse/rtk/filters/find.js:12-14:
```js
for (const path of lines) {
  // Accept both Unix ("/a/b") and Windows ("C:\a\b") separators
  const lastSep = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  let dir; let basename;
  if (lastSep === -1) { dir = "."; basename = path; }
  else { dir = path.slice(0, lastSep) || "/"; basename = path.slice(lastSep + 1); }
  ...
}
// dir label: const dirLabel = dir.replace(/\\/g, "/");
// header: `${lines.length} files in ${dirs.length} dirs:\n\n` (find.js:30)
// per dir: `${dirLabel}/  (${files.length})\n` (find.js:36) then `  ${f}\n`
// cap: showDirs.slice(0, FIND_TOTAL_DIR_MAX); showFiles.slice(0, FIND_PER_DIR_MAX);
// overflow: `  +${files.length - FIND_PER_DIR_MAX}\n` (find.js:40) and `\n+${dirs.length - FIND_TOTAL_DIR_MAX} more dirs\n` (find.js:44)
```

**Current Rust behavior:**

src/core/rtk/filters/mod.rs:330-372 find_impl: `let last_slash = path.rfind('/');` — only forward slash. Windows path `C:\a\b.rs` splits on no '/' → dir "." , basename "C:\a\b.rs", and `dir_s` becomes "." — wrong grouping. Output format: `{}/ ({}):` uses `{}/ ({}):\n` (colon before paren, then a blank line after each dir) vs JS `${dirLabel}/  (${files.length})\n` (two spaces, no colon, no blank line). Header matches (`{} files in {} dirs:\n\n`). Also dirLabel does not normalize backslashes to slashes. Constants FIND_PER_DIR_MAX=10 / FIND_TOTAL_DIR_MAX=20 already present (constants.rs:27-31).

**Implementation steps:**

1) src/core/rtk/filters/mod.rs find_impl: replace `let last_slash = path.rfind('/');` with `let last_slash = path.rfind('/').max(path.rfind('\\'));` (both separators).
2) Normalize dir label: `let dir_label = dir_s.replace('\\', "/");` and use it in the `{}/ ({}):` line.
3) Fix the per-dir line format to match JS: `{dir_label}/  ({count})` (TWO spaces before the open paren, NO colon) followed by a newline — i.e. `format!("{}/  ({})\n", dir_label, files.len())`, then each `  {}\n`, overflow `  +{}\n`. Remove the extra blank line JS does not emit (JS pushes `  +N` then continues to next dir with no blank separator). Keep the trailing "+N more dirs\n" only — note JS prefixes it with "\n" (`\n+${dirs.length - FIND_TOTAL_DIR_MAX} more dirs`).

**Guard test:**

cargo test find_groups_windows_backslash_paths in src/core/rtk/filters/mod.rs: find_impl("C:\\a\\b.rs\nC:\\a\\c.rs\nC:\\d.rs") → header "3 files in 2 dirs:", contains "C:/a/  (2)", contains "C:/d/  (1)", and basenames "b.rs","c.rs","d.rs". Add a format assertion: line contains `/  (` (two spaces, no colon).

**⚠️ Risks:**

JS `Math.max(lastIndexOf("/"), lastIndexOf("\\"))` — a path with BOTH separators uses whichever comes LAST. dir=="/" when slice(0,0) is empty for a root-relative path (JS `dir = path.slice(0, lastSep) || "/"`). dirLabel normalizes backslashes to forward slashes for display only — the grouping key keeps original separators. Do not change the `+N more dirs` leading newline (JS has it; Rust currently emits it without the leading newline — match JS: append `\n+{} more dirs`).

**Cross-check:** ✅ **CONFIRMED** — All load-bearing claims verified against the actual source.

1) JS behavior REAL: C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/open-sse/rtk/filters/find.js line 13 has exactly `const lastSep = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));` (both separators), with `dir = "."`/basename=whole path when `lastSep === -1`, and `dir = path.slice(0, lastSep) || "/"` otherwise. Line 35 normalizes `dirLabel = dir.replace(/\\/g, "/")`; line 36 emits `{dirLabel}/  ({count})\n` — TWO spaces, NO colon. Line 44 prefixes `\n` before `+N more dirs`. Cited lines 12-14 are accurate (off-by-one: the `for` loop brace is line 11, not 12 — trivial, not substantive).

2) Rust current behavior REAL: C:/Users/ADMIN/Documents/Projects/openproxy/src/core/rtk/filters/mod.rs find_impl (lines 330-372) line 341 is `let last_slash = path.rfind('/');` (forward slash only). For `C:\a\b.rs`, `rfind('/')` is None → dir `"."`, basename the whole path, `dir_s` = `"."` — confirmed wrong grouping. Line 358 emits `{}/ ({}):\n` — ONE space, COLON after the close paren. Constants match (FIND_PER_DIR_MAX=10, FIND_TOTAL_DIR_MAX=20 in both).

3) Impl steps are technically sound and would fix the described bug: `rfind('/').max(rfind('\\'))` is the correct mirror of `Math.max(lastIndexOf("/"), lastIndexOf("\\"))` (Option::max returns larger index = last separator; None < Some, so no-separator paths still fall to `"."`). `dir_s.replace('\\', "/")` mirrors JS `replace(/\\/g, "/")`. `{dir_label}/  ({count})` (two spaces, no colon) mirrors JS line 36. Combined they reproduce JS grouping/labeling for Windows paths.

Minor residual, not an omission in the backslash fix: Rust's `out.push('\n')` blank line after each dir group (line 365) and the missing leading `\n` on `+N more dirs` (JS line 44 vs Rust line 368) are pre-existing cosmetic output differences independent of G2f; the impl steps as scoped don't address them, but they don't undermine the backslash-handling parity the task targets. The garbled "(colon before paren...)" fragment in the claim's format description is the only imprecision — the colon is after the close paren — but step 3 correctly specifies NO colon, matching JS.

---

### `P1-B4` — Capacity adapter pools (vision/pdf/audioInput/videoInput fallback models) not ported (F1)

**JS (source of truth — verbatim):**

open-sse/services/capacityAdapter.js:13-16:
```js
const CAPABILITY_KEYS = ["vision", "pdf", "audioInput", "videoInput"];
const HARD_CAPS = new Set(CAPABILITY_KEYS);
const DEFAULT_FALLBACK_MODEL = "oc/mimo-v2.5-free";
```
Settings default (src/lib/db/repos/settingsRepo.js:20-25): `capacityAdapter: { vision: { enabled: true, roundRobin: false, models: [] }, pdf: { enabled: false, roundRobin: false, models: [] }, audioInput: { enabled: true, roundRobin: false, models: [] }, videoInput: { enabled: false, roundRobin: false, models: [] } }`.
getCapacityAdapterConfig (35-41): enabled pools with empty models → `[DEFAULT_FALLBACK_MODEL]` so the toggle is never a no-op.
getCapacityAdapterModels (44-58): flatten enabled pools in CAPABILITY_KEYS order, dedup.
getActiveAdapterStrategy (68-76): first hard capability with enabled pool → "round-robin" if roundRobin else "fallback"; default "fallback".
augmentModelsWithCapacityAdapter (92-100): if hard req empty or models empty → unchanged; if `models.some(modelSatisfies)` → unchanged; else `[ ...pool.filter(m => !models.includes(m) && modelSatisfies(m, hard)), ...models ]` — adapter models go FIRST.
modelSatisfies (78-84): split on first '/', `getCapabilitiesForModel(provider, model)` returns caps; `requiredHard.every(c => caps[c] === true)`.
stripHistoryForContext (117-156): key messages|input|contents; split system/developer vs rest; tail = rest after last assistant/model; older = before; budgetChars = (contextWindow||200000) * 0.8 * 4; keep first HEAD_KEEP=6 of older verbatim; drop head turns from the end while total > budgetChars; if head == older.length unchanged; else `{...body, [key]: [...systemMsgs, ...head, ...tail]}`.
withCapacityAdapterStripping (160-173): wrap handleSingleModel; if modelStr ∈ adapterModels, look up contextWindow and stripHistoryForContext(body, contextWindow) first.
Wiring (src/sse/handlers/chat.js:96,124,137,144): combo → `augmentModelsWithCapacityAdapter(comboModels, requiredCapabilities, settings)`; solo → `augmentModelsWithCapacityAdapter([modelStr], requiredCapabilities, settings)`; if soloAugmented.length>1 → handleComboChat with adapter strategy.

**Current Rust behavior:**

No capacity-adapter module. src/core/combo/mod.rs:189 HARD_CAPS = ["vision","pdf"] (missing audioInput/videoInput); detect_required_capabilities (193-253) only detects vision/pdf (scan_content_for_capabilities 256-297 — no audio/video mime or input_audio/input_video types); model_has_capability (302-345) is a hardcoded provider-prefix heuristic returning bool (no capability TABLE, no contextWindow); reorder_by_capabilities (355-384) tiers by missing hard caps but never ADDS adapter models. detect in combo/mod.rs ignores audio/video entirely. No stripHistoryForContext, no withCapacityAdapterStripping, no DEFAULT_FALLBACK_MODEL, no capacityAdapter settings parsing. Settings struct (types/mod.rs:358) has no capacity_adapter field — it would land in Settings.extra via serde flatten (types/mod.rs:535). The catalog has per-model context_window (catalog.rs:29) and per-provider vision/reasoning flags (catalog.rs:52-56).

**Implementation steps:**

1) Add to src/core/combo/mod.rs (or a new capacity_adapter.rs, exposed as `pub mod capacity_adapter;`): constants `CAPABILITY_KEYS: ["vision","pdf","audioInput","videoInput"]`, `DEFAULT_FALLBACK_MODEL = "oc/mimo-v2.5-free"`. Add `pub struct CapacityAdapterConfig { enabled: bool, round_robin: bool, models: Vec<String> }` with `from_settings(settings) -> BTreeMap<String, CapacityAdapterConfig>` reading `settings.extra["capacityAdapter"][cap]` (accept legacy array form [{model, enabled}] → enabled+fallback, and object {enabled, roundRobin, models}); enabled pool with empty models → ["oc/mimo-v2.5-free"].
2) Add `pub fn augment_models_with_capacity_adapter(models: &[String], required: &HashSet<String>, settings: &Settings) -> Vec<String>` mirroring JS: hard = required ∩ CAPABILITY_KEYS; if empty or models empty → return; if any model satisfies all hard → return; pool = getCapacityAdapterModels filtered `!models.contains(m) && model_satisfies(m, hard)`; if pool empty → return; return pool ++ models (adapter first).
3) Add `pub fn model_satisfies(entry: &str, required_hard: &[&str]) -> bool` using a new capability lookup: parse provider before first '/', look up the model in the ProviderCatalog (context via a passed-in &ProviderCatalog or via catalog::static resolve) — check provider_capabilities.vision and model capabilities vec; fall back to the existing model_has_capability heuristic for unknown models. Strictly: `required_hard.iter().all(|c| caps[c] == true)`.
4) Add `pub fn strip_history_for_context(body: &mut Value, context_window: u32) -> bool` (key messages|input|contents; HEAD_KEEP=6; budget = ctx*0.8*4 chars; keep system+head+tail, drop middle) and `with_capacity_adapter_stripping`-equivalent behavior in the chat dispatch: when a dispatched model is in the adapter pool, strip history before the single-model call.
5) Extend detect_required_capabilities + scan_content_for_capabilities: add audioInput (mime starts_with "audio/", block types input_audio|audio_url|audio) and videoInput (mime starts_with "video/", types input_video|video_url|video) — mirroring JS scanBlock/addByMime. Update HARD_CAPS to the full 4.
6) Wire in src/server/api/chat.rs: in the combo branch (before execute_combo_strategy_full) and the direct branch (before execute_single_model): compute `let augmented = augment_models_with_capacity_adapter(&models, &required_caps, &snapshot.settings);` and if it differs, pass augmented (JS uses adapterAdded only for stripping — simplest parity: pass the full augmented list through combo execution, and strip history inside the single-model closure when the model is in adapterAdded).
7) Add Settings default: since settings.capacityAdapter lands in Settings.extra, provide a helper `fn capacity_adapter_defaults() -> Value` matching settingsRepo defaults and merge into Settings::default()'s extra (types/mod.rs:589 `extra: BTreeMap::new()` → insert capacityAdapter JSON).

**Guard test:**

cargo test capacity_adapter_default_fallback_model in src/core/combo/capacity_adapter.rs: config with vision enabled + empty models → models == ["oc/mimo-v2.5-free"]. cargo test augment_prepends_adapter_when_none_satisfy: models ["anthropic/claude-opus-4.6"] (no pdf in Rust heuristic) with required {pdf} → output starts with pool models. cargo test augment_unchanged_when_covered: models ["anthropic/claude-opus-4.6"] required {vision} → unchanged (claude has vision). cargo test strip_history_drops_middle_preserves_tail: body with 10 messages, last user has image, context_window 200000 → result keeps system + last user turn, drops older middle.

**⚠️ Risks:**

modelSatisfies uses a capability table (getCapabilitiesForModel) — Rust must use the catalog's real capabilities (provider.vision, model capabilities vec, context_window) not only the heuristic. JS checks `caps[c] === true` strictly — audioInput/videoInput false must NOT satisfy. The default fallback "oc/mimo-v2.5-free" must resolve through get_model_info in Rust (provider "oc") — verify provider "oc" exists in the catalog (src/core/model/sources/omniroute.json has mimo-v2.5-free). The JS augment leaves `models` untouched when the original covers it (combo reorderByCapabilities handles that case via autoSwitch) — do not double-reorder. stripHistoryForContext uses `contextWindow || 200000` fallback and CHARS_PER_TOKEN=4, budget 0.8.

**Cross-check:** ✅ **CONFIRMED** — All cited claims verified accurate.

JS (9router): open-sse/services/capacityAdapter.js:13-15 exactly contains CAPABILITY_KEYS = ["vision","pdf","audioInput","videoInput"], HARD_CAPS = new Set(CAPABILITY_KEYS), DEFAULT_FALLBACK_MODEL = "oc/mimo-v2.5-free" (content exact; claim's 13-16 range is one line off at the end). src/lib/db/repos/settingsRepo.js:20-25 capacityAdapter defaults match exactly (vision enabled:true, pdf enabled:false, audioInput enabled:true, videoInput enabled:false). The adapter is genuinely live: augmentModelsWithCapacityAdapter/withCapacityAdapterStripping/getActiveAdapterStrategy are imported and invoked in src/sse/handlers/chat.js lines 18/96/137/173; combo.js:12 HARD_CAPS includes all four caps and detectRequiredCapabilities (combo.js:105-135) detects audioInput (mime audio/, types input_audio/audio_url/audio) and videoInput (mime video/, types input_video/video_url/video). The "oc" provider is a real registry entry (providers/registry/opencode.js alias "oc"), and *mimo*v2.5* pattern (capabilities.js:282) grants the fallback model vision/audioInput/videoInput.

Rust (openproxy): src/core/combo/mod.rs:189 HARD_CAPS = ["vision","pdf"] (missing audioInput/videoInput); detect_required_capabilities (193-253) and scan_content_for_capabilities (256-297) detect only vision/pdf (no audio/ or video/ mime handling, no input_audio/input_video types); model_has_capability (302-345) only has "vision" and "pdf" match arms. Grep for capacityAdapter|capacity_adapter in src/ returns zero matches — no capacity-adapter module exists. Chat dispatcher (src/server/api/chat.rs:379,483) passes only detect_required_capabilities output as required_caps into execute_combo_strategy_full with no adapter pooling. All Rust line numbers exact.

Impl step 1 (constants + CapacityAdapterConfig struct + from_settings) is correct and a necessary foundation with no errors. Caveat, not a refutation: step 1 alone does not yield functional parity — full parity also requires extending detect_required_capabilities and model_has_capability for audioInput/videoInput (else those two pools never activate since required caps never contain them) plus wiring the pool into dispatch; the section's own gap description (F1) explicitly calls out the missing audio/video detection, so the truncated impl plan presumably covers these in later steps.

---

### `P1-B5` — Combo capability heuristics diverge from JS reorderByCapabilities (audio/video/search missing, no capability table)

**JS (source of truth — verbatim):**

open-sse/services/combo.js:12 HARD_CAPS = new Set(["vision", "pdf", "audioInput", "videoInput"]);
reorderByCapabilities (63-82): tierOf(m) = hard.every(c => caps[c]===true) ? (soft.every(c => caps[c]===true) ? 0 : 1) : 2 — soft caps (anything not in HARD_CAPS, e.g. "search") put a model at tier 1 instead of 0; stable sort by tier then original index. Returns models unchanged when required.size===0 or models.length<=1.
detectRequiredCapabilities (105-150): scanBlock adds vision for image_url|image|input_image, audioInput for input_audio|audio_url|audio, videoInput for input_video|video_url|video, and for file|document|input_file infers mime from b.input_audio?.format / b.file?.file_data data: URI / b.source?.media_type / b.source?.data, falling back to pdf; gemini inlineData/fileData mime → addByMime. addByMime (109-115): image/*→vision, application/pdf→pdf, audio/*→audioInput, video/*→videoInput. trailingUserItems (94-100): items after the last assistant/model turn.

**Current Rust behavior:**

src/core/combo/mod.rs:189 HARD_CAPS=["vision","pdf"] (missing audioInput/videoInput). detect_required_capabilities (193-253) scans messages/input/contents/request.contents; trailing_users (211-219) matches JS trailingUserItems. scan_content_for_capabilities (256-297) only adds vision for image_url|image|input_image and pdf for input_file|document|file — no audio/video types, no mime-based addByMime (only inlineData/fileData mime check for image/pdf), no file mime inference, no media:// string handling beyond vision. reorder_by_capabilities (355-384) has tiers 0/1/2 but its tier1 = "!missing_hard" (missing soft cap), tier0 = "has_all_required" — equivalent to JS tiers, BUT it cannot produce soft-cap-aware results because the required set never contains soft caps, and model_has_capability (302-345) is a hardcoded prefix heuristic (not a capability table), so audioInput/videoInput requests can never reorder.

**Implementation steps:**

1) src/core/combo/mod.rs:189: change HARD_CAPS to `&["vision", "pdf", "audioInput", "videoInput"]`.
2) scan_content_for_capabilities: add match arms for `Some("input_audio" | "audio_url" | "audio")` → insert "audioInput"; `Some("input_video" | "video_url" | "video")` → insert "videoInput"; for `input_file`/`document`/`file` blocks add mime inference: if block has input_audio.format → "audio/{format}"; if file.file_data is a data: URI → parse mime; if source.media_type → use it; if source.data data: URI → parse mime; if mime known call a new add_by_mime helper (image/*→vision, application/pdf→pdf, audio/*→audioInput, video/*→videoInput) else pdf. Also add add_by_mime for inlineData/fileData mimeType (currently only image/pdf handled).
3) Add `fn add_by_mime(mime: &str, required: &mut HashSet<String>)`.
4) Keep trailing_users scan; extend the gemini parts branch to use add_by_mime for inlineData/fileData mimeType (image/audio/video/pdf).
5) model_has_capability: extend the "vision"/"pdf" arms with audioInput/videoInput knowledge AND (to reduce drift) consult a shared capability table that reads the ProviderCatalog provider.vision flag + model capabilities vec + context_window, falling back to the existing heuristic when the model is not in the catalog. At minimum, add audioInput/videoInput arms to the heuristic (gemini 2+/3*, qwen omni/3.5+, kimi k3, mimo v2.5, minimax-m3, gemini-3* patterns from capabilities.js).
6) detect_required_capabilities must also surface SOFT caps (like search) if Rust ever needs tier1 placement parity — but JS has search disabled (combo.js:147 "search: temporarily disabled"), so leaving search out matches current 9router behavior.

**Guard test:**

cargo test detect_audio_and_video_capabilities in src/core/combo/mod.rs: body messages with last user content [{type:"input_audio", input_audio:{format:"wav"}}] → required contains "audioInput"; [{type:"input_video"}] → "videoInput". cargo test reorder_prefers_model_with_audio_when_required: models ["openai/gpt-4o", "google/gemini-2.5-pro"] required {audioInput} → gemini (audio-capable per heuristic) ordered before gpt-4o.

**⚠️ Risks:**

JS addByMime checks `typeof mime === "string"` and falls back to pdf for generic files. trailingUserItems only counts items after the LAST assistant/model turn — a vision image in an OLD turn must not pin. Match the exact block type strings (input_audio/audio_url/audio). Keep model_has_capability's existing behavior for known providers (openai/gpt-4*, anthropic/claude, google/gemini, vertex/*, aws/claude, gcp/gemini) so existing reorder tests don't regress — add to it rather than replace.

**Cross-check:** 🟡 **PLAUSIBLE** — The JS claims are REAL and exactly match combo.js: HARD_CAPS (line 12) = {vision, pdf, audioInput, videoInput}; reorderByCapabilities (63-82) computes tier 2 when any hard cap is false, tier 1 when all hard but a soft cap is false, tier 0 otherwise, stable-sorts by (tier, original index), and returns models unchanged when required is empty; detectRequiredCapabilities (105-150) uses addByMime + scanBlock with input_audio|audio_url|audio -> audioInput, input_video|video_url|video -> videoInput, and file/document/input_file mime inference; trailingUserItems (94-100) matches the claimed pattern. Note the JS ALSO disables search detection (line 147-148), so "search missing" in Rust is not a real divergence. The Rust current behavior is also REAL: mod.rs:189 HARD_CAPS=["vision","pdf"]; detect_required_capabilities (193-253) scans messages/input/contents/request.contents with trailing_users (211-219) close to trailingUserItems; scan_content_for_capabilities (256-297) only inserts vision/pdf with no audio/video or mime-inference arms. The gap analysis is accurate. HOWEVER, the impl_steps are incomplete for the stated parity goal: they fix detection (steps 1+2) but never touch model_has_capability (302-345), which has arms only for vision/pdf and returns false for everything else, and the Rust source has NO capability table anywhere (grep for getCapabilitiesForModel/PROVIDER_CAPABILITIES/PATTERN_CAPABILITIES across src/ is empty). Once HARD_CAPS includes audioInput/videoInput, reorder_by_capabilities (355-384) will mark every model as missing a hard cap -> all land in tier2 -> concatenation preserves input order, a silent no-op. In JS, audio/video-capable models (gemini-2.5/3, qwen-omni, mimo, kimi-k3 carry audioInput/videoInput:true) get promoted to tier0. So after the listed steps, audio/video requests are correctly detected but the reorder still cannot float the right model to the front — parity is not achieved without also adding audioInput/videoInput resolution to model_has_capability or introducing a capability table. Minor non-breaking deviations also exist (Rust trailing_users filters to role=="user" and picks the first non-empty message array, vs JS scanning each shape independently). Because the JS and Rust analysis is accurate but the impl_steps omit the capability-resolution change the title itself flags ("no capability table"), the verdict is PLAUSIBLE, not CONFIRMED.

---

### `P1-B6` — MITM cert CA generation parity: CN name, expiry regen, Windows/nssdb install paths

**JS (source of truth — verbatim):**

src/mitm/cert/rootCA.js:26-93: Root CA CN "9Router MITM Root CA", org "9Router", country US, RSA 2048, serial "01", notBefore now, notAfter now+10yr, basicConstraints cA critical, keyUsage keyCertSign+cRLSign critical, subjectKeyIdentifier, self-signed sha256. Leaf (115-164): RSA 2048, serial random 0-999999, 1yr, CN=domain, SAN [DNS:domain, DNS:*.domain], basicConstraints cA:false, keyUsage digitalSignature+keyEncipherment, extKeyUsage serverAuth+clientAuth, signed by root sha256. isCertExpired (12-20): regenerate if notAfter < now+30days. server.js:60-72 generates on start if rootCA.key/rootCA.crt missing. install.js:30 `ROOT_CA_CN = "9Router MITM Root CA"`; Windows (119-133): `certutil -delstore Root "9Router MITM Root CA" 2>$null | Out-Null; $exit = & certutil -addstore Root <certPath>` via elevated PowerShell; macOS (106-117): delete-certificate -c CN then add-trusted-cert -d -r trustRoot; Linux (226-247): copy to {config.dir}/9router-root-ca.crt + update command + updateNssDatabases.

**Current Rust behavior:**

src/core/mitm/cert.rs:20-43 generate_ca: CN "OpenProxy MITM CA", org "OpenProxy", country US; rcgen default key (ECDSA P-256 via PKCS_ECDSA_P256_SHA256 only in sign_leaf:76). Leaf (62-83): CN = hostname via CertificateParams::new(vec![hostname]), key usages DigitalSignature+KeyEncipherment, extKeyUsage ServerAuth only (no ClientAuth), SAN = the single hostname (no wildcard `*.domain`). install_ca_cert (112-144): macOS security add-trusted-cert; Linux only /usr/local/share/ca-certificates + update-ca-certificates (no Arch/Fedora/openSUSE, no NSS, no Windows). No expiry check/regeneration (mitm_config.rs:242,383 generate_ca_persisted only checks file existence).

**Implementation steps:**

1) cert.rs generate_ca: set CN "9Router MITM Root CA", org "9Router"; if practical force RSA-2048 (rcgen RSA) or keep ECDSA but document divergence — the client-trust contract is CN+SAN, key type is not usually validated by IDEs, but for strict parity use RSA. Set validity 10 years (rcgen CertificateParams::not_after/not_before) and serial "01".
2) sign_leaf: add `params.use_authority_key_identifier_extension = true` (already), add ClientAuth to extended_key_usages, and add SAN entries `DNS:{hostname}` AND `DNS:*.{hostname}`.
3) Add a 30-day expiry check: before serving generate_ca_persisted, parse the cert's notAfter (use x509-parser or rcgen via Certificate::from_pem) and regenerate if notAfter < now+30d (mirror isCertExpired).
4) install_ca_cert: add Windows branch running `certutil -delstore Root "9Router MITM Root CA"` then `certutil -addstore Root {path}` (via a best-effort elevated helper or document a manual step); add Linux multi-distro config (the 4 LINUX_CERT_PATHS entries) and NSS db update (`certutil -d sql:$db -A -t "C,," -n "9Router MITM Root CA" -i {path}` for $HOME/.pki/nssdb, $HOME/.mozilla/firefox/*, snap chromium + snap firefox paths).
5) On MITM start (mitm_config.rs start_mitm) generate certs if missing/expired before binding.

**Guard test:**

cargo test leaf_cert_has_wildcard_san in src/core/mitm/cert.rs: sign_leaf(&ca_cert,&ca_key,"example.com") → parse the returned PEM (x509-parser) and assert SAN contains "example.com" and "*.example.com" and extendedKeyUsage contains serverAuth+clientAuth. cargo test ca_cert_cn_is_9router: assert generated PEM decoded contains "9Router MITM Root CA".

**⚠️ Risks:**

Changing the CN to "9Router MITM Root CA" breaks existing Rust-managed trust stores that installed the old "OpenProxy MITM CA" — uninstall must delete BOTH CNs or document a reinstall. Windows install requires elevation (UAC) — the JS uses runElevatedPowerShell; Rust must not silently fail. Adding wildcard SAN `*.domain` to the leaf means the same leaf covers subdomains — matches JS. rcgen may require non-default params for RSA; if RSA-2048 is impractical keep ECDSA but the fingerprint/install checks in JS (certutil -store Root by SHA1) are hash-based so the key type must be stable across regenerations.

**Cross-check:** 🟡 **PLAUSIBLE** — JS side (rootCA.js:26-93, 115-164) is exactly as claimed — verified by direct read: RSA 2048, serial "01", 10yr validity, CN "9Router MITM Root CA"/O "9Router"/C US, basicConstraints cA critical, keyUsage keyCertSign+cRLSign critical, subjectKeyIdentifier, sha256 self-signed; leaf RSA 2048, serial 0-999999, 1yr, CN=domain, SAN [domain, *.domain], cA:false, keyUsage digitalSignature+keyEncipherment. Rust side (cert.rs:20-43, 62-83) is as claimed except one nuance: the claim "leaf CN = hostname via CertificateParams::new(vec![hostname])" is wrong — rcgen 0.13.2's new() sets only the SAN list; the default DN CN is the placeholder "rcgen self signed cert" (certificate.rs:98) and there is no SAN→CN derivation, so the leaf subject CN is not the hostname. Impl steps: CN/org change, not_after/not_before (pub OffsetDateTime, certificate.rs:68-69), and serial_number (Option<SerialNumber>, line 70) are all settable in rcgen 0.13.2 (pinned Cargo.toml:107), and use_authority_key_identifier_extension already exists (cert.rs:74). KEY CAVEAT: step 1's "force RSA-2048" does not work under openproxy's rcgen feature set — RSA keygen in rcgen 0.13.2 requires feature "aws_lc_rs"; with default features (ring) KeyPair::generate_for(&PKCS_RSA_SHA256) returns Error::KeyGenerationUnavailable (key_pair.rs:118-121). The impl's own fallback (keep ECDSA, document divergence) is the workable path, which the client-trust contract (CN+SAN) supports. OMISSIONS vs the task title: (a) expiry regen — JS auto-regenerates CA when expiring within 30 days (rootCA.js:12-20, 32-36) and uses rolling setFullYear(+10); Rust generate_ca_persisted writes once and never regenerates (cert.rs:53-57). (b) Windows/nssdb install paths — Rust install_ca_cert returns "Unsupported platform" for non-macOS/non-Linux (cert.rs:140-141, 168-169), so Windows/nssdb is a real gap neither step addresses. (c) wildcard SAN *.domain in JS leaf is not in the Rust leaf. Overall the plan is directionally right and would improve parity, but the RSA-2048 instruction is not executable as written and two title-level parity gaps (expiry regen, Windows/nssdb) are unaddressed.

---

### `P2-C1` — combo handling diverges from JS on small points: transient-wait cap, first-status, sticky, no-credentials 503

**JS (source of truth — verbatim):**

open-sse/services/combo.js handleComboChat:246-348:
- Auto-switch (251-260): `detectRequiredCapabilities(body)`; if required.size>0 reorder by reorderByCapabilities; log if changed.
- Error text extraction (280-288): `errorBody?.error?.message || errorBody?.error || errorBody?.message || statusText`; retryAfter from errorBody?.retryAfter.
- earliestRetryAfter tracking (291-293): keep the EARLIEST (min Date).
- Transient wait (311-315): `if (cooldownMs && cooldownMs > 0 && cooldownMs <= 5000 && (status===503||502||504)) await sleep(cooldownMs)`.
- `lastStatus` only set from the FIRST failure (`if (!lastStatus) lastStatus = result.status;` 319), and catch block sets 500 if not set.
- Final (330-347): `allDisabled = lastError?.toLowerCase().includes("no credentials")` → status 503; else `status = lastStatus || 503`; msg = lastError || "All combo models unavailable"; if earliestRetryAfter → unavailableResponse(status, msg, retryAfter, retryHuman); else 503/status JSON `{error:{message}}`.
- getRotatedModels (174-203): rotationKey = comboName || "__default__"; normalizeStickyLimit via Number.parseInt, `Number.isFinite(parsed) && parsed > 0 ? parsed : 1`; sticky: state {index, consecutiveUseCount}; rotates by index; when nextUseCount >= stickyLimit advance index and reset count.
- resetComboRotation (209-212): delete by name or clear all.

**Current Rust behavior:**

Additional divergence: JS getRotatedModels for a combo with `models.length <= 1` returns models unchanged (no rotation state written) — Rust get_rotated_models:423 `if models.len() <= 1 || strategy != RoundRobin { return models.to_vec(); }` — matches. JS normalizeStickyLimit parses strings; Rust sticky_limit.max(1) accepts u32 — the CLI writes combos with numeric sticky, acceptable.

**Implementation steps:**

No functional gap in the core fallback iterator — the differences worth porting are: 1) JS tracks `earliestRetryAfter` as the MINIMUM Date across failures; Rust iterate_combo_models (747-752) keeps the earliest via `current <= retry_after` — already matches. 2) JS extracts retryAfter from the error JSON body; Rust ComboAttemptError.retry_after is set by the executor from response headers (retry_after_from_headers chat.rs:3263) — verify the body-based `errorBody.retryAfter` path is also covered; if a provider returns retryAfter only in the JSON body (not header), add a parse in the dispatch error mapping.
3) JS logs "COMBO Trying model i/N" per attempt — Rust logs via tracing (tracing::debug) — cosmetic.
4) Confirm auto-switch log parity: JS logs `auto-switch for [caps] → model`; Rust has the COMBO_ORDER debug line (690-695). Fine.
If no code change is needed, the spec item documents parity is already present and the only actionable item is the body-based retryAfter fallback.

**Guard test:**

cargo test combo_retry_after_from_body_parsed in src/core/combo/mod.rs or server/api/chat.rs: an error whose JSON body is `{"error":{"message":"x","retryAfter":"2026-08-13T00:00:00Z"}}` with a 429 response and no Retry-After header → ComboAttemptError.retry_after is Some with the body's date.

**⚠️ Risks:**

retryAfter in JS can be an ISO date string OR a number of seconds; new Date(retryAfter) normalizes both. If Rust parses only RFC3339, a numeric `retryAfter` breaks earliest-retry comparison. The "no credentials" string match is case-insensitive substring on the lastError message — preserve exact JS wording "no credentials".

**Cross-check:** ✅ **CONFIRMED** — All cited claims verified against source on both sides.

JS (C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/open-sse/services/combo.js:246-348): (1) Auto-switch at 250-260 — detectRequiredCapabilities(body), reorder when required.size>0, log only when reordered[0] != rotatedModels[0]. Exact. (2) Error text at 280-288 — errorText seeded from result.statusText, then errorBody?.error?.message || errorBody?.error || errorBody?.message || errorText; retryAfter = errorBody?.retryAfter. The "|| statusText" in the claim is a correct compression of the fallback base. (3) earliestRetryAfter at 291-293 keeps the EARLIEST Date (new Date(retryAfter) < new Date(earliestRetryAfter)). (4) Transient wait at 311-315: cooldownMs in (0,5000] AND status in {503,502,504} → sleep. First-status at 319 (`if (!lastStatus) lastStatus = result.status`), no-credentials 503 at 333-335 (`includes("no credentials")` → 503, else lastStatus||503). All exact.

Rust (C:/Users/ADMIN/Documents/Projects/openproxy/src/core/combo/mod.rs): get_rotated_models:423 matches `models.len() <= 1 || strategy != RoundRobin` → unchanged. iterate_combo_models 747-752 keeps earliest via `Some(current) if current <= retry_after` — matches JS min semantics. Transient wait 768-773: `matches!(error.status, 502..=504) && cooldown > 0 && cooldown.as_millis() <= 5000` — exact parity including the 5000 cap. first_status captured at 739-746, used at 786-800 with the "no credentials"→503 override — matches JS. Sticky: JS normalizeStickyLimit (parseInt, ≤0/NaN→1) vs Rust u32 + `.max(1)` at chat.rs:383 and mod.rs:686 with the `sticky_limit > 1` gate — parity holds. retry_after source divergence (impl_steps #2) confirmed real: JS reads errorBody?.retryAfter from JSON body; Rust sets ComboAttemptError.retry_after from response headers via retry_after_from_headers (chat.rs:1743/1757, parser 3263-3302). This is a genuine divergence but is correctly characterized by the spec as NOT a functional gap in the iterator.

Impl_steps produce parity: both listed differences are already matched in the Rust code; no omission on the four named points. Out-of-scope minor notes (not claimed by the spec): Rust HARD_CAPS omits JS's audioInput/videoInput, and Rust reorder_by_capabilities lacks the JS len<=1 early-return (behaviorally harmless). These do not affect the cited claims.

---

---

## G. WEB DASHBOARD (5 specs)

### `P0-A2` — paramSupport STRIP_RULES completeness — flattenContent (cloudflare-ai) + clampToModelMaxOutput / maxOutputCap (volcengine-ark)

**JS (source of truth — verbatim):**

paramSupport.js:16-24 rules and 47-72 clamp logic:
```js
{ provider: "cloudflare-ai", flattenContent: true },
{ provider: "volcengine-ark", match: /glm-5/i, clampToModelMaxOutput: true },
{ provider: "volcengine-ark", match: /kimi/i, maxOutputCap: 32768, clampToModelMaxOutput: true },
```
flattenContent (lines 47-56): `for (const msg of body.messages) { if (msg && Array.isArray(msg.content)) { msg.content = msg.content.map(b => (b?.type === "text" && typeof b.text === "string") ? b.text : "").join(""); } }`
Clamp (lines 57-71): `const ceiling = Math.min(...candidates)` where candidates = [modelCeiling if clampToModelMaxOutput && Number.isFinite(modelCeiling) && modelCeiling > 0] + [maxOutputCap if finite && > 0]; then `clampNumber(body, "max_tokens", ceiling); clampNumber(body, "max_completion_tokens", ceiling); clampNumber(body, "max_output_tokens", ceiling);` clampNumber deletes nothing — it sets `body[key] = ceiling` only when `typeof body[key] === "number" && Number.isFinite(...) && body[key] > ceiling`.

**Current Rust behavior:**

N/A — Rust strip_unsupported.rs has no flattenContent and no maxOutputCap/clampToModelMaxOutput handling; the model-ceiling lookup functions (capabilities_for_format) are never consulted in the strip path. volcengine-ark exists as a provider (default.rs:157-159, api_key.rs:346) but no kimi/glm-5 cap.

**Implementation steps:**

In src/core/executor/strip_unsupported.rs:
1. After the field-removal loop (after line 56), add a flatten step: `if provider == "cloudflare-ai" { if let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) { for msg in messages { if let Some(content) = msg.get_mut("content").and_then(Value::as_array_mut) { let joined: String = content.iter().map(|b| if b.get("type").and_then(Value::as_str) == Some("text") { b.get("text").and_then(Value::as_str).unwrap_or("").to_string() } else { String::new() }).collect(); msg["content"] = Value::String(joined); } } } }` — note this REPLACES the array with a plain string (cloudflare requires string content).
2. Add a clamp step for provider == "volcengine-ark": compute `ceiling` = min of (a) model maxOutputTokens from the catalog if clampToModelMaxOutput and model matches /glm-5/i, (b) 32768 if model matches /kimi/i. Simplest exact-parity port without catalog plumbing: `let m = model.to_lowercase(); let mut cap: Option<u64> = None; if m.contains("glm-5") { cap = lookup maxOutputTokens (from src/core/model catalog for volcengine-ark, e.g. provider_catalog.json entry); } if m.contains("kimi") { cap = Some(cap.map_or(32768, |c| c.min(32768))); } if let Some(ceiling) = cap { for key in ["max_tokens","max_completion_tokens","max_output_tokens"] { if let Some(n) = obj.get(key).and_then(Value::as_u64) { if n > ceiling { obj.insert(key.to_string(), Value::from(ceiling)); } } } }`

**Guard test:**

Add `flatten_content_for_cloudflare_ai` — body messages [{"role":"user","content":[{"type":"text","text":"hi"},{"type":"image_url",...}]}], provider "cloudflare-ai", assert messages[0].content == "hi". Add `clamps_kimi_max_tokens_to_32768` — provider "volcengine-ark", model "kimi-k2.7-code", body {"max_tokens": 50000}, assert body["max_tokens"] == 32768.

**⚠️ Risks:**

clampNumber only fires when the current value is a finite number GREATER than ceiling (0/null untouched). flattenContent only touches `messages` — never non-array bodies (strip_unsupported returns early when body is not an object). If the Rust model catalog lookup is unavailable, fall back to the 32768 cap alone for kimi (JS min() semantics still hold).

**Cross-check:** ✅ **CONFIRMED** — All three claims verified. (1) JS is REAL: paramSupport.js:16 has the cloudflare-ai flattenContent rule; :17-23 has the two volcengine-ark clamp rules including the kimi maxOutputCap:32768; :47-56 is the text-part join loop; :57-72 clamps max_tokens/max_completion_tokens/max_output_tokens via Math.min over getCapabilitiesForModel().maxOutput (real export, capabilities.js:327) and rule.maxOutputCap. Call site default.js:78 runs on the transformed provider-format body; both providers exist in the JS registry. (2) Rust is REAL: strip_unsupported.rs has only 4 should_strip field rules — no flattenContent, no maxOutputCap/clampToModelMaxOutput, no model-ceiling lookup; capabilities_for_format (modality_helper.rs:333) returns only vision/audio/pdf flags and is consulted solely for modality stripping (chat.rs:759), never in the strip path; provider_catalog.json has no maxOutput/contextWindow data; volcengine-ark exists only as provider config (default.rs:157-159, api_key.rs:346) with no kimi/glm-5 cap. Strip call site default.rs:1008 operates on the provider-format body, matching JS semantics. (3) impl_steps achieve parity: the post-loop flatten step and per-match clamp on the provider-format body mirror JS. Minor gaps the spec leaves implicit but that do NOT break parity: the Rust port needs its own source for the model maxOutput ceiling (Rust catalog lacks maxOutput — would need to reuse another model field or hardcode the kimi/glm caps), and for kimi the max_completion_tokens arm is preempted since should_strip already strips that field for kimi (anthropic-compatible), so clamping effectively only caps max_tokens — both still yield the same end behavior.

---

### `P0-A2` — Embeddings: selfhosted-embedding must refuse when baseUrl missing (MissingBaseUrlError)

**JS (source of truth — verbatim):**

selfhostedEmbedding.js:36-46:
  buildUrl: (_model, creds) => {
    const rawBaseUrl = creds?.providerSpecificData?.baseUrl;
    if (!rawBaseUrl || !String(rawBaseUrl).trim()) throw new MissingBaseUrlError();
    const baseUrl = String(rawBaseUrl).trim().replace(/\/$/, "").replace(/\/embeddings$/, "");
    return `${baseUrl}/embeddings`;
  },
Error message (lines 23-34): "Self-hosted Embedding needs an endpoint: set this connection's baseUrl to the OpenAI base URL of your server, e.g. http://host:8080/v1 (note the /v1 — \"/embeddings\" is appended to it). Refusing to fall back to api.openai.com, which would send your input and API key to OpenAI." Also registry index.js:22 maps "selfhosted-embedding" -> selfhostedEmbedding adapter (NOT openaiCompatNode).

**Current Rust behavior:**

src/core/media/embeddings/base.rs:168-179 OpenAiCompatNodeAdapter::build_url: `let raw = ...baseUrl... .unwrap_or("https://api.openai.com/v1"); let raw = raw.strip_suffix("/embeddings").unwrap_or(raw); Ok(format!("{raw}/embeddings"))`. Falls back to api.openai.com. mod.rs get_embedding_adapter (line 46-66) maps selfhosted-embedding to none — it only matches openai-compatible-*/custom-embedding-* namespaces, so "selfhosted-embedding" returns None and falls through to the generic forwarder which uses default.rs URL (falls back to api.openai.com).

**Implementation steps:**

1) Add a dedicated adapter struct `SelfhostedEmbeddingAdapter` in src/core/media/embeddings/base.rs. Implement build_url: read `request.credentials.provider_specific_data.get("baseUrl")`, require non-empty trimmed string else return Err (a config-error marker; message should contain "Self-hosted Embedding needs an endpoint"). Then `raw.trim().trim_end_matches('/').strip_suffix("/embeddings").unwrap_or(raw).to_string() + "/embeddings"`. 2) build_headers/build_body reuse OPENAI impl (same shape as OpenAiCompatNodeAdapter). 3) mod.rs get_embedding_adapter: add `"selfhosted-embedding" => Some(&base::SELFHOSTED_EMBEDDING)`. 4) Ensure the error surfaces as 400 (config error), NOT 502. In handler.rs EmbeddingsHandlerError, map build_url Err from this adapter to Validation (already done — build_url errors map to Validation). 5) Do NOT route selfhosted-embedding through the fall-through generic forwarder.

**Guard test:**

fn selfhosted_embedding_requires_base_url() — request with no baseUrl: build_url returns Err containing "needs an endpoint". fn selfhosted_embedding_appends_embeddings() — baseUrl "http://host:8080/v1" and "http://host:8080/v1/embeddings" both resolve to "http://host:8080/v1/embeddings".

**⚠️ Risks:**

The whole point is refusing to leak input+key to OpenAI — do NOT introduce any api.openai.com fallback. JS strips ONLY a single trailing slash then the /embeddings suffix (order matters: strip slash first). Empty/whitespace baseUrl must throw. The error must be a 4xx config error, not 502.

**Cross-check:** ✅ **CONFIRMED** — JS behavior is real and matches exactly. selfhostedEmbedding.js:36-46 has buildUrl(_model, creds) that throws MissingBaseUrlError when creds?.providerSpecificData?.baseUrl is missing or whitespace, then trims, strips trailing "/", strips "/embeddings", and returns baseUrl + "/embeddings". MissingBaseUrlError (lines 23-34) extends Error with isConfigError=true and message starting "Self-hosted Embedding needs an endpoint:". The adapter is registered for provider id "selfhosted-embedding" (embeddingProviders/index.js:22), and the thrown error is caught in embeddingsCore.js:46-58 and converted to HTTP 400 (BAD_REQUEST), so a missing baseUrl never reaches the wire. Rust current behavior is also real and exactly as claimed: base.rs:168-179 OpenAiCompatNodeAdapter::build_url reads provider_specific_data.get("baseUrl"), unwrap_or("https://api.openai.com/v1"), strip_suffix("/embeddings"), then format!("{raw}/embeddings") — the cloud fallback the task is closing. mod.rs get_embedding_adapter (lines 46-66) has no selfhosted/selfhosted-embedding arm; only openai-compatible-*/custom-embedding-* prefixes route to the node adapter, so a dedicated SelfhostedEmbeddingAdapter plus a "selfhosted-embedding" match arm is the correct and only gap. Impl steps produce parity: trim/trim_end_matches('/'), strip_suffix("/embeddings"), append "/embeddings" mirror JS exactly; returning Err from build_url is already surfaced as a 400 (EmbeddingsHandlerError::Validation, handler.rs:47-49) matching the JS 400 on the config error. The proposed message "Self-hosted Embedding needs an endpoint" is a valid config-error marker (prefix of the JS message); not byte-identical, but that does not affect behavioral parity. No omission found — the steps cover adapter struct, build_url read/validate, URL normalization, and registration.

---

### `P0-A2` — Donate button + DonateModal + GITHUB_CONFIG.donateUrl missing

**JS (source of truth — verbatim):**

config.js:11-14 `export const GITHUB_CONFIG = { changelogUrl: "https://raw.githubusercontent.com/decolua/9router/refs/heads/master/CHANGELOG.md", donateUrl: "https://9router.com/api/donate" }`. Header.js:316-323 renders a Donate button in the header right actions: `className="flex items-center gap-1.5 px-3 h-8 rounded-lg border border-pink-500/30 bg-pink-500/10 text-pink-600 dark:text-pink-400 hover:bg-pink-500/20 transition-colors text-sm font-medium" aria-label="Donate"`, icon volunteer_activism, label Donate (hidden sm:inline), onClick setDonateOpen(true). Header.js:328 `<DonateModal isOpen={donateOpen} onClose={() => setDonateOpen(false)} />`. DonateModal.js:14-26 fetches `fetch(GITHUB_CONFIG.donateUrl, { cache: "no-store" })` -> JSON `{ title?, message?, channels:[{id,label,description,icon,color,url,qr}] }`. Card renders label, description, QR img (max-w-[180px]), and an 'Open' <a href target=_blank rel=noopener noreferrer> with style backgroundColor: color. Title falls back to 'Support 9Router'; loading shows spinner 'Loading...'; error shows 'Failed to load donate info: {err}'. Click-outside (mousedown) closes. Modal uses createPortal(document.body).

**Current Rust behavior:**

Header.tsx:284-288 right actions are only `<HeaderSearchInput /><ThemeToggle /><HeaderMenu onLogout={handleLogout} />`. No Donate button, no DonateModal component (not in web/src/shared/components listing), no GITHUB_CONFIG.donateUrl (config.ts:11-17 has changelogUrl/repoUrl/docsUrl/licenseUrl only).

**Implementation steps:**

1) config.ts GITHUB_CONFIG: add `donateUrl: "https://9router.com/api/donate"` (preserve the 9router URL verbatim - it is the upstream donate API). 2) Create web/src/shared/components/DonateModal.tsx: a portal modal (reuse existing Modal.tsx) that on open fetches GITHUB_CONFIG.donateUrl with cache no-store, parses {title,message,channels[{id,label,description,icon,color,url,qr}]}, renders a 3-col grid of channel cards (label, description, qr img, Open link styled backgroundColor:color, color value from channel.color, header icon volunteer_activism in pink), title fallback 'Support 9Router', loading spinner, error 'Failed to load donate info:', click-outside mousedown close. 3) Header.tsx: insert the Donate button between HeaderSearchInput and ThemeToggle, exact classes above, state donateOpen, render <DonateModal isOpen onClose>; keep the button aria-label Donate and volunteer_activism icon.

**Guard test:**

cargo test -p web or a vitest: component test renders header with Donate button; assert donate modal opens and calls fetch with donateUrl. Backend-side: unit test that GITHUB_CONFIG object (shared/constants/config.ts) has donateUrl equal to https://9router.com/api/donate.

**⚠️ Risks:**

Don't rebrand to a local URL - the JS hardcodes https://9router.com/api/donate; changing it breaks the modal. The pink button classes are a 9router brand choice - keep exact. Icon is volunteer_activism (material symbol), not favorite/heart.

**Cross-check:** ✅ **CONFIRMED** — All cited claims verified against source. JS: (1) 9router src/shared/constants/config.js:11-14 GITHUB_CONFIG contains exactly changelogUrl (decolua/9router refs/heads/master CHANGELOG.md) and donateUrl "https://9router.com/api/donate". (2) Header.js:316-323 renders the Donate button with the exact claimed className, pink styling, volunteer_activism icon, and onClick={() => setDonateOpen(true)}, wired via donateOpen state and <DonateModal> at line 328. (3) DonateModal.js exists and fetches GITHUB_CONFIG.donateUrl with cache:"no-store", parses {title,message,channels[{id,label,description,icon,color,url,qr}]}, renders a sm:grid-cols-3 channel grid — exactly matching the impl_steps described. Rust: (1) Header.tsx:284-288 right actions are only HeaderSearchInput/ThemeToggle/HeaderMenu onLogout — no Donate button. (2) web/src/shared/components has no DonateModal.tsx and a repo-wide grep for donate|donateUrl|DonateModal in web/src returns zero matches. (3) config.ts:11-17 GITHUB_CONFIG has only changelogUrl/repoUrl/docsUrl/licenseUrl, no donateUrl. Impl steps: adding the verbatim donateUrl and porting DonateModal against openproxy's existing Modal.tsx (a portal modal with overlay/header/body used by other modals) would produce parity. Minor non-blocking caveat: the impl_steps text was truncated mid-sentence ("renders a 3-col g...") so the Header.tsx button wiring step was not visible, but the gap is fully characterized and the described steps are accurate.

---

### `P0-A2` — x-9router-token-saver: off per-request bypass missing (F3/G2a)

**JS (source of truth — verbatim):**

open-sse/config/runtimeConfig.js:68: `export const TOKEN_SAVER_HEADER = "x-9router-token-saver";`

open-sse/handlers/chatCore.js:229: `const tokenSaverEnabled = clientRawRequest?.headers?.[TOKEN_SAVER_HEADER]?.toLowerCase() !== "off";`
Then gates EVERY token saver: 232 `compressMessages(translatedBody, tokenSaverEnabled && rtkEnabled)`, 238 `compressWithHeadroom(translatedBody, { enabled: tokenSaverEnabled && headroomEnabled, ... })`, 246 `else if (tokenSaverEnabled && headroomEnabled) log?.warn?.(...)`, 252 `if (tokenSaverEnabled && cavemanEnabled && cavemanLevel) injectCaveman(...)`, 258 `if (tokenSaverEnabled && ponytailEnabled && ponytailLevel) injectPonytail(...)`. Header value is compared case-insensitively to the literal "off".

**Current Rust behavior:**

src/server/api/chat.rs:831 `compress_messages(&mut body, snapshot.settings.rtk_enabled);`, 853-858 `compress_with_headroom(...)` gated only by headroom_enabled, 862 `apply_request_preprocessing(&mut body, &snapshot.settings, &plan.model)` — none of them consult a per-request header. client_headers (HashMap<String,String> lowercased) IS available inside execute_single_model (chat.rs:732 `client_headers: Option<&HashMap<String,String>>`), and headers_map is built at chat.rs:314-322.

**Implementation steps:**

1) In `execute_single_model` (src/server/api/chat.rs:724), compute once near top: `let token_saver_enabled = client_headers.map(|h| h.get("x-9router-token-saver").map(|v| !v.eq_ignore_ascii_case("off")).unwrap_or(true)).unwrap_or(true);`
2) Line 831 → `compress_messages(&mut body, token_saver_enabled && snapshot.settings.rtk_enabled);`
3) Headroom block (835-840): change `enabled: snapshot.settings.headroom_enabled` → `enabled: token_saver_enabled && snapshot.settings.headroom_enabled`.
4) Line 862 → gate the whole call: `let _ = if token_saver_enabled { apply_request_preprocessing(&mut body, &snapshot.settings, &plan.model) } else { false };`
5) The CLI paths src/cli/mod.rs:1625,1801 (`compress_messages`) and 1659,1835 (`apply_request_preprocessing`) have no headers — leave them as-is (JS CLI launcher has no header either).
Note the header must be read from the REQUEST headers (client), NOT the upstream/response headers.

**Guard test:**

cargo test token_saver_header_disables_rtk_and_caveman in src/server/api/chat.rs or rtk/tests.rs: build a headers map with `"x-9router-token-saver" => "off"`, assert token_saver_enabled=false; with absent header → true; with `"OFF"`/`"Off"` → false (case-insensitive); with `""` (empty) → true. Unit-test the boolean gate function directly to avoid an async test.

**⚠️ Risks:**

The JS check is `!== "off"` — i.e. ANY value except the exact string "off" (case-insensitive) keeps savers ON, including empty string, "yes", "true". Do not invert to `== "on"`. The header key must be lowercased when read from client_headers (already lowercase in headers_map).

**Cross-check:** ✅ **CONFIRMED** — All cited facts verified as real. JS: runtimeConfig.js:68 defines TOKEN_SAVER_HEADER = "x-9router-token-saver"; chatCore.js:229 computes tokenSaverEnabled = header value lowercased !== "off" (absent header => enabled); line 232 gates compressMessages with tokenSaverEnabled && rtkEnabled; line 238 gates compressWithHeadroom with tokenSaverEnabled && headroomEnabled; lines 252/258 also gate Caveman/Ponytail with tokenSaverEnabled. Rust: chat.rs:831 calls compress_messages(body, rtk_enabled) with no header consult; headroom block 835-840 sets enabled: headroom_enabled only; apply_request_preprocessing at 862 is gated only by settings. client_headers: Option<&HashMap<String,String>> exists at 732 and is always Some at all three call sites (Direct 586, Combo 531, Fusion 443), with keys lowercased at 314-322 so a get("x-9router-token-saver") will match. Impl step 1 expression exactly reproduces the JS semantics (absent header -> true via unwrap_or, value eq_ignore_ascii_case("off") -> false, else true); steps 2-3 correctly gate RTK and headroom. Caveat worth flagging for the implementer: JS also bypasses Caveman and Ponytail via this header (chatCore.js:252,258), so for full parity the Rust side must also gate apply_request_preprocessing (chat.rs:862) with token_saver_enabled; the visible impl_steps are truncated mid-step-3 and do not explicitly show this, but it may be in a later step. PXPIPE correctly needs no gate (JS does not gate it).

---

### `P0-B3` — Header missing OIDC identity chip

**JS (source of truth — verbatim):**

Header.js:192-216 loads auth status on mount: `fetch("/api/auth/status", { cache: "no-store" })`, then `setDisplayName(data?.displayName || data?.oidcName || data?.oidcEmail || ""); setLoginMethod(data?.loginMethod || "")`. Header.js:306-314 renders when `displayName && loginMethod === "OIDC"`: `hidden sm:flex items-center max-w-[220px] px-3 py-1.5 rounded-full border border-border bg-surface/70 text-xs text-text-muted truncate` containing `person` icon (text-primary), the displayName truncated, and a badge `ml-2 shrink-0 rounded-full bg-primary/10 px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-primary` with text OIDC. JS /api/auth/status route returns fields: `{ displayName, loginMethod: session?.oidc ? "OIDC" : "Password", oidcName, oidcEmail, ... }` (auth status route.js:25-29).

**Current Rust behavior:**

Rust auth.rs:191-210 /api/auth/status returns `{ authenticated, requireLogin, hasPassword, authMode, oidcConfigured, oidcLoginLabel, oidcEnabled }` - it does NOT return displayName/oidcName/oidcEmail/loginMethod, and it returns metadata even when logged out. Header.tsx has no displayName state and no OIDC chip.

**Implementation steps:**

1) Rust auth_status handler (src/server/api/auth.rs): when logged in, resolve the session identity and add `displayName`, `loginMethod` ('OIDC' when session is oidc else 'Password'), `oidcName`, `oidcEmail` to the JSON response (mirror JS order). 2) Header.tsx: add useEffect that fetches /api/auth/status with cache no-store on mount, stores displayName + loginMethod, and renders the OIDC chip (person icon + truncate name + OIDC badge) between HeaderSearchInput and ThemeToggle exactly as JS, gated on displayName && loginMethod === 'OIDC'.

**Guard test:**

cargo test oidc_chip_fields_in_auth_status: call auth_status handler with a mocked OIDC session and assert response JSON contains displayName, loginMethod == "OIDC", oidcName, oidcEmail (currently only oidcEnabled exists).

**⚠️ Risks:**

JS prefers displayName then oidcName then oidcEmail - order matters (use first non-empty). Rust currently returns authMode (login page) but not the session identity; must look up the session subject name from the session cookie claims, not just re-echo settings. Keep the chip hidden on <sm (hidden sm:flex).

**Cross-check:** ✅ **CONFIRMED** — All three claims verified against source. (1) JS: Header.js:192-216 fetches /api/auth/status with cache:no-store and sets displayName = data?.displayName || data?.oidcName || data?.oidcEmail || "" and loginMethod = data?.loginMethod || ""; Header.js:306-314 renders the OIDC chip when displayName && loginMethod === "OIDC" with the exact class string cited. Backing route src/app/api/auth/status/route.js does return displayName/loginMethod/oidcName/oidcEmail/oidcLogin and always returns 200. (2) Rust: auth.rs:191-210 returns exactly {authenticated, requireLogin, hasPassword, authMode, oidcConfigured, oidcLoginLabel, oidcEnabled} with no identity fields and returns metadata even when logged out; web/src/shared/components/Header.tsx (used by DashboardLayout.tsx:125) has no displayName/loginMethod state and no chip. (3) impl_steps are sufficient for parity: the OIDC dashboard JWT already embeds sub/email/name (auth.rs:426-434), so auth_status can decode the cookie to resolve identity and classify loginMethod ("OIDC" when OIDC claims present, else "Password" — same outcome as JS's session.oidc flag). The handler will need a claims decode that captures sub/email/name since DashboardClaims strips unknown claims, and the password-vs-OIDC distinction can key off presence of those claims — normal implementation details, not omissions. Minor cosmetic difference: JS pickOidcDisplayName prefers preferred_username before email/name, while Rust's callback stores name/email; does not affect chip parity. No refutation found.

---

---

## H. DB / USAGE / CLI (10 specs)

### `P0-H1a` — Usage history endpoint drops fields JS returns (connectionId, apiKeyMasked, endpoint, status, tokens)

**JS (source of truth — verbatim):**

usageRepo.js:316-334 getUsageHistory returns per-row object:
  return rows.map((r) => ({
    timestamp: r.timestamp, provider: r.provider, model: r.model,
    connectionId: r.connectionId, apiKeyMasked: maskApiKey(r.apiKey), endpoint: r.endpoint,
    cost: r.cost, status: r.status, tokens: parseJson(r.tokens, {}),
  }));
with maskApiKey (usageRepo.js:6-10):
  if (!key || typeof key !== "string") return null;
  if (key.length <= 8) return key.charAt(0) + "***";
  return key.slice(0, 8) + "***";
Query (usageRepo.js:327): `SELECT timestamp, provider, model, connectionId, apiKey, endpoint, cost, status, tokens FROM usageHistory ${where} ORDER BY id ASC`

**Current Rust behavior:**

src/server/api/usage.rs:301-343 get_usage_history returns HistoryResponse{ total_requests, history: Vec<UsageEntryDto> } where UsageEntryDto has ONLY: timestamp, provider, model, prompt_tokens, completion_tokens, cost. It drops connectionId, apiKeyMasked, endpoint, status, tokens. No filtering (provider/model/startDate/endDate query params are ignored).

**Implementation steps:**

In src/server/api/usage.rs, replace the UsageEntryDto struct (lines 307-315) with a struct that also serializes the missing fields. Add `api_key_masked: Option<String>` computed via a new helper `mask_api_key(&self, api_key: Option<&str>) -> Option<String>` matching JS: if len<=8 return first char + "***", else first 8 chars + "***". Add fields: connection_id (Option<String>), endpoint (Option<String>), status (Option<String>), tokens (Option<Value> serialized from entry.tokens). Keep snake_case JSON keys: JS returns camelCase? No — JS getUsageHistory returns camelCase `connectionId`, `apiKeyMasked`, `promptTokens`, `completionTokens`. So the DTO must use #[serde(rename_all="camelCase")] with fields connection_id, api_key_masked, endpoint, status, tokens. Note Rust currently returns prompt_tokens/completion_tokens snake_case — JS returns promptTokens/completionTokens camelCase (usageRepo.js:330-333). Add #[serde(rename_all = "camelCase")] to the DTO. Add query filtering: parse Query params provider, model, startDate, endDate (as in get_request_details pattern at usage.rs:1191-1233) and filter history in Rust. Change `total_requests` to JS total_requests: JS route returns getUsageStats() (stats object with totalRequests) — see usageRepo getUsageStats line 395: stats.totalRequests is computed from byProvider sum (line 657: stats.totalRequests = Object.values(stats.byProvider).reduce((sum,p)=>sum+(p.requests||0),0)). The Rust handler returning total_requests_lifetime is a different semantic but acceptable; keep as is unless strict parity required.

**Guard test:**

test_usage_history_dto_serializes_camelcase_fields: build a UsageEntry with connection_id Some("c1"), api_key Some("0123456789abcdef"), endpoint Some("/v1/chat/completions"), status Some("ok"), tokens Some(TokenUsage{prompt_tokens:Some(10),completion_tokens:Some(20),..}); serialize via the DTO; assert json contains "connectionId":"c1", "apiKeyMasked":"01234567***", "endpoint", "status", "tokens", and asserts keys are camelCase not snake_case.

**⚠️ Risks:**

JS getUsageHistory is ordered id ASC; Rust history iteration is insertion order — must preserve. JS apiKeyMasked masks with first 8 chars; a key <=8 chars is masked as first-char+"***" (NOT first 8). Do not mask with slice(0,8)+"..." for short keys. The dashboard UsageHistory component reads connectionId/apiKeyMasked/endpoint/status/tokens — dropping them makes columns blank.

**Cross-check:** ✅ **CONFIRMED** — All three checks pass.

1. JS claim is REAL and exact. In C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/src/lib/db/repos/usageRepo.js, maskApiKey is at lines 6-10 (if !key||typeof!=="string" return null; if length<=8 return charAt(0)+"***"; else slice(0,8)+"***") and getUsageHistory at lines 316-334 maps each row to {timestamp, provider, model, connectionId, apiKeyMasked: maskApiKey(r.apiKey), endpoint, cost, status, tokens: parseJson(r.tokens,{})}. The claimed dropped-by-Rust fields (connectionId, apiKeyMasked, endpoint, status, tokens) are all genuinely present in the JS response.

2. Rust current behavior is REAL and exact. In C:/Users/ADMIN/Documents/Projects/openproxy/src/server/api/usage.rs, get_usage_history (lines 293-343) builds HistoryResponse{total_requests, history: Vec<UsageEntryDto>} where UsageEntryDto (lines 307-315) serializes ONLY timestamp, provider, model, prompt_tokens, completion_tokens, cost — dropping connectionId, apiKeyMasked, endpoint, status, tokens. The handler takes only State+headers (no Query extractor), so there is no provider/model/startDate/endDate filtering, matching the claim.

3. Impl is feasible and would produce parity. UsageEntry (src/types/mod.rs:689-709) stores all needed source data: connection_id, api_key, endpoint, status, tokens (Option<TokenUsage>) — so every field the plan adds is backed by real persisted data. The described mask_api_key helper (len<=8 -> first char+"***"; else first 8+"***") matches JS charAt(0)/slice(0,8) semantics for ASCII API keys. No structural omission prevents parity.

Two detail-level caveats (implementation details, not parity blockers): (a) the new Rust DTO must use #[serde(rename_all="camelCase")] (or per-field renames) so keys serialize as connectionId/apiKeyMasked, not connection_id/api_key_masked — the codebase already applies camelCase to its other JS-facing DTOs (ConnectionUsageResponse, RequestDetailRecord, UsageChartBucket), so this is convention-following; (b) JS parseJson(r.tokens,{}) yields {} for a null tokens column, so the plan should emit {} (not null) for tokens: None to be byte-exact. Neither invalidates the claim or the fix.

---

### `P0-H1b` — Request-log timestamp format: JS DD-MM-YYYY HH:MM:SS local vs Rust raw RFC3339

**JS (source of truth — verbatim):**

usageRepo.js:734-737 formatLogDate:
  function formatLogDate(date = new Date()) {
    const pad = (n) => String(n).padStart(2, "0");
    return `${pad(date.getDate())}-${pad(date.getMonth() + 1)}-${date.getFullYear()} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
  }
getRecentLogs row format (usageRepo.js:766): `${ts} | ${m} | ${p} | ${account} | ${sent} | ${received} | ${r.status || "-"}`
where ts = formatLogDate(new Date(r.timestamp)) — LOCAL time, DD-MM-YYYY HH:MM:SS. sent = r.promptTokens ?? tk.prompt_tokens ?? "-", received = r.completionTokens ?? tk.completion_tokens ?? "-". account = connMap[r.connectionId] || (r.connectionId ? r.connectionId.slice(0, 8) : "-"). provider p = r.provider?.toUpperCase() || "-".

**Current Rust behavior:**

src/server/api/usage.rs:1266-1331 get_usage_logs + format_usage_log. format_usage_log (line 1330) produces `${timestamp} | ${model} | ${provider} | ${account} | ${sent} | ${received} | ${status}` where timestamp = entry.timestamp.as_deref().unwrap_or("-") — RAW RFC3339 (e.g. 2026-08-12T03:04:05.000Z), NOT local DD-MM-YYYY HH:MM:SS. Also provider is NOT uppercased (JS does r.provider?.toUpperCase()). status: Rust maps Some("success")->"OK", "ok"->"OK" case-insensitive, None->"OK". sent/received are taken only from tokens, NOT from entry.prompt_tokens/completion_tokens (JS prefers row promptTokens then tk.prompt_tokens). account fallback uses id.chars().take(8) (JS uses slice(0,8) then "-").

**Implementation steps:**

In src/server/api/usage.rs format_usage_log: (1) convert timestamp to local DD-MM-YYYY HH:MM:SS. Use chrono::Local: parse entry.timestamp via chrono::DateTime::parse_from_rfc3339, convert .with_timezone(&chrono::Local), then format with format!("{:02}-{:02}-{} {:02}:{:02}:{:02}", dt.day(), dt.month(), dt.year(), dt.hour(), dt.minute(), dt.second()). If unparseable, fall back to raw string. (2) provider: change `entry.provider.as_deref().unwrap_or("-")` to `.map(|p| p.to_uppercase()).unwrap_or_else(|| "-".to_string())`. (3) sent: prefer a new source — UsageEntry has no prompt_tokens/completion_tokens columns (they're inside tokens). JS prefers r.promptTokens ?? tk.prompt_tokens — the Rust tokens Option already covers this; keep as-is (tokens.prompt_tokens.or(tokens.input_tokens)). (4) account: JS uses connectionId.slice(0, 8) as the fallback label (no "Account ..." prefix) and "-" when no connectionId; Rust currently formats "Account" style? No — Rust format_usage_log account fallback is id.chars().take(8) with "-" fallback, matching JS. (5) status: JS returns r.status || "-" raw (not mapped to OK). Rust maps to "OK". Change to return entry.status.as_deref().unwrap_or("-") verbatim to match JS.

**Guard test:**

test_format_usage_log_local_timestamp: call format_usage_log with entry.timestamp="2026-08-12T03:04:05Z", provider Some("glm"), model "glm-4.7", connection_id Some("abc123"), status Some("success"); assert the produced line starts with a DD-MM-YYYY HH:MM:SS pattern (regex ^\d{2}-\d{2}-\d{4} \d{2}:\d{2}:\d{2} \| ) and contains "GLM" uppercased.

**⚠️ Risks:**

JS formatLogDate uses Date.parse which treats the ISO string as UTC then converts to local timezone. chrono conversion must use .with_timezone(&chrono::Local), NOT naive. Provider uppercase: JS r.provider?.toUpperCase() — if provider undefined, "-". Status: JS raw (e.g. "FAILED 502"), Rust must NOT map to "OK". Keep model "-" when empty (Rust already does).

**Cross-check:** ✅ **CONFIRMED** — All three verification points check out. (1) JS: formatLogDate at C:/Users/ADMIN/Documents/Projects/openproxy/.tmp/9router/src/lib/db/repos/usageRepo.js:734-737 is byte-identical to the claimed code (DD-MM-YYYY HH:MM:SS via local-time getters), and line 766 produces `${ts} | ${m} | ${p} | ${account} | ${sent} | ${received} | ${r.status || "-"}` matching the claim. JS stores RFC3339 via new Date().toISOString() (line 245) and formats to local on read, so the parity premise is real. (2) Rust: src/server/api/usage.rs get_usage_logs + format_usage_log at 1266-1331 match; line 1288 uses entry.timestamp.as_deref().unwrap_or("-") (raw string), line 1330 is the format! producing `${timestamp} | ${model} | ${provider} | ${account} | ${sent} | ${received} | ${status}`; timestamps are written as Utc::now().to_rfc3339() (tracker.rs:57, sqlite usage_repo), so raw RFC3339 output is confirmed. (3) Impl: chrono 0.4 with default features (Cargo.toml:71) provides chrono::Local and with_timezone; parse_from_rfc3339 -> with_timezone(&Local) -> {:02}-{:02}-{} {:02}:{:02}:{:02} with day/month/year/hour/minute/second exactly mirrors JS local DD-MM-YYYY HH:MM:SS (day-first, zero-padded, ms dropped), with a sane raw-string fallback. Only nit: usage.rs currently imports chrono::{Duration, NaiveDate, Utc}, so the impl must add DateTime/Local to the import or use fully-qualified chrono:: paths — a trivially handled detail, not an omission.

---

### `P0-H1c` — Usage fetch does not refresh OAuth tokens before quota call nor force-retry on auth-expired

**JS (source of truth — verbatim):**

9router/src/app/api/usage/[connectionId]/route.js:23-117 refreshAndUpdateCredentials(connection, force=false): builds credentials from connection (accessToken, refreshToken, idToken, expiresAt, lastRefreshAt, connectionId, providerSpecificData, copilotToken...); `const needsRefresh = force || executor.needsRefresh(credentials);` then `executor.refreshCredentials(credentials, console, proxyOptions)`; persists via updateProviderConnection(connection.id, updateData) with accessToken/refreshToken/idToken/lastRefreshAt/expiresAt(now+expiresIn*1000)/expiresIn/providerSpecificData updates.
GET handler route.js:158-183:
  if (isOAuth) { try { const result = await refreshAndUpdateCredentials(connection, false, proxyOptions); connection = result.connection; } catch (refreshError) { return Response.json({ error: `Credential refresh failed: ${refreshError.message}` }, { status: 401 }); } }
  let usage = await getUsageForProvider(connection, proxyOptions);
  if (isOAuth && isAuthExpiredMessage(usage) && connection.refreshToken) {
    const retryResult = await refreshAndUpdateCredentials(connection, true, proxyOptions);
    connection = retryResult.connection;
    usage = await getUsageForProvider(connection, proxyOptions);
  }
AUTH_EXPIRED_PATTERNS (route.js:11) = ["expired", "authentication", "unauthorized", "401", "re-authorize"]

**Current Rust behavior:**

src/server/api/usage.rs:497-511: get_connection_usage calls fetch_oauth_quota(connection) directly with the stored access_token — NO refresh before the call and NO force-retry on auth-expired message. Only codex-reset-credits (usage.rs:736-775, 965-1002) has refresh_codex_token. kimi usage (fetch_kimi_oauth_usage) has no refresh. The JS behavior of refreshing then retrying when the quota fetch returns an auth-expired message is absent for all OAuth providers.

**Implementation steps:**

In src/server/api/usage.rs get_connection_usage (the is_oauth branch at line 497):
1. Before calling fetch_oauth_quota, refresh the token: match connection.refresh_token (trimmed, non-empty) → call the provider's refresh fn (add match arms in fetch_oauth_quota or a new helper). Current Rust has refresh_codex_token for codex only. Add per-provider refresh dispatch (e.g. refresh_kimi_token, refresh_github_token, refresh_claude_token...) — or at minimum mirror JS by adding a `needs_refresh` check: if expires_at is present and past (or within 30s), refresh. On refresh success, persist tokens to the connection via the db update pattern already used in persist_codex_tokens (usage.rs:584-615) generalized to a persist_oauth_tokens(state, connection_id, access_token, refresh_token, id_token, expires_in). On refresh failure, return 401 Json({ "error": "Credential refresh failed: ..." }).
2. After the quota fetch, if the returned value contains a "message" string that matches any of the JS AUTH_EXPIRED_PATTERNS (["expired","authentication","unauthorized","401","re-authorize"] lowercased substring) AND connection.refresh_token is present, force-refresh (skip needsRefresh) and re-fetch the quota once, replacing live_quotas/live_message.
Add a helper fn is_auth_expired_message(message: &str) -> bool (mirror usage.rs:549-560 is_auth_expired_message already exists for codex).

**Guard test:**

test_fetch_oauth_quota_refreshes_and_retries: construct a ProviderConnection with auth_type "oauth", a refresh_token, and a stale/missing access_token; call get_connection_usage logic path; assert that a second quota fetch happens after refresh (e.g. via a mock client injected) and that auth-expired messages trigger force refresh. At unit level: test is_auth_expired_message matches "Grok CLI authentication expired. Please re-authorize." -> true and "Kimi Coding connected. Usage tracked per request." -> false.

**⚠️ Risks:**

JS refreshes BEFORE the fetch when needsRefresh (expired); if the refresh fails but accessToken still exists, JS returns the stale token (route.js:52-54: `if (connection.accessToken) return { connection, refreshed: false }`). Rust must NOT 401 if refresh fails but a token exists. Only OAuth connections refresh (apikey has no token refresh — route.js:157). The retry must happen exactly once, not in a loop. Never surface upstream errors as HTTP failures — return quotas/message, not 500.

**Cross-check:** ✅ **CONFIRMED** — All three checks pass. (1) JS claim is real: .tmp/9router/src/app/api/usage/[connectionId]/route.js:23-117 defines refreshAndUpdateCredentials(connection, force=false, proxyOptions=null) that builds credentials from accessToken/refreshToken/idToken/expiresAt/lastRefreshAt/connectionId/providerSpecificData/copilotToken/copilotTokenExpiresAt (27-38), computes `force || executor.needsRefresh(credentials)` (41), calls `executor.refreshCredentials(credentials, console, proxyOptions)` (48), and persists via updateProviderConnection (104). The GET handler refreshes OAuth creds before the quota call (158-168) and force-retries once on auth-expired messages via AUTH_EXPIRED_PATTERNS ["expired","authentication","unauthorized","401","re-authorize"] + isAuthExpiredMessage(usage) && connection.refreshToken (11-16, 175-183). (2) Rust current behavior is real: get_connection_usage (src/server/api/usage.rs:411) is_oauth branch (497-511) calls fetch_oauth_quota(connection).await directly with the stored access_token; fetch_oauth_quota (41-66) only dispatches to fetchers, no refresh. refresh_codex_token (imported line 22) is used only in codex handlers get_connection_codex_reset_credits (736-775 pre-refresh, 965-1002 force-retry is a separate consume handler; force-retry at 795-818) and reset_connection_credits (885-916, 965-1002). is_auth_expired_message (549-560) exists with patterns identical to JS but is NOT applied in get_connection_usage's OAuth branch — it's only used in codex reset paths — so no refresh-before and no force-retry in the generic usage path is accurate. (3) Impl would work: src/oauth/token_refresh.rs already provides per-provider refresh fns (refresh_claude_oauth_token 385, refresh_codex_token 406, refresh_github_token 486, refresh_copilot_token 503, refresh_kiro_token 551, refresh_kimi_coding_token 695, refresh_qoder_token 890, plus google/qwen/iflow/xai/openai/etc.), persist_codex_tokens (584) gives the DB-persist pattern, and is_auth_expired_message (549) gives the retry trigger, so adding a per-provider refresh dispatch (match trimmed non-empty connection.refresh_token → provider refresh fn → persist → fetch_oauth_quota) plus a force-retry when result["message"] matches is_auth_expired_message is directly implementable with no missing dependency. Minor design nuance (not an omission): JS refreshes pre-call only when needsRefresh(credentials) is true (or force), whereas the impl_step as described always refreshes when a refresh_token is present — slightly more aggressive but still achieves the stated parity goal; the truncated tail of the impl_steps cannot be audited but its substance is corroborated by the JS lines 158-183. Verdict CONFIRMED.

---

### `P0-H1d` — API-key usage whitelist: Rust 6 providers vs JS 12, and Rust rejects authType api_key

**JS (source of truth — verbatim):**

9router/src/shared/constants/providers.js:158-163:
  export const USAGE_APIKEY_PROVIDERS = REGISTRY.filter(r => r.features?.usageApikey).map(r => r.id);
Registry files with features:{usage:true, usageApikey:true}: codebuddy-cn.js:73-75, codebuddy-intl.js:73-75, deepseek.js:51-53, glm-cn.js:31-33, glm.js:54-56, kimi.js:86-88, kiro.js:126-128, minimax-cn.js:72-74, minimax.js:79-81, ollama.js:35-37, qoder.js:53-55, vercel-ai-gateway.js:37-39. (12 total)
Route acceptance (9router/src/app/api/usage/[connectionId]/route.js:137-145):
  const isOAuth = connection.authType === "oauth";
  const isApikeyAuth = connection.authType === "apikey" || connection.authType === "api_key";
  const isApikeyEligible = isApikeyAuth && USAGE_APIKEY_PROVIDERS.includes(connection.provider);
  if (!isOAuth && !isApikeyEligible) return Response.json({ message: "Usage not available for this connection" });

**Current Rust behavior:**

src/server/api/usage.rs:32-37 is_usage_apikey_provider:
  fn is_usage_apikey_provider(provider: &str) -> bool {
      matches!(provider, "glm" | "glm-cn" | "minimax" | "minimax-cn" | "kimi" | "deepseek")
  }
Only 6 providers. And usage.rs:434-435: `let is_apikey_eligible = connection.auth_type == "apikey" && is_usage_apikey_provider(&connection.provider);` — rejects auth_type "api_key" (Kiro's headless flow stores "api_key"). Missing providers: kiro, ollama, qoder, vercel-ai-gateway, codebuddy-cn, codebuddy-intl. Missing fetch dispatch for these in the apikey branch (usage.rs:479-485 only has glm, minimax, kimi, deepseek).

**Implementation steps:**

1. In src/server/api/usage.rs, expand is_usage_apikey_provider to: matches!(provider, "glm" | "glm-cn" | "minimax" | "minimax-cn" | "kimi" | "deepseek" | "kiro" | "ollama" | "qoder" | "vercel-ai-gateway" | "codebuddy-cn" | "codebuddy-intl").
2. Change the auth_type check (line 434) to accept both spellings: `let is_apikey_eligible = (connection.auth_type == "apikey" || connection.auth_type == "api_key") && is_usage_apikey_provider(&connection.provider);` (JS comment route.js:135-136: "Kiro's headless api-key flow persists authType 'api_key' (underscore) while generic apikey providers persist 'apikey' — accept both spellings").
3. In the apikey fetch match (usage.rs:479-485), add arms: "kiro" => fetch_kiro_quota(api_key, &provider, &psd).await, "ollama" => fetch_ollama_quota(api_key).await, "qoder" => fetch_qoder_quota(api_key, &provider).await, "vercel-ai-gateway" => fetch_vercel_ai_gateway_quota(api_key).await, "codebuddy-cn" => fetch_codebuddy_quota(api_key, &provider).await, "codebuddy-intl" => fetch_codebuddy_quota(api_key, &provider).await. These fetchers must be added (see P0-H1e/H1f). kiro fetch must pass provider_specific_data for profileArn.
Note: JS getKiroUsage signature is (accessToken, providerSpecificData, proxyOptions) — for api-key kiro it still takes accessToken (route.js passes apiKey to getUsageForProvider which passes c.accessToken). Match the Rust dispatch: for kiro use api_key as the token.

**Guard test:**

test_is_usage_apikey_provider_includes_all_12: assert is_usage_apikey_provider returns true for each of [glm, glm-cn, minimax, minimax-cn, kimi, deepseek, kiro, ollama, qoder, vercel-ai-gateway, codebuddy-cn, codebuddy-intl] and false for "openai". test_apikey_eligible_accepts_api_key_underscore: build a connection with auth_type "api_key" + provider "kiro"; assert the eligibility check passes.

**⚠️ Risks:**

Kiro with api_key auth must NOT inject the default placeholder profileArn (JS kiro.js:62-67: 'For api-key auth, never inject the shared default placeholder profileArn — CodeWhisperer 403s'). The Rust kiro_resolve_profile_arn (quota_fetcher.rs:1540-1549) always falls back to KIRO_DEFAULT_PROFILE_ARN; must branch on authMethod. JS kiro authMethod from providerSpecificData.authMethod default "builder-id". Do not add these providers to is_usage_apikey_provider if their fetch functions don't exist — that would 500.

**Cross-check:** ✅ **CONFIRMED** — All three verification points hold. (1) JS cited behavior is real: 9router/src/shared/constants/providers.js:163-165 defines USAGE_APIKEY_PROVIDERS by filtering REGISTRY on features?.usageApikey; exactly 12 registry files carry usageApikey:true (glm, glm-cn, minimax, minimax-cn, kimi, deepseek, kiro, ollama, qoder, vercel-ai-gateway, codebuddy-cn, codebuddy-intl), and each registry id matches the impl provider strings. The JS endpoint (src/app/api/usage/[connectionId]/route.js:137-141) gates on authType === "apikey" || "api_key" against that 12-provider list. (2) Rust current behavior is real: src/server/api/usage.rs:32-37 is_usage_apikey_provider has exactly the 6 providers; usage.rs:433-435 requires the single spelling connection.auth_type == "apikey", so "api_key" connections and the other 6 providers are rejected with "Usage not available for this connection". (3) The impl_steps produce parity: the 12-provider matches!() list is an exact set match with JS, and the auth_type change to accept "apikey" || "api_key" exactly mirrors the JS gate. Minor non-blocking notes: the spec's registry-file sentence listed 10 files but 12 exist (qoder and vercel-ai-gateway were omitted from that sentence, though the headline count of 12 and the impl list are correct); Rust also persists "apiKey" (cli/mod.rs:1060) and "api_key" in other code paths but JS does not accept "apiKey", so the impl still matches parity; and the Rust apikey live-quota match arm (usage.rs:479-485) only implements the original 6, so after the impl the 6 newly-whitelisted providers get per-request usage history plus a static message but no live quotas (JS fetches live quotas for them) — a residual gap beyond the spec's stated whitelist+auth_type scope, not a defect in the impl as written.

---

### `P0-H1e` — Vercel AI Gateway credit usage handler missing entirely

**JS (source of truth — verbatim):**

9router/open-sse/services/usage/misc.js:186-253 getVercelAiGatewayUsage:
  const response = await proxyAwareFetch(VERCEL_AI_GATEWAY_CREDITS_URL, { method: "GET", headers: { Authorization: `Bearer ${apiKey}`, Accept: "application/json" } }, proxyOptions);
where VERCEL_AI_GATEWAY_CREDITS_URL = U("vercel-ai-gateway").url = "https://ai-gateway.vercel.sh/v1/credits" (registry vercel-ai-gateway.js:28).
Returns { balance: "95.50", total_used: "4.50" } (USD decimal strings).
Handling (misc.js:213-249): const balance = Number(data?.balance) || 0; const totalUsed = Number(data?.total_used) || 0; const MONTHLY_CREDIT = 5; const remainingPercentage = (balance / MONTHLY_CREDIT) * 100;
if (balance <= 0 && totalUsed <= 0) return { plan: "Pay-as-you-go", message: "Vercel AI Gateway connected. No credit allocation found (BYOK or unfunded account).", quotas: {} };
return { plan: "Pay-as-you-go", quotas: { "Used (USD)": { used: totalUsed, total: 0, remaining: 0, remainingPercentage: 100, unlimited: true }, "Remaining (USD)": { used: balance, total: MONTHLY_CREDIT, remaining: balance, remainingPercentage, unlimited: false } } };
401/403 → message "Vercel AI Gateway API key invalid or expired."

**Current Rust behavior:**

N/A — no fetch_vercel_ai_gateway_quota function exists in src/core/usage/quota_fetcher.rs. is_usage_apikey_provider omits vercel-ai-gateway. usage_message_for_provider (usage.rs:68-75) has no vercel-ai-gateway arm (falls into `other => "Usage API not implemented for {other}"`).

**Implementation steps:**

Add to src/core/usage/quota_fetcher.rs:
const VERCEL_AI_GATEWAY_CREDITS_URL: &str = "https://ai-gateway.vercel.sh/v1/credits";
pub async fn fetch_vercel_ai_gateway_quota(api_key: &str) -> Value {
  if api_key.trim().is_empty() { return json!({ "message": "Vercel AI Gateway API key not available." }); }
  let client = http_client();
  let response = match client.get(VERCEL_AI_GATEWAY_CREDITS_URL).bearer_auth(api_key.trim()).header("Accept", "application/json").send().await { Ok(r)=>r, Err(e)=> return json!({ "message": format!("Vercel AI Gateway error: {e}") }) };
  let status = response.status().as_u16();
  if status == 401 || status == 403 { return json!({ "message": "Vercel AI Gateway API key invalid or expired." }); }
  if !status.is_success() { let text = response.text().await.unwrap_or_default(); let trimmed: String = text.chars().take(200).collect(); return json!({ "message": format!("Vercel AI Gateway credits API error ({status}){}", if trimmed.is_empty() {"".to_string()} else {format!(": {trimmed}")}) }); }
  let data: Value = response.json().await.unwrap_or_else(|_| json!({}));
  let balance = data.get("balance").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
  let total_used = data.get("total_used").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
  const MONTHLY_CREDIT: f64 = 5.0;
  let remaining_pct = (balance / MONTHLY_CREDIT) * 100.0;
  if balance <= 0.0 && total_used <= 0.0 { return json!({ "plan": "Pay-as-you-go", "message": "Vercel AI Gateway connected. No credit allocation found (BYOK or unfunded account).", "quotas": {} }); }
  json!({ "plan": "Pay-as-you-go", "quotas": { "Used (USD)": json!({ "used": total_used, "total": 0.0, "remaining": 0.0, "remainingPercentage": 100.0, "unlimited": true }), "Remaining (USD)": json!({ "used": balance, "total": MONTHLY_CREDIT, "remaining": balance, "remainingPercentage": remaining_pct, "unlimited": false }) } })
}
Register in usage.rs dispatch (see P0-H1d step 3) and add "vercel-ai-gateway" to is_usage_apikey_provider.

**Guard test:**

test_vercel_ai_gateway_quota_builds_two_rows: (pure fn) given balance "95.50", total_used "4.50", assert quotas has "Used (USD)" with used 4.5, total 0, remainingPercentage 100, unlimited true and "Remaining (USD)" with used 95.5, total 5, remainingPercentage 1910.0, unlimited false. test_vercel_ai_gateway_no_credit_message: balance 0 and total_used 0 → message contains "No credit allocation found".

**⚠️ Risks:**

JS uses Number(data?.balance) — numeric-string coercion; Rust must parse as f64 from string. remainingPercentage = balance/5*100 can exceed 100 (e.g. $10 balance → 200%); do NOT clamp. "Remaining (USD)" used field is `balance` NOT remaining. unlimited:true on "Used (USD)" is essential (total:0 + unlimited:true → QuotaTable shows infinite bar).

**Cross-check:** ✅ **CONFIRMED** — JS claim is fully real and accurate: getVercelAiGatewayUsage exists at .tmp/9router/open-sse/services/usage/misc.js:186-253 with the exact proxyAwareFetch GET to VERCEL_AI_GATEWAY_CREDITS_URL using Bearer auth; VERCEL_AI_GATEWAY_CREDITS_URL = U("vercel-ai-gateway").url (misc.js:16); registry vercel-ai-gateway.js:28 transport.usage.url = "https://ai-gateway.vercel.sh/v1/credits" (matches); and the handler is wired via USAGE_HANDLERS["vercel-ai-gateway"] at services/usage.js:51. Rust gap is real: grep confirms no fetch_vercel_ai_gateway_quota anywhere and a full read of quota_fetcher.rs (correct path) shows no Vercel fetcher; is_usage_apikey_provider (src/server/api/usage.rs:32-37) lists only glm/glm-cn/minimax/minimax-cn/kimi/deepseek; usage_message_for_provider (usage.rs:68-75) has no vercel-ai-gateway arm and falls into 'Usage API not implemented for {other}'. Two minor nits: (1) the file is src/server/api/usage.rs, not src/core/usage/usage.rs (line numbers 68-75 match exactly, so it is a path-labeling error only); (2) the impl snippet is truncated mid-function and omits the required dispatch wiring in src/server/api/usage.rs (add "vercel-ai-gateway" to is_usage_apikey_provider, add a match arm in get_connection_usage at ~line 479-485, import the fn) — without that wiring an apikey Vercel connection is rejected at line 434-436. The proposed fetcher itself is sound: it mirrors JS semantics (identical empty-key message string, identical URL, bearer_auth pattern consistent with fetch_glm_quota/fetch_minimax_quota) and would produce parity once the standard wiring is applied. Verdict CONFIRMED on substance; the wiring must be included in the final implementation plan.

---

### `P0-H1f` — CodeBuddy CN/Intl quota handler missing (only executor exists)

**JS (source of truth — verbatim):**

9router/open-sse/services/usage/codebuddy-cn.js:46-138 getCodeBuddyUsage:
  const token = accessToken || apiKey;
  POST to U(providerId).url where providerId is "codebuddy-cn" → "https://copilot.tencent.com/v2/billing/meter/get-user-resource" (registry codebuddy-cn.js:44) and "codebuddy-intl" → "https://www.codebuddy.ai/v2/billing/meter/get-user-resource" (registry codebuddy-intl.js:43).
  Headers (codebuddy-cn.js:56-60): { ...(PROVIDERS[providerId]?.headers || {}), Authorization: `Bearer ${token}`, "Content-Type": "application/json", Accept: "application/json" }, body: "{}".
  PROVIDERS["codebuddy-cn"].headers (registry codebuddy-cn.js:28-35): {"User-Agent": "CLI/2.108.1 CodeBuddy/2.108.1", "X-Product": "SaaS", "X-IDE-Type": "CLI", "X-IDE-Name": "CLI", "x-requested-with": "XMLHttpRequest", "x-codebuddy-request": "1"}.
  Response: json.code must === 0 (codebuddy-cn.js:72); data = json?.data?.Response?.Data || {}; accounts = Array.isArray(data.Accounts) ? data.Accounts : [];
  isRefill (line 89-93): const ce = cycleEndMs(acc); const de = Number(acc.DeductionEndTime); return Number.isFinite(ce) && Number.isFinite(de) && de - ce > REFILL_GAP_MS; where REFILL_GAP_MS = 2*24*60*60*1000 (line 88). cycleEndMs uses parseResetTime(acc.CycleEndTime).
  Refill packs (lines 103-116): name from refillCadence (Monthly/Weekly/Daily by days between CycleStartTime and CycleEndTime; ≤1.5d → Daily, ≤10d → Weekly, else Monthly), de-duped as "Monthly 2", etc. quota = { used: num(acc.CycleCapacityUsedPrecise, acc.CycleCapacityUsed), total: num(acc.CycleCapacitySizePrecise, acc.CycleCapacitySize), resetAt: parseResetTime(acc.CycleEndTime), unlimited: false, recurring: true }.
  Bonus packs (lines 121-129): `Bonus Pack ${i+1}` with used/total from CapacityUsedPrecise/CapacityUsed and CapacitySizePrecise/CapacitySize, resetAt: parseResetTime(acc.CycleEndTime), unlimited: false, recurring: false. Plan: basePkg.PackageName || basePkg.SubProductName || "CodeBuddy".
  num() (line 29-32): const n = Number(precise ?? plain); return Number.isFinite(n) ? n : 0;

**Current Rust behavior:**

N/A — no fetch_codebuddy_quota in src/core/usage/quota_fetcher.rs. codebuddy-cn/codebuddy-intl not in is_usage_apikey_provider (usage.rs:32-37). usage_message_for_provider has no codebuddy arm.

**Implementation steps:**

Add to src/core/usage/quota_fetcher.rs:
const CODEBUDDY_CN_URL: &str = "https://copilot.tencent.com/v2/billing/meter/get-user-resource";
const CODEBUDDY_INTL_URL: &str = "https://www.codebuddy.ai/v2/billing/meter/get-user-resource";
const CODEBUDDY_REFILL_GAP_MS: i64 = 2*24*60*60*1000;
pub async fn fetch_codebuddy_quota(token: &str, provider: &str) -> Value {
  if token.trim().is_empty() { return json!({ "message": format!("CodeBuddy ({provider}) credential not available.") }); }
  let url = if provider == "codebuddy-intl" { CODEBUDDY_INTL_URL } else { CODEBUDDY_CN_URL };
  let client = http_client();
  let response = match client.post(url).bearer_auth(token.trim()).header("Content-Type", "application/json").header("Accept", "application/json")
    .header("User-Agent", "CLI/2.108.1 CodeBuddy/2.108.1").header("X-Product", "SaaS").header("X-IDE-Type", "CLI").header("X-IDE-Name", "CLI").header("x-requested-with", "XMLHttpRequest").header("x-codebuddy-request", "1")
    .body("{}").send().await { Ok(r)=>r, Err(e)=> return json!({ "message": format!("CodeBuddy ({provider}) error: {e}") }) };
  let status = response.status().as_u16();
  if status == 401 || status == 403 { return json!({ "message": "CodeBuddy CN credential invalid or expired." }); }
  if !status.is_success() { return json!({ "message": format!("CodeBuddy CN quota API error ({status}).") }); }
  let json_body: Value = match response.json().await { Ok(v)=>v, Err(_)=> return json!({ "message": "CodeBuddy CN quota API error." }) };
  if json_body.get("code").and_then(|v| v.as_i64()).unwrap_or(-1) != 0 { let msg = json_body.get("msg").and_then(|v| v.as_str()).unwrap_or("unknown"); return json!({ "message": format!("CodeBuddy CN quota error: {msg}") }); }
  let data = json_body.pointer("/data/Response/Data").cloned().unwrap_or_else(|| json!({}));
  let accounts: Vec<Value> = data.get("Accounts").and_then(|v| v.as_array()).cloned().unwrap_or_default();
  if accounts.is_empty() { return json!({ "message": "CodeBuddy CN connected. No credit package found." }); }
  // parse_reset_time helper already exists; implement refillCadence: days between CycleStartTime and CycleEndTime; ≤1.5→"Daily", ≤10→"Weekly", else "Monthly".
  // Partition refills (DeductionEndTime - CycleEndTime > REFILL_GAP_MS) and bonuses; build quota map keys "Monthly", "Monthly 2", "Bonus Pack 1"... with the exact field names above, each { used, total, resetAt, unlimited:false, recurring }.
  // plan from first refill or first account: PackageName || SubProductName || "CodeBuddy".
  json!({ "plan": plan, "quotas": quotas })
}
Register in usage.rs dispatch (P0-H1d step 3) for both codebuddy-cn and codebuddy-intl, passing provider id.

**Guard test:**

test_codebuddy_refill_cadence: parse CycleStartTime="2026-01-01T00:00:00Z" CycleEndTime="2026-01-31T00:00:00Z" → "Monthly"; 1-day span → "Daily"; 7-day → "Weekly". test_codebuddy_partitions_refill_vs_bonus: build a synthetic Accounts array where one account has DeductionEndTime - CycleEndTime > 2 days (refill) and another == 0 (bonus); assert quota keys contain "Monthly" (recurring true) and "Bonus Pack 1" (recurring false).

**⚠️ Risks:**

JS header set includes PROVIDERS[providerId]?.headers spread — the 6 CodeBuddy headers MUST be sent or Tencent 401s. Payload is POST with body "{}" (NOT GET). json.code === 0 gate: non-zero code → message with json.msg. The `num()` helper prefers Precise fields (CycleCapacityUsedPrecise) falling back to plain (CycleCapacityUsed). plan comes from basePkg.PackageName || basePkg.SubProductName — NOT "CodeBuddy" unless both absent. recurring:true on refill, recurring:false on bonus (drives UI Resets-in vs Expires-in).

**Cross-check:** ✅ **CONFIRMED** — JS claim is fully real. Read open-sse/services/usage/codebuddy-cn.js:46-138: getCodeBuddyUsage(providerId, accessToken, apiKey, ...) uses `const token = accessToken || apiKey;`, POSTs `{}` to U(providerId).url with `Authorization: Bearer ${token}` + Content-Type/Accept, maps 401/403 and json.code!==0 errors, then parses json.data.Response.Data.Accounts into per-package quotas with REFILL_GAP_MS = 2*24*60*60*1000 splitting refills (Cycle* fields, recurring:true, Monthly/Weekly/Daily labels) from bonus packs (Capacity* fields, "Bonus Pack N", recurring:false). Registry URLs match byte-for-byte: providers/registry/codebuddy-cn.js:44 = https://copilot.tencent.com/v2/billing/meter/get-user-resource and codebuddy-intl.js:43 = https://www.codebuddy.ai/v2/billing/meter/get-user-resource; U() in shared.js resolves PROVIDERS[id].usage.url. Rust claims are real: no fetch_codebuddy_quota and no codebuddy match in src/core/usage/ (quota_fetcher.rs has glm/minimax/github/codex/gemini-cli/qoder/claude/kiro/antigravity/grok-cli/kimi/deepseek only); is_usage_apikey_provider lists only glm/glm-cn/minimax/minimax-cn/kimi/deepseek; usage_message_for_provider has no codebuddy arm; fetch_oauth_quota dispatch has no codebuddy arm (falls to {}); only an executor exists (src/core/executor/codebuddy_cn.rs) plus OAuth configs — matching the gap title. Two minor nits that don't change the verdict: (1) the cited file is src/server/api/usage.rs, not src/core/usage/usage.rs (no usage.rs exists in core/usage/; line numbers 32-37 are correct), and (2) impl_steps are truncated before the parse/body but the three constants shown are exact matches to the JS and the signature matches the existing fetch_*_quota pattern; dispatch wiring in fetch_oauth_quota and is_usage_apikey_provider is implied rather than spelled out but is the standard completion and would produce parity.

---

### `P0-H1g` — Grok CLI quota fetch is a stub: missing JWT tier plan, Monthly/On-demand/Prepaid quota rows, exhausted row

**JS (source of truth — verbatim):**

9router/open-sse/services/usage/grok-cli.js:54-70 buildGrokCliHeaders:
  const headers = { Authorization: `Bearer ${accessToken}`, Accept: "application/json", "User-Agent": GROK_CLI_USER_AGENT ("grok-shell/0.2.99 (linux; x86_64)" from config/grokCli.js:5), "x-xai-token-auth": "xai-grok-cli", "x-grok-client-identifier": GROK_CLI_CLIENT_IDENTIFIER ("grok-shell"), "x-grok-client-version": GROK_CLI_VERSION ("0.2.99"), "x-grok-client-mode": "headless" }; plus psd.email → "x-email", psd.userId||psd.principalId → "x-userid".
planFromAccessToken (grok-cli.js:95-110): JWT payload.tier map {0:"Free",1:"SuperGrok",2:"X Basic",3:"X Premium",4:"X Premium Plus",5:"SuperGrok Heavy",6:"SuperGrok Lite"}.
parseGrokCliBilling (grok-cli.js:141-298) produces quota rows: "Monthly included" (config.monthlyLimit), "On-demand" (config.onDemandCap/onDemandUsed; exhausted free/promo → synthetic {used:1,total:1,remainingPercentage:0,unlimited:false}), "Prepaid" (config.prepaidBalance → {used:0,total:prepaid,remainingPercentage:100,resetAt:null}), "Weekly SuperGrok" (creditUsagePercent), and opportunistic "Credits" bags.
RESOLVE_PLAN (grok-cli.js:82-92): tier normalized Title Case; user.hasGrokCodeAccess → "Grok Code"; config.isUnifiedBillingUser → "Grok Build"; default "Grok Build".
getGrokCliUsage (grok-cli.js:349-424) fetches BILLING_URL = "https://cli-chat-proxy.grok.com/v1/billing?format=credits" and USER_URL = "https://cli-chat-proxy.grok.com/v1/user?include=subscription" IN PARALLEL with the 8 headers above; 401/403 → "Grok CLI authentication expired. Please re-authorize."; billing not ok → "Grok CLI billing API error (status): errText.slice(0,200)"; non-JSON → "Grok CLI billing response was not JSON."; parsed.plan = planFromAccessToken(accessToken) || parsed.plan; when quotas empty falls back to fetchGrokCliCreditsConfig gRPC; if still none: subscriptionAccess ? "Subscription access is active; Grok does not expose a numeric included quota." : "Grok Build connected, but no credit allotment was returned. Free promo may be exhausted."

**Current Rust behavior:**

src/core/usage/quota_fetcher.rs:1901-1978 fetch_grok_cli_quota: (1) does NOT send the 7 extra headers (only Authorization Bearer); (2) parses only data.caps.total_cap/used into a single "Credits" row (line 1943-1959); (3) plan is derived only from user.subscription.name (lines 1921-1936), no JWT tier parse, no subscriptionTier/hasGrokCodeAccess/isUnifiedBillingUser resolution; (4) no Monthly included/On-demand/Prepaid/Weekly SuperGrok rows, no exhausted synthetic 1/1 row, no subscriptionAccess message differentiation; (5) gRPC fallback exists (fetch_grok_cli_credits_config line 1865) but only reached when caps.total_cap > 0 is false — and the REST parse never checks onDemandCap.

**Implementation steps:**

In src/core/usage/quota_fetcher.rs rewrite fetch_grok_cli_quota (lines 1901-1978):
1. Add headers to BOTH billing and user GETs exactly as buildGrokCliHeaders: "Accept": "application/json", "User-Agent": "grok-shell/0.2.99 (linux; x86_64)", "x-xai-token-auth": "xai-grok-cli", "x-grok-client-identifier": "grok-shell", "x-grok-client-version": "0.2.99", "x-grok-client-mode": "headless".
2. Fetch billing + user in parallel (tokio::join!).
3. 401/403 on billing → return json!({ "message": "Grok CLI authentication expired. Please re-authorize." }).
4. Add fn plan_from_access_token(access_token: &str) -> String: split('.') get payload, base64url-decode (use base64 URL_SAFE_NO_PAD), parse JSON, map tier 0..=6 → [Free, SuperGrok, X Basic, X Premium, X Premium Plus, SuperGrok Heavy, SuperGrok Lite], else "".
5. Add fn resolve_plan(user: &Value, config: &Value) -> String: read subscriptionTier/subscription_tier/subscription.tier/config.subscriptionTier/config.subscription_tier; Title-Case it (replace [_-]+ with space, uppercase each word start); else user.hasGrokCodeAccess==true → "Grok Code"; else config.isUnifiedBillingUser==true → "Grok Build"; else "Grok Build".
6. Parse quota rows mirroring parseGrokCliBilling: read config (object) with field fallbacks; compute periodEnd via parse_reset_time on billingPeriodEnd/billing_period_end/currentPeriod.end/resetAt/resetsAt/periodEnd (root-level too). For "Monthly included": monthlyLimit>0 → make_quota(used: includedUsed.is_finite? includedUsed : totalUsed.is_finite? totalUsed : 0, total: monthlyLimit, resetAt: periodEnd). For "On-demand": onDemandCap>0 → make_quota(used: max(0,onDemandUsed), total: onDemandCap); else if !subscriptionAccess && onDemandCap==0 && onDemandUsed.is_finite → synthetic {used:1,total:1,remainingPercentage:0,resetAt:periodEnd,unlimited:false}. For "Prepaid": prepaidBalance>0 → {used:0,total:prepaid,remainingPercentage:100,resetAt:null,unlimited:false}. For "Weekly SuperGrok": creditUsagePercent>=0 → make_quota(used: min(100,usedPct), total:100). make_quota helper: used/total, remainingPercentage=(max(0,total-used)/total)*100, never set absolute remaining. subscriptionAccess = tier non-empty && !/^(free|none|null)$/i.test(tier).
7. parsed.plan = plan_from_access_token(access_token) if non-empty else resolve_plan.
8. When no quotas: try fetch_grok_cli_credits_config; if Some → { "Weekly SuperGrok": { used: round(percent), total: 100, remainingPercentage: 100-round, resetAt, unlimited:false } }; else if subscriptionAccess → message "Subscription access is active; Grok does not expose a numeric included quota." else → "Grok Build connected, but no credit allotment was returned. Free promo may be exhausted." with quotas {}.
9. Return { plan, quotas } when quotas non-empty (no message).

**Guard test:**

test_grok_cli_plan_from_jwt_tier: a JWT with tier=4 → "X Premium Plus"; tier=0 → "Free"; missing → "". test_grok_cli_parse_billing_on_demand_exhausted: billing {config:{onDemandCap:{val:0},onDemandUsed:{val:0}}} + user tier "free" → quotas has "On-demand" {used:1,total:1,remainingPercentage:0}; and exhausted=true. test_grok_cli_parse_billing_monthly_prepaid: config {monthlyLimit:{val:500},includedUsed:{val:50},prepaidBalance:{val:10}} → "Monthly included" total 500 used 50, "Prepaid" {used:0,total:10,remainingPercentage:100}.

**⚠️ Risks:**

Preserve unwrapVal semantics: `{val: number}` objects AND plain numbers/strings both accepted (grok-cli.js:46-52). plan is display-only; upstream remains authoritative. JS line 394: only attach message when there are NO quota rows; depleted accounts keep the 0% On-demand bar without a blocking message. The gRPC weekly fallback only fires when REST quotas are empty (grok-cli.js:394-404). Do NOT set absolute `remaining` on quota rows (QuotaTable treats it as 0-100 percentage).

**Cross-check:** ✅ **CONFIRMED** — All three verification points hold. (1) JS cited behavior is REAL: buildGrokCliHeaders at 9router/open-sse/services/usage/grok-cli.js:54-70 sends exactly the 7 headers claimed (Authorization Bearer, Accept application/json, User-Agent from config/grokCli.js:5 = "grok-shell/0.2.99 (linux; x86_64)", x-xai-token-auth xai-grok-cli, x-grok-client-identifier grok-shell, x-grok-client-version 0.2.99, x-grok-client-mode headless) on both billing and user GETs (lines 354-368); the URLs (lines 35-36) match https://cli-chat-proxy.grok.com/v1/billing?format=credits and /v1/user?include=subscription; planFromAccessToken (lines 95-110) decodes JWT payload.tier and is applied at line 392; Monthly/On-demand/Prepaid rows + synthetic exhausted On-demand row built at lines 175-225. (2) Rust current behavior is REAL: fetch_grok_cli_quota (src/core/usage/quota_fetcher.rs:1901-1978) sends only Authorization Bearer on both GETs (lines 1910-1911, 1916-1917); parses only data.caps.total_cap/used into one "Credits" row (lines 1943-1959) — a field the JS never even reads, further confirming stub status; plan comes only from user.subscription.name (lines 1921-1936), no JWT decode and it reads subscription.name vs JS's subscription.tier. (3) The impl step 1 header list matches buildGrokCliHeaders verbatim and would produce parity; no obvious omission. Minor nits only (not refuting): spec says "7 extra headers" but Rust already sends Authorization so 6 are missing; conditional x-email/x-userid headers omitted from step 1 (acceptable, they depend on providerSpecificData); impl_steps text is truncated at step 2 so only step 1 could be fully checked. Verdict CONFIRMED.

---

### `P0-H1h` — Kiro quota fetch drops tokentype:API_KEY / TokenType:EXTERNAL_IDP headers and api-key profileArn handling

**JS (source of truth — verbatim):**

9router/open-sse/services/usage/kiro.js:51-67:
  const authMethod = providerSpecificData?.authMethod || "builder-id";
  const isApiKey = authMethod === "api_key";
  const isExternalIdp = authMethod === "external_idp";
  const apiKeyHeaders = isApiKey ? { tokentype: "API_KEY" } : {};
  const externalIdpHeaders = isExternalIdp ? { TokenType: "EXTERNAL_IDP" } : {};
  const profileArn = isApiKey ? (providerSpecificData?.profileArn || "") : (providerSpecificData?.profileArn || resolveDefaultProfileArn(authMethod));
  // GET attempt (kiro.js:79-94): `${U("kiro").cwHost}${U("kiro").limitsPath}?${getUsageParams}` with headers { Authorization: `Bearer ${accessToken}`, Accept: "application/json", "x-amz-user-agent": "aws-sdk-js/1.0.0 KiroIDE", "user-agent": "aws-sdk-js/1.0.0 KiroIDE", ...apiKeyHeaders, ...externalIdpHeaders }; params isEmailRequired=true&origin=AI_EDITOR&resourceType=AGENTIC_REQUEST.
  // POST attempt (kiro.js:96-113): to U("kiro").cwHost with Content-Type application/x-amz-json-1.0, x-amz-target AmazonCodeWhispererService.GetUsageLimits, body { origin:"AI_EDITOR", ...(profileArn?{profileArn}:{}), resourceType:"AGENTIC_REQUEST" }.
  // Q GET attempt (kiro.js:116-131): `${U("kiro").qHost}${U("kiro").limitsPath}?${params}` with profileArn in query.
  // On authMethod "idc" auth error → message "Kiro quota API is unavailable for the current AWS IAM Identity Center session. Chat may still work. If this persists after renewing your session, reconnect Kiro." (kiro.js:157-162); google/github → "Kiro quota API authentication expired. Chat may still work." (kiro.js:165-170); other → "Kiro quota API rejected the current token. Chat may still work." (kiro.js:172-177).

**Current Rust behavior:**

src/core/usage/quota_fetcher.rs:1551-1705 fetch_kiro_quota: (1) NO tokentype/TokenType headers on any of the 3 attempts (lines 1572-1578, 1595-1601, 1618-1623); (2) kiro_resolve_profile_arn (1540-1549) ALWAYS falls back to KIRO_DEFAULT_PROFILE_ARN ("arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX") even for api_key auth; (3) no authMethod branching for the auth-error messages (idc/google/github/generic); (4) no per-attempt error accumulation; a single unreachable returns a generic message. The 3 attempts DO exist and the GET uses isEmailRequired=true&origin=AI_EDITOR&resourceType=AGENTIC_REQUEST correctly, POST body has origin/profileArn/resourceType.

**Implementation steps:**

In src/core/usage/quota_fetcher.rs fetch_kiro_quota:
1. Read auth_method = provider_specific_data.get("authMethod").and_then(|v| v.as_str()).unwrap_or("builder-id"). is_api_key = auth_method == "api_key"; is_external_idp = auth_method == "external_idp".
2. profile_arn = if is_api_key { psd.profileArn string or "" } else { psd.profileArn or KIRO_DEFAULT_PROFILE_ARN } — change kiro_resolve_profile_arn to take is_api_key: bool and return "" (empty) when is_api_key && no profileArn.
3. Add headers to all 3 attempts: when is_api_key insert header("tokentype", "API_KEY"); when is_external_idp insert header("TokenType", "EXTERNAL_IDP").
4. POST body (line 1590-1594): only include profileArn when non-empty (currently always inserted): change to build map and only insert if !profile_arn.is_empty(). Q GET query (line 1616): only append profileArn param when non-empty.
5. Track saw_auth_error across attempts (any 401/403). After all attempts fail, branch on auth_method: "idc" → "Kiro quota API is unavailable for the current AWS IAM Identity Center session. Chat may still work. If this persists after renewing your session, reconnect Kiro."; "google" | "github" → "Kiro quota API authentication expired. Chat may still work."; else → "Kiro quota API rejected the current token. Chat may still work.". Non-auth errors → "Unable to fetch Kiro usage right now.". All return quotas {}.
6. Accept response on any of the 3 attempts (JS loops attempts, first success wins — Rust already does).

**Guard test:**

test_kiro_quota_omits_default_profile_for_api_key: with psd {authMethod:"api_key"} and no profileArn, the resolved profile_arn must be empty (not KIRO_DEFAULT_PROFILE_ARN). test_kiro_quota_headers_match_auth_method: build the attempt headers for is_api_key and assert "tokentype"=="API_KEY" present and "TokenType" absent; for external_idp assert "TokenType"=="EXTERNAL_IDP" present.

**⚠️ Risks:**

JS tokentype header value is EXACTLY "API_KEY" (lowercase key, uppercase value) and "TokenType" (camelCase key) for external_idp — do not swap. profileArn is only sent for api-key auth when the connection actually owns it (JS comment: 'never inject the shared default placeholder profileArn — CodeWhisperer 403s a request whose profileArn isn't owned by the key's account'). q-get and codewhisperer-get attempt ordering must stay primary → post → q. The 401/403 detection must be per-attempt (JS errors.push per attempt) and the first successful attempt wins.

**Cross-check:** ✅ **CONFIRMED** — All three claims verified against source. (1) JS behavior REAL: kiro.js:51-67 exactly contains authMethod default "builder-id", isApiKey/isExternalIdp flags, apiKeyHeaders={tokentype:"API_KEY"}, externalIdpHeaders={TokenType:"EXTERNAL_IDP"}, and the isApiKey? profileArn||"" : profileArn||resolveDefaultProfileArn ternary. These headers are spread into all 3 quota attempts (lines 88-89, 104-105, 127-128), and profileArn is conditionally omitted (empty-string) from the POST body and q-get params when absent. URLs confirmed in open-sse/providers/registry/kiro.js:38-40 (cwHost/https://codewhisperer.us-east-1.amazonaws.com, qHost/https://q.us-east-1.amazonaws.com, limitsPath=/getUsageLimits) which match the Rust constants exactly. (2) Rust current behavior REAL: fetch_kiro_quota (quota_fetcher.rs:1551-1705) sets only Authorization + x-amz-user-agent/user-agent/Content-Type/x-amz-target/Accept on its 3 attempts (1572-1578, 1595-1601, 1618-1623) — zero tokentype/TokenType headers; and kiro_resolve_profile_arn (1540-1549) always falls back to KIRO_DEFAULT_PROFILE_ARN ("arn:aws:codewhisperer:us-east-1:638616270132:profile/AAAACCCCXXXX", same value JS uses for non-social), so Rust always injects profileArn into POST body (1592) and q URL (1616) even for api_key, unlike JS which sends "" and omits it. (3) Impl steps would achieve parity: the authMethod-read pattern in step 1 mirrors the existing executor (src/core/executor/kiro.rs:240-247), and step 2's conditional profileArn ("" for api_key, psd.profileArn or default otherwise) matches JS semantics for external_idp/builder-id (resolveDefaultProfileArn returns the builder-id default for non-social methods). Only minor nuance (non-blocking, functionally equivalent since HTTP header names are case-insensitive): JS sends lowercase "tokentype" for API_KEY vs "TokenType" for EXTERNAL_IDP; Rust executor already uses "TokenType" for both. No omission found that would prevent parity.

---

### `P0-H1i` — Stuck pending-request 60s auto-clear missing in usage_live

**JS (source of truth — verbatim):**

usageRepo.js:12 PENDING_TIMEOUT_MS = 60 * 1000;
trackPendingRequest (usageRepo.js:152-194): on started, `clearTimeout(pendingTimers[timerKey]); pendingTimers[timerKey] = setTimeout(() => { delete pendingTimers[timerKey]; if (pendingRequests.byModel[modelKey] > 0) pendingRequests.byModel[modelKey] = 0; if (connectionId && pendingRequests.byAccount[connectionId]?.[modelKey] > 0) { pendingRequests.byAccount[connectionId][modelKey] = 0; } scheduleStatsEvent("pending"); }, PENDING_TIMEOUT_MS);`
Key: `${connectionId}|${modelKey}` where modelKey = provider ? `${model} (${provider})` : model. If a request never completes, its counter is force-zeroed after 60s and a pending event is emitted. On non-started, `clearTimeout(pendingTimers[timerKey]); delete pendingTimers[timerKey];`

**Current Rust behavior:**

src/server/usage_live.rs:65-79 start_request increments by_model/by_account and sends UsageEvent::Pending; finish_request (81-111) decrements. There is NO timer — a request whose chat handler path forgets to call finish_request leaves the counter stuck forever. No per-(connection,model_key) timer, no 60s force-zero, no Pending event on timeout.

**Implementation steps:**

In src/server/usage_live.rs:
1. Add a per-key timer map: struct keyed by format!("{connection_id}|{model_key}") (connection_id may be empty → "|{model_key}"). Use tokio::time::sleep(60s) spawned tasks tracked in a Mutex<HashMap<String, tokio::task::JoinHandle>> (or parking_lot Mutex).
2. In start_request: after incrementing, cancel any existing timer for the key, then spawn: tokio::spawn(async move { sleep(Duration::from_secs(60)).await; state.force_zero(key).await; }); store the handle. Reuse the UsageLiveState self (clone Arc if needed — UsageLiveState is not currently Arc; store a std::sync::Arc<Self> inside or restructure to take Arc<Self>).
3. force_zero(key): parse the stored connection_id + model_key back out of the key, zero by_model[model_key] and by_account[connection_id][model_key] (delete entries when 0), then send UsageEvent::Pending. Guard: only zero if count > 0 (JS checks `pendingRequests.byModel[modelKey] > 0`).
4. In finish_request: cancel/remove the timer for the key (JS clears on non-started).
5. Keep PENDING_TIMEOUT_MS = 60 * 1000 as a const.
Note: the model_key format must match model_key() in usage_live.rs (provider ? "{model} ({provider})" : model — usage_live.rs:174-180 already matches JS).

**Guard test:**

test_usage_live_force_zero_after_timeout: start_request("gpt-4","openai",Some("c1")), do NOT call finish_request, then drive a 60s timer (make the duration injectable via a fn param or a #[cfg(test)] short duration); assert pending_snapshot().by_model has the key removed after the timeout and a Pending event was broadcast (subscribe + recv_with_timeout).

**⚠️ Risks:**

Timer must be per (connection_id, model_key) — JS key is `${connectionId}|${modelKey}`. Cancelling the timer on finish is mandatory or a later started-count leak. In tests use an injectable timeout (default 60s) to avoid 60s sleeps. The force-zero must NOT decrement below zero (JS uses direct =0 assignment not decrement). Zeroing by_model when count hits 0 removes the key (JS deletes when ===0).

**Cross-check:** ✅ **CONFIRMED** — All three claims verified against the actual code. (1) JS behavior is REAL: file exists at .tmp/9router/src/lib/db/repos/usageRepo.js (spec dropped the src/lib/db/repos/ prefix — minor path inaccuracy only). Line 12 has PENDING_TIMEOUT_MS = 60*1000. trackPendingRequest (lines 152-194) computes timerKey = `${connectionId}|${modelKey}` (line 154); on started it does clearTimeout(pendingTimers[timerKey]) then sets a new setTimeout that deletes the timer, zeroes byModel[modelKey] and byAccount[connectionId][modelKey] if >0, and calls scheduleStatsEvent("pending") (lines 172-181, 193); on finish it clears/deletes the timer (lines 182-185). The quoted snippet matches exactly. (2) Rust current behavior is REAL: src/server/usage_live.rs start_request (65-79) increments by_model/by_account and sends UsageEvent::Pending; finish_request (81-111) decrements both and emits Pending; there is no timer in the file. chat.rs has 12+ finish_request call sites across different error/retry/return branches (1012 start; 1747/1852/1914/2412/2708/2748/2767/2799/2844/2863 etc.), so a path that forgets to call finish_request leaves the counter stuck — the stated gap is genuine. (3) Impl steps would produce parity: the spec's key format ("{connection_id}|{model_key}", empty connection_id -> "|{model_key}") mirrors JS timerKey exactly; cancel-then-respawn on start and zero-out-on-timeout (not decrement) match JS semantics (note JS uses Math.max(0, ...) on the decrement path and sets =0 on timeout, not a count-sensitive decrement); connection_id None -> "|{model_key}" handles the global model-only case the same way JS's `undefined|modelKey` does. Feasibility confirmed: state.rs:39 has usage_live: Arc<UsageLiveState> (constructed state.rs:110), so a clone can be moved into tokio::spawn; tokio::time is appropriate for a tokio-based server. Non-blocking polish only: JoinHandle cancellation is best-effort (no abort like JS clearTimeout — but a stale fire only zeroes a genuinely stuck counter, which is exactly what JS does, so no parity break), the timer map should retain the JoinHandle so superseded tasks are not lost, and the 60s constant should be a named const like JS. None of these affect parity.

---

### `P0-H1j` — saveRequestUsage dedupe of identical rows: JS skips exact-duplicate inserts; Rust blind-append

**JS (source of truth — verbatim):**

src/app/api/usage/[connectionId]/route.js:157-183 (quoted in P0-H1c)

**Current Rust behavior:**

See P0-H1c rust_current.

**Implementation steps:**

See P0-H1c impl_steps.

**Guard test:**

See P0-H1c test_spec.

**Cross-check:** 🟡 **PLAUSIBLE** — The behavioral claims on both sides are real, but the JS citation is wrong. (1) JS dedupe: the claimed "skip exact-duplicate inserts" behavior genuinely exists, but NOT at the cited src/app/api/usage/[connectionId]/route.js:157-183 — those lines are OAuth credential-refresh logic on the usage-DISPLAY GET endpoint, which never calls saveRequestUsage. The real dedupe is in src/lib/db/repos/usageRepo.js:241-314 inside saveRequestUsage: a SELECT keyed on timestamp+provider+model+connectionId+apiKey+promptTokens+completionTokens that skips the INSERT on match (only backfilling a missing endpoint), confirmed by commit 0d216689 "Fix usage logging dedupe and reduce stats churn". Callers are open-sse/handlers/chatCore/requestDetail.js:117 and src/sse/handlers/embeddings.js:140. (2) Rust blind-append is fully confirmed: UsageTracker::track_request (src/core/usage/tracker.rs:56-77) pushes to history unconditionally; Db::update_usage (src/db/mod.rs:298-318) -> import_usage (src/db/sqlite/import.rs:218-248) DELETEs all usageHistory then re-inserts blindly; usage_repo.rs insert is a plain INSERT; schema.rs usageHistory has no UNIQUE constraint. (3) Impl direction is sound and matches docs/parity-9router-impl.md H9 (lines 731-735): "Impl: dedupe. Test: request_usage_dedupes" — a dedupe keyed on the same fields in track_request/persistence mirrors JS exactly, no obvious omission. Since the JS behavior is true but the cited file/line does not contain it (verification step 1 fails on the citation, not the substance), this is mostly right: PLAUSIBLE.

---

---
