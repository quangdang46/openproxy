# Plan: Open-sse Rust Completion

**Date**: 2026-05-04
**Feature**: open-sse-rust-completion
**Status**: Draft
**Owner**: quangdang

---

## 0. Context Reference

- Context file: `plans/open-sse-rust-completion/CONTEXT.md`
- Locked decisions in force: `D1`, `D2`, `D3`, `D4`, `D5`
- Planning rule: this plan must not override the locked context silently
- Recommended starting module: translator, specifically request-side translation plus shared chat-core extraction. Executors come second. RTK comes third.

---

## 1. Goal

- Goal: finish the remaining backend/runtime port so the live product can run with Rust backend semantics and the existing Next.js dashboard, with `open-sse/*` no longer needed on active request paths.
- Non-goals: dashboard rewrite, new provider additions outside the current matrix, or a rewrite of the separate `cloud/` worker.
- Constraints: preserve current Rust route behavior, keep dashboard work deferred, reuse existing Rust modules where possible, and maintain compatibility with OpenAI, Anthropic, Gemini, and Responses-style client surfaces.
- Acceptance signal: active runtime paths no longer depend on `open-sse/*`; targeted parity tests for chat/messages/responses/provider flows pass; `cargo clippy --tests --all-targets -- -D warnings` and relevant `cargo test` slices pass; live curl checks pass.

---

## 2. Current Repo Truth

> What exists now. This is the baseline `encode` and `operate` must preserve unless the plan says otherwise.

- Rust already mounts a broad compatibility and management API surface in `src/server/api/mod.rs`, including `/v1/chat/completions`, `/v1/messages`, `/v1/responses`, `/v1/responses/compact`, `/v1/web/fetch`, and many `/api/*` routes.
- `src/server/api/compat.rs` already normalizes `messages` and `responses` request shapes, but then forwards into the same `src/server/api/chat.rs` flow. Route presence is not the main gap.
- `src/server/api/chat.rs` already owns combo routing, request preprocessing, provider fallback, and stream/non-stream response bridging, but it is still a large route-level module rather than a shared runtime core.
- Rust already includes meaningful runtime building blocks: `src/core/account_fallback/mod.rs`, `src/core/usage/*`, `src/core/translator/response_transform.rs`, and specialized executors for `codex`, `cursor`, `kiro`, `ollama`, `vertex`, `grok-web`, and `perplexity-web`.
- The remaining JS runtime delta is concentrated in `open-sse/*`: request translators, centralized `chatCore`, RTK compression filters, token refresh and project-id glue, proxy/header utilities, and several provider-specific executors.
- The rough executor backlog is narrower than the user's initial summary: `perplexity-web`, `azure`, and `opencode` already have Rust support paths, so the real executor gaps are closer to `antigravity`, `gemini-cli`, `iflow`, `qoder`, and `qwen`, plus any provider that fails a later parity audit.

---

## 3. Discovery

### Local Repo

- `docs/ARCHITECTURE.md` still documents `open-sse/*` as the SSE and translation core, including the current JS handling for `/v1/messages` and `/v1/responses`. The plan must replace this architecture, not merely add more Rust routes.
- `src/core/translator/mod.rs` currently exposes only `TranslationFormat` plus `response_transform`, while `open-sse/translator/index.js` lazily registers both request and response translators, performs an OpenAI-intermediate hop, and applies normalization hooks.
- `src/core/rtk/mod.rs` implements caveman/system-prompt injection and token estimation only. The JS RTK pipeline spans `open-sse/rtk/index.js`, `autodetect.js`, `applyFilter.js`, and ten filter files, with parity fixtures already present in `tests/unit/rtk.test.js`.
- `src/core/executor/default.rs` and `src/core/executor/provider.rs` already handle several providers frequently assumed missing, including `azure`, `opencode`, `opencode-go`, and compatible-provider URL/header logic.
- Specialized JS behavior still lives outside Rust for `antigravity`, `gemini-cli`, `iflow`, `qoder`, `qwen`, and cross-cutting helpers such as `proxyFetch`, `tokenRefresh`, `projectId`, `clientDetector`, `claudeHeaderCache`/`claudeCloaking`, and `sessionManager`.
- Existing JS tests are directly useful as parity fixtures, especially `tests/unit/translator-request-normalization.test.js`, `tests/unit/openai-to-claude.test.js`, `tests/unit/rtk.test.js`, `tests/unit/perplexity-web.test.js`, and `tests/unit/claude-header-forwarding.test.js`.
- `src/server/api/cloud_sync.rs` only exposes `/api/init`, while `src/server/api/cloud_credentials.rs` already ports part of the local cloud API surface. Cloud-sync parity is partial rather than blank.

### Official Docs

- OpenAI Responses API docs: https://platform.openai.com/docs/api-reference/responses
  - The official surface treats `responses` as a first-class API with distinct request/streaming behavior, so Rust parity should not treat `/v1/responses*` as a thin alias over chat completions.
- Anthropic Messages docs: https://docs.anthropic.com/en/api/messages
  - Anthropic's event model is message/content-block oriented. Rust streaming translation needs to preserve that structure before mapping it into OpenAI-style chunks.
- Anthropic streaming docs: https://docs.anthropic.com/en/api/messages-streaming
  - Streaming semantics and event ordering are explicit enough to drive a formal Rust response bridge.
- Gemini API docs: https://ai.google.dev/gemini-api/docs/text-generation
  - Gemini distinguishes `generateContent` and `streamGenerateContent`, which supports keeping a dedicated Gemini translation path rather than forcing everything through OpenAI chat JSON.

### Upstream Patterns

- `open-sse/handlers/chatCore.js` centralizes detect -> translate -> RTK/caveman -> executor dispatch -> refresh/fallback -> stream/non-stream normalization. The Rust equivalent should be a shared core module, not more route-local logic.
- `open-sse/translator/index.js` uses a small `source:target` registry with an OpenAI-intermediate hop when formats differ. This pattern keeps provider executors focused on transport/auth instead of request semantics.
- In JS, providers only get bespoke executors when they need custom auth, query, or session behavior. Everything else rides a default executor path. Rust should preserve that rule rather than over-specializing.

### Inference

- The main technical risk is contract duplication. Rust already has request-shaping logic spread across `compat.rs`, `chat.rs`, and several executors, while JS keeps it centralized. Without a request-transform layer, new executor work will likely duplicate or drift from route semantics.
- RTK is valuable but not the first critical path item. It saves tokens, yet it does not by itself retire `open-sse/*` and depends on normalized request surfaces.
- The clean cutover line is "no active Rust request path depends on JS runtime logic". JS deletion and cleanup should only begin after that line is verified.

---

## 4. Gap Analysis

| Component | Have | Need | Gap Size |
|-----------|------|------|----------|
| Request translator core | `src/server/api/compat.rs` normalization plus executor-local request transforms | `src/core/translator/request_transform.rs`, registry, same-format normalization, and conditioning hooks | High |
| Response translator core | `src/core/translator/response_transform.rs` with partial transformer coverage | Full Responses/Messages/Gemini/Ollama/Cursor/Kiro parity and better route-level response shaping | Medium-High |
| Chat core orchestration | `src/server/api/chat.rs` monolith | Shared core modules for request planning, executor dispatch, stream vs JSON handling, logging/refresh/fallback glue | High |
| Executor matrix | Default executor plus `codex`, `cursor`, `kiro`, `ollama`, `vertex`, `grok-web`, `perplexity-web` | Specialized `antigravity`, `gemini-cli`, `iflow`, `qoder`, `qwen`, plus audit of any remaining drift | Medium |
| RTK | Caveman prompt injection only | Filter registry, autodetect, safe apply, `compressMessages`, compact helpers, and golden tests | Medium |
| Utilities/services | Account fallback and request logging partially ported | Proxy-aware fetch policy, Claude header forwarding/cloaking, client detection, reasoning injection, session/project-id/token-refresh glue | High |
| Cloud sync/local cloud APIs | `/api/init` plus cloud credential/model alias routes | Remaining local cloud-sync handlers needed to disconnect JS runtime dependency | Medium |

---

## 5. Recommended Approach

Start with a Rust-native translator plus chat-core contract, not with RTK or provider-by-provider executor work. Extract the orchestration now embedded in `src/server/api/chat.rs` into a shared core module under `src/core/`, and add a registry-backed request-transform layer that mirrors the JS "source -> OpenAI -> target" pattern. Use the existing JS unit tests as golden parity fixtures for `/v1/chat/completions`, `/v1/messages`, and `/v1/responses*` semantics before broad executor expansion. Once that contract is stable, fill missing specialized executors against it, then port RTK compression and the remaining service utilities, and only then prune dormant JS runtime code.

### Why This Approach

- Shared request/response semantics sit above provider transport, so centralizing them first removes the largest source of duplicate work.
- The current repo already has a partially working Rust backend. Tightening its core contract respects the existing structure instead of creating a second runtime inside route handlers.
- The user wants the repo to end up as Rust plus Next.js, not "Rust plus a smaller JS backend". This sequencing moves directly toward that cutover line.

### Key Architectural Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Shared core location | Add a reusable chat-runtime module under `src/core/` with thin route wrappers in `src/server/api/*` | Keeps route handlers small and lets future CLI/direct invoke paths share one execution contract |
| Translation model | Registry-backed request and response transforms with an OpenAI intermediate format | Matches the proven JS baseline and avoids burying translation rules inside executors |
| Provider specialization rule | Only create bespoke executors for providers with custom auth/query/session behavior | Current Rust code already shows that some "missing" providers are fine on the default path |
| Verification basis | Port JS fixtures into Rust tests, then add live curl proofs on the Rust server | Faster and safer than manual parity reasoning; aligned with this repo's existing test posture |
| Cutover strategy | Two-step cutover: parity first, deletion second | Preserves a fallback reference until the Rust path is proven |

---

## 6. Alternatives Considered

### Option A: Executor First

- Description: add the missing executors before touching translator/chat-core work.
- Why considered: the missing-provider list is obvious and parallelizable.
- Why rejected: request/response semantics would stay scattered across routes and executors, causing duplication and likely rework.

### Option B: RTK First

- Description: port the RTK compression/filter stack immediately because it is relatively isolated.
- Why considered: clear module boundary and strong existing tests.
- Why rejected: it does not retire `open-sse/*`, and it depends on the final normalized request path anyway.

### Option C: Big-Bang JS Runtime Removal

- Description: delete `open-sse/*` first and rebuild whatever breaks directly in Rust.
- Why considered: superficially clean end state.
- Why rejected: too much regression risk across translators, streaming semantics, and provider-specific auth/session flows.

---

## 7. Risk Map

| Component | Risk Level | Reason | Verification Needed |
|-----------|------------|--------|---------------------|
| Request translator parity | **HIGH** | Multimodal content, tools, reasoning, and Responses API normalization are easy to drift | Golden fixture tests plus route-level integration tests |
| Streaming and Responses bridge | **HIGH** | SSE event ordering, finish reasons, usage emission, and SSE-to-JSON conversion are cross-cutting | Rust integration tests plus live curl checks |
| Antigravity and Gemini-CLI flows | **HIGH** | Custom project-id/session bootstrap and token-refresh behavior | Targeted executor tests and real endpoint smoke checks |
| Claude header forwarding/cloaking | **MEDIUM** | Host-specific headers and client identity logic are subtle and regression-prone | Ported fixture tests from `claude-header-forwarding.test.js` |
| RTK filter port | **MEDIUM** | Many small transforms with user-visible compression output | Direct fixture parity against `tests/unit/rtk.test.js` |
| Cloud sync local handlers | **MEDIUM** | Partial Rust implementation already exists, so gaps can hide in edge routes | Targeted route tests and curl validation |

### HIGH-Risk Summary (for encode)

- Request translator parity: prove the request registry and OpenAI-intermediate path before broad executor work.
- Streaming and Responses bridge: spike event mapping for OpenAI Responses plus Anthropic/Gemini streaming before deleting any JS response path.
- Antigravity and Gemini-CLI flows: prove project-id and refresh persistence rules early so executor work does not stall later.

---

## 8. Phase Breakdown

| Phase | Outcome | Why Now | Unlocks Next |
|-------|---------|---------|--------------|
| Phase 1 | Corrected provider/format inventory plus golden-fixture map | The rough backlog has drift; implementation needs a precise contract map first | Shared-core design and clean bead routing |
| Phase 2 | Shared Rust chat-core extraction plus request translator registry | Shared request semantics are the widest dependency surface | Response bridge work and executor lanes |
| Phase 3 | Full compatibility-route behavior for `/v1/chat/completions`, `/v1/messages`, and `/v1/responses*` | These routes define the main external API contract | Active-path cutover away from JS translators |
| Phase 4 | Missing specialized executors plus service/helper glue | Executor work is safer once request/response contracts are stable | Provider-matrix parity |
| Phase 5 | RTK compression stack plus remaining compact/cloud parity | RTK depends on the normalized final request path; cloud work depends on the shared core | Final JS runtime retirement |
| Phase 6 | Verified runtime cutover and JS backend pruning | Only safe after parity evidence exists | Clean Rust-plus-Next.js runtime state |

---

## 9. Proposed File Structure

```text
src/
  core/
    chat/
      mod.rs
      request_plan.rs
      dispatch.rs
      stream_bridge.rs
      json_bridge.rs
    translator/
      mod.rs
      request_transform.rs
      response_transform.rs
      registry.rs
    rtk/
      mod.rs
      autodetect.rs
      registry.rs
      filters/
        grep.rs
        find.rs
        tree.rs
        ls.rs
        smart_truncate.rs
        dedup_log.rs
        git_diff.rs
        git_status.rs
        read_numbered.rs
        search_list.rs
    executor/
      antigravity.rs
      gemini_cli.rs
      iflow.rs
      qoder.rs
      qwen.rs
tests/
  golden/
    translator_request/
    translator_response/
    rtk/
```

---

## 10. Dependency Order

```text
Layer 1 (sequential): Provider/format inventory and shared contract scaffolding
Layer 2 (parallel): Request translator pairs, response bridge slices, helper/service scaffolding
Layer 3 (parallel): Missing specialized executors
Layer 4 (sequential): RTK/compact/cloud integration into the final Rust path
Layer 5 (sequential): Runtime cutover and JS pruning
```

### Parallelizable Groups

- Group A: request translator pairs such as `openai <-> claude`, `openai <-> gemini`, `openai <-> cursor`, `openai <-> kiro`, `openai <-> ollama`, and Responses normalization once the registry exists.
- Group B: specialized executor files for `antigravity`, `gemini-cli`, `iflow`, `qoder`, and `qwen` after the shared request contract lands.
- Group C: RTK filter files and golden tests after the shared request/response path is stable.
- Group D: local cloud-sync route completion after shared auth/request helpers are settled.

---

## 11. Encode Notes

- Spike beads:
  - Correct the provider inventory and prove which "missing" providers truly need bespoke executors.
  - Map Responses and Anthropic/Gemini stream events into the Rust response bridge before broad rollout.
  - Prove Antigravity/Gemini-CLI project-id and token-refresh lifecycle behavior.
- Hard dependencies:
  - Shared `src/core/chat/*` and `src/core/translator/request_transform.rs` must land before executor beads that depend on them.
  - Shared-file edits across `src/server/api/chat.rs`, `src/core/translator/*`, `src/core/model/mod.rs`, and `src/core/executor/mod.rs` should be serialized or tightly owned.
- Relevant context decisions:
  - `D1`, `D2`, `D3`, `D4`, `D5`
- Bead detail requirements:
  - Each important bead should name the exact route/provider/format surface, the JS baseline file(s), whether it changes request shape, response shape, or transport/auth only, and the concrete verification command(s).
- Safe parallel lanes:
  - New provider executor files, RTK filter files, cloud-local API files, and independent golden tests.
- Unsafe overlap zones:
  - `src/server/api/chat.rs`, `src/core/translator/*`, `src/core/executor/provider.rs`, `src/core/executor/mod.rs`, and `src/core/model/mod.rs`.
- Acceptance or review checkpoints:
  - After compatibility-route semantics are proven.
  - After the missing-provider matrix is proven.
  - Before any JS runtime deletion claim is accepted.
- Open assumptions that must not be resolved silently:
  - Final cleanup scope for dormant Next.js API files.
  - Whether any provider currently on the default executor path still needs specialization after parity testing.

---

## 12. Institutional Learnings Applied

| Learning Source | Key Insight | How Applied |
|-----------------|-------------|-------------|
| `/data/projects/openproxy/PORT.md` | The Rust port target is a lightweight/full-function backend, not a reduced route subset | The plan prioritizes shared runtime semantics over cosmetic route completion |
| `.beads/issues.jsonl` | Backend route work is already materially ahead of dashboard work | The plan keeps dashboard changes explicitly deferred |

---

## 13. Verification Strategy

### 13.1 Build and test gates (every phase)

- `cargo test --lib`
- `cargo test --test v1_api_chat_api`
- `cargo test --test web_fetch_api`
- New targeted translator/executor/RTK test slices as each phase lands
- `cargo test` (full suite)
- `cargo clippy --tests --all-targets -- -D warnings`
- Optional JS fixture runs from `tests/` while porting, used as reference rather than final production truth

### 13.2 Live curl parity checks (per-phase, after Rust server is running)

Launch the Rust server before each verification wave:

```bash
DATA_DIR=/tmp/op target/debug/openproxy --port 20129 &
sleep 1
```

Health and config baseline:

```bash
curl -sf http://127.0.0.1:20129/health | jq .
curl -sf http://127.0.0.1:20129/api/init | jq .
curl -sf http://127.0.0.1:20129/v1/models | jq .
```

Compatibility route parity (translator and chat-core phases):

```bash
# /v1/chat/completions — streaming
curl -s http://127.0.0.1:20129/v1/chat/completions \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"ping"}],"stream":true}' \
  --no-buffer

# /v1/chat/completions — non-streaming
curl -s http://127.0.0.1:20129/v1/chat/completions \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"ping"}],"stream":false}'

# /v1/messages — Anthropic Messages format
curl -s http://127.0.0.1:20129/v1/messages \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"claude/claude-sonnet-4-20250514","messages":[{"role":"user","content":"ping"}],"max_tokens":128,"stream":true}' \
  --no-buffer

# /v1/responses — OpenAI Responses format
curl -s http://127.0.0.1:20129/v1/responses \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"openai/gpt-4o-mini","input":"ping","stream":true}' \
  --no-buffer

# /v1/responses/compact
curl -s http://127.0.0.1:20129/v1/responses/compact \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"openai/gpt-4o-mini","input":"ping"}'

# /v1/web/fetch — web fetch with auth gating
curl -s http://127.0.0.1:20129/v1/web/fetch \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"url":"https://httpbin.org/get"}'
```

Provider-specific executor parity (executor phase):

```bash
# Codex
curl -s http://127.0.0.1:20129/v1/chat/completions \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"cx/gpt-4o","messages":[{"role":"user","content":"ping"}],"stream":true}' \
  --no-buffer

# Cursor
curl -s http://127.0.0.1:20129/v1/chat/completions \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"cu/claude-sonnet-4-20250514","messages":[{"role":"user","content":"ping"}],"stream":true}' \
  --no-buffer

# Kiro, Vertex, Grok-web — same pattern with respective model prefixes
```

Each curl command should return valid SSE chunks (streaming) or valid JSON (non-streaming) with no Rust panics or 500 errors. Compare response shape against the baseline JS server when needed.

### 13.3 Browser-use dashboard validation (per-phase, after backend changes land)

Dashboard smoke test after each significant backend change. This catches regressions that curl alone cannot see because the dashboard depends on several `/api/*` endpoints together.

```bash
# Start server
DATA_DIR=/tmp/op target/debug/openproxy --port 20129 &

# Open dashboard
browser-use open http://127.0.0.1:20129/dashboard
browser-use state
```

Dashboard login flow:

```bash
# If require-login is enabled
browser-use input <index_email> "admin"
browser-use input <index_password> "$DASHBOARD_PASSWORD"
browser-use click <index_login_button>
browser-use state  # confirm logged-in state
```

Provider management check:

```bash
browser-use open http://127.0.0.1:20129/dashboard/providers
browser-use state  # confirm provider list loads, no JS errors
browser-use screenshot /tmp/dashboard-providers.png
```

Usage and settings check:

```bash
browser-use open http://127.0.0.1:20129/dashboard/usage
browser-use state
browser-use screenshot /tmp/dashboard-usage.png

browser-use open http://127.0.0.1:20129/dashboard/settings
browser-use state
browser-use screenshot /tmp/dashboard-settings.png
```

Chat/completions from dashboard (validates `/api/dashboard/chat/completions`):

```bash
browser-use open http://127.0.0.1:20129/dashboard/basic-chat
browser-use state
browser-use input <index_model_field> "openai/gpt-4o-mini"
browser-use input <index_message_field> "ping"
browser-use click <index_send_button>
browser-use wait text "pong" --timeout 15000
browser-use state  # confirm response appeared
```

After validation:

```bash
browser-use close
kill %1  # stop Rust server
```

Every phase that touches `/api/*` routes or auth/session behavior must pass the browser-use dashboard smoke before the phase is considered complete.

### 13.4 Final cutover verification

After all phases land and before declaring `open-sse/*` retired:

1. Full `cargo test` + clippy gate passes.
2. Every curl parity command in 13.2 returns valid responses for all active providers.
3. Browser-use dashboard smoke test passes: login, providers, usage, settings, basic-chat.
4. At least one real provider request flows end-to-end through the Rust path for each of: OpenAI-format, Anthropic-format, Gemini-format, Responses-format, and at least one combo model.
5. No Rust panic logs or 500 errors during any of the above.

---

## 14. Open Questions for Encode / Operate

- [ ] Should final cutover remove dormant Next.js API route files under `src/app/api/**`, or only disconnect them from active runtime traffic?
- [ ] If a low-use provider misses the main cutover checkpoint, should it remain explicitly unsupported in Rust for one short follow-up wave, or block the entire JS runtime retirement?

---

## 15. Approval Gate

- Goal clarity: `0.92`
- Boundary clarity: `0.85`
- Constraint clarity: `0.82`
- Acceptance clarity: `0.88`
- Weighted ambiguity: `0.12`
- Gate result: `PASS`
- Plan status: `Approved`
- Handoff note: verification strategy expanded to include live curl parity checks (13.2) and browser-use dashboard smoke tests (13.3) — both explicitly requested by user. All three verification layers (cargo tests, curl, browser-use) must pass per phase before cutover claims are made.
